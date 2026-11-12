use axum::{
    Json, Router,
    extract::{Query, State},
    http::HeaderMap,
    routing::{get, post},
};
use magi_conversation_runtime::{
    PendingToolApproval, PendingUserQuestion, ToolApprovalDecision, UserQuestionResponse,
};
#[cfg(test)]
use magi_core::AccessProfile;
use magi_core::{DomainError, EventId, SessionId, UtcMillis, WorkspaceId, public_runtime_excerpt};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_session_store::{NotificationContext, NotificationRecord, NotificationScope};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::scope_binding::{
    SessionScope, parse_session_id, require_session_record_in_scope,
    resolve_existing_session_scope, resolve_explicit_session_scope, resolve_session_scope,
};
use crate::{
    dto::{
        BootstrapDto, ContinueSessionRequest, ContinueSessionResponseDto, NotificationsResponseDto,
        SessionInterruptResponseDto, SessionScopeKindDto, SessionTurnRequestDto,
        SessionTurnResponseDto,
    },
    errors::ApiError,
    state::ApiState,
    turn_service::trimmed_non_empty,
};

static INCIDENT_NOTIFICATION_COUNTER: AtomicU64 = AtomicU64::new(1);
static SESSION_VIEW_EVENT_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/session/materialize", post(materialize_session))
        .route("/session/turn", post(submit_session_turn))
        .route("/session/queue", get(get_session_turn_queue))
        .route(
            "/session/queue/update",
            post(update_session_turn_queue_item),
        )
        .route(
            "/session/queue/remove",
            post(remove_session_turn_queue_item),
        )
        .route("/session/queue/guide", post(guide_session_turn_queue_item))
        .route("/session/queue/retry", post(retry_session_turn_queue_item))
        .route("/session/interrupt", post(interrupt_session_turn))
        .route("/session/tool-approvals", get(get_session_tool_approvals))
        .route(
            "/session/tool-approval",
            post(resolve_session_tool_approval),
        )
        .route("/session/user-questions", get(get_session_user_questions))
        .route(
            "/session/user-question",
            post(resolve_session_user_question),
        )
        .route("/session/continue", post(continue_session))
        .route("/session/navigation", post(navigate_session))
        .route("/session/delete", post(delete_session))
        .route("/session/rename", post(rename_session))
        .route("/session/close", post(close_session))
        .route("/session/viewed", post(mark_session_viewed))
        .route("/notifications", get(get_notifications))
        .route("/notifications/report", post(report_incident))
        .route(
            "/notifications/mark-all-read",
            post(mark_all_notifications_read),
        )
        .route("/notifications/clear", post(clear_notifications))
        .route("/notifications/resolve", post(resolve_notification))
        .route("/notifications/remove", post(remove_notification))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionToolApprovalScope {
    session_id: String,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

impl SessionToolApprovalScope {
    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }

    fn requested_workspace_path(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_path.as_deref())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolveSessionToolApprovalRequest {
    #[serde(flatten)]
    scope: SessionToolApprovalScope,
    approval_id: String,
    decision: ToolApprovalDecision,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionToolApprovalsResponse {
    session_id: SessionId,
    pending_approvals: Vec<PendingToolApproval>,
}

type ResolveSessionToolApprovalResponse = magi_app_server_protocol::ResolveToolApprovalResult;

fn require_session_tool_approval_scope(
    state: &ApiState,
    scope: &SessionToolApprovalScope,
) -> Result<SessionId, ApiError> {
    let session_id = parse_session_id(Some(&scope.session_id))?;
    resolve_existing_session_scope(
        state,
        &session_id,
        scope.requested_workspace_id(),
        scope.requested_workspace_path(),
    )?;
    Ok(session_id)
}

async fn get_session_tool_approvals(
    State(state): State<ApiState>,
    Query(scope): Query<SessionToolApprovalScope>,
) -> Result<Json<SessionToolApprovalsResponse>, ApiError> {
    let session_id = require_session_tool_approval_scope(&state, &scope)?;
    let pending = crate::interaction_service::pending_approvals(&state, &session_id)?;
    Ok(Json(SessionToolApprovalsResponse {
        pending_approvals: pending,
        session_id,
    }))
}

async fn resolve_session_tool_approval(
    State(state): State<ApiState>,
    Json(request): Json<ResolveSessionToolApprovalRequest>,
) -> Result<Json<ResolveSessionToolApprovalResponse>, ApiError> {
    let session_id = require_session_tool_approval_scope(&state, &request.scope)?;
    let approval_id = request.approval_id.trim();
    crate::interaction_service::resolve_approval(&state, &session_id, approval_id, request.decision)
        .map(Json)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolveSessionUserQuestionRequest {
    #[serde(flatten)]
    scope: SessionToolApprovalScope,
    question_id: String,
    response: UserQuestionResponse,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionUserQuestionsResponse {
    session_id: SessionId,
    pending_questions: Vec<PendingUserQuestion>,
}

type ResolveSessionUserQuestionResponse = magi_app_server_protocol::ResolveUserQuestionResult;

/// 模型向用户提出的待回答选择题（`ask_user_question`）。
async fn get_session_user_questions(
    State(state): State<ApiState>,
    Query(scope): Query<SessionToolApprovalScope>,
) -> Result<Json<SessionUserQuestionsResponse>, ApiError> {
    let session_id = require_session_tool_approval_scope(&state, &scope)?;
    Ok(Json(SessionUserQuestionsResponse {
        pending_questions: state
            .session_store
            .pending_user_questions(&session_id)
            .map_err(|error| ApiError::internal_assembly("读取提问失败", error))?,
        session_id,
    }))
}

async fn resolve_session_user_question(
    State(state): State<ApiState>,
    Json(request): Json<ResolveSessionUserQuestionRequest>,
) -> Result<Json<ResolveSessionUserQuestionResponse>, ApiError> {
    let session_id = require_session_tool_approval_scope(&state, &request.scope)?;
    let question_id = request.question_id.trim();
    crate::interaction_service::resolve_question(&state, &session_id, question_id, request.response)
        .map(Json)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MaterializeSessionRequest {
    scope: SessionScopeKindDto,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MaterializeSessionResponse {
    session_id: SessionId,
    workspace_id: Option<WorkspaceId>,
}

async fn materialize_session(
    State(state): State<ApiState>,
    Json(request): Json<MaterializeSessionRequest>,
) -> Result<Json<MaterializeSessionResponse>, ApiError> {
    let _navigation_guard = state.lock_session_navigation().await;
    let scope = resolve_explicit_session_scope(
        &state,
        request.scope,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let workspace_id = scope.workspace_id();
    let previous_current_session_id = state.session_store.current_session_id();
    let session_id = crate::scope_binding::new_session_id();
    state
        .session_store
        .create_session_for_workspace(
            session_id.clone(),
            crate::session_title::NEW_SESSION_PLACEHOLDER_TITLE,
            workspace_id.as_ref().map(ToString::to_string),
        )
        .map_err(|error| ApiError::internal_assembly("创建浏览器会话所属会话失败", error))?;
    // 新建会话会同时写入 session 记录和 current 指针；这里用完整 projection
    // 提交这次实体创建，确保旧 projection、sidecar 和 current 在同一个持久化事务中
    // 收口。导航专用写入只适合已有实体，不能把新实体的回滚留在旧文件之外。
    if let Err(error) =
        state.persist_session_projection_for_sessions_for_api(std::slice::from_ref(&session_id))
    {
        let original_message = error.message().to_string();
        return Err(
            match state
                .rollback_created_session_after_navigation_lock(
                    &session_id,
                    previous_current_session_id,
                )
                .await
            {
                Ok(()) => error,
                Err(rollback_error) => ApiError::internal_assembly(
                    "创建浏览器会话失败且回滚失败",
                    format!("{original_message}；回滚失败: {rollback_error:?}"),
                ),
            },
        );
    }
    publish_session_directory_event(
        &state,
        "session.created",
        &session_id,
        workspace_id.as_ref(),
    );
    Ok(Json(MaterializeSessionResponse {
        session_id,
        workspace_id,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeleteSessionRequest {
    session_id: String,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

/// 会话的“已查看”是会话级动作，由会话自身的 Personal | Workspace 归属决定。
/// 它不能属于项目目录接口，否则个人会话会被迫伪装成一个空项目。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MarkSessionViewedRequest {
    session_id: String,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

impl MarkSessionViewedRequest {
    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }

    fn requested_workspace_path(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_path.as_deref())
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MarkSessionViewedResponse {
    runtime_epoch: String,
    event_stream_next_sequence: u64,
    session_id: String,
    workspace_id: Option<String>,
    has_unread_completion: bool,
}

#[cfg(test)]
use crate::input_service::session_turn_queue_response;
use crate::input_service::{
    GuideSessionTurnQueueItemRequest, SessionTurnQueueItemRequest, SessionTurnQueueResponseDto,
    SessionTurnQueueScope,
};

impl DeleteSessionRequest {
    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }

    fn requested_workspace_path(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_path.as_deref())
    }
}

async fn get_session_turn_queue(
    State(state): State<ApiState>,
    Query(scope): Query<SessionTurnQueueScope>,
) -> Result<Json<SessionTurnQueueResponseDto>, ApiError> {
    crate::input_service::get_session_turn_queue(state, scope)
        .await
        .map(Json)
}

async fn remove_session_turn_queue_item(
    State(state): State<ApiState>,
    Json(request): Json<SessionTurnQueueItemRequest>,
) -> Result<Json<SessionTurnQueueResponseDto>, ApiError> {
    crate::input_service::remove_session_turn_queue_item(state, request)
        .await
        .map(Json)
}

async fn update_session_turn_queue_item(
    State(state): State<ApiState>,
    Json(request): Json<magi_app_server_protocol::generated::SessionQueueUpdateParams>,
) -> Result<Json<SessionTurnQueueResponseDto>, ApiError> {
    crate::input_service::update_session_turn_queue_item(state, request)
        .await
        .map(Json)
}

async fn retry_session_turn_queue_item(
    State(state): State<ApiState>,
    Json(request): Json<SessionTurnQueueItemRequest>,
) -> Result<Json<SessionTurnQueueResponseDto>, ApiError> {
    crate::input_service::retry_session_turn_queue_item(state, request)
        .await
        .map(Json)
}

async fn guide_session_turn_queue_item(
    State(state): State<ApiState>,
    Json(request): Json<GuideSessionTurnQueueItemRequest>,
) -> Result<Json<SessionTurnQueueResponseDto>, ApiError> {
    crate::input_service::guide_session_turn_queue_item(state, request)
        .await
        .map(Json)
}

async fn submit_session_turn(
    headers: HeaderMap,
    State(state): State<ApiState>,
    Json(mut request): Json<SessionTurnRequestDto>,
) -> Result<Json<SessionTurnResponseDto>, ApiError> {
    request.desktop_browser_tools_allowed =
        super::is_trusted_desktop_renderer_request(&state, &headers);
    crate::turn_service::TurnService::new(state)
        .submit(request)
        .await
        .map(Json)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionNavigationRequest {
    target: SessionNavigationTarget,
    scope: SessionScopeKindDto,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

impl SessionNavigationRequest {
    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }

    fn requested_workspace_path(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_path.as_deref())
    }

    fn requested_session_id(&self) -> Option<&str> {
        trimmed_non_empty(self.session_id.as_deref())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum SessionNavigationTarget {
    Draft,
    Session,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InterruptSessionTurnRequest {
    session_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

impl InterruptSessionTurnRequest {
    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }

    fn requested_workspace_path(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_path.as_deref())
    }
}

async fn interrupt_session_turn(
    State(state): State<ApiState>,
    Json(request): Json<InterruptSessionTurnRequest>,
) -> Result<Json<SessionInterruptResponseDto>, ApiError> {
    let session_id = parse_session_id(request.session_id.as_deref())?;
    crate::turn_service::interrupt_session_turn(
        &state,
        &session_id,
        request.requested_workspace_id(),
        request.requested_workspace_path(),
    )
    .await
    .map(Json)
}

async fn navigate_session(
    State(state): State<ApiState>,
    Json(request): Json<SessionNavigationRequest>,
) -> Result<Json<BootstrapDto>, ApiError> {
    let _navigation_guard = state.lock_session_navigation().await;
    let scope = resolve_explicit_session_scope(
        &state,
        request.scope,
        request.requested_workspace_id(),
        request.requested_workspace_path(),
    )?;
    let workspace_id = scope.workspace_id();
    let selected_session_id = match &request.target {
        SessionNavigationTarget::Draft => {
            if request.requested_session_id().is_some() {
                return Err(ApiError::InvalidInput(
                    "草稿导航不能携带 sessionId".to_string(),
                ));
            }
            None
        }
        SessionNavigationTarget::Session => {
            let session_id = parse_session_id(request.requested_session_id())?;
            require_session_record_in_scope(&state, &session_id, &scope)?;
            Some(session_id)
        }
    };
    // 导航请求只提交当前会话指针和工作区选择。Turn 状态由执行生命周期独立管理，
    // 切换会话不应等待后台 Runner 或模型执行。
    if let SessionScope::Workspace(binding) = &scope {
        state
            .workspace_registry
            .activate(&binding.workspace_id)
            .map_err(|error| ApiError::internal_assembly("切换会话工作区失败", error))?;
    }
    match selected_session_id.as_ref() {
        Some(session_id) => {
            state
                .session_store
                .select_current_session(session_id)
                .map_err(|error| ApiError::internal_assembly("切换当前会话失败", error))?;
        }
        None => {
            state.session_store.clear_current_session();
        }
    }
    state.persist_session_navigation_state_for_api(selected_session_id.clone())?;
    Ok(Json(state.bootstrap_dto_for_workspace_session(
        workspace_id.as_ref().map(WorkspaceId::as_str),
        selected_session_id.as_ref(),
    )?))
}

async fn continue_session(
    State(state): State<ApiState>,
    Json(request): Json<ContinueSessionRequest>,
) -> Result<Json<ContinueSessionResponseDto>, ApiError> {
    crate::turn_service::continue_session(&state, request)
        .await
        .map(Json)
}

async fn delete_session(
    State(state): State<ApiState>,
    Json(request): Json<DeleteSessionRequest>,
) -> Result<Json<BootstrapDto>, ApiError> {
    let session_id = SessionId::new(&request.session_id);
    let _lifecycle_guard = state.lock_session_lifecycle(&session_id).await;
    // 启动时无法加载的会话不在会话状态里，只能清理它的持久化文件。
    if state.session_store.session(&session_id).is_none()
        && state
            .session_store
            .unavailable_session(&session_id)
            .is_some()
    {
        let entry = state
            .discard_unavailable_session_after_lifecycle_lock(&session_id)
            .await?;
        let workspace_id = entry.workspace_id.map(WorkspaceId::new);
        publish_session_directory_event(
            &state,
            "session.deleted",
            &session_id,
            workspace_id.as_ref(),
        );
        return Ok(Json(state.bootstrap_dto_for_workspace_session(
            workspace_id.as_ref().map(WorkspaceId::as_str),
            None,
        )?));
    }
    let scope = resolve_existing_session_scope(
        &state,
        &session_id,
        request.requested_workspace_id(),
        request.requested_workspace_path(),
    )?;
    let workspace_id = scope.workspace_id();
    let replacement_session_id =
        if state.session_store.current_session_id().as_ref() == Some(&session_id) {
            state
                .session_records_for_workspace(workspace_id.as_ref().map(WorkspaceId::as_str))
                .into_iter()
                .filter(|session| session.session_id != session_id)
                .max_by(|left, right| {
                    left.updated_at
                        .cmp(&right.updated_at)
                        .then_with(|| left.session_id.as_str().cmp(right.session_id.as_str()))
                })
                .map(|session| session.session_id)
        } else {
            None
        };
    state
        .delete_session_and_resources_after_lifecycle_lock(
            &session_id,
            replacement_session_id.clone(),
        )
        .await?;
    publish_session_directory_event(
        &state,
        "session.deleted",
        &session_id,
        workspace_id.as_ref(),
    );
    Ok(Json(state.bootstrap_dto_for_workspace_session(
        workspace_id.as_ref().map(WorkspaceId::as_str),
        replacement_session_id.as_ref(),
    )?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RenameSessionRequest {
    session_id: String,
    name: String,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

impl RenameSessionRequest {
    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }

    fn requested_workspace_path(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_path.as_deref())
    }
}

async fn rename_session(
    State(state): State<ApiState>,
    Json(request): Json<RenameSessionRequest>,
) -> Result<Json<BootstrapDto>, ApiError> {
    let session_id = SessionId::new(&request.session_id);
    let _lifecycle_guard = state.lock_session_lifecycle(&session_id).await;
    let scope = resolve_existing_session_scope(
        &state,
        &session_id,
        request.requested_workspace_id(),
        request.requested_workspace_path(),
    )?;
    let workspace_id = scope.workspace_id();
    let current = state
        .session_store
        .session(&session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    let renamed = state.rename_session_with_persistence_for_api(&session_id, &request.name)?;
    if current.title != renamed.title {
        crate::session_title::publish_session_title_updated(
            &state,
            &session_id,
            workspace_id.clone(),
            &renamed.title,
        );
    }
    Ok(Json(state.bootstrap_dto_for_workspace_session(
        workspace_id.as_ref().map(WorkspaceId::as_str),
        None,
    )?))
}

async fn mark_session_viewed(
    State(state): State<ApiState>,
    Json(request): Json<MarkSessionViewedRequest>,
) -> Result<Json<MarkSessionViewedResponse>, ApiError> {
    let session_id = SessionId::new(&request.session_id);
    let scope = resolve_existing_session_scope(
        &state,
        &session_id,
        request.requested_workspace_id(),
        request.requested_workspace_path(),
    )?;
    let _session_turn_guard = state.lock_session_turn_commit(&session_id).await;
    let session = state
        .session_store
        .mark_session_viewed(&session_id)
        .map_err(|error| ApiError::internal_assembly("标记会话已查看失败", error))?;
    state.persist_session_state_checkpoint("session_viewed")?;

    let workspace_id = scope.workspace_id();
    let event_suffix = SESSION_VIEW_EVENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let occurred_at = UtcMillis::now();
    state.event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!(
                "event-session-viewed-{session_id}-{}-{event_suffix}",
                occurred_at.0,
            )),
            "session.viewed",
            json!({
                "session_id": session_id.as_str(),
                "workspace_id": workspace_id.as_ref().map(WorkspaceId::as_str),
                "has_unread_completion": session.has_unread_completion(),
            }),
        )
        .with_context(EventContext {
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.clone(),
            ..EventContext::default()
        }),
    );

    Ok(Json(MarkSessionViewedResponse {
        runtime_epoch: state.runtime_epoch().to_string(),
        event_stream_next_sequence: state.event_bus.snapshot().next_sequence,
        session_id: session_id.to_string(),
        workspace_id: workspace_id.map(|workspace_id| workspace_id.to_string()),
        has_unread_completion: session.has_unread_completion(),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CloseSessionRequest {
    session_id: String,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

impl CloseSessionRequest {
    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }

    fn requested_workspace_path(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_path.as_deref())
    }
}

async fn close_session(
    State(state): State<ApiState>,
    Json(request): Json<CloseSessionRequest>,
) -> Result<Json<BootstrapDto>, ApiError> {
    let session_id = SessionId::new(&request.session_id);
    let _lifecycle_guard = state.lock_session_lifecycle(&session_id).await;
    let scope = resolve_existing_session_scope(
        &state,
        &session_id,
        request.requested_workspace_id(),
        request.requested_workspace_path(),
    )?;
    let workspace_id = scope.workspace_id();
    let manager = state.runner_manager();
    crate::turn_service::cancel_active_session_turn_for_lifecycle(&state, &session_id);
    state
        .session_store
        .archive_session(&session_id)
        .map_err(|e| ApiError::internal_assembly("关闭会话失败", e))?;
    state.clear_regular_session_turn_queue_for_lifecycle(&session_id)?;
    if let Some(manager) = manager {
        manager
            .unbind_session_after_lifecycle_lock(&session_id)
            .await
            .map_err(|error| ApiError::internal_assembly("关闭会话时停止执行失败", error))?;
    }
    state
        .close_browser_session_for_magi_session(&session_id)
        .await?;
    state
        .terminal_sessions
        .close_for_session(session_id.as_str());
    state.release_session_git_execution_lease(&session_id);
    state.persist_session_projection_for_sessions_for_api(std::slice::from_ref(&session_id))?;
    publish_session_directory_event(&state, "session.closed", &session_id, workspace_id.as_ref());
    Ok(Json(state.bootstrap_dto_for_workspace_session(
        workspace_id.as_ref().map(WorkspaceId::as_str),
        None,
    )?))
}

fn publish_session_directory_event(
    state: &ApiState,
    event_type: &str,
    session_id: &SessionId,
    workspace_id: Option<&WorkspaceId>,
) {
    state.event_bus.publish(
        EventEnvelope::domain(
            EventId::unique(format!("event-{event_type}-{session_id}")),
            event_type,
            json!({
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.map(WorkspaceId::to_string),
            }),
        )
        .with_context(EventContext {
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.cloned(),
            ..EventContext::default()
        }),
    );
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NotificationsQuery {
    scope: SessionScopeKindDto,
    session_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

impl NotificationsQuery {
    fn requested_session_id(&self) -> Option<SessionId> {
        parse_requested_session_id(self.session_id.as_deref())
    }

    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }

    fn requested_workspace_path(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_path.as_deref())
    }
}

async fn get_notifications(
    State(state): State<ApiState>,
    Query(query): Query<NotificationsQuery>,
) -> Result<Json<NotificationsResponseDto>, ApiError> {
    let scope = resolve_explicit_session_scope(
        &state,
        query.scope,
        query.requested_workspace_id(),
        query.requested_workspace_path(),
    )?;
    let session_id =
        validate_optional_notification_session(&state, query.requested_session_id(), &scope)?;
    Ok(Json(build_notifications_response(
        &state,
        &scope,
        session_id.as_ref(),
    )))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NotificationScopeRequest {
    session_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

impl NotificationScopeRequest {
    fn requested_session_id(&self) -> Option<SessionId> {
        parse_requested_session_id(self.session_id.as_deref())
    }

    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }

    fn requested_workspace_path(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_path.as_deref())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReportIncidentRequest {
    session_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    scope: NotificationScope,
    level: Option<String>,
    title: Option<String>,
    message: String,
    detail: Option<String>,
    error_code: Option<String>,
    failure_stage: Option<String>,
    task_id: Option<String>,
    request_id: Option<String>,
    source: Option<String>,
    action_required: Option<bool>,
}

impl ReportIncidentRequest {
    fn requested_session_id(&self) -> Option<SessionId> {
        parse_requested_session_id(self.session_id.as_deref())
    }

    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }

    fn requested_workspace_path(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_path.as_deref())
    }
}

async fn report_incident(
    State(state): State<ApiState>,
    Json(request): Json<ReportIncidentRequest>,
) -> Result<Json<NotificationsResponseDto>, ApiError> {
    let scope = resolve_session_scope(
        &state,
        request.requested_workspace_id(),
        request.requested_workspace_path(),
    )?;
    let requested_session_id =
        validate_optional_notification_session(&state, request.requested_session_id(), &scope)?;
    let session_id = match request.scope {
        NotificationScope::Session => Some(requested_session_id.clone().ok_or_else(|| {
            ApiError::InvalidInput("session incident 必须提供 sessionId".to_string())
        })?),
        NotificationScope::App | NotificationScope::Workspace => None,
    };
    if matches!(request.scope, NotificationScope::Workspace) && scope.workspace_id().is_none() {
        return Err(ApiError::InvalidInput(
            "个人会话没有项目级通知作用域".to_string(),
        ));
    }
    let message = trimmed_non_empty(Some(request.message.as_str()))
        .ok_or_else(|| ApiError::InvalidInput("通知内容不能为空".to_string()))?
        .to_string();
    let message = public_runtime_excerpt(&message, 4096);
    // 每次异常发生都由服务端生成独立 ID。客户端固定 ID 会让多次错误在前端
    // 被去重，也会使解决/删除操作无法唯一指向某次发生记录。
    let notification_id = format!(
        "notification-{}-{}",
        UtcMillis::now().0,
        INCIDENT_NOTIFICATION_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    state
        .session_store
        .append_incident_record(NotificationRecord {
            notification_id,
            scope: request.scope,
            workspace_id: match request.scope {
                NotificationScope::Workspace => scope.workspace_id().map(|id| id.to_string()),
                NotificationScope::App | NotificationScope::Session => None,
            },
            session_id: session_id.clone(),
            kind: "incident".to_string(),
            level: trimmed_non_empty(request.level.as_deref()).map(str::to_string),
            title: trimmed_non_empty(request.title.as_deref()).map(str::to_string),
            message,
            detail: trimmed_non_empty(request.detail.as_deref())
                .map(|detail| public_runtime_excerpt(detail, 4096)),
            error_code: trimmed_non_empty(request.error_code.as_deref()).map(str::to_string),
            failure_stage: trimmed_non_empty(request.failure_stage.as_deref()).map(str::to_string),
            task_id: trimmed_non_empty(request.task_id.as_deref()).map(str::to_string),
            request_id: trimmed_non_empty(request.request_id.as_deref()).map(str::to_string),
            source: trimmed_non_empty(request.source.as_deref()).map(str::to_string),
            created_at: UtcMillis::now(),
            handled: false,
            action_required: request.action_required.unwrap_or(true),
            count_unread: true,
            fingerprint: String::new(),
            occurrence_count: 1,
            resolved: false,
        })
        .map_err(|error| ApiError::internal_assembly("记录系统异常失败", error))?;
    // 会话级异常存放在所属会话的 projection 里；应用级 / 项目级异常只落到各自的 meta 文件。
    let affected: Vec<SessionId> = session_id.iter().cloned().collect();
    state.persist_session_projection_for_sessions_for_api(&affected)?;
    Ok(Json(build_notifications_response(
        &state,
        &scope,
        requested_session_id.as_ref(),
    )))
}

async fn mark_all_notifications_read(
    State(state): State<ApiState>,
    Json(request): Json<NotificationScopeRequest>,
) -> Result<Json<NotificationsResponseDto>, ApiError> {
    let scope = resolve_session_scope(
        &state,
        request.requested_workspace_id(),
        request.requested_workspace_path(),
    )?;
    let session_id =
        validate_optional_notification_session(&state, request.requested_session_id(), &scope)?;
    let context = notification_context(&scope, session_id.clone());
    let affected = state.sessions_with_notifications_in_context(&context);
    state
        .session_store
        .mark_notifications_handled_for_context(&context);
    state.persist_session_projection_for_sessions_for_api(&affected)?;
    Ok(Json(build_notifications_response(
        &state,
        &scope,
        session_id.as_ref(),
    )))
}

async fn clear_notifications(
    State(state): State<ApiState>,
    Json(request): Json<NotificationScopeRequest>,
) -> Result<Json<NotificationsResponseDto>, ApiError> {
    let scope = resolve_session_scope(
        &state,
        request.requested_workspace_id(),
        request.requested_workspace_path(),
    )?;
    let session_id =
        validate_optional_notification_session(&state, request.requested_session_id(), &scope)?;
    let context = notification_context(&scope, session_id.clone());
    let affected = state.sessions_with_notifications_in_context(&context);
    state
        .session_store
        .clear_notifications_for_context(&context);
    state.persist_session_projection_for_sessions_for_api(&affected)?;
    Ok(Json(build_notifications_response(
        &state,
        &scope,
        session_id.as_ref(),
    )))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveNotificationRequest {
    session_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    notification_id: String,
}

impl RemoveNotificationRequest {
    fn requested_session_id(&self) -> Option<SessionId> {
        parse_requested_session_id(self.session_id.as_deref())
    }

    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }

    fn requested_workspace_path(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_path.as_deref())
    }

    fn requested_notification_id(&self) -> Option<String> {
        trimmed_non_empty(Some(self.notification_id.as_str())).map(str::to_string)
    }
}

async fn remove_notification(
    State(state): State<ApiState>,
    Json(request): Json<RemoveNotificationRequest>,
) -> Result<Json<NotificationsResponseDto>, ApiError> {
    let scope = resolve_session_scope(
        &state,
        request.requested_workspace_id(),
        request.requested_workspace_path(),
    )?;
    let session_id =
        validate_optional_notification_session(&state, request.requested_session_id(), &scope)?;
    let notification_id = request
        .requested_notification_id()
        .ok_or_else(|| ApiError::InvalidInput("notification_id 不能为空".to_string()))?;
    let context = notification_context(&scope, session_id.clone());
    let affected = state.sessions_with_notifications_in_context(&context);
    state
        .session_store
        .remove_notification_for_context(&context, &notification_id)
        .map_err(|error| match error {
            DomainError::NotFound { .. } => ApiError::not_found("通知不存在", &notification_id),
            other => ApiError::internal_assembly("移除通知失败", other),
        })?;
    state.persist_session_projection_for_sessions_for_api(&affected)?;
    Ok(Json(build_notifications_response(
        &state,
        &scope,
        session_id.as_ref(),
    )))
}

async fn resolve_notification(
    State(state): State<ApiState>,
    Json(request): Json<RemoveNotificationRequest>,
) -> Result<Json<NotificationsResponseDto>, ApiError> {
    let scope = resolve_session_scope(
        &state,
        request.requested_workspace_id(),
        request.requested_workspace_path(),
    )?;
    let session_id =
        validate_optional_notification_session(&state, request.requested_session_id(), &scope)?;
    let notification_id = request
        .requested_notification_id()
        .ok_or_else(|| ApiError::InvalidInput("notification_id 不能为空".to_string()))?;
    let context = notification_context(&scope, session_id.clone());
    let affected = state.sessions_with_notifications_in_context(&context);
    state
        .session_store
        .resolve_notification_for_context(&context, &notification_id)
        .map_err(|error| match error {
            DomainError::NotFound { .. } => ApiError::not_found("通知不存在", &notification_id),
            other => ApiError::internal_assembly("解决通知失败", other),
        })?;
    state.persist_session_projection_for_sessions_for_api(&affected)?;
    Ok(Json(build_notifications_response(
        &state,
        &scope,
        session_id.as_ref(),
    )))
}

fn build_notifications_response(
    state: &ApiState,
    scope: &SessionScope,
    session_id: Option<&SessionId>,
) -> NotificationsResponseDto {
    NotificationsResponseDto::from_records(
        scope
            .workspace_id()
            .map(|workspace_id| workspace_id.to_string()),
        session_id,
        state
            .session_store
            .notifications_for_context(&notification_context(scope, session_id.cloned())),
    )
}

fn notification_context(
    scope: &SessionScope,
    session_id: Option<SessionId>,
) -> NotificationContext {
    match scope {
        SessionScope::Personal => NotificationContext::personal(session_id),
        SessionScope::Workspace(binding) => {
            NotificationContext::workspace(binding.workspace_id.to_string(), session_id)
        }
    }
}

fn validate_optional_notification_session(
    state: &ApiState,
    requested_session_id: Option<SessionId>,
    scope: &SessionScope,
) -> Result<Option<SessionId>, ApiError> {
    if let Some(session_id) = requested_session_id {
        let resolved_scope = resolve_existing_session_scope(
            state,
            &session_id,
            scope.workspace_id().as_ref().map(WorkspaceId::as_str),
            None,
        )?;
        if &resolved_scope != scope {
            return Err(ApiError::InvalidInput(
                "会话请求作用域与通知作用域不一致".to_string(),
            ));
        }
        return Ok(Some(session_id));
    }
    Ok(None)
}

fn parse_requested_session_id(value: Option<&str>) -> Option<SessionId> {
    trimmed_non_empty(value).map(SessionId::new)
}

#[cfg(test)]
mod tests;
