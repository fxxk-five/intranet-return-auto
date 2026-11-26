//! DOM 元素清单与控件定位（§7.4 / Q3 / Q4）。
//!
//! 真正的遍历逻辑在 `assets/inspect_dom.js` 里（页面内执行），这里只负责：
//! 注入脚本、传参、把结果落盘。

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::cdp::Cdp;

pub const INSPECT_JS: &str = include_str!("assets/inspect_dom.js");
pub const RECORD_CLICK_JS: &str = include_str!("assets/record_click.js");
pub const MUTATION_PROBE_JS: &str = include_str!("assets/mutation_probe.js");

/// SOP 里出现的控件名（§8.2 control_map 必须覆盖，连同义变体）
pub const CONTROL_TARGETS: &[&str] = &[
    // 阶段A
    "邮件号码",
    "邮件号码输入框",
    "确定",
    "确定按钮",
    "确认下段",
    "确认下段按钮",
    // 阶段B
    "查询",
    "查询按钮",
    "全选",
    "打印模板",
    "国内快包面单-正式",
    "打印",
    "打印按钮",
    "确定打印",
    "确定打印按钮",
    "收件人手机号",
    "填写收件人手机号",
    // 阶段C
    "退回寄件人",
    "退回寄件人按钮",
    "妥投确认",
    "妥投确认按钮",
    "关闭",
    "关闭按钮",
    // 阶段D
    "→",
    "交接",
];

/// 把页面内脚本装进去（新文档 + 当前文档各一次）
pub async fn inject_probes(cdp: &Cdp, session: &str) -> Result<()> {
    for src in [RECORD_CLICK_JS, MUTATION_PROBE_JS, INSPECT_JS] {
        // 新文档：导航后仍然生效
        if let Err(e) = cdp.add_script_on_new(session, src).await {
            tracing::warn!("addScriptToEvaluateOnNewDocument 失败（继续）：{e:#}");
        }
    }
    // 当前文档立刻生效一次
    for src in [RECORD_CLICK_JS, MUTATION_PROBE_JS, INSPECT_JS] {
        if let Err(e) = cdp.evaluate(session, src).await {
            tracing::warn!("当前文档注入失败（继续）：{e:#}");
        }
    }
    Ok(())
}

/// 采集一次元素清单。
pub async fn collect(cdp: &Cdp, session: &str, page_targets: &[Value]) -> Result<Value> {
    let args = json!({
        "controlTargets": CONTROL_TARGETS,
        "pageTargetCount": page_targets.iter().filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("page")).count(),
        "tabTargets": page_targets.iter().filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
            .map(|t| json!({
                "id": t.get("targetId").and_then(|v| v.as_str()).unwrap_or(""),
                "title": t.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                "url": t.get("url").and_then(|v| v.as_str()).unwrap_or("")
            }))
            .collect::<Vec<_>>()
    });
    let expr = format!("window.__probeInspectDom({args})");
    let v = cdp.evaluate(session, &expr).await?;
    let s = v
        .as_str()
        .ok_or_else(|| anyhow!("inspect_dom 返回的不是字符串：{v}"))?;
    let parsed: Value = serde_json::from_str(s)?;
    if let Some(err) = parsed.get("error") {
        return Err(anyhow!("页面内元素采集失败：{err}"));
    }
    Ok(parsed)
}

/// 把多个 session 的采集结果合并成一份 `elements.json`（§8.2）。
pub fn merge(parts: Vec<(String, Value)>) -> Value {
    let mut frames: Vec<Value> = Vec::new();
    let mut page: Value = json!({});
    let mut control_map = serde_json::Map::new();
    let mut truncated = false;

    for (i, (session_id, p)) in parts.into_iter().enumerate() {
        if let Some(fr) = p.get("frames").and_then(|f| f.as_array()) {
            for f in fr {
                let mut f2 = f.clone();
                f2["cdp_session"] = json!(session_id);
                frames.push(f2);
            }
        }
        if i == 0 {
            if let Some(pg) = p.get("page") {
                page = pg.clone();
            }
        }
        if p.get("page").and_then(|x| x.get("elements_truncated")).and_then(|x| x.as_bool()).unwrap_or(false) {
            truncated = true;
        }
        if let Some(cm) = p.get("control_map").and_then(|m| m.as_object()) {
            for (k, v) in cm {
                // 多 session 时后到的只补空，不覆盖已确定的
                if !control_map.contains_key(k) || control_map[k].get("selector").map(|s| s.is_null()).unwrap_or(true) {
                    control_map.insert(k.clone(), v.clone());
                }
            }
        }
    }

    page["elements_truncated"] = json!(truncated);
    json!({
        "generated_at": crate::util::now_iso(),
        "page": page,
        "frames": frames,
        "control_map": Value::Object(control_map)
    })
}
