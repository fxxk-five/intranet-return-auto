//! SELECTORS.json —— 按"控件"聚合的定位器表（PROMPT §6A.5 + 收尾轮修复 1/3/6）。
//!
//! 三条硬规则：
//! 1. **任何 selector / key 里都不许出现脱敏值**（`7100***********`）——那种 selector 在真机 100% 失配，
//!    而且会伪装成 unique:true / stability:high，比没有更危险。
//! 2. 聚合来源有三个：click 目标、change 目标、elements.json 的 control_map。
//! 3. 退化标签（on/off/是/否/纯数字）不能当控件名。

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;

use super::actions::Action;

/// 连续两个以上 `*` 视为脱敏值
pub fn looks_masked(s: &str) -> bool {
    let mut run = 0usize;
    for ch in s.chars() {
        if ch == '*' {
            run += 1;
            if run >= 2 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

const STATE_WORDS: &[&str] = &["on", "off", "true", "false", "是", "否", "启用", "禁用", "开", "关"];

/// 退化标签：不能当控件名（修复 6）
pub fn is_degenerate_label(name: &str) -> bool {
    let t = name.trim();
    if t.chars().count() < 2 {
        return true;
    }
    if t.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    if STATE_WORDS.iter().any(|w| t.eq_ignore_ascii_case(w)) {
        return true;
    }
    looks_masked(t)
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SelectorEntry {
    pub key: String,
    pub primary: Option<String>,
    pub backup: Vec<String>,
    pub unique: bool,
    pub occurrences: usize,
    pub first_seq: u64,
    pub stability: String,
    pub why: String,
    pub sources: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element_ref: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maxlength: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub observed_values: Vec<Value>,
}

pub struct Sources<'a> {
    pub acts: &'a [Action],
    /// (element, seq) of `change` events
    pub change_elements: Vec<(Value, u64)>,
    pub control_map: &'a Value,
    /// `input_number` 事件（脱敏值 + hash8 + seq）
    pub observed: Vec<Value>,
    pub elements: &'a Value,
}

/// 元素身份键：id > name > 结构签名
fn identity(el: &Value) -> String {
    if let Some(id) = el.get("id").and_then(|v| v.as_str()) {
        if !id.is_empty() {
            return format!("id:{id}");
        }
    }
    if let Some(n) = el.get("name").and_then(|v| v.as_str()) {
        if !n.is_empty() {
            return format!("name:{n}");
        }
    }
    let tag = el.get("tag").and_then(|v| v.as_str()).unwrap_or("?");
    let cls = el.get("cls").and_then(|v| v.as_str()).unwrap_or("");
    let rect = el
        .get("rect")
        .and_then(|r| r.as_array())
        .map(|a| {
            format!(
                "{},{}",
                a.first().and_then(|v| v.as_i64()).unwrap_or(0),
                a.get(1).and_then(|v| v.as_i64()).unwrap_or(0)
            )
        })
        .unwrap_or_default();
    format!("sig:{tag}|{cls}|{rect}")
}

/// 展示用的控件名（退化标签换成 tag#id）
fn display_name(step: &str, raw: &str, el: &Value) -> String {
    // 推断出来的动作名带 `~` 前缀，判退化标签前要先剥掉
    let name = raw.trim().trim_start_matches('~');
    let tag = el.get("tag").and_then(|v| v.as_str()).unwrap_or("el");
    let id = el.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let cls = el
        .get("cls")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .split_whitespace()
        .next()
        .unwrap_or("");

    // 看起来像"数据值"（逗号串/大量数字/脱敏星号）的标签绝不能进 key
    let data_like = name.contains(',')
        || name.chars().filter(|c| c.is_ascii_digit()).count() >= 6
        || looks_masked(name)
        || name.chars().count() > 24;

    if is_degenerate_label(name) || data_like {
        if !id.is_empty() {
            return format!("{step}.{tag}#{id}");
        }
        if !cls.is_empty() {
            return format!("{step}.{tag}.{cls}");
        }
        return format!("{step}.{tag}");
    }
    format!("{step}.{name}")
}

/// 从 elements.json 里查同一个控件的 maxlength（Q2 的第一手证据）
fn maxlength_of(elements: &Value, id: &str, name: &str) -> Option<String> {
    let frames = elements.get("frames")?.as_array()?;
    for f in frames {
        let els = f.get("elements")?.as_array()?;
        for e in els {
            let attrs = e.get("attrs").cloned().unwrap_or(Value::Null);
            let eid = attrs.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let ename = attrs.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let hit = (!id.is_empty() && eid == id) || (!name.is_empty() && ename == name);
            if hit {
                if let Some(ml) = attrs.get("maxlength") {
                    return match ml {
                        Value::String(s) if !s.is_empty() => Some(s.clone()),
                        Value::Null => None,
                        other => Some(other.to_string()),
                    };
                }
            }
        }
    }
    None
}

fn input_kind_of(el: &Value) -> Option<String> {
    let tag = el.get("tag").and_then(|v| v.as_str()).unwrap_or("");
    if tag == "select" {
        return Some("select".into());
    }
    if tag != "input" && tag != "textarea" {
        return None;
    }
    let t = el
        .get("type")
        .and_then(|v| v.as_str())
        .or_else(|| el.get("attrs").and_then(|a| a.get("type")).and_then(|v| v.as_str()))
        .unwrap_or("text");
    Some(match t {
        "checkbox" => "checkbox".into(),
        "number" => "number".into(),
        "tel" => "tel".into(),
        "password" => "text".into(),
        other => other.to_string(),
    })
}

pub fn build(src: &Sources<'_>) -> Vec<SelectorEntry> {
    let mut map: HashMap<String, SelectorEntry> = HashMap::new();
    // identity → 元素（用于 element_ref / maxlength）
    let mut el_by_ident: HashMap<String, Value> = HashMap::new();

    // ---- 来源 1：click/enter 的动作 ----
    for a in src.acts {
        if a.kind == "change" {
            continue; // 由来源 2 处理
        }
        let el = json!({
            "tag": a.selector.primary.clone().unwrap_or_default().split('#').next().unwrap_or("").split('.').next().unwrap_or(""),
            "id": a.selector.primary.as_deref().and_then(|p| p.strip_prefix('#')).unwrap_or(""),
            "cls": "",
            "rect": [0, 0],
        });
        let ident = identity(&el);
        upsert(
            &mut map,
            &mut el_by_ident,
            ident,
            Some(el),
            a,
            "click",
            a.name.clone(),
            src.elements,
        );
    }

    // ---- 来源 2：change 事件的目标元素 ----
    for (el, seq) in &src.change_elements {
        let ident = identity(el);
        let label = el
            .get("placeholder")
            .and_then(|v| v.as_str())
            .or_else(|| el.get("name").and_then(|v| v.as_str()))
            .or_else(|| el.get("id").and_then(|v| v.as_str()))
            .unwrap_or("input")
            .to_string();
        let step = "-";
        let key = display_name(step, &label, el);
        let entry = map.entry(ident.clone()).or_insert_with(|| SelectorEntry {
            key,
            primary: None,
            backup: Vec::new(),
            unique: true,
            occurrences: 0,
            first_seq: *seq,
            stability: "medium".into(),
            why: String::new(),
            sources: Vec::new(),
            element_ref: None,
            input_kind: input_kind_of(el),
            maxlength: None,
            observed_values: Vec::new(),
        });
        entry.occurrences += 1;
        entry.first_seq = entry.first_seq.min(*seq);
        if !entry.sources.iter().any(|s| s == "change") {
            entry.sources.push("change".into());
        }
        if entry.primary.is_none() {
            entry.primary = primary_from_element(el);
            entry.unique = entry.primary.as_deref().map(|p| p.starts_with('#')).unwrap_or(false);
            entry.stability = if entry.unique { "high".into() } else { "low".into() };
            entry.why = if entry.unique {
                "id 定位".into()
            } else {
                "无唯一 id，靠 class/结构定位".into()
            };
        }
        if entry.maxlength.is_none() {
            entry.maxlength = maxlength_of(
                src.elements,
                el.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                el.get("name").and_then(|v| v.as_str()).unwrap_or(""),
            );
        }
        el_by_ident.insert(ident, el.clone());
    }

    // ---- 来源 3：control_map 里 unique:true 的项 ----
    if let Some(cm) = src.control_map.as_object() {
        for (name, v) in cm {
            if v.get("unique").and_then(|u| u.as_bool()) != Some(true) {
                continue;
            }
            let Some(sel) = v.get("selector").and_then(|s| s.as_str()) else { continue };
            if looks_masked(sel) || looks_masked(name) {
                continue;
            }
            let el = json!({"tag": v.get("tag").and_then(|t| t.as_str()).unwrap_or("el"),
                            "id": sel.strip_prefix('#').unwrap_or(""),
                            "cls": "", "rect": [0, 0]});
            let ident = identity(&el);
            let step = name.split('_').next().unwrap_or("-");
            let entry = map.entry(ident.clone()).or_insert_with(|| SelectorEntry {
                key: format!("{step}.{}", name.split('_').last().unwrap_or(name)),
                primary: Some(sel.to_string()),
                backup: Vec::new(),
                unique: true,
                occurrences: 0,
                first_seq: 0,
                stability: "high".into(),
                why: "control_map 唯一命中".into(),
                sources: Vec::new(),
                element_ref: None,
                input_kind: None,
                maxlength: None,
                observed_values: Vec::new(),
            });
            if !entry.sources.iter().any(|s| s == "control_map") {
                entry.sources.push("control_map".into());
            }
            if entry.maxlength.is_none() {
                entry.maxlength = v
                    .get("attr_maxlength")
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string());
            }
            el_by_ident.insert(ident, el);
        }
    }

    // ---- 观测到的输入值（来自 input_number 事件）----
    for ev in &src.observed {
        let masked = ev.get("masked").and_then(|m| m.as_str()).unwrap_or("");
        if masked.is_empty() {
            continue;
        }
        // 归到所有 input 类条目上（同一字段的 hash8 已在上层核对过）
        for (ident, el) in &el_by_ident {
            if input_kind_of(el).is_none() {
                continue;
            }
            if let Some(e) = map.get_mut(ident) {
                if e.observed_values.len() < 20 {
                    e.observed_values.push(json!({
                        "masked": masked,
                        "hash8": ev.get("hash8"),
                        "seq": ev.get("ref_seq")
                    }));
                }
            }
        }
    }

    // ---- element_ref：指向 PAGES 里第 N 个同类标签，永不因脱敏失效 ----
    for (ident, el) in &el_by_ident {
        if let Some(e) = map.get_mut(ident) {
            let tag = el.get("tag").and_then(|v| v.as_str()).unwrap_or("el");
            let id = el.get("id").and_then(|v| v.as_str()).unwrap_or("");
            e.element_ref = Some(json!({
                "tag": tag,
                "id": if id.is_empty() { Value::Null } else { Value::String(id.to_string()) },
                "seq_in_page": Value::Null,
                "note": "在 PAGES/*.html 里按 tag 顺序数；id 非空时优先用 id"
            }));
        }
    }

    // ---- 收尾：清掉所有可能残留的脱敏值 ----
    let mut out: Vec<SelectorEntry> = map
        .into_values()
        .map(|mut e| {
            if let Some(p) = &e.primary {
                if looks_masked(p) {
                    e.primary = None;
                    e.unique = false;
                    e.stability = "low".into();
                    e.why = "原候选含脱敏值，已丢弃".into();
                }
            }
            e.backup.retain(|b| !looks_masked(b));
            e
        })
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

fn primary_from_element(el: &Value) -> Option<String> {
    let id = el.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if !id.is_empty() {
        return Some(format!("#{id}"));
    }
    let name = el.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if !name.is_empty() {
        return Some(format!("[name='{name}']"));
    }
    let tag = el.get("tag").and_then(|v| v.as_str()).unwrap_or("");
    let cls = el
        .get("cls")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .split_whitespace()
        .next()
        .unwrap_or("");
    if !tag.is_empty() && !cls.is_empty() {
        return Some(format!("{tag}.{cls}"));
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn upsert(
    map: &mut HashMap<String, SelectorEntry>,
    el_by_ident: &mut HashMap<String, Value>,
    ident: String,
    el: Option<Value>,
    a: &Action,
    source: &str,
    raw_name: String,
    elements: &Value,
) {
    let step = a.step.clone().unwrap_or_else(|| "-".to_string());
    let elv = el.unwrap_or_else(|| json!({"tag": "el", "id": "", "cls": "", "rect": [0, 0]}));
    let key = display_name(&step, &raw_name, &elv);

    if let Some(e) = map.get_mut(&ident) {
        e.occurrences += 1;
        e.first_seq = e.first_seq.min(a.seq);
        if !e.sources.iter().any(|s| s == source) {
            e.sources.push(source.to_string());
        }
        e.unique = e.unique && a.selector.unique;
        for b in &a.selector.backup {
            if !looks_masked(b) && !e.backup.contains(b) {
                e.backup.push(b.clone());
            }
        }
        e.backup.truncate(5);
        if e.primary.is_none() && !looks_masked(a.selector.primary.as_deref().unwrap_or("")) {
            e.primary = a.selector.primary.clone();
        }
        return;
    }

    let masked_primary = a
        .selector
        .primary
        .as_deref()
        .map(looks_masked)
        .unwrap_or(false);
    let (stability, why) = if masked_primary {
        ("low", "候选含脱敏值，已丢弃；只能靠结构路径")
    } else if a
        .selector
        .primary
        .as_deref()
        .map(|p| p.starts_with('#'))
        .unwrap_or(false)
    {
        ("high", "id 定位，最稳定")
    } else if a.selector.unique {
        ("medium", "class/结构定位，可用但不如 id 稳")
    } else {
        ("low", "没有唯一 id，靠 class/结构定位，页面改版易断")
    };

    let mut backup: Vec<String> = a
        .selector
        .backup
        .iter()
        .filter(|b| !looks_masked(b))
        .cloned()
        .collect();
    backup.truncate(5);

    let entry = SelectorEntry {
        key,
        primary: if masked_primary {
            None
        } else {
            a.selector.primary.clone()
        },
        backup,
        unique: a.selector.unique && !masked_primary,
        occurrences: 1,
        first_seq: a.seq,
        stability: stability.into(),
        why: why.into(),
        sources: vec![source.to_string()],
        element_ref: None,
        input_kind: input_kind_of(&elv),
        maxlength: {
            let id = elv.get("id").and_then(|v| v.as_str()).unwrap_or("");
            maxlength_of(elements, id, "")
        },
        observed_values: Vec::new(),
    };
    map.insert(ident.clone(), entry);
    el_by_ident.insert(ident, elv);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentpack::actions::{SelectorInfo, UiDelta};

    fn act(step: &str, name: &str, primary: &str, unique: bool, seq: u64) -> Action {
        Action {
            no: 1,
            name: name.to_string(),
            step: Some(step.to_string()),
            seq,
            kind: "click".to_string(),
            selector: SelectorInfo {
                primary: Some(primary.to_string()),
                backup: vec![],
                unique,
                note: None,
            },
            ui_delta: UiDelta::default(),
            next_waits_for: json!({"type":"none"}),
            ..Default::default()
        }
    }

    #[test]
    fn looks_masked_rules() {
        assert!(looks_masked("7100***********"));
        assert!(!looks_masked("710000000004161"));
        assert!(!looks_masked("a*b"));
    }

    #[test]
    fn masked_text_never_becomes_selector() {
        let acts = vec![act("A", "7100***********", "input:nth-child(2)", false, 5)];
        let cm = json!({});
        let els = json!({});
        let s = build(&Sources {
            acts: &acts,
            change_elements: vec![],
            control_map: &cm,
            observed: vec![],
            elements: &els,
        });
        let txt = serde_json::to_string(&s).unwrap();
        assert!(!txt.contains("**"), "序列化结果里不能有连续星号：{txt}");
        assert_eq!(s[0].stability, "low");
        assert!(s[0].primary.is_none() || !s[0].primary.as_deref().unwrap().contains('*'));
    }

    #[test]
    fn input_from_change_gets_entry_with_maxlength() {
        let acts: Vec<Action> = vec![];
        let cm = json!({});
        let els = json!({"frames":[{"elements":[{"attrs":{"id":"printNos","name":null,"maxlength":"21"}}]}]});
        let change = json!({"tag":"textarea","id":"printNos","name":null,"cls":"","placeholder":"邮件号码"});
        let s = build(&Sources {
            acts: &acts,
            change_elements: vec![(change, 42)],
            control_map: &cm,
            observed: vec![],
            elements: &els,
        });
        assert_eq!(s.len(), 1);
        assert!(s[0].sources.contains(&"change".to_string()));
        assert_eq!(s[0].input_kind.as_deref(), Some("text"));
        assert_eq!(s[0].maxlength.as_deref(), Some("21"));
    }

    #[test]
    fn control_map_entries_are_merged() {
        let acts = vec![];
        let cm = json!({"A_邮件号码": {"selector": "#mailNo", "unique": true, "tag": "input", "attr_maxlength": null}});
        let els = json!({});
        let s = build(&Sources {
            acts: &acts,
            change_elements: vec![],
            control_map: &cm,
            observed: vec![],
            elements: &els,
        });
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].primary.as_deref(), Some("#mailNo"));
        assert!(s[0].sources.contains(&"control_map".to_string()));
        assert!(s[0].element_ref.is_some());
    }

    #[test]
    fn degenerate_label_is_replaced_but_entry_kept() {
        let acts = vec![act("B", "on", "#checkAllTop", true, 9)];
        let cm = json!({});
        let els = json!({});
        let s = build(&Sources {
            acts: &acts,
            change_elements: vec![],
            control_map: &cm,
            observed: vec![],
            elements: &els,
        });
        assert_eq!(s.len(), 1, "过滤标签不能把条目也丢了");
        assert!(!s[0].key.ends_with(".on"), "key 不该是 B.on：{}", s[0].key);
        assert!(s[0].key.contains('#'), "应改用 tag#id：{}", s[0].key);
    }
}
