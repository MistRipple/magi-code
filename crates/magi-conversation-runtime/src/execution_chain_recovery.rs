//! Task System v2 — M10：执行链"继续会话"恢复路径的"上半"。
//!
//! 本文件承担数据载体（[`SessionContinueAccepted`]）+ 纯/弱状态判定（基于
//! `SessionStore` / `TaskStore` / `WorkerRuntime` 直接传参）。
//! 不再依赖 magi-api 的 `ApiState`，错误类型改为 `String` —— 由 magi-api
//! 调用点做 `.map_err(...)` 桥接到 `ApiError`。
//!
//! "下半"（`rebuild_dispatch_plan_for_branch` 及之后的 `continue_execution_chain`
//! 路径，强依赖 magi-api 的 `TaskExecutionPlan` / `RunnerStartError`）暂留在
//! magi-api，M11 再下沉到 `Conversation::resume_for_continue`。

use magi_core::{SessionId, TaskStatus, TerminationReason};
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::{ActiveExecutionBranch, ActiveExecutionChain, SessionStore};
use magi_worker_runtime::{WorkerRuntime, WorkerStage};

#[derive(Clone, Debug)]
pub struct SessionContinueAccepted {
    pub session_id: SessionId,
    pub mission_id: magi_core::MissionId,
    pub root_task_id: magi_core::TaskId,
    pub action_task_id: magi_core::TaskId,
    pub execution_chain_ref: String,
    pub resumed_branch_count: usize,
    pub runner_started: bool,
}

pub fn task_status_is_terminal(status: &TaskStatus) -> bool {
    matches!(status, TaskStatus::Completed | TaskStatus::Cancelled)
}

fn task_status_is_continue_recoverable(status: &TaskStatus) -> bool {
    matches!(status, TaskStatus::Blocked)
}

fn task_status_needs_terminal_branch_finalization(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Blocked
            | TaskStatus::Ready
            | TaskStatus::Running
            | TaskStatus::Verifying
            | TaskStatus::Repairing
    )
}

fn branch_stage_is_terminal(stage: &str) -> bool {
    matches!(
        stage.trim().to_ascii_lowercase().as_str(),
        "finish" | "finished"
    )
}

fn branch_runtime_snapshot_is_terminal(
    worker_runtime: Option<&WorkerRuntime>,
    branch: &ActiveExecutionBranch,
) -> bool {
    worker_runtime
        .and_then(|runtime| runtime.branch_snapshot_for_task(&branch.task_id))
        .is_some_and(|snapshot| {
            snapshot.worker_id == branch.worker_id && matches!(snapshot.stage, WorkerStage::Finish)
        })
}

fn branch_is_terminal_for_recovery(
    worker_runtime: Option<&WorkerRuntime>,
    branch: &ActiveExecutionBranch,
) -> bool {
    branch_stage_is_terminal(&branch.stage)
        || branch_runtime_snapshot_is_terminal(worker_runtime, branch)
}

pub fn active_execution_branch_is_continue_recoverable(
    worker_runtime: Option<&WorkerRuntime>,
    task_store: Option<&TaskStore>,
    chain: &ActiveExecutionChain,
    branch: &ActiveExecutionBranch,
) -> bool {
    if branch_is_terminal_for_recovery(worker_runtime, branch) {
        return false;
    }
    let Some(task_store) = task_store else {
        return false;
    };
    let Some(task) = task_store.get_task(&branch.task_id) else {
        return false;
    };
    task.mission_id == chain.mission_id
        && task.root_task_id == chain.root_task_id
        && task_status_is_continue_recoverable(&task.status)
}

fn terminal_status_for_branch(
    worker_runtime: Option<&WorkerRuntime>,
    branch: &ActiveExecutionBranch,
) -> Option<TaskStatus> {
    let runtime = worker_runtime?;
    let reports = runtime.reports();
    reports
        .iter()
        .rev()
        .find(|report| {
            report.worker_id == branch.worker_id
                && report.task_id == branch.task_id
                && report.stage == WorkerStage::Finish
        })
        .map(|report| match report.termination_reason {
            Some(TerminationReason::Failed) => TaskStatus::Failed,
            Some(TerminationReason::Cancelled) => TaskStatus::Cancelled,
            Some(TerminationReason::Blocked) => TaskStatus::Blocked,
            Some(TerminationReason::Completed) | None => TaskStatus::Completed,
        })
        .or_else(|| {
            branch_runtime_snapshot_is_terminal(worker_runtime, branch)
                .then_some(TaskStatus::Completed)
        })
}

pub fn runtime_terminal_evidence_ref(branch: &ActiveExecutionBranch) -> String {
    format!(
        "evidence://worker-runtime/{}/finish?worker={}",
        branch.task_id, branch.worker_id
    )
}

/// 收敛 chain 中所有"已经在 worker runtime 里跑到 Finish 但 task_store 还停留在
/// 非终态"的 branch：把它们落盘成 `TaskStatus::Completed/Failed/Cancelled`。
/// 用于 `tasks_interaction::interrupt_task` 与 `continue_execution_chain` 的入口护栏。
///
/// `Result::Err(String)`：上层 magi-api 用
/// `.map_err(|msg| ApiError::internal_assembly("收敛 worker 终态失败", msg))` 桥回 ApiError。
pub fn finalize_terminal_worker_branches(
    session_store: &SessionStore,
    task_store: Option<&TaskStore>,
    worker_runtime: Option<&WorkerRuntime>,
    session_id: &SessionId,
) -> Result<usize, String> {
    let Some(chain) = session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.active_execution_chain)
    else {
        return Ok(0);
    };
    let task_store = task_store.ok_or_else(|| "task_store 未配置".to_string())?;
    let mut finalized_count = 0usize;
    for branch in chain
        .branches
        .iter()
        .filter(|branch| branch_is_terminal_for_recovery(worker_runtime, branch))
    {
        let Some(task) = task_store.get_task(&branch.task_id) else {
            continue;
        };
        if !task_status_needs_terminal_branch_finalization(&task.status) {
            continue;
        }
        let terminal_status =
            terminal_status_for_branch(worker_runtime, branch).unwrap_or(TaskStatus::Completed);
        if matches!(terminal_status, TaskStatus::Blocked) {
            continue;
        }
        if matches!(terminal_status, TaskStatus::Completed) && task.evidence_refs.is_empty() {
            task_store
                .set_evidence_refs(&branch.task_id, vec![runtime_terminal_evidence_ref(branch)]);
        }
        task_store
            .update_status(&branch.task_id, terminal_status)
            .map_err(|error| error.to_string())?;
        finalized_count += 1;
    }
    Ok(finalized_count)
}
