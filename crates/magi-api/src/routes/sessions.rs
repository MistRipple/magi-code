use axum::{
    Json, Router,
    extract::{Query, State},
    http::HeaderMap,
    routing::{get, post},
};
use magi_conversation_runtime::{
    PendingToolApproval, PendingUserQuestion, ToolApprovalDecision, UserQuestionResponse,
};
use magi_core::{
    AccessProfile, DomainError, EventId, SessionId, UtcMillis, WorkspaceId, public_runtime_excerpt,
};
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
    turn_service::{
        canonical_turn_matches_queued_regular_turn, session_turn_request_is_plain_text,
        submit_steer_current_turn_after_turn_commit, trimmed_non_empty,
    },
};

static INCIDENT_NOTIFICATION_COUNTER: AtomicU64 = AtomicU64::new(1);
static SESSION_VIEW_EVENT_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/session/materialize", post(materialize_session))
        .route("/session/turn", post(submit_session_turn))
        .route("/session/queue", get(get_session_turn_queue))
        .route(
            "/session/queue/remove",
            post(remove_session_turn_queue_item),
        )
        .route("/session/queue/guide", post(guide_session_turn_queue_item))
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResolveSessionToolApprovalResponse {
    session_id: SessionId,
    approval_id: String,
    decision: ToolApprovalDecision,
    status: &'static str,
}

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
    Ok(Json(SessionToolApprovalsResponse {
        pending_approvals: state
            .conversation_registry
            .tool_approvals()
            .pending_for_session(&session_id),
        session_id,
    }))
}

async fn resolve_session_tool_approval(
    State(state): State<ApiState>,
    Json(request): Json<ResolveSessionToolApprovalRequest>,
) -> Result<Json<ResolveSessionToolApprovalResponse>, ApiError> {
    let session_id = require_session_tool_approval_scope(&state, &request.scope)?;
    let approval_id = request.approval_id.trim();
    if approval_id.is_empty() {
        return Err(ApiError::InvalidInput("approvalId 不能为空".to_string()));
    }
    let pending = state
        .conversation_registry
        .tool_approvals()
        .resolve(&session_id, approval_id, request.decision)
        .map_err(ApiError::Conflict)?;
    let _ = state.event_bus.publish(
        EventEnvelope::domain(
            EventId::unique("event-tool-approval-resolved"),
            "tool.approval.resolved",
            json!({
                "session_id": session_id,
                "task_id": pending.task_id,
                "turn_id": pending.turn_id,
                "tool_call_id": pending.tool_call_id,
                "tool_name": pending.tool_name,
                "approval_id": pending.approval_id,
                "decision": request.decision,
            }),
        )
        .with_context(EventContext {
            session_id: Some(session_id.clone()),
            task_id: Some(pending.task_id),
            ..EventContext::default()
        }),
    );
    Ok(Json(ResolveSessionToolApprovalResponse {
        session_id,
        approval_id: approval_id.to_string(),
        decision: request.decision,
        status: "resolved",
    }))
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResolveSessionUserQuestionResponse {
    session_id: SessionId,
    question_id: String,
    status: &'static str,
}

/// 模型向用户提出的待回答选择题（`ask_user_question`）。
async fn get_session_user_questions(
    State(state): State<ApiState>,
    Query(scope): Query<SessionToolApprovalScope>,
) -> Result<Json<SessionUserQuestionsResponse>, ApiError> {
    let session_id = require_session_tool_approval_scope(&state, &scope)?;
    Ok(Json(SessionUserQuestionsResponse {
        pending_questions: state
            .conversation_registry
            .user_questions()
            .pending_for_session(&session_id),
        session_id,
    }))
}

async fn resolve_session_user_question(
    State(state): State<ApiState>,
    Json(request): Json<ResolveSessionUserQuestionRequest>,
) -> Result<Json<ResolveSessionUserQuestionResponse>, ApiError> {
    let session_id = require_session_tool_approval_scope(&state, &request.scope)?;
    let question_id = request.question_id.trim();
    if question_id.is_empty() {
        return Err(ApiError::InvalidInput("questionId 不能为空".to_string()));
    }
    let outcome = match &request.response {
        UserQuestionResponse::Answered { .. } => "answered",
        UserQuestionResponse::Skipped => "skipped",
    };
    // 回答与题目对不上是调用方的输入错误；问题已经收口（轮次结束、已回答）才是冲突。
    let pending = state
        .conversation_registry
        .user_questions()
        .resolve(&session_id, question_id, request.response)
        .map_err(|message| {
            if message.starts_with("这个问题已经处理过") {
                ApiError::Conflict(message)
            } else {
                ApiError::InvalidInput(message)
            }
        })?;
    let _ = state.event_bus.publish(
        EventEnvelope::domain(
            EventId::unique("event-user-question-resolved"),
            "user.question.resolved",
            json!({
                "session_id": session_id,
                "task_id": pending.task_id,
                "turn_id": pending.turn_id,
                "tool_call_id": pending.tool_call_id,
                "question_id": pending.question_id,
                "outcome": outcome,
            }),
        )
        .with_context(EventContext {
            session_id: Some(session_id.clone()),
            task_id: pending.task_id,
            ..EventContext::default()
        }),
    );
    Ok(Json(ResolveSessionUserQuestionResponse {
        session_id,
        question_id: question_id.to_string(),
        status: "resolved",
    }))
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionTurnQueueScope {
    session_id: String,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

impl SessionTurnQueueScope {
    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }

    fn requested_workspace_path(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_path.as_deref())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveSessionTurnQueueItemRequest {
    #[serde(flatten)]
    scope: SessionTurnQueueScope,
    queue_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueuedSessionTurnDto {
    queue_id: String,
    queue_position: usize,
    request_id: Option<String>,
    session_id: String,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
    accepted_at: UtcMillis,
    content: String,
    text: Option<String>,
    command: Option<magi_app_server_protocol::SessionTurnCommand>,
    workflow_id: Option<String>,
    plugin_command_id: Option<String>,
    skill_name: Option<String>,
    goal_mode: bool,
    access_profile: Option<AccessProfile>,
    images: Vec<crate::dto::SessionTurnImageDto>,
    context_references: Vec<crate::dto::SessionContextReferenceDto>,
    browser_annotation_refs: Vec<String>,
    browser_node_selections: Vec<crate::dto::BrowserNodeSelectionDto>,
    plugin_selections: Vec<magi_app_server_protocol::generated::PluginSelection>,
    can_guide: bool,
    retry_count: u8,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionTurnQueueResponseDto {
    session_id: String,
    queued_turns: Vec<QueuedSessionTurnDto>,
}

impl DeleteSessionRequest {
    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }

    fn requested_workspace_path(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_path.as_deref())
    }
}

fn session_turn_queue_response(
    state: &ApiState,
    session_id: &SessionId,
) -> SessionTurnQueueResponseDto {
    // 能否把排队消息转为引导取决于当前活动轮次：任务轮次不读取引导。
    let active_turn_accepts_steer = state
        .turn_coordinator()
        .active_turn_accepts_steer(session_id);
    let queued_turns = state
        .queued_regular_session_turns(session_id)
        .into_iter()
        .enumerate()
        .map(|(index, queued)| {
            let text = queued.request.trimmed_text();
            let can_guide =
                active_turn_accepts_steer && session_turn_request_is_plain_text(&queued.request);
            QueuedSessionTurnDto {
                queue_id: queued.queue_id,
                queue_position: index + 1,
                request_id: queued.request.request_id(),
                session_id: queued.session_id.to_string(),
                workspace_id: queued
                    .requested_workspace_id
                    .as_ref()
                    .map(ToString::to_string),
                workspace_path: queued.request.workspace_path.clone(),
                accepted_at: queued.accepted_at,
                content: queued.request.timeline_content(text.as_deref()),
                text,
                command: queued.request.command.clone(),
                workflow_id: queued.request.workflow_id.clone(),
                plugin_command_id: queued.request.plugin_command_id.clone(),
                skill_name: queued.request.skill_name.clone(),
                goal_mode: queued.request.goal_mode,
                access_profile: queued.request.access_profile,
                images: queued.request.images,
                context_references: queued.request.context_references,
                browser_annotation_refs: queued.request.browser_annotation_refs,
                browser_node_selections: queued.request.browser_node_selections,
                plugin_selections: queued.request.plugin_selections,
                can_guide,
                retry_count: queued.retry_count,
            }
        })
        .collect();
    SessionTurnQueueResponseDto {
        session_id: session_id.to_string(),
        queued_turns,
    }
}

fn require_session_turn_queue_scope(
    state: &ApiState,
    scope: &SessionTurnQueueScope,
) -> Result<(SessionId, SessionScope), ApiError> {
    let session_id = parse_session_id(Some(&scope.session_id))?;
    let session_scope = resolve_existing_session_scope(
        state,
        &session_id,
        scope.requested_workspace_id(),
        scope.requested_workspace_path(),
    )?;
    Ok((session_id, session_scope))
}

async fn get_session_turn_queue(
    State(state): State<ApiState>,
    Query(scope): Query<SessionTurnQueueScope>,
) -> Result<Json<SessionTurnQueueResponseDto>, ApiError> {
    let (session_id, _) = require_session_turn_queue_scope(&state, &scope)?;
    Ok(Json(session_turn_queue_response(&state, &session_id)))
}

async fn remove_session_turn_queue_item(
    State(state): State<ApiState>,
    Json(request): Json<RemoveSessionTurnQueueItemRequest>,
) -> Result<Json<SessionTurnQueueResponseDto>, ApiError> {
    let (session_id, _) = require_session_turn_queue_scope(&state, &request.scope)?;
    let queue_id = request.queue_id.trim();
    if queue_id.is_empty() {
        return Err(ApiError::InvalidInput("queueId 不能为空".to_string()));
    }
    let _session_turn_guard = state.lock_session_turn_commit(&session_id).await;
    state.remove_regular_session_turn(&session_id, queue_id)?;
    Ok(Json(session_turn_queue_response(&state, &session_id)))
}

async fn guide_session_turn_queue_item(
    State(state): State<ApiState>,
    Json(request): Json<RemoveSessionTurnQueueItemRequest>,
) -> Result<Json<SessionTurnQueueResponseDto>, ApiError> {
    let (session_id, scope) = require_session_turn_queue_scope(&state, &request.scope)?;
    let queue_id = request.queue_id.trim();
    if queue_id.is_empty() {
        return Err(ApiError::InvalidInput("queueId 不能为空".to_string()));
    }

    // 队列消费、删除和转引导必须共用同一个 session 临界区，避免后台出队与用户操作
    // 同时取得同一条消息。引导先写入 canonical Turn，再持久化移除队列项；若第二步
    // 失败，重试会通过 requestId + userMessageId 识别已提交消息，不会重复引导。
    let _session_turn_guard = state.lock_session_turn_commit(&session_id).await;
    let queued = state
        .queued_regular_session_turns(&session_id)
        .into_iter()
        .find(|turn| turn.queue_id == queue_id)
        .ok_or_else(|| ApiError::not_found("排队消息不存在", queue_id))?;

    let already_committed = state
        .session_store
        .canonical_turns_for_session(&session_id)
        .into_iter()
        .any(|turn| canonical_turn_matches_queued_regular_turn(&turn, &queued));
    if !already_committed {
        if !session_turn_request_is_plain_text(&queued.request) {
            return Err(ApiError::InvalidInput(
                "仅纯文字排队消息可以转为当前回复的引导".to_string(),
            ));
        }
        let expected_turn_id = state
            .session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn.map(|turn| turn.turn_id))
            .ok_or_else(|| {
                ApiError::turn_conflict(
                    "no_active_turn",
                    None,
                    "当前回复已经结束，无法把排队消息转为引导",
                )
            })?;
        let mut steer_request = queued.request.clone();
        steer_request.session_id = Some(session_id.to_string());
        steer_request.workspace_id = scope.workspace_id().as_ref().map(ToString::to_string);
        steer_request.steer_current_turn = true;
        steer_request.expected_turn_id = Some(expected_turn_id);
        submit_steer_current_turn_after_turn_commit(
            &state,
            &steer_request,
            &scope,
            crate::turn_service::monotonic_accepted_at(),
        )
        .await?;
    }

    if !state.remove_regular_session_turn(&session_id, queue_id)? {
        return Err(ApiError::not_found("排队消息不存在", queue_id));
    }
    Ok(Json(session_turn_queue_response(&state, &session_id)))
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
            .await;
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClearNotificationsRequest {
    session_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

impl ClearNotificationsRequest {
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

async fn clear_notifications(
    State(state): State<ApiState>,
    Json(request): Json<ClearNotificationsRequest>,
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
mod tests {
    use super::*;
    use crate::dto::SessionTurnRouteDto;
    use crate::session_continue::restore_missing_resumed_branch_threads;
    use crate::session_continue::{SessionContinueAccepted, persist_resumed_branch_user_input};
    use crate::state::{
        ApiState, QueuedRegularSessionTurn, RunnerManager, RuntimeStatePersistence,
    };
    use crate::turn_service::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use magi_browser_authority::BrowserToolKind;
    use magi_conversation_runtime::{
        CanonicalTurnEventSink, execution_admission::ExecutionAdmissionPermit,
        task_execution_registry::TaskExecutionPlan, task_runner_bridge::TaskDispatcher,
    };
    use magi_conversation_runtime::{
        CoordinatorAdmission, CoordinatorCommandResult, CoordinatorTurnStatus, ExecutionProfile,
        TurnAdmission, TurnCommand,
    };
    use magi_core::TaskEvidenceRequirement;
    use magi_core::{
        AbsolutePath, ExecutionOwnership, ExecutionResultStatus, GoalId, MissionId, Task,
        TaskExecutionTarget, TaskExecutorBinding, TaskId, TaskKind, TaskRuntimePayload, TaskStatus,
        ThreadId, ToolCallId, UtcMillis, WorkerId, WorkspaceId,
    };
    use magi_core::{SessionLifecycleStatus, TaskCompletionContract};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_orchestrator::{
        ExecutionWritebackPlans,
        task_store::{TaskLease, TaskStore},
        task_worker_catalog::WorkerInfo,
    };
    use magi_session_store::{
        ActiveExecutionBranch, ExecutionThread, ExecutionThreadStatus, GoalStatus,
        SessionExecutionSidecarStoreState, SessionStore,
    };
    use magi_session_store::{
        ActiveExecutionTurn, ActiveExecutionTurnItem, CANONICAL_TURN_SCHEMA_VERSION,
        CanonicalTurnItemKind, SessionGoal, TimelineEntryInput, TimelineEntryKind,
    };
    use magi_settings_store::SettingsStore;
    use magi_tool_runtime::{
        BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy,
        ToolRegistry,
    };
    use magi_workspace::WorkspaceStore;
    use serde_json::Value;
    use std::{
        fs,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tower::ServiceExt;

    fn test_state() -> ApiState {
        ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_task_store(Arc::new(TaskStore::new()))
    }

    fn seed_conversation_turn(
        state: &ApiState,
        session_id: &SessionId,
        turn_id: &str,
        turn_seq: u64,
        accepted_at: UtcMillis,
        status: &str,
        text: &str,
    ) {
        let coordinator = state.turn_coordinator();
        crate::routes::test_turn_fixtures::seed_conversation_turn(
            &state.session_store,
            coordinator,
            session_id,
            turn_id,
            turn_seq,
            accepted_at,
            status,
            text,
        );
    }

    #[tokio::test]
    async fn user_question_route_lists_and_resolves_pending_questions() {
        let state = test_state();
        let session_id = SessionId::new("session-user-question-route");
        state
            .session_store
            .create_session(session_id.clone(), "user question route")
            .expect("session should be creatable");
        let questions = magi_conversation_runtime::user_question::parse_questions(
            &serde_json::json!({"questions":[{
                "question": "用哪种方案？",
                "header": "方案",
                "multiSelect": false,
                "options": [{"label": "A"}, {"label": "B", "description": "更稳"}]
            }]})
            .to_string(),
        )
        .expect("questions should parse");
        let waiter = state
            .conversation_registry
            .user_questions()
            .request(PendingUserQuestion {
                can_skip: true,
                question_id: "question-route-1".to_string(),
                session_id: session_id.clone(),
                task_id: Some(TaskId::new("task-user-question-route")),
                turn_id: "turn-user-question-route".to_string(),
                tool_call_id: "call-user-question-route".to_string(),
                questions,
                requested_at: UtcMillis::now(),
            });
        let app = routes().with_state(state);
        let post = |body: serde_json::Value| {
            Request::builder()
                .method("POST")
                .uri("/session/user-question")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request should build")
        };

        let listed = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/session/user-questions?sessionId={session_id}"))
                    .body(Body::empty())
                    .expect("list request should build"),
            )
            .await
            .expect("list route should respond");
        assert_eq!(listed.status(), StatusCode::OK);
        let body = axum::body::to_bytes(listed.into_body(), usize::MAX)
            .await
            .unwrap();
        let listed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            listed["pendingQuestions"][0]["questionId"],
            "question-route-1"
        );
        assert_eq!(
            listed["pendingQuestions"][0]["questions"][0]["header"],
            "方案"
        );

        // 回答和题目对不上：输入错误，问题仍然待处理。
        let invalid = app
            .clone()
            .oneshot(post(serde_json::json!({
                "sessionId": session_id,
                "questionId": "question-route-1",
                "response": {"kind": "answered", "answers": [{"selected": ["不存在"]}]},
            })))
            .await
            .expect("invalid answer should respond");
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

        let answered = app
            .clone()
            .oneshot(post(serde_json::json!({
                "sessionId": session_id,
                "questionId": "question-route-1",
                "response": {"kind": "answered", "answers": [{"selected": ["B"]}]},
            })))
            .await
            .expect("answer route should respond");
        assert_eq!(answered.status(), StatusCode::OK);
        assert_eq!(
            waiter
                .response_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("runtime should receive the answer"),
            magi_conversation_runtime::UserQuestionResponse::Answered {
                answers: vec![magi_conversation_runtime::UserQuestionAnswer {
                    selected: vec!["B".to_string()],
                    other: None,
                }]
            }
        );

        let duplicate = app
            .oneshot(post(serde_json::json!({
                "sessionId": session_id,
                "questionId": "question-route-1",
                "response": {"kind": "skipped"},
            })))
            .await
            .expect("duplicate answer should respond");
        assert_eq!(
            duplicate.status(),
            StatusCode::CONFLICT,
            "重复回答必须被确定性拒绝"
        );
    }

    #[tokio::test]
    async fn tool_approval_route_resolves_pending_runtime_call() {
        let state = test_state();
        let session_id = SessionId::new("session-tool-approval-route");
        state
            .session_store
            .create_session(session_id.clone(), "tool approval route")
            .expect("session should be creatable");
        let pending = PendingToolApproval {
            approval_id: "approval-route-1".to_string(),
            session_id: session_id.clone(),
            task_id: TaskId::new("task-tool-approval-route"),
            turn_id: "turn-tool-approval-route".to_string(),
            tool_call_id: "call-tool-approval-route".to_string(),
            tool_name: "file_write".to_string(),
            reason: "需要写入文件".to_string(),
            requested_at: UtcMillis::now(),
            agent: None,
        };
        let magi_conversation_runtime::ToolApprovalRequestOutcome::Pending(waiter) = state
            .conversation_registry
            .tool_approvals()
            .request_with_arguments(pending, "{}")
            .expect("approval should become pending")
        else {
            panic!("first approval request must wait");
        };
        let app = routes().with_state(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/session/tool-approval")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": session_id,
                            "approvalId": "approval-route-1",
                            "decision": "allow_once",
                        })
                        .to_string(),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("approval route should respond");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            waiter
                .decision_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("runtime should receive approval"),
            ToolApprovalDecision::AllowOnce
        );

        let duplicate_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/session/tool-approval")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": session_id,
                            "approvalId": "approval-route-1",
                            "decision": "deny",
                        })
                        .to_string(),
                    ))
                    .expect("duplicate approval request should build"),
            )
            .await
            .expect("duplicate approval route should respond");
        assert_eq!(
            duplicate_response.status(),
            StatusCode::CONFLICT,
            "重复决定必须被确定性拒绝"
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/session/tool-approvals?sessionId={}",
                        session_id.as_str()
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("approval list route should respond");
        assert_eq!(response.status(), StatusCode::OK);
        let payload: serde_json::Value = serde_json::from_slice(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body should read"),
        )
        .expect("response body should be json");
        assert_eq!(payload["pendingApprovals"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn tool_approval_route_rejects_expired_decision_without_resolved_event() {
        let state = test_state();
        let session_id = SessionId::new("session-tool-approval-expired-route");
        state
            .session_store
            .create_session(session_id.clone(), "expired tool approval route")
            .expect("session should be creatable");
        let pending = PendingToolApproval {
            approval_id: "approval-route-expired-1".to_string(),
            session_id: session_id.clone(),
            task_id: TaskId::new("task-tool-approval-expired-route"),
            turn_id: "turn-tool-approval-expired-route".to_string(),
            tool_call_id: "call-tool-approval-expired-route".to_string(),
            tool_name: "file_write".to_string(),
            reason: "需要写入文件".to_string(),
            requested_at: UtcMillis::now(),
            agent: None,
        };
        let approval_id = pending.approval_id.clone();
        let registry = state.conversation_registry.tool_approvals();
        let magi_conversation_runtime::ToolApprovalRequestOutcome::Pending(waiter) = registry
            .request_with_arguments(pending, "{}")
            .expect("approval should become pending")
        else {
            panic!("first approval request must wait");
        };
        let requested_at = waiter.request.requested_at;
        assert_eq!(
            registry.expire_stale(UtcMillis(
                requested_at.0 + magi_conversation_runtime::TOOL_APPROVAL_TTL_MILLIS + 1,
            )),
            1
        );

        let app = routes().with_state(state.clone());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/session/tool-approval")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": session_id,
                            "approvalId": approval_id,
                            "decision": "allow_once",
                        })
                        .to_string(),
                    ))
                    .expect("expired approval request should build"),
            )
            .await
            .expect("expired approval route should respond");
        assert_eq!(
            response.status(),
            StatusCode::CONFLICT,
            "过期审批决定必须返回确定性冲突"
        );
        assert!(waiter.decision_rx.try_recv().is_err());
        assert!(registry.pending_for_session(&session_id).is_empty());
        assert!(
            state
                .event_bus
                .snapshot()
                .recent_events
                .iter()
                .all(|event| event.event_type != "tool.approval.resolved")
        );
    }

    struct PendingTaskDispatcher;

    impl TaskDispatcher for PendingTaskDispatcher {
        fn dispatch(
            &self,
            _task: &Task,
            _worker: &WorkerInfo,
            _lease: &TaskLease,
            _admission_permit: ExecutionAdmissionPermit,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    fn test_state_with_pending_runner() -> ApiState {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let task_store = Arc::new(TaskStore::new());
        let manager = RunnerManager::with_dispatcher_and_worker_catalog(
            Arc::clone(&task_store),
            Arc::clone(&session_store),
            Arc::new(|| {
                vec![WorkerInfo {
                    worker_id: WorkerId::new("worker-pending-test"),
                    role: "executor".to_string(),
                    supported_kinds: vec![TaskKind::LocalAgent],
                    parallelism_limit: None,
                    system_prompt_template: None,
                }]
            }),
            Arc::new(PendingTaskDispatcher),
        );
        ApiState::new(
            "magi-test",
            event_bus,
            session_store,
            workspace_store,
            governance,
        )
        .with_task_store(task_store)
        .with_runner_manager(manager)
    }

    fn seed_active_plan(
        session_store: &Arc<SessionStore>,
        session_id: &SessionId,
        item_id: &str,
        step: &str,
    ) -> magi_plan::PlanStore {
        let plan_store = magi_plan::PlanStore::new(Arc::clone(session_store), session_id.clone());
        plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: None,
                expected_goal_control_revision: None,
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some(item_id.to_string()),
                    step: step.to_string(),
                    status: magi_core::PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should persist");
        plan_store
    }

    fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            UtcMillis::now().0
        ));
        fs::create_dir_all(&path).expect("temp dir should create");
        path
    }

    #[cfg(unix)]
    fn long_running_shell_command() -> &'static str {
        "sleep 5"
    }

    #[cfg(windows)]
    fn long_running_shell_command() -> &'static str {
        "ping 127.0.0.1 -n 6 >NUL"
    }

    fn register_workspace(state: &ApiState, workspace_id: &str, prefix: &str) -> WorkspaceId {
        let root = unique_temp_dir(prefix);
        let workspace_id = WorkspaceId::new(workspace_id);
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(root.display().to_string()),
            )
            .expect("workspace should register");
        workspace_id
    }

    fn git(path: &std::path::Path, args: &[&str]) {
        let output = magi_process::std_command("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .expect("git fixture command should start");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn register_git_workspace(
        state: &ApiState,
        workspace_id: &str,
        prefix: &str,
    ) -> (WorkspaceId, std::path::PathBuf) {
        let root = unique_temp_dir(prefix);
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.name", "Magi Test"]);
        git(&root, &["config", "user.email", "magi@example.test"]);
        fs::write(root.join("README.md"), "base\n").expect("Git fixture file");
        git(&root, &["add", "README.md"]);
        git(&root, &["commit", "-m", "base"]);
        let workspace_id = WorkspaceId::new(workspace_id);
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(root.display().to_string()),
            )
            .expect("Git workspace should register");
        (workspace_id, root)
    }

    fn append_test_incident(
        state: &ApiState,
        notification_id: &str,
        scope: NotificationScope,
        workspace_id: Option<&str>,
        session_id: Option<&SessionId>,
        message: &str,
    ) {
        state
            .session_store
            .append_incident_record(NotificationRecord {
                notification_id: notification_id.to_string(),
                scope,
                workspace_id: workspace_id.map(str::to_string),
                session_id: session_id.cloned(),
                kind: "incident".to_string(),
                level: Some("error".to_string()),
                title: None,
                message: message.to_string(),
                detail: None,
                error_code: None,
                failure_stage: None,
                task_id: None,
                request_id: None,
                source: Some("test".to_string()),
                created_at: UtcMillis::now(),
                handled: false,
                action_required: true,
                count_unread: true,
                fingerprint: notification_id.to_string(),
                occurrence_count: 1,
                resolved: false,
            })
            .expect("test incident should append");
    }

    #[test]
    fn goal_continuation_prompt_keeps_progress_and_terminal_contract_visible() {
        let goal = SessionGoal {
            goal_id: GoalId::new("goal-prompt"),
            session_id: SessionId::new("session-goal-prompt"),
            thread_id: ThreadId::new("thread-goal-prompt"),
            created_by_turn_id: None,
            objective: "完成任务系统升级并验证".to_string(),
            status: GoalStatus::Active,
            control_revision: 1,
            access_profile: AccessProfile::FullAccess,
            token_budget: Some(4096),
            tokens_used: 1024,
            time_used_seconds: 30,
            time_used_millis: 30_000,
            timing_started_at: None,
            timing_turn_id: None,
            blocker: None,
            continuation: magi_session_store::GoalContinuationState::default(),
            completion: None,
            created_at: UtcMillis(1),
            updated_at: UtcMillis(2),
        };

        let prompt = goal_continuation_prompt(&goal);

        assert!(prompt.contains("完成任务系统升级并验证"));
        assert!(prompt.contains("Tokens used: 1024"));
        assert!(prompt.contains("Token budget: 4096"));
        assert!(prompt.contains("Tokens remaining: 3072"));
        assert!(prompt.contains("update_plan"));
        assert!(prompt.contains("主对话输入区上方"));
        assert!(prompt.contains("必须先调用 get_goal"));
        assert!(prompt.contains("expected_plan_revision 传 null"));
        assert!(prompt.contains("禁止调用 create_goal"));
        assert!(prompt.contains("update_goal(status=\"complete\")"));
        assert!(prompt.contains("update_goal(status=\"blocked\")"));
        assert!(prompt.contains("目标仍为 active 时不要输出面向用户的最终总结"));
        assert_eq!(
            goal.access_profile,
            AccessProfile::FullAccess,
            "Goal 自动续跑必须沿用目标最近一次用户选择的访问模式"
        );
    }

    #[test]
    fn goal_continuation_stops_for_paused_or_blocked_plan() {
        let session_store = Arc::new(SessionStore::new());
        let session_id = SessionId::new("session-plan-continuation-gate");
        session_store
            .create_session(session_id.clone(), "plan continuation gate")
            .expect("session should create");
        let plan_store = seed_active_plan(&session_store, &session_id, "implement", "完成实现");
        assert!(plan_allows_goal_continuation(
            plan_store.snapshot().as_ref()
        ));

        let paused = plan_store
            .pause()
            .expect("plan should pause")
            .expect("plan should exist");
        assert!(!plan_allows_goal_continuation(Some(&paused)));

        let resumed = plan_store
            .resume()
            .expect("plan should resume")
            .expect("plan should exist");
        let blocked = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: Some(resumed.plan_id.to_string()),
                expected_revision: Some(resumed.revision),
                expected_goal_id: None,
                expected_goal_control_revision: None,
                language: resumed.language,
                explanation: None,
                plan: resumed
                    .items
                    .into_iter()
                    .map(|item| magi_plan::UpdatePlanItemInput {
                        item_id: Some(item.item_id.to_string()),
                        step: item.title,
                        status: magi_core::PlanItemStatus::Blocked,
                    })
                    .collect(),
            })
            .expect("plan should block");
        assert!(!plan_allows_goal_continuation(Some(&blocked)));
    }

    async fn post_json(
        state: ApiState,
        uri: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        let status = response.status();
        let body = serde_json::from_slice::<serde_json::Value>(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body should read"),
        )
        .expect("response should be json");
        (status, body)
    }

    #[tokio::test]
    async fn personal_session_navigation_and_lifecycle_are_scope_isolated() {
        let root =
            std::env::temp_dir().join(format!("magi-personal-session-test-{}", UtcMillis::now().0));
        std::fs::create_dir_all(&root).expect("personal state root should create");
        let state = test_state().with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
            root.clone(),
            root.join("workspaces.json"),
            root.join("knowledge.json"),
        )));
        let (draft_status, draft) = post_json(
            state.clone(),
            "/session/navigation",
            json!({ "target": "draft", "scope": "personal" }),
        )
        .await;
        assert_eq!(draft_status, StatusCode::OK, "{draft}");
        assert!(draft["currentSession"].is_null());
        assert!(draft["sessions"].as_array().is_some_and(Vec::is_empty));

        let (turn_status, turn) = post_json(
            state.clone(),
            "/session/turn",
            json!({ "scope": "personal", "text": "创建一个个人会话", "images": [], "contextReferences": [], "browserAnnotationRefs": [] }),
        )
        .await;
        assert_eq!(turn_status, StatusCode::OK, "{turn}");
        let session_id = turn["sessionId"].as_str().expect("personal session id");
        let session = state
            .session_store
            .session(&SessionId::new(session_id))
            .expect("personal session should persist");
        assert!(session.workspace_id.is_none());

        let (navigate_status, navigate) = post_json(
            state.clone(),
            "/session/navigation",
            json!({ "target": "session", "scope": "personal", "sessionId": session_id }),
        )
        .await;
        assert_eq!(navigate_status, StatusCode::OK, "{navigate}");
        assert_eq!(navigate["currentSession"]["sessionId"], session_id);
        assert!(navigate["currentSession"]["workspaceId"].is_null());

        let (rename_status, renamed) = post_json(
            state.clone(),
            "/session/rename",
            json!({ "sessionId": session_id, "name": "个人会话已重命名" }),
        )
        .await;
        assert_eq!(rename_status, StatusCode::OK, "{renamed}");
        assert_eq!(
            state
                .session_store
                .session(&SessionId::new(session_id))
                .expect("session")
                .title,
            "个人会话已重命名"
        );

        let (delete_status, deleted) = post_json(
            state.clone(),
            "/session/delete",
            json!({ "sessionId": session_id }),
        )
        .await;
        assert_eq!(delete_status, StatusCode::OK, "{deleted}");
        assert!(
            state
                .session_store
                .session(&SessionId::new(session_id))
                .is_none()
        );
    }

    #[tokio::test]
    async fn session_viewed_uses_personal_session_scope_without_workspace_binding() {
        let state = test_state();
        let session_id = SessionId::new("session-viewed-personal");
        state
            .session_store
            .create_session(session_id.clone(), "个人已查看会话")
            .expect("personal session should create");
        seed_conversation_turn(
            &state,
            &session_id,
            "turn-viewed-personal",
            1,
            UtcMillis(10),
            "completed",
            "触发个人未读完成",
        );

        let (status, body) = post_json(
            state.clone(),
            "/session/viewed",
            json!({ "sessionId": session_id.as_str() }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["sessionId"], session_id.as_str());
        assert!(body["workspaceId"].is_null());
        assert_eq!(body["hasUnreadCompletion"], false);
        assert!(
            !state
                .session_store
                .session(&session_id)
                .expect("personal session should exist")
                .has_unread_completion()
        );
        let viewed_event = state
            .event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_type == "session.viewed")
            .expect("viewed event should publish");
        assert!(viewed_event.workspace_id.is_none());
    }

    fn session_turn_request(text: &str) -> SessionTurnRequestDto {
        SessionTurnRequestDto {
            desktop_browser_tools_allowed: true,
            session_id: None,
            scope: crate::dto::SessionScopeKindDto::Personal,
            workspace_id: None,
            workspace_path: None,
            text: Some(text.to_string()),
            skill_name: None,
            locale: None,
            goal_mode: false,
            resume: false,
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            browser_node_selections: Vec::new(),
            plugin_selections: Vec::new(),
            access_profile: None,
            orchestrator_session_config: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            steer_current_turn: false,
            expected_turn_id: None,
            replace_turn_id: None,
            command: None,
            workflow_id: None,
            plugin_command_id: None,
        }
    }

    fn compact_command_request(text: Option<&str>) -> SessionTurnRequestDto {
        let mut request = session_turn_request(text.unwrap_or_default());
        request.text = text.map(str::to_string);
        request.session_id = Some("session-compact-command".to_string());
        request.command = Some(magi_app_server_protocol::SessionTurnCommand::Compact);
        request
    }

    #[test]
    fn compact_command_is_a_standalone_conversation_turn() {
        let state = test_state();
        let request = compact_command_request(Some("保留接口约束"));
        validate_session_turn_input(&request).expect("带补充要求的 /compact 应被接受");
        validate_session_turn_input(&compact_command_request(None))
            .expect("不带补充要求的 /compact 也应被接受");

        // 命令不经过意图分类，即使参数文本像执行请求也固定走 conversation。
        let mut execution_like = compact_command_request(Some("运行测试并修改代码"));
        execution_like.session_id = Some("session-compact-command".to_string());
        let decision = decide_session_turn(&state, &execution_like).expect("命令应得到固定决策");
        assert!(matches!(decision.route, SessionTurnRouteDto::Chat));

        assert_eq!(
            request.timeline_content(request.trimmed_text().as_deref()),
            "保留接口约束"
        );
        let bare = compact_command_request(None);
        assert_eq!(bare.timeline_content(bare.trimmed_text().as_deref()), "");
        assert!(!session_turn_request_is_plain_text(&request));
        assert_ne!(
            request.request_fingerprint().expect("fingerprint"),
            {
                let mut plain = request.clone();
                plain.command = None;
                plain.request_fingerprint().expect("fingerprint")
            },
            "命令必须进入请求指纹，避免与同文本普通消息幂等冲突"
        );
    }

    #[test]
    fn web_engine_sessions_always_route_as_plain_chat_even_for_tool_like_text() {
        let state = test_state();
        let text = "请用 Magi 连接器里的工具依次完成：创建目录 e2e-dir，写入文件，搜索文本，并运行测试修改代码";

        // 本地引擎：所有普通消息都进入带工具的主线。
        let local = session_turn_request(text);
        let local_decision = decide_session_turn(&state, &local).unwrap();
        assert!(matches!(local_decision.route, SessionTurnRouteDto::Execute));

        // 新会话首条消息携带 GPT Web 引擎配置：固定普通对话，没有任务标题 / 工具意图改写。
        let mut first_message = session_turn_request(text);
        first_message.orchestrator_session_config =
            Some(serde_json::json!({ "engineId": "plugin/openai.chatgpt-web/default" }));
        let decision = decide_session_turn(&state, &first_message).unwrap();
        assert!(matches!(decision.route, SessionTurnRouteDto::Chat));
        assert!(decision.task_title.is_none() && decision.execution_goal.is_none());

        // 已有会话：读会话设置里的引擎。
        let session_id = magi_core::SessionId::new("session-web-routing");
        state
            .settings_store
            .set_session_section(
                &session_id,
                "orchestrator",
                serde_json::json!({ "engineId": "plugin/openai.chatgpt-web/default" }),
            )
            .unwrap();
        let mut existing = session_turn_request(text);
        existing.session_id = Some(session_id.to_string());
        let decision = decide_session_turn(&state, &existing).unwrap();
        assert!(matches!(decision.route, SessionTurnRouteDto::Chat));
    }

    #[tokio::test]
    async fn compact_command_acceptance_preserves_visible_command_and_argument_separation() {
        for text in [None, Some("保留接口约束")] {
            let state = test_state();
            let session_id = SessionId::new("session-compact-command");
            state
                .session_store
                .create_session(session_id.clone(), "compact")
                .unwrap();
            let mut request = compact_command_request(text);
            request.request_id = Some("request-compact-command".to_string());
            request.user_message_id = Some("user-compact-command".to_string());
            let payload = serde_json::to_value(&request).unwrap();
            let (status, body) = post_json(state.clone(), "/session/turn", payload.clone()).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let user = body["canonicalTurn"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["kind"] == "user_message")
                .unwrap();
            assert_eq!(user["content"], text.unwrap_or_default());
            assert_eq!(user["metadata"]["sessionCommand"], "compact");
            assert_eq!(user["visibility"]["renderable"], true);

            let (status, replay) = post_json(state.clone(), "/session/turn", payload).await;
            assert_eq!(status, StatusCode::OK, "{replay}");
            assert_eq!(replay["replayed"], true);
            assert_eq!(replay["turnId"], body["turnId"]);
            assert_eq!(
                state
                    .session_store
                    .canonical_turns_for_session(&session_id)
                    .len(),
                1
            );
            // 此 fixture 没有 dispatcher：等待真实后台失败收口，不能留下悬挂的执行。
            tokio::time::timeout(Duration::from_secs(5), async {
                while state
                    .turn_coordinator()
                    .current_attempt(&session_id, body["turnId"].as_str().unwrap())
                    .is_ok()
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("conversation should settle");
        }
    }

    #[tokio::test]
    async fn compact_command_queue_preserves_structured_command_after_restore() {
        let root = tempfile::tempdir().unwrap();
        let persistence = || {
            Arc::new(RuntimeStatePersistence::new(
                root.path(),
                root.path().join("workspaces.json"),
                root.path().join("knowledge.json"),
            ))
        };
        let state = test_state().with_runtime_persistence(persistence());
        let session_id = SessionId::new("session-queued-compact");
        let workspace_id = register_workspace(&state, "workspace-queued-compact", "queued-compact");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "queued compact",
                Some(workspace_id.to_string()),
            )
            .unwrap();
        for (index, text) in [None, Some("保留接口约束")].into_iter().enumerate() {
            let mut queued = queued_regular_turn(
                &session_id,
                &workspace_id,
                &format!("queue-compact-{index}"),
                UtcMillis(index as u64 + 10),
            );
            queued.request.command = Some(magi_app_server_protocol::SessionTurnCommand::Compact);
            queued.request.text = text.map(str::to_string);
            state.enqueue_regular_session_turn(queued).unwrap();
        }
        let restored = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            state.session_store.clone(),
            state.workspace_registry.clone(),
            Arc::new(GovernanceService::default()),
        )
        .with_runtime_persistence(persistence());
        assert_eq!(restored.restore_regular_session_turn_queues().unwrap(), 2);
        for state in [state, restored] {
            let Json(queue) = get_session_turn_queue(
                State(state),
                Query(SessionTurnQueueScope {
                    session_id: session_id.to_string(),
                    workspace_id: Some(workspace_id.to_string()),
                    workspace_path: None,
                }),
            )
            .await
            .unwrap();
            let queue = serde_json::to_value(queue).unwrap();
            assert_eq!(queue["queuedTurns"].as_array().unwrap().len(), 2);
            for (index, text) in ["", "保留接口约束"].into_iter().enumerate() {
                assert_eq!(queue["queuedTurns"][index]["command"], "compact");
                assert_eq!(queue["queuedTurns"][index]["content"], text);
                assert_eq!(queue["queuedTurns"][index]["canGuide"], false);
            }
        }
    }

    #[test]
    fn compact_command_rejects_combined_or_sessionless_input() {
        let mut without_session = compact_command_request(None);
        without_session.session_id = None;
        assert!(validate_session_turn_input(&without_session).is_err());

        let mut with_goal = compact_command_request(None);
        with_goal.goal_mode = true;
        assert!(validate_session_turn_input(&with_goal).is_err());

        let mut with_skill = compact_command_request(None);
        with_skill.skill_name = Some("skill".to_string());
        assert!(validate_session_turn_input(&with_skill).is_err());

        let mut as_steer = compact_command_request(None);
        as_steer.steer_current_turn = true;
        assert!(validate_session_turn_input(&as_steer).is_err());

        let mut as_edit = compact_command_request(None);
        as_edit.replace_turn_id = Some("turn-1".to_string());
        assert!(validate_session_turn_input(&as_edit).is_err());
    }

    fn queued_regular_turn(
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
        queue_id: &str,
        accepted_at: UtcMillis,
    ) -> QueuedRegularSessionTurn {
        let mut request = session_turn_request(&format!("queued {queue_id}"));
        request.session_id = Some(session_id.to_string());
        request.scope = crate::dto::SessionScopeKindDto::Workspace;
        request.workspace_id = Some(workspace_id.to_string());
        request.request_id = Some(format!("request-{queue_id}"));
        request.user_message_id = Some(format!("user-{queue_id}"));
        request.placeholder_message_id = Some(format!("assistant-{queue_id}"));
        QueuedRegularSessionTurn {
            request,
            request_fingerprint: None,
            requested_workspace_id: Some(workspace_id.clone()),
            accepted_at,
            route: SessionTurnRouteDto::Chat,
            task_title: None,
            execution_goal: None,
            required_tool_chain: Vec::new(),
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: None,
            session_id: session_id.clone(),
            workspace_id: Some(workspace_id.clone()),
            queue_id: queue_id.to_string(),
            retry_count: 0,
        }
    }

    #[test]
    fn session_turn_request_rejects_legacy_snake_case_fields() {
        serde_json::from_value::<SessionTurnRequestDto>(serde_json::json!({
            "workspace_id": "workspace-turn",
            "session_id": "session-turn",
            "request_id": "request-turn",
            "text": "legacy turn"
        }))
        .expect_err("session turn request 不得继续接受 snake_case 请求字段");

        serde_json::from_value::<SessionTurnRequestDto>(serde_json::json!({
            "scope": "workspace",
            "workspaceId": "workspace-turn",
            "sessionId": "session-turn",
            "requestId": "request-turn",
            "images": [{
                "name": "a.png",
                "data_url": "data:image/png;base64,AA=="
            }],
            "text": "legacy image field"
        }))
        .expect_err("session turn image 不得继续接受 data_url 请求字段");

        let request = serde_json::from_value::<SessionTurnRequestDto>(serde_json::json!({
            "scope": "workspace",
            "workspaceId": "workspace-turn",
            "sessionId": "session-turn",
            "requestId": "request-turn",
            "images": [{
                "name": "a.png",
                "dataUrl": "data:image/png;base64,AA=="
            }],
            "text": "canonical turn"
        }))
        .expect("canonical camelCase session turn request");
        assert_eq!(request.workspace_id.as_deref(), Some("workspace-turn"));
        assert_eq!(request.request_id.as_deref(), Some("request-turn"));
        assert_eq!(request.images[0].data_url, "data:image/png;base64,AA==");
    }

    fn test_root_task(task_id: &str, mission_id: &str) -> Task {
        let now = UtcMillis::now();
        let task_id = TaskId::new(task_id);
        Task {
            task_id: task_id.clone(),
            mission_id: MissionId::new(mission_id),
            root_task_id: task_id,
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: "root task".to_string(),
            goal: "run root task".to_string(),
            status: TaskStatus::Running,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn user_cancelled_turn_continue_restores_original_task_contract() {
        let state = test_state();
        let session_id = SessionId::new("session-user-cancelled-resume");
        let mission_id = MissionId::new("mission-user-cancelled-resume");
        let source_task_id = TaskId::new("task-user-cancelled-resume");
        let now = UtcMillis::now();
        state
            .session_store
            .create_session(session_id.clone(), "用户中断恢复")
            .expect("session should create");
        let (_, orchestrator_thread_id) =
            state
                .session_store
                .ensure_session_mission(&session_id, now, || mission_id.clone());
        let source_thread_id = ThreadId::new("thread-user-cancelled-resume-task");
        state
            .session_store
            .register_thread(ExecutionThread {
                thread_id: source_thread_id.clone(),
                session_id: session_id.clone(),
                mission_id: mission_id.clone(),
                role_id: "coordinator".to_string(),
                worker_instance_id: WorkerId::new("worker-user-cancelled-resume"),
                status: ExecutionThreadStatus::Idle,
                created_at: now,
                last_used_at: now,
                observed_context_window_tokens: None,
                handled_task_ids: vec![source_task_id.clone()],
                message_history: Vec::new(),
            })
            .expect("thread should register");

        let mut source_task = test_root_task(source_task_id.as_str(), mission_id.as_str());
        source_task.executor_binding = Some(
            TaskExecutorBinding::for_role("coordinator").with_required_tool_chain(vec![
                "file_read".to_string(),
                "diagram_render".to_string(),
            ]),
        );
        source_task.completion_contract = TaskCompletionContract::default()
            .with_evidence_requirements(vec![TaskEvidenceRequirement::successful_tool_call(
                "diagram_render",
            )]);
        state
            .task_store()
            .expect("task store should exist")
            .insert_task(source_task)
            .expect("源任务应插入");
        let coordinator = state.turn_coordinator();
        let attempt = coordinator
            .execute_command(
                &session_id,
                TurnCommand::Start(TurnAdmission {
                    turn_id: "turn-user-cancelled-resume".to_string(),
                    request_id: "request-user-cancelled-resume".to_string(),
                    request_fingerprint: "fingerprint-user-cancelled-resume".to_string(),
                    profile: ExecutionProfile::Task,
                }),
            )
            .expect("task Turn should start");
        let attempt = match attempt {
            CoordinatorCommandResult::Admission(CoordinatorAdmission::Accepted(attempt)) => attempt,
            other => panic!("unexpected fixture admission: {other:?}"),
        };
        let turn = ActiveExecutionTurn {
            turn_id: "turn-user-cancelled-resume".to_string(),
            turn_seq: now.0,
            accepted_at: now,
            completed_at: None,
            status: "accepted".to_string(),
            user_message: Some(
                "请先读取真实代码，再调用 diagram_render 生成当前项目流程图".to_string(),
            ),
            items: Vec::new(),
        };
        magi_conversation_runtime::CanonicalTurnEventSink::for_store(&state.session_store, None)
            .accept_conversation_turn_with_timeline_entry(
                session_id.clone(),
                None,
                TimelineEntryInput::new(
                    "timeline-user-cancelled-resume",
                    TimelineEntryKind::UserMessage,
                    "请先读取真实代码，再调用 diagram_render 生成当前项目流程图",
                    now,
                ),
                turn,
            )
            .expect("task turn should persist");
        coordinator
            .execute_command(
                &session_id,
                TurnCommand::SetStatus {
                    attempt: attempt.clone(),
                    status: CoordinatorTurnStatus::Preparing,
                },
            )
            .expect("task turn should prepare");
        coordinator
            .execute_command(
                &session_id,
                TurnCommand::SetStatus {
                    attempt,
                    status: CoordinatorTurnStatus::Running,
                },
            )
            .expect("task turn should run");
        state
            .turn_event_sink()
            .upsert_item_sidecar(
                &session_id,
                Some("turn-user-cancelled-resume"),
                ActiveExecutionTurnItem {
                    item_id: "user-user-cancelled-resume".to_string(),
                    item_seq: 2,
                    kind: "user_message".to_string(),
                    status: "completed".to_string(),
                    source: "user".to_string(),
                    title: None,
                    content: Some(
                        "请先读取真实代码，再调用 diagram_render 生成当前项目流程图".to_string(),
                    ),
                    task_id: Some(source_task_id),
                    worker_id: None,
                    role_id: None,
                    tool_call_id: None,
                    tool_name: None,
                    tool_status: None,
                    tool_arguments: None,
                    tool_result: None,
                    tool_error: None,
                    request_id: None,
                    user_message_id: Some("user-user-cancelled-resume".to_string()),
                    placeholder_message_id: None,
                    metadata: Default::default(),
                    timeline_entry_id: None,
                    source_thread_id: orchestrator_thread_id,
                },
            )
            .expect("task-owned turn item should persist");
        state
            .turn_event_sink()
            .interrupt_turn_by_user(&session_id)
            .expect("turn should be cancellable by user");

        // 只有界面显式发出的 resume 请求才从中断检查点继续；文字“继续”本身不触发恢复。
        let mut typed_continue = session_turn_request("继续");
        typed_continue.session_id = Some(session_id.to_string());
        let typed_decision =
            decide_session_turn(&state, &typed_continue).expect("typed continue is a new turn");
        assert!(typed_decision.recovery_checkpoint.is_none());

        let mut request = session_turn_request("继续");
        request.session_id = Some(session_id.to_string());
        request.resume = true;
        let decision = decide_session_turn(&state, &request)
            .expect("user cancelled turn should resume as a new execution task");

        assert!(matches!(decision.route, SessionTurnRouteDto::Execute));
        assert_eq!(
            decision
                .recovery_checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.source_turn_id.as_str()),
            Some("turn-user-cancelled-resume")
        );
        assert_eq!(
            decision
                .recovery_checkpoint
                .as_ref()
                .map(|checkpoint| &checkpoint.source_thread_id),
            Some(&source_thread_id),
            "恢复必须读取原任务独占 thread，不能读取用户消息所在的主线 thread",
        );
        assert_eq!(
            decision.required_tool_chain,
            ["file_read", "diagram_render"]
        );
        assert_eq!(
            decision.completion_contract.evidence_requirements,
            [TaskEvidenceRequirement::successful_tool_call(
                "diagram_render"
            )]
        );
        assert!(
            decision
                .execution_goal
                .as_deref()
                .is_some_and(|goal| goal.contains("原始用户目标：请先读取真实代码"))
        );

        let mut unrelated_request = session_turn_request("解释一下 Rust 所有权");
        unrelated_request.session_id = Some(session_id.to_string());
        let unrelated_decision = decide_session_turn(&state, &unrelated_request)
            .expect("unrelated request should remain independent");
        assert!(matches!(
            unrelated_decision.route,
            SessionTurnRouteDto::Execute
        ));
        assert!(unrelated_decision.recovery_checkpoint.is_none());
    }

    #[test]
    fn interrupted_recovery_does_not_hijack_an_unrelated_next_input() {
        let state = test_state();
        let session_id = SessionId::new("session-interrupted-recovery-route");
        let mission_id = MissionId::new("mission-interrupted-recovery-route");
        let root_task_id = TaskId::new("task-root-interrupted-recovery-route");
        let branch_task_id = TaskId::new("task-branch-interrupted-recovery-route");
        let now = UtcMillis::now();
        state
            .session_store
            .create_session(session_id.clone(), "异常中断恢复路由")
            .expect("session should create");

        let mut root_task = test_root_task(root_task_id.as_str(), mission_id.as_str());
        root_task.status = TaskStatus::Failed;
        state
            .task_store()
            .expect("task store should exist")
            .insert_task(root_task)
            .expect("根任务应插入");
        let mut branch_task = test_root_task(branch_task_id.as_str(), mission_id.as_str());
        branch_task.root_task_id = root_task_id.clone();
        branch_task.status = TaskStatus::Failed;
        state
            .task_store()
            .expect("task store should exist")
            .insert_task(branch_task)
            .expect("分支任务应插入");

        let turn = ActiveExecutionTurn {
            turn_id: "turn-interrupted-recovery-route".to_string(),
            turn_seq: now.0,
            accepted_at: now,
            status: "running".to_string(),
            completed_at: None,
            user_message: Some("原始任务".to_string()),
            items: Vec::new(),
        };
        state
            .session_store
            .accept_active_execution_chain_with_timeline_entry(
                session_id.clone(),
                TimelineEntryInput::new(
                    "timeline-interrupted-recovery-route",
                    TimelineEntryKind::UserMessage,
                    "原始任务",
                    now,
                ),
                magi_session_store::ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id,
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-interrupted-recovery-route".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![branch_task_id.clone()],
                    active_worker_bindings: vec![WorkerId::new("worker-interrupted-recovery")],
                    branches: vec![magi_session_store::ActiveExecutionBranch {
                        task_id: branch_task_id,
                        worker_id: WorkerId::new("worker-interrupted-recovery"),
                        stage: "execute".to_string(),
                        lease_id: None,
                        execution_intent_ref: None,
                        binding_lifecycle: None,
                        checkpoint_stage: Some("execute".to_string()),
                        next_step_index: Some(0),
                        checkpoint_at: Some(now),
                        resume_mode: Some("resume".to_string()),
                        resume_token: Some("recovery-route".to_string()),
                        use_tools: true,
                        skill_name: None,
                        is_primary: true,
                        thread_id: ThreadId::new("thread-interrupted-recovery-route"),
                    }],
                    recovery_ref: None,
                    dispatch_context: magi_session_store::ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-interrupted-recovery-route".to_string(),
                        trimmed_text: Some("原始任务".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(turn),
                },
            )
            .expect("active execution chain should persist");
        state
            .turn_event_sink()
            .interrupt_turn_by_daemon_restart(&session_id)
            .expect("daemon restart interruption should persist");

        for text in ["补充一个新的约束", "看看以上内容"] {
            let mut request = session_turn_request(text);
            request.session_id = Some(session_id.to_string());
            let decision = decide_session_turn(&state, &request)
                .expect("recovery-ready session should accept an unrelated new turn");
            assert!(matches!(decision.route, SessionTurnRouteDto::Execute));
        }

        let mut continue_request = session_turn_request("继续");
        continue_request.session_id = Some(session_id.to_string());
        continue_request.resume = true;
        let continue_decision = decide_session_turn(&state, &continue_request)
            .expect("explicit continuation should still resume the recovery-ready chain");
        assert!(matches!(
            continue_decision.route,
            SessionTurnRouteDto::Continue
        ));
    }

    #[test]
    fn continue_input_waits_for_canonical_acceptance_before_projection() {
        let state = test_state();
        let session_id = SessionId::new("session-resume-input");
        let mission_id = MissionId::new("mission-resume-input");
        let now = UtcMillis::now();
        state
            .session_store
            .create_session(session_id.clone(), "恢复输入持久化")
            .expect("session should create");

        let branches = ["primary", "child"]
            .into_iter()
            .map(|suffix| {
                let task_id = TaskId::new(format!("task-{suffix}"));
                let worker_id = WorkerId::new(format!("worker-{suffix}"));
                let thread_id = ThreadId::new(format!("thread-{suffix}"));
                state
                    .session_store
                    .register_thread(ExecutionThread {
                        thread_id: thread_id.clone(),
                        session_id: session_id.clone(),
                        mission_id: mission_id.clone(),
                        role_id: "executor".to_string(),
                        worker_instance_id: worker_id.clone(),
                        status: ExecutionThreadStatus::Active,
                        created_at: now,
                        last_used_at: now,
                        observed_context_window_tokens: None,
                        handled_task_ids: vec![task_id.clone()],
                        message_history: Vec::new(),
                    })
                    .expect("thread should register");
                ActiveExecutionBranch {
                    task_id,
                    worker_id,
                    stage: "execute".to_string(),
                    lease_id: None,
                    execution_intent_ref: None,
                    binding_lifecycle: None,
                    checkpoint_stage: Some("execute".to_string()),
                    next_step_index: Some(0),
                    checkpoint_at: Some(now),
                    resume_mode: Some("resume".to_string()),
                    resume_token: Some(format!("resume-{suffix}")),
                    use_tools: true,
                    skill_name: None,
                    is_primary: suffix == "primary",
                    thread_id,
                }
            })
            .collect::<Vec<_>>();
        let image = magi_conversation_runtime::session_images::SessionTurnImage::from_data_url(
            "context.png",
            "data:image/png;base64,AAA",
        )
        .expect("image should parse");

        persist_resumed_branch_user_input(
            &state,
            &session_id,
            &branches,
            Some("继续，但先遵守新增约束"),
            &[image],
            now,
        )
        .expect("continue input should persist");

        for branch in branches {
            // 用户输入尚未写入 canonical Turn；该阶段只做 thread 归属校验，
            // 不允许先把 ThreadChatMessage 当作独立事实写入。
            assert!(
                state
                    .session_store
                    .thread_message_history(&branch.thread_id)
                    .is_empty()
            );
        }
    }

    #[test]
    fn continue_rebuilds_missing_branch_thread_with_tool_history() {
        let state = test_state();
        let session_id = SessionId::new("session-recovered-thread");
        let mission_id = MissionId::new("mission-recovered-thread");
        let task_id = TaskId::new("task-recovered-thread");
        let worker_id = WorkerId::new("worker-recovered-thread");
        let thread_id = ThreadId::new("thread-recovered-thread");
        let now = UtcMillis(4_000);
        state
            .session_store
            .create_session(session_id.clone(), "旧会话 thread 恢复")
            .expect("session should create");
        let mut task = test_root_task(task_id.as_str(), mission_id.as_str());
        task.status = TaskStatus::Failed;
        state
            .task_store()
            .expect("task store should exist")
            .insert_task(task)
            .expect("任务应插入");
        CanonicalTurnEventSink::for_store(&state.session_store, None)
            .accept_conversation_turn_with_timeline_entry(
                session_id.clone(),
                None,
                TimelineEntryInput::new(
                    "timeline-recovered-thread",
                    TimelineEntryKind::UserMessage,
                    "检查项目",
                    now,
                ),
                ActiveExecutionTurn {
                    turn_id: "turn-recovered-thread".to_string(),
                    turn_seq: now.0,
                    accepted_at: now,
                    status: "interrupted".to_string(),
                    completed_at: Some(UtcMillis(now.0 + 1)),
                    user_message: Some("检查项目".to_string()),
                    items: vec![ActiveExecutionTurnItem {
                        item_id: "tool-recovered-thread".to_string(),
                        item_seq: 1,
                        kind: "tool_call_result".to_string(),
                        status: "completed".to_string(),
                        source: "tool".to_string(),
                        title: Some("file_read".to_string()),
                        content: Some("读取完成".to_string()),
                        task_id: Some(task_id.clone()),
                        worker_id: Some(worker_id.clone()),
                        role_id: Some("coordinator".to_string()),
                        tool_call_id: Some("call-recovered-read".to_string()),
                        tool_name: Some("file_read".to_string()),
                        tool_status: Some("completed".to_string()),
                        tool_arguments: Some(r#"{"path":"README.md"}"#.to_string()),
                        tool_result: Some(
                            r##"{"status":"succeeded","content":"# Magi"}"##.to_string(),
                        ),
                        tool_error: None,
                        request_id: None,
                        user_message_id: None,
                        placeholder_message_id: None,
                        metadata: Default::default(),
                        timeline_entry_id: None,
                        source_thread_id: thread_id.clone(),
                    }],
                },
            )
            .expect("canonical turn should persist");
        let branch = ActiveExecutionBranch {
            task_id: task_id.clone(),
            worker_id: worker_id.clone(),
            stage: "execute".to_string(),
            lease_id: None,
            execution_intent_ref: None,
            binding_lifecycle: None,
            checkpoint_stage: Some("execute".to_string()),
            next_step_index: Some(0),
            checkpoint_at: Some(now),
            resume_mode: Some("stage-restart".to_string()),
            resume_token: None,
            use_tools: true,
            skill_name: None,
            is_primary: true,
            thread_id: thread_id.clone(),
        };
        let chain = magi_session_store::ActiveExecutionChain {
            session_id: session_id.clone(),
            mission_id,
            root_task_id: task_id.clone(),
            execution_chain_ref: "chain-recovered-thread".to_string(),
            workspace_id: None,
            active_branch_task_ids: vec![task_id],
            active_worker_bindings: vec![worker_id],
            branches: vec![branch.clone()],
            recovery_ref: None,
            dispatch_context: magi_session_store::ActiveExecutionDispatchContext {
                accepted_at: now,
                entry_id: "timeline-recovered-thread".to_string(),
                trimmed_text: Some("检查项目".to_string()),
                skill_name: None,
            },
            current_turn: None,
        };

        assert_eq!(
            restore_missing_resumed_branch_threads(&state, &session_id, &chain, &[branch])
                .expect("missing thread should rebuild")
                .len(),
            1
        );
        let threads = state.session_store.thread_registry_snapshot(&session_id);
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].thread_id, thread_id);
        assert_eq!(threads[0].message_history.len(), 2);
        assert_eq!(threads[0].message_history[0].role, "assistant");
        assert_eq!(threads[0].message_history[0].tool_calls.len(), 1);
        assert_eq!(
            threads[0].message_history[0].tool_calls[0].function.name,
            "file_read"
        );
        assert_eq!(threads[0].message_history[1].role, "tool");
        assert_eq!(
            threads[0].message_history[1].tool_call_id.as_deref(),
            Some("call-recovered-read")
        );
    }

    #[tokio::test]
    async fn a_skill_picked_for_a_plain_chat_turn_reaches_the_turn_instead_of_being_dropped() {
        let state = test_state();
        let (status, body) = post_json(
            state.clone(),
            "/session/turn",
            serde_json::json!({
                "scope": "personal",
                "text": "带我配置",
                "skillName": " magi-cloudflare-tunnel ",
                "requestId": "request-skill-chat",
                "userMessageId": "message-skill-chat",
                "images": [],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["route"], "execute", "普通文本进入当前 task 主线");

        let turn = state
            .session_store
            .canonical_turn_for_request_id("request-skill-chat")
            .expect("turn should be accepted");
        let user = turn
            .items
            .iter()
            .find(|item| item.kind == CanonicalTurnItemKind::UserMessage)
            .expect("user item");
        assert_eq!(
            user.metadata.get("skillName").and_then(Value::as_str),
            Some("magi-cloudflare-tunnel"),
            "所选技能必须记在这一轮上并交给执行，而不是被对话路径丢弃"
        );
    }

    #[tokio::test]
    async fn session_turn_without_workspace_creates_a_personal_session() {
        let (status, body) = post_json(
            test_state(),
            "/session/turn",
            serde_json::json!({
                "scope": "personal",
                "text": "你好",
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert!(body["createdSession"].as_bool().unwrap_or(false));
        assert!(body["sessionId"].as_str().is_some_and(|id| !id.is_empty()));
    }

    #[tokio::test]
    async fn session_turn_requires_an_explicit_scope() {
        let response = crate::routes::build_router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/api/session/turn")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "text": "不得猜测会话作用域",
                            "images": [],
                        })
                        .to_string(),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = serde_json::from_slice::<serde_json::Value>(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body should read"),
        )
        .expect("framework rejection should use canonical json error");
        assert_eq!(body["error_code"], "REQUEST_BODY_INVALID");
    }

    #[tokio::test]
    async fn session_turn_rejects_workspace_binding_for_personal_scope() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-personal-scope-conflict",
            "personal-scope-conflict",
        );
        let (status, body) = post_json(
            state,
            "/session/turn",
            serde_json::json!({
                "scope": "personal",
                "workspaceId": workspace_id.as_str(),
                "text": "不得把项目绑定塞进个人作用域",
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("个人会话不能携带 workspace 绑定")),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn session_turn_rejects_workspace_scope_for_personal_history() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-personal-history-conflict",
            "personal-history-conflict",
        );
        let session_id = SessionId::new("session-personal-history-conflict");
        state
            .session_store
            .create_session(session_id.clone(), "个人历史会话")
            .expect("personal session should create");

        let (status, body) = post_json(
            state,
            "/session/turn",
            serde_json::json!({
                "scope": "workspace",
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
                "text": "不得重解释个人历史会话",
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("不属于 workspace")),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn session_turn_rejects_personal_scope_for_workspace_history() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-project-history-conflict",
            "project-history-conflict",
        );
        let session_id = SessionId::new("session-project-history-conflict");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "项目历史会话",
                Some(workspace_id.to_string()),
            )
            .expect("workspace session should create");

        let (status, body) = post_json(
            state,
            "/session/turn",
            serde_json::json!({
                "scope": "personal",
                "sessionId": session_id.as_str(),
                "text": "不得重解释项目历史会话",
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("不属于个人会话")),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn session_turn_rejects_invalid_context_reference_before_accepting_turn() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-invalid-context-reference",
            "invalid-context-reference",
        );
        let missing_path = unique_temp_dir("missing-context-reference").join("missing.md");

        let (status, body) = post_json(
            state,
            "/session/turn",
            serde_json::json!({
                "scope": "workspace",
                "workspaceId": workspace_id.to_string(),
                "text": "分析引用",
                "contextReferences": [{
                    "kind": "file",
                    "path": missing_path,
                    "name": "missing.md"
                }]
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("上下文引用不可用"),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn session_turn_accepts_context_reference_without_text() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-reference-only-turn",
            "reference-only-turn",
        );
        let external_dir = unique_temp_dir("reference-only-turn-external");
        let external_file = external_dir.join("reference.md");
        fs::write(&external_file, "REFERENCE_ONLY").expect("reference file should write");
        let canonical_external_file = external_file
            .canonicalize()
            .expect("reference file should canonicalize");

        let (status, body) = post_json(
            state,
            "/session/turn",
            serde_json::json!({
                "scope": "workspace",
                "workspaceId": workspace_id.to_string(),
                "contextReferences": [{
                    "kind": "file",
                    "path": external_file,
                    "name": "reference.md"
                }],
                "requestId": "request-reference-only-turn",
                "userMessageId": "user-reference-only-turn",
                "placeholderMessageId": "assistant-reference-only-turn"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(
            body["canonicalItem"]["metadata"]["contextReferences"][0]["path"],
            canonical_external_file.display().to_string()
        );
    }

    #[tokio::test]
    async fn session_turn_rejects_more_than_twenty_context_references() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-too-many-context-references",
            "too-many-context-references",
        );
        let external_dir = unique_temp_dir("too-many-context-references-external");
        let external_file = external_dir.join("reference.md");
        fs::write(&external_file, "REFERENCE").expect("reference file should write");
        let references = (0..21)
            .map(|index| {
                serde_json::json!({
                    "kind": "file",
                    "path": external_file,
                    "name": format!("reference-{index}.md")
                })
            })
            .collect::<Vec<_>>();

        let (status, body) = post_json(
            state,
            "/session/turn",
            serde_json::json!({
                "scope": "workspace",
                "workspaceId": workspace_id.to_string(),
                "text": "分析引用",
                "contextReferences": references
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("单轮最多添加 20 个"),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn session_interrupt_requires_explicit_workspace_and_session_scope() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-interrupt-scope",
            "session-interrupt-scope",
        );
        let session_id = SessionId::new("session-interrupt-scope");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "中断 scope 会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");

        let (status, body) = post_json(
            state.clone(),
            "/session/interrupt",
            serde_json::json!({ "sessionId": session_id.as_str() }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("workspaceId 不能为空"),
            "unexpected body: {body}"
        );

        let (status, body) = post_json(
            state.clone(),
            "/session/interrupt",
            serde_json::json!({ "workspaceId": workspace_id.as_str() }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("sessionId 不能为空"),
            "unexpected body: {body}"
        );

        let (status, body) = post_json(
            state,
            "/session/interrupt",
            serde_json::json!({
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["sessionId"], session_id.as_str());
        assert_eq!(body["workspaceId"], workspace_id.as_str());
    }

    #[tokio::test]
    async fn session_interrupt_cancels_shell_by_session_scope() {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let governance = Arc::new(GovernanceService::default());
        let mut tool_registry = ToolRegistry::new(governance.clone(), event_bus.clone());
        tool_registry.register_default_builtins();
        let runner_registry = tool_registry.clone();
        let state = ApiState::new(
            "magi-test",
            event_bus,
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            governance,
        )
        .with_tool_registry(tool_registry);
        let workspace_id = register_workspace(
            &state,
            "workspace-interrupt-shell-session-scope",
            "session-interrupt-shell-session-scope",
        );
        let session_id = SessionId::new("session-interrupt-shell-session-scope");
        let turn_id = "turn-interrupt-shell-session-scope".to_string();
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "中断 shell 会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        seed_conversation_turn(
            &state,
            &session_id,
            &turn_id,
            1,
            UtcMillis(1),
            "running",
            "执行长命令",
        );

        let runner_session_id = session_id.clone();
        let runner_workspace_id = workspace_id.clone();
        let runner = std::thread::spawn(move || {
            runner_registry.execute_with_policy(
                ToolExecutionInput::for_builtin_invocation(
                    ToolCallId::new("tool-call-interrupt-shell-session-scope"),
                    BuiltinToolName::ShellExec.as_str(),
                    serde_json::json!({
                        "command": long_running_shell_command(),
                        "timeout_ms": 10_000
                    })
                    .to_string(),
                ),
                ToolExecutionContext {
                    session_id: Some(runner_session_id),
                    // shell_exec 的执行根必须来自 workspace；本测试验证的是中断
                    // 按 session 取消，而不是允许无 workspace 执行 shell。
                    workspace_id: Some(runner_workspace_id),
                    access_profile: AccessProfile::FullAccess,
                    ..ToolExecutionContext::default()
                },
                &ToolExecutionPolicy {
                    access_profile: AccessProfile::FullAccess,
                    ..ToolExecutionPolicy::default()
                },
            )
        });
        std::thread::sleep(std::time::Duration::from_millis(150));
        let cancel_started = std::time::Instant::now();

        let (status, body) = post_json(
            state,
            "/session/interrupt",
            serde_json::json!({
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["cancelledToolProcessCount"], 1);
        let output = runner.join().expect("shell execution thread should join");
        assert!(
            cancel_started.elapsed() < std::time::Duration::from_secs(2),
            "session interrupt should not wait for shell timeout"
        );
        assert_eq!(output.status, ExecutionResultStatus::Cancelled);
    }

    #[tokio::test]
    async fn session_interrupt_publishes_terminal_canonical_payload() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-interrupt-canonical",
            "session-interrupt-canonical",
        );
        let session_id = SessionId::new("session-interrupt-canonical");
        let accepted_at = UtcMillis(1777000000300);
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "中断 canonical 会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let (_mission_id, orchestrator_thread_id) =
            state
                .session_store
                .ensure_session_mission(&session_id, accepted_at, || {
                    MissionId::new("mission-interrupt-canonical")
                });
        seed_conversation_turn(
            &state,
            &session_id,
            "turn-interrupt-canonical",
            accepted_at.0,
            accepted_at,
            "running",
            "请生成一段长内容",
        );
        state
            .turn_event_sink()
            .upsert_item_sidecar(
                &session_id,
                Some("turn-interrupt-canonical"),
                ActiveExecutionTurnItem {
                    item_id: "assistant-interrupt-canonical".to_string(),
                    item_seq: 2,
                    kind: "assistant_stream".to_string(),
                    status: "running".to_string(),
                    source: "orchestrator".to_string(),
                    title: Some("生成回复".to_string()),
                    content: Some("生成中".to_string()),
                    task_id: None,
                    worker_id: None,
                    role_id: None,
                    tool_call_id: None,
                    tool_name: None,
                    tool_status: None,
                    tool_arguments: None,
                    tool_result: None,
                    tool_error: None,
                    request_id: Some("request-interrupt-canonical".to_string()),
                    user_message_id: Some("user-interrupt-canonical".to_string()),
                    placeholder_message_id: Some("assistant-interrupt-canonical".to_string()),
                    metadata: Default::default(),
                    timeline_entry_id: None,
                    source_thread_id: orchestrator_thread_id.clone(),
                },
            )
            .expect("assistant stream item should persist");
        state
            .turn_coordinator()
            .begin_session_turn_input(session_id.clone(), "turn-interrupt-canonical".to_string())
            .expect("turn input should begin");
        let plan_store = seed_active_plan(
            &state.session_store,
            &session_id,
            "generate-long-content",
            "生成长内容",
        );

        let (status, body) = post_json(
            state.clone(),
            "/session/interrupt",
            serde_json::json!({
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["interrupted"], true);
        let interrupted_event = state
            .event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_type == "session.turn.interrupted")
            .expect("interrupted event should be published");
        assert_eq!(
            interrupted_event.payload["canonical_schema_version"],
            CANONICAL_TURN_SCHEMA_VERSION
        );
        assert_eq!(
            interrupted_event.payload["canonical_event_kind"],
            "turn_completed"
        );
        assert_eq!(
            interrupted_event.payload["canonical_turn"]["turnId"],
            "turn-interrupt-canonical"
        );
        assert_eq!(
            interrupted_event.payload["canonical_turn"]["status"],
            "cancelled"
        );
        assert_eq!(
            interrupted_event.payload["canonical_turn"]["items"][0]["metadata"]["interruptionSource"],
            "user"
        );
        assert_eq!(
            interrupted_event.payload["canonical_item"]["itemId"],
            "assistant-interrupt-canonical"
        );
        assert_eq!(
            interrupted_event.payload["canonical_item"]["status"],
            "cancelled"
        );
        let plan = plan_store.snapshot().expect("plan should remain visible");
        assert_eq!(plan.state, magi_core::PlanState::Paused);
        assert_eq!(plan.items[0].status, magi_core::PlanItemStatus::InProgress);
        assert!(
            !state
                .turn_coordinator()
                .close_session_turn_input(&session_id, "turn-interrupt-canonical")
        );
    }

    #[tokio::test]
    async fn close_session_cancels_active_turn_and_releases_turn_input() {
        let state = test_state();
        let workspace_id =
            register_workspace(&state, "workspace-close-active-turn", "close-active-turn");
        let session_id = SessionId::new("session-close-active-turn");
        let turn_id = "turn-close-active-turn".to_string();
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "关闭活跃会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        seed_conversation_turn(
            &state,
            &session_id,
            &turn_id,
            1,
            UtcMillis(1),
            "running",
            "仍在执行",
        );
        state
            .turn_coordinator()
            .begin_session_turn_input(session_id.clone(), turn_id)
            .expect("turn input should begin");
        let plan_store = seed_active_plan(
            &state.session_store,
            &session_id,
            "execute-current-step",
            "执行当前步骤",
        );

        let (status, body) = post_json(
            state.clone(),
            "/session/close",
            serde_json::json!({
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(
            state
                .session_store
                .session(&session_id)
                .expect("session should remain archived")
                .status,
            SessionLifecycleStatus::Archived
        );
        assert_eq!(
            state
                .session_store
                .runtime_sidecar(&session_id)
                .and_then(|sidecar| sidecar.current_turn)
                .expect("cancelled turn should remain visible")
                .status,
            "cancelled"
        );
        let plan = plan_store.snapshot().expect("plan should remain visible");
        assert_eq!(plan.state, magi_core::PlanState::Paused);
        assert_eq!(plan.items[0].status, magi_core::PlanItemStatus::InProgress);
        assert!(
            !state
                .turn_coordinator()
                .close_session_turn_input(&session_id, "turn-close-active-turn")
        );
    }

    #[tokio::test]
    async fn close_session_stops_background_process_without_active_turn() {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let governance = Arc::new(GovernanceService::default());
        let mut tool_registry = ToolRegistry::new(governance.clone(), event_bus.clone());
        tool_registry.register_default_builtins();
        let runner_registry = tool_registry.clone();
        let state = ApiState::new(
            "magi-test",
            event_bus,
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            governance,
        )
        .with_tool_registry(tool_registry);
        let workspace_id = register_workspace(
            &state,
            "workspace-close-background-process",
            "close-background-process",
        );
        let session_id = SessionId::new("session-close-background-process");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "关闭后台进程会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let context = ToolExecutionContext {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            working_directory: Some(unique_temp_dir("close-background-process-cwd")),
            access_profile: AccessProfile::FullAccess,
            ..ToolExecutionContext::default()
        };
        let launch = runner_registry.execute_with_policy(
            ToolExecutionInput::for_builtin_invocation(
                ToolCallId::new("tool-call-close-background-process"),
                BuiltinToolName::ShellExec.as_str(),
                serde_json::json!({
                    "command": long_running_shell_command(),
                    "background": true
                })
                .to_string(),
            ),
            context.clone(),
            &ToolExecutionPolicy {
                access_profile: AccessProfile::FullAccess,
                ..ToolExecutionPolicy::default()
            },
        );
        assert_eq!(launch.status, ExecutionResultStatus::Succeeded);

        let (status, body) = post_json(
            state,
            "/session/close",
            serde_json::json!({
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        let list = runner_registry.execute_with_policy(
            ToolExecutionInput::for_builtin_invocation(
                ToolCallId::new("tool-call-list-after-close"),
                BuiltinToolName::ShellExec.as_str(),
                serde_json::json!({ "action": "list" }).to_string(),
            ),
            context,
            &ToolExecutionPolicy {
                access_profile: AccessProfile::FullAccess,
                ..ToolExecutionPolicy::default()
            },
        );
        let payload: serde_json::Value =
            serde_json::from_str(&list.payload).expect("process list json");
        assert!(
            payload["processes"]
                .as_array()
                .expect("processes")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn continue_session_requires_matching_workspace_scope() {
        let state = test_state();
        let workspace_id =
            register_workspace(&state, "workspace-continue-scope", "session-continue-scope");
        let foreign_workspace_id = register_workspace(
            &state,
            "workspace-continue-foreign",
            "session-continue-foreign",
        );
        let session_id = SessionId::new("session-continue-scope");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "继续 scope 会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");

        let (status, body) = post_json(
            state.clone(),
            "/session/continue",
            serde_json::json!({ "sessionId": session_id.as_str() }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("workspaceId 不能为空"),
            "unexpected body: {body}"
        );

        let (status, body) = post_json(
            state,
            "/session/continue",
            serde_json::json!({
                "workspaceId": foreign_workspace_id.as_str(),
                "sessionId": session_id.as_str(),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("不属于 workspace"),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn continue_session_checks_git_context_before_restarting_runner() {
        let state = test_state_with_pending_runner();
        let (workspace_id, workspace_root) = register_git_workspace(
            &state,
            "workspace-continue-git-context",
            "continue-git-context",
        );
        let session_id = SessionId::new("session-continue-git-context");
        let mission_id = MissionId::new("mission-continue-git-context");
        let root_task_id = TaskId::new("task-root-continue-git-context");
        let branch_task_id = TaskId::new("task-branch-continue-git-context");
        let worker_id = WorkerId::new("worker-pending-test");
        let now = UtcMillis::now();
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "继续前校验 Git context",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");

        let mut root_task = test_root_task(root_task_id.as_str(), mission_id.as_str());
        root_task.status = TaskStatus::Running;
        let mut branch_task = test_root_task(branch_task_id.as_str(), mission_id.as_str());
        branch_task.root_task_id = root_task_id.clone();
        branch_task.parent_task_id = Some(root_task_id.clone());
        branch_task.status = TaskStatus::Failed;
        let task_store = state.task_store().expect("task store should exist");
        task_store.insert_task(root_task).expect("根任务应插入");
        task_store.insert_task(branch_task).expect("分支任务应插入");

        state
            .session_store
            .accept_active_execution_chain_with_timeline_entry(
                session_id.clone(),
                TimelineEntryInput::new(
                    "timeline-continue-git-context",
                    TimelineEntryKind::UserMessage,
                    "继续测试",
                    now,
                ),
                magi_session_store::ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-continue-git-context".to_string(),
                    workspace_id: Some(workspace_id.clone()),
                    active_branch_task_ids: vec![branch_task_id.clone()],
                    active_worker_bindings: vec![worker_id.clone()],
                    branches: vec![ActiveExecutionBranch {
                        task_id: branch_task_id,
                        worker_id,
                        stage: "execute".to_string(),
                        lease_id: None,
                        execution_intent_ref: None,
                        binding_lifecycle: None,
                        checkpoint_stage: Some("execute".to_string()),
                        next_step_index: Some(0),
                        checkpoint_at: Some(now),
                        resume_mode: Some("resume".to_string()),
                        resume_token: Some("continue-git-context".to_string()),
                        use_tools: true,
                        skill_name: None,
                        is_primary: true,
                        thread_id: ThreadId::new("thread-continue-git-context"),
                    }],
                    recovery_ref: None,
                    dispatch_context: magi_session_store::ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-continue-git-context".to_string(),
                        trimmed_text: Some("继续测试".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-continue-git-context".to_string(),
                        turn_seq: now.0,
                        accepted_at: now,
                        status: "running".to_string(),
                        completed_at: None,
                        user_message: Some("继续测试".to_string()),
                        items: Vec::new(),
                    }),
                },
            )
            .expect("active execution chain should persist");
        state
            .turn_event_sink()
            .interrupt_turn_by_daemon_restart(&session_id)
            .expect("interrupted turn should persist");
        state
            .ensure_snapshot_session(&session_id, &workspace_root)
            .await
            .expect("snapshot baseline");
        state
            .ensure_session_code_context(&session_id, &Some(workspace_id.clone()))
            .await
            .expect("initial Git context");
        state.release_session_git_execution_lease(&session_id);
        git(&workspace_root, &["switch", "-c", "external/branch"]);

        let (status, body) = post_json(
            state.clone(),
            "/session/continue",
            serde_json::json!({
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("Git context 发生高风险变化")),
            "unexpected body: {body}"
        );
        assert_eq!(
            task_store
                .get_task(&root_task_id)
                .expect("root task")
                .status,
            TaskStatus::Running,
            "Git 校验失败时不能重启或改写原执行链"
        );
        let observation = state
            .git_service
            .observe(&workspace_root)
            .await
            .expect("Git observation");
        assert!(
            !state
                .workspace_git_coordinator
                .session_holds_execution(&session_id.to_string(), &observation.git_common_dir),
            "继续前 Git 校验失败必须释放执行租约"
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn daemon_interrupted_session_continue_resumes_owned_paused_goal() {
        let state = test_state_with_pending_runner();
        let workspace_id = register_workspace(
            &state,
            "workspace-interrupted-goal-continue",
            "interrupted-goal-continue",
        );
        let session_id = SessionId::new("session-interrupted-goal-continue");
        let mission_id = MissionId::new("mission-interrupted-goal-continue");
        let root_task_id = TaskId::new("task-root-interrupted-goal-continue");
        let branch_task_id = TaskId::new("task-branch-interrupted-goal-continue");
        let worker_id = WorkerId::new("worker-pending-test");
        let now = UtcMillis::now();
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "异常中断 Goal 恢复",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let (_, orchestrator_thread_id) =
            state
                .session_store
                .ensure_session_mission(&session_id, now, || mission_id.clone());
        let goal = state
            .session_store
            .create_goal(
                session_id.clone(),
                orchestrator_thread_id,
                root_task_id.to_string(),
                "恢复异常中断执行链并同步目标状态",
                AccessProfile::FullAccess,
                None,
            )
            .expect("goal should create");

        let mut root_task = test_root_task(root_task_id.as_str(), mission_id.as_str());
        root_task.status = TaskStatus::Running;
        let mut branch_task = test_root_task(branch_task_id.as_str(), mission_id.as_str());
        branch_task.root_task_id = root_task_id.clone();
        branch_task.parent_task_id = Some(root_task_id.clone());
        branch_task.status = TaskStatus::Failed;
        let task_store = state.task_store().expect("task store should exist");
        task_store.insert_task(root_task).expect("根任务应插入");
        task_store.insert_task(branch_task).expect("分支任务应插入");

        let mut current_turn = ActiveExecutionTurn {
            turn_id: "turn-interrupted-goal-continue".to_string(),
            turn_seq: now.0,
            accepted_at: now,
            status: "running".to_string(),
            completed_at: None,
            user_message: Some("推进长期目标".to_string()),
            items: Vec::new(),
        };
        current_turn
            .items
            .push(magi_session_store::ActiveExecutionTurnItem {
                item_id: "turn-item-interrupted-goal-continue".to_string(),
                item_seq: 0,
                kind: "task_status".to_string(),
                status: "running".to_string(),
                source: "coordinator".to_string(),
                title: None,
                content: Some("推进长期目标".to_string()),
                task_id: Some(root_task_id.clone()),
                worker_id: None,
                role_id: None,
                tool_call_id: None,
                tool_name: None,
                tool_status: None,
                tool_arguments: None,
                tool_result: None,
                tool_error: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
                metadata: Default::default(),
                timeline_entry_id: None,
                source_thread_id: ThreadId::new("thread-interrupted-goal-continue"),
            });
        state
            .session_store
            .accept_goal_continuation_with_timeline_entry(
                session_id.clone(),
                &goal.goal_id,
                TimelineEntryInput::new(
                    "timeline-interrupted-goal-continue",
                    TimelineEntryKind::UserMessage,
                    "推进长期目标",
                    now,
                ),
                magi_session_store::ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-interrupted-goal-continue".to_string(),
                    workspace_id: Some(workspace_id.clone()),
                    active_branch_task_ids: vec![branch_task_id.clone()],
                    active_worker_bindings: vec![worker_id.clone()],
                    branches: vec![ActiveExecutionBranch {
                        task_id: branch_task_id,
                        worker_id,
                        stage: "execute".to_string(),
                        lease_id: None,
                        execution_intent_ref: None,
                        binding_lifecycle: None,
                        checkpoint_stage: Some("execute".to_string()),
                        next_step_index: Some(0),
                        checkpoint_at: Some(now),
                        resume_mode: Some("resume".to_string()),
                        resume_token: Some("interrupted-goal-continue".to_string()),
                        use_tools: true,
                        skill_name: None,
                        is_primary: true,
                        thread_id: ThreadId::new("thread-interrupted-goal-continue"),
                    }],
                    recovery_ref: None,
                    dispatch_context: magi_session_store::ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-interrupted-goal-continue".to_string(),
                        trimmed_text: Some("推进长期目标".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(current_turn),
                },
            )
            .expect("goal execution chain should persist");
        state
            .turn_event_sink()
            .interrupt_turn_by_daemon_restart(&session_id)
            .expect("daemon interruption should persist");
        let active_goal = state
            .session_store
            .current_goal(&session_id)
            .expect("goal should remain");
        state
            .session_store
            .pause_goal_with_plan(
                &session_id,
                &goal.goal_id,
                active_goal.control_revision,
                None,
            )
            .expect("goal should pause before recovery");

        let (status, body) = post_json(
            state.clone(),
            "/session/continue",
            serde_json::json!({
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        let resumed_goal = state
            .session_store
            .current_goal(&session_id)
            .expect("goal should remain after recovery");
        assert_eq!(resumed_goal.status, GoalStatus::Active);
        assert_eq!(
            resumed_goal.continuation.phase,
            magi_session_store::GoalContinuationPhase::Running
        );
        let current_turn = state
            .session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("resumed turn should exist");
        assert_eq!(current_turn.status, "running");
        assert_eq!(
            resumed_goal.continuation.turn_id.as_deref(),
            Some(current_turn.turn_id.as_str())
        );
        assert_eq!(
            resumed_goal.timing_turn_id.as_deref(),
            Some(current_turn.turn_id.as_str())
        );
    }

    #[test]
    fn continue_user_message_opens_new_running_turn_after_interrupted_turn() {
        let state = test_state();
        let session_id = SessionId::new("session-continue-new-turn");
        let mission_id = MissionId::new("mission-continue-new-turn");
        let root_task_id = TaskId::new("task-root-continue-new-turn");
        let action_task_id = TaskId::new("task-action-continue-new-turn");
        let now = UtcMillis::now();
        state
            .session_store
            .create_session(session_id.clone(), "继续测试")
            .expect("session should create");
        seed_conversation_turn(
            &state,
            &session_id,
            "turn-old-interrupted",
            1,
            now,
            "interrupted",
            "旧任务",
        );

        let accepted = SessionContinueAccepted {
            session_id: session_id.clone(),
            mission_id,
            root_task_id,
            action_task_id: action_task_id.clone(),
            turn_id: "turn-session-continue-2".to_string(),
            execution_chain_ref: "chain-continue-new-turn".to_string(),
            resumed_branch_count: 1,
            runner_started: true,
        };
        let (_, user_item_id) = write_continue_user_message(ContinueUserMessageInput {
            state: &state,
            accepted: &accepted,
            prompt_text: Some("继续推进"),
            continued_at: UtcMillis(now.0 + 1),
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            request_fingerprint: None,
            attempt_id: None,
            orchestrator_thread_id: magi_core::ThreadId::new(
                "thread-orchestrator-continue-new-turn",
            ),
        })
        .expect("continue message should write");

        let turn = state
            .session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("current turn should exist");
        assert_eq!(turn.status, "running");
        assert_ne!(turn.turn_id, "turn-old-interrupted");
        assert_eq!(turn.user_message.as_deref(), Some("继续推进"));
        assert_eq!(turn.items.len(), 1);
        assert_eq!(turn.items[0].task_id.as_ref(), Some(&action_task_id));
        assert_eq!(
            user_item_id.as_deref(),
            Some(turn.items[0].item_id.as_str())
        );
    }

    #[tokio::test]
    async fn steer_route_targets_the_matching_active_session_turn() {
        let state = test_state();
        let workspace_id = register_workspace(&state, "workspace-session-steer", "session-steer");
        let session_id = SessionId::new("session-session-steer");
        let accepted_at = UtcMillis(1_777_100_000_000);
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "Session steer",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let (_, orchestrator_thread_id) =
            state
                .session_store
                .ensure_session_mission(&session_id, accepted_at, || {
                    MissionId::new("mission-session-steer")
                });
        seed_conversation_turn(
            &state,
            &session_id,
            "turn-session-steer",
            accepted_at.0,
            accepted_at,
            "running",
            "请生成详细方案",
        );
        let _active_input = state
            .turn_coordinator()
            .begin_session_turn_input(session_id.clone(), "turn-session-steer".to_string());

        let (status, body) = post_json(
            state.clone(),
            "/session/turn",
            serde_json::json!({
                "scope": "workspace",
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
                "text": "优先收口，不要扩展",
                "requestId": "request-session-steer",
                "userMessageId": "user-session-steer",
                "steerCurrentTurn": true,
                "expectedTurnId": "turn-session-steer"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["route"], "steer");
        assert_eq!(body["steeredTurnId"], "turn-session-steer");
        assert_eq!(body["userMessageItemId"], "user-session-steer");
        let drained = state
            .turn_coordinator()
            .drain_session_turn_steers(&session_id, "turn-session-steer");
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].text.as_deref(), Some("优先收口，不要扩展"));
        let turn = state
            .session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("active turn should remain");
        assert!(turn.items.iter().any(|item| {
            item.item_id == "user-session-steer" && item.source_thread_id == orchestrator_thread_id
        }));

        let (stale_status, stale_body) = post_json(
            state.clone(),
            "/session/turn",
            serde_json::json!({
                "scope": "workspace",
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
                "text": "迟到引导",
                "requestId": "request-session-steer-stale",
                "userMessageId": "user-session-steer-stale",
                "steerCurrentTurn": true,
                "expectedTurnId": "turn-session-steer-stale"
            }),
        )
        .await;
        assert_eq!(stale_status, StatusCode::CONFLICT);
        assert_eq!(stale_body["error_code"], "TURN_CONFLICT");
        assert_eq!(stale_body["conflict_kind"], "expected_turn_mismatch");
        assert_eq!(stale_body["active_turn_id"], "turn-session-steer");
        let turn = state
            .session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("active turn should remain after stale steer");
        assert!(
            !turn
                .items
                .iter()
                .any(|item| item.item_id == "user-session-steer-stale")
        );
    }

    #[test]
    fn web_turn_denies_every_browser_tool_without_affecting_other_tools() {
        let mut request = session_turn_request("检查网页");
        request.desktop_browser_tools_allowed = false;
        let denied = session_turn_denied_tools(&request);
        for tool in BrowserToolKind::ALL {
            assert!(
                denied.iter().any(|name| name == tool.name()),
                "Web Turn 必须隐藏浏览器工具 {}",
                tool.name()
            );
        }
        assert!(!denied.iter().any(|name| name == "shell"));
    }

    #[test]
    fn structured_goal_mode_request_does_not_depend_on_prompt_keywords() {
        let request = serde_json::from_value::<SessionTurnRequestDto>(serde_json::json!({
            "scope": "personal",
            "text": "完成当前产品稳定性验收",
            "images": [],
            "goalMode": true
        }))
        .expect("structured goal mode request should parse");
        let decision = decide_session_turn(&test_state(), &request).expect("goal decision");

        assert!(matches!(decision.route, SessionTurnRouteDto::Chat));
        assert_eq!(decision.required_tool_chain, ["update_plan"]);
        assert!(goal_mode_tool_intent().contains("create_goal"));

        let mut text_only = request.clone();
        text_only.goal_mode = false;
        text_only.text = Some("请用目标模式完成当前产品稳定性验收".to_string());
        let decision = decide_session_turn(&test_state(), &text_only).expect("plain decision");
        assert!(
            decision.required_tool_chain.is_empty(),
            "目标模式只由结构化开关开启，不从文本推断"
        );
    }

    #[test]
    fn plain_messages_route_to_tool_mainline_without_keyword_contracts() {
        let state = test_state();
        for text in [
            "读一下 AGENTS.md",
            "帮我配置 HTTP 代理",
            "这段代码为什么报错",
            "解释一下 Rust 所有权",
            "用多个子代理分析当前项目",
            "不要使用子代理，直接修复",
            "调用 shell_exec 运行测试",
            "生成一张项目架构图",
        ] {
            let request = session_turn_request(text);
            let decision = decide_session_turn(&state, &request).expect("plain decision");
            assert!(
                matches!(decision.route, SessionTurnRouteDto::Execute),
                "{text} 应进入带工具的主线"
            );
            assert!(
                decision.required_tool_chain.is_empty(),
                "{text} 不得从文本推断必调工具"
            );
            assert!(
                decision
                    .completion_contract
                    .evidence_requirements
                    .is_empty()
            );
            let denied = session_turn_denied_tools(&request);
            assert!(
                !denied
                    .iter()
                    .any(|tool| tool.starts_with("agent_") || tool == "update_plan"),
                "{text} 的协作与计划工具由模型自行决定是否使用：{denied:?}"
            );
            assert!(denied.iter().any(|tool| tool == "create_goal"));
        }
    }

    #[test]
    fn resume_without_recoverable_execution_is_rejected() {
        let state = test_state();
        let session_id = SessionId::new("session-resume-nothing");
        state
            .session_store
            .create_session(session_id.clone(), "没有可继续的执行")
            .expect("session should create");
        let mut request = session_turn_request("继续");
        request.session_id = Some(session_id.to_string());
        request.resume = true;
        assert!(matches!(
            decide_session_turn(&state, &request),
            Err(ApiError::InvalidInput(message)) if message.contains("没有可继续的执行")
        ));
    }

    #[test]
    fn resolves_workspace_binding_from_workspace_path_when_id_missing() {
        let state = test_state();
        let workspace_id = WorkspaceId::new("workspace-path-binding");
        let root = unique_temp_dir("magi-workspace-path-binding");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                magi_core::AbsolutePath::new(root.display().to_string()),
            )
            .expect("workspace should register");

        let resolved =
            state.resolve_workspace_id_from_request(None, Some(&root.display().to_string()));

        assert_eq!(resolved, Some(workspace_id));
    }

    #[tokio::test]
    async fn delete_session_rejects_workspace_mismatched_session() {
        let state = test_state();
        register_workspace(&state, "workspace-a", "delete-mismatch-a");
        register_workspace(&state, "workspace-b", "delete-mismatch-b");
        let session_id = SessionId::new("session-delete-workspace-a");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "工作区 A 删除保护",
                Some("workspace-a".to_string()),
            )
            .expect("session should create");

        let (status, body) = post_json(
            state.clone(),
            "/session/delete",
            serde_json::json!({
                "workspaceId": "workspace-b",
                "sessionId": session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("不属于 workspace workspace-b"),
            "unexpected body: {body}"
        );
        assert!(
            state.session_store.session(&session_id).is_some(),
            "workspace 不匹配时不应删除原会话"
        );
    }

    #[tokio::test]
    async fn session_management_actions_require_workspace_scope() {
        let actions = [
            (
                "/session/delete",
                "session-delete-missing-workspace",
                serde_json::json!({
                    "sessionId": "session-delete-missing-workspace",
                }),
            ),
            (
                "/session/rename",
                "session-rename-missing-workspace",
                serde_json::json!({
                    "sessionId": "session-rename-missing-workspace",
                    "name": "不应改名",
                }),
            ),
            (
                "/session/close",
                "session-close-missing-workspace",
                serde_json::json!({
                    "sessionId": "session-close-missing-workspace",
                }),
            ),
        ];

        for (path, session_id, body) in actions {
            let state = test_state();
            let session_id = SessionId::new(session_id);
            state
                .session_store
                .create_session_for_workspace(
                    session_id.clone(),
                    "缺 workspace 操作保护",
                    Some("workspace-management-required".to_string()),
                )
                .expect("session should create");

            let (status, payload) = post_json(state.clone(), path, body).await;

            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "unexpected body: {payload}"
            );
            assert!(
                payload["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("workspaceId 不能为空"),
                "unexpected body: {payload}"
            );
            assert!(
                state.session_store.session(&session_id).is_some(),
                "缺 workspace 时不应修改会话"
            );
        }
    }

    #[tokio::test]
    async fn concurrent_regular_session_turns_accept_one_and_queue_one() {
        let state = test_state_with_pending_runner();
        let workspace_id = register_workspace(
            &state,
            "workspace-concurrent-turn-acceptance",
            "concurrent-turn-acceptance",
        );
        let session_id = SessionId::new("session-concurrent-turn-acceptance");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "并发接受会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let first = post_json(
            state.clone(),
            "/session/turn",
            json!({
                "scope": "workspace",
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
                "text": "并发消息 A",
                "requestId": "request-concurrent-a",
                "userMessageId": "user-concurrent-a",
                "placeholderMessageId": "assistant-concurrent-a"
            }),
        );
        let second = post_json(
            state.clone(),
            "/session/turn",
            json!({
                "scope": "workspace",
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
                "text": "并发消息 B",
                "requestId": "request-concurrent-b",
                "userMessageId": "user-concurrent-b",
                "placeholderMessageId": "assistant-concurrent-b"
            }),
        );

        let ((first_status, first_body), (second_status, second_body)) =
            tokio::join!(first, second);

        assert_eq!(
            first_status,
            StatusCode::OK,
            "unexpected body: {first_body}"
        );
        assert_eq!(
            second_status,
            StatusCode::OK,
            "unexpected body: {second_body}"
        );
        assert_eq!(
            [&first_body, &second_body]
                .into_iter()
                .filter(|body| body["queued"] == true)
                .count(),
            1,
            "unexpected responses: first={first_body}, second={second_body}"
        );
        assert_eq!(state.queued_regular_session_turn_count(&session_id), 1);
    }

    #[tokio::test]
    async fn accepted_session_turn_normalizes_identity_before_preparing() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-accepted-turn-identity",
            "accepted-turn-identity",
        );

        let (status, body) = post_json(
            state.clone(),
            "/session/turn",
            json!({
                "scope": "workspace",
                "workspaceId": workspace_id.as_str(),
                "text": "accepted identity",
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["queued"], false);
        let turn_id = body["canonicalTurn"]["turnId"]
            .as_str()
            .expect("accepted response should carry canonical turn id");
        let request_id = body["canonicalItem"]["metadata"]["requestId"]
            .as_str()
            .expect("accepted canonical user item should carry request id");
        let user_message_id = body["canonicalItem"]["metadata"]["userMessageId"]
            .as_str()
            .expect("accepted canonical user item should carry user message id");
        assert!(!request_id.is_empty());
        assert!(!user_message_id.is_empty());
        assert_eq!(body["canonicalItem"]["itemId"], user_message_id);
        assert_eq!(body["canonicalItem"]["turnId"], turn_id);
        assert_eq!(body["canonicalTurn"]["acceptedAt"], body["acceptedAt"]);

        let event_id = body["eventId"]
            .as_str()
            .expect("accepted response should carry event id");
        let accepted_event = state
            .event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_id.to_string() == event_id)
            .expect("accepted event should be published");
        assert_eq!(accepted_event.payload["request_id"], request_id);
        assert_eq!(accepted_event.payload["user_message_id"], user_message_id);
        assert_eq!(accepted_event.payload["canonical_turn"]["turnId"], turn_id);
        assert_eq!(
            accepted_event.payload["canonical_item"]["metadata"]["requestId"],
            request_id
        );
        assert_eq!(
            accepted_event.payload["canonical_item"]["metadata"]["userMessageId"],
            user_message_id
        );

        let session_id = SessionId::new(
            body["sessionId"]
                .as_str()
                .expect("accepted response should carry session id"),
        );
        let sidecar = state
            .session_store
            .runtime_sidecar(&session_id)
            .expect("accepted turn should have a runtime sidecar");
        let current_turn = sidecar
            .current_turn
            .expect("accepted turn should be present in the sidecar");
        assert_eq!(current_turn.turn_id, turn_id);
        let user_item = current_turn
            .items
            .iter()
            .find(|item| item.item_id == user_message_id)
            .expect("sidecar should contain the canonical user item");
        assert_eq!(user_item.request_id.as_deref(), Some(request_id));
        assert_eq!(user_item.user_message_id.as_deref(), Some(user_message_id));
    }

    #[tokio::test]
    async fn regular_session_turn_adopts_same_branch_git_fast_forward() {
        let state = test_state_with_pending_runner();
        let (workspace_id, workspace_root) = register_git_workspace(
            &state,
            "workspace-turn-git-fast-forward",
            "turn-git-fast-forward",
        );
        let session_id = SessionId::new("session-turn-git-fast-forward");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "Git 快进后的下一轮",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        state
            .ensure_snapshot_session(&session_id, &workspace_root)
            .await
            .expect("snapshot baseline");
        state
            .ensure_session_code_context(&session_id, &Some(workspace_id.clone()))
            .await
            .expect("initial Git context");
        state.release_session_git_execution_lease(&session_id);

        fs::write(
            workspace_root.join("external.txt"),
            "external fast-forward\n",
        )
        .expect("external fixture");
        git(&workspace_root, &["add", "external.txt"]);
        git(&workspace_root, &["commit", "-m", "external fast-forward"]);
        let advanced_head = state
            .git_service
            .observe(&workspace_root)
            .await
            .expect("advanced Git observation")
            .head
            .expect("advanced HEAD");

        let (status, body) = post_json(
            state.clone(),
            "/session/turn",
            serde_json::json!({
                "scope": "workspace",
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
                "text": "继续检查当前项目",
                "requestId": "request-turn-git-fast-forward",
                "userMessageId": "user-turn-git-fast-forward",
                "placeholderMessageId": "assistant-turn-git-fast-forward"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["canonicalEventKind"], "turn_started");
        let context = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(context) = state.session_code_contexts.get(session_id.as_str())
                    && context.git.base_head.as_deref() == Some(advanced_head.as_str())
                {
                    break context;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("accepted turn preparation should adopt Git context");
        assert_eq!(
            context.git.base_head.as_deref(),
            Some(advanced_head.as_str())
        );
        assert!(!context.has_external_drift());
        assert!(
            state
                .snapshot_session(&session_id, &workspace_root)
                .expect("rebased snapshot")
                .pending_changes()
                .expect("pending changes")
                .is_empty()
        );

        if let Some(root_task_id) = body["rootTaskId"].as_str() {
            state
                .runner_manager()
                .expect("runner manager")
                .quiesce_for_restart(root_task_id)
                .await;
        }
        state.release_session_git_execution_lease(&session_id);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn session_interrupt_starts_next_queued_turn_and_preserves_fifo_tail() {
        let state = test_state_with_pending_runner();
        let workspace_id = register_workspace(
            &state,
            "workspace-interrupt-queued-turns",
            "interrupt-queued-turns",
        );
        let session_id = SessionId::new("session-interrupt-queued-turns");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "中断排队会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let (first_status, first_body) = post_json(
            state.clone(),
            "/session/turn",
            json!({
                "scope": "workspace",
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
                "text": "正在处理的消息",
                "requestId": "request-before-interrupt",
                "userMessageId": "user-before-interrupt",
                "placeholderMessageId": "assistant-before-interrupt",
            }),
        )
        .await;
        assert_eq!(
            first_status,
            StatusCode::OK,
            "unexpected body: {first_body}"
        );
        assert_eq!(first_body["queued"], false);
        assert!(
            first_body["rootTaskId"].is_string(),
            "普通文本当前进入 task 主线，应创建 root task"
        );
        assert!(
            state
                .session_store
                .active_execution_chain(&session_id)
                .is_some(),
            "task 主线应持久化 execution chain"
        );

        for (request_id, user_message_id, text) in [
            (
                "request-queue-interrupt-a",
                "user-queue-interrupt-a",
                "停止后第一条发送",
            ),
            (
                "request-queue-interrupt-b",
                "user-queue-interrupt-b",
                "停止后第二条继续排队",
            ),
        ] {
            let (queued_status, queued_body) = post_json(
                state.clone(),
                "/session/turn",
                json!({
                    "scope": "workspace",
                    "workspaceId": workspace_id.as_str(),
                    "sessionId": session_id.as_str(),
                    "text": text,
                    "requestId": request_id,
                    "userMessageId": user_message_id,
                    "placeholderMessageId": format!("assistant-{request_id}"),
                }),
            )
            .await;
            assert_eq!(
                queued_status,
                StatusCode::OK,
                "unexpected body: {queued_body}"
            );
            assert_eq!(queued_body["queued"], true);
        }

        let (status, body) = post_json(
            state.clone(),
            "/session/interrupt",
            json!({
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["interrupted"], true);
        assert_eq!(body["nextQueuedTurnStarted"], true);
        assert_eq!(body["remainingQueuedTurnCount"], 1);
        assert_eq!(state.queued_regular_session_turn_count(&session_id), 1);
        assert_eq!(
            state
                .peek_next_regular_session_turn(&session_id)
                .expect("FIFO tail should remain queued")
                .request
                .request_id()
                .as_deref(),
            Some("request-queue-interrupt-b")
        );
        assert!(
            state
                .session_store
                .canonical_turns_for_session(&session_id)
                .into_iter()
                .any(|turn| turn
                    .items
                    .iter()
                    .any(|item| { item.item_id == "user-queue-interrupt-a" })),
            "停止后必须按 FIFO 启动队首消息"
        );
        let active_chain = state
            .session_store
            .active_execution_chain(&session_id)
            .expect("队首消息现在通过 task 主线运行，应建立活动 execution chain");
        assert_eq!(
            active_chain.dispatch_context.trimmed_text.as_deref(),
            Some("停止后第一条发送"),
            "中断后应启动 FIFO 队首消息，而不是尾部或旧消息"
        );
    }

    #[tokio::test]
    async fn session_turn_queue_routes_expose_and_remove_persisted_turns() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-session-turn-queue-routes",
            "session-turn-queue-routes",
        );
        let session_id = SessionId::new("session-turn-queue-routes");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "队列路由会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        state
            .enqueue_regular_session_turn(queued_regular_turn(
                &session_id,
                &workspace_id,
                "queue-route-a",
                UtcMillis(151),
            ))
            .expect("queued turn should persist");

        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/session/queue?workspaceId={}&sessionId={}",
                        workspace_id, session_id
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        assert_eq!(response.status(), StatusCode::OK);
        let payload: serde_json::Value = serde_json::from_slice(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body should read"),
        )
        .expect("response should be json");
        assert_eq!(payload["queuedTurns"][0]["queueId"], "queue-route-a");
        assert_eq!(payload["queuedTurns"][0]["queuePosition"], 1);
        assert_eq!(payload["queuedTurns"][0]["text"], "queued queue-route-a");
        // 会话没有运行中的轮次，排队消息没有可引导的对象。
        assert_eq!(payload["queuedTurns"][0]["canGuide"], false);

        let (status, payload) = post_json(
            state.clone(),
            "/session/queue/remove",
            json!({
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
                "queueId": "queue-route-a",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {payload}");
        assert_eq!(payload["queuedTurns"], json!([]));
        assert_eq!(state.queued_regular_session_turn_count(&session_id), 0);
    }

    #[tokio::test]
    async fn queued_plain_text_can_be_guided_once_and_removed_atomically() {
        let state = test_state();
        let workspace_id =
            register_workspace(&state, "workspace-queued-turn-guide", "queued-turn-guide");
        let session_id = SessionId::new("session-queued-turn-guide");
        let accepted_at = UtcMillis(1_777_200_000_000);
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "排队消息转引导",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        state
            .session_store
            .ensure_session_mission(&session_id, accepted_at, || {
                MissionId::new("mission-queued-turn-guide")
            });
        seed_conversation_turn(
            &state,
            &session_id,
            "turn-queued-turn-guide",
            accepted_at.0,
            accepted_at,
            "running",
            "继续执行当前工作",
        );
        state
            .turn_coordinator()
            .begin_session_turn_input(session_id.clone(), "turn-queued-turn-guide".to_string())
            .expect("active turn input should begin");
        let queued = queued_regular_turn(
            &session_id,
            &workspace_id,
            "queue-guide-once",
            UtcMillis(accepted_at.0 + 1),
        );
        state
            .enqueue_regular_session_turn(queued.clone())
            .expect("queued turn should persist");

        let guide_request = json!({
            "workspaceId": workspace_id.as_str(),
            "sessionId": session_id.as_str(),
            "queueId": "queue-guide-once",
        });
        let (status, body) =
            post_json(state.clone(), "/session/queue/guide", guide_request.clone()).await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["queuedTurns"], json!([]));
        let steers = state
            .turn_coordinator()
            .drain_session_turn_steers(&session_id, "turn-queued-turn-guide");
        assert_eq!(steers.len(), 1);
        assert_eq!(steers[0].text.as_deref(), Some("queued queue-guide-once"));
        assert_eq!(
            steers[0].request_id.as_deref(),
            Some("request-queue-guide-once")
        );
        assert_eq!(
            steers[0].user_message_id.as_deref(),
            Some("user-queue-guide-once")
        );

        // 模拟 canonical 引导已提交、队列持久化删除尚未完成后的恢复重试。
        state
            .enqueue_regular_session_turn(queued)
            .expect("recovery queue should persist");
        let (retry_status, retry_body) =
            post_json(state.clone(), "/session/queue/guide", guide_request).await;
        assert_eq!(
            retry_status,
            StatusCode::OK,
            "unexpected body: {retry_body}"
        );
        assert_eq!(retry_body["queuedTurns"], json!([]));
        assert!(
            state
                .turn_coordinator()
                .drain_session_turn_steers(&session_id, "turn-queued-turn-guide")
                .is_empty(),
            "恢复重试不得重复写入引导信号"
        );
    }

    #[tokio::test]
    async fn queued_guide_rejects_structured_message_and_missing_active_turn_without_removal() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-queued-turn-guide-reject",
            "queued-turn-guide-reject",
        );
        let session_id = SessionId::new("session-queued-turn-guide-reject");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "拒绝无效排队引导",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let mut structured = queued_regular_turn(
            &session_id,
            &workspace_id,
            "queue-guide-structured",
            UtcMillis(1_777_200_000_100),
        );
        structured.request.goal_mode = true;
        state
            .enqueue_regular_session_turn(structured)
            .expect("structured queue should persist");

        let (structured_status, structured_body) = post_json(
            state.clone(),
            "/session/queue/guide",
            json!({
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
                "queueId": "queue-guide-structured",
            }),
        )
        .await;
        assert_eq!(
            structured_status,
            StatusCode::BAD_REQUEST,
            "unexpected body: {structured_body}"
        );
        assert_eq!(state.queued_regular_session_turn_count(&session_id), 1);

        state
            .remove_regular_session_turn(&session_id, "queue-guide-structured")
            .expect("structured queue removal should persist");
        state
            .enqueue_regular_session_turn(queued_regular_turn(
                &session_id,
                &workspace_id,
                "queue-guide-no-active-turn",
                UtcMillis(1_777_200_000_101),
            ))
            .expect("plain queue should persist");
        let (inactive_status, inactive_body) = post_json(
            state.clone(),
            "/session/queue/guide",
            json!({
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
                "queueId": "queue-guide-no-active-turn",
            }),
        )
        .await;
        assert_eq!(
            inactive_status,
            StatusCode::CONFLICT,
            "unexpected body: {inactive_body}"
        );
        assert_eq!(inactive_body["conflict_kind"], "no_active_turn");
        assert_eq!(state.queued_regular_session_turn_count(&session_id), 1);
    }

    #[tokio::test]
    async fn non_retryable_queued_turn_failure_is_dropped_so_later_messages_continue() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-queued-turn-failure",
            "queued-turn-failure",
        );
        let session_id = SessionId::new("session-queued-turn-failure");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "排队失败保留会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let missing_workspace_id = WorkspaceId::new("workspace-queued-turn-missing");
        let mut queued = queued_regular_turn(
            &session_id,
            &workspace_id,
            "queue-submit-failure",
            UtcMillis(201),
        );
        queued.requested_workspace_id = Some(missing_workspace_id.clone());
        queued.request.workspace_id = Some(missing_workspace_id.to_string());
        queued.retry_count = 3;
        state
            .enqueue_regular_session_turn(queued)
            .expect("queued turn should persist");
        state
            .enqueue_regular_session_turn(queued_regular_turn(
                &session_id,
                &workspace_id,
                "queue-after-failure",
                UtcMillis(202),
            ))
            .expect("second queued turn should persist");

        assert_eq!(
            drain_next_queued_regular_session_turn(
                state.clone(),
                session_id.clone(),
                Some(workspace_id),
            )
            .await,
            QueuedRegularSessionTurnDrainOutcome::DroppedAfterFailure,
        );

        let next = state
            .peek_next_regular_session_turn(&session_id)
            .expect("later queued message must remain");
        assert_eq!(
            next.queue_id, "queue-after-failure",
            "不可重试的失败消息必须移出队首，不能阻塞后续消息"
        );
        let snapshot = state.event_bus.snapshot();
        let failed_event = snapshot
            .recent_events
            .iter()
            .find(|event| event.event_type == "session.turn.queue_failed")
            .expect("queue failure event should publish");
        let failure_detail = failed_event.payload["failure_detail"]
            .as_str()
            .expect("queue failure should include direct error");
        assert_eq!(
            failed_event.payload["request_id"],
            "request-queue-submit-failure"
        );
        assert_eq!(
            failed_event.payload["user_message_id"],
            "user-queue-submit-failure"
        );
        assert!(failed_event.payload["turn_id"].is_null());
        assert_eq!(failed_event.payload["accepted_at"], 201);
        assert_eq!(failed_event.payload["retry_count"], 4);
        assert!(failure_detail.contains(missing_workspace_id.as_str()));
        assert!(!failure_detail.contains("服务恢复后"));
    }

    #[tokio::test]
    async fn recovered_queue_acknowledges_only_matching_canonical_user_identity() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-queue-canonical-recovery",
            "queue-canonical-recovery",
        );
        let session_id = SessionId::new("session-queue-canonical-recovery");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "队列 canonical 恢复会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let accepted_at = UtcMillis(301);
        let matching = queued_regular_turn(
            &session_id,
            &workspace_id,
            "queue-canonical-match",
            accepted_at,
        );
        let (_, user_item) = build_user_message_turn_item(UserMessageTurnItemInput {
            accepted_at,
            message: "已接受的排队消息",
            entry_id: "entry-canonical-match",
            request_id: matching.request.request_id(),
            user_message_id: matching.request.user_message_id(),
            placeholder_message_id: matching.request.placeholder_message_id(),
            metadata: Default::default(),
            task_id: None,
            source_thread_id: ThreadId::new("thread-canonical-match"),
        });
        let mut user_item = user_item;
        user_item.item_seq = 2;
        let coordinator = state.turn_coordinator();
        crate::routes::test_turn_fixtures::seed_conversation_turn(
            &state.session_store,
            coordinator,
            &session_id,
            "turn-canonical-match",
            1,
            accepted_at,
            "running",
            "已接受的排队消息",
        );
        state
            .turn_event_sink()
            .upsert_item_sidecar(&session_id, Some("turn-canonical-match"), user_item)
            .expect("canonical user item should persist");
        crate::routes::test_turn_fixtures::finish_conversation_turn(
            &state.session_store,
            coordinator,
            &session_id,
            "turn-canonical-match",
            "completed",
        );
        let canonical = state
            .session_store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .next()
            .expect("canonical turn should exist");
        let same_timestamp_different_identity = queued_regular_turn(
            &session_id,
            &workspace_id,
            "queue-canonical-different",
            accepted_at,
        );
        assert!(!canonical_turn_matches_queued_regular_turn(
            &canonical,
            &same_timestamp_different_identity
        ));
        assert!(canonical_turn_matches_queued_regular_turn(
            &canonical, &matching
        ));
        state
            .enqueue_regular_session_turn(matching)
            .expect("matching recovery turn should persist");

        assert_eq!(
            drain_next_queued_regular_session_turn(
                state.clone(),
                session_id.clone(),
                Some(workspace_id),
            )
            .await,
            QueuedRegularSessionTurnDrainOutcome::Started,
        );
        assert_eq!(state.queued_regular_session_turn_count(&session_id), 0);
        assert_eq!(
            state
                .session_store
                .canonical_turns_for_session(&session_id)
                .len(),
            1,
            "恢复确认不能重复接受 canonical turn"
        );
    }

    #[tokio::test]
    async fn regular_session_turn_busy_session_is_queued_and_drained_as_independent_turn() {
        let state = test_state();
        let workspace_id =
            register_workspace(&state, "workspace-regular-turn-queue", "regular-turn-queue");
        let session_id = SessionId::new("session-regular-turn-queue");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "排队会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        seed_conversation_turn(
            &state,
            &session_id,
            "turn-active-before-queue",
            1,
            UtcMillis(1_777_000_000_200),
            "running",
            "第一条还在运行",
        );

        let (status, body) = post_json(
            state.clone(),
            "/session/turn",
            serde_json::json!({
                "scope": "workspace",
                "workspaceId": workspace_id.to_string(),
                "sessionId": session_id.to_string(),
                "text": "第二条应该排队",
                "requestId": "request-queued-turn",
                "userMessageId": "user-queued-turn",
                "placeholderMessageId": "assistant-queued-turn"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["queued"], true);
        assert_eq!(body["queuePosition"], 1);
        assert_eq!(body["sessionId"], session_id.as_str());
        assert_eq!(body["userMessageItemId"], "user-queued-turn");
        assert!(
            body.get("canonicalEventKind").is_none(),
            "排队响应不应伪造 turn_started: {body}"
        );
        let queued_at = UtcMillis(
            body["acceptedAt"]
                .as_u64()
                .expect("acceptedAt should serialize as integer"),
        );
        let queued_event = state
            .event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_type == "session.turn.queued")
            .expect("queued event should be published");
        assert_eq!(queued_event.session_id.as_ref(), Some(&session_id));
        assert_eq!(queued_event.workspace_id.as_ref(), Some(&workspace_id));
        assert_eq!(queued_event.payload["queue_position"], 1);

        let queued = state
            .peek_next_regular_session_turn(&session_id)
            .expect("queued turn should be stored by session key");
        assert_eq!(
            queued.queue_id,
            body["queueId"].as_str().unwrap_or_default()
        );
        assert_eq!(
            queued.request.trimmed_text().as_deref(),
            Some("第二条应该排队")
        );
        crate::routes::test_turn_fixtures::finish_conversation_turn(
            &state.session_store,
            state.turn_coordinator(),
            &session_id,
            "turn-active-before-queue",
            "completed",
        );
        assert_eq!(
            drain_next_queued_regular_session_turn(
                state.clone(),
                session_id.clone(),
                Some(workspace_id.clone()),
            )
            .await,
            QueuedRegularSessionTurnDrainOutcome::Started,
            "terminal current turn should drain one queued turn",
        );

        let drained_turn = state
            .session_store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| {
                turn.items
                    .iter()
                    .any(|item| item.item_id == "user-queued-turn")
            })
            .expect("queued message should become an independent canonical turn");
        assert_eq!(drained_turn.accepted_at, queued_at);
        assert_eq!(
            drained_turn.items[0].kind,
            CanonicalTurnItemKind::UserMessage
        );
        assert_eq!(state.queued_regular_session_turn_count(&session_id), 0);
    }

    #[tokio::test]
    async fn queued_session_turn_persists_identity_and_does_not_claim_a_canonical_turn() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-queued-turn-identity",
            "queued-turn-identity",
        );
        let session_id = SessionId::new("session-queued-turn-identity");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "排队身份会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        seed_conversation_turn(
            &state,
            &session_id,
            "turn-queued-identity-active",
            1,
            UtcMillis(1_777_000_000_400),
            "running",
            "当前消息仍在运行",
        );

        let (status, body) = post_json(
            state.clone(),
            "/session/turn",
            json!({
                "scope": "workspace",
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
                "text": "queued identity",
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["queued"], true);
        let accepted_at = body["acceptedAt"]
            .as_u64()
            .expect("queued response should carry acceptedAt");
        let queued = state
            .peek_next_regular_session_turn(&session_id)
            .expect("queued turn should be retained");
        let request_id = queued
            .request
            .request_id()
            .expect("queued turn should carry request id");
        let user_message_id = queued
            .request
            .user_message_id()
            .expect("queued turn should carry user message id");
        assert!(!request_id.is_empty());
        assert!(!user_message_id.is_empty());
        assert_eq!(body["userMessageItemId"], user_message_id);
        assert_eq!(queued.accepted_at.0, accepted_at);

        let queued_event = state
            .event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_type == "session.turn.queued")
            .expect("queued event should be published");
        assert_eq!(queued_event.payload["request_id"], request_id);
        assert_eq!(queued_event.payload["user_message_id"], user_message_id);
        assert!(queued_event.payload["turn_id"].is_null());
        assert_eq!(queued_event.payload["accepted_at"], accepted_at);
        assert_eq!(queued_event.payload["queue_id"], queued.queue_id);
        assert!(
            !state
                .session_store
                .canonical_turns_for_session(&session_id)
                .into_iter()
                .any(|turn| {
                    turn.items.iter().any(|item| {
                        item.item_id == user_message_id
                            || item
                                .metadata
                                .get("requestId")
                                .and_then(serde_json::Value::as_str)
                                == Some(request_id.as_str())
                    })
                }),
            "排队阶段不得提前创建该消息的 canonical Turn"
        );
    }

    #[tokio::test]
    async fn queued_goal_mode_turn_preserves_goal_required_tool_chain_when_drained() {
        let task_store = Arc::new(TaskStore::new());
        let state = test_state().with_task_store(task_store.clone());
        let workspace_id =
            register_workspace(&state, "workspace-queued-goal-mode", "queued-goal-mode");
        let session_id = SessionId::new("session-queued-goal-mode");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "排队目标模式",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        seed_conversation_turn(
            &state,
            &session_id,
            "turn-before-queued-goal",
            1,
            UtcMillis(1_777_000_000_300),
            "running",
            "前一轮仍在运行",
        );

        let (status, body) = post_json(
            state.clone(),
            "/session/turn",
            serde_json::json!({
                "scope": "workspace",
                "workspaceId": workspace_id.to_string(),
                "sessionId": session_id.to_string(),
                "text": "建立目标并按步骤完成排队稳定性验收",
                "goalMode": true,
                "requestId": "request-queued-goal-mode",
                "userMessageId": "user-queued-goal-mode",
                "placeholderMessageId": "assistant-queued-goal-mode"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["queued"], true);
        assert!(
            state
                .peek_next_regular_session_turn(&session_id)
                .is_some_and(|queued| queued.request.goal_mode),
            "Goal 模式判定必须随排队消息持久化"
        );

        crate::routes::test_turn_fixtures::finish_conversation_turn(
            &state.session_store,
            state.turn_coordinator(),
            &session_id,
            "turn-before-queued-goal",
            "completed",
        );
        assert_eq!(
            drain_next_queued_regular_session_turn(state, session_id, Some(workspace_id),).await,
            QueuedRegularSessionTurnDrainOutcome::Started,
            "terminal current turn should drain queued goal turn",
        );

        let goal_task = task_store
            .all_tasks()
            .into_iter()
            .find(|task| !task.required_tool_chain().is_empty())
            .expect("drained goal turn should create a required tool chain");
        assert_eq!(
            goal_task.required_tool_chain(),
            ["get_goal", "create_goal", "update_plan"]
        );
    }

    #[tokio::test]
    async fn active_goal_keeps_queued_messages_waiting_until_the_goal_stops() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-goal-queue-priority",
            "goal-queue-priority",
        );
        let session_id = SessionId::new("session-goal-queue-priority");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "目标与排队",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        assert!(
            !goal_continuation_takes_priority(&state, &session_id),
            "没有目标时排队消息照常出队"
        );
        let (_, thread_id) =
            state
                .session_store
                .ensure_session_mission(&session_id, UtcMillis::now(), || {
                    MissionId::new("mission-goal-queue-priority")
                });
        let goal = state
            .session_store
            .create_goal(
                session_id.clone(),
                thread_id,
                "turn-goal-queue-priority",
                "分析当前项目",
                magi_core::AccessProfile::Restricted,
                None,
            )
            .expect("goal should create");
        assert!(
            goal_continuation_takes_priority(&state, &session_id),
            "目标正在推进时，下一轮属于目标续跑，排队消息不能插队"
        );
        state
            .session_store
            .stop_goal_for_runtime_failure(
                &session_id,
                &goal.goal_id,
                None,
                "turn-goal-queue-priority",
                "目标停下来了",
            )
            .expect("goal should stop");
        assert!(
            !goal_continuation_takes_priority(&state, &session_id),
            "目标受阻 / 暂停后，排队消息按顺序继续发送"
        );
    }

    #[tokio::test]
    async fn first_goal_runtime_failure_keeps_the_goal_active_and_backs_off() {
        let state = test_state();
        let workspace_id = register_workspace(&state, "workspace-goal-retry", "goal-retry");
        let session_id = SessionId::new("session-goal-retry");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "目标失败重试",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let (_, thread_id) =
            state
                .session_store
                .ensure_session_mission(&session_id, UtcMillis::now(), || {
                    MissionId::new("mission-goal-retry")
                });
        let _goal = state
            .session_store
            .create_goal(
                session_id.clone(),
                thread_id,
                "turn-goal-retry-1",
                "分析当前项目",
                magi_core::AccessProfile::Restricted,
                None,
            )
            .expect("goal should create");
        assert_eq!(goal_runtime_retry_backoff(&state, &session_id), None);

        assert!(
            record_active_goal_turn_failure(&state, &session_id, "turn-goal-retry-1", "timeout"),
            "第一次失败应保持目标 active 并重试"
        );
        assert_eq!(
            goal_runtime_retry_backoff(&state, &session_id),
            Some(std::time::Duration::from_secs(5))
        );
        assert!(state.session_store.active_goal(&session_id).is_some());
    }

    #[tokio::test]
    async fn regular_session_turn_queue_is_scoped_by_session() {
        let state = test_state();
        let workspace_a = register_workspace(&state, "workspace-queue-scope-a", "queue-scope-a");
        let workspace_b = register_workspace(&state, "workspace-queue-scope-b", "queue-scope-b");
        let session_a = SessionId::new("session-queue-scope-a");
        let session_b = SessionId::new("session-queue-scope-b");
        for (session_id, workspace_id, title) in [
            (&session_a, &workspace_a, "队列 A"),
            (&session_b, &workspace_b, "队列 B"),
        ] {
            state
                .session_store
                .create_session_for_workspace(
                    session_id.clone(),
                    title,
                    Some(workspace_id.to_string()),
                )
                .expect("session should create");
            seed_conversation_turn(
                &state,
                session_id,
                &format!("turn-active-{session_id}"),
                1,
                UtcMillis(1_777_000_001_000),
                "running",
                "运行中",
            );
        }

        let (status_a, response_a) = post_json(
            state.clone(),
            "/session/turn",
            serde_json::json!({
                "scope": "workspace",
                "workspaceId": workspace_a.to_string(),
                "sessionId": session_a.to_string(),
                "text": "A 的下一条",
                "requestId": "request-queue-a",
                "userMessageId": "user-queue-a",
                "placeholderMessageId": "assistant-queue-a"
            }),
        )
        .await;
        assert_eq!(status_a, StatusCode::OK, "unexpected body: {response_a}");
        let (status_b, response_b) = post_json(
            state.clone(),
            "/session/turn",
            serde_json::json!({
                "scope": "workspace",
                "workspaceId": workspace_b.to_string(),
                "sessionId": session_b.to_string(),
                "text": "B 的下一条",
                "requestId": "request-queue-b",
                "userMessageId": "user-queue-b",
                "placeholderMessageId": "assistant-queue-b"
            }),
        )
        .await;
        assert_eq!(status_b, StatusCode::OK, "unexpected body: {response_b}");
        assert_eq!(response_a["queued"], true);
        assert_eq!(response_b["queued"], true);

        assert!(
            state.peek_next_regular_session_turn(&session_a).is_some(),
            "session A 的队列应独立存在"
        );
        assert!(
            state.peek_next_regular_session_turn(&session_b).is_some(),
            "session B 的队列不应被 session A drain 影响"
        );
    }

    #[tokio::test]
    async fn task_session_turn_busy_session_is_queued_before_dispatch_acceptance() {
        let state = test_state();
        let workspace_id =
            register_workspace(&state, "workspace-task-turn-queue", "task-turn-queue");
        let session_id = SessionId::new("session-task-turn-queue");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "任务排队会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        seed_conversation_turn(
            &state,
            &session_id,
            "turn-active-before-task-queue",
            1,
            UtcMillis(1_777_000_002_000),
            "running",
            "当前任务还在运行",
        );

        let (status, body) = post_json(
            state.clone(),
            "/session/turn",
            serde_json::json!({
                "scope": "workspace",
                "workspaceId": workspace_id.to_string(),
                "sessionId": session_id.to_string(),
                "text": "以任务模式整理当前问题并输出修复计划",
                "requestId": "request-task-queued-turn",
                "userMessageId": "user-task-queued-turn",
                "placeholderMessageId": "assistant-task-queued-turn"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["route"], "execute");
        assert_eq!(body["queued"], true);
        assert_eq!(body["queuePosition"], 1);
        assert_eq!(body["userMessageItemId"], "user-task-queued-turn");
        assert!(
            body.get("canonicalEventKind").is_none(),
            "任务排队响应不应提前发布 turn_started: {body}"
        );
        let queued = state
            .peek_next_regular_session_turn(&session_id)
            .expect("task turn should be queued with the same session key");
        assert!(matches!(queued.route, SessionTurnRouteDto::Execute));
        assert_eq!(
            queued.request.trimmed_text().as_deref(),
            Some("以任务模式整理当前问题并输出修复计划")
        );
    }

    #[tokio::test]
    async fn task_session_turn_accept_returns_canonical_user_image_item() {
        let task_store = Arc::new(TaskStore::new());
        let state = test_state().with_task_store(task_store);
        let workspace_id =
            register_workspace(&state, "workspace-task-image-turn", "task-image-turn");
        let (status, body) = post_json(
            state.clone(),
            "/session/turn",
            serde_json::json!({
                "scope": "workspace",
                "workspaceId": workspace_id.to_string(),
                "text": "以任务模式分析这张图片并整理成待办",
                "images": [{
                    "name": "paste.png",
                    "dataUrl": "data:image/png;base64,AAA"
                }],
                "requestId": "request-task-image-turn",
                "userMessageId": "user-task-image-turn",
                "placeholderMessageId": "assistant-task-image-turn"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["route"], "execute");
        assert_eq!(body["userMessageItemId"], "user-task-image-turn");
        assert_eq!(body["canonicalEventKind"], "turn_started");
        assert_eq!(
            body["canonicalItem"]["metadata"]["images"][0]["dataUrl"],
            "data:image/png;base64,AAA"
        );
        assert_eq!(
            body["canonicalItem"]["metadata"]["images"][0]["name"],
            "paste.png"
        );

        let event_id = body["eventId"]
            .as_str()
            .expect("task response should carry event id");
        let accepted_event = state
            .event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_id.to_string() == event_id)
            .expect("task accepted event should be published");
        assert_eq!(
            accepted_event.payload["workspace_id"],
            workspace_id.as_str()
        );
        assert_eq!(
            accepted_event.payload["canonical_event_kind"],
            "turn_started"
        );
        assert_eq!(
            accepted_event.payload["canonical_item"]["metadata"]["images"][0]["dataUrl"],
            "data:image/png;base64,AAA"
        );
    }

    #[tokio::test]
    async fn task_session_turn_persists_context_reference_and_read_only_policy() {
        let task_store = Arc::new(TaskStore::new());
        let state = test_state().with_task_store(task_store.clone());
        let workspace_id = register_workspace(
            &state,
            "workspace-task-context-reference",
            "task-context-reference",
        );
        let external_dir = unique_temp_dir("task-context-reference-external");
        let external_file = external_dir.join("reference.md");
        fs::write(&external_file, "REFERENCE_CONTENT").expect("reference file should write");
        let canonical_external_file = external_file
            .canonicalize()
            .expect("reference file should canonicalize");

        let (status, body) = post_json(
            state,
            "/session/turn",
            serde_json::json!({
                "scope": "workspace",
                "workspaceId": workspace_id.to_string(),
                "text": "以任务模式分析引用文件并输出结论",
                "contextReferences": [{
                    "kind": "file",
                    "path": external_file,
                    "name": "reference.md"
                }],
                "requestId": "request-task-context-reference",
                "userMessageId": "user-task-context-reference",
                "placeholderMessageId": "assistant-task-context-reference"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["route"], "execute");
        assert_eq!(
            body["canonicalItem"]["metadata"]["contextReferences"][0]["path"],
            canonical_external_file.display().to_string()
        );
        let action_task_id = TaskId::new(
            body["actionTaskId"]
                .as_str()
                .expect("task response should carry actionTaskId"),
        );
        let task = task_store
            .get_task(&action_task_id)
            .expect("action task should exist");
        let policy = task.policy_snapshot.expect("task policy should exist");
        assert!(
            policy
                .read_only_paths
                .contains(&canonical_external_file.display().to_string())
        );
        assert!(
            task.input_refs
                .iter()
                .any(|value| value.contains(&canonical_external_file.display().to_string()))
        );
    }

    #[tokio::test]
    async fn new_goal_mode_session_requires_goal_creation_before_plan() {
        let task_store = Arc::new(TaskStore::new());
        let state = test_state().with_task_store(task_store.clone());
        let workspace_id = register_workspace(
            &state,
            "workspace-new-goal-required-chain",
            "new-goal-required-chain",
        );

        let (status, body) = post_json(
            state,
            "/session/turn",
            serde_json::json!({
                "scope": "workspace",
                "workspaceId": workspace_id.to_string(),
                "text": "建立目标和两步任务清单，第一步调用 shell_exec 执行 sleep 30，第二步汇总结果",
                "goalMode": true,
                "requestId": "request-new-goal-required-chain",
                "userMessageId": "user-new-goal-required-chain",
                "placeholderMessageId": "assistant-new-goal-required-chain"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["route"], "chat");
        let action_task_id = TaskId::new(
            body["actionTaskId"]
                .as_str()
                .expect("goal response should carry actionTaskId"),
        );
        let task = task_store
            .get_task(&action_task_id)
            .expect("goal action task should exist");
        assert!(task.is_goal_mode());
        assert_eq!(
            task.required_tool_chain(),
            ["get_goal", "create_goal", "update_plan"]
        );
    }

    #[tokio::test]
    async fn existing_unfinished_goal_mode_session_must_not_require_duplicate_goal_creation() {
        let task_store = Arc::new(TaskStore::new());
        let state = test_state().with_task_store(task_store.clone());
        let workspace_id = register_workspace(
            &state,
            "workspace-existing-goal-required-chain",
            "existing-goal-required-chain",
        );
        let session_id = SessionId::new("session-existing-goal-required-chain");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "existing goal required chain",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let (_, thread_id) =
            state
                .session_store
                .ensure_session_mission(&session_id, UtcMillis::now(), || {
                    MissionId::new("mission-existing-goal-required-chain")
                });
        state
            .session_store
            .create_goal(
                session_id.clone(),
                thread_id,
                "turn-existing-goal-required-chain",
                "完成已有目标",
                magi_core::AccessProfile::Restricted,
                None,
            )
            .expect("unfinished goal should create");

        let (status, body) = post_json(
            state,
            "/session/turn",
            serde_json::json!({
                "scope": "workspace",
                "sessionId": session_id.to_string(),
                "workspaceId": workspace_id.to_string(),
                "text": "继续按步骤推进当前目标",
                "goalMode": true,
                "requestId": "request-existing-goal-required-chain",
                "userMessageId": "user-existing-goal-required-chain",
                "placeholderMessageId": "assistant-existing-goal-required-chain"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["route"], "chat");
        let action_task_id = TaskId::new(
            body["actionTaskId"]
                .as_str()
                .expect("goal response should carry actionTaskId"),
        );
        let task = task_store
            .get_task(&action_task_id)
            .expect("goal action task should exist");
        assert!(task.is_goal_mode());
        assert_eq!(task.required_tool_chain(), ["get_goal", "update_plan"]);
    }

    #[tokio::test]
    async fn session_turn_rejects_invalid_image_payload_before_accepting_turn() {
        let state = test_state();
        let workspace_id =
            register_workspace(&state, "workspace-invalid-image-turn", "invalid-image-turn");
        let (status, body) = post_json(
            state.clone(),
            "/session/turn",
            serde_json::json!({
                "scope": "workspace",
                "workspaceId": workspace_id.to_string(),
                "text": "请分析这张图片",
                "images": [{
                    "name": "paste.txt",
                    "dataUrl": "data:text/plain;base64,AAA"
                }],
                "requestId": "request-invalid-image-turn",
                "userMessageId": "user-invalid-image-turn",
                "placeholderMessageId": "assistant-invalid-image-turn"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("图片输入无效")),
            "invalid image error should be public and actionable: {body}"
        );
        assert!(
            state
                .session_store
                .sessions_for_workspace(workspace_id.as_str())
                .is_empty(),
            "invalid image request must not create a session or accepted turn"
        );
        assert!(
            state.event_bus.snapshot().recent_events.is_empty(),
            "invalid image request must not publish accepted turn events"
        );
    }

    #[tokio::test]
    async fn delete_session_returns_workspace_scoped_bootstrap() {
        let persisted_current = Arc::new(Mutex::new(None));
        let persisted_current_capture = Arc::clone(&persisted_current);
        let state = test_state().with_session_projection_persist(Arc::new(
            move |durable, _sidecars, _mode| {
                *persisted_current_capture
                    .lock()
                    .expect("persisted current capture should lock") =
                    durable.current_session_id.clone();
                Ok(())
            },
        ));
        register_workspace(&state, "workspace-a", "delete-scoped-a");
        register_workspace(&state, "workspace-b", "delete-scoped-b");
        let deleted_session_id = SessionId::new("session-delete-scoped-a1");
        let sibling_session_id = SessionId::new("session-delete-scoped-a2");
        let foreign_session_id = SessionId::new("session-delete-scoped-b1");
        state
            .session_store
            .create_session_for_workspace(
                deleted_session_id.clone(),
                "待删除 A1",
                Some("workspace-a".to_string()),
            )
            .expect("session should create");
        state
            .session_store
            .create_session_for_workspace(
                sibling_session_id.clone(),
                "保留 A2",
                Some("workspace-a".to_string()),
            )
            .expect("session should create");
        state
            .session_store
            .create_session_for_workspace(
                foreign_session_id.clone(),
                "外部 B1",
                Some("workspace-b".to_string()),
            )
            .expect("session should create");
        // bootstrap 现在按"会话是否有用户消息"过滤——给每个测试会话补一条用户消息让它可见
        for id in [
            &deleted_session_id,
            &sibling_session_id,
            &foreign_session_id,
        ] {
            state.session_store.append_timeline_entry(
                id.clone(),
                TimelineEntryKind::UserMessage,
                "hello",
            );
        }
        state
            .session_store
            .select_current_session(&deleted_session_id)
            .expect("deleted session should be current for replacement assertion");

        let (status, body) = post_json(
            state,
            "/session/delete",
            serde_json::json!({
                "workspaceId": "workspace-a",
                "sessionId": deleted_session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        let session_ids = body["sessions"]
            .as_array()
            .expect("sessions should be array")
            .iter()
            .map(|session| session["sessionId"].as_str().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(session_ids, vec![sibling_session_id.as_str()]);
        // 删除当前展示的会话后，bootstrap 自动选中同 workspace 内最近一条可见会话作为 current
        assert_eq!(
            body["currentSession"]["sessionId"]
                .as_str()
                .unwrap_or_default(),
            sibling_session_id.as_str()
        );
        assert_eq!(
            persisted_current
                .lock()
                .expect("persisted current capture should lock")
                .as_ref(),
            Some(&sibling_session_id),
            "删除响应中的 replacement current 必须和同一次持久化事实一致"
        );
    }

    #[tokio::test]
    async fn delete_session_purges_all_session_owned_runtime_resources() {
        let task_store = Arc::new(TaskStore::new());
        let state = test_state().with_task_store(task_store.clone());
        let workspace_id = register_workspace(
            &state,
            "workspace-delete-runtime-resources",
            "delete-runtime-resources",
        );
        let session_id = SessionId::new("session-delete-runtime-resources");
        let mission_id = MissionId::new("mission-delete-runtime-resources");
        let root_task = test_root_task("task-delete-runtime-root", mission_id.as_str());
        let mut child_task = root_task.clone();
        child_task.task_id = TaskId::new("task-delete-runtime-child");
        child_task.root_task_id = root_task.task_id.clone();
        child_task.parent_task_id = Some(root_task.task_id.clone());
        child_task.title = "child task".to_string();
        task_store
            .insert_task(root_task.clone())
            .expect("根任务应插入");
        task_store
            .insert_task(child_task.clone())
            .expect("子任务应插入");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "删除运行资源",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let (_, orchestrator_thread_id) =
            state
                .session_store
                .ensure_session_mission(&session_id, UtcMillis(10), || mission_id.clone());
        seed_conversation_turn(
            &state,
            &session_id,
            "turn-delete-runtime-resources",
            11,
            UtcMillis(11),
            "completed",
            "删除我",
        );
        state
            .task_execution_registry()
            .insert(
                root_task.task_id.clone(),
                TaskExecutionPlan::Dispatch {
                    target: TaskExecutionTarget {
                        mission_id: mission_id.clone(),
                        root_task_id: root_task.task_id.clone(),
                        task_id: root_task.task_id.clone(),
                        requested_worker_id: None,
                        recovery_id: None,
                        execution_chain_ref: None,
                    },
                    worker_id: WorkerId::new("worker-delete-runtime-resources"),
                    thread_id: orchestrator_thread_id,
                    is_primary: true,
                    session_id: session_id.clone(),
                    turn_id: "turn-delete-runtime-resources".to_string(),
                    workspace_id: Some(workspace_id.clone()),
                    execution_root: None,
                    ownership: ExecutionOwnership::default(),
                    writebacks: ExecutionWritebackPlans::default(),
                    use_tools: true,
                    skill_name: None,
                    images: Vec::new(),
                    execution_settings_snapshot: None,
                },
            )
            .expect("execution plan should register");
        state
            .conversation_registry
            .conversation_for_task(&session_id, &child_task.task_id);
        state
            .settings_store
            .set_session_section(
                &session_id,
                "orchestrator",
                json!({"model": "delete-model", "reasoningEffort": "high"}),
            )
            .unwrap();
        state
            .enqueue_regular_session_turn(QueuedRegularSessionTurn {
                request: session_turn_request("排队消息"),
                request_fingerprint: None,
                requested_workspace_id: Some(workspace_id.clone()),
                accepted_at: UtcMillis(13),
                route: SessionTurnRouteDto::Chat,
                task_title: None,
                execution_goal: None,
                required_tool_chain: Vec::new(),
                completion_contract: TaskCompletionContract::default(),
                recovery_checkpoint: None,
                session_id: session_id.clone(),
                workspace_id: None,
                queue_id: "queue-delete-runtime-resources".to_string(),
                retry_count: 0,
            })
            .expect("queued turn should persist");

        let (status, body) = post_json(
            state.clone(),
            "/session/delete",
            json!({
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert!(task_store.get_tasks_by_mission(&mission_id).is_empty());
        assert!(
            state
                .task_execution_registry()
                .get(&root_task.task_id)
                .is_none()
        );
        assert!(state.conversation_registry.is_empty());
        assert_eq!(
            state
                .settings_store
                .get_session_section(&session_id, "orchestrator"),
            serde_json::Value::Null
        );
        assert_eq!(state.queued_regular_session_turn_count(&session_id), 0);
        assert!(
            state
                .session_store
                .thread_registry_snapshot(&session_id)
                .is_empty()
        );
        assert!(
            state
                .session_store
                .canonical_turns_for_session(&session_id)
                .is_empty()
        );
    }

    #[tokio::test]
    async fn delete_session_keeps_session_when_settings_cleanup_cannot_persist() {
        let root = unique_temp_dir("session-delete-settings-failure");
        let blocked_parent = root.join("blocked-parent");
        fs::write(&blocked_parent, b"not-a-directory").unwrap();
        let settings_store = Arc::new(SettingsStore::with_persistence_path(
            blocked_parent.join("settings.json"),
        ));
        let state = test_state().with_settings_store(settings_store);
        let workspace_id = register_workspace(
            &state,
            "workspace-delete-settings-failure",
            "workspace-delete-settings-failure",
        );
        let session_id = SessionId::new("session-delete-settings-failure");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "设置失败保留会话",
                Some(workspace_id.to_string()),
            )
            .unwrap();

        let (status, body) = post_json(
            state.clone(),
            "/session/delete",
            json!({
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "unexpected body: {body}"
        );
        assert!(state.session_store.session(&session_id).is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn delete_session_after_restart_recovers_task_mission_from_canonical_history() {
        let session_id = SessionId::new("session-delete-after-restart");
        let workspace_id = WorkspaceId::new("workspace-delete-after-restart");
        let mission_id = MissionId::new("mission-delete-after-restart");
        let root_task = test_root_task("task-delete-after-restart", mission_id.as_str());
        let pre_restart_store = SessionStore::new();
        pre_restart_store
            .create_session_for_workspace(
                session_id.clone(),
                "重启后删除",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let pre_restart_coordinator = magi_conversation_runtime::SessionTurnCoordinator::new();
        crate::routes::test_turn_fixtures::seed_conversation_turn(
            &pre_restart_store,
            &pre_restart_coordinator,
            &session_id,
            "turn-delete-after-restart",
            21,
            UtcMillis(21),
            "running",
            "重启后删除",
        );
        magi_conversation_runtime::CanonicalTurnEventSink::for_store(&pre_restart_store, None)
            .upsert_item_sidecar(
                &session_id,
                Some("turn-delete-after-restart"),
                ActiveExecutionTurnItem {
                    item_id: "item-delete-after-restart-task-owner".to_string(),
                    item_seq: 2,
                    kind: "user_message".to_string(),
                    status: "completed".to_string(),
                    source: "user".to_string(),
                    title: None,
                    content: Some("重启后删除".to_string()),
                    task_id: Some(root_task.task_id.clone()),
                    worker_id: None,
                    role_id: None,
                    tool_call_id: None,
                    tool_name: None,
                    tool_status: None,
                    tool_arguments: None,
                    tool_result: None,
                    tool_error: None,
                    request_id: None,
                    user_message_id: None,
                    placeholder_message_id: None,
                    metadata: Default::default(),
                    timeline_entry_id: None,
                    source_thread_id: ThreadId::new("thread-before-restart"),
                },
            )
            .expect("canonical task owner item should persist");
        crate::routes::test_turn_fixtures::finish_conversation_turn(
            &pre_restart_store,
            &pre_restart_coordinator,
            &session_id,
            "turn-delete-after-restart",
            "completed",
        );
        let restored_store = Arc::new(
            SessionStore::from_persisted_parts(
                pre_restart_store.durable_state(),
                SessionExecutionSidecarStoreState::default(),
            )
            .expect("canonical session history should restore"),
        );
        assert!(
            restored_store
                .thread_registry_snapshot(&session_id)
                .is_empty()
        );

        let workspace_store = Arc::new(WorkspaceStore::default());
        let workspace_root = unique_temp_dir("delete-after-restart");
        workspace_store
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_root.display().to_string()),
            )
            .expect("workspace should register");
        let task_store = Arc::new(TaskStore::new());
        task_store.insert_task(root_task).expect("根任务应插入");
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            restored_store,
            workspace_store,
            Arc::new(GovernanceService::default()),
        )
        .with_task_store(task_store.clone());

        let (status, body) = post_json(
            state,
            "/session/delete",
            json!({
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert!(task_store.get_tasks_by_mission(&mission_id).is_empty());
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn rename_session_rejects_workspace_mismatched_session() {
        let state = test_state();
        register_workspace(&state, "workspace-a", "rename-mismatch-a");
        register_workspace(&state, "workspace-b", "rename-mismatch-b");
        let session_id = SessionId::new("session-rename-workspace-a");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "原始名称",
                Some("workspace-a".to_string()),
            )
            .expect("session should create");

        let (status, body) = post_json(
            state.clone(),
            "/session/rename",
            serde_json::json!({
                "workspaceId": "workspace-b",
                "sessionId": session_id.as_str(),
                "name": "错误改名",
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("不属于 workspace workspace-b"),
            "unexpected body: {body}"
        );
        assert_eq!(
            state
                .session_store
                .session(&session_id)
                .expect("session should remain")
                .title,
            "原始名称"
        );
    }

    #[tokio::test]
    async fn rename_session_updates_title_without_switching_current_session() {
        let state = test_state();
        let workspace_id = register_workspace(&state, "workspace-rename", "rename-session");
        let renamed_session_id = SessionId::new("session-to-rename");
        let current_session_id = SessionId::new("session-current");
        state
            .session_store
            .create_session_for_workspace(
                renamed_session_id.clone(),
                "原始名称",
                Some(workspace_id.to_string()),
            )
            .expect("renamed session should create");
        state
            .session_store
            .create_session_for_workspace(
                current_session_id.clone(),
                "当前会话",
                Some(workspace_id.to_string()),
            )
            .expect("current session should create");
        for session_id in [&renamed_session_id, &current_session_id] {
            state.session_store.append_timeline_entry(
                session_id.clone(),
                TimelineEntryKind::UserMessage,
                "用于验证可见会话重命名",
            );
        }

        let (status, body) = post_json(
            state.clone(),
            "/session/rename",
            serde_json::json!({
                "workspaceId": workspace_id.as_str(),
                "sessionId": renamed_session_id.as_str(),
                "name": "  新名称  ",
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(
            body["currentSession"]["sessionId"],
            current_session_id.as_str()
        );
        assert_eq!(
            state
                .session_store
                .session(&renamed_session_id)
                .expect("renamed session should remain")
                .title,
            "新名称"
        );
        assert!(body["sessions"].as_array().is_some_and(|sessions| {
            sessions.iter().any(|session| {
                session["sessionId"] == renamed_session_id.as_str() && session["title"] == "新名称"
            })
        }));
        let title_events = state
            .event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .filter(|event| event.event_type == "session.title.updated")
            .collect::<Vec<_>>();
        assert_eq!(title_events.len(), 1);
        assert_eq!(
            title_events[0].payload["session_id"],
            renamed_session_id.as_str()
        );
        assert_eq!(
            title_events[0].payload["workspace_id"],
            workspace_id.as_str()
        );
        assert_eq!(title_events[0].payload["title"], "新名称");

        let (status, _) = post_json(
            state.clone(),
            "/session/rename",
            serde_json::json!({
                "workspaceId": workspace_id.as_str(),
                "sessionId": renamed_session_id.as_str(),
                "name": "新名称",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            state
                .event_bus
                .snapshot()
                .recent_events
                .into_iter()
                .filter(|event| event.event_type == "session.title.updated")
                .count(),
            1,
            "相同名称不能重复发布标题更新事件"
        );
    }

    #[tokio::test]
    async fn rename_session_rejects_invalid_names_as_input_errors() {
        let state = test_state();
        let workspace_id = register_workspace(&state, "workspace-rename-invalid", "rename-invalid");
        let session_id = SessionId::new("session-rename-invalid");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "原始名称",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let invalid_names = [
            "   ".to_string(),
            "包含\n换行".to_string(),
            std::iter::repeat_n('字', magi_session_store::SESSION_TITLE_MAX_CHARS + 1).collect(),
        ];

        for name in invalid_names {
            let (status, body) = post_json(
                state.clone(),
                "/session/rename",
                serde_json::json!({
                    "workspaceId": workspace_id.as_str(),
                    "sessionId": session_id.as_str(),
                    "name": name,
                }),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
            assert_eq!(body["error_code"], "INPUT_INVALID");
        }
        assert_eq!(
            state
                .session_store
                .session(&session_id)
                .expect("session should remain")
                .title,
            "原始名称"
        );
        assert!(
            state
                .event_bus
                .snapshot()
                .recent_events
                .into_iter()
                .all(|event| event.event_type != "session.title.updated")
        );
    }

    #[tokio::test]
    async fn close_session_rejects_workspace_mismatched_session() {
        let state = test_state();
        register_workspace(&state, "workspace-a", "close-mismatch-a");
        register_workspace(&state, "workspace-b", "close-mismatch-b");
        let session_id = SessionId::new("session-close-workspace-a");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "工作区 A 关闭保护",
                Some("workspace-a".to_string()),
            )
            .expect("session should create");

        let (status, body) = post_json(
            state.clone(),
            "/session/close",
            serde_json::json!({
                "workspaceId": "workspace-b",
                "sessionId": session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("不属于 workspace workspace-b"),
            "unexpected body: {body}"
        );
        assert_eq!(
            format!(
                "{:?}",
                state
                    .session_store
                    .session(&session_id)
                    .expect("session should remain")
                    .status
            ),
            "Active"
        );
    }

    #[tokio::test]
    async fn session_navigation_requires_workspace_scope() {
        let state = test_state();
        let session_id = SessionId::new("session-switch-requires-workspace");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "需要 workspace 的切换",
                Some("workspace-a".to_string()),
            )
            .expect("session should create");

        let (status, body) = post_json(
            state,
            "/session/navigation",
            serde_json::json!({
                "target": "session",
                "scope": "personal",
                "sessionId": session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("不属于个人会话"),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn materialize_session_creates_selected_personal_session() {
        let state = test_state();

        let (status, body) = post_json(
            state.clone(),
            "/session/materialize",
            serde_json::json!({ "scope": "personal" }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        let session_id = SessionId::new(
            body["sessionId"]
                .as_str()
                .expect("materialized session id should exist"),
        );
        let session = state
            .session_store
            .session(&session_id)
            .expect("materialized session should persist");
        assert_eq!(session.workspace_id, None);
        assert_eq!(
            state
                .session_store
                .current_session()
                .map(|item| item.session_id),
            Some(session_id)
        );
    }

    #[tokio::test]
    async fn materialize_session_rolls_back_entity_and_current_on_persist_failure() {
        let existing_session_id = SessionId::new("session-materialize-existing-current");
        let base_state = test_state();
        base_state
            .session_store
            .create_session(existing_session_id.clone(), "已有会话")
            .expect("existing session should create");
        base_state
            .session_store
            .select_current_session(&existing_session_id)
            .expect("existing session should select");

        let persist_calls = Arc::new(AtomicUsize::new(0));
        let persist_calls_for_callback = persist_calls.clone();
        let state = base_state.with_session_projection_persist(Arc::new(
            move |_durable, _sidecars, _mode| {
                if persist_calls_for_callback.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(ApiError::internal_assembly(
                        "测试 projection 持久化失败",
                        "injected failure",
                    ));
                }
                Ok(())
            },
        ));

        let (status, _body) = post_json(
            state.clone(),
            "/session/materialize",
            serde_json::json!({ "scope": "personal" }),
        )
        .await;

        assert_ne!(status, StatusCode::OK);
        assert_eq!(persist_calls.load(Ordering::SeqCst), 2);
        assert_eq!(state.session_store.sessions().len(), 1);
        assert_eq!(
            state
                .session_store
                .current_session()
                .map(|session| session.session_id),
            Some(existing_session_id)
        );
        assert!(
            state
                .event_bus
                .snapshot()
                .recent_events
                .iter()
                .all(|event| event.event_type != "session.created"),
            "持久化失败的会话不能发布 session.created"
        );
    }

    #[tokio::test]
    async fn materialize_session_preserves_workspace_binding() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-materialize-session",
            "workspace-materialize-session",
        );

        let (status, body) = post_json(
            state.clone(),
            "/session/materialize",
            serde_json::json!({
                "scope": "workspace",
                "workspaceId": workspace_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        let session_id = SessionId::new(
            body["sessionId"]
                .as_str()
                .expect("materialized session id should exist"),
        );
        let session = state
            .session_store
            .session(&session_id)
            .expect("materialized session should persist");
        assert_eq!(session.workspace_id.as_deref(), Some(workspace_id.as_str()));
        assert_eq!(body["workspaceId"], workspace_id.as_str());
    }

    #[test]
    fn session_navigation_requires_explicit_scope() {
        let result = serde_json::from_value::<SessionNavigationRequest>(
            serde_json::json!({ "target": "draft" }),
        );

        assert!(result.is_err(), "缺少 scope 的导航请求必须在协议边界被拒绝");
    }

    #[tokio::test]
    async fn draft_navigation_persists_empty_selection_without_deleting_history() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-draft-navigation",
            "session-draft-navigation",
        );
        let session_id = SessionId::new("session-draft-existing-history");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "已有会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        state.session_store.append_timeline_entry(
            session_id.clone(),
            TimelineEntryKind::UserMessage,
            "已有消息",
        );

        let (status, body) = post_json(
            state.clone(),
            "/session/navigation",
            serde_json::json!({
                "target": "draft",
                "scope": "workspace",
                "workspaceId": workspace_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert!(body["currentSession"].is_null());
        assert_eq!(body["sessions"].as_array().map(Vec::len), Some(1));
        assert!(body["timeline"].as_array().is_some_and(Vec::is_empty));
        assert!(state.session_store.current_session().is_none());
        assert!(state.session_store.session(&session_id).is_some());
        assert_eq!(
            state.workspace_registry.active_workspace_id(),
            Some(workspace_id)
        );
    }

    #[tokio::test]
    async fn draft_navigation_rejects_session_id_without_mutating_selection() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-draft-invalid-session",
            "session-draft-invalid-session",
        );
        let session_id = SessionId::new("session-draft-invalid-session");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "保留当前会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        state
            .session_store
            .select_current_session(&session_id)
            .expect("session should select");

        let (status, body) = post_json(
            state.clone(),
            "/session/navigation",
            serde_json::json!({
                "target": "draft",
                "scope": "workspace",
                "workspaceId": workspace_id.as_str(),
                "sessionId": session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert_eq!(
            state.session_store.current_session().unwrap().session_id,
            session_id
        );
    }

    #[tokio::test]
    async fn session_navigation_uses_workspace_path_when_workspace_id_is_stale() {
        let state = test_state();
        let workspace_a = WorkspaceId::new("workspace-switch-path-a");
        let workspace_b = WorkspaceId::new("workspace-switch-path-b");
        let root_a = unique_temp_dir("session-switch-path-a");
        let root_b = unique_temp_dir("session-switch-path-b");
        state
            .workspace_registry
            .register(
                workspace_a.clone(),
                AbsolutePath::new(root_a.display().to_string()),
            )
            .expect("workspace a should register");
        state
            .workspace_registry
            .register(
                workspace_b.clone(),
                AbsolutePath::new(root_b.display().to_string()),
            )
            .expect("workspace b should register");
        let session_id = SessionId::new("session-switch-path-a");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "路径绑定切换",
                Some(workspace_a.to_string()),
            )
            .expect("session should create");

        let (status, body) = post_json(
            state,
            "/session/navigation",
            serde_json::json!({
                "target": "session",
                "scope": "workspace",
                "workspaceId": workspace_b.as_str(),
                "workspacePath": root_a.display().to_string(),
                "sessionId": session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["currentSession"]["sessionId"], session_id.as_str());
    }

    #[tokio::test]
    async fn session_navigation_persists_selection_without_business_side_effects() {
        let state = test_state();
        register_workspace(&state, "workspace-a", "session-switch-navigation-only");
        let first_session_id = SessionId::new("session-switch-navigation-first");
        let second_session_id = SessionId::new("session-switch-navigation-second");
        state
            .session_store
            .create_session_for_workspace(
                first_session_id.clone(),
                "第一会话",
                Some("workspace-a".to_string()),
            )
            .unwrap();
        state
            .session_store
            .create_session_for_workspace(
                second_session_id.clone(),
                "第二会话",
                Some("workspace-a".to_string()),
            )
            .unwrap();
        let timeline_count = state.session_store.timeline().len();
        let first_updated_at = state
            .session_store
            .session(&first_session_id)
            .expect("first session should exist")
            .updated_at;
        let second_updated_at = state
            .session_store
            .session(&second_session_id)
            .expect("second session should exist")
            .updated_at;

        let (status, body) = post_json(
            state.clone(),
            "/session/navigation",
            serde_json::json!({
                "target": "session",
                "scope": "workspace",
                "workspaceId": "workspace-a",
                "sessionId": first_session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(
            body["currentSession"]["sessionId"],
            first_session_id.as_str()
        );
        assert_eq!(
            state.session_store.current_session().unwrap().session_id,
            first_session_id
        );
        assert_eq!(state.session_store.timeline().len(), timeline_count);
        assert_eq!(
            state
                .session_store
                .session(&first_session_id)
                .expect("first session should remain")
                .updated_at,
            first_updated_at
        );
        assert_eq!(
            state
                .session_store
                .session(&second_session_id)
                .expect("second session should remain")
                .updated_at,
            second_updated_at
        );
    }

    #[tokio::test]
    async fn notifications_require_workspace_but_allow_workspace_scope_without_session() {
        let state = test_state();
        register_workspace(&state, "workspace-a", "notification-explicit-a");
        let session_id = SessionId::new("session-notification-explicit-scope");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "通知显式 scope 会话",
                Some("workspace-a".to_string()),
            )
            .expect("session should create");
        append_test_incident(
            &state,
            "notification-explicit-scope",
            NotificationScope::Workspace,
            Some("workspace-a"),
            None,
            "必须显式指定 workspace 和 session",
        );

        let personal_response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/notifications?scope=personal")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        assert_eq!(personal_response.status(), StatusCode::OK);

        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/notifications?scope=workspace&sessionId={}",
                        session_id.as_str()
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        let status = response.status();
        let body = serde_json::from_slice::<serde_json::Value>(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body should read"),
        )
        .expect("response should be json");

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("workspaceId 不能为空"),
            "unexpected body: {body}"
        );

        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/notifications?scope=workspace&workspaceId=workspace-a")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        let status = response.status();
        let body = serde_json::from_slice::<serde_json::Value>(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body should read"),
        )
        .expect("response should be json");

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert!(body["sessionId"].is_null());
        assert_eq!(
            body["notifications"]["records"]
                .as_array()
                .expect("records should be array")
                .len(),
            1
        );

        let (status, body) = post_json(
            state,
            "/notifications/mark-all-read",
            serde_json::json!({ "sessionId": session_id.as_str() }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("workspaceId 不能为空"),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn notifications_workspace_query_uses_explicit_execution_owned_session() {
        let state = test_state();
        register_workspace(
            &state,
            "workspace-owned-notifications",
            "notification-owned-current",
        );
        let session_id = SessionId::new("session-notification-owned-current");
        state
            .session_store
            .create_session(session_id.clone(), "ownership 绑定当前会话")
            .expect("session should create");
        state.session_store.bind_execution_ownership(
            session_id.clone(),
            ExecutionOwnership {
                session_id: Some(session_id.clone()),
                workspace_id: Some(WorkspaceId::new("workspace-owned-notifications")),
                ..ExecutionOwnership::default()
            },
        );
        append_test_incident(
            &state,
            "notification-owned-current",
            NotificationScope::Session,
            Some("workspace-owned-notifications"),
            Some(&session_id),
            "应按 execution ownership 归属加载",
        );

        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/notifications?scope=workspace&workspaceId=workspace-owned-notifications&sessionId={}",
                        session_id.as_str()
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        let status = response.status();
        let body = serde_json::from_slice::<serde_json::Value>(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body should read"),
        )
        .expect("response should be json");

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["sessionId"], session_id.as_str());
        assert_eq!(body["workspaceId"], "workspace-owned-notifications");
        assert_eq!(
            body["notifications"]["records"]
                .as_array()
                .expect("records should be array")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn mark_all_notifications_read_rejects_workspace_mismatched_session() {
        let state = test_state();
        register_workspace(&state, "workspace-a", "notification-mismatch-a");
        register_workspace(&state, "workspace-b", "notification-mismatch-b");
        let session_id = SessionId::new("session-notification-workspace-a");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "工作区 A 会话",
                Some("workspace-a".to_string()),
            )
            .expect("session should create");
        append_test_incident(
            &state,
            "notification-workspace-a",
            NotificationScope::Session,
            Some("workspace-a"),
            Some(&session_id),
            "只能在 workspace-a 中处理",
        );

        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/notifications/mark-all-read")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "workspaceId": "workspace-b",
                            "sessionId": session_id.as_str(),
                        })
                        .to_string(),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        let status = response.status();
        let body = serde_json::from_slice::<serde_json::Value>(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body should read"),
        )
        .expect("response should be json");

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("不属于 workspace workspace-b"),
            "unexpected body: {body}"
        );
        assert!(
            !state
                .session_store
                .notifications_for_context(&NotificationContext::workspace(
                    "workspace-a",
                    Some(session_id.clone()),
                ))[0]
                .handled
        );
    }

    #[tokio::test]
    async fn notifications_actions_accept_execution_owned_unbound_workspace_session() {
        let state = test_state();
        register_workspace(
            &state,
            "workspace-owned-actions",
            "notification-owned-actions",
        );
        let session_id = SessionId::new("session-notification-owned-actions");
        state
            .session_store
            .create_session(session_id.clone(), "ownership 绑定通知操作")
            .expect("session should create");
        state.session_store.bind_execution_ownership(
            session_id.clone(),
            ExecutionOwnership {
                session_id: Some(session_id.clone()),
                workspace_id: Some(WorkspaceId::new("workspace-owned-actions")),
                ..ExecutionOwnership::default()
            },
        );
        for notification_id in ["notification-owned-read", "notification-owned-remove"] {
            append_test_incident(
                &state,
                notification_id,
                NotificationScope::Session,
                Some("workspace-owned-actions"),
                Some(&session_id),
                "应允许归属 workspace 的通知操作",
            );
        }

        let (status, body) = post_json(
            state.clone(),
            "/notifications/mark-all-read",
            serde_json::json!({
                "workspaceId": "workspace-owned-actions",
                "sessionId": session_id.as_str(),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["workspaceId"], "workspace-owned-actions");
        assert!(
            state
                .session_store
                .notifications_for_context(&NotificationContext::workspace(
                    "workspace-owned-actions",
                    Some(session_id.clone()),
                ))
                .iter()
                .all(|notification| notification.handled)
        );

        let (status, body) = post_json(
            state.clone(),
            "/notifications/resolve",
            serde_json::json!({
                "workspaceId": "workspace-owned-actions",
                "sessionId": session_id.as_str(),
                "notificationId": "notification-owned-read",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        let resolved = body["notifications"]["records"]
            .as_array()
            .expect("records should be array")
            .iter()
            .find(|record| record["notificationId"] == "notification-owned-read")
            .expect("resolved incident should remain visible");
        assert_eq!(resolved["resolved"], true);

        let (status, body) = post_json(
            state.clone(),
            "/notifications/remove",
            serde_json::json!({
                "workspaceId": "workspace-owned-actions",
                "sessionId": session_id.as_str(),
                "notificationId": "notification-owned-remove",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        let records = body["notifications"]["records"]
            .as_array()
            .expect("records should be array");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["notificationId"], "notification-owned-read");

        let (status, body) = post_json(
            state.clone(),
            "/notifications/clear",
            serde_json::json!({
                "workspaceId": "workspace-owned-actions",
                "sessionId": session_id.as_str(),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(
            body["notifications"]["records"]
                .as_array()
                .expect("records should be array")
                .len(),
            0
        );
        assert!(
            state
                .session_store
                .notifications_for_context(&NotificationContext::workspace(
                    "workspace-owned-actions",
                    Some(session_id.clone()),
                ))
                .is_empty()
        );
    }

    #[tokio::test]
    async fn report_incident_persists_each_sanitized_failure_with_unique_server_id() {
        let state = test_state();
        register_workspace(&state, "workspace-a", "notification-append-a");
        let session_id = SessionId::new("session-notification-append");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "通知 append 会话",
                Some("workspace-a".to_string()),
            )
            .expect("session should create");

        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/notifications/report")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "workspaceId": "workspace-a",
                            "sessionId": session_id.as_str(),
                            "scope": "session",
                            "level": "error",
                            "title": "模型请求失败",
                            "message": "provider timeout at /Users/xie/code/model.rs",
                            "detail": "request failed with sk-test-secret-value",
                            "errorCode": "MODEL_TIMEOUT",
                            "failureStage": "model_invocation",
                            "taskId": "task-model-request-failed",
                            "requestId": "request-model-request-failed",
                            "source": "web-action",
                            "actionRequired": true
                        })
                        .to_string(),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        let status = response.status();
        let body = serde_json::from_slice::<serde_json::Value>(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body should read"),
        )
        .expect("response should be json");

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["sessionId"], session_id.as_str());
        let records = body["notifications"]["records"]
            .as_array()
            .expect("records should be array");
        assert_eq!(records.len(), 1);
        let first_notification_id = records[0]["notificationId"]
            .as_str()
            .expect("notification id should exist")
            .to_string();
        assert!(first_notification_id.starts_with("notification-"));
        assert_eq!(records[0]["kind"], "incident");
        assert_eq!(records[0]["scope"], "session");
        assert_eq!(records[0]["level"], "error");
        assert_eq!(records[0]["title"], "模型请求失败");
        assert_eq!(records[0]["errorCode"], "MODEL_TIMEOUT");
        assert_eq!(records[0]["failureStage"], "model_invocation");
        assert_eq!(records[0]["taskId"], "task-model-request-failed");
        assert_eq!(records[0]["requestId"], "request-model-request-failed");
        assert!(
            records[0]["message"]
                .as_str()
                .is_some_and(|message| message.contains("provider timeout at [path]"))
        );
        assert!(
            records[0]["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("sk-[redacted]"))
        );
        assert_eq!(records[0]["source"], "web-action");
        assert_eq!(records[0]["read"], false);
        assert_eq!(records[0]["handled"], false);
        assert_eq!(records[0]["resolved"], false);
        assert_eq!(records[0]["occurrenceCount"], 1);

        let stored =
            state
                .session_store
                .notifications_for_context(&NotificationContext::workspace(
                    "workspace-a",
                    Some(session_id.clone()),
                ));
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].notification_id, first_notification_id);
        assert_eq!(stored[0].kind, "incident");
        assert!(!stored[0].handled);

        let second_response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/notifications/report")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "workspaceId": "workspace-a",
                            "sessionId": session_id.as_str(),
                            "scope": "session",
                            "level": "error",
                            "title": "模型请求失败",
                            "message": "provider timeout at /Users/xie/code/model.rs",
                            "source": "web-action"
                        })
                        .to_string(),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("second report should respond");
        let second_body = serde_json::from_slice::<serde_json::Value>(
            &to_bytes(second_response.into_body(), usize::MAX)
                .await
                .expect("second response body should read"),
        )
        .expect("second response should be json");
        let second_records = second_body["notifications"]["records"]
            .as_array()
            .expect("second records should be array");
        assert_eq!(second_records.len(), 2);
        let second_notification_id = second_records
            .iter()
            .filter_map(|record| record["notificationId"].as_str())
            .find(|notification_id| *notification_id != first_notification_id)
            .expect("second notification id should be unique");
        assert_ne!(second_notification_id, first_notification_id);

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/session/notifications/append")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn personal_session_incidents_are_session_owned_without_workspace_binding() {
        let state = test_state();
        let session_id = SessionId::new("session-personal-notification-incident");
        state
            .session_store
            .create_session(session_id.clone(), "个人异常记录")
            .expect("personal session should create");

        let (status, body) = post_json(
            state.clone(),
            "/notifications/report",
            serde_json::json!({
                "sessionId": session_id.as_str(),
                "scope": "session",
                "level": "error",
                "message": "个人会话异常记录合同测试"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert!(body.get("workspaceId").is_none());
        assert_eq!(body["sessionId"], session_id.as_str());
        let records = body["notifications"]["records"]
            .as_array()
            .expect("records should be array");
        assert_eq!(records.len(), 1);
        assert!(records[0]["workspaceId"].is_null());
        assert_eq!(records[0]["sessionId"], session_id.as_str());

        let personal_context = NotificationContext::personal(Some(session_id.clone()));
        let stored = state
            .session_store
            .notifications_for_context(&personal_context);
        assert_eq!(stored.len(), 1);
        assert!(stored[0].workspace_id.is_none());

        let (status, body) = post_json(
            state.clone(),
            "/notifications/resolve",
            serde_json::json!({
                "sessionId": session_id.as_str(),
                "notificationId": stored[0].notification_id,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["notifications"]["records"][0]["resolved"], true);

        let (status, body) = post_json(
            state.clone(),
            "/notifications/clear",
            serde_json::json!({ "sessionId": session_id.as_str() }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert!(
            body["notifications"]["records"]
                .as_array()
                .expect("records should be array")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn mark_all_notifications_read_rejects_unregistered_workspace_scope() {
        let persistence_root = unique_temp_dir("magi-api-notification-orphan-workspace");
        let session_id = SessionId::new("session-notification-orphan-workspace");
        let state = test_state().with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
            persistence_root.clone(),
            persistence_root.join("workspaces.json"),
            persistence_root.join("knowledge.json"),
        )));
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "未知工作区会话",
                Some("workspace-missing".to_string()),
            )
            .expect("session should create");
        append_test_incident(
            &state,
            "notification-orphan-workspace",
            NotificationScope::Session,
            Some("workspace-missing"),
            Some(&session_id),
            "未知工作区通知",
        );

        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/notifications/mark-all-read")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "workspaceId": "workspace-missing",
                            "sessionId": session_id.as_str(),
                        })
                        .to_string(),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        let status = response.status();
        let body = serde_json::from_slice::<serde_json::Value>(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body should read"),
        )
        .expect("response should be json");

        assert_eq!(status, StatusCode::NOT_FOUND, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("workspace 不存在"),
            "unexpected body: {body}"
        );
        assert!(
            !state
                .session_store
                .notifications_for_context(&NotificationContext::workspace(
                    "workspace-missing",
                    Some(session_id.clone()),
                ))[0]
                .handled
        );

        let _ = fs::remove_dir_all(persistence_root);
    }
}
