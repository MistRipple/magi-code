use crate::{errors::ApiError, state::ApiState};
use magi_conversation_runtime::dispatch_submission::{
    DispatchSubmissionAcceptError, DispatchSubmissionRuntime, accept_dispatch_submission,
    ensure_dispatch_submission_acceptance_available, run_dispatch_submission,
};
pub(crate) use magi_conversation_runtime::dispatch_submission::{
    DispatchSubmissionAccepted, DispatchSubmissionRequest,
};
use magi_core::TaskTier;

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

pub fn drive_dispatch_submission(
    state: &ApiState,
    accepted: &mut DispatchSubmissionAccepted,
) -> Result<(), ApiError> {
    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("驱动任务派发", "task_store 未配置"))?;
    let task = task_store
        .get_task(&accepted.root_task_id)
        .ok_or_else(|| ApiError::not_found("任务不存在", accepted.root_task_id.as_str()))?;
    let is_long_mission = task
        .policy_snapshot
        .as_ref()
        .is_some_and(|policy| policy.task_tier == TaskTier::LongMission);

    if is_long_mission {
        let manager = state
            .runner_manager()
            .ok_or_else(|| ApiError::internal_assembly("驱动任务派发", "runner_manager 未配置"))?;
        let _ = manager.start(
            accepted.root_task_id.as_str(),
            Some(accepted.session_id.clone()),
        );
        accepted.runner_started = true;
        return Ok(());
    }

    let drive_result = crate::a_path::drive_a_path(
        state,
        &accepted.root_task_id,
        &accepted.action_task_id,
        "驱动任务派发失败",
    )
    .map_err(|error| ApiError::internal_assembly("驱动任务派发失败", error.message()))?;
    accepted.runner_started = drive_result.runner_started;
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
