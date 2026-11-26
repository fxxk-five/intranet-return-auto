//! 常用 CDP 命令的强类型薄封装。
//!
//! 只放"形状固定、会重复用到"的几条；其余直接在调用点写 `call(method, params)`，
//! 免得为了抽象而抽象（§3.2 的初衷就是别把协议藏起来）。

use anyhow::{bail, Result};
use base64::Engine;
use serde_json::json;

use super::Cdp;

/// 页面截图（§7.8.1），返回 PNG 字节。
pub async fn capture_screenshot(cdp: &Cdp, session: &str, clip: Option<(f64, f64, f64, f64)>) -> Result<Vec<u8>> {
    capture_screenshot_ex(cdp, session, clip, true).await
}

/// 可指定 `captureBeyondViewport` 的版本：
/// 快照带默认只截视口（长列表页整页截图会是 2000×8000 的巨图，见 PROMPT §4.5）
pub async fn capture_screenshot_ex(
    cdp: &Cdp,
    session: &str,
    clip: Option<(f64, f64, f64, f64)>,
    beyond_viewport: bool,
) -> Result<Vec<u8>> {
    let mut params = json!({
        "format": "png",
        "captureBeyondViewport": beyond_viewport,
        "fromSurface": true
    });
    if let Some((x, y, w, h)) = clip {
        params["clip"] = json!({"x": x, "y": y, "width": w, "height": h, "scale": 1});
    }
    let r = cdp.call("Page.captureScreenshot", params, Some(session)).await?;
    let b64 = r
        .get("data")
        .and_then(|d| d.as_str())
        .ok_or_else(|| anyhow::anyhow!("Page.captureScreenshot 没返回 data"))?;
    Ok(base64::engine::general_purpose::STANDARD.decode(b64)?)
}

/// 取响应体（§7.3.4）。资源已被回收时返回 Err，调用方记 body_status。
pub async fn get_response_body(cdp: &Cdp, session: &str, request_id: &str) -> Result<(Vec<u8>, bool)> {
    let r = cdp
        .call(
            "Network.getResponseBody",
            json!({"requestId": request_id}),
            Some(session),
        )
        .await?;
    let body = r.get("body").and_then(|b| b.as_str()).unwrap_or("");
    let encoded = r.get("base64Encoded").and_then(|b| b.as_bool()).unwrap_or(false);
    if encoded {
        Ok((base64::engine::general_purpose::STANDARD.decode(body)?, true))
    } else {
        Ok((body.as_bytes().to_vec(), false))
    }
}

/// 取所有 page/iframe target（Q4 用）
pub async fn get_targets(cdp: &Cdp) -> Result<Vec<serde_json::Value>> {
    let r = cdp.call_browser("Target.getTargets", json!({})).await?;
    match r.get("targetInfos").and_then(|t| t.as_array()) {
        Some(a) => Ok(a.clone()),
        None => bail!("Target.getTargets 返回体异常"),
    }
}

/// 放行/拒绝 JS 弹窗（§7.6）。不处理的话页面会冻结。
pub async fn handle_js_dialog(cdp: &Cdp, session: &str, accept: bool) -> Result<()> {
    cdp.call(
        "Page.handleJavaScriptDialog",
        json!({"accept": accept}),
        Some(session),
    )
    .await
    .map(|_| ())
}

/// 导出 PDF（正式程序阶段的打印替代方案，探针里不做实际操作，仅留能力）
pub async fn print_to_pdf(cdp: &Cdp, session: &str) -> Result<Vec<u8>> {
    let r = cdp
        .call("Page.printToPDF", json!({"printBackground": true}), Some(session))
        .await?;
    let b64 = r.get("data").and_then(|d| d.as_str()).unwrap_or("");
    Ok(base64::engine::general_purpose::STANDARD.decode(b64)?)
}

// ---------------------------------------------------------------------------
// 影印机（源码镜像 / 快照带）需要的封装（见 PROMPT §3.2、§7）
// 全部是薄封装：CDP 方法直发，不做二次抽象。
// ---------------------------------------------------------------------------

/// `Page.getResourceTree` —— 拿到所有 frame 的资源 URL 全集
pub async fn get_resource_tree(cdp: &Cdp, session: &str) -> Result<serde_json::Value> {
    cdp.call("Page.getResourceTree", json!({}), Some(session)).await
}

/// `Page.getResourceContent` —— 按 frameId + url 取资源正文（不依赖 requestId）
pub async fn get_resource_content(
    cdp: &Cdp,
    session: &str,
    frame_id: &str,
    url: &str,
) -> Result<(Vec<u8>, bool)> {
    let r = cdp
        .call(
            "Page.getResourceContent",
            json!({"frameId": frame_id, "url": url}),
            Some(session),
        )
        .await?;
    let content = r.get("content").and_then(|c| c.as_str()).unwrap_or("");
    let b64 = r.get("base64Encoded").and_then(|b| b.as_bool()).unwrap_or(false);
    if b64 {
        Ok((base64::engine::general_purpose::STANDARD.decode(content)?, true))
    } else {
        Ok((content.as_bytes().to_vec(), false))
    }
}

/// `Page.captureSnapshot` —— MHTML 整页存档（自包含，可离线打开）
pub async fn capture_snapshot_mhtml(cdp: &Cdp, session: &str) -> Result<String> {
    let r = cdp
        .call("Page.captureSnapshot", json!({"format": "mhtml"}), Some(session))
        .await?;
    Ok(r.get("data").and_then(|d| d.as_str()).unwrap_or("").to_string())
}

/// `Debugger.enable` —— 抓运行时脚本（eval / 动态 script）
pub async fn enable_debugger(cdp: &Cdp, session: &str, max_scripts_cache: u64) -> Result<()> {
    cdp.call(
        "Debugger.enable",
        json!({"maxScriptsCacheSize": max_scripts_cache}),
        Some(session),
    )
    .await
    .map(|_| ())
}

pub async fn disable_debugger(cdp: &Cdp, session: &str) -> Result<()> {
    cdp.call("Debugger.disable", json!({}), Some(session)).await.map(|_| ())
}

/// `Debugger.getScriptSource`
pub async fn get_script_source(cdp: &Cdp, session: &str, script_id: &str) -> Result<String> {
    let r = cdp
        .call(
            "Debugger.getScriptSource",
            json!({"scriptId": script_id}),
            Some(session),
        )
        .await?;
    Ok(r.get("scriptSource")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string())
}

/// 固定视口与 deviceScaleFactor：让同一操作在不同机器上截出同样尺寸的图
pub async fn set_device_metrics(
    cdp: &Cdp,
    session: &str,
    width: u32,
    height: u32,
    scale: f64,
) -> Result<()> {
    cdp.call(
        "Emulation.setDeviceMetricsOverride",
        json!({
            "width": width,
            "height": height,
            "deviceScaleFactor": scale,
            "mobile": false
        }),
        Some(session),
    )
    .await
    .map(|_| ())
}

/// 开始录屏（修复 4b）。`--no-filmstrip` 时不要调用。
pub async fn start_screencast(cdp: &Cdp, session: &str) -> Result<()> {
    cdp.call(
        "Page.startScreencast",
        json!({
            "format": "jpeg",
            "quality": 70,
            "maxWidth": 1280,
            "maxHeight": 720,
            "everyNthFrame": 2
        }),
        Some(session),
    )
    .await
    .map(|_| ())
}
