//! 自动巡页（PROMPT 之后的体验改进）：登录后由工具自己把那 4 个业务页面打开。
//!
//! **只读链接、只开标签页，绝不点页面里的任何按钮。**
//! 之所以安全，是因为打开这些页面本身没有副作用（已与业务方确认）。
//!
//! 找不到（比如菜单是折叠的、或者根本没有 `<a href>`）就如实说"只找到 N 个"，
//! 剩下的让操作员手点——绝不猜一个 URL 去打开。

use serde_json::Value;

/// SOP 里的 4 个业务页面
pub const PAGES: &[&str] = &["下段扫描", "详情单补打", "退回寄件人", "投递归班"];

#[derive(Debug, Clone)]
pub struct Candidate {
    pub text: String,
    pub url: String,
}

/// 在页面（含同源 iframe）里列出所有 `<a href>`。只读，不点。
pub const SCAN_JS: &str = r#"(function(){
  try {
    var out = [];
    function walk(doc, base) {
      try {
        var as = doc.querySelectorAll('a[href]');
        for (var i = 0; i < as.length; i++) {
          var a = as[i];
          var href = a.getAttribute('href') || '';
          if (!href || href.indexOf('javascript:') === 0 || href === '#') continue;
          var abs = href;
          try { abs = new URL(href, base).href; } catch (e) { continue; }
          var t = (a.getAttribute('title') || a.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 40);
          if (!t) continue;
          out.push({ text: t, url: abs });
        }
        var ifr = doc.querySelectorAll('iframe');
        for (var j = 0; j < ifr.length; j++) {
          try {
            var cd = ifr[j].contentDocument;
            if (cd) walk(cd, cd.location.href);
          } catch (e) {}
        }
      } catch (e) {}
    }
    walk(document, location.href);
    return JSON.stringify(out);
  } catch (e) { return '[]'; }
})()"#;

pub fn parse_candidates(v: &Value) -> Vec<Candidate> {
    let s = v.as_str().unwrap_or("[]");
    let arr: Vec<Value> = serde_json::from_str(s).unwrap_or_default();
    let mut out: Vec<Candidate> = Vec::new();
    for item in arr {
        let text = item.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let url = item.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
        if text.is_empty() || url.is_empty() {
            continue;
        }
        if out.iter().any(|c| c.url == url) {
            continue;
        }
        out.push(Candidate { text, url });
    }
    out
}

/// 为每个业务页面挑一个候选。相似度不够就不挑（宁可手点，也不猜）。
pub fn pick(cands: &[Candidate], origin: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for page in PAGES {
        let mut best: Option<(f64, &Candidate)> = None;
        for c in cands {
            // 只开同源的页面
            if !origin.is_empty() && !c.url.starts_with(origin) {
                continue;
            }
            let score = crate::sopdiff::similarity(&c.text, page);
            if score >= 0.5 && best.map(|(b, _)| score > b).unwrap_or(true) {
                best = Some((score, c));
            }
        }
        if let Some((_, c)) = best {
            if !out.iter().any(|(_, u)| u == &c.url) {
                out.push((page.to_string(), c.url.clone()));
            }
        }
    }
    out
}

/// 从 URL 里取 origin（`http://host:port`）
pub fn origin_of(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let host = rest.split('/').next().unwrap_or("");
            format!("{scheme}://{host}")
        }
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn c(t: &str, u: &str) -> Candidate {
        Candidate { text: t.into(), url: u.into() }
    }

    #[test]
    fn parses_and_dedupes_by_url() {
        let v = json!(r#"[{"text":"下段扫描","url":"http://h/a"},{"text":"下段扫描2","url":"http://h/a"},{"text":"","url":"http://h/b"}]"#);
        let got = parse_candidates(&v);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].url, "http://h/a");
    }

    #[test]
    fn picks_each_page_once() {
        let cands = vec![
            c("下段扫描", "http://h/scan"),
            c("详情单补打", "http://h/print"),
            c("退回寄件人", "http://h/return"),
            c("投递归班交接", "http://h/handover"),
            c("首页", "http://h/"),
        ];
        let got = pick(&cands, "http://h");
        assert_eq!(got.len(), 4, "{got:?}");
        assert!(got.iter().any(|(n, u)| n == "投递归班" && u == "http://h/handover"));
    }

    #[test]
    fn ignores_foreign_origin_and_unknown_menu() {
        let cands = vec![
            c("下段扫描", "http://other/scan"),
            c("帮助文档", "http://h/help"),
        ];
        assert!(pick(&cands, "http://h").is_empty(), "异源/不相关的不该被打开");
    }

    #[test]
    fn origin_extraction() {
        assert_eq!(origin_of("http://10.1.2.3:8080/a/b?x=1"), "http://10.1.2.3:8080");
        assert_eq!(origin_of("https://h/"), "https://h");
    }
}
