//! CDP 客户端门面：连浏览器级 WebSocket、下发命令、管理 target/session。
//!
//! 用**浏览器级**端点（`/json/version` 里的 `webSocketDebuggerUrl`）而不是
//! 逐个 page 端点，是为了：
//! - 用 `Target.setAutoAttach` 自动接管 4 个标签页（Q4）
//! - 自动接管跨域 iframe（OOPIF）——它们的 DOM 单独一个 session
//!
//! 命令带 `sessionId` 即作用于某个 target，不带即作用于浏览器本身。

pub mod commands;
pub mod events;
pub mod transport;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::http1;
use events::Event;
use transport::Transport;

pub struct Cdp {
    pub t: Transport,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TargetInfo {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub url: String,
}

impl Cdp {
    /// 连浏览器级 WebSocket，返回 (Cdp, 事件流)。
    pub async fn connect(port: u16) -> Result<(Arc<Cdp>, UnboundedReceiver<Event>)> {
        let raw = http1::get_text(port, "/json/version", 3000)
            .await
            .map_err(|e| anyhow!("读 /json/version 失败（Edge 是否带 --remote-debugging-port 启动？）：{e}"))?;
        let v: Value = serde_json::from_str(&raw)?;
        let ws = v
            .get("webSocketDebuggerUrl")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow!("/json/version 里没有 webSocketDebuggerUrl：{raw}"))?;
        let (t, rx) = Transport::connect(ws).await?;
        Ok((Arc::new(Cdp { t }), rx))
    }

    pub async fn version(port: u16) -> Result<Value> {
        let raw = http1::get_text(port, "/json/version", 3000).await?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub async fn list_targets(port: u16) -> Result<Vec<TargetInfo>> {
        let raw = http1::get_text(port, "/json/list", 3000).await?;
        let arr: Vec<Value> = serde_json::from_str(&raw)?;
        Ok(arr
            .into_iter()
            .map(|v| TargetInfo {
                id: v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                kind: v.get("type").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                title: v.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                url: v.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            })
            .collect())
    }

    pub async fn call(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        self.t.call(method, params, session).await
    }

    /// 浏览器级（无 sessionId）
    pub async fn call_browser(&self, method: &str, params: Value) -> Result<Value> {
        self.t.call(method, params, None).await
    }

    /// 打开自动挂接：所有 page/iframe 一出现就 attach，并且 flatten（sessionId 直接可用）。
    pub async fn enable_auto_attach(&self) -> Result<()> {
        self.call_browser("Target.setDiscoverTargets", json!({"discover": true})).await?;
        self.call_browser(
            "Target.setAutoAttach",
            json!({
                "autoAttach": true,
                "waitForDebuggerOnStart": false,
                "flatten": true
            }),
        )
        .await?;
        Ok(())
    }

    /// 对一个 target 会话做全套启用（§7.3.1）。任何一项失败都不致命，只记日志。
    pub async fn enable_domains(&self, session: &str) {
        let jobs: [(&str, Value); 4] = [
            (
                "Network.enable",
                json!({
                    "maxTotalBufferSize": 104857600,
                    "maxResourceBufferSize": 10485760,
                    "maxPostDataSize": 1048576
                }),
            ),
            ("Page.enable", json!({})),
            ("Runtime.enable", json!({})),
            ("DOM.enable", json!({})),
        ];
        for (m, p) in jobs {
            if let Err(e) = self.call(m, p, Some(session)).await {
                tracing::warn!("{m} 在 session {session} 上失败：{e:#}");
            }
        }
    }

    /// 装 `window.__probe` 回调通道（§7.5.1）
    pub async fn add_binding(&self, session: &str, name: &str) -> Result<()> {
        self.call("Runtime.addBinding", json!({ "name": name }), Some(session))
            .await
            .map(|_| ())
    }

    /// 每个新文档都装一次（导航后仍然有效）
    pub async fn add_script_on_new(&self, session: &str, source: &str) -> Result<()> {
        self.call(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": source }),
            Some(session),
        )
        .await
        .map(|_| ())
    }

    /// 在当前文档里执行表达式，`returnByValue` 直接拿结构化结果。
    pub async fn evaluate(&self, session: &str, expr: &str) -> Result<Value> {
        let r = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": expr,
                    "returnByValue": true,
                    "awaitPromise": true,
                    "userGesture": false,
                    "silent": true
                }),
                Some(session),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            bail!("页面脚本异常：{ex}");
        }
        Ok(r.get("result").and_then(|x| x.get("value")).cloned().unwrap_or(Value::Null))
    }
}
