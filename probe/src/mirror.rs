//! 全站源码镜像（PROMPT §3）。
//!
//! 把整个站点"搬"到磁盘：HTML / JS / CSS / 图片 / MHTML / 运行时脚本。
//! 定位是**影印机**，不是分析器 —— 这里只抓、不判断（判断留给 agentpack）。
//!
//! 与主产物的脱敏策略**不同**（§3.4）：`mirror/` 下的源码不脱敏，因为脱敏会破坏
//! JS 语法；`INDEX.json` 只含 URL 与路径，做脱敏。凭据风险靠 `00-先读我.txt` 的
//! 警告 + `mirror-scan` 人工复核覆盖。

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::cdp::{commands, Cdp};
use crate::util::sha256_hex;

/// 图片/字体单文件上限（§3.2c）
const MAX_BINARY: usize = 512 * 1024;

#[derive(Debug, Clone, serde::Serialize)]
pub struct MirrorEntry {
    pub url: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub mime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct InlineScript {
    pub path: String,
    pub size: usize,
    pub source: String,
    pub parent_url: Option<String>,
    pub script_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MhtmlEntry {
    pub step: Option<String>,
    pub path: String,
    pub size: usize,
}

pub struct Mirror {
    pub root: PathBuf,
    pub host: String,
    pub entries: Vec<MirrorEntry>,
    pub inline: Vec<InlineScript>,
    pub mhtml: Vec<MhtmlEntry>,
    pub total_bytes: u64,
    pub max_bytes: u64,
    pub truncated: bool,
    /// 仅记录“已成功落盘”或“确定永久跳过（data/blob/超限二进制）”的 URL。
    /// 瞬时失败绝不插入这里，确保后续途径（loadingFinished / Debugger / 同源兜底）可补抓。
    seen_urls: HashSet<String>,
    /// 已成功落盘的相对路径（已去 query），防止不同通道或不同 query 重复写盘，也供看板自检。
    saved_paths: HashSet<String>,
    seen_scripts: HashSet<String>,
    pub fetched: usize,
    pub skipped: usize,
    script_seq: usize,
}

/// 资源类别（决定超预算时的舍弃顺序，§3.5：图片 > 字体 > CSS > MHTML > JS > HTML）
pub fn classify(kind: &str, mime: &str) -> &'static str {
    let k = kind.to_ascii_lowercase();
    let m = mime.to_ascii_lowercase();
    if k == "image" || m.starts_with("image/") {
        "image"
    } else if k == "font" || m.contains("font") {
        "font"
    } else if k == "stylesheet" || m.contains("css") {
        "css"
    } else if k == "script" || m.contains("javascript") || m.contains("ecmascript") {
        "js"
    } else if k == "document" || m.contains("html") {
        "html"
    } else {
        "other"
    }
}

/// 保留优先级：数字越大越不容易被舍弃
pub fn keep_rank(cat: &str) -> u8 {
    match cat {
        "image" => 1,
        "font" => 2,
        "css" => 3,
        "mhtml" => 4,
        "js" => 5,
        "html" => 6,
        _ => 3,
    }
}

/// 预算判定：超限时只放行 JS / HTML（§3.5 的舍弃顺序）
pub fn budget_allows(cat: &str, size: usize, used: u64, max: u64) -> bool {
    if used + size as u64 <= max {
        return true;
    }
    keep_rank(cat) >= 5
}

/// URL → 落盘相对路径。必须防目录穿越、处理 Windows 保留名、丢弃 query。
pub fn url_to_path(url: &str) -> Option<PathBuf> {
    if url.starts_with("data:") || url.starts_with("blob:") || url.starts_with("about:") {
        return None;
    }
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    // host[:port]/path?query
    let (hostport, pathpart) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if hostport.is_empty() {
        return None;
    }
    let host = hostport.replace(':', "_").replace(['<', '>', '"', '|'], "_");
    let mut path_only = pathpart.split(['?', '#']).next().unwrap_or("/").to_string();

    // 逐段清洗：去掉 . / .. / 空段 / 非法字符 / Windows 保留名
    let mut segs: Vec<String> = Vec::new();
    for raw in path_only.split('/') {
        let mut seg = raw.trim().to_string();
        if seg.is_empty() || seg == "." || seg == ".." {
            // ".." 直接丢弃 = 不能穿透；"." 与空段也丢
            continue;
        }
        // 转义非法字符
        for bad in ['<', '>', ':', '"', '|', '?', '*', '\\'] {
            seg = seg.replace(bad, "_");
        }
        seg = seg.replace('\0', "");
        // Windows 保留名（含带扩展名的形式）
        let stem = seg.split('.').next().unwrap_or("").to_ascii_uppercase();
        let reserved = [
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ];
        if reserved.contains(&stem.as_str()) {
            seg = format!("_{seg}");
        }
        segs.push(seg);
    }

    if path_only.ends_with('/') {
        path_only.push_str("index.html");
        segs.push("index.html".to_string());
    }
    if segs.is_empty() {
        segs.push("index.html".to_string());
    }
    let mut p = PathBuf::from(&host);
    for s in segs {
        p.push(s);
    }
    Some(p)
}

impl Mirror {
    pub fn new(root: PathBuf, max_mb: u64) -> Mirror {
        Mirror {
            root,
            host: String::new(),
            entries: Vec::new(),
            inline: Vec::new(),
            mhtml: Vec::new(),
            total_bytes: 0,
            max_bytes: max_mb.max(1) * 1024 * 1024,
            truncated: false,
            seen_urls: HashSet::new(),
            saved_paths: HashSet::new(),
            seen_scripts: HashSet::new(),
            fetched: 0,
            skipped: 0,
            script_seq: 0,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 该 URL（或对应的落盘相对路径）是否已经成功落盘或确定永久跳过
    pub fn is_captured(&self, url: &str) -> bool {
        if self.seen_urls.contains(url) {
            return true;
        }
        if let Some(rel) = url_to_path(url) {
            let key = rel.to_string_lossy().replace('\\', "/");
            if self.saved_paths.contains(&key) {
                return true;
            }
        }
        false
    }

    /// 供看板自检：是否已真实落盘了以指定后缀结尾的资源（忽略大小写）
    pub fn has_saved_suffix(&self, suffix: &str) -> bool {
        let suf = suffix.to_ascii_lowercase();
        self.saved_paths
            .iter()
            .any(|p| p.to_ascii_lowercase().ends_with(&suf))
    }

    /// 从 frameTree 递归收 URL（§3.2b）
    pub async fn walk_resource_tree(&mut self, cdp: &Cdp, session: &str) -> Result<Vec<(String, String)>> {
        let tree = commands::get_resource_tree(cdp, session).await?;
        let mut out = Vec::new();
        collect_frames(&tree, &mut out);
        if self.host.is_empty() {
            self.host = out
                .first()
                .and_then(|(u, _)| u.split("://").nth(1))
                .and_then(|r| r.split('/').next())
                .map(|h| h.replace(':', "_"))
                .unwrap_or_else(|| "site".to_string());
        }
        Ok(out)
    }

    /// 按 frameId + url 取正文并落盘（§3.2c 首选途径）
    /// 注意：失败绝不写入 `seen_urls`，允许后续 loadingFinished / Debugger / 同源兜底重试。
    pub async fn fetch_by_content(
        &mut self,
        cdp: &Cdp,
        session: &str,
        frame_id: &str,
        url: &str,
        kind: &str,
        mime: &str,
    ) -> Result<()> {
        if self.is_captured(url) {
            return Ok(());
        }

        let cat = classify(kind, mime);
        let Some(rel) = url_to_path(url) else {
            self.seen_urls.insert(url.to_string());
            self.push_skipped(url, kind, mime, if url.starts_with("blob:") { "blob" } else { "data_uri" });
            return Ok(());
        };

        match commands::get_resource_content(cdp, session, frame_id, url).await {
            Ok((bytes, _b64)) if !bytes.is_empty() => {
                self.store(url, kind, mime, cat, &rel, &bytes, "page_getResourceContent", None)?;
            }
            Ok(_) => {
                self.push_skipped_transient(url, kind, mime, "getResourceContent_empty");
            }
            Err(e) => {
                // 失败留痕但不占坑，让后续途径可补抓（修复：上次 A 阶段 setroadseg/main.js 丢失根因）
                self.push_skipped_transient(url, kind, mime, &format!("getResourceContent_failed: {e}"));
            }
        }
        Ok(())
    }

    /// 用 netcap 已捕获的 requestId 补正文（§3.2c 后备途径）
    pub async fn fetch_by_request(
        &mut self,
        cdp: &Cdp,
        session: &str,
        request_id: &str,
        url: &str,
        kind: &str,
        mime: &str,
        status: Option<i64>,
    ) -> Result<()> {
        if self.is_captured(url) {
            return Ok(());
        }
        let cat = classify(kind, mime);
        let Some(rel) = url_to_path(url) else {
            self.seen_urls.insert(url.to_string());
            self.push_skipped(url, kind, mime, "data_uri");
            return Ok(());
        };
        match commands::get_response_body(cdp, session, request_id).await {
            Ok((bytes, _b64)) if !bytes.is_empty() => {
                self.store(url, kind, mime, cat, &rel, &bytes, "network_getResponseBody", status)?;
            }
            Ok(_) => {
                self.push_skipped_transient(url, kind, mime, "getResponseBody_empty");
            }
            Err(e) => {
                self.push_skipped_transient(url, kind, mime, &format!("getResponseBody_failed: {e}"));
            }
        }
        Ok(())
    }

    /// 直接把已拿到的原始字节落盘（用于 Network Document 全量正文、页面内同源 fetch 兜底等）
    /// 返回是否实际写入磁盘。`overwrite_existing` 为 true 时允许用更权威的响应（如真实 Document）覆盖旧文件。
    pub fn store_raw(
        &mut self,
        url: &str,
        kind: &str,
        mime: &str,
        bytes: &[u8],
        source: &str,
        status: Option<i64>,
        overwrite_existing: bool,
    ) -> Result<bool> {
        if bytes.is_empty() {
            return Ok(false);
        }
        if !overwrite_existing && self.is_captured(url) {
            return Ok(false);
        }
        let cat = classify(kind, mime);
        let Some(rel) = url_to_path(url) else {
            return Ok(false);
        };
        if overwrite_existing {
            // 移除同名旧记录，避免 INDEX.json 出现重复条
            let rel_s = rel.to_string_lossy().replace('\\', "/");
            let before = self.entries.len();
            self.entries.retain(|e| e.path.as_deref() != Some(rel_s.as_str()));
            if self.entries.len() < before && self.fetched > 0 {
                self.fetched = self.fetched.saturating_sub(before - self.entries.len());
            }
        }
        self.store(url, kind, mime, cat, &rel, bytes, source, status)?;
        Ok(self.is_captured(url))
    }

    fn clear_transient_skipped(&mut self, url: &str) {
        let before = self.entries.len();
        self.entries
            .retain(|e| !(e.url == url && e.skipped.is_some()));
        let removed = before.saturating_sub(self.entries.len());
        self.skipped = self.skipped.saturating_sub(removed);
    }

    fn store(
        &mut self,
        url: &str,
        kind: &str,
        mime: &str,
        cat: &str,
        rel: &Path,
        bytes: &[u8],
        source: &str,
        status: Option<i64>,
    ) -> Result<()> {
        if (cat == "image" || cat == "font") && bytes.len() > MAX_BINARY {
            self.seen_urls.insert(url.to_string());
            self.push_skipped(url, kind, mime, "too_large");
            return Ok(());
        }
        if !budget_allows(cat, bytes.len(), self.total_bytes, self.max_bytes) {
            self.truncated = true;
            self.seen_urls.insert(url.to_string());
            self.push_skipped(url, kind, mime, "budget_exceeded");
            return Ok(());
        }

        let abs = self.root.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&abs, bytes).with_context(|| format!("写资源失败：{}", abs.display()))?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        self.clear_transient_skipped(url);
        self.seen_urls.insert(url.to_string());
        self.saved_paths.insert(rel_str.clone());
        self.total_bytes += bytes.len() as u64;
        self.fetched += 1;
        self.entries.push(MirrorEntry {
            url: url.to_string(),
            kind: kind.to_string(),
            mime: Some(mime.to_string()),
            path: Some(rel_str),
            size: Some(bytes.len()),
            sha256: Some(sha256_hex(bytes)),
            source: source.to_string(),
            status,
            skipped: None,
        });
        Ok(())
    }

    fn push_skipped(&mut self, url: &str, kind: &str, mime: &str, why: &str) {
        self.skipped += 1;
        self.entries.push(MirrorEntry {
            url: url.to_string(),
            kind: kind.to_string(),
            mime: Some(mime.to_string()),
            path: None,
            size: None,
            sha256: None,
            source: "none".to_string(),
            status: None,
            skipped: Some(why.to_string()),
        });
    }

    /// 瞬时失败记录：同一 URL 已有 skipped 项时更新原因而不重复追加，且绝不写入 `seen_urls`
    fn push_skipped_transient(&mut self, url: &str, kind: &str, mime: &str, why: &str) {
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|e| e.url == url && e.skipped.is_some())
        {
            existing.skipped = Some(why.to_string());
            return;
        }
        self.push_skipped(url, kind, mime, why);
    }

    /// 直接把当前文档的 HTML 存成镜像里的页面（保证 `file://` 能打开骨架）
    pub async fn save_document(&mut self, cdp: &Cdp, session: &str, page_url: &str) -> Result<Option<String>> {
        let Ok(v) = cdp
            .evaluate(
                session,
                "document.documentElement ? document.documentElement.outerHTML : ''",
            )
            .await
        else {
            return Ok(None);
        };
        let Some(html) = v.as_str() else { return Ok(None) };
        if html.len() < 32 {
            return Ok(None);
        }
        if self.is_captured(page_url) {
            return Ok(Some(html.to_string()));
        }
        let Some(rel) = url_to_path(page_url) else { return Ok(None) };
        let saved = html.to_string();
        let abs = self.root.join(&rel);
        if let Some(p) = abs.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&abs, html.as_bytes())?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        self.clear_transient_skipped(page_url);
        self.seen_urls.insert(page_url.to_string());
        self.saved_paths.insert(rel_str.clone());
        self.total_bytes += html.len() as u64;
        self.fetched += 1;
        self.entries.push(MirrorEntry {
            url: page_url.to_string(),
            kind: "Document".to_string(),
            mime: Some("text/html".to_string()),
            path: Some(rel_str),
            size: Some(html.len()),
            sha256: Some(sha256_hex(html.as_bytes())),
            source: "dom_outerHTML".to_string(),
            status: Some(200),
            skipped: None,
        });
        Ok(Some(saved))
    }

    /// MHTML 整页存档（§3.2a）——原样落盘，不解码
    pub async fn capture_mhtml(&mut self, cdp: &Cdp, session: &str, step: Option<&str>) -> Result<Option<PathBuf>> {
        let cat = "mhtml";
        if !budget_allows(cat, 0, self.total_bytes, self.max_bytes) {
            self.truncated = true;
            return Ok(None);
        }
        let Ok(mhtml) = commands::capture_snapshot_mhtml(cdp, session).await else {
            return Ok(None);
        };
        if mhtml.is_empty() {
            return Ok(None);
        }
        let dir = self.root.join("mhtml");
        std::fs::create_dir_all(&dir)?;
        let ts = time::OffsetDateTime::now_local()
            .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
            .unix_timestamp();
        let name = format!("{}-{}.mhtml", step.unwrap_or("page"), ts);
        let abs = dir.join(&name);
        std::fs::write(&abs, mhtml.as_bytes())?;
        self.total_bytes += mhtml.len() as u64;
        let rel = format!("mhtml/{name}");
        self.mhtml.push(MhtmlEntry {
            step: step.map(|s| s.to_string()),
            path: rel,
            size: mhtml.len(),
        });
        Ok(Some(abs))
    }

    /// 运行时脚本（§3.2d）：由 app 在 `Debugger.scriptParsed` 里调
    pub async fn add_runtime_script(
        &mut self,
        cdp: &Cdp,
        session: &str,
        script_id: &str,
        url: &str,
        parent_url: Option<String>,
    ) -> Result<()> {
        if !self.seen_scripts.insert(script_id.to_string()) {
            return Ok(());
        }
        let Ok(src) = commands::get_script_source(cdp, session, script_id).await else {
            return Ok(());
        };
        if src.trim().len() < 24 {
            return Ok(());
        }
        // 探针自己注入的三个脚本没有 URL，会把 inline_scripts 灌满噪声
        if src.contains("__probeInstalled")
            || src.contains("__probeMutationInstalled")
            || src.contains("__probeInspectDom")
        {
            return Ok(());
        }
        // 有 URL 的脚本走普通资源途径（若还没抓到就补抓）
        if !url.is_empty() {
            if self.is_captured(url) {
                return Ok(());
            }
            let rel = match url_to_path(url) {
                Some(p) => p,
                None => return Ok(()),
            };
            let abs = self.root.join(&rel);
            if let Some(p) = abs.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::write(&abs, src.as_bytes())?;
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            self.clear_transient_skipped(url);
            self.seen_urls.insert(url.to_string());
            self.saved_paths.insert(rel_str.clone());
            self.total_bytes += src.len() as u64;
            self.fetched += 1;
            self.entries.push(MirrorEntry {
                url: url.to_string(),
                kind: "Script".to_string(),
                mime: Some("application/javascript".to_string()),
                path: Some(rel_str),
                size: Some(src.len()),
                sha256: Some(sha256_hex(src.as_bytes())),
                source: "debugger".to_string(),
                status: None,
                skipped: None,
            });
            return Ok(());
        }
        // 无 URL：eval / 动态 script → inline_scripts/
        self.script_seq += 1;
        let dir = self.root.join("inline_scripts");
        std::fs::create_dir_all(&dir)?;
        let rel = format!("inline_scripts/script-{:04}.js", self.script_seq);
        std::fs::write(self.root.join(&rel), src.as_bytes())?;
        self.total_bytes += src.len() as u64;
        self.inline.push(InlineScript {
            path: rel,
            size: src.len(),
            source: "debugger".to_string(),
            parent_url,
            script_id: Some(script_id.to_string()),
        });
        Ok(())
    }

    /// 内联脚本兜底（§3.2e）：Debugger 不可用时，从**已经保存下来的页面 HTML**
    /// 里抠 `<script>` 文本。
    ///
    /// 为什么不再发一次 `Runtime.evaluate`：页面可能被弹窗冻住、也可能刚在导航中，
    /// 而 HTML 已经在手上，纯文本解析更稳、更省一次往返，也好写单元测试。
    pub fn extract_inline_scripts(&mut self, html: &str, parent_url: &str) {
        use once_cell::sync::Lazy;
        use regex::Regex;
        // Rust 的 regex 不支持 look-ahead，所以先连属性一起抓，再自己排除带 src 的
        static RE: Lazy<Regex> = Lazy::new(|| {
            Regex::new(r#"(?is)<script\b([^>]*)>(.*?)</script>"#).unwrap()
        });

        let texts: Vec<String> = RE
            .captures_iter(html)
            .filter(|c| {
                let attrs = c.get(1).map(|m| m.as_str().to_ascii_lowercase()).unwrap_or_default();
                !attrs.contains("src")
            })
            .filter_map(|c| c.get(2).map(|m| m.as_str().to_string()))
            .filter(|t| t.trim().len() > 8)
            .collect();
        if texts.is_empty() {
            return;
        }
        if std::fs::create_dir_all(self.root.join("inline_scripts")).is_err() {
            return;
        }
        for text in texts {
            self.script_seq += 1;
            let rel = format!("inline_scripts/eval-{:04}.js", self.script_seq);
            if std::fs::write(self.root.join(&rel), text.as_bytes()).is_err() {
                continue;
            }
            self.total_bytes += text.len() as u64;
            self.inline.push(InlineScript {
                path: rel,
                size: text.len(),
                source: "eval".to_string(),
                parent_url: Some(parent_url.to_string()),
                script_id: None,
            });
        }
    }

    /// `INDEX.json`（§3.3）：resources 按 url 字典序，稳定；失败的也在里面
    pub fn index_json(&self) -> Value {
        let mut entries = self.entries.clone();
        entries.sort_by(|a, b| a.url.cmp(&b.url));
        let mut inline = self.inline.clone();
        inline.sort_by(|a, b| a.path.cmp(&b.path));
        let mut mhtml = self.mhtml.clone();
        mhtml.sort_by(|a, b| a.path.cmp(&b.path));

        json!({
            "host": self.host,
            "captured_at": crate::util::now_iso(),
            "mhtml": mhtml,
            "resources": entries,
            "inline_scripts": inline,
            "summary": {
                "total": self.fetched + self.skipped,
                "captured": self.fetched,
                "skipped": self.skipped,
                "bytes": self.total_bytes,
                "truncated": self.truncated,
                "inline": self.inline.len(),
                "mhtml": self.mhtml.len()
            }
        })
    }

    /// 写 INDEX.json + `00-先读我.txt`（§3.4 的警告）
    pub fn write_index(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::write(
            self.root.join("INDEX.json"),
            serde_json::to_vec_pretty(&self.index_json())?,
        )?;
        std::fs::write(
            self.root.join("00-先读我.txt"),
            "mirror/ 里是原样保存的前端源码（HTML/JS/CSS/图片/MHTML）。\n\
             为了保证代码可运行、可读，这里**没有脱敏**——可能含硬编码的账号、密钥、签名盐。\n\
             \n\
             ⚠️ 禁止外传、禁止上传公网、禁止放进任何外部工具。\n\
             如需复核是否含凭据，运行：probe.exe mirror-scan --out <out 目录>\n",
        )?;
        Ok(())
    }

    pub fn summary_line(&self) -> String {
        format!(
            "[镜像] 抓取 {} 个资源 / 跳过 {} 个 / inline {} 个 / MHTML {} 份 / {:.1} MB{}",
            self.fetched,
            self.skipped,
            self.inline.len(),
            self.mhtml.len(),
            self.total_bytes as f64 / 1048576.0,
            if self.truncated { "（已按预算截断）" } else { "" }
        )
    }
}

/// 递归展开 frameTree
fn collect_frames(v: &Value, out: &mut Vec<(String, String)>) {
    let Some(tree) = v.get("frameTree") else { return };
    collect_one(tree, out);
}

fn collect_one(node: &Value, out: &mut Vec<(String, String)>) {
    if let Some(frame) = node.get("frame") {
        let frame_id = frame.get("id").and_then(|s| s.as_str()).unwrap_or("").to_string();
        if !frame_id.is_empty() {
            if let Some(res) = node.get("resources").and_then(|r| r.as_array()) {
                for r in res {
                    let url = r.get("url").and_then(|u| u.as_str()).unwrap_or("");
                    let kind = r.get("type").and_then(|t| t.as_str()).unwrap_or("Other");
                    if !url.is_empty() && !url.starts_with("data:") {
                        out.push((url.to_string(), format!("{frame_id}|{kind}")));
                    }
                }
            }
        }
    }
    if let Some(kids) = node.get("childFrames").and_then(|c| c.as_array()) {
        for k in kids {
            collect_one(k, out);
        }
    }
}

/// `mirror-scan`：只读扫描 mirror 里的疑似凭据（§3.4）
pub fn scan_for_credentials(dir: &Path) -> Result<String> {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r#"(?i)(password|passwd|pwd|secret|appkey|app_key|accesskey|access_key|salt|sign|token|privatekey|private_key)\s*[:=]\s*['"]?([^\s'",;]{4,80})"#)
            .unwrap()
    });

    let mut findings: Vec<(String, usize, String, String)> = Vec::new();
    let mut files = 0usize;
    for path in walk(dir) {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if !matches!(ext.as_str(), "js" | "html" | "htm" | "css" | "json" | "txt" | "mhtml") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        files += 1;
        for (lineno, line) in text.lines().enumerate() {
            for cap in RE.captures_iter(line) {
                let key = cap.get(1).map(|m| m.as_str()).unwrap_or("");
                let val = cap.get(2).map(|m| m.as_str()).unwrap_or("");
                findings.push((
                    path.strip_prefix(dir).unwrap_or(&path).to_string_lossy().replace('\\', "/"),
                    lineno + 1,
                    key.to_string(),
                    crate::redact::redact_text(val),
                ));
            }
        }
    }

    let mut md = String::new();
    md.push_str("# mirror 凭据扫描结果（只读，需人工复核）\n\n");
    md.push_str(&format!(
        "扫描文件 {} 个，疑似命中 {} 处。\n\n",
        files,
        findings.len()
    ));
    md.push_str("> 值已脱敏显示。命中不等于一定是真凭据（可能是变量名、示例值），**请逐条人工确认**。\n\n");
    if findings.is_empty() {
        md.push_str("未发现疑似凭据键值。\n");
    } else {
        md.push_str("| 文件 | 行 | 键 | 值（脱敏） |\n|---|---|---|---|\n");
        for (f, l, k, v) in &findings {
            md.push_str(&format!("| `{f}` | {l} | `{k}` | `{v}` |\n"));
        }
    }
    Ok(md)
}

fn walk(dir: &Path) -> Vec<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_blocks_traversal_and_reserved() {
        let p = url_to_path("http://a:8080/b/../../etc/passwd").unwrap();
        let s = p.to_string_lossy().replace('\\', "/");
        assert!("a_8080/etc/passwd" == s || "a_8080/b/etc/passwd" == s, "实际 {s}");
        assert!(!s.contains(".."));

        let c = url_to_path("http://h:1/CON").unwrap();
        assert!(c.to_string_lossy().contains("_CON"));

        let q = url_to_path("http://h:1/a.js?v=2#x").unwrap();
        assert!(q.to_string_lossy().ends_with("a.js"));
        assert!(!q.to_string_lossy().contains('?'));
    }

    #[test]
    fn path_keeps_chinese_and_dir_index() {
        let p = url_to_path("http://h:1/中文/目录/").unwrap();
        let s = p.to_string_lossy().replace('\\', "/");
        assert!(s.contains("中文"));
        assert!(s.ends_with("目录/index.html"));
    }

    #[test]
    fn data_and_blob_rejected() {
        assert!(url_to_path("data:text/html,<b/>").is_none());
        assert!(url_to_path("blob:http://h/abc").is_none());
    }

    #[test]
    fn index_is_byte_stable_and_sorted() {
        let mut m = Mirror::new(PathBuf::from("out/mirror"), 300);
        m.host = "h:1".to_string();
        m.entries.push(MirrorEntry {
            url: "http://h/b.js".into(),
            kind: "Script".into(),
            mime: Some("application/javascript".into()),
            path: Some("h_1/b.js".into()),
            size: Some(3),
            sha256: Some(sha256_hex(b"abc")),
            source: "page_getResourceContent".into(),
            status: Some(200),
            skipped: None,
        });
        m.entries.push(MirrorEntry {
            url: "http://h/a.css".into(),
            kind: "Stylesheet".into(),
            mime: Some("text/css".into()),
            path: Some("h_1/a.css".into()),
            size: Some(2),
            sha256: Some(sha256_hex(b"ab")),
            source: "page_getResourceContent".into(),
            status: Some(200),
            skipped: None,
        });
        let a = m.index_json();
        let b = m.index_json();
        // captured_at 会变，比较 resources 部分
        let ra = serde_json::to_string(&a["resources"]).unwrap();
        let rb = serde_json::to_string(&b["resources"]).unwrap();
        assert_eq!(ra, rb);
        assert!(ra.find("a.css").unwrap() < ra.find("b.js").unwrap());
    }

    #[test]
    fn budget_drops_images_before_js() {
        // 已用 99，上限 100：图片必须放弃，JS 必须放行
        assert!(!budget_allows("image", 5, 99, 100));
        assert!(!budget_allows("css", 5, 99, 100));
        assert!(budget_allows("js", 5, 99, 100));
        assert!(budget_allows("html", 5, 99, 100));
        // 未超限时都放行
        assert!(budget_allows("image", 5, 10, 100));
    }

    #[test]
    fn skipped_entries_are_preserved() {
        let mut m = Mirror::new(PathBuf::from("out/mirror"), 300);
        m.host = "h".into();
        m.push_skipped("http://h/big.png", "Image", "image/png", "too_large");
        let idx = m.index_json();
        let res = idx["resources"].as_array().unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0]["skipped"], "too_large");
        assert_eq!(idx["summary"]["total"], 1);
        assert_eq!(idx["summary"]["skipped"], 1);
        assert_eq!(idx["summary"]["captured"], 0);
    }

    #[test]
    fn extracts_inline_scripts_but_not_external() {
        let mut m = Mirror::new(PathBuf::from("out/mirror-inline-test"), 300);
        m.host = "h".into();
        let html = r#"<html><head>
          <script>window.a = 1; console.log('inline one');</script>
          <script src="app.js"></script>
          <script type="text/javascript">var b = 2; // inline two</script>
          </head><body></body></html>"#;
        m.extract_inline_scripts(html, "http://h/index.html");
        assert_eq!(m.inline.len(), 2, "只应抓到两个内联，不抓 src 的");
        assert!(m.inline.iter().all(|i| i.path.starts_with("inline_scripts/eval-")));
        assert!(m.entries.is_empty());
        let _ = std::fs::remove_dir_all("out/mirror-inline-test");
    }

    #[test]
    fn sha256_is_recomputable() {
        let bytes = b"console.log(1)";
        let mut m = Mirror::new(PathBuf::from("out/mirror-test"), 300);
        m.host = "h".into();
        let rel = PathBuf::from("h/x.js");
        m.store("http://h/x.js", "Script", "application/javascript", "js", &rel, bytes, "test", None)
            .unwrap();
        let e = &m.entries[0];
        assert_eq!(e.sha256.as_deref(), Some(sha256_hex(bytes).as_str()));
        let _ = std::fs::remove_dir_all("out/mirror-test");
    }

    #[test]
    fn transient_failure_does_not_block_later_retry() {
        let dir = std::env::temp_dir().join("probe-mirror-retry-test");
        let _ = std::fs::remove_dir_all(&dir);
        let mut m = Mirror::new(dir.clone(), 300);
        let url = "https://10.4.188.1/delivery-web/static/delivery/js/setroadseg/main.js?261120";

        // 模拟第一次 walk_resource_tree 抢跑失败
        m.push_skipped_transient(url, "Script", "application/javascript", "getResourceContent_failed");
        assert!(!m.is_captured(url), "瞬时失败绝不能把 URL 标成已捕获");
        assert_eq!(m.skipped, 1);

        // 同一失败再发生一次不重复累加 skipped
        m.push_skipped_transient(url, "Script", "application/javascript", "getResponseBody_empty");
        assert_eq!(m.skipped, 1);

        // 后续通过兜底/重试成功拿到正文
        let wrote = m
            .store_raw(
                url,
                "Script",
                "application/javascript",
                b"function addWaybill() { return true; }",
                "same_origin_fetch",
                Some(200),
                false,
            )
            .unwrap();
        assert!(wrote, "补抓应成功落盘");
        assert!(m.is_captured(url));
        assert!(m.has_saved_suffix("setroadseg/main.js"));
        assert_eq!(m.fetched, 1);
        assert_eq!(m.skipped, 0, "补抓成功后旧的瞬时 skipped 应被清理");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_raw_overwrite_replaces_portal_shell() {
        let dir = std::env::temp_dir().join("probe-mirror-overwrite-test");
        let _ = std::fs::remove_dir_all(&dir);
        let mut m = Mirror::new(dir.clone(), 300);
        let url = "https://10.4.188.1/delivery-web/a/delivery/setroadseg/main";

        m.store_raw(url, "Document", "text/html", b"<html>portal shell</html>", "dom_outerHTML", Some(200), false)
            .unwrap();
        assert_eq!(m.fetched, 1);

        // 用真实网络 Document 响应覆盖
        m.store_raw(url, "Document", "text/html", b"<html><input id='itemid'></html>", "network_document", Some(200), true)
            .unwrap();
        assert_eq!(m.fetched, 1);
        let saved = std::fs::read_to_string(dir.join("10.4.188.1/delivery-web/a/delivery/setroadseg/main")).unwrap();
        assert!(saved.contains("itemid"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
