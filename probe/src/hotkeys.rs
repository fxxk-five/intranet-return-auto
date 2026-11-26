//! 全局热键切 step（§7.5.4）。
//!
//! 三条硬要求：检查 `RegisterHotKey` 返回值、按整组降级、带 `MOD_NOREPEAT`。

use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::cli::Step;
use crate::session::Session;

const MOD_ALT: u32 = 0x0001;
const MOD_CONTROL: u32 = 0x0002;
const MOD_SHIFT: u32 = 0x0004;
const MOD_NOREPEAT: u32 = 0x4000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Step(Step),
    End,
}

/// 解析 `ctrl+alt+f1=A,ctrl+alt+f9=END`
pub fn parse_spec(spec: &str) -> Result<Vec<(String, Target)>> {
    let mut out = Vec::new();
    for item in spec.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let (combo, target) = match item.split_once('=') {
            Some((c, t)) => (c.trim(), t.trim()),
            None => bail!("热键项缺少 '='：{item:?}"),
        };
        let step = if target.eq_ignore_ascii_case("end") {
            Target::End
        } else {
            match Step::from_letter(target) {
                Some(s) => Target::Step(s),
                None => bail!("热键目标无法识别：{target:?}（应为 A/B/C/D/END）"),
            }
        };
        // 早失败：解析不出键位就报错，不静默回退（§7.5.4）
        let _ = parse_combo(combo).map_err(|e| anyhow::anyhow!("热键 {combo:?} 无法解析：{e}"))?;
        out.push((combo.to_string(), step));
    }
    if out.is_empty() {
        bail!("--hotkeys 解析后为空");
    }
    Ok(out)
}

fn parse_combo(combo: &str) -> Result<(u32, u32)> {
    let mut mods = MOD_NOREPEAT;
    let mut vk: Option<u32> = None;
    for part in combo.split('+') {
        let p = part.trim().to_ascii_lowercase();
        match p.as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "" => {}
            _ => {
                if let Some(rest) = p.strip_prefix('f') {
                    let n: u32 = rest.parse().map_err(|_| anyhow::anyhow!("不是功能键"))?;
                    if !(1..=24).contains(&n) {
                        bail!("F{n} 超出范围");
                    }
                    vk = Some(0x70 + n - 1);
                } else if p.len() == 1 {
                    let c = p.chars().next().unwrap();
                    if c.is_ascii_alphanumeric() {
                        vk = Some(c.to_ascii_uppercase() as u32);
                    } else {
                        bail!("不支持的键 {p:?}");
                    }
                } else {
                    bail!("不支持的键 {p:?}");
                }
            }
        }
    }
    match vk {
        Some(v) => Ok((mods, v)),
        None => bail!("缺少主键（如 F1 / 1 / A）"),
    }
}

/// 三套默认键位（整组降级）
fn default_tiers() -> Vec<Vec<(String, Target)>> {
    let mk = |prefix: &str, end_key: &str| -> Vec<(String, Target)> {
        vec![
            (format!("{prefix}+F1"), Target::Step(Step::A)),
            (format!("{prefix}+F2"), Target::Step(Step::B)),
            (format!("{prefix}+F3"), Target::Step(Step::C)),
            (format!("{prefix}+F4"), Target::Step(Step::D)),
            (end_key.to_string(), Target::End),
        ]
    };
    vec![
        mk("ctrl+alt", "ctrl+alt+F9"),
        mk("ctrl+shift", "ctrl+shift+F9"),
        mk("ctrl+alt", "ctrl+alt+0").into_iter().map(|(c, t)| {
            // 第三套把 F1..F4 换成 1..4
            let c = c
                .replace("+F1", "+1")
                .replace("+F2", "+2")
                .replace("+F3", "+3")
                .replace("+F4", "+4");
            (c, t)
        }).collect(),
    ]
}

/// 注册热键并起消息循环线程。返回实际生效的键位表。
pub fn install(spec: Option<&str>, sess: Arc<Session>) -> Result<Vec<(String, String)>> {
    let table: Vec<(String, Target)> = match spec {
        Some(s) => parse_spec(s)?,
        None => {
            // 默认表先按第一套；注册失败再整组降级
            default_tiers().remove(0)
        }
    };

    let tier_sets: Vec<Vec<(String, Target)>> = if spec.is_some() {
        // 用户显式指定时不降级，注册失败就报错（避免操作员按错键）
        vec![table]
    } else {
        default_tiers()
    };

    // 逐套尝试；一旦某套整组注册成功，立刻起消息循环并返回
    for tier in tier_sets {
        let mut parsed: Vec<(String, u32, u32)> = Vec::new();
        let mut parse_ok = true;
        for (combo, _target) in &tier {
            match parse_combo(combo) {
                Ok((mods, vk)) => parsed.push((combo.clone(), mods, vk)),
                Err(_) => {
                    parse_ok = false;
                    break;
                }
            }
        }
        if !parse_ok {
            continue;
        }

        let mut registered: Vec<i32> = Vec::new();
        let mut all_ok = true;
        for (i, (_combo, mods, vk)) in parsed.iter().enumerate() {
            let id = 0xB000 + i as i32;
            if register(id, *mods, *vk) {
                registered.push(id);
            } else {
                all_ok = false;
                break;
            }
        }
        if !all_ok {
            for id in &registered {
                unregister(*id);
            }
            continue;
        }

        let mut table_desc: Vec<(String, String)> = Vec::new();
        let mut mapping: Vec<(u32, Target)> = Vec::new();
        for (i, (combo, _, _)) in parsed.iter().enumerate() {
            let target = tier[i].1.clone();
            table_desc.push((
                combo.clone(),
                match &target {
                    Target::Step(s) => format!("{}（{}）", s.cn(), s.as_str()),
                    Target::End => "结束记录".to_string(),
                },
            ));
            mapping.push((0xB000u32 + i as u32, target));
        }
        spawn_loop(mapping, sess);
        return Ok(table_desc);
    }

    bail!("三套默认热键全部注册失败（可能被输入法/远程桌面/安全软件占用）。请用 --hotkeys 指定别的组合，或用 --step 固定阶段。")
}

fn register(id: i32, mods: u32, vk: u32) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, HOT_KEY_MODIFIERS};
    unsafe { RegisterHotKey(HWND::default(), id, HOT_KEY_MODIFIERS(mods), vk).is_ok() }
}

fn unregister(id: i32) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey;
    unsafe {
        let _ = UnregisterHotKey(HWND::default(), id);
    }
}

fn spawn_loop(mapping: Vec<(u32, Target)>, sess: Arc<Session>) {
    std::thread::spawn(move || {
        use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};
        let mut msg = MSG::default();
        loop {
            let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            if r.0 <= 0 {
                break;
            }
            if msg.message == WM_HOTKEY {
                let id = msg.wParam.0 as u32;
                if let Some((_, target)) = mapping.iter().find(|(mid, _)| *mid == id) {
                    match target {
                        Target::Step(s) => {
                            sess.set_step(*s);
                            println!("[热键] 阶段 → {}（{}）", s.as_str(), s.cn());
                        }
                        Target::End => {
                            println!("[热键] 结束记录，正在生成产物…");
                            sess.request_stop();
                        }
                    }
                }
            }
        }
    });
}

/// SOP 动作标注键：Ctrl+Alt+1..9 → 标记"下一个操作是 #N"（PROMPT §6.2 模式 A）。
///
/// **为什么不用裸数字键**：裸数字会被 RegisterHotKey 全局吃掉，操作员就没法在
/// 邮件号码框里输数字了。所以必须带修饰键；10..21 用 stdin 输两位数。
pub fn install_annot_keys(
    q: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
) -> Vec<String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, HOT_KEY_MODIFIERS};

    let mut ids: Vec<(i32, u32)> = Vec::new();
    for n in 1..=9u32 {
        let id = 0xC000 + n as i32;
        let mods = HOT_KEY_MODIFIERS(MOD_CONTROL | MOD_ALT | MOD_NOREPEAT);
        if unsafe { RegisterHotKey(HWND::default(), id, mods, 0x30 + n).is_ok() } {
            ids.push((id, n));
        }
    }
    if ids.is_empty() {
        return Vec::new();
    }

    let ids_clone = ids.clone();
    std::thread::spawn(move || {
        use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};
        let mut msg = MSG::default();
        loop {
            let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            if r.0 <= 0 {
                break;
            }
            if msg.message == WM_HOTKEY {
                let id = msg.wParam.0 as i32;
                if let Some((_, n)) = ids_clone.iter().find(|(i, _)| *i == id) {
                    if let Ok(mut g) = q.lock() {
                        g.push_back(format!("annot:{n}"));
                    }
                }
            }
        }
    });

    ids.iter()
        .map(|(_, n)| format!("ctrl+alt+{n} → 标注动作 #{n}"))
        .collect()
}

/// 写 `out/hotkeys.json` 并在 stdout 打印实际生效的表
pub fn report_effective(sess: &Session, table: &[(String, String)]) {
    let v: Value = json!({
        "generated_at": crate::util::now_iso(),
        "hotkeys": table.iter().map(|(k, v)| json!({"combo": k, "target": v})).collect::<Vec<_>>()
    });
    let _ = std::fs::write(sess.path("hotkeys.json"), serde_json::to_vec_pretty(&v).unwrap_or_default());
    println!("实际生效的热键：");
    for (combo, label) in table {
        println!("  {combo} → {label}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_combo() {
        assert_eq!(parse_combo("ctrl+alt+f1").unwrap(), (MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 0x70));
        assert_eq!(parse_combo("ctrl+shift+1").unwrap(), (MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT, 0x31));
    }

    #[test]
    fn rejects_garbage_spec() {
        assert!(parse_spec("ctrl+alt+f1").is_err()); // 缺 =目标
        assert!(parse_spec("ctrl+alt+f1=X").is_err());
        assert!(parse_spec("hyper+f1=A").is_err());
    }

    #[test]
    fn parses_full_spec() {
        let v = parse_spec("ctrl+alt+f1=A,ctrl+alt+f9=END").unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].1, Target::End);
    }
}
