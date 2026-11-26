//! 小工具：时间格式、hex、字节长度。

use time::OffsetDateTime;

/// ISO8601 本地时间，毫秒精度，带时区偏移。例：`2026-11-17T14:03:22.181+08:00`
pub fn iso8601_local(od: OffsetDateTime) -> String {
    let off = od.offset().whole_seconds();
    let sign = if off < 0 { '-' } else { '+' };
    let a = off.abs();
    let (oh, om) = (a / 3600, (a % 3600) / 60);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}{}{:02}:{:02}",
        od.year(),
        u8::from(od.month()),
        od.day(),
        od.hour(),
        od.minute(),
        od.second(),
        od.millisecond(),
        sign,
        oh,
        om
    )
}

/// 当前本地时间字符串。
pub fn now_iso() -> String {
    iso8601_local(OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc()))
}

/// 当前 UTC 时间字符串（HAR 的 startedDateTime 用）。
pub fn now_iso_utc() -> String {
    iso8601_local(OffsetDateTime::now_utc())
}

pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    to_hex(&h.finalize())
}

/// 截断到 n 字节，并保证不切坏 UTF-8。
pub fn truncate_bytes(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut end = n;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// 进程启动以来的毫秒数（单调时钟，用于排序与事件窗口）。
pub fn mono_ms(start: std::time::Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_utf8_boundary() {
        let s = "邮件号码abc";
        // 截断点落在汉字中间时也不能 panic
        for n in 0..s.len() + 1 {
            let t = truncate_bytes(s, n);
            assert!(t.is_char_boundary(t.len()));
        }
    }

    #[test]
    fn hex_works() {
        assert_eq!(to_hex(&[0x00, 0x0f, 0xff]), "000fff");
    }
}
