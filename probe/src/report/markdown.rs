//! `report.md` 渲染（§9.1）。**标题层级和顺序不得改动。**

use std::fmt::Write as _;

use super::Input;
use crate::util::truncate_bytes;
use serde_json::Value;

const NO_DATA: &str = "未获取到（原因：本次运行没有采集到对应数据）";

pub fn render(input: &Input<'_>, rj: &Value, _assertions: &Value) -> String {
    let mut s = String::new();
    let path = rj["verdict"]["path"].as_str().unwrap_or("unknown");
    let reason = rj["verdict"]["reason"].as_str().unwrap_or("");

    // ---- 头 ----
    s.push_str("# 退货流程自动化探针报告\n\n");
    s.push_str("> ⚠️ 本报告及同目录文件可能包含账号凭据与业务数据，仅限内网留存，禁止外传。\n");
    s.push_str("> 凭据完整值见 secrets/secrets.json\n\n");
    let _ = writeln!(s, "生成时间：{}", rj["meta"]["generated_at"].as_str().unwrap_or("-"));
    let _ = writeln!(s, "会话数：{}", rj["meta"]["sessions"]);
    let _ = writeln!(s, "Edge 版本：{}", rj["meta"]["edge_version"].as_str().unwrap_or("未获取到"));
    let _ = writeln!(s, "页面 UA：{}", rj["meta"]["ua"].as_str().unwrap_or("未获取到"));
    s.push_str("\n---\n\n");

    // ---- 一 ----
    s.push_str("## 一、一句话结论\n\n");
    let label = match path {
        "protocol" => "建议走【协议回放】",
        "ui" => "建议走【UI 自动化】",
        "hybrid" => "建议【混合】",
        _ => "结论：未获取到足够证据",
    };
    let _ = writeln!(s, "{label}：{reason}\n");

    // ---- 二：SOP 一致性（收尾轮新增，紧跟一句话结论）----
    s.push_str("## 二、SOP 一致性（文档 vs 实际）\n\n");
    match std::fs::read_to_string(input.sess.path("SOP-DIFF.md")) {
        Ok(t) => {
            // 直接引用 SOP-DIFF 的正文（去掉它自己的一级标题）
            for line in t.lines().skip_while(|l| !l.starts_with("比对对象") ) {
                s.push_str(line);
                s.push('\n');
            }
        }
        Err(_) => s.push_str("未获取到（原因：没有运行 sopdiff）\n"),
    }
    s.push('\n');

    // ---- 三 ----
    s.push_str("## 三、Q1 请求取证\n\n");
    s.push_str("| 步骤 | 方法 | 路径 | 协议 | 是否含签名 | 备注 |\n|---|---|---|---|---|---|\n");
    let mut rows = 0;
    if let Some(steps) = rj["scenario"]["steps"].as_array() {
        for st in steps {
            let body = st["body_template"].as_str().unwrap_or("");
            let head = st["headers"].to_string();
            let hay = format!("{body} {head}").to_ascii_lowercase();
            let signed = ["sign", "nonce", "token", "encrypt"].iter().any(|k| hay.contains(k));
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} | 动态字段 {} |",
                st["trigger"].as_str().unwrap_or(""),
                st["method"].as_str().unwrap_or(""),
                st["path"].as_str().unwrap_or(""),
                st["protocol"].as_str().unwrap_or(""),
                if signed { "是" } else { "否" },
                st["dynamic_fields"].to_string()
            );
            rows += 1;
        }
    }
    if rows == 0 {
        let _ = writeln!(s, "| {NO_DATA} | | | | | |");
    }
    s.push('\n');

    // ---- 三 ----
    s.push_str("## 四、Q2 关于\"21\"\n\n");
    let q2v = rj["q2"]["q2_verdict"].as_str().unwrap_or("unknown");
    let _ = write!(s, "判定：{q2v}\n");
    let _ = write!(s, "依据：{}\n", rj["q2"]["explanation"].as_str().unwrap_or(NO_DATA));
    let _ = write!(s, "对后续程序的影响：{}\n", rj["q2"]["implication"].as_str().unwrap_or(NO_DATA));
    let nc = &rj["q2"]["network_check"];
    let _ = writeln!(
        s,
        "\n旁路观测数据：与\"确定/确认\"类点击关联的请求 {} 个，其中每个请求携带单号数 {}。\n",
        nc["detail"]["scan_requests"].as_i64().unwrap_or(0),
        nc["numbers_per_request"].to_string()
    );

    // ---- 四 ----
    s.push_str("## 五、Q3 控件定位器\n\n");
    s.push_str("| 控件名 | 主定位器 | 唯一性 | 备选 | 脆弱性 |\n|---|---|---|---|---|\n");
    let mut ctr = 0;
    if let Some(cm) = rj["q3"]["control_map"].as_object() {
        for (k, v) in cm {
            let sel = v["selector"].as_str().unwrap_or("");
            let uniq = v["unique"].as_bool().unwrap_or(false);
            let note = v["note"].as_str().unwrap_or("");
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} |",
                k,
                if sel.is_empty() { "—" } else { sel },
                if uniq { "唯一" } else { "不唯一" },
                truncate_bytes(note, 40),
                if sel.is_empty() { "需人工确认" } else { "可维护" }
            );
            ctr += 1;
        }
    }
    if ctr == 0 {
        let _ = writeln!(s, "| {NO_DATA} | | | | |");
    }
    s.push('\n');

    // ---- 五 ----
    s.push_str("## 六、Q4 页面结构\n\n");
    let _ = writeln!(
        s,
        "判定：{}",
        rj["q4"]["tab_structure"].as_str().unwrap_or("unknown")
    );
    let _ = writeln!(
        s,
        "详情：{}",
        truncate_bytes(&rj["q4"]["detail"].to_string(), 500)
    );
    s.push('\n');

    // ---- 六 ----
    s.push_str("## 七、Q5 打印分支判定\n\n");
    let pp = rj["q5"]["print_preview_count"].as_i64().unwrap_or(0);
    let nat = rj["q5"]["native_count"].as_i64().unwrap_or(0);
    let spool = rj["q5"]["spool_events"].as_array().cloned().unwrap_or_default();
    let _ = writeln!(
        s,
        "分支B1 触发条件：{}",
        if pp == 0 { "未观测到打印预览窗口（可能直接完成或纯前端）" } else { "出现打印预览窗口，属原生窗口分支" }
    );
    let _ = writeln!(
        s,
        "分支B2 触发条件：{}",
        if nat + pp == 0 { "未观测到额外原生窗口" } else { "点击后出现额外原生窗口，需要 Win32 层处理" }
    );
    let _ = writeln!(
        s,
        "点\"确定打印\"后是否真的进 spooler：{}",
        if spool.is_empty() {
            "否（观测期内 spooler 无新增 job）".to_string()
        } else {
            format!("是：{} 个 spooler 事件，见 spool.jsonl", spool.len())
        }
    );
    let _ = writeln!(
        s,
        "置信度：{}",
        if pp + nat > 0 || !spool.is_empty() { "中" } else { "低（未观测到打印相关窗口/job）" }
    );
    s.push('\n');

    // ---- 七 ----
    s.push_str("## 八、Q6 性能\n\n");
    s.push_str("| 步骤 | 单次耗时 | 500 单外推 | 瓶颈 |\n|---|---|---|---|\n");
    let mut q6rows = 0;
    if let Some(arr) = rj["q6"]["by_step"].as_array() {
        for x in arr {
            let avg = x["avg_ms"].as_f64().unwrap_or(0.0);
            let _ = writeln!(
                s,
                "| {} | {:.0} ms | ≈ {:.0} s | {} |",
                x["step"].as_str().unwrap_or("未标注"),
                avg,
                avg * 500.0 / 1000.0,
                "接口往返"
            );
            q6rows += 1;
        }
    }
    if q6rows == 0 {
        let _ = writeln!(s, "| {NO_DATA} | | | |");
    }
    s.push('\n');

    // ---- 八 ----
    s.push_str("## 九、Q7 打印环境\n\n");
    let dp = rj["q7"]["default_printer"].as_str().unwrap_or("未获取到");
    let virt = rj["q7"]["is_virtual"].as_bool().map(|b| if b { "是" } else { "否" }).unwrap_or("未知");
    let _ = writeln!(s, "默认打印机：{dp}（是否虚拟打印机：{virt}）");
    let _ = writeln!(
        s,
        "检测到的打印窗口：{}",
        if pp > 0 { format!("{pp} 个 print_preview 类窗口") } else { "未检测到".to_string() }
    );
    let _ = writeln!(
        s,
        "点\"确定打印\"后 spooler 事件：{}",
        if spool.is_empty() { "无".to_string() } else { format!("{} 条", spool.len()) }
    );
    let _ = writeln!(
        s,
        "可行绕过方案：{}",
        if virt == "是" {
            "默认打印机本身就是虚拟打印机，正式程序需保证打印到 PDF 即可，不必碰浏览器打印参数。"
        } else {
            "优先用 CDP Page.printToPDF 生成 PDF（不经 spooler、不弹窗）；不建议使用 --kiosk-printing（会真的出纸）。"
        }
    );
    s.push('\n');

    // ---- 九：操作序列时间线（PROMPT §6.5）----
    s.push_str("## 十、操作序列时间线\n\n");
    s.push_str("| # | 动作 | 阶段 | 时刻 | 定位器 | 请求数 | 截图 |\n|---|---|---|---|---|---|---|\n");
    let rows = action_rows(input.sess);
    if rows.is_empty() {
        s.push_str("| 未获取到（原因：本次没有采集到点击/输入事件） | | | | | | |\n");
    } else {
        for (i, (name, step, at, sel, reqs, shot)) in rows.iter().enumerate() {
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | `{}` | {} | {} |",
                i + 1,
                name,
                step,
                at,
                truncate_bytes(sel, 40),
                reqs,
                shot
            );
        }
    }
    s.push('\n');

    // ---- 十：源码镜像（PROMPT §3）----
    s.push_str("## 十一、源码镜像\n\n");
    let idx = input.sess.path("mirror/INDEX.json");
    if let Ok(t) = std::fs::read_to_string(&idx) {
        if let Ok(v) = serde_json::from_str::<Value>(&t) {
            let sum = &v["summary"];
            let _ = writeln!(
                s,
                "- 资源：抓取 {} / 跳过 {} / inline {} / MHTML {} 份，共 {:.1} MB{}",
                sum["captured"].as_u64().unwrap_or(0),
                sum["skipped"].as_u64().unwrap_or(0),
                sum["inline"].as_u64().unwrap_or(0),
                sum["mhtml"].as_u64().unwrap_or(0),
                sum["bytes"].as_u64().unwrap_or(0) as f64 / 1048576.0,
                if sum["truncated"].as_bool().unwrap_or(false) { "（已按预算截断）" } else { "" }
            );
            let _ = writeln!(s, "- 索引：`mirror/INDEX.json`；离机复现请先用浏览器打开 `mirror/<host>/index.html` 或 `mirror/mhtml/*.mhtml`");
            if let Some(m) = v["mhtml"].as_array() {
                let _ = writeln!(s, "- MHTML 存档：{} 份（自包含，可 `file://` 直接打开）", m.len());
            }
            let skipped: Vec<String> = v["resources"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|r| r.get("skipped").and_then(|x| x.as_str()).map(|x| x.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            if !skipped.is_empty() {
                let _ = writeln!(s, "- ⚠️ 有 {} 个资源未抓取（原因见 `mirror/INDEX.json` 的 `skipped`）", skipped.len());
            }
            let _ = writeln!(
                s,
                "- 快照带：`filmstrip/`（每次操作前后各一张图 + 两份完整 DOM，按 seq 命名）"
            );
            let _ = writeln!(s, "- ⚠️ `mirror/` 不脱敏（保持代码可运行），可能含硬编码凭据，禁止外传");
        }
    } else {
        s.push_str("- 未获取到（原因：本次没有采集源码镜像）\n");
    }
    s.push('\n');

    // ---- 十一 ----
    s.push_str("## 十二、弹窗清单\n\n");
    s.push_str("| # | 步骤 | 类型 | 触发 | 关闭方式 | 定位器 |\n|---|---|---|---|---|---|\n");
    let mut dcount = 0;
    if let Some(arr) = rj["dialogs"].as_array() {
        for d in arr {
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} | {} |",
                d["seq"].as_i64().unwrap_or(0),
                d["step"].as_str().unwrap_or("-"),
                d["type"].as_str().unwrap_or("-"),
                truncate_bytes(d["trigger_click_text"].as_str().unwrap_or("-"), 20),
                d["close_method"].as_str().unwrap_or("-"),
                truncate_bytes(
                    d["evidence"]["selector"]
                        .as_str()
                        .or_else(|| d["evidence"]["title"].as_str())
                        .unwrap_or("-"),
                    60
                )
            );
            dcount += 1;
        }
    }
    if dcount == 0 {
        let _ = writeln!(s, "| {NO_DATA} | | | | | |");
    }
    s.push('\n');

    // ---- 十二 ----
    s.push_str("## 十三、风险清单\n\n");
    if let Some(arr) = rj["risks"].as_array() {
        for (i, r) in arr.iter().enumerate() {
            let _ = writeln!(
                s,
                "{}. [{}] {}",
                i + 1,
                r["level"].as_str().unwrap_or("-"),
                r["text"].as_str().unwrap_or("")
            );
        }
    } else {
        s.push_str("未获取到（原因：无法评估风险）\n");
    }
    s.push('\n');

    // ---- 十三 ----
    s.push_str("## 十四、后续三个月路线建议\n\n");
    if let Some(arr) = rj["roadmap"].as_array() {
        for r in arr {
            let _ = writeln!(
                s,
                "- **{}**：{}（判据：{}）",
                r["phase"].as_str().unwrap_or(""),
                r["goal"].as_str().unwrap_or(""),
                r["criteria"].as_str().unwrap_or("")
            );
        }
    } else {
        s.push_str(NO_DATA);
        s.push('\n');
    }
    s.push('\n');

    // ---- 十四 ----
    s.push_str("## 十五、附录\n\n");
    s.push_str("- 证据文件索引：\n");
    for f in [
        "network.har",
        "elements.json",
        "dialogs.json",
        "experiment21.json",
        "assertions.json",
        "timeline.jsonl",
        "screenshots/",
        "snapshots/",
        "replay/scenario.json",
        "secrets/secrets.json",
    ] {
        let exists = input.sess.path(f).exists();
        let _ = writeln!(s, "  - `{f}`{}", if exists { "" } else { "（未生成）" });
    }
    s.push_str("- 未解决的问题：\n");
    let mut unresolved = 0;
    if rj["q2"]["q2_verdict"].as_str().unwrap_or("") == "unknown" {
        s.push_str("  - Q2 未判定，建议补做 §7.7.3 协议重放\n");
        unresolved += 1;
    }
    if dcount == 0 {
        s.push_str("  - 未采集到任何弹窗样本，弹窗清单可能不完整\n");
        unresolved += 1;
    }
    if pos_or_zero(&rj["q1"]["requests"]) == 0 {
        s.push_str("  - 未采集到业务请求，Q1 结论不可靠\n");
        unresolved += 1;
    }
    if unresolved == 0 {
        s.push_str("  - 无（本次采集范围内）\n");
    }

    s
}

fn pos_or_zero(v: &Value) -> i64 {
    v.as_i64().unwrap_or(0)
}

/// 从 timeline 里抽"操作序列"（给报告第九节用）
fn action_rows(sess: &crate::session::Session) -> Vec<(String, String, String, String, usize, String)> {
    let Ok(text) = std::fs::read_to_string(sess.path("timeline.jsonl")) else {
        return Vec::new();
    };
    let mut events: Vec<Value> = Vec::new();
    for line in text.lines() {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            events.push(v);
        }
    }
    // 请求按点击 seq 归堆
    let mut reqs: std::collections::HashMap<u64, usize> = Default::default();
    for e in &events {
        if e.get("kind").and_then(|k| k.as_str()) == Some("request") {
            if let Some(s) = e.get("caused_by_click_seq").and_then(|x| x.as_u64()) {
                *reqs.entry(s).or_insert(0) += 1;
            }
        }
    }
    let mut rows = Vec::new();
    for e in &events {
        let kind = e.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        if !matches!(kind, "click" | "change" | "enter") {
            continue;
        }
        let seq = e.get("seq").and_then(|s| s.as_u64()).unwrap_or(0);
        let name = e
            .get("annotation")
            .and_then(|a| a.get("action_name"))
            .and_then(|n| n.as_str())
            .or_else(|| e.get("element").and_then(|el| el.get("text")).and_then(|t| t.as_str()))
            .unwrap_or(kind)
            .to_string();
        // 双保险：即使上游漏了，报告里也不许出现明文单号/手机号
        let name = crate::redact::redact_text(&name);
        let step = e.get("step").and_then(|s| s.as_str()).unwrap_or("-").to_string();
        let at = e.get("ts").and_then(|t| t.as_str()).map(|t| t.get(11..19).unwrap_or(t).to_string()).unwrap_or_default();
        let sel = e
            .get("element")
            .and_then(|el| el.get("css"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        let shot = e
            .get("filmstrip")
            .and_then(|f| f.get("before"))
            .and_then(|b| b.as_str())
            .and_then(|p| p.rsplit('/').next())
            .map(|p| p.split('-').next().unwrap_or("").to_string())
            .unwrap_or_else(|| "-".to_string());
        rows.push((name, step, at, sel, reqs.get(&seq).copied().unwrap_or(0), shot));
    }
    rows
}
