//! CDP 事件类型。

use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Event {
    pub method: String,
    pub params: Value,
    pub session_id: Option<String>,
}

impl Event {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.params.get(key)
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.params.get(key).and_then(|v| v.as_str())
    }

    /// `Runtime.bindingCalled` 的 JSON 载荷（探针自己的 JS 都走这个通道）
    pub fn binding_payload(&self) -> Option<Value> {
        if self.method != "Runtime.bindingCalled" {
            return None;
        }
        let raw = self.params.get("payload")?.as_str()?;
        serde_json::from_str(raw).ok()
    }
}
