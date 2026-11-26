//! SOP 一致性校验（PROMPT §8）：把 Word 文档变成"期望序列"，跟实际记录比对。
//!
//! 为什么要这个模块：现有链路是「人照文档做 → 探针记录她做了什么」。
//! 如果她本来就照文档做的，探针永远发现不了**文档**是错的。
//! 只有把文档写成期望序列再比对，才能回答"我们的 SOP 跟系统实际行为一致吗"。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repeat {
    pub min: u32,
    pub max: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopStep {
    pub no: u32,
    pub step: String,
    pub name: String,
    pub repeat: Repeat,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopFlow {
    pub version: u32,
    pub source: String,
    pub steps: Vec<SopStep>,
}

/// §8.2 的初稿（来自 SOP，照抄；允许用户改 plan.json）
pub fn default_flow() -> SopFlow {
    let rows: &[(u32, &str, &str, u32, u32, bool)] = &[
        (1, "A", "输入邮件号", 1, 21, true),
        (2, "A", "点确定", 1, 21, true),
        (3, "A", "点确认下段", 1, 1, true),
        (4, "B", "点详情单补打", 1, 1, true),
        (5, "B", "粘贴单号串", 1, 1, true),
        (6, "B", "点查询", 1, 3, true),
        (7, "B", "勾选全选", 1, 3, true),
        (8, "B", "选打印模板", 0, 3, false),
        (9, "B", "点打印", 1, 50, true),
        (10, "B", "点确定打印", 1, 50, false),
        (11, "B", "关闭弹框", 0, 999, false),
        (12, "B", "填手机号", 0, 999, false),
        (13, "C", "点退回寄件人", 1, 1, true),
        (14, "C", "逐单输入Enter", 1, 50, true),
        (15, "C", "点妥投确认", 1, 1, true),
        (16, "C", "点关闭", 0, 3, false),
        (17, "D", "打开投递归班", 1, 1, true),
        (18, "D", "选投递员", 1, 1, true),
        (19, "D", "点查询", 1, 3, true),
        (20, "D", "点确定", 1, 3, true),
        (21, "D", "点 ->", 1, 1, true),
    ];
    SopFlow {
        version: 1,
        source: "探针设计方案-Rust-v1.md §1.3 / SOP 初稿".to_string(),
        steps: rows
            .iter()
            .map(|(no, step, name, min, max, required)| SopStep {
                no: *no,
                step: step.to_string(),
                name: name.to_string(),
                repeat: Repeat { min: *min, max: *max },
                required: *required,
            })
            .collect(),
    }
}

/// 读 plan.json 的 `sop_flow`；没有就用默认表
pub fn load_flow(plan: Option<&Path>) -> SopFlow {
    let Some(p) = plan else { return default_flow() };
    let Ok(text) = std::fs::read_to_string(p) else { return default_flow() };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return default_flow() };
    match v.get("sop_flow").cloned() {
        Some(sf) => serde_json::from_value(sf).unwrap_or_else(|_| default_flow()),
        None => default_flow(),
    }
}

#[derive(Debug, Clone)]
pub struct Actual {
    pub seq: u64,
    pub step: Option<String>,
    pub no: Option<u32>,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MissingItem {
    pub no: u32,
    pub name: String,
    pub step: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnexpectedItem {
    pub seq: u64,
    pub name: String,
    pub step: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepeatItem {
    pub no: u32,
    pub name: String,
    pub expected: String,
    pub actual: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrderReport {
    pub lcs: usize,
    pub expected_len: usize,
    pub coverage: f64,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Diff {
    pub missing: Vec<MissingItem>,
    pub unexpected: Vec<UnexpectedItem>,
    pub over_repeat: Vec<RepeatItem>,
    pub under_repeat: Vec<RepeatItem>,
    pub order: Option<OrderReport>,
    pub annotated: usize,
    pub total: usize,
}

impl Diff {
    pub fn has_issue(&self) -> bool {
        !self.missing.is_empty()
            || !self.unexpected.is_empty()
            || !self.over_repeat.is_empty()
            || !self.under_repeat.is_empty()
            || self.order.is_some()
    }
    /// 标注率 < 50% → 低置信度（§13.2-7 禁令：不许假装结论可靠）
    pub fn low_confidence(&self) -> bool {
        self.total > 0 && (self.annotated as f64 / self.total as f64) < 0.5
    }
}

/// 归一化：去 `~` 前缀、空白、标点，便于模糊匹配
fn norm(s: &str) -> String {
    s.trim()
        .trim_start_matches('~')
        .chars()
        .filter(|c| !c.is_whitespace() && !"，,。.、；;：:()（）".contains(*c))
        .collect::<String>()
        .to_ascii_lowercase()
}

/// 相似度：包含关系优先，其次字符交集比例
pub fn similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (norm(a), norm(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    if a.contains(&b) || b.contains(&a) {
        return 0.9;
    }
    let sa: std::collections::HashSet<char> = a.chars().collect();
    let sb: std::collections::HashSet<char> = b.chars().collect();
    let inter = sa.intersection(&sb).count() as f64;
    let union = sa.union(&sb).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// 把实际记录匹配到期望步骤
fn match_actual(actual: &Actual, flow: &SopFlow) -> Option<u32> {
    // 1) 有标注：按 (step, no) 精确匹配
    if let Some(no) = actual.no {
        if flow.steps.iter().any(|s| s.no == no) {
            return Some(no);
        }
    }
    // 2) 无标注：按名字模糊匹配（相似度 > 0.6 视为同一动作）
    let mut best: Option<(u32, f64)> = None;
    for s in &flow.steps {
        let score = similarity(&actual.name, &s.name);
        if score > 0.6 && best.map(|(_, b)| score > b).unwrap_or(true) {
            best = Some((s.no, score));
        }
    }
    best.map(|(no, _)| no)
}

pub fn compare(flow: &SopFlow, actual: &[Actual]) -> Diff {
    let mut counts: std::collections::HashMap<u32, u32> = Default::default();
    let mut unexpected: Vec<UnexpectedItem> = Vec::new();
    let mut matched_seq: Vec<u32> = Vec::new();
    let annotated = actual.iter().filter(|a| a.no.is_some()).count();

    for a in actual {
        match match_actual(a, flow) {
            Some(no) => {
                *counts.entry(no).or_insert(0) += 1;
                if matched_seq.last() != Some(&no) {
                    matched_seq.push(no);
                }
            }
            None => unexpected.push(UnexpectedItem {
                seq: a.seq,
                name: a.name.clone(),
                step: a.step.clone(),
            }),
        }
    }

    let mut diff = Diff {
        unexpected,
        annotated,
        total: actual.len(),
        ..Default::default()
    };

    for s in &flow.steps {
        let n = counts.get(&s.no).copied().unwrap_or(0);
        if n == 0 {
            if s.required {
                diff.missing.push(MissingItem {
                    no: s.no,
                    name: s.name.clone(),
                    step: s.step.clone(),
                });
            }
            continue;
        }
        if n > s.repeat.max {
            diff.over_repeat.push(RepeatItem {
                no: s.no,
                name: s.name.clone(),
                expected: format!("{}-{}", s.repeat.min, s.repeat.max),
                actual: n,
            });
        } else if n < s.repeat.min {
            diff.under_repeat.push(RepeatItem {
                no: s.no,
                name: s.name.clone(),
                expected: format!("{}-{}", s.repeat.min, s.repeat.max),
                actual: n,
            });
        }
    }

    // 顺序：期望序列 vs 实际出现序列的 LCS
    let expected: Vec<u32> = flow.steps.iter().map(|s| s.no).collect();
    let lcs = lcs_len(&expected, &matched_seq);
    let coverage = if expected.is_empty() {
        1.0
    } else {
        lcs as f64 / expected.len() as f64
    };
    if coverage < 0.8 {
        diff.order = Some(OrderReport {
            lcs,
            expected_len: expected.len(),
            coverage,
            note: "实际出现顺序与文档差异较大（LCS 覆盖率 < 0.8）".to_string(),
        });
    }

    diff
}

fn lcs_len(a: &[u32], b: &[u32]) -> usize {
    let mut dp = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            dp[i][j] = if a[i - 1] == b[j - 1] {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }
    dp[a.len()][b.len()]
}

/// 从 `timeline.jsonl` 收集实际操作序列
pub fn collect_actual(src: &Path) -> Result<Vec<Actual>> {
    let text = std::fs::read_to_string(src.join("timeline.jsonl"))
        .with_context(|| format!("读 timeline 失败：{}", src.display()))?;
    let mut events: Vec<Value> = Vec::new();
    for line in text.lines() {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            events.push(v);
        }
    }
    // 标注按 ref_seq 索引
    let mut ann: std::collections::HashMap<u64, (u32, String, String)> = Default::default();
    for e in &events {
        if e.get("kind").and_then(|k| k.as_str()) == Some("annotation") {
            let Some(r) = e.get("ref_seq").and_then(|x| x.as_u64()) else { continue };
            let no = e.get("action_no").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
            let name = e.get("action_name").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let step = e.get("step").and_then(|x| x.as_str()).unwrap_or("").to_string();
            ann.insert(r, (no, name, step));
        }
    }

    let mut out = Vec::new();
    for e in &events {
        let kind = e.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        if !matches!(kind, "click" | "change" | "enter") {
            continue;
        }
        let seq = e.get("seq").and_then(|x| x.as_u64()).unwrap_or(0);
        let el = e.get("element").cloned().unwrap_or(Value::Null);
        let text = el.get("text").and_then(|t| t.as_str()).unwrap_or("").trim().to_string();
        let tag = el.get("tag").and_then(|t| t.as_str()).unwrap_or("el");
        let id = el.get("id").and_then(|t| t.as_str()).unwrap_or("");
        let inferred = if !text.is_empty() {
            text
        } else if !id.is_empty() {
            format!("{tag}#{id}")
        } else {
            tag.to_string()
        };
        let step = e.get("step").and_then(|s| s.as_str()).map(|s| s.to_string());

        match ann.get(&seq) {
            Some((no, name, astep)) => out.push(Actual {
                seq,
                step: Some(astep.clone()),
                no: Some(*no),
                name: name.clone(),
            }),
            None => out.push(Actual {
                seq,
                step,
                no: None,
                name: inferred,
            }),
        }
    }
    Ok(out)
}

/// 渲染 `SOP-DIFF.md`（§8.4）
pub fn render(flow: &SopFlow, _actual: &[Actual], diff: &Diff) -> String {
    let mut s = String::new();
    s.push_str("# SOP 一致性校验\n\n");
    s.push_str(&format!(
        "比对对象：`{}`（{} 个动作）\n本次记录：{} 次操作，其中 {} 次无人工标注\n\n",
        flow.source,
        flow.steps.len(),
        diff.total,
        diff.total - diff.annotated
    ));

    if diff.low_confidence() {
        s.push_str("> **置信度：低。** 标注率低于 50%，下面的结论只能当线索，不能当定论。\n");
        s.push_str("> 重跑时请按数字键标注动作（操作员卡第 3 步）。\n\n");
    }

    s.push_str("## 结论\n\n");
    if !diff.has_issue() {
        s.push_str("**文档与实际流程未发现不一致。**\n\n");
    } else {
        let n = diff.missing.len()
            + diff.unexpected.len()
            + diff.over_repeat.len()
            + diff.under_repeat.len()
            + if diff.order.is_some() { 1 } else { 0 };
        s.push_str(&format!("**发现 {n} 处需要人工确认的差异。**\n\n"));
    }

    s.push_str("## 文档有、实际没做（可能导致自动化失败）\n\n");
    if diff.missing.is_empty() {
        s.push_str("无\n\n");
    } else {
        s.push_str("| # | 动作 | 阶段 | 说明 |\n|---|---|---|---|\n");
        for m in &diff.missing {
            s.push_str(&format!(
                "| {} | {} | {} | 未记录到该操作。若真机上确实需要，是本次采集缺失；若不需要，文档应删除 |\n",
                m.no, m.name, m.step
            ));
        }
        s.push('\n');
    }

    s.push_str("## 实际有、文档没写（文档漏项）\n\n");
    if diff.unexpected.is_empty() {
        s.push_str("无\n\n");
    } else {
        s.push_str("| seq | 实际动作 | 阶段 | 建议补充到文档 |\n|---|---|---|---|\n");
        for u in diff.unexpected.iter().take(30) {
            s.push_str(&format!(
                "| {} | {} | {} | 文档未提及该操作 |\n",
                u.seq,
                u.name,
                u.step.clone().unwrap_or_else(|| "-".into())
            ));
        }
        s.push('\n');
    }

    s.push_str("## 次数不符\n\n");
    if diff.over_repeat.is_empty() && diff.under_repeat.is_empty() {
        s.push_str("无\n\n");
    } else {
        s.push_str("| # | 动作 | 文档 | 实际 |\n|---|---|---|---|\n");
        for r in diff.over_repeat.iter().chain(diff.under_repeat.iter()) {
            s.push_str(&format!("| {} | {} | {} | {} |\n", r.no, r.name, r.expected, r.actual));
        }
        s.push('\n');
    }

    s.push_str("## 顺序异常\n\n");
    match &diff.order {
        None => s.push_str("无\n\n"),
        Some(o) => s.push_str(&format!(
            "LCS 覆盖率 {:.2}（阈值 0.80），第 {} / {} 步能按文档顺序对上。{}\n\n",
            o.coverage, o.lcs, o.expected_len, o.note
        )),
    }

    s.push_str("## 无法判定\n\n");
    if diff.low_confidence() {
        s.push_str(&format!(
            "- {}/{} 个动作没有人工标注，以上结论置信度 **低**。\n",
            diff.total - diff.annotated,
            diff.total
        ));
    } else {
        s.push_str("无\n");
    }

    s
}

/// `--out` 可能是 session 目录，也可能是含 sessions/ 的输出根目录
pub fn resolve_session(p: &Path) -> Result<std::path::PathBuf> {
    if p.join("timeline.jsonl").exists() {
        return Ok(p.to_path_buf());
    }
    let sessions = p.join("sessions");
    if sessions.is_dir() {
        let mut dirs: Vec<std::path::PathBuf> = std::fs::read_dir(&sessions)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|x| x.is_dir() && x.join("timeline.jsonl").exists())
            .collect();
        dirs.sort();
        if let Some(last) = dirs.pop() {
            return Ok(last);
        }
    }
    anyhow::bail!("找不到 timeline.jsonl：{}", p.display())
}

/// `probe.exe sopdiff --out <DIR>`：单独跑（改 plan.json 后不必重新采集）
pub fn run(out: &Path, plan: Option<&Path>) -> Result<String> {
    let flow = load_flow(plan);
    let actual = collect_actual(out)?;
    let diff = compare(&flow, &actual);
    let md = render(&flow, &actual, &diff);
    std::fs::write(out.join("SOP-DIFF.md"), &md)?;
    Ok(md)
}

/// 给 guide 用：期望序列（按文档顺序）
pub fn expected_sequence(flow: &SopFlow) -> Vec<(u32, String, String)> {
    flow.steps
        .iter()
        .map(|s| (s.no, s.name.clone(), s.step.clone()))
        .collect()
}

/// 给 report/agentpack 用：结构化差异
pub fn diff_json(diff: &Diff) -> Value {
    json!({
        "low_confidence": diff.low_confidence(),
        "annotated": diff.annotated,
        "total": diff.total,
        "missing": diff.missing,
        "unexpected": diff.unexpected,
        "over_repeat": diff.over_repeat,
        "under_repeat": diff.under_repeat,
        "order": diff.order,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow_n(n: usize) -> SopFlow {
        let mut f = default_flow();
        f.steps = f.steps.into_iter().take(n).collect();
        f
    }

    fn act(seq: u64, step: &str, no: Option<u32>, name: &str) -> Actual {
        Actual {
            seq,
            step: Some(step.into()),
            no,
            name: name.into(),
        }
    }

    #[test]
    fn perfect_match_has_no_diff() {
        let f = flow_n(3);
        let actual = vec![
            act(1, "A", Some(1), "输入邮件号"),
            act(2, "A", Some(2), "点确定"),
            act(3, "A", Some(3), "点确认下段"),
        ];
        let d = compare(&f, &actual);
        assert!(!d.has_issue(), "{d:?}");
    }

    #[test]
    fn missing_required_hits() {
        let f = flow_n(3);
        let actual = vec![act(1, "A", Some(1), "输入邮件号"), act(2, "A", Some(2), "点确定")];
        let d = compare(&f, &actual);
        assert_eq!(d.missing.len(), 1);
        assert_eq!(d.missing[0].no, 3);
    }

    #[test]
    fn unexpected_hits() {
        let f = flow_n(2);
        let actual = vec![
            act(1, "A", Some(1), "输入邮件号"),
            act(2, "A", Some(2), "点确定"),
            act(3, "A", None, "点了一个文档没写的神秘按钮"),
        ];
        let d = compare(&f, &actual);
        assert_eq!(d.unexpected.len(), 1);
    }

    #[test]
    fn over_repeat_hits() {
        let mut f = flow_n(2);
        f.steps[1].repeat.max = 21;
        let mut actual = vec![act(1, "A", Some(1), "输入邮件号")];
        for i in 0..24 {
            actual.push(act(10 + i, "A", Some(2), "点确定"));
        }
        let d = compare(&f, &actual);
        assert_eq!(d.over_repeat.len(), 1);
        assert_eq!(d.over_repeat[0].actual, 24);
    }

    #[test]
    fn order_hits_with_lcs_coverage() {
        let f = flow_n(4);
        let actual = vec![
            act(1, "A", Some(3), "点确认下段"),
            act(2, "A", Some(4), "点详情单补打"),
            act(3, "A", Some(1), "输入邮件号"),
            act(4, "A", Some(2), "点确定"),
        ];
        let d = compare(&f, &actual);
        let o = d.order.expect("应报顺序异常");
        assert!(o.coverage < 0.8, "coverage={}", o.coverage);
    }

    #[test]
    fn low_confidence_when_annotation_rate_low() {
        let f = flow_n(2);
        let mut actual = vec![act(1, "A", Some(1), "输入邮件号")];
        for i in 0..9 {
            actual.push(act(10 + i, "A", None, &format!("点击{i}")));
        }
        let d = compare(&f, &actual);
        assert!(d.low_confidence());
        let md = render(&f, &actual, &d);
        assert!(md.contains("置信度：低"));
    }

    #[test]
    fn similarity_prefers_containment() {
        assert!(similarity("点打印", "点打印按钮") > 0.8);
        assert!(similarity("点确定", "点取消") < 0.6);
    }
}
