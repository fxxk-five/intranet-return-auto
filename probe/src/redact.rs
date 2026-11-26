//! 脱敏（§10）。
//!
//! 全部是**纯函数**，输入输出都可测（§10 要求写单元测试）。
//! 原则：宁可多剥一层，不可漏明文——报告是要打包传出去的。

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Map, Value};

/// 邮件号：保留前 4 位。
///
/// 方案书写的是"13 位"，但它给的例子 `710000000004161` 是 15 位 —— 现场单号位数
/// 不唯一（邮政既有 13 位也有 15 位）。所以这里按 **11–16 位数字串** 处理：
/// 既是保守的（宁可多剥一层），也能覆盖两类单号。
static RE_MAILNO: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b\d{11,16}\b").unwrap());
/// 11 位手机号（1 开头）：保留前 3 后 2
static RE_PHONE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b1\d{10}\b").unwrap());

pub fn mask_mailno(s: &str) -> String {
    let n = s.chars().count();
    if !(11..=16).contains(&n) {
        return s.to_string();
    }
    let mut out: String = s.chars().take(4).collect();
    for _ in 4..n {
        out.push('*');
    }
    out
}

pub fn mask_phone(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() != 11 {
        return s.to_string();
    }
    let mut out: String = chars[..3].iter().collect();
    for _ in 3..9 {
        out.push('*');
    }
    out.extend(&chars[9..]);
    out
}

/// 姓名/地址：保留首字 + `*`
pub fn mask_person(s: &str) -> String {
    let mut it = s.chars();
    match it.next() {
        None => String::new(),
        Some(c) => {
            let rest = it.count();
            let mut out = String::from(c);
            for _ in 0..rest {
                out.push('*');
            }
            out
        }
    }
}

/// 字段名是否属于凭据类（整值替换）
pub fn is_sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    let k = k.as_str();
    k.contains("pass")
        || k.contains("pwd")
        || k.contains("secret")
        || k.contains("token")
        || k.contains("cookie")
        || k.contains("authorization")
        || k.contains("sign")
}

fn redacted_tag(v: &str) -> String {
    format!("<REDACTED:len={}>", v.chars().count())
}

/// 任意文本里的单号/手机号就地脱敏（用于 body_preview、URL query、报告正文）
pub fn redact_text(s: &str) -> String {
    // 先手机号（11 位 1 开头），再邮件号：顺序反了的话手机号会被当成邮件号，
    // 虽然都脱敏，但保留位数不对，report 里看起来会像数据错误。
    let s = RE_PHONE.replace_all(s, |c: &regex::Captures| mask_phone(&c[0]));
    RE_MAILNO
        .replace_all(&s, |c: &regex::Captures| {
            let v = &c[0];
            if v.chars().count() == 11 && v.starts_with('1') {
                v.to_string()
            } else {
                mask_mailno(v)
            }
        })
        .to_string()
}

/// 递归脱敏一个 JSON 值。键名命中敏感词 → 整值替换；字符串值再做单号/手机号替换。
pub fn redact_json(v: &Value, include_cookies: bool) -> Value {
    match v {
        Value::Object(m) => {
            let mut out = Map::new();
            for (k, val) in m {
                if is_sensitive_key(k) && !(include_cookies && k.eq_ignore_ascii_case("cookie")) {
                    let len = match val {
                        Value::String(s) => s.chars().count(),
                        other => other.to_string().chars().count(),
                    };
                    out.insert(k.clone(), Value::String(format!("<REDACTED:len={len}>")));
                } else {
                    out.insert(k.clone(), redact_json(val, include_cookies));
                }
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| redact_json(x, include_cookies)).collect()),
        Value::String(s) => Value::String(redact_text(s)),
        other => other.clone(),
    }
}

/// header 列表脱敏。cookie/authorization 整值替换。
pub fn redact_headers(headers: &[(String, String)], include_cookies: bool) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(k, v)| {
            let sensitive = is_sensitive_key(k);
            if !sensitive {
                (k.clone(), redact_text(v))
            } else if include_cookies && k.eq_ignore_ascii_case("cookie") {
                (k.clone(), v.clone())
            } else if k.eq_ignore_ascii_case("cookie") {
                (k.clone(), "<REDACTED>".to_string())
            } else {
                (k.clone(), redacted_tag(v))
            }
        })
        .collect()
}

/// 请求体（可能是 JSON、表单、或非文本）
pub fn redact_body(body: &str, include_cookies: bool) -> String {
    match serde_json::from_str::<Value>(body) {
        Ok(v) => redact_json(&v, include_cookies).to_string(),
        Err(_) => redact_text(body),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mailno_keeps_first4() {
        // 方案书例子本身是 15 位，保留前 4 位 + 其余打码
        let m = mask_mailno("710000000004161");
        assert!(m.starts_with("7100"));
        assert_eq!(m.chars().count(), 15);
        assert_eq!(m, "7100***********");
        // 13 位单号同样支持
        assert_eq!(mask_mailno("7100000000041"), "7100*********");
    }

    #[test]
    fn mailno_wrong_length_untouched() {
        assert_eq!(mask_mailno("123"), "123");
    }

    #[test]
    fn phone_keeps_3_and_2() {
        assert_eq!(mask_phone("13000000000"), "130******00");
    }

    #[test]
    fn person_keeps_first() {
        assert_eq!(mask_person("张三"), "张*");
    }

    #[test]
    fn text_masks_both() {
        let s = r#"{"mailNo":"710000000004161","phone":"13000000000"}"#;
        let r = redact_text(s);
        assert!(!r.contains("710000000004161"));
        assert!(!r.contains("13000000000"));
        assert!(r.contains("7100***********"));
        assert!(r.contains("130******00"));
    }

    #[test]
    fn digits_outside_range_left_alone() {
        // 17 位不是单号（比如长流水号），不按邮件号处理
        let s = "71000000000416199";
        assert_eq!(redact_text(s), s);
    }

    #[test]
    fn sensitive_keys_replaced_whole() {
        let v = json!({"userName":"admin","password":"P@ssw0rd","token":"abc","nested":{"sign":"x"}});
        let r = redact_json(&v, false);
        assert_eq!(r["password"], json!("<REDACTED:len=8>"));
        assert_eq!(r["token"], json!("<REDACTED:len=3>"));
        assert_eq!(r["nested"]["sign"], json!("<REDACTED:len=1>"));
        assert_eq!(r["userName"], json!("admin"));
    }

    #[test]
    fn cookies_hidden_by_default() {
        let h = vec![("Cookie".to_string(), "SESSION=abc123; x=y".to_string())];
        let r = redact_headers(&h, false);
        assert_eq!(r[0].1, "<REDACTED>");
        let r2 = redact_headers(&h, true);
        assert_eq!(r2[0].1, "SESSION=abc123; x=y");
    }

    #[test]
    fn authorization_tagged_with_len() {
        let h = vec![("Authorization".to_string(), "Bearer abcdefg".to_string())];
        let r = redact_headers(&h, false);
        assert_eq!(r[0].1, "<REDACTED:len=14>");
    }

    #[test]
    fn body_json_redacted() {
        let b = r#"{"mailNo":"710000000004161","pwd":"123"}"#;
        let r = redact_body(b, false);
        assert!(!r.contains("710000000004161"));
        assert!(!r.contains("\"123\""));
    }

    #[test]
    fn body_non_json_still_text_redacted() {
        let b = "mailNo=710000000004161&phone=13000000000";
        let r = redact_body(b, false);
        assert!(!r.contains("710000000004161"));
        assert!(r.contains("7100*********"));
    }
}
