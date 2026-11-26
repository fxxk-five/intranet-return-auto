//! WebSocket 传输层：连 CDP、收发 JSON 帧、把响应和事件分流。
//!
//! CDP 的协议面极小（§3.2）：
//! ```text
//! 发：{"id":N,"method":"...","params":{...},"sessionId":"<可选>"}
//! 回：{"id":N,"result":{...}}  或  {"id":N,"error":{...}}
//! 推：{"method":"...","params":{...},"sessionId":"<可选>"}
//! ```
//! 所以自己实现，不用 chromiumoxide（§3.2 硬性规定）。

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::cdp::events::Event;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, String>>>>>;

pub struct Transport {
    cmd_tx: mpsc::UnboundedSender<Value>,
    pending: Pending,
    next_id: AtomicU64,
    closed: Arc<AtomicBool>,
}

impl Transport {
    /// 连上一条 CDP WebSocket，返回 (Transport, 事件接收端)。
    pub async fn connect(ws_url: &str) -> Result<(Transport, mpsc::UnboundedReceiver<Event>)> {
        let (ws, _resp) = tokio_tungstenite::connect_async(ws_url)
            .await
            .with_context(|| format!("连接 CDP WebSocket 失败：{ws_url}"))?;

        let (mut sink, mut stream) = ws.split();
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Value>();
        let (evt_tx, evt_rx) = mpsc::unbounded_channel::<Event>();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));

        // 写端
        tokio::spawn(async move {
            while let Some(v) = cmd_rx.recv().await {
                let txt = v.to_string();
                if let Err(e) = sink.send(Message::Text(txt.into())).await {
                    tracing::warn!("CDP 写失败：{e}");
                    break;
                }
            }
            let _ = sink.close().await;
        });

        // 读端
        let pending_reader = pending.clone();
        let closed_reader = closed.clone();
        let dialog_tx = cmd_tx.clone();
        tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(Message::Text(txt)) => {
                        let v: Value = match serde_json::from_str(txt.as_str()) {
                            Ok(v) => v,
                            Err(e) => {
                                tracing::warn!("CDP 收到非 JSON 帧（忽略）：{e}");
                                continue;
                            }
                        };
                        let id = v.get("id").and_then(|x| x.as_u64());
                        match id {
                            Some(id) => {
                                let tx = pending_reader.lock().unwrap().remove(&id);
                                if let Some(tx) = tx {
                                    let payload = if let Some(err) = v.get("error") {
                                        Err(err.to_string())
                                    } else {
                                        Ok(v.get("result").cloned().unwrap_or(Value::Null))
                                    };
                                    let _ = tx.send(payload);
                                }
                            }
                            None => {
                                let method = v
                                    .get("method")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let params = v.get("params").cloned().unwrap_or(Value::Null);
                                let session_id = v
                                    .get("sessionId")
                                    .and_then(|s| s.as_str())
                                    .map(|s| s.to_string());

                                // 弹窗放行必须在这里做，不能等主循环：
                                // 主循环可能正卡在截图上，而页面被 alert 冻住时截图也不会返回，
                                // 两边互等就是死锁（实测踩到过，表现为驱动侧超时 + 探针卡死）。
                                if method == "Page.javascriptDialogOpening" {
                                    let mut f = json!({
                                        "id": 900_000_001u64,
                                        "method": "Page.handleJavaScriptDialog",
                                        "params": {"accept": true}
                                    });
                                    if let Some(sid) = &session_id {
                                        f["sessionId"] = Value::String(sid.clone());
                                    }
                                    let _ = dialog_tx.send(f);
                                }
                                let _ = evt_tx.send(Event {
                                    method,
                                    params,
                                    session_id,
                                });
                            }
                        }
                    }
                    Ok(Message::Close(_)) => break,
                    Ok(_) => {} // Ping/Pong/Binary 忽略
                    Err(e) => {
                        tracing::warn!("CDP 读失败：{e}");
                        break;
                    }
                }
            }
            closed_reader.store(true, Ordering::SeqCst);
            // 唤醒所有等待中的调用方
            let mut map = pending_reader.lock().unwrap();
            for (_, tx) in map.drain() {
                let _ = tx.send(Err("CDP 连接已关闭".to_string()));
            }
            let _ = evt_tx.send(Event {
                method: "__closed".to_string(),
                params: Value::Null,
                session_id: None,
            });
        });

        Ok((
            Transport {
                cmd_tx,
                pending,
                next_id: AtomicU64::new(1),
                closed,
            },
            evt_rx,
        ))
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// 发命令并等结果，超时 10s（§15.3）。
    pub async fn call(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        if self.is_closed() {
            bail!("CDP 连接已关闭，无法发送 {method}");
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);

        let mut frame = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            frame["sessionId"] = Value::String(s.to_string());
        }
        if self.cmd_tx.send(frame).is_err() {
            bail!("CDP 写通道已关闭，无法发送 {method}");
        }

        match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(anyhow!("CDP {method} 返回错误：{e}")),
            Ok(Err(_)) => Err(anyhow!("CDP {method} 响应通道中断")),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(anyhow!("CDP {method} 超时（10s）"))
            }
        }
    }
}
