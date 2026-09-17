use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use magi_core::{SessionId, TaskId};

use crate::conversation::Conversation;
use crate::mailbox::RuntimeSignal;
use crate::session_turn_coordinator::SessionTurnCoordinator;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum ConversationKey {
    Task {
        session_id: SessionId,
        task_id: TaskId,
    },
}

/// 按任务身份持有 Conversation 实例。Session 级 Turn 不再创建第二个
/// Conversation 生命周期；只有 Task profile 的 worker 需要独立 mailbox 和
/// TurnDriver 状态。
#[derive(Debug, Default)]
pub struct ConversationRegistry {
    inner: Mutex<HashMap<ConversationKey, Arc<Mutex<Conversation>>>>,
    /// Session Turn 的 steer 队列和工具授权由 Coordinator 持有；Registry 只管理
    /// task/worker Conversation 与运行时信号通道。
    turn_coordinator: Arc<SessionTurnCoordinator>,
    task_signal_channels: Mutex<HashMap<(SessionId, TaskId), VecDeque<RuntimeSignal>>>,
    task_signal_ready: Condvar,
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

impl ConversationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_turn_coordinator(turn_coordinator: Arc<SessionTurnCoordinator>) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            turn_coordinator,
            task_signal_channels: Mutex::new(HashMap::new()),
            task_signal_ready: Condvar::new(),
        }
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

    pub fn tool_approvals(&self) -> &crate::ToolApprovalRegistry {
        self.turn_coordinator.tool_approvals()
    }

    pub fn turn_coordinator(&self) -> &SessionTurnCoordinator {
        self.turn_coordinator.as_ref()
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
            drop(channels);
            self.tool_approvals().remove_task(session_id, task_id);
            TaskSignalBoundary::Closed
        } else {
            TaskSignalBoundary::Pending(channel.drain(..).collect())
        }
    }

    pub fn close_task_signal_channel(&self, session_id: &SessionId, task_id: &TaskId) -> bool {
        let removed = self
            .task_signal_channels
            .lock()
            .expect("task signal channel mutex poisoned")
            .remove(&(session_id.clone(), task_id.clone()))
            .is_some();
        if removed {
            self.tool_approvals().remove_task(session_id, task_id);
        }
        removed
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
            ConversationKey::Task {
                session_id: candidate,
                ..
            } => candidate != session_id,
        });
        let removed = before.saturating_sub(guard.len());
        drop(guard);
        self.task_signal_channels
            .lock()
            .expect("task signal channel mutex poisoned")
            .retain(|(candidate, _), _| candidate != session_id);
        self.turn_coordinator.clear_session(session_id);
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mailbox::UserSignal;
    use crate::{SessionTurnInputBoundary, SessionTurnInputError};
    use magi_core::UtcMillis;

    #[test]
    fn task_conversations_are_isolated_by_task_identity() {
        let registry = ConversationRegistry::new();
        let session_id = SessionId::new("session-a");
        let task_a = registry.conversation_for_task(&session_id, &TaskId::new("task-a"));
        let task_b = registry.conversation_for_task(&session_id, &TaskId::new("task-b"));

        assert!(!Arc::ptr_eq(&task_a, &task_b));
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn remove_session_drops_task_conversations() {
        let registry = ConversationRegistry::new();
        let session_a = SessionId::new("session-a");
        let session_b = SessionId::new("session-b");
        registry.conversation_for_task(&session_a, &TaskId::new("task-a"));
        registry.conversation_for_task(&session_b, &TaskId::new("task-b"));

        assert_eq!(registry.remove_session(&session_a), 1);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.remove_session(&session_a), 0);
    }

    #[test]
    fn session_turn_steer_requires_matching_active_turn_and_drains_fifo() {
        let registry = ConversationRegistry::new();
        let session_id = SessionId::new("session-steer");
        let _active = registry
            .turn_coordinator()
            .begin_session_turn_input(session_id.clone(), "turn-steer".to_string());

        registry
            .turn_coordinator()
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
            .turn_coordinator()
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

        let drained = registry
            .turn_coordinator()
            .drain_session_turn_steers(&session_id, "turn-steer");
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].text.as_deref(), Some("first"));
        assert_eq!(drained[1].text.as_deref(), Some("second"));
        assert!(
            registry
                .turn_coordinator()
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
            .turn_coordinator()
            .begin_session_turn_input(session_id.clone(), "turn-steer-close".to_string())
            .expect("active input turn should begin");

        assert_eq!(
            registry
                .turn_coordinator()
                .take_session_turn_steers_or_close(&session_id, "turn-steer-close"),
            SessionTurnInputBoundary::Closed
        );
        assert_eq!(
            registry.turn_coordinator().try_steer_session_turn(
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
