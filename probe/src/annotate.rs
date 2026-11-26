//! 操作语义标注（PROMPT §6）：把"阶段 B 有 12 次点击"变成"这 12 次分别是什么动作"。
//!
//! 两种模式：
//! - 模式 A：操作员按数字键/输入数字 → 标记"下一个操作是 SOP 第 N 步"（单次有效，5 秒过期）
//! - 模式 B：事后用 `annotations.csv` 合并（`probe annotate`）
//!
//! 关联规则：一条 `annotation` 之后、下一条之前的所有 click/change/enter 都归它。

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

/// 标注有效期（§6.2）
pub const EXPIRE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SopAction {
    pub no: u32,
    pub step: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// SOP 动作表初稿（§6.3）。**内容来自 SOP，不要改。** 允许用户通过 plan.json 覆盖。
pub fn default_table() -> Vec<SopAction> {
    let rows: &[(u32, &str, &str, Option<&str>)] = &[
        (1, "A", "输入邮件号", Some("在'邮件号码'框里输入")),
        (2, "A", "点确定", Some("每个单号都要点一次")),
        (3, "A", "点确认下段", Some("全部输完后")),
        (4, "B", "点详情单补打", None),
        (5, "B", "粘贴单号串", Some("英文逗号，无空格")),
        (6, "B", "点查询", None),
        (7, "B", "勾选全选", None),
        (8, "B", "选打印模板", None),
        (9, "B", "点打印", None),
        (10, "B", "点确定打印", None),
        (11, "B", "关闭弹框(x)", None),
        (12, "B", "填收件人手机号", None),
        (13, "C", "点退回寄件人", None),
        (14, "C", "逐单输入+Enter", None),
        (15, "C", "点妥投确认", None),
        (16, "C", "点关闭", None),
        (17, "D", "打开投递归班交接", None),
        (18, "D", "选投递员", None),
        (19, "D", "点查询", None),
        (20, "D", "点确定", None),
        (21, "D", "点 ->", None),
    ];
    rows.iter()
        .map(|(no, step, name, hint)| SopAction {
            no: *no,
            step: step.to_string(),
            name: name.to_string(),
            hint: hint.map(|h| h.to_string()),
        })
        .collect()
}

pub struct Annotator {
    pub table: Vec<SopAction>,
    pending: Option<(SopAction, Instant)>,
    pub last_no: Option<u32>,
    /// 本次会话里出现过的标注（给 finish 写 annotations.csv 用）
    pub applied: Vec<(u64, SprungAction)>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SprungAction {
    pub no: u32,
    pub name: String,
    pub step: String,
    pub mode: String,
}

impl Annotator {
    pub fn new(table: Vec<SopAction>) -> Annotator {
        Annotator {
            table,
            pending: None,
            last_no: None,
            applied: Vec::new(),
        }
    }

    /// 从 `plan.json` 读 SOP 表（可选）
    pub fn from_plan(plan: Option<&Path>) -> Result<Annotator> {
        let Some(p) = plan else {
            return Ok(Annotator::new(default_table()));
        };
        let text = std::fs::read_to_string(p)
            .with_context(|| format!("读 plan.json 失败：{}", p.display()))?;
        let v: Value = serde_json::from_str(&text)?;
        match v.get("sop_actions").and_then(|a| a.as_array()) {
            Some(arr) => {
                let table: Vec<SopAction> = serde_json::from_value(Value::Array(arr.clone()))
                    .context("plan.json 的 sop_actions 结构不对")?;
                Ok(Annotator::new(table))
            }
            None => Ok(Annotator::new(default_table())),
        }
    }

    pub fn find(&self, no: u32) -> Option<&SopAction> {
        self.table.iter().find(|a| a.no == no)
    }

    /// 标记"下一个操作是 #no"；返回给控制台回显的文案
    pub fn mark(&mut self, no: u32) -> Result<String> {
        let action = self
            .find(no)
            .ok_or_else(|| anyhow::anyhow!("SOP 动作表里没有 #{no}（可选 1..={}）", self.table.len()))?;
        let text = format!("[标注] 下一个操作 = #{} {}（{}）", action.no, action.name, action.step);
        self.pending = Some((action.clone(), Instant::now()));
        Ok(text)
    }

    /// 有操作发生时调用：若标注仍有效则消费掉，返回 annotation 事件体
    pub fn take_for_operation(&mut self, seq: u64) -> Option<Value> {
        let (action, at) = self.pending.clone()?;
        if at.elapsed() > EXPIRE {
            self.pending = None;
            return None;
        }
        self.pending = None;
        self.last_no = Some(action.no);
        self.applied.push((
            seq,
            SprungAction {
                no: action.no,
                name: action.name.clone(),
                step: action.step.clone(),
                mode: "hotkey".to_string(),
            },
        ));
        Some(json!({
            "action_no": action.no,
            "action_name": action.name,
            "step": action.step,
            "mode": "hotkey"
        }))
    }

    /// 定时检查是否过期（控制台提示一次）
    pub fn expire_if_needed(&mut self) -> Option<String> {
        if let Some((action, at)) = &self.pending {
            if at.elapsed() > EXPIRE {
                let msg = format!("[标注] #{} {} 已过期（5 秒内没有操作）", action.no, action.name);
                self.pending = None;
                return Some(msg);
            }
        }
        None
    }

    /// 控制台打印 SOP 表（stdin 输入 `#` 时）
    pub fn table_text(&self) -> String {
        let mut s = String::new();
        for a in &self.table {
            s.push_str(&format!(
                "  {:>2}  [{}] {}{}\n",
                a.no,
                a.step,
                a.name,
                a.hint.as_ref().map(|h| format!("  —— {h}")).unwrap_or_default()
            ));
        }
        s
    }

    /// 收尾时把标注写成 CSV（人可读、可回填）
    pub fn write_csv(&self, path: &Path) -> Result<()> {
        let mut s = String::from("seq,动作编号,动作名,阶段,来源\n");
        for (seq, a) in &self.applied {
            s.push_str(&format!("{seq},{},{},{},{}\n", a.no, a.name, a.step, a.mode));
        }
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, s)?;
        Ok(())
    }
}

/// 事后标注（模式 B）：把 `annotations.csv` 转成 annotation 事件追加到 timeline
pub fn parse_csv(text: &str, table: &[SopAction]) -> Result<Vec<(u64, u32, String)>> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || i == 0 && line.to_ascii_lowercase().starts_with("seq") {
            continue;
        }
        let cols: Vec<&str> = line.split(',').map(|c| c.trim()).collect();
        if cols.len() < 2 {
            continue;
        }
        let seq: u64 = cols[0].parse().with_context(|| format!("第 {} 行 seq 不是数字", i + 1))?;
        let no: u32 = cols[1]
            .parse()
            .with_context(|| format!("第 {} 行动作编号不是数字", i + 1))?;
        let name = cols
            .get(2)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| table.iter().find(|a| a.no == no).map(|a| a.name.clone()))
            .unwrap_or_else(|| format!("动作#{no}"));
        out.push((seq, no, name));
    }
    Ok(out)
}

/// `probe annotate`：把事后 CSV 合并进 timeline（§6.2 模式 B）
pub fn merge_csv_into_timeline(
    dir: &Path,
    csv: &Path,
    plan: Option<&Path>,
) -> Result<usize> {
    let ann = Annotator::from_plan(plan)?;
    let text = std::fs::read_to_string(csv)
        .with_context(|| format!("读标注文件失败：{}", csv.display()))?;
    let rows = parse_csv(&text, &ann.table)?;
    if rows.is_empty() {
        return Ok(0);
    }

    // 追加到 timeline.jsonl：用当前最大 seq 继续
    let path = dir.join("timeline.jsonl");
    let mut max_seq = 0u64;
    if let Ok(t) = std::fs::read_to_string(&path) {
        for line in t.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                max_seq = max_seq.max(v.get("seq").and_then(|s| s.as_u64()).unwrap_or(0));
            }
        }
    }
    let mut out = String::new();
    for (i, (ref_seq, no, name)) in rows.iter().enumerate() {
        max_seq += 1;
        let step = ann
            .find(*no)
            .map(|a| a.step.clone())
            .unwrap_or_else(|| "-".to_string());
        out.push_str(&json!({
            "seq": max_seq,
            "ts": crate::util::now_iso(),
            "ts_mono": 0,
            "kind": "annotation",
            "step": step,
            "action_no": no,
            "action_name": name,
            "mode": "csv",
            "ref_seq": ref_seq,
            "source_row": i + 2
        }).to_string());
        out.push('\n');
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    f.write_all(out.as_bytes())?;
    f.flush()?;
    Ok(rows.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_21_actions_in_order() {
        let t = default_table();
        assert_eq!(t.len(), 21);
        assert_eq!(t[0].no, 1);
        assert_eq!(t[0].name, "输入邮件号");
        assert_eq!(t[20].no, 21);
        assert_eq!(t[20].step, "D");
        // 阶段顺序不应跳跃（A→B→C→D）
        let steps: Vec<&str> = t.iter().map(|a| a.step.as_str()).collect();
        let mut idx = 0;
        for s in ["A", "B", "C", "D"] {
            let first = steps.iter().position(|x| *x == s).expect(s);
            assert!(first >= idx, "{s} 阶段顺序乱了");
            idx = first;
        }
    }

    #[test]
    fn mark_then_take_once() {
        let mut a = Annotator::new(default_table());
        assert!(a.mark(7).is_ok());
        let ev = a.take_for_operation(42).expect("应消费掉");
        assert_eq!(ev["action_no"], 7);
        assert_eq!(ev["action_name"], "勾选全选");
        // 单次有效：再来一次就没有了
        assert!(a.take_for_operation(43).is_none());
        assert_eq!(a.applied.len(), 1);
    }

    #[test]
    fn mark_expires_after_5s() {
        let mut a = Annotator::new(default_table());
        a.mark(3).unwrap();
        // 手动把时间往后拨：直接构造一个过期的 pending
        let action = a.find(3).cloned().unwrap();
        a.pending = Some((action, Instant::now() - Duration::from_secs(6)));
        assert!(a.take_for_operation(1).is_none(), "过期后不应消费");
        assert!(a.expire_if_needed().is_none(), "已被 take 清掉");
    }

    #[test]
    fn csv_parses_and_fills_names() {
        let csv = "seq,动作编号,备注\n42,5,点详情单补打\n43,6,\n\n";
        let rows = parse_csv(csv, &default_table()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (42, 5, "点详情单补打".to_string()));
        assert_eq!(rows[1], (43, 6, "点查询".to_string()));
    }

    #[test]
    fn unknown_action_rejected() {
        let mut a = Annotator::new(default_table());
        assert!(a.mark(99).is_err());
    }
}
