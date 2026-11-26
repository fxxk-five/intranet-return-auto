//! 点击 ↔ 请求关联（§7.5.3）。
//!
//! 500ms 滑动窗口：点完 500ms 内冒出来的请求，算这次点击引起的。

#[derive(Debug, Clone)]
pub struct ClickInfo {
    /// 该 click 事件在 timeline 里的 seq
    pub seq: u64,
    pub text: String,
    pub css: Option<String>,
    /// 单调时钟毫秒
    pub mono: u64,
}

#[derive(Default)]
pub struct Correlator {
    last: Option<ClickInfo>,
    window_ms: u64,
}

impl Correlator {
    pub fn new() -> Correlator {
        Correlator {
            last: None,
            window_ms: 500,
        }
    }

    pub fn note_click(&mut self, seq: u64, text: String, css: Option<String>, mono: u64) {
        self.last = Some(ClickInfo {
            seq,
            text,
            css,
            mono,
        });
    }

    /// 请求到达时问一句：这次算谁引起的？
    pub fn attribute(&self, now_mono: u64) -> Option<&ClickInfo> {
        match &self.last {
            Some(c) if now_mono.saturating_sub(c.mono) <= self.window_ms => Some(c),
            _ => None,
        }
    }

    pub fn last(&self) -> Option<&ClickInfo> {
        self.last.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_within_window_only() {
        let mut c = Correlator::new();
        c.note_click(1, "确定".into(), None, 1000);
        assert!(c.attribute(1400).is_some());
        assert!(c.attribute(1500).is_some()); // 边界包含
        assert!(c.attribute(1501).is_none());
    }

    #[test]
    fn no_click_no_attribution() {
        let c = Correlator::new();
        assert!(c.attribute(10).is_none());
    }
}
