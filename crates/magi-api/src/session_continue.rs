//! 继续会话 API 适配层。
//!
//! 纯判定、校验、writeback 落盘、branch checkpoint 同步和子树解封逻辑已下沉到
//! `magi_conversation_runtime::execution_chain_recovery`；本模块只负责把 `ApiState`
//! 持有的 runner、task store 与 execution registry 装配给 runtime 恢复流程。

use crate::{
    errors::ApiError,
    state::{ApiState, RunnerStartError},
};
use magi_conversation_runtime::{
    execution_chain_recovery::{
        apply_chain_recovery_if_needed, release_resumed_branch_path,
        sync_branch_checkpoint_to_worker_runtime,
    },
    task_execution_registry::TaskExecutionPlan,
};
use magi_core::{
    ExecutionOwnership, SessionId, TaskExecutionTarget, TaskStatus, TaskTier, ThreadId, WorkerId,
};
use magi_orchestrator::ExecutionWritebackPlans;
use magi_session_store::{ActiveExecutionBranch, ActiveExecutionChain};

// 对 routes/sessions.rs 暴露继续会话所需的 runtime 数据载体与判定函数。
pub(crate) use magi_conversation_runtime::execution_chain_recovery::{
    SessionContinueAccepted, active_execution_branch_is_continue_recoverable,
    finalize_terminal_worker_branches, task_status_is_terminal,
};

fn rebuild_dispatch_plan_for_branch(
    chain: &ActiveExecutionChain,
    branch: &ActiveExecutionBranch,
    orchestrator_thread_id: &ThreadId,
) -> TaskExecutionPlan {
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
            },
        )
    } else {
        ExecutionWritebackPlans::default()
    };
    // 恢复链路的 thread_id 来源：
    //  * sub task 在 current_turn.worker_lanes 中有 lane，直接复用 lane.thread_id；
    //  * primary action 没有 lane，归属 session 级 orchestrator thread。
    // orchestrator thread 由调用方在 resume 入口通过 ensure_session_mission 幂等保证。
    let lane_thread_id = chain.current_turn.as_ref().and_then(|turn| {
        turn.worker_lanes
            .iter()
            .find(|lane| lane.task_id == branch.task_id)
            .map(|lane| lane.thread_id.clone())
    });
    let thread_id = lane_thread_id.unwrap_or_else(|| orchestrator_thread_id.clone());
    TaskExecutionPlan::Dispatch {
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
        thread_id,
        is_primary: branch.is_primary,
        session_id: chain.session_id.clone(),
        workspace_id: chain.workspace_id.clone(),
        ownership,
        writebacks,
        use_tools: branch.use_tools,
        skill_name: branch.skill_name.clone(),
    }
}

pub(crate) fn continue_execution_chain(
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
    let worker_runtime_handle = state
        .execution_pipeline()
        .map(|pipeline| pipeline.execution_runtime.worker_runtime());
    finalize_terminal_worker_branches(
        &state.session_store,
        state.task_store(),
        worker_runtime_handle,
        session_id,
    )
    .map_err(|msg| ApiError::internal_assembly("收敛 worker 终态失败", msg))?;

    let resumable_branches = chain
        .branches
        .iter()
        .filter_map(|branch| {
            active_execution_branch_is_continue_recoverable(
                worker_runtime_handle,
                state.task_store(),
                &chain,
                branch,
            )
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
    let memory_store = state
        .execution_pipeline()
        .map(|pipeline| &pipeline.memory_store);
    apply_chain_recovery_if_needed(
        &state.session_store,
        &state.workspace_registry,
        memory_store,
        session_id,
        &mut chain,
        primary_branch,
    )
    .map_err(|error| {
        let message = error.into_message();
        // 与原实现保持一致：NotFound 与 InvalidStatus 走 InvalidInput / NotFound 分类。
        if message.starts_with("recovery 不存在") {
            ApiError::recovery_not_found(
                message
                    .strip_prefix("recovery 不存在: ")
                    .unwrap_or(message.as_str()),
            )
        } else if message.contains("继续检查点")
            || message.contains("恢复入口")
            || message.contains("workspace 不一致")
        {
            ApiError::InvalidInput(message)
        } else {
            ApiError::internal_assembly("继续会话失败", message)
        }
    })?;

    // resume 入口幂等地保证 orchestrator thread 存在：
    //  * 已存在 → 直接复用 (同 session 同 mission 同 orchestrator thread 不变量)；
    //  * 不存在 → 用 chain.mission_id spawn 新 thread，UI 主线归属即此 thread。
    let (_, orchestrator_thread_id) = state.session_store.ensure_session_mission(
        session_id,
        chain.dispatch_context.accepted_at,
        || chain.mission_id.clone(),
    );

    let mut root_status = root_task.status;
    if matches!(root_status, TaskStatus::Completed) {
        task_store
            .update_status(&chain.root_task_id, TaskStatus::Failed)
            .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;
        root_status = TaskStatus::Failed;
    } else if task_status_is_terminal(&root_status) {
        return Err(ApiError::InvalidInput(
            "当前会话执行链已结束，不能继续".to_string(),
        ));
    }

    for branch in &branches_to_resume {
        state.task_execution_registry().insert(
            branch.task_id.clone(),
            rebuild_dispatch_plan_for_branch(&chain, branch, &orchestrator_thread_id),
        );
        if let Some(worker_runtime) = worker_runtime_handle {
            sync_branch_checkpoint_to_worker_runtime(worker_runtime, branch);
        }
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
        TaskStatus::Failed if requested_worker_ids.is_empty() => manager
            .resume_tree(chain.root_task_id.as_str())
            .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?,
        TaskStatus::Failed => {
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
        release_resumed_branch_path(task_store, state.spawn_graph.as_ref(), &chain, branch)
            .map_err(|msg| ApiError::internal_assembly("继续会话失败", msg))?;
    }

    // Long Mission：恢复任务状态后需要重新启动后台 runner，避免退化为同步执行
    let is_long_mission = root_task
        .policy_snapshot
        .as_ref()
        .map(|policy| policy.task_tier == TaskTier::LongMission)
        .unwrap_or(false);
    if is_long_mission {
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
