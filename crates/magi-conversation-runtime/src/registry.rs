use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use magi_core::{SessionId, TaskId};

use crate::conversation::Conversation;
use crate::mailbox::{RuntimeSignal, UserSignal};

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
    task_signal_channels: Mutex<HashMap<(SessionId, TaskId), VecDeque<RuntimeSignal>>>,
    task_signal_ready: Condvar,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskSignalBoundary {
    Pending(Vec<RuntimeSignal>),
    Closed,
}

#[derive(Debug)]
pub enum TaskSignalCommitError<E> {
    ChannelClosed,
    Commit(E),
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

    /// 注册任务级运行时信号通道。agent_spawn 可在子任务 runner 启动前调用，因此
    /// 已排队信号会保留；run_conversation_loop 再次注册不会清空队列。
    pub fn open_task_signal_channel(&self, session_id: &SessionId, task_id: &TaskId) {
        self.task_signal_channels
            .lock()
            .expect("task signal channel mutex poisoned")
            .entry((session_id.clone(), task_id.clone()))
            .or_default();
    }

    /// 向已注册且尚未关闭的任务通道投递运行时信号。该入口不获取 Conversation
    /// mutex，因此可以在目标代理正执行模型或工具轮次时立即送达。
    pub fn enqueue_task_signal(
        &self,
        session_id: &SessionId,
        task_id: &TaskId,
        signal: RuntimeSignal,
    ) -> Result<(), String> {
        self.enqueue_task_signal_with(session_id, task_id, || {
            Ok::<_, std::convert::Infallible>(((), signal))
        })
        .map_err(|error| match error {
            TaskSignalCommitError::ChannelClosed => {
                format!("任务 {task_id} 的运行时信号通道未打开或已关闭")
            }
            TaskSignalCommitError::Commit(never) => match never {},
        })
    }

    /// 在持有目标任务信号边界锁时提交上下文包 revision，再把与该 revision 对应的
    /// 信号加入 FIFO，保证持久化和运行中投递不会出现一边成功、一边失败。
    pub fn enqueue_task_signal_with<T, E, F>(
        &self,
        session_id: &SessionId,
        task_id: &TaskId,
        commit: F,
    ) -> Result<T, TaskSignalCommitError<E>>
    where
        F: FnOnce() -> Result<(T, RuntimeSignal), E>,
    {
        let mut channels = self
            .task_signal_channels
            .lock()
            .expect("task signal channel mutex poisoned");
        let channel = channels
            .get_mut(&(session_id.clone(), task_id.clone()))
            .ok_or(TaskSignalCommitError::ChannelClosed)?;
        let (committed, signal) = commit().map_err(TaskSignalCommitError::Commit)?;
        channel.push_back(signal);
        self.task_signal_ready.notify_all();
        Ok(committed)
    }

    pub fn drain_task_signals(
        &self,
        session_id: &SessionId,
        task_id: &TaskId,
    ) -> Vec<RuntimeSignal> {
        self.task_signal_channels
            .lock()
            .expect("task signal channel mutex poisoned")
            .get_mut(&(session_id.clone(), task_id.clone()))
            .map(|channel| channel.drain(..).collect())
            .unwrap_or_default()
    }

    pub fn wait_for_task_signals(
        &self,
        session_id: &SessionId,
        task_id: &TaskId,
        timeout: Duration,
    ) -> Vec<RuntimeSignal> {
        let key = (session_id.clone(), task_id.clone());
        let channels = self
            .task_signal_channels
            .lock()
            .expect("task signal channel mutex poisoned");
        if !channels.contains_key(&key) {
            return Vec::new();
        }
        let (mut channels, _) = self
            .task_signal_ready
            .wait_timeout_while(channels, timeout, |channels| {
                channels.get(&key).is_some_and(VecDeque::is_empty)
            })
            .expect("task signal channel wait poisoned");
        channels
            .get_mut(&key)
            .map(|channel| channel.drain(..).collect())
            .unwrap_or_default()
    }

    /// 模型准备结束任务 Turn 时原子读取待处理信号；没有信号时关闭通道，保证迟到
    /// agent_send/context_request 明确失败，不会写入已完成任务。
    pub fn take_task_signals_or_close(
        &self,
        session_id: &SessionId,
        task_id: &TaskId,
    ) -> TaskSignalBoundary {
        let mut channels = self
            .task_signal_channels
            .lock()
            .expect("task signal channel mutex poisoned");
        let key = (session_id.clone(), task_id.clone());
        let Some(channel) = channels.get_mut(&key) else {
            return TaskSignalBoundary::Closed;
        };
        if channel.is_empty() {
            channels.remove(&key);
            TaskSignalBoundary::Closed
        } else {
            TaskSignalBoundary::Pending(channel.drain(..).collect())
        }
    }

    pub fn close_task_signal_channel(&self, session_id: &SessionId, task_id: &TaskId) -> bool {
        self.task_signal_channels
            .lock()
            .expect("task signal channel mutex poisoned")
            .remove(&(session_id.clone(), task_id.clone()))
            .is_some()
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
        self.task_signal_channels
            .lock()
            .expect("task signal channel mutex poisoned")
            .retain(|(candidate, _), _| candidate != session_id);
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

    #[test]
    fn task_signal_channel_accepts_runtime_input_without_conversation_lock() {
        let registry = ConversationRegistry::new();
        let session_id = SessionId::new("session-task-signal");
        let task_id = TaskId::new("task-signal");
        registry.open_task_signal_channel(&session_id, &task_id);
        registry
            .enqueue_task_signal(
                &session_id,
                &task_id,
                RuntimeSignal {
                    author: crate::MailboxAuthor::Parent("task-parent".to_string()),
                    kind: crate::MailboxKind::Message,
                    trigger_turn: true,
                    payload: serde_json::json!({"message": "补充事实"}),
                    enqueued_at: UtcMillis(10),
                },
            )
            .expect("打开的任务通道应接受信号");

        let drained = registry.drain_task_signals(&session_id, &task_id);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].payload["message"], "补充事实");
        assert_eq!(
            registry.take_task_signals_or_close(&session_id, &task_id),
            TaskSignalBoundary::Closed
        );
        assert!(
            registry
                .enqueue_task_signal(
                    &session_id,
                    &task_id,
                    RuntimeSignal {
                        author: crate::MailboxAuthor::System,
                        kind: crate::MailboxKind::Message,
                        trigger_turn: true,
                        payload: serde_json::json!({}),
                        enqueued_at: UtcMillis(11),
                    },
                )
                .is_err(),
            "任务结束边界后必须拒绝迟到信号"
        );
    }
}
