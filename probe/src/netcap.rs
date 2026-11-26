//! 网络取证（§7.3）：把 CDP 的 Network.* 事件落成结构化记录 + HAR。
//!
//! 这一模块直接回答 Q1（请求长什么样、有没有签名）和 Q2-H2（一次"确定"
//! 到底发几个请求、每个请求几个单号）。

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

use crate::cdp::events::Event;
use crate::redact;
use crate::util::{now_iso_utc, sha256_hex, truncate_bytes};

const MAX_BODY: usize = 2 * 1024 * 1024; // §7.3.4：> 2MB 截断

static RE_HEARTBEAT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)/(heartbeat|ping|track|log|monitor|report)[\\/?]").unwrap());
static RE_MAILNO: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b\d{11,16}\b").unwrap());

#[derive(Debug, Clone)]
pub struct ReqRecord {
    pub request_id: String,
    pub seq: u64,
    pub ts: String,
    pub ts_mono: u64,
    pub method: String,
    pub url: String,
    pub request_type: String,
    pub post_data: Option<String>,
    pub headers: Vec<(String, String)>,
    pub plaintext: bool,
    pub status: Option<i64>,
    pub status_text: Option<String>,
    pub mime: Option<String>,
    pub encoded_len: Option<i64>,
    /// HAR 的 startedDateTime（UTC ISO8601）
    pub started_utc: String,
    pub duration_ms: Option<f64>,
    pub body_preview: Option<String>,
    pub body_status: String,
    pub body_truncated: bool,
    /// 正文已完整落盘时的相对路径（相对 session 目录）
    pub body_file: Option<String>,
    pub body_sha256: Option<String>,
    pub caused_by_click_seq: Option<u64>,
    pub click_text: Option<String>,
    pub numbers_in_body: usize,
    /// 只记 URL 的（静态资源/心跳）
    pub url_only: bool,
}

pub struct NetStore {
    pub recs: Vec<ReqRecord>,
    index: HashMap<String, usize>,
    wall_start: f64,
}

/// Q2 判定用的统计
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ScanStats {
    /// 与"确定"类点击关联的请求数
    pub scan_requests: usize,
    /// 这些请求里每个请求包含的单号数量
    pub numbers_per_request: Vec<usize>,
    /// 观测到的"确定"点击次数
    pub ok_clicks: usize,
    /// 有没有出现一次点击触发多个请求的情况
    pub multi_request_clicks: usize,
}

fn is_url_only_type(t: &str) -> bool {
    matches!(t, "Script" | "Stylesheet" | "Image" | "Font" | "Media" | "Manifest" | "Other")
}

fn is_heartbeat(url: &str) -> bool {
    RE_HEARTBEAT.is_match(url)
}

/// 统计一段文本里出现的单号个数（兜底用，不看字段名）
pub fn count_mail_numbers(text: &str) -> usize {
    RE_MAILNO.find_iter(text).count()
}

static RE_MAIL_KEY: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)mail|waybill|track|number|单号|运单").unwrap());

/// 请求体里的单号个数。
///
/// 优先只数"键名像单号"的字段：否则 `{"ts":1794897871136}` 这种毫秒时间戳
/// 也会被当成 13 位邮件号，把 Q2 的统计直接带偏（自测时真的踩到了）。
pub fn count_numbers_in_body(body: &str) -> usize {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        let mut n = 0usize;
        collect_from_json(&v, None, &mut n);
        return n;
    }
    count_mail_numbers(body)
}

fn collect_from_json(v: &Value, key: Option<&str>, out: &mut usize) {
    match v {
        Value::Object(m) => {
            for (k, val) in m {
                collect_from_json(val, Some(k), out);
            }
        }
        Value::Array(a) => {
            for x in a {
                collect_from_json(x, key, out);
            }
        }
        Value::String(s) => {
            let keyed = key.map(|k| RE_MAIL_KEY.is_match(k)).unwrap_or(false);
            if keyed {
                *out += RE_MAILNO.find_iter(s).count();
            }
        }
        Value::Number(num) => {
            let keyed = key.map(|k| RE_MAIL_KEY.is_match(k)).unwrap_or(false);
            if keyed {
                let s = num.to_string();
                if (11..=16).contains(&s.chars().count()) {
                    *out += 1;
                }
            }
        }
        _ => {}
    }
}

impl NetStore {
    pub fn new() -> NetStore {
        NetStore {
            recs: Vec::new(),
            index: HashMap::new(),
            wall_start: 0.0,
        }
    }

    pub fn get(&self, request_id: &str) -> Option<&ReqRecord> {
        self.index.get(request_id).map(|i| &self.recs[*i])
    }

    pub fn get_mut(&mut self, request_id: &str) -> Option<&mut ReqRecord> {
        let idx = *self.index.get(request_id)?;
        self.recs.get_mut(idx)
    }

    /// `Network.requestWillBeSent`
    pub fn on_request(&mut self, ev: &Event, seq: u64, mono: u64) -> Option<usize> {
        let request_id = ev.str("requestId")?.to_string();
        if self.index.contains_key(&request_id) {
            // 重定向会复用 requestId，只补记录不新建
            return self.index.get(&request_id).copied();
        }
        let req = ev.get("request")?;
        let url = req.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("GET").to_string();
        let rtype = ev.str("type").unwrap_or("Other").to_string();
        let post_data = req
            .get("postData")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let headers: Vec<(String, String)> = req
            .get("headers")
            .and_then(|h| h.as_object())
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let url_only = is_url_only_type(&rtype) || is_heartbeat(&url);
        let plaintext = url.starts_with("http://");

        // 静态资源只记 URL + 状态码（§7.3.3）
        let post_data = if url_only { None } else { post_data };
        let numbers_in_body = post_data.as_deref().map(count_numbers_in_body).unwrap_or(0);

        let rec = ReqRecord {
            request_id: request_id.clone(),
            seq,
            ts: crate::util::now_iso(),
            ts_mono: mono,
            method,
            url,
            request_type: rtype,
            post_data,
            headers,
            plaintext,
            status: None,
            status_text: None,
            mime: None,
            encoded_len: None,
            started_utc: now_iso_utc(),
            duration_ms: None,
            body_preview: None,
            body_status: "pending".to_string(),
            body_truncated: false,
            body_file: None,
            body_sha256: None,
            caused_by_click_seq: None,
            click_text: None,
            numbers_in_body,
            url_only,
        };
        self.recs.push(rec);
        let idx = self.recs.len() - 1;
        self.index.insert(request_id, idx);
        Some(idx)
    }

    /// `Network.requestWillBeSentExtraInfo`（cookie 常只出现在这里）
    pub fn on_extra_info(&mut self, ev: &Event) {
        let Some(rid) = ev.str("requestId") else { return };
        let Some(idx) = self.index.get(rid).copied() else { return };
        if let Some(h) = ev.get("headers").and_then(|v| v.as_object()) {
            for (k, v) in h {
                let val = v.as_str().unwrap_or("").to_string();
                if !self.recs[idx].headers.iter().any(|(ek, _)| ek.eq_ignore_ascii_case(k)) {
                    self.recs[idx].headers.push((k.clone(), val));
                }
            }
        }
    }

    /// `Network.responseReceived`
    pub fn on_response(&mut self, ev: &Event) -> Option<usize> {
        let rid = ev.str("requestId")?.to_string();
        let idx = *self.index.get(&rid)?;
        if let Some(r) = ev.get("response") {
            self.recs[idx].status = r.get("status").and_then(|v| v.as_i64());
            self.recs[idx].status_text = r
                .get("statusText")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            self.recs[idx].mime = r.get("mimeType").and_then(|v| v.as_str()).map(|s| s.to_string());
            self.recs[idx].encoded_len = r.get("encodedDataLength").and_then(|v| v.as_i64());
            if let Some(t) = r.get("timing").and_then(|t| t.get("receiveHeadersEnd")).and_then(|v| v.as_f64()) {
                if t > 0.0 {
                    self.recs[idx].duration_ms = Some(t);
                }
            }
        }
        Some(idx)
    }

    /// `Network.loadingFinished`。返回 (idx, 是否需要抓响应体)
    pub fn on_finished(&mut self, ev: &Event) -> Option<(usize, bool)> {
        let rid = ev.str("requestId")?.to_string();
        let idx = *self.index.get(&rid)?;
        if let Some(len) = ev.get("encodedDataLength").and_then(|v| v.as_i64()) {
            self.recs[idx].encoded_len = Some(len);
        }
        let r = &self.recs[idx];
        let want_body = !r.url_only
            && matches!(r.request_type.as_str(), "Document" | "XHR" | "Fetch")
            && r.status.map(|s| s < 400).unwrap_or(true);
        Some((idx, want_body))
    }

    /// `Network.loadingFailed`
    pub fn on_failed(&mut self, ev: &Event) -> Option<usize> {
        let rid = ev.str("requestId")?.to_string();
        let idx = *self.index.get(&rid)?;
        self.recs[idx].body_status = format!(
            "failed:{}",
            ev.str("errorText").unwrap_or("unknown")
        );
        Some(idx)
    }

    /// 响应体落库（§7.3.4）
    pub fn set_body(&mut self, idx: usize, bytes: &[u8], is_binary: bool) {
        if idx >= self.recs.len() {
            return;
        }
        let rec = &mut self.recs[idx];
        if is_binary {
            rec.body_status = "skipped_binary".to_string();
            rec.body_sha256 = Some(sha256_hex(bytes));
            rec.body_preview = None;
            return;
        }
        let truncated = bytes.len() > MAX_BODY;
        let slice = &bytes[..bytes.len().min(MAX_BODY)];
        let text = String::from_utf8_lossy(slice).to_string();
        // Document（HTML 页面）保留完整正文：业务页只有 10-15KB，没必要截
        let keep = if rec.request_type == "Document" { text.len() } else { 64 * 1024 };
        rec.body_preview = Some(truncate_bytes(&text, keep).to_string());
        rec.body_status = "ok".to_string();
        rec.body_truncated = truncated;
        rec.body_sha256 = Some(sha256_hex(bytes));
    }

    /// 正文已完整写文件：记下相对路径，报告里可追溯
    pub fn mark_body_file(&mut self, idx: usize, rel: &str) {
        if idx < self.recs.len() {
            self.recs[idx].body_file = Some(rel.to_string());
        }
    }

    pub fn mark_body_unavailable(&mut self, idx: usize, why: &str) {
        if idx < self.recs.len() {
            self.recs[idx].body_status = format!("unavailable:{why}");
        }
    }

    /// Q2 统计：与"确定/确认"类点击关联的请求，各自带几个单号
    pub fn scan_stats(&self) -> ScanStats {
        let mut s = ScanStats::default();
        let mut clicks: HashMap<u64, usize> = HashMap::new();
        for r in &self.recs {
            let Some(click_seq) = r.caused_by_click_seq else { continue };
            // 只认"下段扫描"的确定按钮。查询/定打打印/确认下段/妥投确认都不算，
            // 否则一次批量查询就能把 H2 判成"批量请求"（自测时踩到过）。
            let is_scan_click = r
                .click_text
                .as_deref()
                .map(|t| {
                    (t.contains("确定") || t.contains("确认"))
                        && !t.contains("打印")
                        && !t.contains("下段")
                        && !t.contains("妥投")
                        && !t.contains("查询")
                })
                .unwrap_or(false);
            if !is_scan_click {
                continue;
            }
            // 真实系统里"确定"按钮到处都是（打印、退回、交班都用它），
            // 光看点击文案会把无关请求算进 Q2。这里按 URL 再排一次：
            // 明显属于其它业务的请求不计入"下段扫描"统计。
            let u = r.url.to_ascii_lowercase();
            if ["print", "phone", "deliver", "return", "handover", "query", "confirm"]
                .iter()
                .any(|k| u.contains(k))
            {
                continue;
            }
            *clicks.entry(click_seq).or_insert(0) += 1;
            if matches!(r.request_type.as_str(), "XHR" | "Fetch" | "Document") {
                s.scan_requests += 1;
                s.numbers_per_request.push(r.numbers_in_body);
            }
        }
        s.ok_clicks = clicks.len();
        s.multi_request_clicks = clicks.values().filter(|c| **c > 1).count();
        s
    }

    /// 所有与"下段扫描"有关、可能作为协议模板的请求
    pub fn scan_candidates(&self) -> Vec<&ReqRecord> {
        self.recs
            .iter()
            .filter(|r| matches!(r.request_type.as_str(), "XHR" | "Fetch" | "Document"))
            .filter(|r| r.method != "GET" || r.url.contains("scan"))
            .collect()
    }

    /// HAR 1.2（§7.3.5）
    pub fn to_har(&self, include_cookies: bool) -> Value {
        let entries: Vec<Value> = self
            .recs
            .iter()
            .filter(|r| !r.url_only || r.status.is_some())
            .map(|r| {
                let req_headers = redact::redact_headers(&r.headers, include_cookies);
                let query: Vec<Value> = url_query_pairs(&r.url)
                    .iter()
                    .map(|(k, v)| json!({"name": k, "value": redact::redact_text(v)}))
                    .collect();
                let post_redacted = r
                    .post_data
                    .as_deref()
                    .map(|b| redact::redact_body(b, include_cookies));
                let body_preview = r
                    .body_preview
                    .as_deref()
                    .map(|b| truncate_bytes(b, 20000).to_string())
                    .map(|b| redact::redact_text(&b));

                let mut entry = json!({
                    "startedDateTime": r.started_utc,
                    "time": r.duration_ms.unwrap_or(0.0),
                    "request": {
                        "method": r.method,
                        "url": redact::redact_text(&r.url),
                        "httpVersion": "HTTP/1.1",
                        "headers": req_headers.iter().map(|(k,v)| json!({"name":k,"value":v})).collect::<Vec<_>>(),
                        "queryString": query,
                        "headersSize": -1,
                        "bodySize": r.post_data.as_ref().map(|b| b.len() as i64).unwrap_or(0),
                    },
                    "response": {
                        "status": r.status.unwrap_or(0),
                        "statusText": r.status_text.clone().unwrap_or_default(),
                        "httpVersion": "HTTP/1.1",
                        "headers": [],
                        "content": {
                            "size": r.encoded_len.unwrap_or(0),
                            "mimeType": r.mime.clone().unwrap_or_default(),
                            "text": body_preview.unwrap_or_default(),
                            "encoding": ""
                        },
                        "redirectURL": "",
                        "headersSize": -1,
                        "bodySize": r.encoded_len.unwrap_or(-1),
                    },
                    "cache": {},
                    "timings": {
                        "send": 0,
                        "wait": r.duration_ms.unwrap_or(0.0),
                        "receive": 0
                    },
                    "_probe": {
                        "requestId": r.request_id,
                        "type": r.request_type,
                        "plaintext": r.plaintext,
                        "causedByClickSeq": r.caused_by_click_seq,
                        "clickText": r.click_text,
                        "numbersInBody": r.numbers_in_body,
                        "bodyStatus": r.body_status,
                        "bodySha256": r.body_sha256,
                        "urlOnly": r.url_only
                    }
                });
                if let Some(p) = post_redacted {
                    entry["request"]["postData"] = json!({"mimeType": "application/json", "text": p});
                }
                entry
            })
            .collect();

        json!({
            "log": {
                "version": "1.2",
                "creator": {"name": "probe", "version": env!("CARGO_PKG_VERSION")},
                "browser": {"name": "Edge (Chromium)", "version": ""},
                "pages": [],
                "entries": entries,
                "_probe": {
                    "generated_at": crate::util::now_iso(),
                    "wall_start": self.wall_start,
                }
            }
        })
    }

    /// timeline 事件体（§8.1 的 request / response）
    pub fn request_event(&self, idx: usize, include_cookies: bool) -> Value {
        let r = &self.recs[idx];
        let headers: Map<String, Value> = redact::redact_headers(&r.headers, include_cookies)
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect();
        json!({
            "request_id": r.request_id,
            "method": r.method,
            "url": redact::redact_text(&r.url),
            "type": r.request_type,
            "plaintext": r.plaintext,
            "post_data": r.post_data.as_deref().map(|b| redact::redact_body(b, include_cookies)),
            "post_data_len": r.post_data.as_ref().map(|b| b.len()),
            "headers": Value::Object(headers),
            "caused_by_click_seq": r.caused_by_click_seq,
            "click_text": r.click_text,
            "numbers_in_body": r.numbers_in_body,
            "url_only": r.url_only
        })
    }

    pub fn response_event(&self, idx: usize) -> Value {
        let r = &self.recs[idx];
        json!({
            "request_id": r.request_id,
            "status": r.status,
            "mime": r.mime,
            "encoded_len": r.encoded_len,
            "duration_ms": r.duration_ms,
            "body_status": r.body_status,
            "body_truncated": r.body_truncated,
            "body_preview": r.body_preview.as_deref().map(|b| {
                let lim = if r.request_type == "Document" { 64 * 1024 } else { 2000 };
                crate::redact::redact_text(&truncate_bytes(b, lim))
            }),
            "body_file": r.body_file,
            "numbers_in_body": r.numbers_in_body
        })
    }
}

impl NetStore {
    /// 从 `timeline.jsonl` 的事件重建记录（`report` 子命令崩溃后补救用）。
    pub fn ingest_timeline(&mut self, v: &Value) {
        match v.get("kind").and_then(|k| k.as_str()) {
            Some("request") => {
                let Some(rid) = v.get("request_id").and_then(|x| x.as_str()) else { return };
                if self.index.contains_key(rid) {
                    return;
                }
                let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let rtype = v.get("type").and_then(|x| x.as_str()).unwrap_or("Other").to_string();
                let url_only = v.get("url_only").and_then(|x| x.as_bool()).unwrap_or(false);
                let post_data = v.get("post_data").and_then(|x| x.as_str()).map(|s| s.to_string());
                let numbers = post_data.as_deref().map(count_numbers_in_body).unwrap_or(0);
                let headers = v
                    .get("headers")
                    .and_then(|h| h.as_object())
                    .map(|m| {
                        m.iter()
                            .map(|(k, val)| (k.clone(), val.as_str().unwrap_or("").to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                self.recs.push(ReqRecord {
                    request_id: rid.to_string(),
                    seq: v.get("seq").and_then(|x| x.as_u64()).unwrap_or(0),
                    ts: v.get("ts").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    ts_mono: v.get("ts_mono").and_then(|x| x.as_u64()).unwrap_or(0),
                    method: v.get("method").and_then(|x| x.as_str()).unwrap_or("GET").to_string(),
                    url: url.clone(),
                    request_type: rtype,
                    post_data,
                    headers,
                    plaintext: v.get("plaintext").and_then(|x| x.as_bool()).unwrap_or(url.starts_with("http://")),
                    status: None,
                    status_text: None,
                    mime: None,
                    encoded_len: None,
                    started_utc: v.get("ts").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    duration_ms: None,
                    body_preview: None,
                    body_status: "pending".to_string(),
                    body_truncated: false,
                    body_file: None,
                    body_sha256: None,
                    caused_by_click_seq: v.get("caused_by_click_seq").and_then(|x| x.as_u64()),
                    click_text: v.get("click_text").and_then(|x| x.as_str()).map(|s| s.to_string()),
                    numbers_in_body: numbers,
                    url_only,
                });
                let idx = self.recs.len() - 1;
                self.index.insert(rid.to_string(), idx);
            }
            Some("response") => {
                let Some(rid) = v.get("request_id").and_then(|x| x.as_str()) else { return };
                let Some(idx) = self.index.get(rid).copied() else { return };
                self.recs[idx].status = v.get("status").and_then(|x| x.as_i64());
                self.recs[idx].mime = v.get("mime").and_then(|x| x.as_str()).map(|s| s.to_string());
                self.recs[idx].encoded_len = v.get("encoded_len").and_then(|x| x.as_i64());
                self.recs[idx].duration_ms = v.get("duration_ms").and_then(|x| x.as_f64());
                self.recs[idx].body_preview = v.get("body_preview").and_then(|x| x.as_str()).map(|s| s.to_string());
                self.recs[idx].body_status = v.get("body_status").and_then(|x| x.as_str()).unwrap_or("pending").to_string();
            }
            _ => {}
        }
    }
}

/// URL 的 query 拆成键值对（HAR 用）
fn url_query_pairs(url: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(q) = url.split_once('?').map(|(_, q)| q) {
        let q = q.split('#').next().unwrap_or("");
        for pair in q.split('&') {
            if pair.is_empty() {
                continue;
            }
            match pair.split_once('=') {
                Some((k, v)) => out.push((k.to_string(), v.to_string())),
                None => out.push((pair.to_string(), String::new())),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_mail_numbers() {
        assert_eq!(count_mail_numbers(r#"{"mailNo":"710000000004161"}"#), 1);
        assert_eq!(count_mail_numbers("710000000004161,710000000004162"), 2);
        assert_eq!(count_mail_numbers("no numbers here"), 0);
        assert_eq!(count_mail_numbers("12345678901234"), 1); // 14 位也算单号
    }

    #[test]
    fn numbers_only_from_mail_like_keys() {
        assert_eq!(count_numbers_in_body(r#"{"mailNo":"710000000004161"}"#), 1);
        assert_eq!(count_numbers_in_body(r#"{"mailNos":["710000000004161","710000000004162"]}"#), 2);
        // 毫秒时间戳不是单号
        assert_eq!(count_numbers_in_body(r#"{"ts":1794897871136}"#), 0);
        assert_eq!(count_numbers_in_body(r#"{"code":0,"msg":"成功"}"#), 0);
    }

    #[test]
    fn heartbeat_detected() {
        assert!(is_heartbeat("http://x/api/heartbeat?t=1"));
        assert!(is_heartbeat("http://x/monitor/report"));
        assert!(!is_heartbeat("http://x/api/scan"));
    }

    #[test]
    fn url_only_types() {
        assert!(is_url_only_type("Image"));
        assert!(!is_url_only_type("XHR"));
    }
}
