//! API.md —— 从 HAR 反推接口契约与"能不能协议回放"（PROMPT §6A.6）。

use serde_json::Value;
use std::collections::BTreeMap;

use super::actions::Action;

const DYN_KEYS: &[&str] = &[
    "ts", "time", "timestamp", "nonce", "sign", "signature", "token", "_t", "rand", "random",
];

pub struct ApiGroup {
    pub method: String,
    pub path: String,
    pub samples: usize,
    pub status_ok: usize,
    pub req_body: Option<String>,
    pub resp_body: Option<String>,
    pub triggers: Vec<String>,
    pub dynamic: Vec<String>,
    pub signed: bool,
    pub har_entries: Vec<usize>,
}

fn path_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .and_then(|r| r.find('/').map(|i| r[i..].to_string()))
        .unwrap_or_else(|| url.to_string())
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .to_string()
}

pub fn group(har: &[Value], acts: &[Action]) -> Vec<ApiGroup> {
    let mut map: BTreeMap<(String, String), ApiGroup> = BTreeMap::new();

    for (idx, e) in har.iter().enumerate() {
        let req = match e.get("request") {
            Some(r) => r,
            None => continue,
        };
        let probe = e.get("_probe").cloned().unwrap_or(Value::Null);
        let url_only = probe.get("urlOnly").and_then(|v| v.as_bool()).unwrap_or(false);
        if url_only {
            continue;
        }
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("GET").to_string();
        let url = req.get("url").and_then(|u| u.as_str()).unwrap_or("");
        let path = path_of(url);
        // 只保留业务接口：POST / 或带 api 的 GET
        if method == "GET" && !path.contains("api") {
            continue;
        }
        let key = (method.clone(), path.clone());
        let g = map.entry(key).or_insert_with(|| ApiGroup {
            method: method.clone(),
            path: path.clone(),
            samples: 0,
            status_ok: 0,
            req_body: None,
            resp_body: None,
            triggers: Vec::new(),
            dynamic: Vec::new(),
            signed: false,
            har_entries: Vec::new(),
        });
        g.samples += 1;
        g.har_entries.push(idx);
        let status = e
            .get("response")
            .and_then(|r| r.get("status"))
            .and_then(|s| s.as_i64())
            .unwrap_or(0);
        if (200..400).contains(&status) {
            g.status_ok += 1;
        }
        if g.req_body.is_none() {
            g.req_body = req
                .get("postData")
                .and_then(|p| p.get("text"))
                .and_then(|t| t.as_str())
                .map(|s| s.to_string());
        }
        if g.resp_body.is_none() {
            g.resp_body = e
                .get("response")
                .and_then(|r| r.get("content"))
                .and_then(|c| c.get("text"))
                .and_then(|t| t.as_str())
                .filter(|t| !t.is_empty())
                .map(|s| s.to_string());
        }

        // 触发动作：靠 clickText / causedByClickSeq 找到对应动作名
        let click_seq = probe.get("causedByClickSeq").and_then(|v| v.as_u64());
        let click_text = probe.get("clickText").and_then(|v| v.as_str());
        let name = click_seq
            .and_then(|s| acts.iter().find(|a| a.seq == s).map(|a| a.name.clone()))
            .or_else(|| click_text.map(|t| t.to_string()));
        if let Some(n) = name {
            if !g.triggers.contains(&n) {
                g.triggers.push(n);
            }
        }

        // 签名/动态字段
        let body = g.req_body.clone().unwrap_or_default();
        let headers = req.get("headers").cloned().unwrap_or(Value::Null).to_string();
        let hay = format!("{body} {headers}").to_ascii_lowercase();
        if ["sign", "nonce", "encrypt", "signature"].iter().any(|k| hay.contains(k)) {
            g.signed = true;
        }
        for k in dynamic_fields(&body) {
            if !g.dynamic.contains(&k) {
                g.dynamic.push(k);
            }
        }
    }

    map.into_values().collect()
}

/// 动态字段识别（§6A.6）：键名命中或值看起来像时间/随机
pub fn dynamic_fields(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(body) {
        for (k, v) in m {
            let kl = k.to_ascii_lowercase();
            let by_name = DYN_KEYS.iter().any(|d| d.eq_ignore_ascii_case(&kl));
            // 只认"毫秒时间戳量级"的数字。
            // 字符串不能只看长度：`7100*********`（脱敏单号）和中文模板名都会误判。
            // 真正的随机值判定需要多份样本对比，而那属于"判断"，离线阶段只做保守识别。
            let by_value = match &v {
                Value::Number(n) => n
                    .as_i64()
                    .map(|x| x > 1_600_000_000_000)
                    .unwrap_or(false),
                _ => false,
            };
            if by_name || by_value {
                out.push(k);
            }
        }
    }
    out.sort();
    out
}

pub fn render(har: &[Value], acts: &[Action], _elements: &Value) -> String {
    let groups = group(har, acts);
    let mut md = String::new();
    md.push_str("# 接口清单与调用契约\n\n");
    md.push_str("> 请求/响应体是**脱敏后**的样本（单号保留前 4 位）。真实值不在包里，按占位符实现即可。\n\n");

    if groups.is_empty() {
        md.push_str("未获取到任何业务接口（原因：本次没有捕获到 XHR/Fetch 请求）。\n");
        return md;
    }

    for g in &groups {
        md.push_str(&format!("## {} {}\n\n", g.method, g.path));
        md.push_str(&format!("- 触发动作：{}\n", if g.triggers.is_empty() { "未关联到具体动作".to_string() } else { g.triggers.join(" / ") }));
        md.push_str(&format!(
            "- 样本数：{}（成功 {}/{}）\n",
            g.samples, g.status_ok, g.samples
        ));
        if let Some(b) = &g.req_body {
            md.push_str(&format!("- 请求体（实测样本）：\n\n  ```json\n  {}\n  ```\n", crop(b, 600)));
        }
        if let Some(b) = &g.resp_body {
            md.push_str(&format!("- 响应体（实测样本）：\n\n  ```json\n  {}\n  ```\n", crop(b, 600)));
        }
        md.push_str(&format!(
            "- 动态字段：{}\n",
            if g.dynamic.is_empty() {
                "**未检测到**".to_string()
            } else {
                format!("**{}**（每次请求会变，回放时必须重新生成）", g.dynamic.join("、"))
            }
        ));
        md.push_str(&format!(
            "- 签名：{}\n",
            if g.signed {
                "**疑似有 sign/nonce 类字段，需读源码确认**（搜 `RAW/mirror/` 里对应 JS）"
            } else {
                "未检测到 sign / nonce / 加密字段"
            }
        ));
        let verdict = if g.signed {
            "**需复现签名**（先看源码确认，再决定是否值得）"
        } else if g.dynamic.is_empty() {
            "**可直接协议回放**"
        } else {
            "**可回放**，但每次需重新生成动态字段"
        };
        md.push_str(&format!("- 结论：{verdict}\n\n---\n\n"));
    }

    md
}

fn crop(s: &str, n: usize) -> String {
    let t = crate::util::truncate_bytes(s, n).to_string();
    t.replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_fields_by_name_and_value() {
        let body = r#"{"ts":1794897871136,"nonce":"abc12345xyz","template":"国内快包面单-正式"}"#;
        let d = dynamic_fields(body);
        assert!(d.contains(&"ts".to_string()));
        assert!(d.contains(&"nonce".to_string()));
        assert!(!d.contains(&"template".to_string()));
    }

    #[test]
    fn dynamic_fields_ignore_plain_fields() {
        let d = dynamic_fields(r#"{"code":0,"msg":"成功","mailNo":"7100*********"}"#);
        assert!(d.is_empty(), "实际 {d:?}");
    }
}
