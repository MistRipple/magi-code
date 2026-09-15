//! Task 终态完成通知。
//!
//! TaskStore 的终态提交完成后，通知器把轻量的 Task/Turn 关联投递给上层。
//! 它不在 TaskStore mutation guard 内执行磁盘或 SessionStore 工作；调用方可以在
//! 通知回调中交给 SessionTurnCoordinator 收口 Turn。

use crate::task_runner::apply_task_result;
use crate::task_runner_bridge::{TaskCompletionSink, TaskResult};
use magi_core::{SessionId, TaskId, TaskStatus};
use magi_orchestrator::task_store::TaskStore;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskCompletionNotification {
    pub session_id: Option<SessionId>,
    pub turn_id: Option<String>,
    pub root_task_id: TaskId,
    pub task_id: TaskId,
    pub attempt_id: String,
    pub status: TaskStatus,
}

#[derive(Clone, Debug)]
struct TaskCompletionContext {
    session_id: Option<SessionId>,
    turn_id: Option<String>,
    /// Coordinator 的 Turn attempt。它是 Turn 侧的终态关联身份。
    coordinator_attempt_id: Option<String>,
    /// Worker lease。它只用于把显式 Worker 结果与 TaskStore 终态关联起来。
    lease_id: Option<String>,
}

type Observer = Arc<dyn Fn(TaskCompletionNotification) + Send + Sync>;

fn merge_completion_context(
    stored: Option<TaskCompletionContext>,
    explicit: Option<TaskCompletionContext>,
) -> Option<TaskCompletionContext> {
    match (stored, explicit) {
        (None, None) => None,
        (Some(context), None) | (None, Some(context)) => Some(context),
        (Some(stored), Some(explicit)) => Some(TaskCompletionContext {
            session_id: explicit.session_id.or(stored.session_id),
            turn_id: explicit.turn_id.or(stored.turn_id),
            coordinator_attempt_id: explicit
                .coordinator_attempt_id
                .or(stored.coordinator_attempt_id),
            lease_id: explicit.lease_id.or(stored.lease_id),
        }),
    }
}

/// 将 Worker 结果转换为 TaskStore durable 终态，并在提交之后投递轻量通知。
#[derive(Clone)]
pub struct TaskCompletionNotifier {
    store: Arc<TaskStore>,
    contexts: Arc<Mutex<HashMap<(TaskId, String), TaskCompletionContext>>>,
    /// Latest execution context by task.  This is used for terminal paths
    /// which do not originate from a worker result (kill, lease expiry,
    /// unrunnable tasks and runner panic).
    task_contexts: Arc<Mutex<HashMap<TaskId, TaskCompletionContext>>>,
    /// A TaskStore transition and an explicit worker result can describe the
    /// same durable terminal fact. Suppress duplicate notifications with the
    /// committed timestamp plus the execution identities. The timestamp is
    /// the durable transition generation; the identities distinguish a retry
    /// that happens within the same millisecond.
    published_terminals: Arc<Mutex<HashSet<(TaskId, String, u64, String, String)>>>,
    observer: Arc<Mutex<Option<Observer>>>,
}

impl TaskCompletionNotifier {
    pub fn new(store: Arc<TaskStore>) -> Self {
        Self {
            store,
            contexts: Arc::new(Mutex::new(HashMap::new())),
            task_contexts: Arc::new(Mutex::new(HashMap::new())),
            published_terminals: Arc::new(Mutex::new(HashSet::new())),
            observer: Arc::new(Mutex::new(None)),
        }
    }

    /// 绑定 Worker lease 与 Turn。显式 Worker 结果通过 lease 查找上下文，
    /// 通知给 Turn 侧时仍优先携带 Coordinator attempt。
    pub fn bind_context(
        &self,
        task_id: &TaskId,
        lease_id: &str,
        session_id: Option<SessionId>,
        turn_id: Option<String>,
    ) {
        self.bind_lease_context(task_id, lease_id, session_id, turn_id);
    }

    pub fn bind_lease_context(
        &self,
        task_id: &TaskId,
        lease_id: &str,
        session_id: Option<SessionId>,
        turn_id: Option<String>,
    ) {
        let prior = self
            .task_contexts
            .lock()
            .expect("task completion task contexts lock poisoned")
            .get(task_id)
            .cloned();
        let context = TaskCompletionContext {
            session_id: session_id
                .or_else(|| prior.as_ref().and_then(|value| value.session_id.clone())),
            turn_id: turn_id.or_else(|| prior.as_ref().and_then(|value| value.turn_id.clone())),
            coordinator_attempt_id: prior
                .as_ref()
                .and_then(|value| value.coordinator_attempt_id.clone()),
            lease_id: Some(lease_id.to_string()),
        };
        self.contexts
            .lock()
            .expect("task completion contexts lock poisoned")
            .insert((task_id.clone(), lease_id.to_string()), context.clone());
        self.task_contexts
            .lock()
            .expect("task completion task contexts lock poisoned")
            .insert(task_id.clone(), context);
    }

    /// 绑定 Coordinator attempt。Task admission 和 Continue 在 Worker lease
    /// 尚未创建时调用，确保 kill、lease expiry 和 Runner panic 也能定位 Turn。
    pub fn bind_coordinator_attempt(
        &self,
        task_id: &TaskId,
        attempt_id: &str,
        session_id: Option<SessionId>,
        turn_id: Option<String>,
    ) {
        let prior = self
            .task_contexts
            .lock()
            .expect("task completion task contexts lock poisoned")
            .get(task_id)
            .cloned();
        let context = TaskCompletionContext {
            session_id: session_id
                .or_else(|| prior.as_ref().and_then(|value| value.session_id.clone())),
            turn_id: turn_id.or_else(|| prior.as_ref().and_then(|value| value.turn_id.clone())),
            coordinator_attempt_id: Some(attempt_id.to_string()),
            lease_id: prior.as_ref().and_then(|value| value.lease_id.clone()),
        };
        self.task_contexts
            .lock()
            .expect("task completion task contexts lock poisoned")
            .insert(task_id.clone(), context);
    }

    /// 为没有显式 Worker 结果的终态补齐会话/Turn 关联，同时保留已经绑定的
    /// Coordinator attempt 和 lease。调用者应在 notify_terminal_status 前调用。
    pub fn bind_task_context(
        &self,
        task_id: &TaskId,
        session_id: Option<SessionId>,
        turn_id: Option<String>,
    ) {
        let prior = self
            .task_contexts
            .lock()
            .expect("task completion task contexts lock poisoned")
            .get(task_id)
            .cloned();
        let context = TaskCompletionContext {
            session_id: session_id
                .or_else(|| prior.as_ref().and_then(|value| value.session_id.clone())),
            turn_id: turn_id.or_else(|| prior.as_ref().and_then(|value| value.turn_id.clone())),
            coordinator_attempt_id: prior
                .as_ref()
                .and_then(|value| value.coordinator_attempt_id.clone()),
            lease_id: prior.as_ref().and_then(|value| value.lease_id.clone()),
        };
        self.task_contexts
            .lock()
            .expect("task completion task contexts lock poisoned")
            .insert(task_id.clone(), context);
    }

    pub fn set_observer(
        &self,
        observer: impl Fn(TaskCompletionNotification) + Send + Sync + 'static,
    ) {
        *self
            .observer
            .lock()
            .expect("task completion observer lock poisoned") = Some(Arc::new(observer));
    }

    fn publish_terminal(
        &self,
        task_id: TaskId,
        task: &magi_core::Task,
        status: TaskStatus,
        attempt_id: String,
        explicit_context: Option<TaskCompletionContext>,
    ) {
        if !matches!(
            status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
        ) {
            return;
        }
        let stored_context = {
            let mut contexts = self
                .task_contexts
                .lock()
                .expect("task completion task contexts lock poisoned");
            if explicit_context.is_none() {
                contexts.remove(&task_id)
            } else {
                contexts.get(&task_id).cloned()
            }
        };
        let context = merge_completion_context(stored_context, explicit_context);
        let coordinator_attempt_id = context
            .as_ref()
            .and_then(|context| context.coordinator_attempt_id.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_default();
        let lease_id = context
            .as_ref()
            .and_then(|context| context.lease_id.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| attempt_id.clone());
        let key = (
            task_id.clone(),
            format!("{status:?}"),
            task.updated_at.0,
            coordinator_attempt_id,
            lease_id,
        );
        if !self
            .published_terminals
            .lock()
            .expect("task completion published terminals lock poisoned")
            .insert(key)
        {
            return;
        }
        let notification = TaskCompletionNotification {
            session_id: context
                .as_ref()
                .and_then(|context| context.session_id.clone()),
            turn_id: context.as_ref().and_then(|context| context.turn_id.clone()),
            root_task_id: task.root_task_id.clone(),
            task_id: task_id.clone(),
            attempt_id: context
                .as_ref()
                .and_then(|context| context.coordinator_attempt_id.clone())
                .filter(|value| !value.trim().is_empty())
                .or_else(|| {
                    context
                        .as_ref()
                        .and_then(|context| context.lease_id.clone())
                })
                .unwrap_or(attempt_id),
            status,
        };
        let observer = self
            .observer
            .lock()
            .expect("task completion observer lock poisoned")
            .clone();
        if let Some(observer) = observer {
            observer(notification);
        }
    }

    /// Observe a TaskStore terminal status that was already durably committed.
    ///
    /// This path covers terminal transitions which have no worker result to
    /// feed into [`notify`], including user/system kill, lease expiry,
    /// unrunnable tasks and a Runner panic.  It intentionally performs no
    /// TaskStore mutation; the caller invokes it from the post-commit status
    /// notification path.
    pub fn notify_terminal_status(
        &self,
        task_id: &TaskId,
        status: TaskStatus,
        task: &magi_core::Task,
    ) {
        if !matches!(
            status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
        ) {
            return;
        }
        self.publish_terminal(
            task_id.clone(),
            task,
            status,
            format!("task-terminal-{}-{}", task_id, task.updated_at.0),
            None,
        );
    }

    /// 接收 Worker 结果。TaskStore durable 提交成功后才调用 observer。
    /// 迟到 lease 或不存在任务只会被丢弃，不会产生 Turn 通知。
    pub fn notify(&self, result: TaskResult) {
        let task_id = result.task_id.clone();
        let lease_id = result.lease_id.to_string();
        let context = self
            .contexts
            .lock()
            .expect("task completion contexts lock poisoned")
            .remove(&(task_id.clone(), lease_id.clone()));
        let changed = match apply_task_result(self.store.as_ref(), result) {
            Ok(changed) => changed,
            Err(error) => {
                // 完成合同不通过时 apply_task_result 会原子写入 Failed；即使随后
                // 返回诊断错误，也必须把这次已经 durable 的终态通知 Turn 侧。
                let terminal = self.store.get_task(&task_id).is_some_and(|task| {
                    matches!(
                        task.status,
                        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
                    )
                });
                tracing::error!(%task_id, %lease_id, %error, "Task completion durable 提交失败");
                terminal
            }
        };
        if !changed {
            return;
        }
        let Some(task) = self.store.get_task(&task_id) else {
            return;
        };
        if !matches!(
            task.status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
        ) {
            return;
        }
        self.publish_terminal(task_id, &task, task.status, lease_id, context);
    }
}

impl TaskCompletionSink for TaskCompletionNotifier {
    fn notify(&self, result: TaskResult) {
        TaskCompletionNotifier::notify(self, result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{MissionId, Task, TaskKind, TaskRuntimePayload, UtcMillis, WorkerId};
    use magi_orchestrator::task_store::TaskStore;
    use std::sync::mpsc;

    fn pending_task(task_id: &str) -> Task {
        let now = UtcMillis(1);
        let id = TaskId::new(task_id);
        Task {
            task_id: id.clone(),
            mission_id: MissionId::new("mission-notifier"),
            root_task_id: id,
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: "notifier test".to_string(),
            goal: "verify completion notification".to_string(),
            status: TaskStatus::Pending,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            completion_contract: Default::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn durable_terminal_notifies_after_task_store_commit() {
        let store = Arc::new(TaskStore::new());
        let task_id = TaskId::new("task-notifier");
        store.insert_task(pending_task(task_id.as_str())).unwrap();
        let lease = store
            .grant_lease_and_start_task(
                &task_id,
                &task_id,
                &WorkerId::new("worker-notifier"),
                "executor",
                10_000,
            )
            .unwrap()
            .expect("lease should be granted");
        let notifier = TaskCompletionNotifier::new(Arc::clone(&store));
        notifier.bind_context(
            &task_id,
            lease.lease_id.as_str(),
            Some(SessionId::new("session-notifier")),
            Some("turn-notifier".to_string()),
        );
        let (tx, rx) = mpsc::channel();
        notifier.set_observer(move |notification| {
            tx.send(notification).unwrap();
        });
        notifier.notify(TaskResult {
            task_id: task_id.clone(),
            lease_id: lease.lease_id.clone(),
            outcome: crate::task_runner_bridge::TaskOutcome::Failed {
                error: "expected failure".to_string(),
            },
        });
        let notification = rx
            .recv()
            .expect("observer should receive terminal notification");
        assert_eq!(
            notification.session_id,
            Some(SessionId::new("session-notifier"))
        );
        assert_eq!(notification.turn_id.as_deref(), Some("turn-notifier"));
        assert_eq!(notification.root_task_id, task_id);
        assert_eq!(notification.status, TaskStatus::Failed);
        assert_eq!(
            store.get_task(&notification.task_id).unwrap().status,
            TaskStatus::Failed
        );
    }
}
