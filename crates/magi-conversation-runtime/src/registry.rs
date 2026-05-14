use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use magi_core::SessionId;

use crate::conversation::Conversation;

/// 按 SessionId 持有 Conversation 实例。每个 SessionId 至多一个 Conversation，
/// 首次访问时 lazy 创建。
///
/// 后续 slice 引入并发执行（S6 SpawnGraph）后，本 registry 也会承担父子 Conversation
/// 的关系记录；S1 仅保留扁平 map。
#[derive(Debug, Default)]
pub struct ConversationRegistry {
    inner: Mutex<HashMap<SessionId, Arc<Mutex<Conversation>>>>,
}

impl ConversationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn conversation_for(&self, session_id: &SessionId) -> Arc<Mutex<Conversation>> {
        let mut guard = self
            .inner
            .lock()
            .expect("ConversationRegistry mutex poisoned");
        guard
            .entry(session_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(Conversation::new(session_id.clone()))))
            .clone()
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("ConversationRegistry mutex poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mailbox::UserSignal;
    use magi_core::UtcMillis;

    #[test]
    fn lazily_creates_then_reuses_same_conversation() {
        let registry = ConversationRegistry::new();
        let session = SessionId::new("session-a");

        let conv1 = registry.conversation_for(&session);
        conv1.lock().unwrap().ingest_user_signal(UserSignal {
            text: Some("first".to_string()),
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            accepted_at: UtcMillis(1),
        });

        let conv2 = registry.conversation_for(&session);
        assert!(Arc::ptr_eq(&conv1, &conv2));

        let drained = conv2.lock().unwrap().drain_user_signals();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].text.as_deref(), Some("first"));
    }

    #[test]
    fn different_sessions_get_distinct_conversations() {
        let registry = ConversationRegistry::new();
        let a = registry.conversation_for(&SessionId::new("a"));
        let b = registry.conversation_for(&SessionId::new("b"));
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(registry.len(), 2);
    }
}
