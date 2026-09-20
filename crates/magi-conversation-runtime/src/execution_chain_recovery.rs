//! 任务系统 — 执行链"继续会话"恢复路径。
//!
//! 本文件承担数据载体（[`SessionContinueAccepted`]）+ 纯/弱状态判定 + recovery
//! 校验 / 校对 / 应用 / writeback 落盘 / branch checkpoint 同步 / 子树解封等
//! "继续会话"实现细节。错误类型统一为 `String`，函数签名走显式 stores。

use magi_core::{
    ExecutionOwnership, RecoveryResumeInput, SessionId, TaskCompletionAttempt, TaskStatus,
    TerminationReason, UtcMillis,
};
use magi_memory_store::MemoryStore;
use magi_orchestrator::{ExecutionWritebackPlans, task_store::TaskStore};
use magi_session_store::{ActiveExecutionBranch, ActiveExecutionChain, SessionStore};
use magi_spawn_graph::SpawnGraph;
use magi_worker_runtime::{
    WorkerBranchCheckpointState, WorkerCheckpointResumeMode, WorkerExecutionBindingLifecycle,
    WorkerExecutionCheckpointCursor, WorkerRuntime, WorkerStage,
};
use magi_workspace::{RecoveryStatus, WorkspaceStore};

#[derive(Clone, Debug)]
pub struct SessionContinueAccepted {
    pub session_id: SessionId,
    pub mission_id: magi_core::MissionId,
    pub root_task_id: magi_core::TaskId,
    pub action_task_id: magi_core::TaskId,
    pub turn_id: String,
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

fn root_task_allows_continue(status: &TaskStatus) -> bool {
    !matches!(status, TaskStatus::Completed | TaskStatus::Killed)
}

fn task_status_needs_terminal_branch_finalization(status: &TaskStatus) -> bool {
    matches!(status, TaskStatus::Pending | TaskStatus::Running)
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
    let Some(root_task) = task_store.get_task(&chain.root_task_id) else {
        return false;
    };
    if root_task.mission_id != chain.mission_id || !root_task_allows_continue(&root_task.status) {
        return false;
    }
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

/// 收敛 chain 中所有"已经在 worker runtime 里跑到 Finish 但 task_store 还停留在
/// 非终态"的 branch：把它们落盘成 `TaskStatus::Completed/Failed/Cancelled`。
/// 用于会话中断与执行链续跑入口的统一护栏。
///
/// `Result::Err(String)`：上层 magi-api 用
/// `.map_err(|msg| ApiError::internal_assembly("收敛代理终态失败", msg))` 桥回 ApiError。
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
        if matches!(terminal_status, TaskStatus::Completed) {
            let lease_id = branch.lease_id.as_ref().ok_or_else(|| {
                format!(
                    "恢复 branch {} 缺少执行租约，拒绝无 lease 完成提交",
                    branch.task_id
                )
            })?;
            let final_response = task.output_refs.join("\n\n");
            let attempt = TaskCompletionAttempt {
                output_refs: task.output_refs.clone(),
                final_response: Some(final_response),
                evidence: Vec::new(),
            };
            if !task_store
                .complete_lease_and_task(&branch.task_id, &chain.root_task_id, lease_id, attempt)
                .map_err(|error| error.to_string())?
            {
                return Err(format!(
                    "恢复 branch {} 的执行租约已失效，未提交完成事实",
                    branch.task_id
                ));
            }
        } else {
            let changed = task_store
                .revoke_lease_and_set_task_terminal(
                    &branch.task_id,
                    &chain.root_task_id,
                    branch.lease_id.as_ref(),
                    terminal_status,
                    Vec::new(),
                )
                .map_err(|error| error.to_string())?;
            if !changed {
                return Err(format!(
                    "恢复 branch {} 的执行租约已失效，未提交终止事实",
                    branch.task_id
                ));
            }
        }
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
                .reopen_failed_task_for_recovery(&task_id)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// 将一次恢复尝试触及的 branch 路径收口为 Failed。
///
/// 该函数只使用当前仍然有效的 lease 做撤销；没有 lease 的 Pending/Running 任务可以
/// 被标记为 Failed，但绝不允许借此伪造 Completed。它是 Continue 的失败回滚边界，重复
/// 调用对已终态任务幂等。
pub fn fail_resumed_execution_paths(
    task_store: &TaskStore,
    spawn_graph: &std::sync::Mutex<SpawnGraph>,
    chain: &ActiveExecutionChain,
    branches: &[ActiveExecutionBranch],
) -> Result<(), String> {
    let mut task_ids = Vec::new();
    for branch in branches {
        let mut path_seen = Vec::new();
        let mut current_task_id = Some(branch.task_id.clone());
        while let Some(task_id) = current_task_id {
            if path_seen.iter().any(|seen| seen == &task_id) {
                return Err(format!("恢复 branch {} 的父任务路径存在环", branch.task_id));
            }
            path_seen.push(task_id.clone());
            if !task_ids.iter().any(|seen| seen == &task_id) {
                task_ids.push(task_id.clone());
            }
            if task_id == chain.root_task_id {
                break;
            }
            let task = task_store
                .get_task(&task_id)
                .ok_or_else(|| format!("恢复 branch 任务不存在: {task_id}"))?;
            if task.mission_id != chain.mission_id || task.root_task_id != chain.root_task_id {
                return Err(format!("恢复 branch 路径任务不属于当前执行链: {task_id}"));
            }
            current_task_id = {
                let graph = spawn_graph
                    .lock()
                    .map_err(|error| format!("SpawnGraph 锁中毒: {error}"))?;
                graph.parent_of(&task_id).cloned()
            };
        }
    }
    if !task_ids
        .iter()
        .any(|task_id| task_id == &chain.root_task_id)
    {
        task_ids.push(chain.root_task_id.clone());
    }

    for task_id in task_ids {
        let task = task_store
            .get_task(&task_id)
            .ok_or_else(|| format!("恢复失败收口任务不存在: {task_id}"))?;
        if task_status_is_terminal(&task.status) || task.status == TaskStatus::Failed {
            continue;
        }
        if !matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
            return Err(format!(
                "恢复失败收口任务 {} 当前状态 {:?} 不可写入 Failed",
                task_id, task.status
            ));
        }
        let active_lease = task_store.get_active_lease(&task_id);
        let changed = task_store
            .revoke_lease_and_set_task_terminal(
                &task_id,
                &chain.root_task_id,
                active_lease.as_ref().map(|lease| &lease.lease_id),
                TaskStatus::Failed,
                Vec::new(),
            )
            .map_err(|error| error.to_string())?;
        if !changed {
            let current = task_store
                .get_task(&task_id)
                .ok_or_else(|| format!("恢复失败收口任务不存在: {task_id}"))?;
            if !matches!(
                current.status,
                TaskStatus::Failed | TaskStatus::Completed | TaskStatus::Killed
            ) {
                return Err(format!(
                    "恢复失败收口任务 {} 未提交 Failed，当前状态 {:?}",
                    task_id, current.status
                ));
            }
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
        WorkerBranchCheckpointState {
            lease_id: branch.lease_id.as_ref().map(ToString::to_string),
            execution_intent_ref: branch.execution_intent_ref.clone(),
            binding_lifecycle: parse_branch_binding_lifecycle(branch.binding_lifecycle.as_deref()),
            checkpoint_cursor: branch_checkpoint_cursor(branch),
        },
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
        magi_core::DomainError::Persistence { message } => {
            RecoveryValidationError::Internal { message }
        }
        magi_core::DomainError::CurrentTurnConflict {
            session_id,
            active_turn_id,
        } => RecoveryValidationError::Internal {
            message: format!(
                "recovery 输入构建遇到无关的会话轮次冲突: session={session_id}, turn={active_turn_id}"
            ),
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

#[derive(Clone, Debug)]
pub struct ChainRecoveryCommit {
    recovery_id: String,
    ownership: ExecutionOwnership,
}

/// 若 chain.recovery_ref 存在：校验 recovery 状态 / 入口匹配 → 应用 recovery resume
/// 输入到 session_store → 落盘 writebacks。Recovery 仍保持 Ready，直到 runner 成功启动
/// 后由 [`commit_chain_recovery`] 完成消费和 chain ref 清理。
pub fn apply_chain_recovery_if_needed(
    session_store: &SessionStore,
    workspace_registry: &WorkspaceStore,
    memory_store: Option<&MemoryStore>,
    session_id: &SessionId,
    chain: &mut ActiveExecutionChain,
    primary_branch: &ActiveExecutionBranch,
) -> Result<Option<ChainRecoveryCommit>, RecoveryValidationError> {
    let Some(recovery_id) = chain.recovery_ref.clone() else {
        return Ok(None);
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

    Ok(Some(ChainRecoveryCommit {
        recovery_id: input.recovery_id,
        ownership: ExecutionOwnership {
            session_id: Some(chain.session_id.clone()),
            workspace_id: chain.workspace_id.clone(),
            mission_id: Some(chain.mission_id.clone()),
            task_id: Some(primary_branch.task_id.clone()),
            worker_id: Some(primary_branch.worker_id.clone()),
            execution_chain_ref: Some(chain.execution_chain_ref.clone()),
        },
    }))
}

/// 在 runner 已成功启动后提交 recovery 消费。
///
/// 消费和 session ref 清理跨两个 store，若清理失败，补偿回 Ready 并保留 ref，使下一次
/// Continue 仍然可以重新进入，而不会留下 consumed recovery + active chain 的裂缝。
pub fn commit_chain_recovery(
    session_store: &SessionStore,
    workspace_registry: &WorkspaceStore,
    session_id: &SessionId,
    chain: &mut ActiveExecutionChain,
    commit: ChainRecoveryCommit,
) -> Result<(), RecoveryValidationError> {
    if chain.recovery_ref.as_deref() != Some(commit.recovery_id.as_str()) {
        return Err(RecoveryValidationError::Mismatch {
            message: format!(
                "恢复提交 {} 与当前执行链 recovery_ref 不一致",
                commit.recovery_id
            ),
        });
    }
    workspace_registry
        .consume_recovery_with_ownership(&commit.recovery_id, commit.ownership)
        .map_err(|error| RecoveryValidationError::Internal {
            message: error.to_string(),
        })?;
    if let Err(error) = session_store.attach_recovery_ref(session_id, None) {
        let compensation = workspace_registry.mark_recovery_ready(&commit.recovery_id);
        let message = match compensation {
            Ok(_) => format!(
                "恢复 {} 已消费但 chain ref 清理失败，已补偿为 Ready: {}",
                commit.recovery_id, error
            ),
            Err(compensation_error) => format!(
                "恢复 {} 消费后 chain ref 清理失败，且无法补偿为 Ready: {}; {}",
                commit.recovery_id, error, compensation_error
            ),
        };
        return Err(RecoveryValidationError::Internal { message });
    }
    chain.recovery_ref = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{
        ExecutionOwnership, MissionId, Task, TaskId, TaskKind, TaskRuntimePayload, ThreadId,
        WorkerId, WorkspaceId,
    };
    use magi_session_store::{
        ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
    };
    use std::time::SystemTime;

    fn task(
        task_id: &str,
        root_task_id: &str,
        mission_id: &str,
        parent_task_id: Option<&str>,
        status: TaskStatus,
    ) -> Task {
        let now = UtcMillis(1);
        Task {
            task_id: TaskId::new(task_id),
            mission_id: MissionId::new(mission_id),
            root_task_id: TaskId::new(root_task_id),
            parent_task_id: parent_task_id.map(TaskId::new),
            kind: TaskKind::LocalAgent,
            title: task_id.to_string(),
            goal: task_id.to_string(),
            status,
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

    fn branch(
        task_id: &str,
        worker_id: &str,
        stage: &str,
        lease_id: Option<&str>,
    ) -> ActiveExecutionBranch {
        ActiveExecutionBranch {
            task_id: TaskId::new(task_id),
            worker_id: WorkerId::new(worker_id),
            stage: stage.to_string(),
            lease_id: lease_id.map(magi_core::LeaseId::new),
            execution_intent_ref: None,
            binding_lifecycle: None,
            checkpoint_stage: None,
            next_step_index: None,
            checkpoint_at: None,
            resume_mode: None,
            resume_token: None,
            use_tools: true,
            skill_name: None,
            is_primary: true,
            thread_id: ThreadId::new(format!("thread-{task_id}")),
        }
    }

    fn chain(
        session_id: &SessionId,
        mission_id: &MissionId,
        root_task_id: &TaskId,
        branch: ActiveExecutionBranch,
        recovery_ref: Option<String>,
    ) -> ActiveExecutionChain {
        ActiveExecutionChain {
            session_id: session_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            execution_chain_ref: "chain-recovery-test".to_string(),
            workspace_id: Some(WorkspaceId::new("workspace-recovery-test")),
            active_branch_task_ids: vec![branch.task_id.clone()],
            active_worker_bindings: vec![branch.worker_id.clone()],
            branches: vec![branch],
            recovery_ref,
            dispatch_context: ActiveExecutionDispatchContext {
                accepted_at: UtcMillis(1),
                entry_id: "entry-recovery-test".to_string(),
                trimmed_text: None,
                skill_name: None,
            },
            current_turn: None,
        }
    }

    #[test]
    fn terminal_completion_requires_branch_lease() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let session_id = SessionId::new("session-terminal-lease");
        let mission_id = MissionId::new("mission-terminal-lease");
        let root_task_id = TaskId::new("task-root-terminal-lease");
        let branch_task_id = TaskId::new("task-branch-terminal-lease");
        session_store
            .create_session(session_id.clone(), "terminal lease")
            .expect("session should exist");
        task_store
            .insert_task(task(
                root_task_id.as_str(),
                root_task_id.as_str(),
                mission_id.as_str(),
                None,
                TaskStatus::Running,
            ))
            .expect("root should insert");
        task_store
            .insert_task(task(
                branch_task_id.as_str(),
                root_task_id.as_str(),
                mission_id.as_str(),
                Some(root_task_id.as_str()),
                TaskStatus::Running,
            ))
            .expect("branch should insert");
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                chain(
                    &session_id,
                    &mission_id,
                    &root_task_id,
                    branch(
                        branch_task_id.as_str(),
                        "worker-terminal-lease",
                        "finish",
                        None,
                    ),
                    None,
                ),
            )
            .expect("chain should persist");

        let error =
            finalize_terminal_worker_branches(&session_store, Some(&task_store), None, &session_id)
                .expect_err("completion without lease must be rejected");
        assert!(error.contains("拒绝无 lease 完成提交"));
        assert_eq!(
            task_store
                .get_task(&branch_task_id)
                .expect("branch should remain")
                .status,
            TaskStatus::Running
        );
    }

    #[test]
    fn recovery_apply_stays_ready_until_runner_commit() {
        let session_store = SessionStore::new();
        let workspace_registry = WorkspaceStore::new();
        let session_id = SessionId::new("session-recovery-commit");
        let mission_id = MissionId::new("mission-recovery-commit");
        let root_task_id = TaskId::new("task-root-recovery-commit");
        let branch = branch(
            "task-branch-recovery-commit",
            "worker-recovery-commit",
            "execute",
            None,
        );
        let recovery_id = "recovery-commit-boundary";
        let chain_ref = "chain-recovery-test";
        session_store
            .create_session(session_id.clone(), "recovery commit")
            .expect("session should exist");
        workspace_registry.prepare_recovery_entry(
            WorkspaceId::new("workspace-recovery-test"),
            ExecutionOwnership {
                session_id: Some(session_id.clone()),
                mission_id: Some(mission_id.clone()),
                execution_chain_ref: Some(chain_ref.to_string()),
                ..ExecutionOwnership::default()
            },
            "snapshot-recovery-commit",
            recovery_id,
            None,
        );
        workspace_registry
            .mark_recovery_ready(recovery_id)
            .expect("recovery should become ready");
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                chain(
                    &session_id,
                    &mission_id,
                    &root_task_id,
                    branch.clone(),
                    Some(recovery_id.to_string()),
                ),
            )
            .expect("chain should persist");
        let mut active_chain = session_store
            .active_execution_chain(&session_id)
            .expect("active chain should exist");

        let commit = apply_chain_recovery_if_needed(
            &session_store,
            &workspace_registry,
            None,
            &session_id,
            &mut active_chain,
            &branch,
        )
        .expect("recovery should apply")
        .expect("recovery commit should be required");
        assert_eq!(
            workspace_registry
                .recovery_sidecar_export(recovery_id)
                .expect("recovery should exist")
                .current_status,
            RecoveryStatus::Ready
        );
        assert_eq!(active_chain.recovery_ref.as_deref(), Some(recovery_id));
        assert_eq!(
            session_store
                .active_execution_chain(&session_id)
                .expect("chain should exist")
                .recovery_ref
                .as_deref(),
            Some(recovery_id)
        );

        commit_chain_recovery(
            &session_store,
            &workspace_registry,
            &session_id,
            &mut active_chain,
            commit,
        )
        .expect("recovery commit should succeed");
        assert_eq!(
            workspace_registry
                .recovery_sidecar_export(recovery_id)
                .expect("recovery should exist")
                .current_status,
            RecoveryStatus::Consumed
        );
        assert!(active_chain.recovery_ref.is_none());
        assert!(
            session_store
                .active_execution_chain(&session_id)
                .expect("chain should exist")
                .recovery_ref
                .is_none()
        );
    }

    #[test]
    fn failed_recovery_paths_are_idempotent_and_revoke_active_lease() {
        let task_store = TaskStore::new();
        let session_id = SessionId::new("session-failed-path");
        let mission_id = MissionId::new("mission-failed-path");
        let root_task_id = TaskId::new("task-root-failed-path");
        let branch_task_id = TaskId::new("task-branch-failed-path");
        task_store
            .insert_task(task(
                root_task_id.as_str(),
                root_task_id.as_str(),
                mission_id.as_str(),
                None,
                TaskStatus::Running,
            ))
            .expect("root should insert");
        task_store
            .insert_task(task(
                branch_task_id.as_str(),
                root_task_id.as_str(),
                mission_id.as_str(),
                Some(root_task_id.as_str()),
                TaskStatus::Pending,
            ))
            .expect("branch should insert");
        let worker_id = WorkerId::new("worker-failed-path");
        let lease = task_store
            .grant_lease_and_start_task(
                &branch_task_id,
                &root_task_id,
                &worker_id,
                "executor",
                60_000,
            )
            .expect("lease should grant")
            .expect("lease should be active");
        let mut graph = SpawnGraph::new();
        graph
            .add_edge(
                root_task_id.clone(),
                branch_task_id.clone(),
                TaskKind::LocalAgent,
                SystemTime::UNIX_EPOCH,
            )
            .expect("spawn edge should insert");
        let chain = chain(
            &session_id,
            &mission_id,
            &root_task_id,
            branch(
                branch_task_id.as_str(),
                worker_id.as_str(),
                "execute",
                Some(lease.lease_id.as_str()),
            ),
            None,
        );

        fail_resumed_execution_paths(
            &task_store,
            &std::sync::Mutex::new(graph),
            &chain,
            &chain.branches,
        )
        .expect("failure path should close");
        fail_resumed_execution_paths(
            &task_store,
            &std::sync::Mutex::new(SpawnGraph::new()),
            &chain,
            &chain.branches,
        )
        .expect("repeated failure path should be idempotent");
        assert_eq!(
            task_store.get_task(&root_task_id).unwrap().status,
            TaskStatus::Failed
        );
        assert_eq!(
            task_store.get_task(&branch_task_id).unwrap().status,
            TaskStatus::Failed
        );
        assert_eq!(
            task_store.get_lease(&lease.lease_id).unwrap().lease_status,
            magi_orchestrator::task_store::TaskLeaseState::Revoked
        );
    }
}
