use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use magi_core::{SessionId, TaskId};

use crate::conversation::Conversation;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum ConversationKey {
    Session(SessionId),
    Task {
        session_id: SessionId,
        task_id: TaskId,
    },
}

/// 按 ConversationKey 持有 Conversation 实例。用户输入入口使用 session 级
/// Conversation；任务执行入口使用 task 级 Conversation，确保不同 task 拥有独立
/// Mailbox 与 Turn 并发边界。
#[derive(Debug, Default)]
pub struct ConversationRegistry {
    inner: Mutex<HashMap<ConversationKey, Arc<Mutex<Conversation>>>>,
}

impl ConversationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn conversation_for(&self, session_id: &SessionId) -> Arc<Mutex<Conversation>> {
        self.conversation_for_key(ConversationKey::Session(session_id.clone()), session_id)
    }

    pub fn conversation_for_task(
        &self,
        session_id: &SessionId,
        task_id: &TaskId,
    ) -> Arc<Mutex<Conversation>> {
        self.conversation_for_key(
            ConversationKey::Task {
                session_id: session_id.clone(),
                task_id: task_id.clone(),
            },
            session_id,
        )
    }

    fn conversation_for_key(
        &self,
        key: ConversationKey,
        session_id: &SessionId,
    ) -> Arc<Mutex<Conversation>> {
        let mut guard = self
            .inner
            .lock()
            .expect("ConversationRegistry mutex poisoned");
        guard
            .entry(key)
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

    #[test]
    fn task_conversations_are_isolated_from_session_conversation() {
        let registry = ConversationRegistry::new();
        let session_id = SessionId::new("session-a");
        let root = registry.conversation_for(&session_id);
        let task_a = registry.conversation_for_task(&session_id, &TaskId::new("task-a"));
        let task_b = registry.conversation_for_task(&session_id, &TaskId::new("task-b"));

        assert!(!Arc::ptr_eq(&root, &task_a));
        assert!(!Arc::ptr_eq(&task_a, &task_b));
        assert_eq!(registry.len(), 3);
    }
}
