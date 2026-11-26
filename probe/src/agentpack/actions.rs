//! ACTIONS.json —— 动作 → 定位器 → 请求 → 界面变化 → 等待条件（PROMPT §6A.4）。
//!
//! `next_waits_for` 是给 agent 写 `waitForSelector` 的唯一依据，必须给；
//! 推不出来就进 `unresolved`，**不许猜**。

use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Default)]
pub struct SelectorInfo {
    pub primary: Option<String>,
    pub backup: Vec<String>,
    pub unique: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct AddedNode {
    pub tag: String,
    pub cls: Option<String>,
    pub text: Option<String>,
    pub selector: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct UiDelta {
    pub added: Vec<AddedNode>,
    pub removed: Vec<String>,
    pub navigated: bool,
    pub url_after: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ReqInfo {
    pub request_id: String,
    pub method: String,
    pub path: String,
    pub status: Option<i64>,
    pub req_preview: Option<String>,
    pub resp_preview: Option<String>,
    pub har_entry: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Annotation {
    pub action_no: Option<u32>,
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Evidence {
    pub timeline_seqs: Vec<u64>,
    pub raw_dom_before: Option<String>,
    pub raw_dom_after: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Action {
    /// 跨会话稳定标识（修复 5）：阶段 + 动作名 + 出现次序，与 seq 无关
    pub action_uid: String,
    pub name: String,
    /// hotkey = 人工标注；inferred = 推断（名字前加 `~`）
    pub name_from: String,
    pub no: u32,
    pub step: Option<String>,
    pub seq: u64,
    pub at: String,
    pub kind: String,
    pub selector: SelectorInfo,
    pub pre_screenshot: Option<String>,
    pub post_screenshot: Option<String>,
    pub ui_delta: UiDelta,
    pub requests: Vec<ReqInfo>,
    pub next_waits_for: Value,
    pub annotation: Annotation,
    pub evidence: Evidence,
}

/// 从 timeline 事件构建动作链
pub fn build(
    src: &Path,
    events: &[Value],
    har: &[Value],
) -> Result<(Vec<Action>, Vec<Value>)> {
    // requestId → (request 事件, response 事件)
    let mut reqs: HashMap<String, (Value, Option<Value>)> = HashMap::new();
    let mut film_after: HashMap<u64, (Option<String>, Option<String>)> = HashMap::new();
    let mut annotation_by_seq: HashMap<u64, (u32, String, String)> = HashMap::new();

    for ev in events {
        match ev.get("kind").and_then(|k| k.as_str()).unwrap_or("") {
            "request" => {
                if let Some(rid) = ev.get("request_id").and_then(|r| r.as_str()) {
                    reqs.entry(rid.to_string()).or_insert((ev.clone(), None));
                }
            }
            "response" => {
                if let Some(rid) = ev.get("request_id").and_then(|r| r.as_str()) {
                    if let Some(e) = reqs.get_mut(rid) {
                        e.1 = Some(ev.clone());
                    }
                }
            }
            "filmstrip_after" => {
                // 用 ref_seq（被拍到的那次操作的 seq），不是本条事件自己的 seq
                let seq = ev
                    .get("ref_seq")
                    .and_then(|s| s.as_u64())
                    .unwrap_or(0);
                let after = ev.get("after").and_then(|a| a.as_str()).map(|s| s.to_string());
                let after_html = ev
                    .get("after_html")
                    .and_then(|a| a.as_str())
                    .map(|s| s.to_string());
                film_after.insert(seq, (after, after_html));
            }
            "annotation" => {
                if let Some(seq) = ev.get("ref_seq").and_then(|s| s.as_u64()) {
                    let no = ev.get("action_no").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
                    let name = ev
                        .get("action_name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let step = ev.get("step").and_then(|s| s.as_str()).unwrap_or("").to_string();
                    annotation_by_seq.insert(seq, (no, name, step));
                }
            }
            _ => {}
        }
    }

    let har_index: HashMap<String, usize> = har
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            e.get("_probe")
                .and_then(|p| p.get("requestId"))
                .and_then(|r| r.as_str())
                .map(|r| (r.to_string(), i))
        })
        .collect();

    let mut out: Vec<Action> = Vec::new();
    let mut unresolved: Vec<Value> = Vec::new();
    let mut auto_no = 0u32;
    let mut uid_counter: HashMap<(String, String), usize> = HashMap::new();

    for ev in events {
        let kind = ev.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        if !matches!(kind, "click" | "change" | "enter") {
            continue;
        }
        let seq = ev.get("seq").and_then(|s| s.as_u64()).unwrap_or(0);
        let element = ev.get("element").cloned().unwrap_or(Value::Null);
        let step = ev.get("step").and_then(|s| s.as_str()).map(|s| s.to_string());
        let at = ev.get("ts").and_then(|s| s.as_str()).unwrap_or("").to_string();

        let sel = selector_of(&element);
        let ann = annotation_by_seq.get(&seq).cloned();
        let no = match &ann {
            Some((n, _, _)) => *n,
            None => {
                auto_no += 1;
                auto_no
            }
        };
        let annotated = ann.is_some();
        let name = match &ann {
            Some((_, n, _)) => n.clone(),
            None => {
                let t = element
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let data_like = t.contains(',')
                    || t.chars().filter(|c| c.is_ascii_digit()).count() >= 6
                    || super::selectors::looks_masked(&t);
                let base = if !t.is_empty() && !data_like {
                    t.chars().take(20).collect::<String>()
                } else {
                    let tag = element.get("tag").and_then(|x| x.as_str()).unwrap_or("el");
                    let id = element.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    if id.is_empty() {
                        format!("{tag}")
                    } else {
                        format!("{tag}#{id}")
                    }
                };
                format!("~{base}")
            }
        };
        let name_from = if annotated { "hotkey" } else { "inferred" }.to_string();
        let step_key = step.clone().unwrap_or_else(|| "-".to_string());
        let nth = {
            let e = uid_counter.entry((step_key.clone(), name.clone())).or_insert(0);
            *e += 1;
            *e
        };
        let action_uid = format!("{step_key}:{name}:{nth}");

        // 该点击引起的请求（可能多个，全部列出）
        let mut requests: Vec<ReqInfo> = Vec::new();
        for (rid, (req, resp)) in &reqs {
            if req.get("caused_by_click_seq").and_then(|c| c.as_u64()) != Some(seq) {
                continue;
            }
            requests.push(ReqInfo {
                request_id: rid.clone(),
                method: req.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string(),
                path: path_of(req.get("url").and_then(|u| u.as_str()).unwrap_or("")),
                status: resp.as_ref().and_then(|r| r.get("status")).and_then(|s| s.as_i64()),
                req_preview: req
                    .get("post_data")
                    .and_then(|p| p.as_str())
                    .map(|p| crate::util::truncate_bytes(p, 400).to_string()),
                resp_preview: resp
                    .as_ref()
                    .and_then(|r| r.get("body_preview"))
                    .and_then(|b| b.as_str())
                    .map(|b| crate::util::truncate_bytes(b, 400).to_string()),
                har_entry: har_index.get(rid).copied(),
            });
        }
        requests.sort_by(|a, b| a.path.cmp(&b.path));

        // 快照路径
        let film = ev.get("filmstrip").cloned().unwrap_or(Value::Null);
        let pre_shot = film.get("before").and_then(|b| b.as_str()).map(|s| s.to_string());
        let pre_dom = film.get("before_html").and_then(|b| b.as_str()).map(|s| s.to_string());
        // "前"图挂在 pointerdown 的 seq 上，click 用 ref_seq 指回来
        let action_seq = ev
            .get("ref_seq")
            .and_then(|x| x.as_u64())
            .unwrap_or(seq);
        let (post_shot, post_dom) = film_after
            .get(&action_seq)
            .cloned()
            .unwrap_or((None, None));

        // 界面变化（离线 diff 两份原始 HTML）
        let mut ui_delta = UiDelta::default();
        if let (Some(b), Some(a)) = (&pre_dom, &post_dom) {
            let bh = std::fs::read_to_string(src.join(b)).unwrap_or_default();
            let ah = std::fs::read_to_string(src.join(a)).unwrap_or_default();
            ui_delta = diff_dom(&bh, &ah);
        }

        let mut next = next_waits_for(&ui_delta, &requests);
        let closing = name.contains("关闭")
            || name.contains("取消")
            || name.contains("×")
            || name.contains("x关闭");
        if next.get("type").and_then(|t| t.as_str()) == Some("none") && kind == "click" {
            // 点了但界面没变化、也没请求：至少断言"这个控件还在、还能点"，
            // 比给 agent 一个 type:none 更有用（否则它只能裸 sleep）
            if let Some(sel) = sel.primary.clone() {
                // 关闭类动作点完自己就不可见了，"等它 visible"是自相矛盾的（agent 会卡死）；
                // 这类只断言"节点还在"，不要求可见。
                next = json!({
                    "type": "element",
                    "selector": sel,
                    "state": if closing { "attached" } else { "visible" },
                    "timeout_ms_suggested": 3000
                });
            }
        }
        if next.get("type").and_then(|t| t.as_str()) == Some("none")
            && matches!(kind, "change" | "enter")
        {
            // 输入类操作的"等待"就是值写进去（或表单校验完成），否则等于没有等待条件
            if let Some(sel) = sel.primary.clone() {
                next = json!({
                    "type": "element",
                    "selector": sel,
                    "state": "value_set",
                    "timeout_ms_suggested": 3000
                });
            }
        }
        if next.get("type").and_then(|t| t.as_str()) == Some("none") {
            unresolved.push(json!({
                "seq": seq,
                "action": name,
                "reason": "既没有关联请求，DOM 也没有可识别的变化，无法判断这一步的作用"
            }));
        }

        out.push(Action {
            action_uid,
            name_from,
            no,
            name,
            step,
            seq,
            at,
            kind: kind.to_string(),
            selector: sel,
            pre_screenshot: pre_shot,
            post_screenshot: post_shot,
            ui_delta,
            requests,
            next_waits_for: next,
            annotation: Annotation {
                action_no: ann.as_ref().map(|(n, _, _)| *n),
                mode: Some(if ann.is_some() { "hotkey" } else { "inferred" }.to_string()),
            },
            evidence: Evidence {
                timeline_seqs: vec![seq],
                raw_dom_before: pre_dom,
                raw_dom_after: post_dom,
            },
        });
    }

    Ok((out, unresolved))
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

fn selector_of(element: &Value) -> SelectorInfo {
    let id = element.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let cls = element.get("cls").and_then(|v| v.as_str()).unwrap_or("");
    let css = element.get("css").and_then(|v| v.as_str()).unwrap_or("");
    let tag = element.get("tag").and_then(|v| v.as_str()).unwrap_or("");

    let mut backup = Vec::new();
    if !css.is_empty() {
        backup.push(css.to_string());
    }
    if !cls.is_empty() {
        let first = cls.split_whitespace().next().unwrap_or("");
        if !first.is_empty() {
            backup.push(format!("{tag}.{first}"));
        }
    }
    if let Some(t) = element.get("text").and_then(|v| v.as_str()) {
        let tt = t.trim();
        // 脱敏值（7100****）绝不能进 selector：真机上必然失配
        if !tt.is_empty() && tt.chars().count() >= 2 && !super::selectors::looks_masked(tt) {
            backup.push(format!(
                "//{tag}[normalize-space()='{}']",
                tt.chars().take(20).collect::<String>()
            ));
        }
    }

    if !id.is_empty() {
        SelectorInfo {
            primary: Some(format!("#{id}")),
            backup,
            unique: true,
            note: None,
        }
    } else if !cls.is_empty() {
        let first = cls.split_whitespace().next().unwrap_or("");
        SelectorInfo {
            primary: Some(format!("{tag}.{first}")),
            backup,
            unique: false,
            note: Some("没有 id，靠 class 定位；class 可能带构建哈希，需人工确认".to_string()),
        }
    } else {
        SelectorInfo {
            primary: css.is_empty().then(|| tag.to_string()),
            backup,
            unique: false,
            note: Some("无 id / class / 文本，只能靠结构路径，稳定性低".to_string()),
        }
    }
}

/// 离线 DOM diff（原则一：探针不判断，这里算）
///
/// 关键修正（收尾轮）：按 **tag+id/class 为身份**比对，而不是逐字指纹。
/// 否则 `class="modal-mask hidden"` 这种"显隐切换"会被算成"删一个、加一个"，
/// 于是"点 × 关闭弹窗"看起来像"弹窗又出现了"。
pub fn diff_dom(before: &str, after: &str) -> UiDelta {
    let b = fingerprint(before);
    let a = fingerprint(after);

    let mut bmap: HashMap<String, &Node> = HashMap::new();
    for n in &b {
        bmap.insert(n.key.clone(), n);
    }
    let mut amap: HashMap<String, &Node> = HashMap::new();
    for n in &a {
        amap.insert(n.key.clone(), n);
    }

    let mut added: Vec<AddedNode> = Vec::new();
    let mut removed: Vec<String> = Vec::new();

    for (k, n) in &amap {
        match bmap.get(k) {
            None => added.push(to_added(n)),
            Some(prev) => {
                // 同一节点：显隐切换算"加/删"，属性微调不算
                if prev.hidden && !n.hidden {
                    added.push(to_added(n));
                } else if !prev.hidden && n.hidden {
                    removed.push(n.fp.clone());
                }
            }
        }
    }
    for (k, n) in &bmap {
        if !amap.contains_key(k) {
            removed.push(n.fp.clone());
        }
    }

    removed.sort();
    removed.dedup();

    // 弹窗类排前面（它最可能是"下一步该等的东西"），但不是只留弹窗 ——
    // 面板/区域切换同样是有意义的界面变化，否则 1/4 的动作会没有等待条件。
    added.sort_by_key(|x| {
        let c = x.cls.clone().unwrap_or_default().to_ascii_lowercase();
        let dialogish = ["dialog", "modal", "message", "mask", "tip", "alert"]
            .iter()
            .any(|k| c.contains(k));
        if dialogish {
            0
        } else {
            1
        }
    });
    added.truncate(3);

    UiDelta {
        added,
        removed,
        navigated: false,
        url_after: None,
    }
}

fn to_added(n: &Node) -> AddedNode {
    AddedNode {
        tag: n.tag.clone(),
        cls: n.cls.clone(),
        text: n.text.clone(),
        selector: n.selector.clone(),
    }
}

#[derive(Debug, Clone)]
struct Node {
    /// 逐字指纹（用于报告和 removed 列表）
    fp: String,
    /// 身份键：tag + id（无 id 用首个 class）。显隐切换靠它对齐
    key: String,
    tag: String,
    cls: Option<String>,
    text: Option<String>,
    selector: Option<String>,
    hidden: bool,
}

/// 规范化后建指纹：去注释、去内联 style 值、去 data-v-*、压空白
pub fn normalize(html: &str) -> String {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static RE_COMMENT: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)<!--.*?-->").unwrap());
    static RE_STYLE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?i)\sstyle\s*=\s*"[^"]*""#).unwrap());
    static RE_DATAV: Lazy<Regex> = Lazy::new(|| Regex::new(r#"\sdata-v-[a-z0-9-]+(=("[^"]*"))?"#).unwrap());
    static RE_WS: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s+").unwrap());

    let s = RE_COMMENT.replace_all(html, "");
    let s = RE_STYLE.replace_all(&s, "");
    let s = RE_DATAV.replace_all(&s, "");
    RE_WS.replace_all(&s, " ").to_string()
}

fn fingerprint(html: &str) -> Vec<Node> {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static RE_TAG: Lazy<Regex> = Lazy::new(|| Regex::new(r"<([a-zA-Z][\w-]*)([^>]*)>").unwrap());
    static RE_ID: Lazy<Regex> = Lazy::new(|| Regex::new(r#"id\s*=\s*"([^"]*)""#).unwrap());
    static RE_CLASS: Lazy<Regex> = Lazy::new(|| Regex::new(r#"class\s*=\s*"([^"]*)""#).unwrap());

    let s = normalize(html);
    let mut out = Vec::new();
    for cap in RE_TAG.captures_iter(&s) {
        let tag = cap.get(1).map(|m| m.as_str().to_ascii_lowercase()).unwrap_or_default();
        if matches!(tag.as_str(), "br" | "hr" | "meta" | "link" | "img" | "input" | "path" | "svg") {
            continue;
        }
        let attrs = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let id = RE_ID.captures(attrs).map(|c| c[1].to_string());
        let cls = RE_CLASS.captures(attrs).map(|c| c[1].to_string());
        // 后面的文本（用于区分同结构节点）
        let after = &s[cap.get(0).unwrap().end()..];
        let text: String = after
            .chars()
            .take_while(|c| *c != '<')
            .collect::<String>()
            .trim()
            .chars()
            .take(40)
            .collect();

        let selector = if let Some(i) = &id {
            Some(format!("#{i}"))
        } else if let Some(c) = &cls {
            let first = c.split_whitespace().next().unwrap_or("").to_string();
            if first.is_empty() {
                None
            } else {
                Some(format!("{tag}.{first}"))
            }
        } else {
            None
        };

        let hidden = cls
            .as_deref()
            .map(|c| c.split_whitespace().any(|x| x.eq_ignore_ascii_case("hidden")))
            .unwrap_or(false)
            || attrs.contains("hidden");
        let key = match &id {
            Some(i) if !i.is_empty() => format!("{tag}#{i}"),
            _ => match &cls {
                Some(c) => format!(
                    "{tag}.{}",
                    c.split_whitespace().next().unwrap_or("")
                ),
                None => tag.clone(),
            },
        };
        out.push(Node {
            fp: format!(
                "{tag}|{}|{}|{text}",
                id.clone().unwrap_or_default(),
                cls.clone().unwrap_or_default()
            ),
            key,
            tag,
            cls,
            text: if text.is_empty() { None } else { Some(text) },
            selector,
            hidden,
        });
    }
    out
}

/// `next_waits_for` 推断规则（§6A.4）
pub fn next_waits_for(ui: &UiDelta, requests: &[ReqInfo]) -> Value {
    if let Some(first) = ui.added.first() {
        let sel = first
            .selector
            .clone()
            .or_else(|| first.cls.as_ref().map(|c| c.split_whitespace().next().unwrap_or("").to_string()))
            .unwrap_or_else(|| first.tag.clone());
        return json!({
            "type": "element",
            "selector": sel,
            "timeout_ms_suggested": 5000
        });
    }
    // 只有"消失"时，等它消失也是有效等待（关弹窗、隐藏面板）
    if let Some(removed) = first_removed_selector(ui) {
        return json!({
            "type": "element",
            "selector": removed,
            "state": "hidden",
            "timeout_ms_suggested": 5000
        });
    }
    if let Some(r) = requests.first() {
        return json!({
            "type": "request",
            "method": r.method,
            "path": r.path,
            "timeout_ms_suggested": 8000
        });
    }
    json!({"type": "none"})
}

/// removed 里存的是 `tag|id|class|text` 指纹，这里把它还原成能用的选择器
pub fn first_removed_selector(ui: &UiDelta) -> Option<String> {
    for fp in &ui.removed {
        let mut it = fp.split('|');
        let tag = it.next().unwrap_or("");
        let id = it.next().unwrap_or("");
        let cls = it.next().unwrap_or("");
        if !id.is_empty() {
            return Some(format!("#{id}"));
        }
        let first_cls = cls.split_whitespace().next().unwrap_or("");
        if !first_cls.is_empty() && !tag.is_empty() {
            return Some(format!("{tag}.{first_cls}"));
        }
        if !tag.is_empty() {
            return Some(tag.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_detects_added_dialog_ignoring_random_style() {
        let before = r#"<html><body><div class="app"><span>a</span></div></body></html>"#;
        let after = r#"<html><body><div class="app"><span>a</span></div>
            <div class="el-dialog__wrapper" style="top: 137px; left: 42px">请填写收件人手机号</div></body></html>"#;
        let d = diff_dom(before, after);
        assert_eq!(d.added.len(), 1, "added={:?}", d.added);
        assert_eq!(d.added[0].cls.as_deref(), Some("el-dialog__wrapper"));
        assert_eq!(d.added[0].selector.as_deref(), Some("div.el-dialog__wrapper"));

        // 只有 style 的随机值变化，不算变化
        let a1 = r#"<div class="x" style="top:1px">t</div>"#;
        let a2 = r#"<div class="x" style="top:999px">t</div>"#;
        assert_eq!(diff_dom(a1, a2).added.len(), 0);
        assert_eq!(diff_dom(a1, a2).removed.len(), 0);
    }

    #[test]
    fn close_dialog_is_removed_not_added() {
        let before = r#"<div class="modal-mask"><div class="modal">提示</div></div>"#;
        let after = r#"<div class="modal-mask hidden"><div class="modal">提示</div></div>"#;
        let d = diff_dom(before, after);
        assert!(d.added.is_empty(), "关闭不该有新增：{:?}", d.added);
        assert!(!d.removed.is_empty(), "关闭应记 removed");
    }

    #[test]
    fn next_wait_prefers_element_then_request() {
        let d = UiDelta {
            added: vec![AddedNode {
                tag: "div".into(),
                cls: Some("el-dialog__wrapper".into()),
                text: Some("hi".into()),
                selector: Some("div.el-dialog__wrapper".into()),
            }],
            ..Default::default()
        };
        let w = next_waits_for(&d, &[]);
        assert_eq!(w["type"], "element");
        assert_eq!(w["selector"], "div.el-dialog__wrapper");

        let r = ReqInfo {
            method: "POST".into(),
            path: "/api/scan".into(),
            ..Default::default()
        };
        let w2 = next_waits_for(&UiDelta::default(), &[r]);
        assert_eq!(w2["type"], "request");
        assert_eq!(w2["path"], "/api/scan");

        let w3 = next_waits_for(&UiDelta::default(), &[]);
        assert_eq!(w3["type"], "none");
    }

    #[test]
    fn selector_of_prefers_id() {
        let el = json!({"id":"printBtn","cls":"el-button el-button--primary","css":"body > div > button:nth-child(3)","tag":"button","text":"打印"});
        let s = selector_of(&el);
        assert_eq!(s.primary.as_deref(), Some("#printBtn"));
        assert!(s.unique);
        assert!(s.backup.iter().any(|b| b.contains("printBtn") || b.contains("nth-child")));
    }

    #[test]
    fn wait_falls_back_to_hidden_element() {
        let ui = UiDelta {
            removed: vec!["div|dialogBox|el-dialog__wrapper|提示".to_string()],
            ..Default::default()
        };
        let w = next_waits_for(&ui, &[]);
        assert_eq!(w["type"], "element");
        assert_eq!(w["selector"], "#dialogBox");
        assert_eq!(w["state"], "hidden");
    }

    #[test]
    fn path_strips_host_and_query() {
        assert_eq!(path_of("http://h:8080/api/x?t=1"), "/api/x");
        assert_eq!(path_of("http://h/api/y#z"), "/api/y");
    }
}
