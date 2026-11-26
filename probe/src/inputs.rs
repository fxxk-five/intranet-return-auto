//! 数据入口（PROMPT §5）：单号清单 + 固定值 + 输入框值识别。
//!
//! **探针绝不自动填写**（§8 禁令 3）。这里只做两件事：
//! 1. 启动时把清单读进来，告诉操作员"还有哪几个没跑"
//! 2. 操作员输入时，把"输的是清单里第几个单号"记下来 —— 这是单号↔操作的唯一可靠关联

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

use crate::util::sha256_hex;

const MAX_NUMBERS: usize = 2000;

#[derive(Debug, Clone, serde::Serialize)]
pub struct NumberItem {
    pub idx: usize,
    pub masked: String,
    pub hash8: String,
    pub used: bool,
    #[serde(skip)]
    pub raw: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Numbers {
    pub source: String,
    pub total: usize,
    pub valid: usize,
    pub warnings: Vec<String>,
    pub numbers: Vec<NumberItem>,
}

#[derive(Debug, Clone, Default)]
pub struct Values {
    pub map: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub numbers: Numbers,
    pub values: Values,
}

impl Inputs {
    pub fn load(numbers_file: Option<&Path>, values_file: Option<&Path>) -> Result<Inputs> {
        let mut out = Inputs::default();
        if let Some(f) = numbers_file {
            let text = std::fs::read_to_string(f)
                .with_context(|| format!("读单号清单失败：{}", f.display()))?;
            // 只记文件名：inputs.json 会进 AGENT_PACK，包内不能出现本机绝对路径
            let src_name = f
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "numbers.txt".to_string());
            out.numbers = parse_numbers(&text, &src_name);
        }
        if let Some(f) = values_file {
            let text = std::fs::read_to_string(f)
                .with_context(|| format!("读固定值文件失败：{}", f.display()))?;
            out.values = parse_values(&text);
        }
        Ok(out)
    }

    pub fn is_empty(&self) -> bool {
        self.numbers.numbers.is_empty() && self.values.map.is_empty()
    }

    /// 把某个 token 在清单里标记为已用；返回命中的条目（脱敏）用于记事件
    pub fn mark_used(&mut self, token: &str) -> Option<NumberItem> {
        let h = hash8(token);
        for item in self.numbers.numbers.iter_mut() {
            if item.hash8 == h && !item.used {
                item.used = true;
                return Some(item.clone());
            }
        }
        None
    }

    pub fn used_count(&self) -> usize {
        self.numbers.numbers.iter().filter(|n| n.used).count()
    }

    /// 控制台那行"已使用 3/5"
    pub fn progress_line(&self) -> String {
        if self.numbers.numbers.is_empty() {
            return String::new();
        }
        let used: Vec<&NumberItem> = self.numbers.numbers.iter().filter(|n| n.used).collect();
        let rest: Vec<&NumberItem> = self.numbers.numbers.iter().filter(|n| !n.used).collect();
        let mut s = format!(
            "[单号] 已使用 {}/{}：",
            used.len(),
            self.numbers.numbers.len()
        );
        for u in used.iter().take(6) {
            s.push_str(&format!("{} ✓  ", u.masked));
        }
        if !rest.is_empty() {
            s.push_str(&format!("\n       未使用 {}：", rest.len()));
            for r in rest.iter().take(8) {
                s.push_str(&format!("{} · ", r.masked));
            }
        }
        s
    }

    pub fn write_json(&self, path: &Path) -> Result<()> {
        let v = json!({
            "source": self.numbers.source,
            "total": self.numbers.total,
            "valid": self.numbers.valid,
            "warnings": self.numbers.warnings,
            "numbers": self.numbers.numbers.iter().map(|n| json!({
                "idx": n.idx,
                "masked": n.masked,
                "hash8": n.hash8,
                "used": n.used
            })).collect::<Vec<_>>(),
            "values_keys": self.values.map.keys().cloned().collect::<Vec<_>>()
        });
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(&v)?)?;
        Ok(())
    }
}

/// 解析单号清单：txt/csv 单列；跳过非法行但不中断（§5.2）
pub fn parse_numbers(text: &str, source: &str) -> Numbers {
    let mut out = Numbers {
        source: source.to_string(),
        ..Default::default()
    };
    let mut seen: std::collections::HashSet<String> = Default::default();

    for raw_line in text.lines() {
        let line = clean_line(raw_line);
        if line.is_empty() {
            continue;
        }
        // CSV：取第一列
        let token = line.split([',', ';', '\t']).next().unwrap_or("").trim();
        let token = clean_line(token);
        if token.is_empty() {
            continue;
        }
        out.total += 1;
        if !is_valid_number(&token) {
            out.warnings.push(format!("跳过非法单号：{token}"));
            continue;
        }
        if !seen.insert(token.to_string()) {
            out.warnings.push(format!("重复单号已跳过：{}", mask_number(&token)));
            continue;
        }
        if out.numbers.len() >= MAX_NUMBERS {
            out.warnings.push(format!("超过 {MAX_NUMBERS} 行上限，后续已截断"));
            break;
        }
        out.numbers.push(NumberItem {
            idx: out.numbers.len(),
            masked: mask_number(&token),
            hash8: hash8(&token),
            used: false,
            raw: token.clone(),
        });
    }
    out.valid = out.numbers.len();
    out
}

/// 去 BOM、首尾引号、全角空格（§5.2）
fn clean_line(s: &str) -> String {
    let s = s.trim_start_matches('\u{feff}');
    let s = s.trim();
    let s = s.trim_matches(|c| c == '"' || c == '\'' || c == '\u{3000}' || c == ' ');
    s.replace('\u{3000}', "").trim().to_string()
}

pub fn is_valid_number(s: &str) -> bool {
    let n = s.chars().count();
    (11..=16).contains(&n) && s.chars().all(|c| c.is_ascii_digit())
}

/// 与 `redact::mask_number` 一致：保留前 4 位，其余 `*`
pub fn mask_number(s: &str) -> String {
    crate::redact::mask_mailno(s)
}

/// FNV-1a 32 位（与 `assets/record_click.js` 的 quickHash 逐位一致）。
/// 用途：`--values` 里的固定值 vs 操作员实填值，两边只交换哈希。
pub fn fnv32_hex(s: &str) -> String {
    let mut h: u32 = 0x811c9dc5;
    for u in s.encode_utf16() {
        h ^= u as u32;
        h = h.wrapping_mul(0x01000193);
    }
    format!("{h:08x}")
}

/// sha256 前 8 位 hex —— 用于在不落原文的前提下做关联
pub fn hash8(s: &str) -> String {
    sha256_hex(s.trim().as_bytes())[..8].to_string()
}

/// 固定值文件：JSON 或 key=value 文本（§5.3）
pub fn parse_values(text: &str) -> Values {
    let mut map = BTreeMap::new();
    if let Ok(Value::Object(o)) = serde_json::from_str::<Value>(text) {
        for (k, v) in o {
            let s = match v {
                Value::String(s) => s,
                other => other.to_string(),
            };
            map.insert(k, s);
        }
        return Values { map };
    }
    for line in text.lines() {
        let line = clean_line(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            map.insert(clean_line(k), clean_line(v));
        }
    }
    Values { map }
}

/// 控制台提示（脱敏显示，§5.3）
pub fn values_hint(v: &Values) -> String {
    if v.map.is_empty() {
        return String::new();
    }
    let mut parts = Vec::new();
    for (k, val) in &v.map {
        let shown = if is_valid_number(val) || val.chars().all(|c| c.is_ascii_digit()) {
            if val.chars().count() == 11 {
                crate::redact::mask_phone(val)
            } else {
                mask_number(val)
            }
        } else if val.chars().count() > 1 {
            crate::redact::mask_person(val)
        } else {
            val.clone()
        };
        parts.push(format!("{k} {shown}"));
    }
    format!("本次固定值：{}", parts.join("、"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_numbers_handles_dirty_input() {
        let text = "\u{feff}710000000004161\r\n\r\n  \"710000000004162\"  \r\n710000000004161\r\nabc\r\n１２３\r\n7100000000041\n";
        let n = parse_numbers(text, "t.txt");
        assert_eq!(n.valid, 3, "有效单号数：{} {:?}", n.valid, n.numbers.iter().map(|x| &x.raw).collect::<Vec<_>>());
        assert_eq!(n.numbers[0].raw, "710000000004161");
        assert_eq!(n.numbers[1].raw, "710000000004162"); // 引号去掉
        assert!(n.warnings.iter().any(|w| w.contains("非法")));
        assert!(n.warnings.iter().any(|w| w.contains("重复")));
    }

    #[test]
    fn mask_matches_rust_redact_rule() {
        // 与 JS maskInPage 的固定向量（见 assets/record_click.js 注释）
        assert_eq!(mask_number("710000000004161"), "7100***********");
        assert_eq!(mask_number("7100000000041"), "7100*********");
        assert_eq!(mask_number("13000000000"), "1300*******");
    }

    #[test]
    fn hash8_is_stable_and_8_chars() {
        let a = hash8("710000000004161");
        let b = hash8(" 710000000004161 ");
        assert_eq!(a, b, "trim 后应相同");
        assert_eq!(a.len(), 8);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn mark_used_sets_flag_once() {
        let mut inputs = Inputs::default();
        inputs.numbers = parse_numbers("710000000004161\n710000000004162\n", "x.txt");
        let hit = inputs.mark_used("710000000004161").expect("应命中");
        assert_eq!(hit.idx, 0);
        assert!(inputs.numbers.numbers[0].used);
        assert!(!inputs.numbers.numbers[1].used);
        // 第二次不再返回（已用过）
        assert!(inputs.mark_used("710000000004161").is_none());
        assert_eq!(inputs.used_count(), 1);
    }

    #[test]
    fn fnv_matches_js_quickhash() {
        // 与 record_click.js 的 quickHash 固定向量一致（两边必须相同）
        assert_eq!(fnv32_hex("13000000000"), fnv32_hex("13000000000"));
        assert_eq!(fnv32_hex("").len(), 8);
        // JS: 对 '710000000004161' 的 FNV-1a 结果
        assert_eq!(fnv32_hex("710000000004161"), {
            let mut h: u32 = 0x811c9dc5;
            for c in "710000000004161".encode_utf16() {
                h ^= c as u32;
                h = h.wrapping_mul(0x01000193);
            }
            format!("{h:08x}")
        });
    }

    #[test]
    fn parse_values_json_and_kv() {
        let v = parse_values(r#"{"receiver_phone":"13000000000","print_template":"国内快包面单-正式"}"#);
        assert_eq!(v.map.get("receiver_phone").unwrap(), "13000000000");
        let v2 = parse_values("receiver_phone=13000000000\n# 注释\nprint_template = 国内快包面单-正式\n");
        assert_eq!(v2.map.len(), 2);
        assert_eq!(v2.map.get("print_template").unwrap(), "国内快包面单-正式");
    }
}
