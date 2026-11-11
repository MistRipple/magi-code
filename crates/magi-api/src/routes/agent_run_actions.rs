use axum::{Json, Router, extract::State, routing::post};
use magi_core::{EventId, SessionId, TaskId, TaskStatus, UtcMillis};
use magi_event_bus::{EventContext, EventEnvelope};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    dto::SessionScopeKindDto,
    errors::ApiError,
    scope_binding::{SessionRequestScope, require_session_request_scope},
    state::ApiState,
};
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::ActiveExecutionTurn;

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/agent-runs/action", post(perform_task_action))
        .route("/agent-runs/transfer", post(transfer_agent_run))
        .route("/agent-runs/continue", post(continue_agent_run))
}

async fn continue_agent_run(
    State(state): State<ApiState>,
    Json(request): Json<magi_app_server_protocol::generated::AgentContinueParams>,
) -> Result<Json<magi_app_server_protocol::generated::AgentContinueResult>, ApiError> {
    crate::agent_service::continue_agent_run(&state, request)
        .await
        .map(Json)
}

async fn transfer_agent_run(
    State(state): State<ApiState>,
    Json(request): Json<magi_app_server_protocol::generated::AgentTransferParams>,
) -> Result<Json<magi_app_server_protocol::generated::AgentTransferResult>, ApiError> {
    crate::agent_service::transfer_agent_run(&state, request)
        .await
        .map(Json)
}

fn require_task_request_scope(
    state: &ApiState,
    session_id_value: Option<&str>,
    requested_scope: SessionScopeKindDto,
    workspace_id_value: Option<&str>,
    workspace_path_value: Option<&str>,
) -> Result<SessionRequestScope, ApiError> {
    require_session_request_scope(
        state,
        session_id_value,
        requested_scope,
        workspace_id_value,
        workspace_path_value,
    )
}

fn turn_contains_task_root(
    store: &TaskStore,
    turn: &ActiveExecutionTurn,
    root_task_id: &TaskId,
) -> bool {
    turn.items.iter().any(|item| {
        item.task_id
            .as_ref()
            .and_then(|task_id| store.get_task(task_id))
            .is_some_and(|task| task.root_task_id == *root_task_id)
    })
}

fn session_history_contains_task(
    state: &ApiState,
    store: &TaskStore,
    session_id: &SessionId,
    task: &magi_core::Task,
) -> bool {
    if let Some(ownership) = state.session_store.execution_ownership(session_id)
        && ownership.mission_id.as_ref() == Some(&task.mission_id)
    {
        return true;
    }
    if let Some(sidecar) = state.session_store.runtime_sidecar(session_id) {
        if sidecar
            .active_execution_chain
            .as_ref()
            .is_some_and(|chain| chain.root_task_id == task.root_task_id)
        {
            return true;
        }
        if sidecar
            .current_turn
            .as_ref()
            .is_some_and(|turn| turn_contains_task_root(store, turn, &task.root_task_id))
        {
            return true;
        }
    }
    state
        .session_store
        .canonical_turns_for_session(session_id)
        .into_iter()
        .flat_map(|turn| turn.items)
        .filter_map(|item| item.worker.and_then(|worker| worker.task_id))
        .filter_map(|task_id| store.get_task(&task_id))
        .any(|history_task| history_task.root_task_id == task.root_task_id)
}

fn require_session_historical_task(
    state: &ApiState,
    session_id_value: Option<&str>,
    requested_scope: SessionScopeKindDto,
    workspace_id_value: Option<&str>,
    workspace_path_value: Option<&str>,
    task_id: &str,
) -> Result<(SessionRequestScope, magi_core::Task), ApiError> {
    let scope = require_task_request_scope(
        state,
        session_id_value,
        requested_scope,
        workspace_id_value,
        workspace_path_value,
    )?;
    let store = state.task_store().ok_or_else(|| {
        ApiError::internal_assembly("session task history guard", "task_store 未配置")
    })?;
    let tid = TaskId::new(task_id);
    let task = store
        .get_task(&tid)
        .ok_or_else(|| ApiError::not_found("任务不存在", task_id))?;
    if !session_history_contains_task(state, store, &scope.session_id, &task) {
        return Err(ApiError::InvalidInput(format!(
            "任务 {} 不属于当前会话 {}",
            task_id, scope.session_id
        )));
    }
    Ok((scope, task))
}

/// 运行视图上对一轮已结束运行的操作。
///
/// “继续”不在这里：任何停止（失败、中断、用户主动停止）后的继续都是会话级的同一个动作，
/// 由会话 Turn 的 `resume` 经 `continuation::decide_continuation` 统一判定和执行，
/// 读模型把可否继续投影给客户端。这里只剩与继续无关的归档。
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentRunActionKind {
    Archive,
}

impl AgentRunActionKind {
    pub(super) fn supports_status(self, status: TaskStatus) -> bool {
        match self {
            Self::Archive => matches!(
                status,
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
            ),
        }
    }
}

pub(super) fn available_agent_run_actions(status: TaskStatus) -> Vec<AgentRunActionKind> {
    [AgentRunActionKind::Archive]
        .into_iter()
        .filter(|action| action.supports_status(status))
        .collect()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskIdRequest {
    action: AgentRunActionKind,
    operation_id: String,
    task_id: String,
    session_id: Option<String>,
    scope: SessionScopeKindDto,
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

async fn perform_task_action(
    State(state): State<ApiState>,
    Json(request): Json<TaskIdRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if request.operation_id.trim().is_empty() {
        return Err(ApiError::InvalidInput("operationId 不能为空".to_string()));
    }
    match request.action {
        AgentRunActionKind::Archive => archive_task(state, request).await,
    }
}

fn ensure_terminal_root_action(
    state: &ApiState,
    root_task_id: &TaskId,
    action: &str,
) -> Result<magi_core::Task, ApiError> {
    let store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly(action, "task_store 未配置"))?;
    let root_task = store
        .get_task(root_task_id)
        .ok_or_else(|| ApiError::not_found("根任务不存在", root_task_id.as_str()))?;
    if matches!(root_task.status, TaskStatus::Running | TaskStatus::Pending) {
        return Err(ApiError::InvalidInput(format!(
            "任务仍在执行中，不能{action}；请先停止当前任务"
        )));
    }
    Ok(root_task)
}

/// 运行视图的 Continue/Archive 是执行链级操作，不能把某个子代理的
/// task_id 当成 root task_id 处理。否则收到子代理的“继续”请求时，
/// 旧实现会静默重启整条主线，造成错误的唤起和重复副作用。
fn ensure_root_action_target(task: &magi_core::Task, action: &str) -> Result<(), ApiError> {
    if task.task_id != task.root_task_id || task.parent_task_id.is_some() {
        return Err(ApiError::InvalidInput(format!(
            "主线{action}入口只接受根任务 taskId；不能把子代理请求转换为主线操作"
        )));
    }
    Ok(())
}

fn ensure_supported_root_action(
    root_task: &magi_core::Task,
    action: AgentRunActionKind,
    action_label: &str,
) -> Result<(), ApiError> {
    if action.supports_status(root_task.status) {
        return Ok(());
    }
    Err(ApiError::InvalidInput(format!(
        "当前任务状态为 {:?}，不能{action_label}",
        root_task.status
    )))
}

/// 从目标面板归档当前 root 任务：只移除会话当前执行链指针，不删除 TaskStore 历史。
async fn archive_task(
    state: ApiState,
    request: TaskIdRequest,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now = UtcMillis::now();
    let task_id = request.task_id.trim();
    if task_id.is_empty() {
        return Err(ApiError::InvalidInput("taskId 不能为空".to_string()));
    }
    let (scope, task) = require_session_historical_task(
        &state,
        request.session_id.as_deref(),
        request.scope,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        task_id,
    )?;
    ensure_root_action_target(&task, "从面板移除")?;
    let session_id = scope.session_id.clone();
    let _session_turn_guard = state.lock_session_turn_commit(&session_id).await;
    let chain = state
        .session_store
        .active_execution_chain(&session_id)
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有可归档的执行链".into()))?;
    let root_task = ensure_terminal_root_action(&state, &task.root_task_id, "从面板移除")?;
    ensure_supported_root_action(&root_task, AgentRunActionKind::Archive, "归档")?;
    state
        .session_store
        .archive_active_execution_chain(
            &session_id,
            &root_task.task_id,
            chain.current_turn_id.as_deref(),
        )
        .map_err(|error| ApiError::internal_assembly("归档任务失败", error))?;
    state.persist_runtime_durable_state_for_sessions_for_api(std::slice::from_ref(&session_id))?;

    let event_id = EventId::unique("event-task-archive");
    let event = EventEnvelope::domain(
        event_id.clone(),
        "task.archive.requested",
        json!({
            "task_id": task_id,
            "root_task_id": root_task.task_id.to_string(),
            "session_id": session_id.to_string(),
            "workspace_id": scope.workspace_id().map(|id| id.to_string()),
            "requested_at": now.0,
        }),
    )
    .with_context(EventContext {
        workspace_id: scope.workspace_id(),
        session_id: Some(session_id.clone()),
        mission_id: Some(root_task.mission_id.clone()),
        task_id: Some(root_task.task_id.clone()),
        ..EventContext::default()
    });
    state.event_bus.publish(event);

    Ok(Json(json!({
        "action": "archive",
        "operationId": request.operation_id,
        "archived": true,
        "sessionId": session_id.to_string(),
        "rootTaskId": root_task.task_id.to_string(),
        "eventId": event_id.to_string(),
        "requestedAt": now.0,
        "workspaceId": scope.workspace_id().map(|id| id.to_string()),
        "workspacePath": scope.workspace_path(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{
        AccessProfile, MissionId, Task, TaskExecutorBinding, TaskKind, TaskPolicy,
        TaskRuntimePayload, TaskStatus, TaskTier,
    };

    fn terminal_root_task_with_binding(binding: TaskExecutorBinding) -> Task {
        Task {
            task_id: TaskId::new("task-root-restart-skill"),
            mission_id: MissionId::new("mission-restart-skill"),
            root_task_id: TaskId::new("task-root-restart-skill"),
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: "重启 skill 任务".to_string(),
            goal: "重启时保留 active skill".to_string(),
            status: TaskStatus::Completed,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: Some(TaskPolicy {
                autonomy_level: "Autonomous".to_string(),
                access_profile: AccessProfile::Restricted,
                allowed_tools: Vec::new(),
                denied_tools: Vec::new(),
                allowed_paths: Vec::new(),
                denied_paths: Vec::new(),
                read_only_paths: Vec::new(),
                network_mode: "full".to_string(),
                command_mode: "full".to_string(),
                retry_limit: 1,
                validation_profile: None,
                checkpoint_mode: "turn".to_string(),
                task_tier: TaskTier::ExecutionChain,
                background_allowed: false,
                escalation_conditions: Vec::new(),
            }),
            executor_binding: Some(binding),
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: TaskRuntimePayload::None,
            created_at: UtcMillis(1),
            updated_at: UtcMillis(2),
        }
    }

    #[test]
    fn archive_is_the_only_run_action_and_only_for_ended_runs() {
        assert_eq!(
            available_agent_run_actions(TaskStatus::Running),
            Vec::<AgentRunActionKind>::new()
        );
        for status in [
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Killed,
        ] {
            assert_eq!(
                available_agent_run_actions(status),
                vec![AgentRunActionKind::Archive]
            );
        }
    }

    /// “继续”不再是运行视图的动作：它是会话级的同一个动作，线上不得再接受这个词。
    #[test]
    fn continue_and_restart_are_not_run_wire_actions() {
        for removed in ["continue", "restart"] {
            assert!(serde_json::from_str::<AgentRunActionKind>(&format!("\"{removed}\"")).is_err());
        }
        assert_eq!(
            serde_json::from_str::<AgentRunActionKind>("\"archive\"").unwrap(),
            AgentRunActionKind::Archive
        );
    }

    #[test]
    fn root_actions_reject_child_tasks_before_resolving_or_mutating_the_root() {
        let mut child = terminal_root_task_with_binding(TaskExecutorBinding::for_role("executor"));
        child.task_id = TaskId::new("task-child");
        child.root_task_id = TaskId::new("task-root");
        child.parent_task_id = Some(TaskId::new("task-root"));
        let error = ensure_root_action_target(&child, "从面板移除").unwrap_err();
        assert!(matches!(error, ApiError::InvalidInput(message) if message.contains("根任务")));
    }
}
