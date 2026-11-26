//! MISSING.md —— 本次采集的缺口（PROMPT §6A.7）。
//!
//! 这一份是**防幻觉**的核心：agent 先读它，才知道哪些东西不能假设存在。
//! 汇总规则（§6A.7）：unresolved / unique:false / INDEX skipped / body_status != ok
//! 全部自动收集，不许人工写。

use anyhow::Result;
use serde_json::Value;
use std::path::Path;

use super::actions::Action;
use super::selectors::SelectorEntry;

pub fn render(
    src: &Path,
    acts: &[Action],
    unresolved: &[Value],
    sels: &[SelectorEntry],
    har: &[Value],
) -> Result<String> {
    let mut md = String::new();
    md.push_str("# 本次采集的缺口\n\n");
    md.push_str("> 以下内容**没有采到**。不要推断，不要假设它们在真实系统里不存在。\n\n");

    // ---- 完全缺失：响应体没拿到 ----
    let mut missing_bodies: Vec<String> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(src.join("timeline.jsonl")) {
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if v.get("kind").and_then(|k| k.as_str()) != Some("response") {
                continue;
            }
            let status = v.get("body_status").and_then(|s| s.as_str()).unwrap_or("");
            // url_only 是"按采样规则本来就不取 body"（静态资源/心跳），不算缺口；
            // pending 是还没收尾完，也不算
            if status == "ok"
                || status.is_empty()
                || status == "pending"
                || status.contains("url_only")
            {
                continue;
            }
            missing_bodies.push(format!(
                "`{}`：body_status = `{status}`",
                v.get("request_id").and_then(|r| r.as_str()).unwrap_or("?")
            ));
        }
    }
    md.push_str("## 完全缺失\n\n");
    if missing_bodies.is_empty() {
        md.push_str("无\n");
    } else {
        for m in missing_bodies.iter().take(30) {
            md.push_str(&format!("- {m}\n"));
        }
        if missing_bodies.len() > 30 {
            md.push_str(&format!("- …共 {} 条，完整清单见 RAW/timeline.jsonl\n", missing_bodies.len()));
        }
    }
    if har.is_empty() {
        md.push_str("- 没有任何 HAR 记录\n");
    }
    md.push('\n');

    // ---- 部分缺失：镜像跳过 ----
    md.push_str("## 部分缺失\n\n");
    let mut skipped_lines = 0;
    if let Ok(text) = std::fs::read_to_string(src.join("mirror").join("INDEX.json")) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            if let Some(res) = v.get("resources").and_then(|r| r.as_array()) {
                let mut by_reason: std::collections::BTreeMap<String, usize> = Default::default();
                for r in res {
                    if let Some(s) = r.get("skipped").and_then(|s| s.as_str()) {
                        *by_reason.entry(s.split(':').next().unwrap_or(s).to_string()).or_insert(0) += 1;
                    }
                }
                for (reason, n) in by_reason {
                    md.push_str(&format!("- `mirror/` 有 {n} 个资源未抓取，原因：`{reason}`\n"));
                    skipped_lines += 1;
                }
            }
        }
    } else {
        md.push_str("- 没有 `mirror/INDEX.json`（源码镜像未运行）\n");
        skipped_lines += 1;
    }
    let no_film = acts.iter().all(|a| a.pre_screenshot.is_none());
    if no_film {
        md.push_str("- 没有操作前后截图（filmstrip 未运行或被 `--no-filmstrip` 关闭）\n");
        skipped_lines += 1;
    }
    if skipped_lines == 0 {
        md.push_str("无\n");
    }
    md.push('\n');

    // ---- 置信度低 ----
    md.push_str("## 置信度低\n\n");
    let mut low = 0;
    for s in sels.iter().filter(|s| !s.unique) {
        md.push_str(&format!(
            "- `{}` 定位器不唯一（出现 {} 次，seq={}）：{}\n",
            s.key, s.occurrences, s.first_seq, s.why
        ));
        low += 1;
    }
    for u in unresolved {
        md.push_str(&format!(
            "- seq={} 的操作意图不明：{}\n",
            u.get("seq").and_then(|s| s.as_u64()).unwrap_or(0),
            u.get("reason").and_then(|r| r.as_str()).unwrap_or("")
        ));
        low += 1;
    }
    if low == 0 {
        md.push_str("无\n");
    }
    md.push('\n');

    // ---- 未标注 ----
    let unannotated = acts
        .iter()
        .filter(|a| a.annotation.action_no.is_none())
        .count();
    // 全部标注了就整节省略（写"无"反而会被 agent 当成"没标注"）
    if unannotated > 0 {
        md.push_str("## 未语义标注\n\n");
        md.push_str(&format!(
            "- {} / {} 个动作没有人工标注（动作名是推断的，参考价值低于已标注项）\n\n",
            unannotated,
            acts.len()
        ));
    }

    // ---- 建议补采 ----
    md.push_str("## 建议补采\n\n");
    if missing_bodies.is_empty() && low == 0 && skipped_lines == 1 && unannotated == 0 {
        md.push_str("无\n");
    } else {
        if !missing_bodies.is_empty() {
            md.push_str("- 重新走一遍对应阶段，让响应体落盘（`Network.getResponseBody` 有时效）\n");
        }
        if unannotated > 0 {
            md.push_str("- 重跑时按数字键标注动作，动作名的可信度会显著提高\n");
        }
        if sels.iter().any(|s| !s.unique) {
            md.push_str("- 对 `unique:false` 的控件，实机确认一眼可见的区分特征（位置/图标/相邻文本）\n");
        }
        if skipped_lines > 0 {
            md.push_str("- 若确实需要缺失的静态资源，把 `--mirror-max-mb` 调大后重跑一次\n");
        }
    }

    Ok(md)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentpack::actions::{Action, SelectorInfo, UiDelta};

    #[test]
    fn aggregates_all_sources() {
        let dir = std::env::temp_dir().join("probe-missing-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("mirror")).unwrap();
        std::fs::write(
            dir.join("mirror").join("INDEX.json"),
            r#"{"resources":[{"url":"http://h/a.png","skipped":"too_large"},{"url":"http://h/b.png","skipped":"too_large"}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("timeline.jsonl"),
            "{\"kind\":\"response\",\"request_id\":\"r1\",\"body_status\":\"unavailable:4042\"}\n",
        )
        .unwrap();

        let acts = vec![Action {
            no: 1,
            name: "点打印".into(),
            step: Some("B".into()),
            seq: 42,
            kind: "click".into(),
            selector: SelectorInfo::default(),
            ui_delta: UiDelta::default(),
            next_waits_for: serde_json::json!({"type":"none"}),
            pre_screenshot: None,
            ..Default::default()
        }];
        let sels = vec![SelectorEntry {
            key: "D.点 ->".into(),
            primary: Some("button:nth-child(4)".into()),
            backup: vec![],
            unique: false,
            occurrences: 1,
            first_seq: 9,
            stability: "low".into(),
            why: "同级 4 个同结构按钮".into(),
            ..Default::default()
        }];
        let unresolved = vec![serde_json::json!({"seq":57,"reason":"既没有关联请求，DOM 也没变化"})];

        let md = render(&dir, &acts, &unresolved, &sels, &[]).unwrap();
        // 四类来源都要出现
        assert!(md.contains("unavailable:4042"), "响应体缺失");
        assert!(md.contains("too_large"), "镜像跳过");
        assert!(md.contains("不唯一"), "定位器不唯一");
        assert!(md.contains("意图不明"), "unresolved");
        assert!(md.contains("没有任何 HAR 记录"));
        assert!(md.contains("未语义标注"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
