use crate::{
    errors::ApiError,
    state::{ApiState, RunnerStartError},
};
use magi_conversation_runtime::dispatch_submission::{
    DispatchSubmissionAcceptError, DispatchSubmissionGraph, DispatchSubmissionRuntime,
    accept_dispatch_submission, dispatch_turn_id, ensure_dispatch_submission_acceptance_available,
    materialize_dispatch_submission, prepare_pending_dispatch_submission,
};
pub(crate) use magi_conversation_runtime::dispatch_submission::{
    DispatchSubmissionAccepted, DispatchSubmissionRequest, DispatchTurnOrigin,
};
use magi_conversation_runtime::{
    CoordinatorAdmission, CoordinatorCommandResult, ExecutionProfile, TurnAdmission, TurnCommand,
};
use sha2::{Digest, Sha256};

pub fn submit_dispatch_submission(
    state: &ApiState,
    mut request: DispatchSubmissionRequest,
) -> Result<DispatchSubmissionAccepted, ApiError> {
    ensure_dispatch_submission_acceptance_available(&state.session_store, &request)
        .map_err(dispatch_accept_error_to_api_error)?;

    // Task 与 Conversation 共用同一个 SessionTurnCoordinator。Task 的 root task
    // 仍由 TaskStore 管理，但 Turn 身份、attempt 和 request 幂等必须先注册，
    // 再进入 pending task / canonical accepted 事务；否则两个并发提交可能各自
    // 创建一棵任务树。
    let turn_id = dispatch_turn_id(&request);
    let request_id = request
        .request_id
        .clone()
        .unwrap_or_else(|| format!("request-{turn_id}"));
    let request_fingerprint = request
        .user_message_metadata
        .get("requestFingerprint")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| dispatch_request_fingerprint(&request));
    request.request_id = Some(request_id.clone());
    request.user_message_metadata.insert(
        "executionProfile".to_string(),
        serde_json::Value::String("task".to_string()),
    );
    request.user_message_metadata.insert(
        "requestId".to_string(),
        serde_json::Value::String(request_id.clone()),
    );
    request.user_message_metadata.insert(
        "requestFingerprint".to_string(),
        serde_json::Value::String(request_fingerprint.clone()),
    );
    let admission = match state
        .turn_coordinator()
        .execute_command(
            &request.session_id,
            TurnCommand::Start(TurnAdmission {
                turn_id: turn_id.clone(),
                request_id,
                request_fingerprint,
                profile: ExecutionProfile::Task,
            }),
        )
        .map_err(|error| ApiError::Conflict(error.to_string()))?
    {
        CoordinatorCommandResult::Admission(admission) => admission,
        other => {
            return Err(ApiError::internal_assembly(
                "接纳 task Turn",
                format!("Coordinator 返回了非法 Start 结果: {other:?}"),
            ));
        }
    };
    let coordinator_attempt = match admission {
        CoordinatorAdmission::Accepted(attempt) => attempt,
        CoordinatorAdmission::Replay(attempt) => {
            // canonical replay 在 HTTP/App Server 入口已经优先处理；到这里仍遇到
            // 进程内并发重试时，返回明确冲突，让调用方读取同一个 accepted 事实，
            // 绝不能再次 materialize 第二个 root task。
            return Err(ApiError::Conflict(format!(
                "Task Turn {} 已经接纳（attempt {}），请重放原请求读取 canonical receipt",
                attempt.turn_id, attempt.attempt_id
            )));
        }
    };
    request.user_message_metadata.insert(
        "attemptId".to_string(),
        serde_json::Value::String(coordinator_attempt.attempt_id.clone()),
    );

    let task_store = state.task_store().ok_or_else(|| {
        let _ = state.turn_coordinator().execute_command(
            &request.session_id,
            TurnCommand::Abort {
                attempt: coordinator_attempt.clone(),
            },
        );
        ApiError::internal_assembly("构建任务派发运行时", "task_store 未配置")
    })?;
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
    let pending = match prepare_pending_dispatch_submission(&runtime, &request) {
        Ok(pending) => pending,
        Err(error) => {
            let _ = state
                .turn_coordinator()
                .abort(&request.session_id, &coordinator_attempt);
            return Err(ApiError::internal_assembly(
                "准备任务派发提交失败",
                error.into_message(),
            ));
        }
    };
    let accepted_session_id_for_abort = request.session_id.clone();
    let graph = DispatchSubmissionGraph::new(
        pending.task.task_id.clone(),
        pending.task.task_id,
        Some(pending.active_execution_chain),
    );
    match accept_dispatch_submission(
        &state.session_store,
        task_store,
        state.task_execution_registry(),
        request,
        graph,
    ) {
        Ok(accepted) => {
            if let Some(notifier) = state.task_completion_notifier() {
                notifier.bind_coordinator_attempt(
                    &accepted.root_task_id,
                    &coordinator_attempt.attempt_id,
                    Some(accepted.session_id.clone()),
                    Some(accepted.turn_id.clone()),
                );
            }
            Ok(accepted)
        }
        Err(error) => {
            let _ = state
                .turn_coordinator()
                .abort(&accepted_session_id_for_abort, &coordinator_attempt);
            Err(dispatch_accept_error_to_api_error(error))
        }
    }
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

fn dispatch_request_fingerprint(request: &DispatchSubmissionRequest) -> String {
    let bytes = serde_json::to_vec(request).expect("DispatchSubmissionRequest must serialize");
    let digest = Sha256::digest(bytes);
    format!("sha256:{digest:x}")
}

fn dispatch_accept_error_to_api_error(error: DispatchSubmissionAcceptError) -> ApiError {
    match error {
        DispatchSubmissionAcceptError::Conflict { message } => ApiError::Conflict(message),
        DispatchSubmissionAcceptError::Internal { message } => {
            ApiError::InternalAssemblyError(format!("任务派发接受失败: {}", message))
        }
    }
}
