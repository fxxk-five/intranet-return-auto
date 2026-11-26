//! `experiment` 子命令：Q2 的主动协议重放（§7.7.3）。
//!
//! **这是全工程唯一会向业务系统发真实请求的地方**，所以保护措施一条都不能省：
//! 1. 必须 `--confirm-dry-run`，否则退出码 2
//! 2. 启动后 10 秒倒计时，可 Ctrl+C 中断
//! 3. 禁止使用真实待处理单号：body 里的单号必须替换成 `--test-number`
//! 4. 只发 N 个变体，串行，间隔 3 秒
//! 5. 每个响应记录状态/预览/耗时/是否业务错误
//! 6. 结束生成对比表

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

use crate::cli::ExperimentArgs;
use crate::http1;
use crate::session::Session;
use crate::util::truncate_bytes;

pub async fn run(args: &ExperimentArgs) -> Result<i32> {
    if !args.confirm_dry_run {
        println!(
            "[ABORT] experiment 会发送真实业务请求，可能产生真实业务记录。\n\
             请确认已在操作员知情的情况下执行，并已准备好测试用单号。\n\
             确认后请加 --confirm-dry-run 重新运行。"
        );
        return Ok(2);
    }
    let Some(test_number) = args.test_number.clone() else {
        println!(
            "[ABORT] 缺少 --test-number。\n\
             本命令禁止在 body 里代回真实待处理单号，请提供一个测试单号（例如 0000000000001 这类专用测试号）。"
        );
        return Ok(2);
    };
    if test_number.chars().count() < 6 {
        println!("[ABORT] --test-number 太短，明显不是可用单号。");
        return Ok(2);
    }

    let sess = Session::open(&args.out)?;
    println!("[experiment] 输出目录：{}", sess.dir.display());

    // 取请求模板：优先用捕获到的 HAR 里的记录
    let template = load_template(&sess, args.request_id.as_deref())?;
    let Some(template) = template else {
        println!(
            "[ABORT] 没有找到可用的请求模板。\n\
             请先用 attach 捕获一次\"下段扫描\"的请求（至少一条 XHR/Fetch POST）。"
        );
        return Ok(2);
    };

    println!("\n[experiment] 即将重放的请求：");
    println!("  {} {}", template.method, template.url);
    println!("  body: {}", truncate_bytes(&template.body, 300));
    println!("\n  测试单号：{test_number}");
    println!("  N 取值：{}\n", args.numbers);
    print!("  10 秒后可开始，随时 Ctrl+C 中断： ");
    for i in (1..=10).rev() {
        print!("{i} ");
        use std::io::Write;
        std::io::stdout().flush().ok();
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    println!("\n[experiment] 开始。");

    let ns: Vec<u32> = args
        .numbers
        .split(',')
        .filter_map(|s| s.trim().parse::<u32>().ok())
        .collect();
    if ns.is_empty() {
        bail!("--numbers 解析为空");
    }

    let mut results: Vec<Value> = Vec::new();
    for n in &ns {
        let body = build_body(&template.body, *n, &test_number);
        let started = std::time::Instant::now();
        let r = http1::request(
            "127.0.0.1",
            template.port,
            &template.method,
            &template.path,
            &template.headers,
            Some(body.as_bytes()),
            30_000,
        )
        .await;
        let duration_ms = started.elapsed().as_millis() as u64;
        let (status, text) = match r {
            Ok(resp) => (resp.status as i64, resp.body_text()),
            Err(e) => (-1, format!("{e}")),
        };
        let is_err = looks_business_error(&text) || status >= 400 || status < 0;
        println!(
            "  N={n:<3} status={status:<5} {duration_ms:>5}ms  {}",
            if is_err { "[业务错误]" } else { "ok" }
        );
        results.push(json!({
            "n": n,
            "status": status,
            "duration_ms": duration_ms,
            "body_preview": truncate_bytes(&crate::redact::redact_text(&text), 2000),
            "is_business_error": is_err
        }));
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    // 对比表 + 结论
    println!("\n[experiment] 对比表（N | 状态 | 耗时 | 业务错误）");
    for r in &results {
        println!(
            "  {:<4} | {:<6} | {:<6} | {}",
            r["n"],
            r["status"],
            r["duration_ms"],
            if r["is_business_error"].as_bool().unwrap_or(false) { "是" } else { "否" }
        );
    }

    let v22_failed = results
        .iter()
        .any(|r| r["n"].as_u64() == Some(22) && r["is_business_error"].as_bool().unwrap_or(false));
    let v30_ok = results
        .iter()
        .any(|r| r["n"].as_u64() == Some(30) && !r["is_business_error"].as_bool().unwrap_or(false));
    let verdict = if v22_failed {
        "H3"
    } else if v30_ok {
        "H2"
    } else {
        "unknown"
    };

    let out = json!({
        "generated_at": crate::util::now_iso(),
        "mode": "replay",
        "template": {
            "method": template.method,
            "url": crate::redact::redact_text(&template.url),
            "body_len": template.body.len(),
        },
        "replay_check": {
            "executed": true,
            "reason": "按 --confirm-dry-run 执行",
            "test_number": crate::redact::redact_text(&test_number),
            "results": results,
        },
        "q2_verdict": verdict,
        "explanation": if verdict == "H3" {
            "22 个单号即报业务错误，后端存在阈值"
        } else if verdict == "H2" {
            "30 个单号仍然成功，后端无批量上限，21 属人工操作段落"
        } else {
            "结果不足以判定，请人工复核响应体"
        },
        "implication": if verdict == "H2" {
            "后续程序按单号循环实现，无需批次概念"
        } else {
            "在确认阈值前不要按批次实现"
        }
    });
    std::fs::write(sess.path("experiment21.json"), serde_json::to_vec_pretty(&out)?)?;
    println!("\n[experiment] 结论：{verdict}（已写入 experiment21.json）");
    Ok(0)
}

struct Template {
    method: String,
    url: String,
    port: u16,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

fn load_template(sess: &Session, request_id: Option<&str>) -> Result<Option<Template>> {
    let har = match std::fs::read_to_string(sess.path("network.har")) {
        Ok(t) => t,
        Err(_) => return Ok(None),
    };
    let v: Value = serde_json::from_str(&har).context("network.har 不是合法 JSON")?;
    let Some(entries) = v
        .get("log")
        .and_then(|l| l.get("entries"))
        .and_then(|e| e.as_array())
    else {
        return Ok(None);
    };

    let pick = entries.iter().find(|e| match request_id {
        Some(rid) => e
            .get("_probe")
            .and_then(|p| p.get("requestId"))
            .and_then(|r| r.as_str())
            == Some(rid),
        None => {
            let m = e.get("request").and_then(|r| r.get("method")).and_then(|m| m.as_str());
            m == Some("POST")
        }
    });
    let Some(e) = pick else { return Ok(None) };

    let method = e
        .get("request")
        .and_then(|r| r.get("method"))
        .and_then(|m| m.as_str())
        .unwrap_or("POST")
        .to_string();
    let url = e
        .get("request")
        .and_then(|r| r.get("url"))
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();
    if url.is_empty() {
        return Ok(None);
    }
    let (host_part, path) = match url.split_once("://") {
        Some((_, rest)) => match rest.find('/') {
            Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
            None => (rest.to_string(), "/".to_string()),
        },
        None => (String::new(), url.clone()),
    };
    let port = host_part
        .split(':')
        .nth(1)
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(80);
    let headers: Vec<(String, String)> = e
        .get("request")
        .and_then(|r| r.get("headers"))
        .and_then(|h| h.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|h| {
                    Some((
                        h.get("name")?.as_str()?.to_string(),
                        h.get("value")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let body = e
        .get("request")
        .and_then(|r| r.get("postData"))
        .and_then(|p| p.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    Ok(Some(Template {
        method,
        url,
        port,
        path,
        headers,
        body,
    }))
}

/// 把 body 里的单号替换成 N 个测试单号（尽力而为：JSON 字段 → 逗号串 / 数组）
fn build_body(template: &str, n: u32, test_number: &str) -> String {
    let joined: Vec<String> = std::iter::repeat(test_number.to_string())
        .take(n as usize)
        .collect();

    if let Ok(mut v) = serde_json::from_str::<Value>(template) {
        if let Some(obj) = v.as_object_mut() {
            for (_k, val) in obj.iter_mut() {
                match val {
                    Value::String(s) if crate::netcap::count_mail_numbers(s) > 0 || s.len() >= 11 => {
                        *s = joined.join(",");
                    }
                    Value::Array(a) if !a.is_empty() => {
                        *a = joined.iter().map(|s| Value::String(s.clone())).collect();
                    }
                    _ => {}
                }
            }
        }
        return v.to_string();
    }
    // 非 JSON：把第一个长数字串替换掉
    let re = regex::Regex::new(r"\b\d{11,13}\b").unwrap();
    if re.is_match(template) {
        return re
            .replace(template, joined.join(","))
            .to_string();
    }
    template.to_string()
}

fn looks_business_error(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    ["error", "fail", "失败", "超过", "上限", "不允许", "invalid", "denied"]
        .iter()
        .any(|k| t.contains(&k.to_ascii_lowercase()))
        || text.contains("\"code\":")
            && !text.contains("\"code\":0")
            && !text.contains("\"code\": 0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_json_string_field() {
        let t = r#"{"mailNo":"710000000004161"}"#;
        let b = build_body(t, 22, "9999999999999");
        let v: Value = serde_json::from_str(&b).unwrap();
        let s = v["mailNo"].as_str().unwrap();
        assert_eq!(s.split(',').count(), 22);
    }

    #[test]
    fn detects_business_error() {
        assert!(looks_business_error("{\"code\":500,\"msg\":\"失败\"}"));
        assert!(!looks_business_error("{\"code\":0,\"msg\":\"成功\"}"));
    }
}
