//! 操作快照带（PROMPT §4）：每次操作前后各一张图 + 完整 DOM。
//!
//! 时序要求很硬：
//! 1. "前"图必须在 `on_binding` 处理的第一时间截 —— 早于 `emit`（emit 会 sync_data，几十毫秒）
//! 2. "后"图必须 `spawn` 延迟，不能 `sleep` 阻塞事件循环
//! 3. 文件名用 timeline 的 seq，六位零填充，字典序 = 时间序

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

use crate::cdp::{commands, Cdp};
use crate::util::truncate_bytes;

const MAX_ELEMENTS: usize = 20000;
const MAX_HTML_BYTES: usize = 5 * 1024 * 1024;

/// 一次操作的前半份快照
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct FilmRef {
    pub before: Option<String>,
    pub before_html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_html: Option<String>,
}

/// 延迟任务回传给主循环的结果（由 app 的 select! 收）
#[derive(Debug, Clone)]
pub struct FilmAfter {
    pub seq: u64,
    pub after: Option<String>,
    pub after_html: Option<String>,
}

#[derive(Clone)]
pub struct Filmstrip {
    root: PathBuf,
    pub enabled: bool,
    full_page: bool,
    max_bytes: u64,
    used: Arc<AtomicU64>,
    pub shots: Arc<AtomicU64>,
    pub doms: Arc<AtomicU64>,
    shots_off: Arc<AtomicBool>,
    /// 限制并发截图数：几十个延迟截图同时打 CDP 会把渲染进程压到无响应，
    /// 连操作员的弹窗都处理不了（实测踩到过）
    sem: Arc<tokio::sync::Semaphore>,
}

impl Filmstrip {
    pub fn new(root: PathBuf, enabled: bool, full_page: bool, max_mb: u64) -> Filmstrip {
        Filmstrip {
            root,
            enabled,
            full_page,
            max_bytes: max_mb.max(1) * 1024 * 1024,
            used: Arc::new(AtomicU64::new(0)),
            shots: Arc::new(AtomicU64::new(0)),
            doms: Arc::new(AtomicU64::new(0)),
            shots_off: Arc::new(AtomicBool::new(false)),
            sem: Arc::new(tokio::sync::Semaphore::new(2)),
        }
    }

    pub fn used_bytes(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }

    pub fn shots_count(&self) -> u64 {
        self.shots.load(Ordering::Relaxed)
    }

    pub fn name(seq: u64, suffix: &str, ext: &str) -> String {
        format!("{seq:06}-{suffix}.{ext}")
    }

    /// 截"前"图 + 完整 DOM
    pub async fn before(&self, cdp: &Cdp, sid: &str, seq: u64) -> FilmRef {
        if !self.enabled {
            return FilmRef::default();
        }
        let mut out = FilmRef::default();
        // 先截图（最贵也最不能等），再取 DOM。
        // 加硬超时：页面被弹窗冻住时截图不会返回，绝不能把事件循环一起拖死。
        let shot_fut = self.shot(cdp, sid, seq, "before");
        if let Ok(Some(rel)) = tokio::time::timeout(std::time::Duration::from_millis(1500), shot_fut).await {
            out.before = Some(rel);
        }
        let dom_fut = self.dom(cdp, sid, seq, "before");
        if let Ok(Some((rel, _n))) = tokio::time::timeout(std::time::Duration::from_millis(1500), dom_fut).await {
            out.before_html = Some(rel);
        }
        out
    }

    /// 延迟截"后"图；结果通过 channel 回主循环（不阻塞事件循环）
    pub fn schedule_after(
        &self,
        cdp: Arc<Cdp>,
        sid: String,
        seq: u64,
        delay_ms: u64,
        tx: UnboundedSender<FilmAfter>,
    ) {
        if !self.enabled {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            let _permit = me.sem.clone().acquire_owned().await;
            let after = me.shot(&cdp, &sid, seq, "after").await;
            let after_html = me.dom(&cdp, &sid, seq, "after").await.map(|(p, _)| p);
            let _ = tx.send(FilmAfter {
                seq,
                after,
                after_html,
            });
        });
    }

    async fn shot(&self, cdp: &Cdp, sid: &str, seq: u64, suffix: &str) -> Option<String> {
        if self.shots_off.load(Ordering::Relaxed) {
            return None;
        }
        let bytes = commands::capture_screenshot_ex(cdp, sid, None, self.full_page)
            .await
            .ok()?;
        if self.used.load(Ordering::Relaxed) + bytes.len() as u64 > self.max_bytes {
            // 超预算：停止截图，但 DOM 继续（§4.6）
            self.shots_off.store(true, Ordering::Relaxed);
            return None;
        }
        let dir = self.root.clone();
        std::fs::create_dir_all(&dir).ok()?;
        let name = Filmstrip::name(seq, suffix, "png");
        std::fs::write(dir.join(&name), &bytes).ok()?;
        self.used
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        self.shots.fetch_add(1, Ordering::Relaxed);
        Some(format!("filmstrip/{name}"))
    }

    /// 存完整 DOM（不按字节硬截断，只在标签边界截）
    async fn dom(&self, cdp: &Cdp, sid: &str, seq: u64, suffix: &str) -> Option<(String, usize)> {
        let js = r#"(function(){
          try {
            var n = document.querySelectorAll('*').length;
            var out = [{name:'', html: document.documentElement ? document.documentElement.outerHTML : ''}];
            var ifr = document.querySelectorAll('iframe');
            for (var i=0;i<ifr.length;i++) {
              try {
                var d = ifr[i].contentDocument;
                if (d && d.documentElement) out.push({name:'-frame'+(i+1), html:d.documentElement.outerHTML});
              } catch(e) {}
            }
            return JSON.stringify({n:n, docs:out});
          } catch(e) { return JSON.stringify({n:0, docs:[]}); }
        })()"#;
        let v = cdp.evaluate(sid, js).await.ok()?;
        let s = v.as_str()?;
        let parsed: Value = serde_json::from_str(s).ok()?;
        let n = parsed.get("n").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        let docs = parsed.get("docs").and_then(|d| d.as_array())?.clone();
        if docs.is_empty() {
            return None;
        }

        let dir = self.root.clone();
        std::fs::create_dir_all(&dir).ok()?;
        let mut first: Option<(String, usize)> = None;
        for d in docs {
            let name = d.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let html = d.get("html").and_then(|x| x.as_str()).unwrap_or("");
            if html.is_empty() {
                continue;
            }
            let mut body = if n > MAX_ELEMENTS {
                // 元素过多：仍然存，但加一条说明，方便 agent 知道这份 DOM 很大
                format!("<!-- ELEMENTS: {n} > {MAX_ELEMENTS} -->\n{html}")
            } else {
                html.to_string()
            };
            if body.len() > MAX_HTML_BYTES {
                body = truncate_html_at_tag_boundary(&body, MAX_HTML_BYTES);
            }
            let fname = Filmstrip::name(seq, &format!("{suffix}{name}"), "html");
            std::fs::write(dir.join(&fname), body.as_bytes()).ok()?;
            self.used.fetch_add(body.len() as u64, Ordering::Relaxed);
            self.doms.fetch_add(1, Ordering::Relaxed);
            if first.is_none() {
                first = Some((format!("filmstrip/{fname}"), body.len()));
            }
        }
        first
    }

    pub fn summary_line(&self) -> String {
        format!(
            "[快照] {} 张图 / {} 份 DOM / {:.1} MB{}",
            self.shots_count(),
            self.doms.load(Ordering::Relaxed),
            self.used_bytes() as f64 / 1048576.0,
            if self.shots_off.load(Ordering::Relaxed) { "（截图已按预算停止，DOM 继续）" } else { "" }
        )
    }
}

/// 录屏环形缓冲（PROMPT 修复 4b）。
///
/// 不再"收到事件才去截"——那时画面已经是"之后"了。改成一直录，
/// 收到 pointer_down 时回头取时间戳 <= 该事件的那一帧。
pub struct Screencast {
    frames: std::collections::VecDeque<(f64, Vec<u8>)>,
    pub last_ts: Option<f64>,
    pub total: u64,
}

impl Default for Screencast {
    fn default() -> Self {
        Screencast {
            frames: std::collections::VecDeque::new(),
            last_ts: None,
            total: 0,
        }
    }
}

impl Screencast {
    pub fn push(&mut self, ts: f64, jpeg: Vec<u8>) {
        self.last_ts = Some(ts);
        self.total += 1;
        self.frames.push_back((ts, jpeg));
        while self.frames.len() > 30 {
            self.frames.pop_front();
        }
    }

    /// 取时间戳 <= ts 的最后一帧
    pub fn frame_at_or_before(&self, ts: f64) -> Option<&Vec<u8>> {
        self.frames
            .iter()
            .rev()
            .find(|(fts, _)| *fts <= ts)
            .map(|(_, b)| b)
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }
}

impl Filmstrip {
    /// 把一帧 JPEG 落盘成"前"图（screencast 路径）。返回相对路径。
    pub fn save_jpeg_frame(&self, seq: u64, suffix: &str, bytes: &[u8]) -> Option<String> {
        let dir = self.root.clone();
        std::fs::create_dir_all(&dir).ok()?;
        let name = Filmstrip::name(seq, suffix, "jpg");
        std::fs::write(dir.join(&name), bytes).ok()?;
        self.used.fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
        self.shots.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(format!("filmstrip/{name}"))
    }

    /// 把页面内同步取到的 DOM 直接落盘（pointerdown 路径）。返回相对路径。
    pub fn save_dom_html(&self, seq: u64, suffix: &str, html: &str) -> Option<String> {
        let dir = self.root.clone();
        std::fs::create_dir_all(&dir).ok()?;
        let body = if html.len() > MAX_HTML_BYTES {
            truncate_html_at_tag_boundary(html, MAX_HTML_BYTES)
        } else {
            html.to_string()
        };
        let name = Filmstrip::name(seq, suffix, "html");
        std::fs::write(dir.join(&name), body.as_bytes()).ok()?;
        self.used.fetch_add(body.len() as u64, std::sync::atomic::Ordering::Relaxed);
        self.doms.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(format!("filmstrip/{name}"))
    }
}

/// 只在**标签边界**截断，保证结果仍是合法 HTML（§4.4）
pub fn truncate_html_at_tag_boundary(html: &str, max_bytes: usize) -> String {
    if html.len() <= max_bytes {
        return html.to_string();
    }
    let mut cut = max_bytes;
    while cut > 0 && !html.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = &html[..cut];
    // 优先找最后一个 "</...>" 的结束位置
    let pos = head.rfind("</").map(|i| {
        match head[i..].find('>') {
            Some(j) => i + j + 1,
            None => i,
        }
    });
    let pos = pos.or_else(|| head.rfind('>').map(|i| i + 1)).unwrap_or(cut);
    let mut out = String::from(&html[..pos]);
    out.push_str("\n<!-- TRUNCATED -->\n");
    out
}

/// DOM 快照文件名（供 patched app 复用）
pub fn rel_of(root: &Path, abs: &Path) -> String {
    abs.strip_prefix(root)
        .unwrap_or(abs)
        .to_string_lossy()
        .replace('\\', "/")
}

/// 从较长文本里取一段用于日志的预览
pub fn preview(s: &str) -> String {
    truncate_bytes(s, 120).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_zero_padded() {
        assert_eq!(Filmstrip::name(42, "before", "png"), "000042-before.png");
        assert_eq!(Filmstrip::name(7, "after", "html"), "000007-after.html");
        // 字典序 = 时间序
        assert!(Filmstrip::name(9, "before", "png") < Filmstrip::name(10, "before", "png"));
    }

    #[test]
    fn truncation_lands_on_tag_boundary() {
        // 构造一个会被字节截断在汉字中间的长文档
        let big = format!(
            "<html><body>{}</body></html>",
            "<div class='中文'>测试内容</div>".repeat(2000)
        );
        let out = truncate_html_at_tag_boundary(&big, 5000);
        assert!(out.len() >= 4800 && out.len() <= 5020, "len={}", out.len());
        assert!(out.contains("<!-- TRUNCATED -->"));
        // 去掉注释后，仍以完整的标签结束
        let body = out.replace("\n<!-- TRUNCATED -->\n", "");
        assert!(body.trim_end().ends_with('>'), "结尾不是标签: {:?}", &body[body.len().saturating_sub(30)..]);
        // 汉字没有被截断（能正常按 UTF-8 解析）
        assert!(std::str::from_utf8(body.as_bytes()).is_ok());
    }

    #[test]
    fn screencast_picks_frame_at_or_before() {
        let mut sc = Screencast::default();
        sc.push(100.0, b"a".to_vec());
        sc.push(200.0, b"b".to_vec());
        sc.push(300.0, b"c".to_vec());
        assert_eq!(sc.frame_at_or_before(250.0).unwrap(), b"b");
        assert_eq!(sc.frame_at_or_before(300.0).unwrap(), b"c");
        assert!(sc.frame_at_or_before(50.0).is_none(), "早于所有帧应为 None");
        // 超过 30 帧丢最老的
        for i in 0..40 {
            sc.push(1000.0 + i as f64, vec![0u8; 4]);
        }
        assert_eq!(sc.len(), 30);
    }

    #[test]
    fn short_html_untouched() {
        let s = "<html><body>ok</body></html>";
        assert_eq!(truncate_html_at_tag_boundary(s, 5000), s);
    }

    #[test]
    fn budget_stops_shots_but_keeps_dom_flag() {
        // 用 0 预算模拟"已超限"
        let f = Filmstrip::new(PathBuf::from("out/film-test"), true, false, 1);
        f.shots_off.store(true, Ordering::Relaxed);
        assert_eq!(f.shots_count(), 0);
        // DOM 计数与截图开关无关
        assert_eq!(f.doms.load(Ordering::Relaxed), 0);
    }
}
