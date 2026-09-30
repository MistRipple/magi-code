//! GPT Web 单槽位的可重建运行态投影。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or_default()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebModelTurnStage {
    Generating,
    Finished,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebModelRuntimeEntry {
    pub binding_key: String,
    pub session_id: String,
    pub thread_id: String,
    pub engine_id: String,
    pub effort: String,
    pub epoch: u64,
    pub page_id: Option<String>,
    pub stage: WebModelTurnStage,
    pub active: bool,
    pub queue_position: Option<usize>,
    pub sent_messages: u64,
    pub last_sent_tokens: u64,
    pub ownership: Option<String>,
    pub updated_at_ms: u64,
}

#[derive(Default)]
struct RuntimeInner {
    entries: HashMap<String, WebModelRuntimeEntry>,
}

/// 只保留一个 Web 槽位的运行投影。正文、远端上下文和登录态不在这里存储。
#[derive(Default)]
pub struct WebModelRuntimeRegistry {
    inner: Mutex<RuntimeInner>,
}

impl WebModelRuntimeRegistry {
    pub fn new() -> Self { Self::default() }

    fn entry_key(session_id: &str) -> String { format!("web:{session_id}") }

    pub fn mark_web_turn(
        &self,
        session_id: &str,
        thread_id: &str,
        engine_id: &str,
        page_id: &str,
    ) {
        let mut inner = self.inner.lock().expect("web runtime lock poisoned");
        let key = Self::entry_key(session_id);
        inner.entries.insert(key.clone(), WebModelRuntimeEntry {
            binding_key: key,
            session_id: session_id.to_string(),
            thread_id: thread_id.to_string(),
            engine_id: engine_id.to_string(),
            effort: "web-selected-in-page".to_string(),
            epoch: 1,
            page_id: Some(page_id.to_string()),
            stage: WebModelTurnStage::Generating,
            active: true,
            queue_position: None,
            sent_messages: 0,
            last_sent_tokens: 0,
            ownership: Some("magi_owned".to_string()),
            updated_at_ms: now_ms(),
        });
    }

    pub fn record_web_send(&self, session_id: &str) {
        let mut inner = self.inner.lock().expect("web runtime lock poisoned");
        if let Some(entry) = inner.entries.get_mut(&Self::entry_key(session_id)) {
            entry.sent_messages = entry.sent_messages.saturating_add(1);
            entry.updated_at_ms = now_ms();
        }
    }

    pub fn finish_web_turn(&self, session_id: &str) {
        let mut inner = self.inner.lock().expect("web runtime lock poisoned");
        if let Some(entry) = inner.entries.get_mut(&Self::entry_key(session_id)) {
            entry.active = false;
            entry.stage = WebModelTurnStage::Finished;
            entry.updated_at_ms = now_ms();
        }
    }

    pub fn forget_session(&self, session_id: &str) -> usize {
        self.inner
            .lock()
            .expect("web runtime lock poisoned")
            .entries
            .remove(&Self::entry_key(session_id))
            .map(|_| 1)
            .unwrap_or_default()
    }

    /// 兼容 reset 路径的无状态清理入口。键的具体类型不再是业务事实。
    pub fn forget_key<T: ?Sized>(&self, _key: &T) -> Option<WebModelRuntimeEntry> {
        None
    }

    pub fn snapshot(&self, session_id: Option<&str>) -> Vec<WebModelRuntimeEntry> {
        let inner = self.inner.lock().expect("web runtime lock poisoned");
        let mut entries = inner.entries.values()
            .filter(|entry| session_id.is_none_or(|id| id == entry.session_id))
            .cloned().collect::<Vec<_>>();
        entries.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_is_single_slot_and_does_not_expose_queue() {
        let runtime = WebModelRuntimeRegistry::new();
        runtime.mark_web_turn("s", "orchestrator", "chatgpt-web/default", "home");
        runtime.record_web_send("s");
        let entries = runtime.snapshot(Some("s"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].sent_messages, 1);
        assert_eq!(entries[0].queue_position, None);
    }
}
