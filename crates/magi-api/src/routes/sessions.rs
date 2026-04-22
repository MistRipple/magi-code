use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use magi_core::{DomainError, EventId, SessionId, UtcMillis, WorkerId};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_session_store::SessionRecord;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    dto::{BootstrapDto, SessionNotificationsResponseDto},
    errors::ApiError,
    state::ApiState,
    task_execution::continue_shadow_execution_chain,
};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/session/new", post(create_session))
        .route("/session/continue", post(continue_session))
        .route("/session/switch", post(switch_session))
        .route("/session/delete", post(delete_session))
        .route("/session/rename", post(rename_session))
        .route("/session/close", post(close_session))
        .route("/session/save", post(save_session))
        .route("/session/notifications", get(get_notifications))
        .route("/session/notifications/mark-all-read", post(mark_all_notifications_read))
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

async fn create_session(
    State(state): State<ApiState>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<Json<SessionSelectionResponseDto>, ApiError> {
    let session_id = super::new_session_id();
    let workspace_id = request.workspace_id.filter(|s| !s.is_empty());
    state
        .session_store
        .create_session_for_workspace(session_id, "新会话", workspace_id)
        .map_err(|e| ApiError::internal_assembly("创建会话失败", e))?;
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SwitchSessionRequest {
    session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContinueSessionRequest {
    session_id: String,
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
    let requested_worker_ids = request
        .requested_worker_ids
        .into_iter()
        .map(|worker_id| worker_id.trim().to_string())
        .filter(|worker_id| !worker_id.is_empty())
        .map(WorkerId::new)
        .collect::<Vec<_>>();
    let continued_at = UtcMillis::now();
    let accepted = continue_shadow_execution_chain(&state, &session_id, &requested_worker_ids)?;
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
    Ok(Json(build_notifications_response(&state, session_id.as_ref())))
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
    Ok(Json(build_notifications_response(&state, Some(&session_id))))
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
    Ok(Json(build_notifications_response(&state, Some(&session_id))))
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
    Ok(Json(build_notifications_response(&state, Some(&session_id))))
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
    Ok(state.session_store.current_session().map(|session| session.session_id))
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
