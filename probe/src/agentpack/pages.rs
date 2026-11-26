//! PAGES/ —— 精简 HTML（L2 层，PROMPT §6A.9）。
//!
//! 每份 < 200 KB；剥掉脚本/样式/注释/构建产物属性，保留 id/name/class/type/placeholder/
//! value(脱敏)/role/文本 —— agent 靠这些定位元素。

use anyhow::Result;
use std::path::{Path, PathBuf};

use super::actions::Action;

const MAX_PAGE_BYTES: usize = 200 * 1024;

const STEPS: &[(&str, &str)] = &[
    ("A", "下段扫描"),
    ("B", "详情单补打"),
    ("C", "退回寄件人"),
    ("D", "投递归班"),
];

/// 每个阶段在业务页 HTML 里的特征词（用于从 `pages/` 里挑对应的那一页）
const STEP_KEYWORDS: &[(&str, &[&str])] = &[
    ("A", &["投递下段", "下段扫描", "setroadseg"]),
    ("B", &["打印补打", "详情单", "waybillprint"]),
    ("C", &["妥投寄件人", "退回寄件人", "signwaybillreturned"]),
    ("D", &["投递归班", "归班交接", "returnwork"]),
];

fn walk_html(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk_html(&p));
        } else if p.extension().map(|x| x.eq_ignore_ascii_case("html")).unwrap_or(false) {
            out.push(p);
        }
    }
    out
}

/// 从 `pages/`（探针落盘的完整业务页 DOM，含 iframe）里挑出该阶段的页面
pub fn find_doc_by_keyword(src: &Path, step: &str) -> Option<PathBuf> {
    let kws = STEP_KEYWORDS
        .iter()
        .find(|(s, _)| *s == step)
        .map(|(_, k)| *k)?;
    let dir = src.join("pages");
    if !dir.is_dir() {
        return None;
    }
    let mut best: Option<(usize, PathBuf)> = None;
    for p in walk_html(&dir) {
        let Ok(t) = std::fs::read_to_string(&p) else { continue };
        let score = kws.iter().filter(|k| t.contains(*k)).count();
        if score > 0 && best.as_ref().map(|(b, _)| score > *b).unwrap_or(true) {
            best = Some((score, p));
        }
    }
    best.map(|(_, p)| p)
}

pub fn render(
    src: &Path,
    dest: &Path,
    only: &Option<Vec<String>>,
    acts: &[Action],
) -> Result<Vec<String>> {
    let dir = dest.join("PAGES");
    std::fs::create_dir_all(&dir)?;
    let mut written = Vec::new();

    for (step, cn) in STEPS {
        if let Some(list) = only {
            if !list.iter().any(|s| s.eq_ignore_ascii_case(step)) {
                continue;
            }
        }

        // 优先取该阶段"最后一个动作之后"的 DOM（界面已经稳定）
        let candidate = acts
            .iter()
            .filter(|a| a.step.as_deref() == Some(*step))
            .next_back()
            .and_then(|a| a.evidence.raw_dom_after.clone().or_else(|| a.evidence.raw_dom_before.clone()));

        // 退路：snapshots/ 里按阶段前缀找
        let candidate = candidate.or_else(|| {
            let snap_dir = src.join("snapshots");
            let mut files: Vec<PathBuf> = std::fs::read_dir(&snap_dir)
                .ok()?
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().starts_with(step))
                        .unwrap_or(false)
                })
                .collect();
            files.sort();
            files.pop().map(|p| {
                p.strip_prefix(src)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
        });

        let candidate = candidate.or_else(|| {
            find_doc_by_keyword(src, step).map(|p| {
                p.strip_prefix(src)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
        });
        let Some(rel) = candidate else { continue };
        let html = std::fs::read_to_string(src.join(&rel)).unwrap_or_default();
        if html.trim().is_empty() {
            continue;
        }
        let slim = slim_html(&html);
        let mut body = format!(
            "<!-- 精简自 RAW/{rel}（已剥离 script/style/注释/inline style） -->\n"
        );
        body.push_str(&slim);
        if body.len() > MAX_PAGE_BYTES {
            body = crate::filmstrip::truncate_html_at_tag_boundary(&body, MAX_PAGE_BYTES);
        }
        let name = format!("{step}-{cn}.html");
        std::fs::write(dir.join(&name), body.as_bytes())?;
        written.push(name);
    }

    Ok(written)
}

/// 精简规则（§6A.9）
pub fn slim_html(html: &str) -> String {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static RE_SCRIPT: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?is)<script\b[^>]*>.*?</script>").unwrap());
    static RE_STYLE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?is)<style\b[^>]*>.*?</style>").unwrap());
    static RE_LINK: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?is)<link\b[^>]*>").unwrap());
    static RE_COMMENT: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)<!--.*?-->").unwrap());
    static RE_INLINE_STYLE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"(?i)\sstyle\s*=\s*"[^"]*""#).unwrap());
    static RE_DATAV: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\sdata-v-[a-z0-9-]+(=("[^"]*"))?"#).unwrap());
    static RE_MULTI_WS: Lazy<Regex> = Lazy::new(|| Regex::new(r"[ \t]{2,}").unwrap());

    let s = RE_SCRIPT.replace_all(html, "");
    let s = RE_STYLE.replace_all(&s, "");
    let s = RE_LINK.replace_all(&s, "");
    let s = RE_COMMENT.replace_all(&s, "");
    let s = RE_INLINE_STYLE.replace_all(&s, "");
    let s = RE_DATAV.replace_all(&s, "");
    // 保留缩进层级：每个闭合标签后换行
    let s = s.replace("><", ">\n<");
    let s = RE_MULTI_WS.replace_all(&s, " ");
    // 清掉纯空白行
    s.lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slim_removes_scripts_and_styles_keeps_structure() {
        let html = r#"<html><head><style>.x{color:red}</style><link rel="stylesheet" href="a.css"></head>
        <body><!-- c --><div id="mailNo" class="el-input" style="width:100px" data-v-abc123><span>邮件号码</span></div>
        <script>var a=1;</script></body></html>"#;
        let s = slim_html(html);
        assert!(!s.contains("<script"));
        assert!(!s.contains("<style"));
        assert!(!s.contains("<!--"));
        assert!(!s.contains("style=\""));
        assert!(!s.contains("data-v-"));
        // 结构和关键属性保留
        assert!(s.contains("id=\"mailNo\""));
        assert!(s.contains("class=\"el-input\""));
        assert!(s.contains("邮件号码"));
    }

    #[test]
    fn picks_page_by_keyword_from_pages_dir() {
        let dir = std::env::temp_dir().join("probe-pages-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("pages")).unwrap();
        std::fs::write(
            dir.join("pages").join("dom-1.html"),
            "<html><title>打印补打详情单</title><input id='waybillNo'></html>",
        )
        .unwrap();
        std::fs::write(
            dir.join("pages").join("dom-2.html"),
            "<html><title>投递下段主界面</title><input id='segcode'></html>",
        )
        .unwrap();
        let a = find_doc_by_keyword(&dir, "A").expect("A 应命中");
        let b = find_doc_by_keyword(&dir, "B").expect("B 应命中");
        assert!(a.to_string_lossy().contains("dom-2"));
        assert!(b.to_string_lossy().contains("dom-1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn slim_keeps_indent_lines() {
        let s = slim_html("<div><span>a</span><span>b</span></div>");
        assert!(s.lines().count() >= 2, "应保留换行层级：{s:?}");
    }
}
