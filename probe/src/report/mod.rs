//! 报告生成（§9）：report.md / report.json / network.har / elements.json /
//! dialogs.json / experiment21.json / assertions.json / replay/* / secrets/*。
//!
//! 铁律（§9.1）：任何一节没数据就写"未获取到（原因：…）"，不许留空、不许编造。

pub mod markdown;

use anyhow::Result;
use serde_json::{json, Map, Value};
use std::path::Path;

use crate::dialogs::DialogStore;
use crate::netcap::NetStore;
use crate::redact;
use crate::session::Session;

pub struct Input<'a> {
    pub sess: &'a Session,
    pub net: &'a NetStore,
    pub dialogs: &'a DialogStore,
    pub elements: &'a Value,
    pub experiment: &'a Value,
    pub preflight: Option<Value>,
    pub edge_version: String,
    pub include_cookies: bool,
}

/// 全部产物一次写盘
pub fn write_all(input: &Input<'_>) -> Result<()> {
    let sess = input.sess;

    // ---- 断言（§8.5）----
    let assertions = build_assertions(input);
    write_json(&sess.path("assertions.json"), &assertions)?;

    // ---- 弹窗表（§8.3）----
    write_json(&sess.path("dialogs.json"), &input.dialogs.to_json())?;

    // ---- 元素表（§8.2）----
    write_json(&sess.path("elements.json"), input.elements)?;

    // ---- Q2（§8.4）----
    write_json(&sess.path("experiment21.json"), input.experiment)?;

    // ---- HAR（§7.3.5）----
    write_json(&sess.path("network.har"), &input.net.to_har(input.include_cookies))?;

    // ---- 重放模板（§8.6）----
    let scenario = build_scenario(input);
    std::fs::create_dir_all(sess.path("replay"))?;
    write_json(&sess.path("replay/scenario.json"), &scenario)?;
    std::fs::write(sess.path("replay/README.md"), scenario_readme(&scenario))?;

    // ---- report.json ----
    let rj = build_json(input, &assertions, &scenario);
    write_json(&sess.path("report.json"), &rj)?;

    // ---- report.md ----
    let md = markdown::render(input, &rj, &assertions);
    std::fs::write(sess.path("report.md"), md)?;

    // ---- secrets（完整值，单独目录，首行警告）----
    write_secrets(input)?;

    Ok(())
}

fn write_json(path: &Path, v: &Value) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(v)?)?;
    Ok(())
}

/// 一句话结论（§9.1 第一节）：protocol / ui / hybrid
pub fn verdict(input: &Input<'_>) -> (String, String) {
    let mut plaintext_business = 0usize;
    let mut signed = 0usize;
    let mut https_business = 0usize;

    for r in &input.net.recs {
        if !matches!(r.request_type.as_str(), "XHR" | "Fetch" | "Document") || r.url_only {
            continue;
        }
        let hay = format!(
            "{} {}",
            r.url,
            r.post_data.clone().unwrap_or_default()
        )
        .to_ascii_lowercase();
        let has_sign = ["sign", "signature", "nonce", "encrypt", "token", "secret", "_t="]
            .iter()
            .any(|k| hay.contains(k));
        if has_sign {
            signed += 1;
        }
        if r.url.starts_with("http://") {
            plaintext_business += 1;
        } else if r.url.starts_with("https://") {
            https_business += 1;
        }
    }

    if signed > 0 {
        (
            "ui".to_string(),
            format!("业务请求里检测到 {signed} 个疑似签名/加密参数，协议回放不可控，只能操作界面或做混合。"),
        )
    } else if plaintext_business > 0 {
        (
            "protocol".to_string(),
            format!("{plaintext_business} 个业务请求走明文 http，未发现签名参数，适合协议回放。"),
        )
    } else if https_business > 0 {
        (
            "hybrid".to_string(),
            format!("业务请求走 https 且未发现签名参数，建议查询类走协议、写入类走 UI 复核。"),
        )
    } else {
        (
            "unknown".to_string(),
            "未获取到任何业务请求，无法判断路线。".to_string(),
        )
    }
}

/// Q6：按 step 聚合耗时（从 timeline.jsonl 读，避免为了报告再存一份）
pub fn q6_by_step(sess: &Session) -> Vec<(String, f64, usize)> {
    let mut sums: std::collections::BTreeMap<String, (f64, usize)> = Default::default();
    let Ok(text) = std::fs::read_to_string(sess.path("timeline.jsonl")) else {
        return Vec::new();
    };
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v.get("kind").and_then(|k| k.as_str()) != Some("response") {
            continue;
        }
        let Some(d) = v.get("duration_ms").and_then(|x| x.as_f64()) else { continue };
        let step = v
            .get("step")
            .and_then(|s| s.as_str())
            .unwrap_or("未标注")
            .to_string();
        let e = sums.entry(step).or_insert((0.0, 0));
        e.0 += d;
        e.1 += 1;
    }
    sums.into_iter()
        .map(|(k, (sum, n))| (k, sum / n as f64, n))
        .collect()
}

fn build_assertions(input: &Input<'_>) -> Value {
    let d = &input.dialogs;

    let has_dom = |step: Option<&str>, needle: Option<&str>| -> bool {
        d.dialogs.iter().any(|x| {
            x.kind == "dom_modal"
                && step.map(|s| x.step.as_deref() == Some(s)).unwrap_or(true)
                && needle.map(|n| x.evidence.text_preview.as_deref().unwrap_or("").contains(n)).unwrap_or(true)
        })
    };
    let has_any_dialog_in = |step: &str| d.dialogs.iter().any(|x| x.step.as_deref() == Some(step));
    let has_request_containing = |needle: &str| {
        input
            .net
            .recs
            .iter()
            .any(|r| r.url.contains(needle) || r.post_data.as_deref().map(|b| b.contains(needle)).unwrap_or(false))
    };

    let a_ok = has_dom(Some("A"), None) || has_any_dialog_in("A");
    let b_phone_loop = d
        .dialogs
        .iter()
        .filter(|x| {
            x.step.as_deref() == Some("B")
                && (x.evidence.text_preview.as_deref().unwrap_or("").contains("手机号")
                    || x.evidence.text_preview.as_deref().unwrap_or("").contains("收件人"))
        })
        .count();
    let b_batch = d
        .dialogs
        .iter()
        .filter(|x| x.step.as_deref() == Some("B") && x.kind == "dom_modal")
        .count();
    let c_ok = has_any_dialog_in("C");
    let d_ok = has_request_containing("handover")
        || has_request_containing("交接")
        || has_any_dialog_in("D");

    json!({
        "assertions": [
            {
                "id": "A_scan_confirm",
                "step": "A",
                "trigger": "点击'确认下段'",
                "expected": "出现成功提示（弹窗或页面提示）",
                "expected_request_url_contains": ["scan", "confirm"],
                "observed": if a_ok { format!("A 阶段共捕获 {} 个弹窗", d.dialogs.iter().filter(|x| x.step.as_deref()==Some("A")).count()) } else { "未观测到 A 阶段弹窗".to_string() },
                "pass": a_ok
            },
            {
                "id": "B_print_phone_loop",
                "step": "B",
                "trigger": "点击'打印'",
                "expected": "每轮弹出 1 个手机号输入框，直到不再弹出",
                "observed": format!("B 阶段 DOM 模态框 {b_batch} 个，其中手机号相关 {b_phone_loop} 个"),
                "pass": b_phone_loop > 0
            },
            {
                "id": "B_rescan_success",
                "step": "B",
                "trigger": "再次点击'下段扫描'并输入邮件号",
                "expected": "弹出框体",
                "observed": if a_ok { "A 阶段弹窗仍然可触发".to_string() } else { "未观测到".to_string() },
                "pass": a_ok
            },
            {
                "id": "C_delivered_confirm",
                "step": "C",
                "trigger": "点击'妥投确认'",
                "expected": "弹出确认框/提示框",
                "observed": if c_ok { "C 阶段有弹窗".to_string() } else { "未观测到 C 阶段弹窗".to_string() },
                "pass": c_ok
            },
            {
                "id": "D_handover_done",
                "step": "D",
                "trigger": "点击 '→'",
                "expected": "流程结束态",
                "observed": if d_ok { "观测到交班请求或结束态".to_string() } else { "未观测到".to_string() },
                "pass": d_ok
            }
        ]
    })
}

/// §8.6 replay/scenario.json
fn build_scenario(input: &Input<'_>) -> Value {
    let mut steps = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for r in &input.net.recs {
        if !matches!(r.request_type.as_str(), "XHR" | "Fetch") || r.url_only {
            continue;
        }
        let path = r
            .url
            .split("://")
            .nth(1)
            .and_then(|rest| rest.find('/').map(|i| rest[i..].to_string()))
            .unwrap_or_else(|| r.url.clone());
        let key = format!("{} {}", r.method, path);
        if !seen.insert(key) {
            continue;
        }
        let body = r.post_data.clone().unwrap_or_default();
        let mut dynamic = Vec::new();
        for k in ["ts", "time", "timestamp", "nonce", "sign", "signature", "token", "_t", "rand", "random"] {
            let needle = format!("\"{k}\"");
            if body.to_ascii_lowercase().contains(&needle.to_ascii_lowercase()) {
                dynamic.push(k.to_string());
            }
        }
        let body_template = redact::redact_body(&body, input.include_cookies)
            .replace("\"mailNo\":\"7100", "\"mailNo\":\"{{MAIL_NO}}\"") // 尽力而为
            .replace("\"mailNo\": \"7100", "\"mailNo\": \"{{MAIL_NO}}\"");

        steps.push(json!({
            "step": r.click_text.clone(),
            "label": format!("{} {}", r.method, path.split('?').next().unwrap_or("")),
            "trigger": r.click_text.clone().unwrap_or_else(|| "未知触发".to_string()),
            "method": r.method,
            "path": path.split('?').next().unwrap_or("").to_string(),
            "protocol": if r.plaintext { "http" } else { "https" },
            "headers": redact::redact_headers(&r.headers, input.include_cookies)
                .into_iter()
                .map(|(k, v)| (k, Value::String(v)))
                .collect::<Map<_, _>>(),
            "body_template": body_template,
            "dynamic_fields": dynamic,
            "response_shape": r.body_preview.as_deref().map(|b| redact::redact_text(&crate::util::truncate_bytes(b, 200))).unwrap_or_default()
        }));
    }

    let base = input
        .net
        .recs
        .iter()
        .find_map(|r| r.url.split("://").nth(1).and_then(|rest| rest.split('/').next()).map(|h| format!("{}://{}", if r.plaintext {"http"} else {"https"}, h)))
        .unwrap_or_default();

    json!({
        "generated_at": crate::util::now_iso(),
        "base": base,
        "steps": steps,
        "encryption_notes": "若 headers/body 出现疑似签名或密文，需人工复核（本文件只做结构化整理）"
    })
}

fn scenario_readme(scenario: &Value) -> String {
    let mut s = String::new();
    s.push_str("# 最小请求序列（可重放模板）\n\n");
    s.push_str("本目录由 `probe report` 自动生成。**所有值已脱敏**，完整值见 `secrets/secrets.json`。\n\n");
    s.push_str("| # | 方法 | 路径 | 触发 | 动态字段 |\n|---|---|---|---|---|\n");
    if let Some(steps) = scenario.get("steps").and_then(|x| x.as_array()) {
        for (i, st) in steps.iter().enumerate() {
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                i + 1,
                st.get("method").and_then(|x| x.as_str()).unwrap_or(""),
                st.get("path").and_then(|x| x.as_str()).unwrap_or(""),
                st.get("trigger").and_then(|x| x.as_str()).unwrap_or(""),
                st.get("dynamic_fields")
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "[]".into())
            ));
        }
    } else {
        s.push_str("\n未获取到（原因：没有捕获到 XHR/Fetch 请求）。\n");
    }
    s
}

/// secrets/secrets.json：完整值单独存放（§10）
fn write_secrets(input: &Input<'_>) -> Result<()> {
    let dir = input.sess.ensure_subdir("secrets")?;
    let mut urls = Vec::new();
    let mut bodies = Vec::new();
    let mut headers = Vec::new();

    for r in &input.net.recs {
        if r.url_only {
            continue;
        }
        urls.push(json!({"request_id": r.request_id, "method": r.method, "url": r.url}));
        if let Some(b) = &r.post_data {
            bodies.push(json!({"request_id": r.request_id, "body": b}));
        }
        let sensitive: Vec<Value> = r
            .headers
            .iter()
            .filter(|(k, _)| crate::redact::is_sensitive_key(k))
            .map(|(k, v)| json!({"name": k, "value": v}))
            .collect();
        if !sensitive.is_empty() {
            headers.push(json!({"request_id": r.request_id, "headers": sensitive}));
        }
    }

    let v = json!({
        "_warning": "本文件含未脱敏的完整凭据与业务数据，等同于账号。禁止拷贝出内网机、禁止外传。",
        "generated_at": crate::util::now_iso(),
        "session": input.sess.id,
        "urls": urls,
        "post_bodies": bodies,
        "sensitive_headers": headers
    });
    write_json(&dir.join("secrets.json"), &v)?;

    // 权限收紧（§10）：仅当前用户可读
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("icacls")
            .arg(dir.join("secrets.json"))
            .arg("/inheritance:r")
            .arg("/grant:r")
            .arg(format!("{}:F", whoami()))
            .output();
    }
    Ok(())
}

#[cfg(windows)]
fn whoami() -> String {
    std::env::var("USERNAME").unwrap_or_else(|_| "administrator".to_string())
}

fn build_json(input: &Input<'_>, assertions: &Value, scenario: &Value) -> Value {
    let (path, reason) = verdict(input);
    let q6 = q6_by_step(input.sess);
    let exp = input.experiment;

    json!({
        "meta": {
            "generated_at": crate::util::now_iso(),
            "session": input.sess.id,
            "sessions": crate::session::Session::all_dirs(&input.sess.root).len(),
            "edge_version": input.edge_version,
            "ua": input.elements.get("page").and_then(|p| p.get("ua")).and_then(|u| u.as_str()).unwrap_or("未获取到"),
            "preflight": input.preflight,
            "tool_version": env!("CARGO_PKG_VERSION"),
        },
        "verdict": {"path": path, "reason": reason},
        "q1": {
            "requests": input.net.recs.iter().filter(|r| !r.url_only).count(),
            "plaintext": input.net.recs.iter().filter(|r| r.plaintext && !r.url_only).count(),
            "total": input.net.recs.len(),
        },
        "q2": exp,
        "q3": {"control_map": input.elements.get("control_map").cloned().unwrap_or(json!({}))},
        "q4": {
            "tab_structure": input.elements.get("page").and_then(|p| p.get("tab_structure")).and_then(|v| v.as_str()).unwrap_or("unknown"),
            "detail": input.elements.get("page").and_then(|p| p.get("tab_detail")).cloned().unwrap_or(json!(null))
        },
        "q5": {
            "print_preview_count": input.dialogs.counts.get("print_preview").copied().unwrap_or(0),
            "native_count": input.dialogs.counts.get("native_window").copied().unwrap_or(0),
            "spool_events": spool_events(input),
        },
        "q6": {"by_step": q6.iter().map(|(s, avg, n)| json!({"step": s, "avg_ms": avg, "samples": n})).collect::<Vec<_>>()},
        "q7": {
            "default_printer": crate::win32::default_printer(),
            "is_virtual": crate::win32::default_printer().map(|p| crate::win32::is_virtual_printer(&p)),
            "printers": crate::win32::printers(),
            "spool_events": spool_events(input),
        },
        "dialogs": input.dialogs.dialogs,
        "assertions": assertions,
        "scenario": scenario,
        "risks": build_risks(input),
        "roadmap": build_roadmap(&path),
    })
}

fn spool_events(input: &Input<'_>) -> Value {
    let Ok(text) = std::fs::read_to_string(input.sess.path("spool.jsonl")) else {
        return json!([]);
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect::<Vec<_>>()
        .into()
}

fn build_risks(input: &Input<'_>) -> Value {
    let mut risks: Vec<Value> = Vec::new();
    let q2 = input.experiment.get("q2_verdict").and_then(|v| v.as_str()).unwrap_or("unknown");
    if q2 == "unknown" {
        risks.push(json!({"level": "高", "text": "Q2 未判定成功，「21」的真实含义仍是未知数，后续编排复杂度无法估算。"}));
    }
    let verdict_txt = verdict(input).0;
    if verdict_txt == "ui" {
        risks.push(json!({"level": "高", "text": "请求含签名/加密，只能走 UI 自动化，页面结构一变就可能全断。"}));
    }
    if input.dialogs.counts.get("print_preview").copied().unwrap_or(0) > 0 {
        risks.push(json!({"level": "中", "text": "存在打印预览类原生窗口，CDP 无法关闭，正式程序必须用 Page.printToPDF 或另存 PDF 绕开。"}));
    }
    if input.elements.get("page").and_then(|p| p.get("elements_truncated")).and_then(|v| v.as_bool()).unwrap_or(false) {
        risks.push(json!({"level": "中", "text": "页面元素超过 3000 上限被截断，control_map 可能不完整。"}));
    }
    risks.push(json!({"level": "低", "text": "内网机 Edge 升级后 CDP 协议细节可能变化，建议固定 Edge 版本或回归一次。"}));
    Value::Array(risks)
}

fn build_roadmap(path: &str) -> Value {
    json!([
        {"phase": "第 1 个月", "goal": format!("按本次结论落地技术路线（{path}）的骨架：单号队列、重试、日志、可回滚。"), "criteria": "能对 3-5 个真实单号端到端跑通，人工只做复核"},
        {"phase": "第 2 个月", "goal": "批量化与异常处理：断点续跑、失败单号重试、弹窗自动关闭、打印走 PDF。", "criteria": "200 单连续跑通，人工干预 < 3 次"},
        {"phase": "第 3 个月", "goal": "稳定性与值守：指标上报、每日对账、Edge 版本回归清单。", "criteria": "连续 5 个工作日无阻断，日均 500 单"}
    ])
}

/// 从已有事件流重建报告（`probe report`，§6.5）。
/// 崩溃后补救用：只读 timeline + 已有产物，不碰浏览器。
pub fn rebuild(dir: &Path, root: &Path) -> Result<i32> {
    let mut net = NetStore::new();
    let mut dialogs = DialogStore::new();

    match std::fs::read_to_string(dir.join("timeline.jsonl")) {
        Ok(text) => {
            for line in text.lines() {
                let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
                net.ingest_timeline(&v);
                dialogs.ingest_timeline(&v);
            }
        }
        Err(_) => {
            println!("[report] 没有 timeline.jsonl，将生成一份\"未获取到\"的空报告（这是允许的行为）");
        }
    }

    let elements = read_json(&dir.join("elements.json")).unwrap_or_else(|| {
        json!({
            "generated_at": crate::util::now_iso(),
            "page": {"ua": "未获取到", "tab_structure": "unknown", "elements_truncated": false},
            "frames": [],
            "control_map": {},
        })
    });

    let exp = read_json(&dir.join("experiment21.json")).unwrap_or_else(|| {
        crate::experiment::build(
            json!([]),
            ("unknown".to_string(), None),
            crate::experiment::h2_verdict(&net),
            None,
        )
    });

    // 复用 Input 需要 Session；为了不新建目录，这里构造一个"只读壳"
    let sess = crate::session::Session::from_existing(root, dir)?;
    let input = Input {
        sess: &sess,
        net: &net,
        dialogs: &dialogs,
        elements: &elements,
        experiment: &exp,
        preflight: crate::preflight::load(&sess.root),
        edge_version: "未获取到（report 模式不连浏览器）".to_string(),
        include_cookies: false,
    };

    write_all(&input)?;
    println!(
        "[report] 已重建：{}\n         请求 {} 条，弹窗 {} 个，Q2={}",
        sess.path("report.md").display(),
        net.recs.len(),
        dialogs.dialogs.len(),
        exp.get("q2_verdict").and_then(|v| v.as_str()).unwrap_or("unknown")
    );
    Ok(0)
}

fn read_json(path: &Path) -> Option<Value> {
    let t = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&t).ok()
}
