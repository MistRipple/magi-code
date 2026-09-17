//! 任务系统 — TaskRunner 执行桥层。
//!
//! WorkerInfo 仍由 magi_orchestrator::task_worker_catalog 提供；本模块持有调度 trait、
//! 结果接收器、event-based dispatcher 与 WorkerRuntime dispatcher。

use crate::execution_admission::ExecutionAdmissionPermit;
use magi_core::{LeaseId, Task, TaskCompletionAttempt, TaskId};
use magi_orchestrator::task_store::TaskLease;
use magi_orchestrator::task_worker_catalog::WorkerInfo;
use std::sync::{Arc, Mutex};
use std::{
    collections::{HashSet, VecDeque},
    future::Future,
    pin::Pin,
};

/// The outcome of a single `run_cycle` iteration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunCycleOutcome {
    /// There are still tasks to process; the runner should continue.
    Continue,
    /// Every task in the graph has reached a terminal state.
    AllComplete,
    /// Dispatch is intentionally paused by a non-terminal runtime gate.
    Blocked {
        task_ids: Vec<TaskId>,
        reason: String,
    },
    /// 其他 Runner 抢先取得租约，本轮未派发任务，等待下一轮重试。
    Waiting,
    /// 当前任务图仍有非终态任务，但这些任务在现有结构下无法进入可运行状态。
    Unrunnable(Vec<TaskId>),
    /// An unexpected error occurred during the cycle.
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskDispatchGateDecision {
    Allow,
    Blocked(String),
}

pub type TaskDispatchGate = dyn Fn(&Task) -> Result<TaskDispatchGateDecision, String> + Send + Sync;
// --- Dispatch callback trait

/// Trait for dispatching a matched task to a worker for execution.
///
/// Implementations receive the task, worker info, and the granted lease, and
/// are responsible for triggering the actual execution pipeline.  The Runner
/// calls `dispatch` after granting a lease and marking the task as Running.
pub trait TaskDispatcher: Send + Sync {
    fn dispatch(
        &self,
        task: &Task,
        worker: &WorkerInfo,
        lease: &TaskLease,
        admission_permit: ExecutionAdmissionPermit,
    ) -> Result<(), String>;

    /// 等待指定 root 的异步派发工作全部退出。
    /// Runner 只能直接等待自己的循环任务；异步 dispatcher 若另起
    /// `spawn_blocking`，必须把该任务纳入同一 quiesce 边界。
    fn wait_for_quiesce<'a>(
        &'a self,
        _root_task_id: &'a TaskId,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(std::future::ready(()))
    }
}
// --- Result receiver trait

/// The outcome of a single task execution, reported back to the Runner.
#[derive(Clone, Debug)]
pub struct TaskResult {
    pub task_id: TaskId,
    pub lease_id: LeaseId,
    pub outcome: TaskOutcome,
}

/// Possible outcomes when a dispatched task finishes.
#[derive(Clone, Debug)]
pub enum TaskOutcome {
    /// 执行器提交完成尝试；只有统一完成门验证合同后才会进入 Completed。
    Completed { attempt: TaskCompletionAttempt },
    /// Execution failed with the given error description.
    Failed { error: String },
}

/// 接收 Worker 执行结果。
///
/// `poll_results` 仅供没有主动完成通知目标的嵌入式 Runner 使用。生产装配会安装
/// completion sink，结果立即交付，不再等待下一轮调度循环。
pub trait TaskResultReceiver: Send + Sync {
    /// 生产运行时若返回 true，Worker 结果已经由主动完成通知提交，Runner
    /// 不应再通过周期轮询消费同一结果。
    fn uses_active_completion_sink(&self) -> bool {
        false
    }

    /// 仅保留给未装配主动通知目标的嵌入测试/兼容读取器。
    fn poll_results(&self) -> Vec<TaskResult>;
}
// --- Event-based result receiver

/// Task 完成的主动通知目标。生产运行时把 Worker 结果直接交给该目标，
/// 由目标先完成 TaskStore durable mutation，再唤醒 Turn Coordinator；测试和
/// 未装配通知目标的嵌入场景仍可使用 poll_results。
pub trait TaskCompletionSink: Send + Sync {
    fn notify(&self, result: TaskResult);
}

#[derive(Default)]
struct CompletionReceiverState {
    pending: VecDeque<TaskResult>,
    sink: Option<Arc<dyn TaskCompletionSink>>,
    seen: HashSet<(TaskId, LeaseId)>,
    notifying: bool,
}

/// 接收外部推送结果（例如来自 TaskStore 的 `StatusChangeCallback`）。
///
/// 没有主动 completion sink 时，结果会进入嵌入式 Runner 的兼容轮询队列；安装 sink
/// 后，已经缓冲的结果会在返回前交给 sink，切换装配模式不会遗留终态结果。
///
/// 结果按 task ID 和 lease ID 去重。恢复后的任务可能在新租约建立后收到旧执行结果；
/// 两类结果保持可区分，以便 Runner 只拒绝旧租约。任务回到非终态时调用
/// `clear_task_result_state` 清理此前执行轮次的缓冲终态结果。
pub struct EventBasedResultReceiver {
    state: Mutex<CompletionReceiverState>,
}

impl Default for EventBasedResultReceiver {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBasedResultReceiver {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(CompletionReceiverState::default()),
        }
    }

    /// 配置主动完成通知目标。配置后新结果直接进入 durable completion path，
    /// 不再等待 Runner 的下一轮 `poll_results`；已经缓冲的结果也会先交给该目标。
    pub fn set_completion_sink(&self, sink: Arc<dyn TaskCompletionSink>) {
        let should_notify = {
            let mut state = self
                .state
                .lock()
                .expect("EventBasedResultReceiver state lock poisoned");
            state.sink = Some(sink);
            if state.pending.is_empty() || state.notifying {
                false
            } else {
                state.notifying = true;
                true
            }
        };
        if should_notify {
            self.drain_notifications();
        }
    }

    /// 接收 TaskStore 在任务进入终态时通过 `StatusChangeCallback` 推送的结果。
    ///
    /// 同一 `task_id` 和 `lease_id` 的结果只接受一次，避免形成反馈循环。
    pub fn push_result(&self, result: TaskResult) {
        let should_notify = {
            let mut state = self
                .state
                .lock()
                .expect("EventBasedResultReceiver state lock poisoned");
            if !state
                .seen
                .insert((result.task_id.clone(), result.lease_id.clone()))
            {
                return;
            }
            if state.sink.is_some() {
                state.pending.push_back(result);
                if state.notifying {
                    false
                } else {
                    state.notifying = true;
                    true
                }
            } else {
                state.pending.push_back(result);
                false
            }
        };
        if should_notify {
            self.drain_notifications();
        }
    }

    /// 逐个交付已排队的结果。通知回调在状态锁外执行，因此允许回调重入
    /// `push_result` 或 `set_completion_sink`；`notifying` 保证重入结果仍按入队顺序
    /// 由当前 drain 循环继续交付。
    fn drain_notifications(&self) {
        loop {
            let (sink, result) = {
                let mut state = self
                    .state
                    .lock()
                    .expect("EventBasedResultReceiver state lock poisoned");
                let Some(result) = state.pending.pop_front() else {
                    state.notifying = false;
                    return;
                };
                let Some(sink) = state.sink.as_ref().cloned() else {
                    state.pending.push_front(result);
                    state.notifying = false;
                    return;
                };
                (sink, result)
            };
            sink.notify(result);
        }
    }

    /// 清理指定任务尚未交付的终态结果状态。
    ///
    /// 恢复链路会把 Failed 任务重新放回非终态。如果这里只清 dedup 标记而不清
    /// 队列，Runner 下一轮会先消费旧 Failed 结果，把刚恢复的任务再次打失败。
    pub fn clear_task_result_state(&self, task_id: &TaskId) {
        let mut state = self
            .state
            .lock()
            .expect("EventBasedResultReceiver state lock poisoned");
        state
            .seen
            .retain(|(seen_task_id, _)| seen_task_id != task_id);
        state.pending.retain(|result| &result.task_id != task_id);
    }
}

impl TaskResultReceiver for EventBasedResultReceiver {
    fn uses_active_completion_sink(&self) -> bool {
        self.state
            .lock()
            .expect("EventBasedResultReceiver state lock poisoned")
            .sink
            .is_some()
    }

    fn poll_results(&self) -> Vec<TaskResult> {
        let mut state = self
            .state
            .lock()
            .expect("EventBasedResultReceiver state lock poisoned");
        if state.sink.is_some() {
            return Vec::new();
        }
        std::mem::take(&mut state.pending).into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingCompletionSink {
        results: Mutex<Vec<TaskResult>>,
    }

    impl TaskCompletionSink for RecordingCompletionSink {
        fn notify(&self, result: TaskResult) {
            self.results
                .lock()
                .expect("recording completion sink lock poisoned")
                .push(result);
        }
    }

    struct ReentrantCompletionSink {
        receiver: Mutex<Option<Arc<EventBasedResultReceiver>>>,
        lease_ids: Mutex<Vec<String>>,
    }

    impl ReentrantCompletionSink {
        fn new() -> Self {
            Self {
                receiver: Mutex::new(None),
                lease_ids: Mutex::new(Vec::new()),
            }
        }
    }

    impl TaskCompletionSink for ReentrantCompletionSink {
        fn notify(&self, result: TaskResult) {
            if result.lease_id == LeaseId::new("lease-buffered-before-sink") {
                let receiver = self
                    .receiver
                    .lock()
                    .expect("reentrant receiver lock poisoned")
                    .clone()
                    .expect("reentrant receiver should be installed");
                receiver.push_result(TaskResult {
                    task_id: TaskId::new("task-reentrant"),
                    lease_id: LeaseId::new("lease-reentrant"),
                    outcome: TaskOutcome::Failed {
                        error: "reentrant result".to_string(),
                    },
                });
            }
            self.lease_ids
                .lock()
                .expect("reentrant lease ids lock poisoned")
                .push(result.lease_id.to_string());
        }
    }

    #[test]
    fn installing_active_sink_flushes_buffered_results_without_polling() {
        let receiver = EventBasedResultReceiver::new();
        let task_id = TaskId::new("task-buffered-before-sink");
        receiver.push_result(TaskResult {
            task_id: task_id.clone(),
            lease_id: LeaseId::new("lease-buffered-before-sink"),
            outcome: TaskOutcome::Failed {
                error: "buffered failure".to_string(),
            },
        });

        let sink = Arc::new(RecordingCompletionSink::default());
        receiver.set_completion_sink(sink.clone());

        assert!(receiver.poll_results().is_empty());
        let results = sink
            .results
            .lock()
            .expect("recording completion sink lock poisoned");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].task_id, task_id);
        assert_eq!(
            results[0].lease_id,
            LeaseId::new("lease-buffered-before-sink")
        );
    }

    #[test]
    fn reentrant_sink_keeps_buffered_result_before_new_result() {
        let receiver = Arc::new(EventBasedResultReceiver::new());
        receiver.push_result(TaskResult {
            task_id: TaskId::new("task-buffered-before-sink"),
            lease_id: LeaseId::new("lease-buffered-before-sink"),
            outcome: TaskOutcome::Failed {
                error: "buffered failure".to_string(),
            },
        });

        let sink = Arc::new(ReentrantCompletionSink::new());
        *sink
            .receiver
            .lock()
            .expect("reentrant receiver lock poisoned") = Some(Arc::clone(&receiver));
        receiver.set_completion_sink(sink.clone());

        let lease_ids = sink
            .lease_ids
            .lock()
            .expect("reentrant lease ids lock poisoned")
            .clone();
        assert_eq!(
            lease_ids,
            vec![
                "lease-buffered-before-sink".to_string(),
                "lease-reentrant".to_string()
            ]
        );
        assert!(receiver.poll_results().is_empty());
    }

    #[test]
    fn clear_task_result_state_drops_stale_terminal_result() {
        let receiver = EventBasedResultReceiver::new();
        let task_id = TaskId::new("task-recovered");

        receiver.push_result(TaskResult {
            task_id: task_id.clone(),
            lease_id: LeaseId::new("lease-stale"),
            outcome: TaskOutcome::Failed {
                error: "stale failure".to_string(),
            },
        });
        receiver.clear_task_result_state(&task_id);
        receiver.push_result(TaskResult {
            task_id: task_id.clone(),
            lease_id: LeaseId::new("lease-current"),
            outcome: TaskOutcome::Completed {
                attempt: TaskCompletionAttempt {
                    output_refs: vec!["current result".to_string()],
                    final_response: Some("current result".to_string()),
                    evidence: Vec::new(),
                },
            },
        });

        let results = receiver.poll_results();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].lease_id, LeaseId::new("lease-current"));
        match &results[0].outcome {
            TaskOutcome::Completed { attempt } => {
                assert_eq!(attempt.output_refs, vec!["current result".to_string()]);
            }
            TaskOutcome::Failed { error } => panic!("不应消费旧失败结果: {error}"),
        }
    }

    #[test]
    fn results_from_different_leases_are_not_deduplicated() {
        let receiver = EventBasedResultReceiver::new();
        let task_id = TaskId::new("task-multi-lease-results");

        receiver.push_result(TaskResult {
            task_id: task_id.clone(),
            lease_id: LeaseId::new("lease-stale"),
            outcome: TaskOutcome::Failed {
                error: "stale failure".to_string(),
            },
        });
        receiver.push_result(TaskResult {
            task_id,
            lease_id: LeaseId::new("lease-current"),
            outcome: TaskOutcome::Completed {
                attempt: TaskCompletionAttempt {
                    output_refs: vec!["current result".to_string()],
                    final_response: Some("current result".to_string()),
                    evidence: Vec::new(),
                },
            },
        });

        let results = receiver.poll_results();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].lease_id, LeaseId::new("lease-stale"));
        assert_eq!(results[1].lease_id, LeaseId::new("lease-current"));
    }
}
