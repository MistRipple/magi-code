//! GPT Web 单槽位的可重建运行态投影。

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
    pub session_id: String,
    pub thread_id: String,
    pub engine_id: String,
    pub page_id: Option<String>,
    pub stage: WebModelTurnStage,
    pub active: bool,
    pub sent_messages: u64,
    pub updated_at_ms: u64,
}

#[derive(Default)]
struct RuntimeInner {
    entry: Option<WebModelRuntimeEntry>,
}

/// 只保留一个 Web 槽位的运行投影。正文、远端上下文和登录态不在这里存储。
#[derive(Default)]
pub struct WebModelRuntimeRegistry {
    inner: Mutex<RuntimeInner>,
}

impl WebModelRuntimeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mark_web_turn(&self, session_id: &str, thread_id: &str, engine_id: &str, page_id: &str) {
        let mut inner = self.inner.lock().expect("web runtime lock poisoned");
        let sent_messages = inner
            .entry
            .as_ref()
            .filter(|entry| entry.session_id == session_id)
            .map(|entry| entry.sent_messages)
            .unwrap_or_default();
        inner.entry = Some(WebModelRuntimeEntry {
            session_id: session_id.to_string(),
            thread_id: thread_id.to_string(),
            engine_id: engine_id.to_string(),
            page_id: Some(page_id.to_string()),
            stage: WebModelTurnStage::Generating,
            active: true,
            sent_messages,
            updated_at_ms: now_ms(),
        });
    }

    pub fn record_web_send(&self, session_id: &str) {
        let mut inner = self.inner.lock().expect("web runtime lock poisoned");
        if let Some(entry) = inner
            .entry
            .as_mut()
            .filter(|entry| entry.session_id == session_id)
        {
            entry.sent_messages = entry.sent_messages.saturating_add(1);
            entry.updated_at_ms = now_ms();
        }
    }

    pub fn finish_web_turn(&self, session_id: &str) {
        let mut inner = self.inner.lock().expect("web runtime lock poisoned");
        if let Some(entry) = inner
            .entry
            .as_mut()
            .filter(|entry| entry.session_id == session_id)
        {
            entry.active = false;
            entry.stage = WebModelTurnStage::Finished;
            entry.updated_at_ms = now_ms();
        }
    }

    pub fn forget_session(&self, session_id: &str) -> usize {
        let mut inner = self.inner.lock().expect("web runtime lock poisoned");
        if inner
            .entry
            .as_ref()
            .is_some_and(|entry| entry.session_id == session_id)
        {
            inner.entry = None;
            1
        } else {
            0
        }
    }

    pub fn snapshot(&self, session_id: Option<&str>) -> Vec<WebModelRuntimeEntry> {
        let inner = self.inner.lock().expect("web runtime lock poisoned");
        inner
            .entry
            .as_ref()
            .filter(|entry| session_id.is_none_or(|id| id == entry.session_id))
            .cloned()
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_is_single_slot() {
        let runtime = WebModelRuntimeRegistry::new();
        runtime.mark_web_turn("s", "orchestrator", "chatgpt-web/default", "home");
        runtime.record_web_send("s");
        let entries = runtime.snapshot(Some("s"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].sent_messages, 1);
    }
}
