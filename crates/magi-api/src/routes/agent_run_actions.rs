use axum::{Json, Router, extract::State, routing::post};
use magi_core::{EventId, SessionId, TaskId, TaskStatus, TaskTier, UtcMillis};
use magi_event_bus::{EventContext, EventEnvelope};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    dispatch_flow::{accept_session_task_submission, finalize_session_task_dispatch},
    session_scope::{SessionWorkspaceScope, require_session_workspace_scope},
};
use crate::{dto::SessionTurnRequestDto, errors::ApiError, state::ApiState};
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::{ActiveExecutionTurn, SessionPlan};

pub fn routes() -> Router<ApiState> {
    Router::new().route("/agent-runs/action", post(perform_task_action))
}

fn require_session_owned_task(
    state: &ApiState,
    session_id_value: Option<&str>,
    workspace_id_value: Option<&str>,
    workspace_path_value: Option<&str>,
    task_id: &str,
) -> Result<(SessionWorkspaceScope, magi_core::Task), ApiError> {
    let scope = require_task_request_scope(
        state,
        session_id_value,
        workspace_id_value,
        workspace_path_value,
    )?;
    let ownership = state
        .session_store
        .execution_ownership(&scope.session_id)
        .ok_or_else(|| ApiError::session_not_found(scope.session_id.as_str()))?;
    let mission_id = ownership
        .mission_id
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有活跃任务".to_string()))?;
    let store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("session task guard", "task_store 未配置"))?;
    let tid = TaskId::new(task_id);
    let task = store
        .get_task(&tid)
        .ok_or_else(|| ApiError::not_found("任务不存在", task_id))?;
    if task.mission_id != mission_id {
        return Err(ApiError::InvalidInput(format!(
            "任务 {} 不属于当前会话 {}",
            task_id, scope.session_id
        )));
    }
    Ok((scope, task))
}

fn require_task_request_scope(
    state: &ApiState,
    session_id_value: Option<&str>,
    workspace_id_value: Option<&str>,
    workspace_path_value: Option<&str>,
) -> Result<SessionWorkspaceScope, ApiError> {
    require_session_workspace_scope(
        state,
        session_id_value,
        workspace_id_value,
        workspace_path_value,
        "执行任务操作",
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
    workspace_id_value: Option<&str>,
    workspace_path_value: Option<&str>,
    task_id: &str,
) -> Result<(SessionWorkspaceScope, magi_core::Task), ApiError> {
    let scope = require_task_request_scope(
        state,
        session_id_value,
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

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentRunActionKind {
    Continue,
    Restart,
    Archive,
}

impl AgentRunActionKind {
    pub(super) fn supports_status(self, status: TaskStatus) -> bool {
        match self {
            Self::Continue => status == TaskStatus::Failed,
            Self::Restart => matches!(status, TaskStatus::Failed | TaskStatus::Killed),
            Self::Archive => matches!(
                status,
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
            ),
        }
    }
}

pub(super) fn available_agent_run_actions(
    status: TaskStatus,
    has_recoverable_chain: bool,
) -> Vec<AgentRunActionKind> {
    [
        AgentRunActionKind::Continue,
        AgentRunActionKind::Restart,
        AgentRunActionKind::Archive,
    ]
    .into_iter()
    .filter(|action| {
        action.supports_status(status)
            && (*action != AgentRunActionKind::Continue || has_recoverable_chain)
    })
    .collect()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskIdRequest {
    action: AgentRunActionKind,
    operation_id: String,
    task_id: String,
    session_id: Option<String>,
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
        AgentRunActionKind::Continue => continue_task(state, request).await,
        AgentRunActionKind::Restart => restart_task(state, request).await,
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

fn restart_active_skill_id(root_task: &magi_core::Task) -> Option<String> {
    root_task
        .executor_binding_active_skill_id()
        .map(str::to_string)
}

fn resume_blocked_plan_for_retry(
    state: &ApiState,
    session_id: &SessionId,
    workspace_id: &magi_core::WorkspaceId,
    root_task_id: &TaskId,
) -> Result<Option<SessionPlan>, ApiError> {
    let plan_store = magi_plan::PlanStore::from_store(&state.session_store, session_id.clone());
    let Some(previous_plan) = plan_store.snapshot() else {
        return Ok(None);
    };
    let Some(item_id) = previous_plan.task_bindings.get(root_task_id) else {
        return Ok(None);
    };
    if !previous_plan
        .items
        .iter()
        .any(|item| &item.item_id == item_id && item.status == magi_core::PlanItemStatus::Blocked)
    {
        return Ok(None);
    }
    let resumed = plan_store
        .resume()
        .map_err(|error| ApiError::InvalidInput(error.to_string()))?
        .ok_or_else(|| ApiError::internal_assembly("恢复阻塞计划失败", "plan disappeared"))?;
    let root_task = state
        .task_store()
        .and_then(|store| store.get_task(root_task_id))
        .ok_or_else(|| ApiError::not_found("根任务不存在", root_task_id.as_str()))?;
    magi_plan::publish_plan_event(
        &state.event_bus,
        magi_plan::plan_event_type(&resumed),
        &resumed,
        Some(workspace_id),
        Some(root_task_id),
        Some(&root_task.mission_id),
    );
    Ok(Some(previous_plan))
}

fn restore_plan_after_retry_rejection(
    state: &ApiState,
    session_id: &SessionId,
    previous_plan: Option<SessionPlan>,
) {
    let Some(previous_plan) = previous_plan else {
        return;
    };
    let expected_revision = state
        .session_store
        .plan(session_id)
        .map(|plan| plan.revision);
    if let Err(error) =
        state
            .session_store
            .upsert_plan(session_id, previous_plan, expected_revision)
    {
        tracing::error!(
            session_id = %session_id,
            ?error,
            "重新执行任务接收失败后还原计划状态失败"
        );
    }
}

async fn continue_task(
    state: ApiState,
    request: TaskIdRequest,
) -> Result<Json<serde_json::Value>, ApiError> {
    let task_id = request.task_id.trim();
    if task_id.is_empty() {
        return Err(ApiError::InvalidInput("taskId 不能为空".to_string()));
    }
    let (scope, task) = require_session_owned_task(
        &state,
        request.session_id.as_deref(),
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        task_id,
    )?;
    let root_task = ensure_terminal_root_action(&state, &task.root_task_id, "继续")?;
    ensure_supported_root_action(&root_task, AgentRunActionKind::Continue, "继续")?;
    let response =
        super::sessions::continue_agent_run(&state, &scope.session_id, &scope.workspace_id).await?;
    let mut payload = serde_json::to_value(response)
        .map_err(|error| ApiError::internal_assembly("序列化任务继续结果失败", error))?;
    payload["action"] = json!("continue");
    payload["operationId"] = json!(request.operation_id);
    Ok(Json(payload))
}

/// 重新执行当前 root 任务：创建新的执行链，旧任务树只保留为历史事实。
async fn restart_task(
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
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        task_id,
    )?;
    let session_id = scope.session_id.clone();
    let root_task = ensure_terminal_root_action(&state, &task.root_task_id, "重新执行")?;
    ensure_supported_root_action(&root_task, AgentRunActionKind::Restart, "重新执行")?;
    let previous_plan = resume_blocked_plan_for_retry(
        &state,
        &session_id,
        &scope.workspace_id,
        &root_task.task_id,
    )?;
    let restart_text = if root_task.goal.trim().is_empty() {
        root_task.title.trim().to_string()
    } else {
        root_task.goal.trim().to_string()
    };
    let restart_request = SessionTurnRequestDto {
        session_id: Some(session_id.to_string()),
        workspace_id: Some(scope.workspace_id.to_string()),
        workspace_path: Some(scope.workspace_path.clone()),
        text: Some(restart_text),
        skill_name: restart_active_skill_id(&root_task),
        locale: None,
        goal_mode: false,
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: root_task
            .policy_snapshot
            .as_ref()
            .map(|policy| policy.access_profile),
        orchestrator_session_config: None,
        request_id: Some(request.operation_id.clone()),
        user_message_id: None,
        placeholder_message_id: None,
        steer_current_turn: false,
        expected_turn_id: None,
        replace_turn_id: None,
    };
    let task_tier = root_task
        .policy_snapshot
        .as_ref()
        .map(|policy| policy.task_tier)
        .unwrap_or(TaskTier::ExecutionChain);
    let accepted_result = accept_session_task_submission(
        &state,
        &restart_request,
        Vec::new(),
        scope.workspace_id.clone(),
        Some(root_task.title.clone()),
        Some(root_task.goal.clone()),
        task_tier,
    )
    .await;
    let (accepted, event_id) = match accepted_result {
        Ok(accepted) => accepted,
        Err(error) => {
            restore_plan_after_retry_rejection(&state, &session_id, previous_plan);
            return Err(error);
        }
    };
    finalize_session_task_dispatch(state.clone(), accepted.clone()).await;
    let execution_chain_ref = state
        .session_store
        .runtime_sidecar(&accepted.session_id)
        .and_then(|sidecar| sidecar.ownership.execution_chain_ref);
    let event = EventEnvelope::domain(
        EventId::new(format!("event-task-restart-{}", now.0)),
        "task.restart.requested",
        json!({
            "taskId": task_id,
            "oldRootTaskId": task.root_task_id.to_string(),
            "newRootTaskId": accepted.root_task_id.to_string(),
            "sessionId": accepted.session_id.to_string(),
            "workspaceId": scope.workspace_id.to_string(),
            "requestedAt": now.0,
        }),
    )
    .with_context(EventContext {
        workspace_id: Some(scope.workspace_id.clone()),
        session_id: Some(accepted.session_id.clone()),
        mission_id: Some(root_task.mission_id.clone()),
        task_id: Some(accepted.root_task_id.clone()),
        ..EventContext::default()
    });
    state.event_bus.publish(event);

    Ok(Json(json!({
        "action": "restart",
        "operationId": request.operation_id,
        "restarted": true,
        "sessionId": accepted.session_id.to_string(),
        "entryId": accepted.entry_id,
        "eventId": event_id.to_string(),
        "acceptedAt": accepted.accepted_at.0,
        "createdSession": accepted.created_session,
        "rootTaskId": accepted.root_task_id.to_string(),
        "actionTaskId": accepted.action_task_id.to_string(),
        "executionChainRef": execution_chain_ref,
        "requestedAt": now.0,
        "workspaceId": scope.workspace_id.to_string(),
        "workspacePath": scope.workspace_path,
        "oldRootTaskId": task.root_task_id.to_string(),
        "newRootTaskId": accepted.root_task_id.to_string(),
    })))
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
    let (scope, task) = require_session_owned_task(
        &state,
        request.session_id.as_deref(),
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        task_id,
    )?;
    let session_id = scope.session_id.clone();
    let root_task = ensure_terminal_root_action(&state, &task.root_task_id, "从面板移除")?;
    ensure_supported_root_action(&root_task, AgentRunActionKind::Archive, "归档")?;
    state
        .session_store
        .archive_active_execution_chain(&session_id, &root_task.task_id)
        .map_err(|error| ApiError::internal_assembly("归档任务失败", error))?;
    state.persist_runtime_durable_state_for_api()?;

    let event_id = EventId::new(format!("event-task-archive-{}", now.0));
    let event = EventEnvelope::domain(
        event_id.clone(),
        "task.archive.requested",
        json!({
            "taskId": task_id,
            "rootTaskId": root_task.task_id.to_string(),
            "sessionId": session_id.to_string(),
            "workspaceId": scope.workspace_id.to_string(),
            "requestedAt": now.0,
        }),
    )
    .with_context(EventContext {
        workspace_id: Some(scope.workspace_id.clone()),
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
        "workspaceId": scope.workspace_id.to_string(),
        "workspacePath": scope.workspace_path,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{
        AccessProfile, MissionId, Task, TaskExecutorBinding, TaskKind, TaskPolicy,
        TaskRuntimePayload, TaskStatus,
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
    fn restart_uses_active_skill_id_not_legacy_skill_name_binding() {
        let task = terminal_root_task_with_binding(
            TaskExecutorBinding::for_role("reviewer")
                .with_active_skill_id(Some("code-review".to_string())),
        );

        assert_eq!(
            restart_active_skill_id(&task).as_deref(),
            Some("code-review")
        );
    }

    #[test]
    fn action_policy_is_shared_by_projection_and_execution() {
        assert_eq!(
            available_agent_run_actions(TaskStatus::Running, false),
            Vec::<AgentRunActionKind>::new()
        );
        assert_eq!(
            available_agent_run_actions(TaskStatus::Failed, false),
            vec![AgentRunActionKind::Restart, AgentRunActionKind::Archive]
        );
        assert_eq!(
            available_agent_run_actions(TaskStatus::Failed, true),
            vec![
                AgentRunActionKind::Continue,
                AgentRunActionKind::Restart,
                AgentRunActionKind::Archive,
            ]
        );
        assert_eq!(
            available_agent_run_actions(TaskStatus::Completed, false),
            vec![AgentRunActionKind::Archive]
        );
        assert!(!AgentRunActionKind::Restart.supports_status(TaskStatus::Completed));
    }
}
