//! Q2 的判定（§7.7）：H1 静态检测、H2 网络旁路、H3 协议重放。
//!
//! 顺序刻意如此：H2 是**纯旁路观测**，不需要发任何请求，所以先看它。
//! 绝大多数情况下，看到"一次确定 = 一个请求 = 一个单号"，Q2 就结案了。

use anyhow::Result;
use serde_json::{json, Value};

use crate::cdp::Cdp;
use crate::netcap::NetStore;

/// §7.7.1 静态检测：读 maxlength，并程序化写入 30 字符后读回
pub const STATIC_CHECK_JS: &str = r#"
(function () {
  var inputs = Array.from(document.querySelectorAll('input,textarea'));
  return JSON.stringify(inputs.map(function (el) {
    var r = {
      id: el.id || null,
      name: el.name || null,
      tag: el.tagName.toLowerCase(),
      maxlength_attr: el.getAttribute('maxlength'),
      maxLength_prop: (typeof el.maxLength === 'number' ? el.maxLength : null),
      type: el.type
    };
    var old = el.value;
    try {
      el.focus();
      el.value = '1'.repeat(30);
      el.dispatchEvent(new Event('input', { bubbles: true }));
      r.after_write_len = el.value.length;
    } catch (e) { r.after_write_len = -1; }
    try {
      el.value = old;
      el.dispatchEvent(new Event('input', { bubbles: true }));
    } catch (e) {}
    return r;
  }));
})()
"#;

pub async fn static_check(cdp: &Cdp, session: &str) -> Result<Value> {
    let v = cdp.evaluate(session, STATIC_CHECK_JS).await?;
    let s = v.as_str().unwrap_or("[]");
    let inputs: Value = serde_json::from_str(s).unwrap_or(json!([]));
    Ok(inputs)
}

/// 由静态检测结果判 H1
pub fn h1_verdict(inputs: &Value) -> (String, Option<String>) {
    let arr = match inputs.as_array() {
        Some(a) => a,
        None => return ("unknown".into(), None),
    };
    let mut framework_hint = None;
    for el in arr {
        let attr = el.get("maxlength_attr").and_then(|v| v.as_str()).unwrap_or("");
        let prop = el.get("maxLength_prop").and_then(|v| v.as_i64()).unwrap_or(-1);
        let after = el.get("after_write_len").and_then(|v| v.as_i64()).unwrap_or(-1);
        if attr == "21" || prop == 21 {
            return ("supported".into(), Some("attr_or_prop_maxlength_21".into()));
        }
        if after == 21 {
            framework_hint = Some("framework_truncated_to_21".to_string());
        }
    }
    if let Some(h) = framework_hint {
        return ("supported".into(), Some(h));
    }
    if arr.is_empty() {
        return ("unknown".into(), Some("no_inputs_found".into()));
    }
    ("not_applicable".into(), None)
}

/// 由网络记录判 H2（§7.7.2，无需主动发请求）
pub fn h2_verdict(net: &NetStore) -> (String, Value) {
    let s = net.scan_stats();
    let verdict = if s.scan_requests == 0 {
        "unknown"
    } else if s.numbers_per_request.iter().all(|n| *n <= 1) && s.multi_request_clicks == 0 {
        "supported"
    } else if s.numbers_per_request.iter().any(|n| *n > 1) {
        "refuted_batch_request"
    } else {
        "inconclusive"
    };
    (
        verdict.to_string(),
        json!({
            "scan_requests": s.scan_requests,
            "numbers_per_request": s.numbers_per_request,
            "ok_clicks": s.ok_clicks,
            "multi_request_clicks": s.multi_request_clicks,
            "sample_size": s.numbers_per_request.len()
        }),
    )
}

/// 汇总成 `experiment21.json`（§8.4）
pub fn build(
    static_inputs: Value,
    h1: (String, Option<String>),
    h2: (String, Value),
    replay: Option<Value>,
) -> Value {
    let (q2, explanation, implication) = match (h1.0.as_str(), h2.0.as_str()) {
        ("supported", _) => (
            "H1",
            "输入框本身带 maxlength=21（或框架层截断到 21），前端硬限制存在。".to_string(),
            "后续程序必须按 21 分组；一次最多 21 个单号。".to_string(),
        ),
        (_, "supported") => (
            "H2",
            "每次点'确定'只发送 1 个单号、1 个请求，系统没有批量接口，21 是操作员的操作段落。"
                .to_string(),
            "后续程序按'单号循环、单请求'实现，无需批次概念。".to_string(),
        ),
        (_, "refuted_batch_request") => (
            "unknown",
            "观测到一次点击携带多个单号的请求，既不是 H1 也不是 H2，需要继续观察或补做 §7.7.3。"
                .to_string(),
            "在补测之前不要按批次实现。".to_string(),
        ),
        _ => (
            "unknown",
            "未观测到足够的'确定→请求'样本，无法判定。建议补做 §7.7.3 协议重放。".to_string(),
            "补测后重新判定。".to_string(),
        ),
    };

    // H3：重放结果里 22 失败 → 后端阈值
    let mut q2 = q2.to_string();
    let explanation = if let Some(r) = &replay {
        if r.get("executed").and_then(|v| v.as_bool()).unwrap_or(false) {
            let fail_22 = r
                .get("results")
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter().any(|x| {
                        x.get("n").and_then(|n| n.as_i64()) == Some(22)
                            && x.get("is_business_error").and_then(|b| b.as_bool()).unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            if fail_22 {
                q2 = "H3".to_string();
                "协议重放显示 22 个单号起就报业务错误，后端存在阈值。".to_string()
            } else {
                explanation
            }
        } else {
            explanation
        }
    } else {
        explanation
    };

    json!({
        "generated_at": crate::util::now_iso(),
        "static_check": {
            "inputs": static_inputs,
            "h1_verdict": h1.0,
            "h1_hint": h1.1
        },
        "network_check": {
            "h2_verdict": h2.0,
            "detail": h2.1,
            "numbers_per_request": h2.1.get("numbers_per_request").cloned().unwrap_or(json!([]))
        },
        "replay_check": replay.unwrap_or_else(|| json!({
            "executed": false,
            "reason": "未执行（H2 通常已足够判定；需要时用 experiment 子命令补做）",
            "results": []
        })),
        "q2_verdict": q2,
        "explanation": explanation,
        "implication": implication
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h1_by_attr() {
        let inputs = json!([{"id":"mailNo","maxlength_attr":"21","maxLength_prop":21,"after_write_len":21}]);
        assert_eq!(h1_verdict(&inputs).0, "supported");
    }

    #[test]
    fn h1_by_framework_truncate() {
        let inputs = json!([{"id":"mailNo","maxlength_attr":null,"maxLength_prop":524288,"after_write_len":21}]);
        let (v, hint) = h1_verdict(&inputs);
        assert_eq!(v, "supported");
        assert_eq!(hint.as_deref(), Some("framework_truncated_to_21"));
    }

    #[test]
    fn h1_not_applicable() {
        let inputs = json!([{"id":"mailNo","maxlength_attr":null,"maxLength_prop":524288,"after_write_len":30}]);
        assert_eq!(h1_verdict(&inputs).0, "not_applicable");
    }
}
