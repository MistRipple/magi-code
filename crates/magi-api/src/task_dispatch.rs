use crate::{
    errors::ApiError,
    state::{ApiState, RunnerStartError},
};
use magi_conversation_runtime::dispatch_submission::{
    DispatchSubmissionAcceptError, DispatchSubmissionRuntime, accept_dispatch_submission,
    ensure_dispatch_submission_acceptance_available, run_dispatch_submission,
};
pub(crate) use magi_conversation_runtime::dispatch_submission::{
    DispatchSubmissionAccepted, DispatchSubmissionRequest,
};

pub fn submit_dispatch_submission(
    state: &ApiState,
    request: DispatchSubmissionRequest,
) -> Result<DispatchSubmissionAccepted, ApiError> {
    ensure_dispatch_submission_acceptance_available(&state.session_store, &request).map_err(
        |error| ApiError::internal_assembly("检查任务派发接受条件失败", error.message()),
    )?;
    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("构建任务派发运行时", "task_store 未配置"))?;
    let workspace_root_path = state.workspace_root_path(&request.workspace_id);
    let runtime = DispatchSubmissionRuntime {
        session_store: &state.session_store,
        task_store,
        execution_registry: state.task_execution_registry(),
        event_bus: &state.event_bus,
        agent_role_registry: &state.agent_role_registry,
        spawn_graph: &state.spawn_graph,
        model_bridge_client: state.model_bridge_client(),
        settings_store: Some(&state.settings_store),
        workspace_root_path: workspace_root_path.as_deref(),
    };
    let graph = run_dispatch_submission(&runtime, &request).map_err(|error| {
        ApiError::internal_assembly("构建任务派发提交失败", error.into_message())
    })?;
    accept_dispatch_submission(
        &state.session_store,
        state.task_store(),
        state.task_execution_registry(),
        request,
        graph,
    )
    .map_err(dispatch_accept_error_to_api_error)
}

/// 驱动任务派发：所有 tier 统一交给 [`RunnerManager`] 后台循环驱动。
///
/// 所有 tier 都走同一条后台 runner 路径：单一实现、单一调度模型，
/// 避免「同一功能两种实现方式」（cn-engineering-standard）。
///
/// agent_spawn 只创建代理并投递初始任务消息；后台 runner 模型下 dispatch
/// 由独立调度循环持续推进，父代理后续通过 agent_wait 收集代理终态。
pub fn drive_dispatch_submission(
    state: &ApiState,
    accepted: &mut DispatchSubmissionAccepted,
) -> Result<(), ApiError> {
    let manager = state
        .runner_manager()
        .ok_or_else(|| ApiError::internal_assembly("驱动任务派发", "runner_manager 未配置"))?;
    match manager.start(
        accepted.root_task_id.as_str(),
        Some(accepted.session_id.clone()),
    ) {
        Ok(_) | Err(RunnerStartError::AlreadyRunning) => {}
        Err(RunnerStartError::NotFound) => {
            return Err(ApiError::internal_assembly("驱动任务派发", "根任务不存在"));
        }
    }
    accepted.runner_started = true;
    Ok(())
}

fn dispatch_accept_error_to_api_error(error: DispatchSubmissionAcceptError) -> ApiError {
    match error {
        DispatchSubmissionAcceptError::Conflict { message } => ApiError::Conflict(message),
        DispatchSubmissionAcceptError::Internal { message } => {
            ApiError::InternalAssemblyError(format!("任务派发接受失败: {}", message))
        }
    }
}
