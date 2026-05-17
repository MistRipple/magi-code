//! Task System v2 — 执行链"继续会话"恢复路径。
//!
//! 本文件承担数据载体（[`SessionContinueAccepted`]）+ 纯/弱状态判定 + recovery
//! 校验 / 校对 / 应用 / writeback 落盘 / branch checkpoint 同步 / 子树解封等
//! "继续会话"的 v2 实现细节。错误类型统一为 `String`，函数签名走显式 stores。

use magi_core::{
    ExecutionOwnership, RecoveryResumeInput, SessionId, TaskStatus, TerminationReason, UtcMillis,
};
use magi_memory_store::MemoryStore;
use magi_orchestrator::{ExecutionWritebackPlans, task_store::TaskStore};
use magi_session_store::{ActiveExecutionBranch, ActiveExecutionChain, SessionStore};
use magi_spawn_graph::SpawnGraph;
use magi_worker_runtime::{
    WorkerCheckpointResumeMode, WorkerExecutionBindingLifecycle, WorkerExecutionCheckpointCursor,
    WorkerRuntime, WorkerStage,
};
use magi_workspace::{RecoveryStatus, WorkspaceStore};

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
    matches!(status, TaskStatus::Completed | TaskStatus::Killed)
}

fn task_status_is_continue_recoverable(status: &TaskStatus) -> bool {
    matches!(status, TaskStatus::Failed)
}

fn task_status_needs_terminal_branch_finalization(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Failed | TaskStatus::Pending | TaskStatus::Running
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
            Some(TerminationReason::Cancelled) => TaskStatus::Killed,
            Some(TerminationReason::Blocked) => TaskStatus::Failed,
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
        if matches!(terminal_status, TaskStatus::Failed) {
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

/// 沿 SpawnGraph 上溯，把恢复 branch 与其祖先链上的可恢复 Failed 任务恢复为 Pending。
pub fn release_resumed_branch_path(
    task_store: &TaskStore,
    spawn_graph: &std::sync::Mutex<SpawnGraph>,
    chain: &ActiveExecutionChain,
    branch: &ActiveExecutionBranch,
) -> Result<(), String> {
    let mut current_task_id = Some(branch.task_id.clone());
    while let Some(task_id) = current_task_id {
        if task_id == chain.root_task_id {
            break;
        }
        let task = task_store
            .get_task(&task_id)
            .ok_or_else(|| format!("继续 branch 任务不存在: {task_id}"))?;
        if task.mission_id != chain.mission_id || task.root_task_id != chain.root_task_id {
            return Err(format!("branch 路径任务不属于当前执行链: {task_id}"));
        }
        current_task_id = {
            let graph = spawn_graph
                .lock()
                .map_err(|error| format!("SpawnGraph 锁中毒: {error}"))?;
            graph.parent_of(&task_id).cloned()
        };
        if task.status == TaskStatus::Failed {
            task_store
                .update_status(&task_id, TaskStatus::Pending)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

pub fn parse_branch_worker_stage(value: &str) -> WorkerStage {
    match value.trim().to_ascii_lowercase().as_str() {
        "review" => WorkerStage::Review,
        "verify" => WorkerStage::Verify,
        "repair" => WorkerStage::Repair,
        "finish" | "finished" => WorkerStage::Finish,
        _ => WorkerStage::Execute,
    }
}

pub fn parse_branch_resume_mode(value: Option<&str>) -> WorkerCheckpointResumeMode {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("step-checkpoint") => WorkerCheckpointResumeMode::StepCheckpoint,
        _ => WorkerCheckpointResumeMode::StageRestart,
    }
}

pub fn parse_branch_binding_lifecycle(
    value: Option<&str>,
) -> Option<WorkerExecutionBindingLifecycle> {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("bound") => Some(WorkerExecutionBindingLifecycle::Bound),
        Some("released") => Some(WorkerExecutionBindingLifecycle::Released),
        Some("none") => Some(WorkerExecutionBindingLifecycle::None),
        Some("requested") => Some(WorkerExecutionBindingLifecycle::Requested),
        Some(_) => Some(WorkerExecutionBindingLifecycle::Requested),
        None => None,
    }
}

pub fn branch_checkpoint_cursor(
    branch: &ActiveExecutionBranch,
) -> Option<WorkerExecutionCheckpointCursor> {
    branch
        .checkpoint_stage
        .as_deref()
        .map(|checkpoint_stage| WorkerExecutionCheckpointCursor {
            checkpoint_stage: parse_branch_worker_stage(checkpoint_stage),
            next_step_index: branch.next_step_index.unwrap_or(0),
            checkpoint_at: branch.checkpoint_at.unwrap_or_else(UtcMillis::now),
            resume_mode: parse_branch_resume_mode(branch.resume_mode.as_deref()),
            resume_token: branch.resume_token.clone(),
        })
}

/// 把 active branch 上的 checkpoint 信息回灌 worker runtime —— 让 worker 进程恢复
/// 时拿到与 session sidecar 一致的 stage/cursor。
pub fn sync_branch_checkpoint_to_worker_runtime(
    worker_runtime: &WorkerRuntime,
    branch: &ActiveExecutionBranch,
) {
    worker_runtime.record_branch_checkpoint(
        &branch.task_id,
        &branch.worker_id,
        parse_branch_worker_stage(&branch.stage),
        branch.lease_id.as_ref().map(ToString::to_string),
        branch.execution_intent_ref.clone(),
        parse_branch_binding_lifecycle(branch.binding_lifecycle.as_deref()),
        branch_checkpoint_cursor(branch),
    );
}

#[derive(Debug)]
pub enum RecoveryValidationError {
    /// recovery_id 在 workspace_registry 里查不到。
    NotFound { recovery_id: String },
    /// recovery 当前状态不允许继续。
    InvalidStatus { message: String },
    /// recovery 输入构建失败或入口与当前 chain 不匹配。
    Mismatch { message: String },
    /// 其它装配性错误（writebacks 落盘 / session_store 写入失败等）。
    Internal { message: String },
}

impl RecoveryValidationError {
    pub fn into_message(self) -> String {
        match self {
            Self::NotFound { recovery_id } => format!("recovery 不存在: {recovery_id}"),
            Self::InvalidStatus { message } => message,
            Self::Mismatch { message } => message,
            Self::Internal { message } => message,
        }
    }
}

pub fn validate_recovery_status(
    workspace_registry: &WorkspaceStore,
    recovery_id: &str,
) -> Result<(), RecoveryValidationError> {
    let export = workspace_registry
        .recovery_sidecar_export(recovery_id)
        .ok_or_else(|| RecoveryValidationError::NotFound {
            recovery_id: recovery_id.to_string(),
        })?;
    match export.current_status {
        RecoveryStatus::Ready => Ok(()),
        RecoveryStatus::Prepared => Err(RecoveryValidationError::InvalidStatus {
            message: format!(
                "继续检查点 {} 当前状态为 prepared，必须先进入 ready 才能继续会话",
                recovery_id
            ),
        }),
        RecoveryStatus::Consumed => Err(RecoveryValidationError::InvalidStatus {
            message: format!("继续检查点 {} 已被消费，不能再次继续会话", recovery_id),
        }),
    }
}

pub fn map_recovery_input_error(
    recovery_id: &str,
    error: magi_core::DomainError,
) -> RecoveryValidationError {
    match error {
        magi_core::DomainError::NotFound { .. } => RecoveryValidationError::NotFound {
            recovery_id: recovery_id.to_string(),
        },
        magi_core::DomainError::InvalidState { message }
        | magi_core::DomainError::Validation { message } => {
            RecoveryValidationError::Mismatch { message }
        }
        magi_core::DomainError::AlreadyExists { entity } => RecoveryValidationError::Internal {
            message: format!("recovery 输入构建遇到重复实体: {entity}"),
        },
    }
}

pub fn validate_recovery_input_matches_chain(
    chain: &ActiveExecutionChain,
    input: &RecoveryResumeInput,
) -> Result<(), RecoveryValidationError> {
    if input.ownership.session_id.as_ref() != Some(&chain.session_id) {
        return Err(RecoveryValidationError::Mismatch {
            message: format!(
                "恢复入口 {} 不属于当前会话 {}",
                input.recovery_id, chain.session_id
            ),
        });
    }
    if input.ownership.mission_id.as_ref() != Some(&chain.mission_id) {
        return Err(RecoveryValidationError::Mismatch {
            message: format!(
                "恢复入口 {} 不属于当前执行链 mission {}",
                input.recovery_id, chain.mission_id
            ),
        });
    }
    if input.ownership.workspace_id != chain.workspace_id {
        return Err(RecoveryValidationError::Mismatch {
            message: format!("恢复入口 {} 的工作区与当前执行链不一致", input.recovery_id),
        });
    }
    if input.ownership.execution_chain_ref.as_deref() != Some(chain.execution_chain_ref.as_str()) {
        return Err(RecoveryValidationError::Mismatch {
            message: format!(
                "恢复入口 {} 的 execution_chain_ref 与当前执行链不一致",
                input.recovery_id
            ),
        });
    }
    Ok(())
}

/// 若 chain.recovery_ref 存在：校验 recovery 状态 / 入口匹配 → 应用 recovery resume
/// 输入到 session_store → 落盘 writebacks → 消费 recovery → 清理 chain.recovery_ref。
pub fn apply_chain_recovery_if_needed(
    session_store: &SessionStore,
    workspace_registry: &WorkspaceStore,
    memory_store: Option<&MemoryStore>,
    session_id: &SessionId,
    chain: &mut ActiveExecutionChain,
    primary_branch: &ActiveExecutionBranch,
) -> Result<(), RecoveryValidationError> {
    let Some(recovery_id) = chain.recovery_ref.clone() else {
        return Ok(());
    };
    validate_recovery_status(workspace_registry, &recovery_id)?;
    let input = workspace_registry
        .build_recovery_resume_input(&recovery_id)
        .map_err(|error| map_recovery_input_error(&recovery_id, error))?;
    validate_recovery_input_matches_chain(chain, &input)?;

    session_store
        .apply_recovery_resume_input(session_id.clone(), input.clone())
        .map_err(|error| RecoveryValidationError::Internal {
            message: error.to_string(),
        })?;

    let writebacks = ExecutionWritebackPlans::from_continue_checkpoint_input(&input);
    if !writebacks.is_empty() {
        let memory_store = memory_store.ok_or_else(|| RecoveryValidationError::Internal {
            message: "execution pipeline 未配置".to_string(),
        })?;
        writebacks.apply(memory_store);
    }

    workspace_registry
        .consume_recovery_with_ownership(
            &input.recovery_id,
            ExecutionOwnership {
                session_id: Some(chain.session_id.clone()),
                workspace_id: chain.workspace_id.clone(),
                mission_id: Some(chain.mission_id.clone()),
                task_id: Some(primary_branch.task_id.clone()),
                worker_id: Some(primary_branch.worker_id.clone()),
                execution_chain_ref: Some(chain.execution_chain_ref.clone()),
            },
        )
        .map_err(|error| RecoveryValidationError::Internal {
            message: error.to_string(),
        })?;

    session_store
        .attach_recovery_ref(session_id, None)
        .map_err(|error| RecoveryValidationError::Internal {
            message: error.to_string(),
        })?;
    chain.recovery_ref = None;
    Ok(())
}
