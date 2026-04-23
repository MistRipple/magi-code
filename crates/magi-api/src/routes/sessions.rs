use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_bridge_client::{ModelInvocationRequest, SHADOW_MODEL_PROVIDER};
use magi_core::TaskStatus;
use magi_core::{DomainError, EventId, SessionId, UtcMillis, WorkerId};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_session_store::{SessionRecord, TimelineEntryKind};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{Duration, Instant};

use super::append_session_user_message;
use crate::{
    dto::{BootstrapDto, SessionNotificationsResponseDto},
    errors::ApiError,
    state::ApiState,
    task_execution::continue_shadow_execution_chain,
};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/session/new", post(create_session))
        .route("/session/chat", post(chat_session))
        .route("/session/continue", post(continue_session))
        .route("/session/switch", post(switch_session))
        .route("/session/delete", post(delete_session))
        .route("/session/rename", post(rename_session))
        .route("/session/close", post(close_session))
        .route("/session/save", post(save_session))
        .route("/session/notifications", get(get_notifications))
        .route(
            "/session/notifications/mark-all-read",
            post(mark_all_notifications_read),
        )
        .route("/session/notifications/clear", post(clear_notifications))
        .route("/session/notifications/remove", post(remove_notification))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteSessionRequest {
    session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionRequest {
    workspace_id: Option<String>,
    #[allow(dead_code)]
    workspace_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatSessionRequest {
    session_id: Option<String>,
    workspace_id: Option<String>,
    text: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatSessionResponseDto {
    session_id: String,
    entry_id: String,
    event_id: String,
    accepted_at: UtcMillis,
    created_session: bool,
}

async fn create_session(
    State(state): State<ApiState>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<Json<SessionSelectionResponseDto>, ApiError> {
    let session_id = super::new_session_id();
    let workspace_id = request.workspace_id.filter(|s| !s.is_empty());
    let created_session = state
        .session_store
        .create_session_for_workspace(session_id, "新会话", workspace_id)
        .map_err(|e| ApiError::internal_assembly("创建会话失败", e))?;
    state.persist_session_durable_state()?;
    Ok(Json(SessionSelectionResponseDto {
        session_id: created_session.session_id.to_string(),
        current_session: Some(created_session),
    }))
}

async fn chat_session(
    State(state): State<ApiState>,
    Json(request): Json<ChatSessionRequest>,
) -> Result<Json<ChatSessionResponseDto>, ApiError> {
    let text = request
        .text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("普通对话内容不能为空".to_string()))?;
    let accepted_at = super::monotonic_accepted_at();
    let requested_session_id = request
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .map(SessionId::new);
    let requested_workspace_id = request
        .workspace_id
        .as_deref()
        .map(str::trim)
        .filter(|workspace_id| !workspace_id.is_empty())
        .map(str::to_string);
    let response_text = invoke_plain_chat(&state, text)?;
    let (session_id, created_session) =
        resolve_chat_session(&state, requested_session_id, requested_workspace_id, text)?;

    append_session_user_message(&state, &session_id, accepted_at, text);
    append_session_assistant_message(&state, &session_id, accepted_at, &response_text);
    state.persist_session_durable_state()?;

    let event_id = EventId::new(format!("event-session-chat-{}", accepted_at.0));
    state
        .event_bus
        .publish(
            EventEnvelope::domain(
                event_id.clone(),
                "session.chat.completed",
                json!({
                    "session_id": session_id.to_string(),
                    "created_session": created_session,
                }),
            )
            .with_context(EventContext {
                session_id: Some(session_id.clone()),
                ..EventContext::default()
            }),
        )
        .map_err(|err| ApiError::event_publish_failed("普通对话事件发布失败", err))?;

    Ok(Json(ChatSessionResponseDto {
        session_id: session_id.to_string(),
        entry_id: format!("timeline-{}-{}", session_id, accepted_at.0),
        event_id: event_id.to_string(),
        accepted_at,
        created_session,
    }))
}

fn invoke_plain_chat(state: &ApiState, text: &str) -> Result<String, ApiError> {
    let client = state
        .model_bridge_client()
        .ok_or_else(|| ApiError::internal_assembly("普通对话失败", "model bridge 未配置"))?;
    let response = client
        .invoke(ModelInvocationRequest {
            provider: SHADOW_MODEL_PROVIDER.to_string(),
            prompt: text.to_string(),
            messages: None,
            tools: None,
        })
        .map_err(|error| ApiError::internal_assembly("普通对话模型调用失败", error))?;
    let payload = response.payload.trim();
    if !response.ok || payload.is_empty() {
        return Err(ApiError::internal_assembly(
            "普通对话模型调用失败",
            "模型返回空内容",
        ));
    }
    Ok(payload.to_string())
}

fn resolve_chat_session(
    state: &ApiState,
    requested_session_id: Option<SessionId>,
    requested_workspace_id: Option<String>,
    title_seed: &str,
) -> Result<(SessionId, bool), ApiError> {
    if let Some(session_id) = requested_session_id {
        state
            .session_store
            .session(&session_id)
            .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
        return Ok((session_id, false));
    }

    if let Some(current_session) = state.session_store.current_session() {
        if let Some(requested_workspace_id) = requested_workspace_id.as_deref() {
            if current_session.workspace_id.as_deref() != Some(requested_workspace_id) {
                let session_id = super::new_session_id();
                state
                    .session_store
                    .create_session_for_workspace(
                        session_id.clone(),
                        chat_session_title(title_seed),
                        Some(requested_workspace_id.to_string()),
                    )
                    .map_err(|err| ApiError::internal_assembly("创建普通对话会话失败", err))?;
                return Ok((session_id, true));
            }
        }
        return Ok((current_session.session_id, false));
    }

    let session_id = super::new_session_id();
    state
        .session_store
        .create_session_for_workspace(
            session_id.clone(),
            chat_session_title(title_seed),
            requested_workspace_id,
        )
        .map_err(|err| ApiError::internal_assembly("创建普通对话会话失败", err))?;
    Ok((session_id, true))
}

fn chat_session_title(text: &str) -> String {
    let title = text.chars().take(80).collect::<String>().trim().to_string();
    if title.is_empty() {
        "新会话".to_string()
    } else {
        title
    }
}

fn append_session_assistant_message(
    state: &ApiState,
    session_id: &SessionId,
    accepted_at: UtcMillis,
    message: &str,
) {
    state.session_store.append_timeline_entry(
        session_id.clone(),
        TimelineEntryKind::AssistantMessage,
        message.to_string(),
    );

    let _ = state.event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-message-assistant-{}", accepted_at.0)),
            "message.created",
            json!({
                "session_id": session_id.to_string(),
                "role": "assistant",
                "content": message,
            }),
        )
        .with_context(EventContext {
            session_id: Some(session_id.clone()),
            ..EventContext::default()
        }),
    );
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SwitchSessionRequest {
    session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContinueSessionRequest {
    session_id: String,
    prompt_text: Option<String>,
    #[serde(default)]
    requested_worker_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ContinueSessionResponseDto {
    session_id: String,
    mission_id: String,
    root_task_id: String,
    execution_chain_ref: String,
    resumed_branch_count: usize,
    status: String,
    runner_started: bool,
    event_id: String,
    continued_at: UtcMillis,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionSelectionResponseDto {
    session_id: String,
    current_session: Option<SessionRecord>,
}

async fn switch_session(
    State(state): State<ApiState>,
    Json(request): Json<SwitchSessionRequest>,
) -> Result<Json<SessionSelectionResponseDto>, ApiError> {
    let session_id = SessionId::new(&request.session_id);
    state
        .session_store
        .switch_session(&session_id)
        .map_err(|e| ApiError::internal_assembly("切换会话失败", e))?;
    state.persist_session_durable_state()?;
    let current_session = state.session_store.current_session();
    Ok(Json(SessionSelectionResponseDto {
        session_id: current_session
            .as_ref()
            .map(|session| session.session_id.to_string())
            .unwrap_or_default(),
        current_session,
    }))
}

async fn continue_session(
    State(state): State<ApiState>,
    Json(request): Json<ContinueSessionRequest>,
) -> Result<Json<ContinueSessionResponseDto>, ApiError> {
    let session_id = SessionId::new(&request.session_id);
    let prompt_text = request
        .prompt_text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let requested_worker_ids = request
        .requested_worker_ids
        .into_iter()
        .map(|worker_id| worker_id.trim().to_string())
        .filter(|worker_id| !worker_id.is_empty())
        .map(WorkerId::new)
        .collect::<Vec<_>>();
    let continued_at = UtcMillis::now();
    let accepted = continue_shadow_execution_chain(&state, &session_id, &requested_worker_ids)?;
    spawn_continue_session_finalize(state.clone(), accepted.clone(), continued_at);
    if let Some(prompt_text) = prompt_text.as_deref() {
        append_session_user_message(&state, &session_id, continued_at, prompt_text);
    }
    state.persist_runtime_durable_state()?;
    let event_id = EventId::new(format!("event-session-continue-{}", continued_at.0));
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.continue.executed",
        json!({
            "session_id": accepted.session_id.to_string(),
            "mission_id": accepted.mission_id.to_string(),
            "root_task_id": accepted.root_task_id.to_string(),
            "execution_chain_ref": accepted.execution_chain_ref,
            "resumed_branch_count": accepted.resumed_branch_count,
            "runner_started": accepted.runner_started,
        }),
    )
    .with_context(EventContext {
        session_id: Some(accepted.session_id.clone()),
        mission_id: Some(accepted.mission_id.clone()),
        task_id: Some(accepted.root_task_id.clone()),
        ..EventContext::default()
    });
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("会话继续事件发布失败", err))?;
    Ok(Json(ContinueSessionResponseDto {
        session_id: accepted.session_id.to_string(),
        mission_id: accepted.mission_id.to_string(),
        root_task_id: accepted.root_task_id.to_string(),
        execution_chain_ref: accepted.execution_chain_ref,
        resumed_branch_count: accepted.resumed_branch_count,
        status: "continued".to_string(),
        runner_started: accepted.runner_started,
        event_id: event_id.to_string(),
        continued_at,
    }))
}

fn spawn_continue_session_finalize(
    state: ApiState,
    accepted: crate::task_execution::SessionContinueAccepted,
    continued_at: UtcMillis,
) {
    tokio::task::spawn_blocking(move || {
        let Some(task_store) = state.task_store() else {
            return;
        };

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let Some(task) = task_store.get_task(&accepted.action_task_id) else {
                return;
            };
            if matches!(
                task.status,
                TaskStatus::Completed
                    | TaskStatus::Failed
                    | TaskStatus::Cancelled
                    | TaskStatus::Blocked
                    | TaskStatus::Skipped
            ) {
                break;
            }
            if Instant::now() >= deadline {
                tracing::warn!(
                    session_id = %accepted.session_id,
                    root_task_id = %accepted.root_task_id,
                    action_task_id = %accepted.action_task_id,
                    "session continue finalize timed out waiting for action task terminal state"
                );
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        super::append_dispatch_assistant_message(
            &state,
            &crate::task_execution::DispatchSubmissionAccepted {
                session_id: accepted.session_id.clone(),
                entry_id: format!("timeline-{}-{}", accepted.session_id, continued_at.0),
                accepted_at: continued_at,
                created_session: false,
                root_task_id: accepted.root_task_id.clone(),
                action_task_id: accepted.action_task_id.clone(),
                runner_started: accepted.runner_started,
            },
        );

        if let Err(error) = state.persist_session_durable_state() {
            tracing::error!(
                session_id = %accepted.session_id,
                root_task_id = %accepted.root_task_id,
                action_task_id = %accepted.action_task_id,
                ?error,
                "session continue finalize persist failed"
            );
        }
    });
}

async fn delete_session(
    State(state): State<ApiState>,
    Json(request): Json<DeleteSessionRequest>,
) -> Result<Json<BootstrapDto>, ApiError> {
    let session_id = SessionId::new(&request.session_id);
    state
        .session_store
        .delete_session(&session_id)
        .map_err(|e| ApiError::internal_assembly("删除会话失败", e))?;
    state.persist_session_durable_state()?;
    Ok(Json(state.bootstrap_dto()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameSessionRequest {
    session_id: String,
    name: String,
}

async fn rename_session(
    State(state): State<ApiState>,
    Json(request): Json<RenameSessionRequest>,
) -> Result<Json<BootstrapDto>, ApiError> {
    let session_id = SessionId::new(&request.session_id);
    state
        .session_store
        .rename_session(&session_id, &request.name)
        .map_err(|e| ApiError::internal_assembly("重命名会话失败", e))?;
    state.persist_session_durable_state()?;
    Ok(Json(state.bootstrap_dto()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloseSessionRequest {
    session_id: String,
}

async fn close_session(
    State(state): State<ApiState>,
    Json(request): Json<CloseSessionRequest>,
) -> Result<Json<BootstrapDto>, ApiError> {
    let session_id = SessionId::new(&request.session_id);
    state
        .session_store
        .archive_session(&session_id)
        .map_err(|e| ApiError::internal_assembly("关闭会话失败", e))?;
    state.persist_session_durable_state()?;
    Ok(Json(state.bootstrap_dto()))
}

async fn save_session(State(state): State<ApiState>) -> Result<Json<BootstrapDto>, ApiError> {
    state.persist_session_durable_state()?;
    Ok(Json(state.bootstrap_dto()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificationsQuery {
    session_id: Option<String>,
}

impl NotificationsQuery {
    fn requested_session_id(&self) -> Option<SessionId> {
        parse_requested_session_id(self.session_id.as_deref())
    }
}

async fn get_notifications(
    State(state): State<ApiState>,
    Query(query): Query<NotificationsQuery>,
) -> Result<Json<SessionNotificationsResponseDto>, ApiError> {
    let session_id = resolve_notifications_session_id(&state, query.requested_session_id())?;
    Ok(Json(build_notifications_response(
        &state,
        session_id.as_ref(),
    )))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificationScopeRequest {
    session_id: Option<String>,
}

impl NotificationScopeRequest {
    fn requested_session_id(&self) -> Option<SessionId> {
        parse_requested_session_id(self.session_id.as_deref())
    }
}

async fn mark_all_notifications_read(
    State(state): State<ApiState>,
    Json(request): Json<NotificationScopeRequest>,
) -> Result<Json<SessionNotificationsResponseDto>, ApiError> {
    let session_id = require_notifications_session_id(&state, request.requested_session_id())?;
    state
        .session_store
        .mark_notifications_handled_for_session(&session_id);
    state.persist_session_durable_state()?;
    Ok(Json(build_notifications_response(
        &state,
        Some(&session_id),
    )))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClearNotificationsRequest {
    session_id: Option<String>,
}

impl ClearNotificationsRequest {
    fn requested_session_id(&self) -> Option<SessionId> {
        parse_requested_session_id(self.session_id.as_deref())
    }
}

async fn clear_notifications(
    State(state): State<ApiState>,
    Json(request): Json<ClearNotificationsRequest>,
) -> Result<Json<SessionNotificationsResponseDto>, ApiError> {
    let session_id = require_notifications_session_id(&state, request.requested_session_id())?;
    state
        .session_store
        .clear_notifications_for_session(&session_id);
    state.persist_session_durable_state()?;
    Ok(Json(build_notifications_response(
        &state,
        Some(&session_id),
    )))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveNotificationRequest {
    session_id: Option<String>,
    notification_id: String,
}

impl RemoveNotificationRequest {
    fn requested_session_id(&self) -> Option<SessionId> {
        parse_requested_session_id(self.session_id.as_deref())
    }

    fn requested_notification_id(&self) -> Option<String> {
        trimmed_non_empty(Some(self.notification_id.as_str())).map(str::to_string)
    }
}

async fn remove_notification(
    State(state): State<ApiState>,
    Json(request): Json<RemoveNotificationRequest>,
) -> Result<Json<SessionNotificationsResponseDto>, ApiError> {
    let session_id = require_notifications_session_id(&state, request.requested_session_id())?;
    let notification_id = request
        .requested_notification_id()
        .ok_or_else(|| ApiError::InvalidInput("notification_id 不能为空".to_string()))?;
    state
        .session_store
        .remove_notification_for_session(&session_id, &notification_id)
        .map_err(|error| match error {
            DomainError::NotFound { .. } => ApiError::not_found("通知不存在", &notification_id),
            other => ApiError::internal_assembly("移除通知失败", other),
        })?;
    state.persist_session_durable_state()?;
    Ok(Json(build_notifications_response(
        &state,
        Some(&session_id),
    )))
}

fn build_notifications_response(
    state: &ApiState,
    session_id: Option<&SessionId>,
) -> SessionNotificationsResponseDto {
    match session_id {
        Some(session_id) => SessionNotificationsResponseDto::from_records(
            session_id,
            state.session_store.notifications_for_session(session_id),
        ),
        None => SessionNotificationsResponseDto::empty(None),
    }
}

fn resolve_notifications_session_id(
    state: &ApiState,
    requested_session_id: Option<SessionId>,
) -> Result<Option<SessionId>, ApiError> {
    if let Some(session_id) = requested_session_id {
        if state.session_store.session(&session_id).is_none() {
            return Err(ApiError::session_not_found(session_id.as_str()));
        }
        return Ok(Some(session_id));
    }
    Ok(state
        .session_store
        .current_session()
        .map(|session| session.session_id))
}

fn require_notifications_session_id(
    state: &ApiState,
    requested_session_id: Option<SessionId>,
) -> Result<SessionId, ApiError> {
    resolve_notifications_session_id(state, requested_session_id)?
        .ok_or_else(|| ApiError::InvalidInput("当前没有活动 session".to_string()))
}

fn parse_requested_session_id(value: Option<&str>) -> Option<SessionId> {
    trimmed_non_empty(value).map(SessionId::new)
}

fn trimmed_non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
