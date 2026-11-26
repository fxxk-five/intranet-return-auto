//! AGENT PACK 编译器（PROMPT §6A）。
//!
//! **只在开发机上跑，只读 `<out>`，不连浏览器。**
//! 原则一（§6A.1）：抓取要笨、分析要晚 —— 内网只负责抓原始数据，所有归纳都在这里做，
//! 因为这里改错重跑一次不要钱，内网那头改错要再进一次内网。

pub mod actions;
pub mod api;
pub mod missing;
pub mod pages;
pub mod selectors;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub struct PackOptions {
    /// 原始 out 目录下的某个 session 目录（或 out 根目录，会自动挑最新 session）
    pub src: PathBuf,
    pub dest: PathBuf,
    /// 只处理这些阶段（None = 全部）
    pub pages: Option<Vec<String>>,
}

#[derive(Debug, Default)]
pub struct Summary {
    pub actions: usize,
    pub selectors: usize,
    pub requests: usize,
    pub pages: usize,
    pub unresolved: usize,
    pub l1_bytes: u64,
}

pub fn build(opts: &PackOptions) -> Result<Summary> {
    let src = resolve_session(&opts.src)?;
    std::fs::create_dir_all(&opts.dest)
        .with_context(|| format!("创建 AGENT_PACK 失败：{}", opts.dest.display()))?;

    // ---- 读原始素材 ----
    let events = read_timeline(&src)?;
    let har = read_json(&src.join("network.har")).unwrap_or(json!({}));
    let har_entries: Vec<Value> = har
        .get("log")
        .and_then(|l| l.get("entries"))
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();
    // HAR 里存的是脱敏值，重新抓 body 不可能；这里只用于 API 契约与请求清单
    let elements = read_json(&src.join("elements.json")).unwrap_or(json!({}));
    let inputs = read_json(&src.join("inputs.json")).unwrap_or(json!(null));

    // ---- 动作链路 ----
    let (acts, unresolved) = actions::build(&src, &events, &har_entries)?;
    let actions_v = serde_json::to_value(&acts)?;
    write_json(
        &opts.dest.join("ACTIONS.json"),
        &json!({
            "pack_version": 1,
            "generated_from": {
                "session_id": src.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
                "rule_version": env!("CARGO_PKG_VERSION"),
                "source_dir": "RAW/"
            },
            "actions": actions_v,
            "unresolved": unresolved,
        }),
    )?;

    // ---- 定位器表（三源合并：click / change / control_map）----
    let change_elements: Vec<(Value, u64)> = events
        .iter()
        .filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("change"))
        .filter_map(|e| {
            Some((
                e.get("element")?.clone(),
                e.get("seq").and_then(|s| s.as_u64()).unwrap_or(0),
            ))
        })
        .collect();
    let observed: Vec<Value> = events
        .iter()
        .filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("input_number"))
        .cloned()
        .collect();
    let control_map = elements
        .get("control_map")
        .cloned()
        .unwrap_or(json!({}));
    let sels = selectors::build(&selectors::Sources {
        acts: &acts,
        change_elements,
        control_map: &control_map,
        observed,
        elements: &elements,
    });
    write_json(
        &opts.dest.join("SELECTORS.json"),
        &json!({ "selectors": sels }),
    )?;

    // ---- 接口契约 ----
    let api_md = api::render(&har_entries, &acts, &elements);
    std::fs::write(opts.dest.join("API.md"), api_md)?;

    // ---- 精简页面（L2）----
    let page_names = pages::render(&src, &opts.dest, &opts.pages, &acts)?;

    // ---- SOP 一致性校验（收尾轮新增，进 L1）----
    let sop_md = crate::sopdiff::run(&src, None).unwrap_or_else(|_| {
        "# SOP 一致性校验\n\n未获取到（原因：没有 timeline.jsonl）\n".to_string()
    });
    std::fs::write(opts.dest.join("SOP-DIFF.md"), &sop_md)?;

    // ---- 缺口清单 ----
    let miss = missing::render(&src, &acts, &unresolved, &sels, &har_entries)?;
    std::fs::write(opts.dest.join("MISSING.md"), &miss)?;

    // ---- RAW 原样拷贝（不含 secrets/）----
    copy_raw(&src, &opts.dest)?;

    // ---- 截图对齐 ----
    let shot_count = copy_screenshots(&src, &opts.dest, &acts)?;

    // ---- 索引 ----
    let readme = readme(&actions_v, &sels, page_names.len(), shot_count, Some(&inputs));
    std::fs::write(opts.dest.join("00-AGENT-READ-ME.md"), &readme)?;

    // ---- 自检：包里不能出现本地绝对路径 ----
    let leaks = check_absolute_paths(&opts.dest)?;

    let l1: u64 = ["00-AGENT-READ-ME.md", "ACTIONS.json", "SELECTORS.json", "API.md", "MISSING.md", "SOP-DIFF.md"]
        .iter()
        .map(|f| std::fs::metadata(opts.dest.join(f)).map(|m| m.len()).unwrap_or(0))
        .sum();

    Ok(Summary {
        actions: acts.len(),
        selectors: sels.len(),
        requests: har_entries.len(),
        pages: page_names.len(),
        unresolved: unresolved.len() + leaks.len(),
        l1_bytes: l1,
    })
}

/// `--out` 可能给的是 out 根目录（含 sessions/）或直接是 session 目录
fn resolve_session(p: &Path) -> Result<PathBuf> {
    if p.join("timeline.jsonl").exists() {
        return Ok(p.to_path_buf());
    }
    let sessions = p.join("sessions");
    if sessions.is_dir() {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&sessions)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|x| x.is_dir() && x.join("timeline.jsonl").exists())
            .collect();
        dirs.sort();
        if let Some(last) = dirs.pop() {
            return Ok(last);
        }
    }
    anyhow::bail!(
        "找不到 timeline.jsonl。--out 要么指向 session 目录，要么指向含 sessions/ 的输出根目录：{}",
        p.display()
    )
}

fn read_timeline(src: &Path) -> Result<Vec<Value>> {
    let text = std::fs::read_to_string(src.join("timeline.jsonl"))
        .with_context(|| format!("读 timeline 失败：{}", src.display()))?;
    Ok(text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect())
}

fn read_json(p: &Path) -> Option<Value> {
    let t = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&t).ok()
}

fn write_json(p: &Path, v: &Value) -> Result<()> {
    std::fs::write(p, serde_json::to_vec_pretty(v)?)?;
    Ok(())
}

/// RAW/：原样拷贝原始素材。`secrets/` 不拷（与现有策略一致）。
fn copy_raw(src: &Path, dest: &Path) -> Result<()> {
    let raw = dest.join("RAW");
    std::fs::create_dir_all(&raw)?;
    for name in [
        "timeline.jsonl",
        "network.har",
        "elements.json",
        "dialogs.json",
        "experiment21.json",
        "assertions.json",
        "inputs.json",
        "annotations.csv",
        "report.json",
        "preflight.json",
    ] {
        let from = src.join(name);
        if from.exists() {
            let _ = std::fs::copy(&from, raw.join(name));
        }
    }
    for dir in ["mirror", "filmstrip", "screenshots", "snapshots"] {
        let from = src.join(dir);
        if from.is_dir() {
            copy_dir(&from, &raw.join(dir))?;
        }
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let p = e.path();
        let name = e.file_name();
        if p.is_dir() {
            copy_dir(&p, &to.join(&name))?;
        } else {
            let _ = std::fs::copy(&p, to.join(&name));
        }
    }
    Ok(())
}

/// 把 ACTIONS 引用的截图拷到 SCREENSHOTS/（L3 层，按 seq 命名）
fn copy_screenshots(src: &Path, dest: &Path, acts: &[actions::Action]) -> Result<usize> {
    let dir = dest.join("SCREENSHOTS");
    std::fs::create_dir_all(&dir)?;
    let mut n = 0;
    for a in acts {
        for p in [&a.pre_screenshot, &a.post_screenshot].into_iter().flatten() {
            let name = Path::new(p).file_name().map(|f| f.to_owned());
            if let Some(name) = name {
                let from = src.join(p);
                if from.exists() && std::fs::copy(&from, dir.join(&name)).is_ok() {
                    n += 1;
                }
            }
        }
    }
    Ok(n)
}

/// 包内不允许出现 `D:\...` / `C:/...` 这类本机路径（§6A.10）
pub fn check_absolute_paths(dest: &Path) -> Result<Vec<String>> {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[A-Za-z]:[\\/][^\s]{3,}").unwrap());

    let mut leaks = Vec::new();
    let mut stack = vec![dest.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d)?.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let ext = p
                .extension()
                .map(|x| x.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if !matches!(ext.as_str(), "json" | "md" | "html" | "csv" | "txt" | "har") {
                continue;
            }
            let Ok(t) = std::fs::read_to_string(&p) else { continue };
            if RE.is_match(&t) {
                leaks.push(p.strip_prefix(dest).unwrap_or(&p).to_string_lossy().to_string());
            }
        }
    }
    Ok(leaks)
}

fn readme(
    acts: &Value,
    sels: &[selectors::SelectorEntry],
    pages: usize,
    shots: usize,
    inputs: Option<&Value>,
) -> String {
    let n_actions = acts.as_array().map(|a| a.len()).unwrap_or(0);
    let unique_false = sels.iter().filter(|s| !s.unique).count();
    let used_numbers = inputs
        .and_then(|v| v.get("numbers"))
        .and_then(|n| n.as_array())
        .map(|a| {
            let total = a.len();
            let used = a
                .iter()
                .filter(|x| x.get("used").and_then(|u| u.as_bool()).unwrap_or(false))
                .count();
            format!("{used}/{total}")
        })
        .unwrap_or_else(|| "未提供".to_string());

    format!(
        r#"# 这个包是什么

内网系统「内网寄递业务平台」退货流程的一次**完整操作记录**，已编译成可直接消费的语料。

你的目标：**写出能自动完成 SOP 四个阶段的可运行脚本。**

## 阅读顺序（严格按此，不要跳）

1. `MISSING.md` —— **先读**。知道什么没有，才不会编。
1.5 `SOP-DIFF.md` —— 文档与实测流程的差异。**文档本身可能有错**，别照抄。
2. `ACTIONS.json` —— {n_actions} 个动作的完整链路（定位器 / 请求 / 界面变化 / 等待条件）。这是写代码的主要依据。
3. `SELECTORS.json` —— 控件定位器与稳定性。`unique:false` 的 {unique_false} 条一定要看 `why`。
4. `API.md` —— 判断哪一步能走接口、哪一步必须点界面。
5. `PAGES/*.html` —— 需要看页面结构时读，每份 < 200 KB。

## 不要做的事

- **不要**一次性读 `RAW/`（原始源码与 timeline，会撑爆上下文）。按需检索。
- **不要**假设 `MISSING.md` 里列出的东西存在。
- **不要**用截图推断元素 id —— id 在 `SELECTORS.json` 里。
- **引用动作请用 `action_uid`，不要用 `seq`。** `seq` 只在本包内有效，重生成会变。
- **不要**把 `RAW/mirror/` 里的源码整份贴进上下文；需要哪个文件再读哪个。

## 目录说明

| 路径 | 用途 |
|---|---|
| `ACTIONS.json` | 动作 → 定位器 → 请求 → 界面变化 → 等待条件 |
| `SELECTORS.json` | 去重后的定位器与稳定性评级 |
| `API.md` | 接口契约与可回放性判定 |
| `MISSING.md` | 本次没采到什么（防幻觉） |
| `PAGES/` | 精简 HTML（{pages} 份，已剥离脚本与样式） |
| `SCREENSHOTS/` | 按 seq 命名，与 ACTIONS 的 pre/post 对应（{shots} 张） |
| `RAW/` | 原始素材：`mirror/`（全站源码）、`filmstrip/`（操作前后 DOM+截图）、`timeline.jsonl` 等 |

## 数据说明

单号使用情况：{used_numbers}（清单见 `RAW/inputs.json`，值为脱敏 + hash8）。
所有路径都是**相对路径**，可以在任何机器上解开使用。
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_path_detection() {
        let dir = std::env::temp_dir().join("probe-pack-test-abs");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.md"), "路径是 D:\\probe\\out 这样").unwrap();
        let leaks = check_absolute_paths(&dir).unwrap();
        assert_eq!(leaks.len(), 1);

        std::fs::write(dir.join("b.md"), "相对路径 RAW/mirror/index.html 没问题").unwrap();
        let leaks2 = check_absolute_paths(&dir).unwrap();
        assert_eq!(leaks2.len(), 1, "没问题的文件不应新增泄漏");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_session_prefers_latest() {
        let root = std::env::temp_dir().join("probe-pack-test-resolve");
        let _ = std::fs::remove_dir_all(&root);
        let s1 = root.join("sessions").join("20260101-000000");
        let s2 = root.join("sessions").join("20260102-000000");
        std::fs::create_dir_all(&s1).unwrap();
        std::fs::create_dir_all(&s2).unwrap();
        std::fs::write(s1.join("timeline.jsonl"), "").unwrap();
        std::fs::write(s2.join("timeline.jsonl"), "").unwrap();
        let got = resolve_session(&root).unwrap();
        assert!(got.ends_with("20260102-000000"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
