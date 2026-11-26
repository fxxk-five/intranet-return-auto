//! 截图（§7.8）：页面截图走 CDP，原生窗口走 PrintWindow。

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::cdp::commands;
use crate::cdp::Cdp;
use crate::session::Session;
use crate::win32;

fn file_name(tag: &str, step: Option<&str>, seq: u64) -> String {
    let ts = time::OffsetDateTime::now_local()
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
        .format(&time::macros::format_description!(
            "[year][month][day][hour][minute][second]"
        ))
        .unwrap_or_else(|_| "0".to_string());
    format!("{tag}-{}-{seq}-{ts}.png", step.unwrap_or("X"))
}

/// 页面截图。返回写入的相对路径（相对 session 目录）。
pub async fn page_shot(cdp: &Cdp, session_id: &str, sess: &Session, tag: &str) -> Result<Option<PathBuf>> {
    let dir = sess.ensure_subdir("screenshots")?;
    let png = commands::capture_screenshot(cdp, session_id, None).await?;
    // 用 image crate 校验一次：坏图不写盘（避免报告里出现打不开的证据）
    image::load_from_memory(&png)?;
    let name = file_name(tag, sess.step().map(|s| s.as_str()), sess.next_seq());
    let path = dir.join(name);
    std::fs::write(&path, &png)?;
    Ok(Some(path))
}

/// 原生窗口截图（§7.8.2）。失败返回 None，由调用方记 `error` 事件，不许 panic。
pub fn native_shot(hwnd: isize, sess: &Session, tag: &str) -> Option<(PathBuf, u32, u32, &'static str)> {
    let dir = sess.ensure_subdir("screenshots").ok()?;
    match win32::capture_window(hwnd) {
        Ok((png, w, h, method)) => {
            let name = file_name(tag, sess.step().map(|s| s.as_str()), sess.next_seq());
            let path = dir.join(name);
            std::fs::write(&path, &png).ok()?;
            Some((path, w, h, method))
        }
        Err(e) => {
            tracing::warn!("原生窗口截图失败 hwnd={hwnd}: {e:#}");
            None
        }
    }
}

/// 相对路径（写进 report / dialogs.json 用 `/` 分隔）
pub fn rel(sess: &Session, p: &Path) -> String {
    p.strip_prefix(&sess.dir)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}
