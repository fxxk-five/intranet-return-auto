//! 弹窗分类（§7.6）：dom_modal / js_dialog / native_window / print_preview。
//!
//! 关键点：这四类里只有前两类 CDP 能看见，原生窗口必须靠 `EnumWindows` 增量。

use serde_json::{json, Value};
use std::collections::HashMap;

use crate::win32::WinInfo;

pub const DOM_MODAL: &str = "dom_modal";
pub const JS_DIALOG: &str = "js_dialog";
pub const NATIVE_WINDOW: &str = "native_window";
pub const PRINT_PREVIEW: &str = "print_preview";

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Evidence {
    pub selector: Option<String>,
    pub text_preview: Option<String>,
    pub rect: Option<[i32; 4]>,
    pub z_index: Option<i32>,
    pub hwnd: Option<isize>,
    pub class: Option<String>,
    pub title: Option<String>,
    pub pid: Option<u32>,
    pub exe: Option<String>,
    pub screenshot: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DialogRec {
    pub seq: u64,
    pub ts: String,
    pub step: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub trigger_click_seq: Option<u64>,
    pub trigger_click_text: Option<String>,
    pub matched_request_ids: Vec<String>,
    pub evidence: Evidence,
    pub close_method: Option<String>,
    pub close_selector: Option<String>,
}

pub struct DialogStore {
    pub dialogs: Vec<DialogRec>,
    pub counts: HashMap<String, usize>,
    /// 探针启动时的窗口快照，用来做增量
    baseline: Vec<WinInfo>,
    step: Option<String>,
    click: Option<(u64, String)>,
}

impl DialogStore {
    pub fn new() -> DialogStore {
        let mut counts = HashMap::new();
        for k in [DOM_MODAL, JS_DIALOG, NATIVE_WINDOW, PRINT_PREVIEW] {
            counts.insert(k.to_string(), 0);
        }
        DialogStore {
            dialogs: Vec::new(),
            counts,
            baseline: crate::win32::list_windows(),
            step: None,
            click: None,
        }
    }

    pub fn set_step(&mut self, step: Option<String>) {
        self.step = step;
    }

    pub fn note_click(&mut self, seq: u64, text: String) {
        self.click = Some((seq, text));
    }

    fn push(&mut self, kind: &str, evidence: Evidence, close_method: Option<&str>, close_selector: Option<String>) -> u64 {
        *self.counts.entry(kind.to_string()).or_insert(0) += 1;
        let seq = self.dialogs.len() as u64 + 1;
        self.dialogs.push(DialogRec {
            seq,
            ts: crate::util::now_iso(),
            step: self.step.clone(),
            kind: kind.to_string(),
            trigger_click_seq: self.click.as_ref().map(|(s, _)| *s),
            trigger_click_text: self.click.as_ref().map(|(_, t)| t.clone()),
            matched_request_ids: Vec::new(),
            evidence,
            close_method: close_method.map(|s| s.to_string()),
            close_selector,
        });
        seq
    }

    /// DOM 模态框（来自 mutation_probe.js）
    pub fn add_dom(&mut self, selector: Option<String>, text: Option<String>, rect: Option<[i32; 4]>, z: Option<i32>) -> u64 {
        let close_selector = selector
            .as_ref()
            .map(|s| format!("{s} button.el-dialog__headerbtn, {s} .modal-close, {s} .el-message-box__headerbtn"));
        self.push(
            DOM_MODAL,
            Evidence {
                selector,
                text_preview: text,
                rect,
                z_index: z,
                ..Default::default()
            },
            Some("click_x"),
            close_selector,
        )
    }

    /// JS 原生弹窗（alert/confirm/prompt）
    pub fn add_js(&mut self, message: Option<String>, dialog_type: Option<String>) -> u64 {
        self.push(
            JS_DIALOG,
            Evidence {
                text_preview: message,
                title: dialog_type,
                ..Default::default()
            },
            Some("handleJavaScriptDialog"),
            None,
        )
    }

    /// 原生窗口（PrintWindow 截图由调用方补）
    pub fn add_native(&mut self, w: &WinInfo, screenshot: Option<String>) -> u64 {
        let kind = classify_window(w);
        self.push(
            kind,
            Evidence {
                hwnd: Some(w.hwnd),
                class: Some(w.class.clone()),
                title: Some(w.title.clone()),
                pid: Some(w.pid),
                exe: w.exe.clone(),
                rect: Some(w.rect),
                screenshot,
                ..Default::default()
            },
            Some("用户手动关闭（探针不碰 Win32 弹窗）"),
            None,
        )
    }

    /// 窗口增量：返回"新出现的"窗口（调用方负责截图后 add_native）
    pub fn diff_windows(&mut self, cur: &[WinInfo]) -> Vec<WinInfo> {
        let known: std::collections::HashSet<isize> =
            self.baseline.iter().map(|w| w.hwnd).collect();
        let known_after = self
            .dialogs
            .iter()
            .filter_map(|d| d.evidence.hwnd)
            .collect::<std::collections::HashSet<_>>();

        let new: Vec<WinInfo> = cur
            .iter()
            .filter(|w| !known.contains(&w.hwnd) && !known_after.contains(&w.hwnd))
            .filter(|w| w.visible)
            .filter(|w| w.rect[2] > 80 && w.rect[3] > 40)
            .filter(|w| !is_noise_window(w))
            .cloned()
            .collect();

        // 已经消失的窗口从 baseline 里移除，避免同一 hwnd 被复用后漏记
        self.baseline.retain(|b| cur.iter().any(|c| c.hwnd == b.hwnd));
        new
    }

    /// 从 `timeline.jsonl` 的弹窗事件重建（`report` 子命令用）
    pub fn ingest_timeline(&mut self, v: &Value) {
        let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        match kind {
            "dialog_dom" | "dialog_js" | "dialog_native" => {
                let ty = match kind {
                    "dialog_dom" => DOM_MODAL,
                    "dialog_js" => JS_DIALOG,
                    _ => v
                        .get("kind_of_window")
                        .and_then(|k| k.as_str())
                        .unwrap_or(NATIVE_WINDOW),
                };
                if v.get("kind_of_window").is_none() && kind == "dialog_native" {
                    let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("");
                    let class = v.get("class").and_then(|x| x.as_str()).unwrap_or("");
                    let w = WinInfo {
                        hwnd: v.get("hwnd").and_then(|x| x.as_i64()).unwrap_or(0) as isize,
                        title: title.to_string(),
                        class: class.to_string(),
                        pid: v.get("pid").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                        exe: v.get("exe").and_then(|x| x.as_str()).map(|s| s.to_string()),
                        rect: [0, 0, 0, 0],
                        visible: true,
                    };
                    self.push(
                        classify_window(&w),
                        Evidence {
                            hwnd: Some(w.hwnd),
                            class: Some(w.class.clone()),
                            title: Some(w.title.clone()),
                            pid: Some(w.pid),
                            exe: w.exe.clone(),
                            screenshot: v.get("screenshot").and_then(|x| x.as_str()).map(|s| s.to_string()),
                            ..Default::default()
                        },
                        Some("用户手动关闭"),
                        None,
                    );
                    return;
                }
                *self.counts.entry(ty.to_string()).or_insert(0) += 1;
                self.dialogs.push(DialogRec {
                    seq: self.dialogs.len() as u64 + 1,
                    ts: v.get("ts").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    step: v.get("step").and_then(|x| x.as_str()).map(|s| s.to_string()),
                    kind: ty.to_string(),
                    trigger_click_seq: None,
                    trigger_click_text: None,
                    matched_request_ids: Vec::new(),
                    evidence: Evidence {
                        selector: v.get("selector").and_then(|x| x.as_str()).map(|s| s.to_string()),
                        text_preview: v
                            .get("text_preview")
                            .or_else(|| v.get("message"))
                            .and_then(|x| x.as_str())
                            .map(|s| s.to_string()),
                        title: v.get("title").and_then(|x| x.as_str()).map(|s| s.to_string()),
                        screenshot: v.get("screenshot").and_then(|x| x.as_str()).map(|s| s.to_string()),
                        ..Default::default()
                    },
                    close_method: None,
                    close_selector: None,
                });
            }
            _ => {}
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "generated_at": crate::util::now_iso(),
            "dialogs": self.dialogs,
            "classifications": {
                DOM_MODAL: self.counts.get(DOM_MODAL).copied().unwrap_or(0),
                JS_DIALOG: self.counts.get(JS_DIALOG).copied().unwrap_or(0),
                NATIVE_WINDOW: self.counts.get(NATIVE_WINDOW).copied().unwrap_or(0),
                PRINT_PREVIEW: self.counts.get(PRINT_PREVIEW).copied().unwrap_or(0),
            }
        })
    }
}

/// §7.6 表格的判定：标题含"打印/Print"或 Chrome_WidgetWin 且像打印预览 → print_preview
pub fn classify_window(w: &WinInfo) -> &'static str {
    let title = &w.title;
    let class = &w.class;
    // 只有标题明确提到打印、或类名是打印预览，才算 print_preview。
    // 光有 Chrome_WidgetWin 不算 —— 浏览器的每个窗口都是这个类名，
    // 把浏览器自己的窗口误判成"打印预览"会让 Q5/Q7 结论整体失真。
    let looks_print = title.contains("打印")
        || title.to_ascii_lowercase().contains("print")
        || class.contains("PrintPreview")
        || class.contains("PrintDialog");
    if looks_print {
        PRINT_PREVIEW
    } else {
        NATIVE_WINDOW
    }
}

fn is_noise_window(w: &WinInfo) -> bool {
    let c = &w.class;
    let t = &w.title;
    c.starts_with("Shell_")
        || c.contains("IME")
        || c.contains("TaskList")
        || c.contains("Foreground")
        || c == "Progman"
        || c == "WorkerW"
        || t.is_empty() && c.is_empty()
        || c.contains("MsctfIME")
        || c.contains("Chrome_SystemMessageDialog") // 浏览器自身的"恢复页面"之类
        || (c.contains("Chrome_WidgetWin") && t.trim().is_empty()) // 浏览器的无标题辅助窗口
        || c == "Intermediate D3D Window"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(title: &str, class: &str) -> WinInfo {
        WinInfo {
            hwnd: 1,
            title: title.into(),
            class: class.into(),
            pid: 1,
            exe: Some("msedge.exe".into()),
            rect: [0, 0, 800, 600],
            visible: true,
        }
    }

    #[test]
    fn print_preview_by_title() {
        assert_eq!(classify_window(&win("打印", "Chrome_WidgetWin_1")), PRINT_PREVIEW);
        assert_eq!(classify_window(&win("Print", "XYZ")), PRINT_PREVIEW);
    }

    #[test]
    fn native_by_default() {
        assert_eq!(classify_window(&win("填写收件人手机号", "TForm")), NATIVE_WINDOW);
    }
}
