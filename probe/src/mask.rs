//! `mask` 子命令（§6.6）：对已有产物目录再做一次脱敏。
//!
//! 用于"跑完才发现某个文件漏了脱敏"的补救。**会原地改写文件**，
//! 所以先打印将处理哪些文件、跳过哪些。

use anyhow::Result;


pub fn run(dir: &std::path::Path) -> Result<i32> {
    println!("[mask] 目录：{}", dir.display());
    let mut touched = 0usize;
    let mut skipped = 0usize;

    for entry in walk(dir) {
        let rel = entry
            .strip_prefix(dir)
            .unwrap_or(&entry)
            .to_string_lossy()
            .replace('\\', "/");

        // secrets/ 本来就要保留完整值；二进制不动
        if rel.starts_with("secrets/") || rel.ends_with(".png") || rel.ends_with(".zip") {
            skipped += 1;
            continue;
        }
        if !(rel.ends_with(".json")
            || rel.ends_with(".jsonl")
            || rel.ends_with(".md")
            || rel.ends_with(".html")
            || rel.ends_with(".har"))
        {
            skipped += 1;
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&entry) else {
            skipped += 1;
            continue;
        };
        let masked = if rel.ends_with(".json") {
            match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(v) => serde_json::to_string_pretty(&crate::redact::redact_json(&v, false))?,
                Err(_) => crate::redact::redact_text(&text),
            }
        } else {
            crate::redact::redact_text(&text)
        };
        if masked != text {
            std::fs::write(&entry, masked)?;
            touched += 1;
            println!("  已脱敏 {rel}");
        }
    }
    println!("[mask] 完成：改写 {touched} 个文件，跳过 {skipped} 个。");
    Ok(0)
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}
