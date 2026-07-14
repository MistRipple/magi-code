use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use magi_core::{SessionId, TaskId};

use crate::conversation::Conversation;
use crate::mailbox::UserSignal;

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
    session_turn_inputs: Mutex<HashMap<SessionId, SessionTurnInputState>>,
}

#[derive(Debug)]
struct SessionTurnInputState {
    turn_id: String,
    pending: VecDeque<UserSignal>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionTurnInputError {
    AlreadyActive {
        active_turn_id: String,
    },
    NoActiveTurn,
    TurnMismatch {
        active_turn_id: String,
        expected_turn_id: String,
    },
}

impl std::fmt::Display for SessionTurnInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyActive { active_turn_id } => {
                write!(f, "session already has active input turn {active_turn_id}")
            }
            Self::NoActiveTurn => f.write_str("session has no active input turn"),
            Self::TurnMismatch {
                active_turn_id,
                expected_turn_id,
            } => write!(
                f,
                "active input turn {active_turn_id} does not match expected turn {expected_turn_id}"
            ),
        }
    }
}

impl std::error::Error for SessionTurnInputError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionTurnInputBoundary {
    Pending(Vec<UserSignal>),
    Closed,
}

#[derive(Debug)]
pub enum SessionTurnInputCommitError<E> {
    Input(SessionTurnInputError),
    Commit(E),
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

    /// 注册当前主会话 Turn 的引导输入通道。每个 session 同时只能有一个活跃
    /// Turn，后续引导必须携带精确的 turn_id 才能进入该通道。
    pub fn begin_session_turn_input(
        &self,
        session_id: SessionId,
        turn_id: String,
    ) -> Result<(), SessionTurnInputError> {
        let mut guard = self
            .session_turn_inputs
            .lock()
            .expect("session turn input mutex poisoned");
        if let Some(active) = guard.get(&session_id) {
            return Err(SessionTurnInputError::AlreadyActive {
                active_turn_id: active.turn_id.clone(),
            });
        }
        guard.insert(
            session_id,
            SessionTurnInputState {
                turn_id,
                pending: VecDeque::new(),
            },
        );
        Ok(())
    }

    /// 把引导输入追加到当前 Turn。expected_turn_id 是完成边界校验条件，禁止把
    /// 已完成 Turn 的迟到输入串入下一轮。
    pub fn try_steer_session_turn(
        &self,
        session_id: &SessionId,
        expected_turn_id: &str,
        signal: UserSignal,
    ) -> Result<(), SessionTurnInputError> {
        self.try_steer_session_turn_with(session_id, expected_turn_id, signal, || {
            Ok::<(), std::convert::Infallible>(())
        })
        .map_err(|error| match error {
            SessionTurnInputCommitError::Input(error) => error,
            SessionTurnInputCommitError::Commit(never) => match never {},
        })
    }

    /// 在持有 Turn 输入边界锁时先提交关联状态，再把信号加入 FIFO。该入口用于把
    /// canonical 用户项写入与引导接收收敛为一次不可穿插的状态变更。
    pub fn try_steer_session_turn_with<T, E, F>(
        &self,
        session_id: &SessionId,
        expected_turn_id: &str,
        signal: UserSignal,
        commit: F,
    ) -> Result<T, SessionTurnInputCommitError<E>>
    where
        F: FnOnce() -> Result<T, E>,
    {
        let mut guard = self
            .session_turn_inputs
            .lock()
            .expect("session turn input mutex poisoned");
        let active = guard
            .get_mut(session_id)
            .ok_or(SessionTurnInputCommitError::Input(
                SessionTurnInputError::NoActiveTurn,
            ))?;
        if active.turn_id != expected_turn_id {
            return Err(SessionTurnInputCommitError::Input(
                SessionTurnInputError::TurnMismatch {
                    active_turn_id: active.turn_id.clone(),
                    expected_turn_id: expected_turn_id.to_string(),
                },
            ));
        }
        let committed = commit().map_err(SessionTurnInputCommitError::Commit)?;
        active.pending.push_back(signal);
        Ok(committed)
    }

    /// 在工具轮结束后读取当前已到达的引导，但保持 Turn 继续接收后续引导。
    pub fn drain_session_turn_steers(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Vec<UserSignal> {
        let mut guard = self
            .session_turn_inputs
            .lock()
            .expect("session turn input mutex poisoned");
        let Some(active) = guard.get_mut(session_id) else {
            return Vec::new();
        };
        if active.turn_id != turn_id {
            return Vec::new();
        }
        active.pending.drain(..).collect()
    }

    /// 模型准备结束当前 Turn 时的唯一边界操作：若已有引导则原子取出并继续；
    /// 若没有引导则在同一把锁内关闭通道，使迟到引导明确失败而不是串入下一轮。
    pub fn take_session_turn_steers_or_close(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> SessionTurnInputBoundary {
        let mut guard = self
            .session_turn_inputs
            .lock()
            .expect("session turn input mutex poisoned");
        let Some(active) = guard.get_mut(session_id) else {
            return SessionTurnInputBoundary::Closed;
        };
        if active.turn_id != turn_id {
            return SessionTurnInputBoundary::Closed;
        }
        if active.pending.is_empty() {
            guard.remove(session_id);
            SessionTurnInputBoundary::Closed
        } else {
            SessionTurnInputBoundary::Pending(active.pending.drain(..).collect())
        }
    }

    pub fn close_session_turn_input(&self, session_id: &SessionId, turn_id: &str) -> bool {
        let mut guard = self
            .session_turn_inputs
            .lock()
            .expect("session turn input mutex poisoned");
        if guard
            .get(session_id)
            .is_some_and(|active| active.turn_id == turn_id)
        {
            guard.remove(session_id);
            true
        } else {
            false
        }
    }

    /// 删除 session 主对话及其全部 task 对话。会话删除后这些内存态不能继续存活，
    /// 否则 Mailbox 与 Turn 状态会成为无法再访问的孤儿。
    pub fn remove_session(&self, session_id: &SessionId) -> usize {
        let mut guard = self
            .inner
            .lock()
            .expect("ConversationRegistry mutex poisoned");
        let before = guard.len();
        guard.retain(|key, _| match key {
            ConversationKey::Session(candidate) => candidate != session_id,
            ConversationKey::Task {
                session_id: candidate,
                ..
            } => candidate != session_id,
        });
        let removed = before.saturating_sub(guard.len());
        self.session_turn_inputs
            .lock()
            .expect("session turn input mutex poisoned")
            .remove(session_id);
        removed
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

    #[test]
    fn remove_session_drops_session_and_task_conversations() {
        let registry = ConversationRegistry::new();
        let session_a = SessionId::new("session-a");
        let session_b = SessionId::new("session-b");
        registry.conversation_for(&session_a);
        registry.conversation_for_task(&session_a, &TaskId::new("task-a"));
        registry.conversation_for(&session_b);

        assert_eq!(registry.remove_session(&session_a), 2);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.remove_session(&session_a), 0);
    }

    #[test]
    fn session_turn_steer_requires_matching_active_turn_and_drains_fifo() {
        let registry = ConversationRegistry::new();
        let session_id = SessionId::new("session-steer");
        let _active =
            registry.begin_session_turn_input(session_id.clone(), "turn-steer".to_string());

        registry
            .try_steer_session_turn(
                &session_id,
                "turn-steer",
                UserSignal {
                    text: Some("first".to_string()),
                    request_id: Some("request-first".to_string()),
                    user_message_id: Some("user-first".to_string()),
                    placeholder_message_id: None,
                    accepted_at: UtcMillis(1),
                },
            )
            .expect("matching active turn should accept steer");
        registry
            .try_steer_session_turn(
                &session_id,
                "turn-steer",
                UserSignal {
                    text: Some("second".to_string()),
                    request_id: Some("request-second".to_string()),
                    user_message_id: Some("user-second".to_string()),
                    placeholder_message_id: None,
                    accepted_at: UtcMillis(2),
                },
            )
            .expect("second steer should remain FIFO");

        let drained = registry.drain_session_turn_steers(&session_id, "turn-steer");
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].text.as_deref(), Some("first"));
        assert_eq!(drained[1].text.as_deref(), Some("second"));
        assert!(
            registry
                .try_steer_session_turn(
                    &session_id,
                    "turn-other",
                    UserSignal {
                        text: Some("stale".to_string()),
                        request_id: None,
                        user_message_id: None,
                        placeholder_message_id: None,
                        accepted_at: UtcMillis(3),
                    },
                )
                .is_err(),
            "stale expected turn id must be rejected",
        );
    }

    #[test]
    fn session_turn_completion_boundary_closes_atomically_when_no_steer_is_pending() {
        let registry = ConversationRegistry::new();
        let session_id = SessionId::new("session-steer-close");
        registry
            .begin_session_turn_input(session_id.clone(), "turn-steer-close".to_string())
            .expect("active input turn should begin");

        assert_eq!(
            registry.take_session_turn_steers_or_close(&session_id, "turn-steer-close"),
            SessionTurnInputBoundary::Closed
        );
        assert_eq!(
            registry.try_steer_session_turn(
                &session_id,
                "turn-steer-close",
                UserSignal {
                    text: Some("too late".to_string()),
                    request_id: None,
                    user_message_id: None,
                    placeholder_message_id: None,
                    accepted_at: UtcMillis(4),
                },
            ),
            Err(SessionTurnInputError::NoActiveTurn),
            "completion boundary must reject late steer instead of leaking it into the next turn"
        );
    }
}
