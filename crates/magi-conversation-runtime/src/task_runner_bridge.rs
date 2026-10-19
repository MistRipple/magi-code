//! 任务系统 — TaskRunner 执行桥层。
//!
//! WorkerInfo 仍由 magi_orchestrator::task_worker_catalog 提供；本模块持有调度 trait、
//! 派发与完成通知合同。Task 终态只由 TaskCompletionNotifier 提交。

use crate::execution_admission::ExecutionAdmissionPermit;
use magi_core::{LeaseId, Task, TaskCompletionAttempt, TaskId};
use magi_orchestrator::task_store::TaskLease;
use magi_orchestrator::task_worker_catalog::WorkerInfo;
use std::{future::Future, pin::Pin};

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
    /// 工作流明确请求用户输入；Runner 退出等待，由恢复入口重新启动。
    UserInputRequired,
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

    /// Session 关闭或显式停止整树时等待原审计树的所有执行，包含已转交分支。
    /// 普通拥有方退出使用 wait_for_quiesce，不应误等其他拥有方。
    fn wait_for_audit_tree_quiesce<'a>(
        &'a self,
        root_task_id: &'a TaskId,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        self.wait_for_quiesce(root_task_id)
    }
}
/// 一次任务执行的结果，由完成通知器提交到 TaskStore。
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
    /// 当前动作已持久化但任务回到 Pending，等待外部输入后由同一执行链继续。
    Yielded,
    /// 来源 Turn 已接受取消；仍按原租约结算为 Killed，不能与普通失败竞争。
    Cancelled,
    /// Execution failed with the given error description.
    Failed { error: String },
}

/// Task 完成的主动通知目标。生产运行时把 Worker 结果直接交给该目标，
/// 由目标先完成 TaskStore durable mutation，再唤醒 Turn Coordinator。
pub trait TaskCompletionSink: Send + Sync {
    fn notify(&self, result: TaskResult);
}

impl<F> TaskCompletionSink for F
where
    F: Fn(TaskResult) + Send + Sync,
{
    fn notify(&self, result: TaskResult) {
        self(result);
    }
}
