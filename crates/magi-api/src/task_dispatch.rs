use crate::{
    errors::ApiError,
    state::{ApiState, RunnerStartError},
};
use magi_conversation_runtime::dispatch_submission::{
    DispatchSubmissionAcceptError, DispatchSubmissionGraph, DispatchSubmissionRuntime,
    accept_dispatch_submission, ensure_dispatch_submission_acceptance_available,
    materialize_dispatch_submission, prepare_pending_dispatch_submission,
};
pub(crate) use magi_conversation_runtime::dispatch_submission::{
    DispatchSubmissionAccepted, DispatchSubmissionRequest, DispatchTurnOrigin,
};

pub fn submit_dispatch_submission(
    state: &ApiState,
    request: DispatchSubmissionRequest,
) -> Result<DispatchSubmissionAccepted, ApiError> {
    ensure_dispatch_submission_acceptance_available(&state.session_store, &request)
        .map_err(dispatch_accept_error_to_api_error)?;
    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("构建任务派发运行时", "task_store 未配置"))?;
    let workspace_root_path =
        if let Some(context) = state.session_code_contexts.get(request.session_id.as_str()) {
            Some(context.execution_root)
        } else if let Some(root) = state.workspace_root_path(&request.workspace_id) {
            Some(root)
        } else if request.workspace_id.is_none() {
            Some(state.personal_session_execution_root_path(&request.session_id))
        } else {
            None
        };
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
    let pending = prepare_pending_dispatch_submission(&runtime, &request).map_err(|error| {
        ApiError::internal_assembly("准备任务派发提交失败", error.into_message())
    })?;
    let graph = DispatchSubmissionGraph::new(
        pending.task.task_id.clone(),
        pending.task.task_id,
        Some(pending.active_execution_chain),
    );
    accept_dispatch_submission(
        &state.session_store,
        task_store,
        state.task_execution_registry(),
        request,
        graph,
    )
    .map_err(dispatch_accept_error_to_api_error)
}

/// accepted 之后才创建 Thread、执行计划和模型快照。
pub(crate) fn materialize_dispatch_submission_after_acceptance(
    state: &ApiState,
    accepted: &DispatchSubmissionAccepted,
) -> Result<(), ApiError> {
    let request = &accepted.request;
    let workspace_root_path = state
        .session_code_contexts
        .get(request.session_id.as_str())
        .map(|context| context.execution_root)
        .or_else(|| state.workspace_root_path(&request.workspace_id))
        .or_else(|| {
            request
                .workspace_id
                .is_none()
                .then(|| state.personal_session_execution_root_path(&request.session_id))
        });
    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("装配任务派发", "task_store 未配置"))?;
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
    materialize_dispatch_submission(&runtime, request, &accepted.turn_id).map_err(|error| {
        ApiError::internal_assembly("装配任务派发执行面失败", error.into_message())
    })
}

/// 在调用方已经持有 session lifecycle lock 与 root restart lock 时启动 runner。
///
/// 普通提交的后台 finalizer 必须使用这条路径：如果 Continue 先取得生命周期锁，旧
/// finalizer 就不能在新 Turn 上继续执行；如果 finalizer 先取得锁，Continue 会等待旧
/// runner 完整注册并在恢复前 quiesce。这样“accepted 但尚未登记 runner”的窗口不会
/// 再与恢复流程竞态。
pub(crate) fn drive_dispatch_submission_after_lifecycle_and_restart_lock(
    state: &ApiState,
    accepted: &mut DispatchSubmissionAccepted,
) -> Result<(), ApiError> {
    let manager = state
        .runner_manager()
        .ok_or_else(|| ApiError::internal_assembly("驱动任务派发", "runner_manager 未配置"))?;
    match manager.start_after_quiesce(
        accepted.root_task_id.as_str(),
        Some(accepted.session_id.clone()),
    ) {
        Ok(_) | Err(RunnerStartError::AlreadyRunning) => {}
        Err(RunnerStartError::NotFound) => {
            return Err(ApiError::internal_assembly("驱动任务派发", "根任务不存在"));
        }
        Err(RunnerStartError::SessionUnavailable) => {
            return Err(ApiError::InvalidInput(
                "当前会话已关闭，不能启动任务执行".to_string(),
            ));
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
