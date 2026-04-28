use crate::{
    errors::ApiError,
    state::{ApiState, RunnerStartError},
    task_execution::ShadowTaskExecutionPlan,
};
use magi_core::{
    ExecutionOwnership, RecoveryResumeInput, SessionId, TaskExecutionTarget, TaskId, TaskStatus,
    WorkerId,
};
use magi_orchestrator::ExecutionWritebackPlans;
use magi_session_store::{ActiveExecutionBranch, ActiveExecutionChain};
use magi_workspace::RecoveryStatus;

#[derive(Clone, Debug)]
pub struct SessionContinueAccepted {
    pub session_id: SessionId,
    pub mission_id: magi_core::MissionId,
    pub root_task_id: TaskId,
    pub action_task_id: TaskId,
    pub execution_chain_ref: String,
    pub resumed_branch_count: usize,
    pub runner_started: bool,
}

fn task_status_is_terminal(status: &TaskStatus) -> bool {
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

pub(crate) fn active_execution_branch_is_continue_recoverable(
    state: &ApiState,
    chain: &ActiveExecutionChain,
    branch: &ActiveExecutionBranch,
) -> bool {
    if branch_stage_is_terminal(&branch.stage) {
        return false;
    }
    let Some(task_store) = state.task_store() else {
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
    state: &ApiState,
    branch: &ActiveExecutionBranch,
) -> Option<TaskStatus> {
    let reports = state
        .shadow_execution_pipeline()?
        .execution_runtime
        .worker_runtime()
        .reports();
    reports
        .iter()
        .rev()
        .find(|report| {
            report.worker_id == branch.worker_id
                && report.task_id == branch.task_id
                && report.stage == magi_worker_runtime::WorkerStage::Finish
        })
        .map(|report| match report.termination_reason {
            Some(magi_core::TerminationReason::Failed) => TaskStatus::Failed,
            Some(magi_core::TerminationReason::Cancelled) => TaskStatus::Cancelled,
            Some(magi_core::TerminationReason::Blocked) => TaskStatus::Blocked,
            Some(magi_core::TerminationReason::Completed) | None => TaskStatus::Completed,
        })
}

pub(crate) fn finalize_terminal_worker_branches(
    state: &ApiState,
    session_id: &SessionId,
) -> Result<usize, ApiError> {
    let Some(chain) = state
        .session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.active_execution_chain)
    else {
        return Ok(0);
    };
    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("收敛 worker 终态失败", "task_store 未配置"))?;
    let mut finalized_count = 0usize;
    for branch in chain
        .branches
        .iter()
        .filter(|branch| branch_stage_is_terminal(&branch.stage))
    {
        let Some(task) = task_store.get_task(&branch.task_id) else {
            continue;
        };
        if !task_status_needs_terminal_branch_finalization(&task.status) {
            continue;
        }
        let terminal_status =
            terminal_status_for_branch(state, branch).unwrap_or(TaskStatus::Completed);
        if matches!(terminal_status, TaskStatus::Blocked) {
            continue;
        }
        task_store
            .update_status(&branch.task_id, terminal_status)
            .map_err(|error| ApiError::internal_assembly("收敛 worker 终态失败", error))?;
        finalized_count += 1;
    }
    Ok(finalized_count)
}

fn rebuild_dispatch_plan_for_branch(
    chain: &ActiveExecutionChain,
    branch: &ActiveExecutionBranch,
) -> ShadowTaskExecutionPlan {
    let ownership = ExecutionOwnership {
        session_id: Some(chain.session_id.clone()),
        workspace_id: chain.workspace_id.clone(),
        mission_id: Some(chain.mission_id.clone()),
        task_id: Some(branch.task_id.clone()),
        worker_id: Some(branch.worker_id.clone()),
        execution_chain_ref: Some(chain.execution_chain_ref.clone()),
    };
    let writebacks = if branch.is_primary {
        ExecutionWritebackPlans::from_session_action_input(
            magi_orchestrator::DispatchMemoryExtractionInput {
                accepted_at: chain.dispatch_context.accepted_at,
                session_id: &chain.session_id,
                timeline_entry_id: chain.dispatch_context.entry_id.as_str(),
                text: chain.dispatch_context.trimmed_text.as_deref(),
                skill_name: chain.dispatch_context.skill_name.as_deref(),
                deep_task: chain.dispatch_context.deep_task,
            },
        )
    } else {
        ExecutionWritebackPlans::default()
    };
    ShadowTaskExecutionPlan::Dispatch {
        target: TaskExecutionTarget {
            mission_id: chain.mission_id.clone(),
            root_task_id: chain.root_task_id.clone(),
            task_id: branch.task_id.clone(),
            requested_worker_id: Some(branch.worker_id.clone()),
            recovery_id: chain.recovery_ref.clone(),
            execution_chain_ref: Some(chain.execution_chain_ref.clone()),
        },
        worker_id: branch.worker_id.clone(),
        lane_id: chain.current_turn.as_ref().and_then(|turn| {
            turn.worker_lanes
                .iter()
                .find(|lane| lane.task_id == branch.task_id)
                .map(|lane| lane.lane_id.clone())
        }),
        lane_seq: chain.current_turn.as_ref().and_then(|turn| {
            turn.worker_lanes
                .iter()
                .find(|lane| lane.task_id == branch.task_id)
                .map(|lane| lane.lane_seq)
        }),
        is_primary: branch.is_primary,
        session_id: chain.session_id.clone(),
        workspace_id: chain.workspace_id.clone(),
        ownership,
        writebacks,
        use_tools: branch.use_tools,
        skill_name: branch.skill_name.clone(),
    }
}

fn validate_recovery_status(state: &ApiState, recovery_id: &str) -> Result<(), ApiError> {
    let export = state
        .workspace_registry
        .recovery_sidecar_export(recovery_id)
        .ok_or_else(|| ApiError::recovery_not_found(recovery_id))?;
    match export.current_status {
        RecoveryStatus::Ready => Ok(()),
        RecoveryStatus::Prepared => Err(ApiError::InvalidInput(format!(
            "继续检查点 {} 当前状态为 prepared，必须先进入 ready 才能继续会话",
            recovery_id
        ))),
        RecoveryStatus::Consumed => Err(ApiError::InvalidInput(format!(
            "继续检查点 {} 已被消费，不能再次继续会话",
            recovery_id
        ))),
    }
}

fn map_recovery_input_error(recovery_id: &str, error: magi_core::DomainError) -> ApiError {
    match error {
        magi_core::DomainError::NotFound { .. } => ApiError::recovery_not_found(recovery_id),
        magi_core::DomainError::InvalidState { message }
        | magi_core::DomainError::Validation { message } => ApiError::InvalidInput(message),
        magi_core::DomainError::AlreadyExists { entity } => ApiError::internal_assembly(
            "继续会话失败",
            format!("recovery 输入构建遇到重复实体: {entity}"),
        ),
    }
}

fn validate_recovery_input_matches_chain(
    chain: &ActiveExecutionChain,
    input: &RecoveryResumeInput,
) -> Result<(), ApiError> {
    if input.ownership.session_id.as_ref() != Some(&chain.session_id) {
        return Err(ApiError::InvalidInput(format!(
            "恢复入口 {} 不属于当前会话 {}",
            input.recovery_id, chain.session_id
        )));
    }
    if input.ownership.mission_id.as_ref() != Some(&chain.mission_id) {
        return Err(ApiError::InvalidInput(format!(
            "恢复入口 {} 不属于当前执行链 mission {}",
            input.recovery_id, chain.mission_id
        )));
    }
    if input.ownership.workspace_id != chain.workspace_id {
        return Err(ApiError::InvalidInput(format!(
            "恢复入口 {} 的工作区与当前执行链不一致",
            input.recovery_id
        )));
    }
    if input.ownership.execution_chain_ref.as_deref() != Some(chain.execution_chain_ref.as_str()) {
        return Err(ApiError::InvalidInput(format!(
            "恢复入口 {} 的 execution_chain_ref 与当前执行链不一致",
            input.recovery_id
        )));
    }
    Ok(())
}

fn apply_chain_recovery_if_needed(
    state: &ApiState,
    session_id: &SessionId,
    chain: &mut ActiveExecutionChain,
    primary_branch: &ActiveExecutionBranch,
) -> Result<(), ApiError> {
    let Some(recovery_id) = chain.recovery_ref.clone() else {
        return Ok(());
    };
    validate_recovery_status(state, &recovery_id)?;
    let input = state
        .workspace_registry
        .build_recovery_resume_input(&recovery_id)
        .map_err(|error| map_recovery_input_error(&recovery_id, error))?;
    validate_recovery_input_matches_chain(chain, &input)?;

    state
        .session_store
        .apply_recovery_resume_input(session_id.clone(), input.clone())
        .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;

    let writebacks = ExecutionWritebackPlans::from_continue_checkpoint_input(&input);
    if !writebacks.is_empty() {
        let pipeline = state.shadow_execution_pipeline().ok_or_else(|| {
            ApiError::internal_assembly("继续会话失败", "shadow execution pipeline 未配置")
        })?;
        writebacks.apply(&pipeline.memory_store);
    }

    state
        .workspace_registry
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
        .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;

    state
        .session_store
        .attach_recovery_ref(session_id, None)
        .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;
    chain.recovery_ref = None;
    Ok(())
}

pub(crate) fn continue_shadow_execution_chain(
    state: &ApiState,
    session_id: &SessionId,
    requested_worker_ids: &[WorkerId],
) -> Result<SessionContinueAccepted, ApiError> {
    if state.session_store.session(session_id).is_none() {
        return Err(ApiError::session_not_found(session_id.as_str()));
    }
    let sidecar = state
        .session_store
        .runtime_sidecar(session_id)
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有可继续的执行链".to_string()))?;
    let mut chain = sidecar
        .active_execution_chain
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有可继续的执行链".to_string()))?;
    if &chain.session_id != session_id {
        return Err(ApiError::internal_assembly(
            "继续会话失败",
            format!(
                "session sidecar 与 active execution chain 不一致: {} != {}",
                chain.session_id, session_id
            ),
        ));
    }
    if let Some(ownership_chain_ref) = sidecar.ownership.execution_chain_ref.as_deref()
        && ownership_chain_ref != chain.execution_chain_ref
    {
        return Err(ApiError::internal_assembly(
            "继续会话失败",
            format!(
                "session sidecar 的 execution_chain_ref 与 active chain 不一致: {} != {}",
                ownership_chain_ref, chain.execution_chain_ref
            ),
        ));
    }

    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("继续会话失败", "task_store 未配置"))?;
    let root_task = task_store
        .get_task(&chain.root_task_id)
        .ok_or_else(|| ApiError::not_found("根任务不存在", chain.root_task_id.as_str()))?;
    if root_task.mission_id != chain.mission_id {
        return Err(ApiError::internal_assembly(
            "继续会话失败",
            format!(
                "active chain 的 mission_id 与根任务不一致: {} != {}",
                chain.mission_id, root_task.mission_id
            ),
        ));
    }
    finalize_terminal_worker_branches(state, session_id)?;

    let resumable_branches = chain
        .branches
        .iter()
        .filter_map(|branch| {
            active_execution_branch_is_continue_recoverable(state, &chain, branch)
                .then(|| branch.clone())
        })
        .collect::<Vec<_>>();
    if resumable_branches.is_empty() {
        return Err(ApiError::InvalidInput(
            "当前会话没有可继续的 branch".to_string(),
        ));
    }
    if !requested_worker_ids.is_empty() {
        for worker_id in requested_worker_ids {
            if !chain
                .branches
                .iter()
                .any(|branch| &branch.worker_id == worker_id)
            {
                return Err(ApiError::InvalidInput(format!(
                    "请求继续的 worker 不属于当前执行链: {}",
                    worker_id
                )));
            }
        }
        let has_requested_resumable_worker = requested_worker_ids.iter().any(|worker_id| {
            resumable_branches
                .iter()
                .any(|branch| &branch.worker_id == worker_id)
        });
        if !has_requested_resumable_worker {
            return Err(ApiError::InvalidInput(
                "请求继续的 worker 当前不可继续".to_string(),
            ));
        }
    }

    let branches_to_resume = if requested_worker_ids.is_empty() {
        resumable_branches.clone()
    } else {
        resumable_branches
            .iter()
            .filter(|branch| {
                requested_worker_ids
                    .iter()
                    .any(|worker_id| worker_id == &branch.worker_id)
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    if branches_to_resume.is_empty() {
        return Err(ApiError::InvalidInput(
            "请求继续的 worker 当前不可继续".to_string(),
        ));
    }

    let primary_branch = branches_to_resume
        .iter()
        .find(|branch| {
            requested_worker_ids
                .iter()
                .any(|worker_id| worker_id == &branch.worker_id)
        })
        .or_else(|| branches_to_resume.iter().find(|branch| branch.is_primary))
        .or_else(|| branches_to_resume.first())
        .expect("branches_to_resume checked as non-empty");
    apply_chain_recovery_if_needed(state, session_id, &mut chain, primary_branch)?;

    let mut root_status = root_task.status;
    if matches!(root_status, TaskStatus::Completed) {
        task_store
            .update_status(&chain.root_task_id, TaskStatus::Blocked)
            .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;
        root_status = TaskStatus::Blocked;
    } else if task_status_is_terminal(&root_status) {
        return Err(ApiError::InvalidInput(
            "当前会话执行链已结束，不能继续".to_string(),
        ));
    }

    for branch in &branches_to_resume {
        state.shadow_task_execution_registry().insert(
            branch.task_id.clone(),
            rebuild_dispatch_plan_for_branch(&chain, branch),
        );
    }

    state
        .session_store
        .apply_resume_execution_target(
            session_id,
            &TaskExecutionTarget {
                mission_id: chain.mission_id.clone(),
                root_task_id: chain.root_task_id.clone(),
                task_id: primary_branch.task_id.clone(),
                requested_worker_id: Some(primary_branch.worker_id.clone()),
                recovery_id: chain.recovery_ref.clone(),
                execution_chain_ref: Some(chain.execution_chain_ref.clone()),
            },
        )
        .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;

    let manager = state
        .runner_manager()
        .ok_or_else(|| ApiError::internal_assembly("继续会话失败", "runner_manager 未配置"))?;
    match root_status {
        TaskStatus::Blocked if requested_worker_ids.is_empty() => manager
            .resume_tree(chain.root_task_id.as_str())
            .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?,
        TaskStatus::Blocked => {
            task_store
                .update_status(&chain.root_task_id, TaskStatus::Running)
                .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;
        }
        TaskStatus::Running => {}
        other => {
            return Err(ApiError::InvalidInput(format!(
                "当前执行链状态不支持继续: {other:?}"
            )));
        }
    }
    for branch in &branches_to_resume {
        if branch.task_id != chain.root_task_id
            && task_store
                .get_task(&branch.task_id)
                .is_some_and(|task| task.status == TaskStatus::Blocked)
        {
            task_store
                .update_status(&branch.task_id, TaskStatus::Ready)
                .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;
        }
    }

    // 深度模式：恢复任务状态后需要重新启动后台 runner，避免退化为同步执行
    let background_allowed = root_task
        .policy_snapshot
        .as_ref()
        .map(|policy| policy.background_allowed)
        .unwrap_or(false);
    if background_allowed {
        match manager.start(chain.root_task_id.as_str(), Some(session_id.clone())) {
            Ok(_) | Err(RunnerStartError::AlreadyRunning) => {}
            Err(RunnerStartError::NotFound) => {
                return Err(ApiError::internal_assembly("继续会话失败", "根任务不存在"));
            }
        }
    }

    Ok(SessionContinueAccepted {
        session_id: session_id.clone(),
        mission_id: chain.mission_id,
        root_task_id: chain.root_task_id,
        action_task_id: primary_branch.task_id.clone(),
        execution_chain_ref: chain.execution_chain_ref,
        resumed_branch_count: branches_to_resume.len(),
        runner_started: true,
    })
}
