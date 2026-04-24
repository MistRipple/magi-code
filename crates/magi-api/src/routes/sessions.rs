use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_core::TaskStatus;
use magi_core::{DomainError, EventId, SessionId, UtcMillis, WorkerId};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_session_store::{
    ActiveExecutionTurn, ActiveExecutionTurnItem, SessionRecord, TimelineEntryKind,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{Duration, Instant};

use super::append_session_user_message;
use crate::{
    dto::{
        BootstrapDto, SessionNotificationsResponseDto, SessionTurnRequestDto,
        SessionTurnResponseDto, SessionTurnRouteDto,
    },
    errors::ApiError,
    state::ApiState,
    task_execution::{SessionTurnExecutionRequest, continue_shadow_execution_chain},
};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/session/new", post(create_session))
        .route("/session/turn", post(submit_session_turn))
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

async fn submit_session_turn(
    State(state): State<ApiState>,
    Json(request): Json<SessionTurnRequestDto>,
) -> Result<Json<SessionTurnResponseDto>, ApiError> {
    validate_session_turn_input(&request)?;
    let accepted_at = super::monotonic_accepted_at();
    let decision = decide_session_turn_locally(&request);
    match decision.route {
        SessionTurnRouteDto::Chat | SessionTurnRouteDto::Execute => {
            submit_regular_session_turn(state, request, accepted_at, decision).map(Json)
        }
        SessionTurnRouteDto::Task => {
            let (accepted, event_id) = super::accept_session_task_submission(
                &state,
                &request,
                decision.task_title.clone(),
                decision.execution_goal.clone(),
            )?;
            super::spawn_session_task_dispatch(state.clone(), accepted.clone());
            let execution_chain_ref = state
                .session_store
                .runtime_sidecar(&accepted.session_id)
                .and_then(|sidecar| sidecar.ownership.execution_chain_ref);
            Ok(Json(SessionTurnResponseDto::new(
                accepted.session_id,
                accepted.entry_id,
                event_id,
                accepted.accepted_at,
                accepted.created_session,
                SessionTurnRouteDto::Task,
                Some(accepted.root_task_id),
                Some(accepted.action_task_id),
                execution_chain_ref,
            )))
        }
        SessionTurnRouteDto::Continue => {
            let session_id = request
                .requested_session_id()
                .or_else(|| {
                    state
                        .session_store
                        .current_session()
                        .map(|session| session.session_id)
                })
                .ok_or_else(|| ApiError::InvalidInput("继续会话需要明确的 session".to_string()))?;
            let prompt_text = request.trimmed_text();
            let accepted = continue_shadow_execution_chain(&state, &session_id, &[])?;
            spawn_continue_session_finalize(state.clone(), accepted.clone(), accepted_at);
            if let Some(prompt_text) = prompt_text.as_deref() {
                append_session_user_message(&state, &session_id, accepted_at, prompt_text);
            }
            state.persist_runtime_durable_state()?;
            let event_id = publish_session_turn_continue_event(&state, &accepted, accepted_at)?;
            Ok(Json(SessionTurnResponseDto::new(
                accepted.session_id,
                format!("timeline-{}-{}", session_id, accepted_at.0),
                event_id,
                accepted_at,
                false,
                SessionTurnRouteDto::Continue,
                Some(accepted.root_task_id),
                Some(accepted.action_task_id),
                Some(accepted.execution_chain_ref),
            )))
        }
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

#[derive(Debug)]
struct SessionTurnIntentDecision {
    route: SessionTurnRouteDto,
    task_title: Option<String>,
    execution_goal: Option<String>,
    required_workers: Vec<String>,
    tool_intent: Option<String>,
}

fn validate_session_turn_input(request: &SessionTurnRequestDto) -> Result<(), ApiError> {
    if request.trimmed_text().is_none()
        && request
            .skill_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
        && request.images.is_empty()
    {
        return Err(ApiError::InvalidInput("会话输入不能为空".to_string()));
    }
    Ok(())
}

fn decide_session_turn_locally(request: &SessionTurnRequestDto) -> SessionTurnIntentDecision {
    if request.deep_task {
        let trimmed_text = request.trimmed_text();
        let task_title = request.mission_title(trimmed_text.as_deref());
        return SessionTurnIntentDecision {
            route: SessionTurnRouteDto::Task,
            task_title: Some(task_title),
            execution_goal: trimmed_text,
            required_workers: Vec::new(),
            tool_intent: None,
        };
    }

    SessionTurnIntentDecision {
        route: SessionTurnRouteDto::Chat,
        task_title: None,
        execution_goal: None,
        required_workers: Vec::new(),
        tool_intent: None,
    }
}

fn submit_regular_session_turn(
    state: ApiState,
    request: SessionTurnRequestDto,
    accepted_at: UtcMillis,
    decision: SessionTurnIntentDecision,
) -> Result<SessionTurnResponseDto, ApiError> {
    let trimmed_text = request.trimmed_text();
    let message = request.timeline_message(trimmed_text.as_deref());
    let title_seed = trimmed_text.as_deref().unwrap_or("新会话");
    let (session_id, created_session, workspace_id) = super::resolve_dispatch_session(
        &state,
        request.requested_session_id(),
        request
            .requested_workspace_id()
            .map(magi_core::WorkspaceId::new),
        title_seed,
        accepted_at,
    )?;
    let entry_id = format!("timeline-{}-{}", session_id, accepted_at.0);
    append_session_user_message(&state, &session_id, accepted_at, &message);
    let mut turn = ActiveExecutionTurn {
        turn_id: format!("turn-session-{}", accepted_at.0),
        turn_seq: accepted_at.0 as u64,
        accepted_at,
        status: "running".to_string(),
        user_message: Some(message.clone()),
        items: vec![ActiveExecutionTurnItem {
            item_id: format!("turn-item-user-{}", accepted_at.0),
            item_seq: 1,
            lane_id: None,
            lane_seq: None,
            kind: "user_message".to_string(),
            status: "completed".to_string(),
            source: "user".to_string(),
            title: None,
            content: Some(message.clone()),
            task_id: None,
            worker_id: None,
            role_id: None,
            tool_call_id: None,
            tool_name: None,
            tool_status: None,
            tool_arguments: None,
            tool_result: None,
            tool_error: None,
            thread_visible: false,
            worker_visible: false,
        }],
        worker_lanes: Vec::new(),
    };
    turn.normalize();
    state
        .session_store
        .upsert_current_turn(session_id.clone(), turn)
        .map_err(|error| ApiError::internal_assembly("写入 session turn 失败", error))?;
    let event_id = publish_regular_session_turn_accepted_event(
        &state,
        &session_id,
        workspace_id.as_ref(),
        accepted_at,
        created_session,
        decision.route,
    )?;
    let prompt = decision
        .tool_intent
        .as_deref()
        .filter(|intent| !intent.trim().is_empty())
        .map(|intent| format!("{}\n\n用户原始输入：{}", intent.trim(), message))
        .unwrap_or_else(|| message.clone());
    spawn_regular_session_turn_execution(
        state.clone(),
        SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            workspace_id: workspace_id.clone(),
            prompt,
            use_tools: true,
            skill_name: request.skill_name.clone(),
        },
        accepted_at,
        decision.route,
        created_session,
        decision.required_workers,
    );

    Ok(SessionTurnResponseDto::new(
        session_id,
        entry_id,
        event_id,
        accepted_at,
        created_session,
        decision.route,
        None,
        None,
        None,
    ))
}

fn spawn_regular_session_turn_execution(
    state: ApiState,
    execution_request: SessionTurnExecutionRequest,
    accepted_at: UtcMillis,
    route: SessionTurnRouteDto,
    created_session: bool,
    required_workers: Vec<String>,
) {
    tokio::task::spawn_blocking(move || {
        let session_id = execution_request.session_id.clone();
        let workspace_id = execution_request.workspace_id.clone();
        let dispatcher = match state.session_turn_dispatcher() {
            Some(dispatcher) => dispatcher.clone(),
            None => {
                tracing::error!(
                    session_id = %session_id,
                    "regular session turn background execution failed: dispatcher missing"
                );
                let _ = state
                    .session_store
                    .update_current_turn_status(&session_id, "failed");
                let _ = state.persist_session_durable_state();
                return;
            }
        };

        match dispatcher.execute_session_turn(execution_request) {
            Ok(output) => {
                append_session_assistant_message(
                    &state,
                    &session_id,
                    accepted_at,
                    &output.final_content,
                );
                if let Err(error) = state.persist_session_durable_state() {
                    tracing::error!(
                        session_id = %session_id,
                        ?error,
                        "regular session turn background persist failed"
                    );
                }
                let event_id = EventId::new(format!("event-session-turn-{}", accepted_at.0));
                if let Err(error) = state.event_bus.publish(
                    EventEnvelope::domain(
                        event_id,
                        "session.turn.completed",
                        json!({
                            "session_id": session_id.to_string(),
                            "route": route,
                            "created_session": created_session,
                            "required_workers": required_workers,
                        }),
                    )
                    .with_context(EventContext {
                        session_id: Some(session_id.clone()),
                        workspace_id,
                        ..EventContext::default()
                    }),
                ) {
                    tracing::error!(
                        session_id = %session_id,
                        ?error,
                        "regular session turn completed event publish failed"
                    );
                }
            }
            Err(error) => {
                tracing::error!(
                    session_id = %session_id,
                    ?error,
                    "regular session turn background execution failed"
                );
                let _ = state
                    .session_store
                    .update_current_turn_status(&session_id, "failed");
                let _ = state.persist_session_durable_state();
                let event_id = EventId::new(format!("event-session-turn-failed-{}", accepted_at.0));
                let _ = state.event_bus.publish(
                    EventEnvelope::domain(
                        event_id,
                        "session.turn.failed",
                        json!({
                            "session_id": session_id.to_string(),
                            "route": route,
                            "error": format!("{error:?}"),
                        }),
                    )
                    .with_context(EventContext {
                        session_id: Some(session_id),
                        workspace_id,
                        ..EventContext::default()
                    }),
                );
            }
        }
    });
}

fn publish_regular_session_turn_accepted_event(
    state: &ApiState,
    session_id: &SessionId,
    workspace_id: Option<&magi_core::WorkspaceId>,
    accepted_at: UtcMillis,
    created_session: bool,
    route: SessionTurnRouteDto,
) -> Result<EventId, ApiError> {
    let event_id = EventId::new(format!("event-session-turn-accepted-{}", accepted_at.0));
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.turn.accepted",
        json!({
            "session_id": session_id.to_string(),
            "workspace_id": workspace_id.map(ToString::to_string),
            "created_session": created_session,
            "route": route,
        }),
    )
    .with_context(EventContext {
        workspace_id: workspace_id.cloned(),
        session_id: Some(session_id.clone()),
        ..EventContext::default()
    });
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("session turn 接受事件发布失败", err))?;
    Ok(event_id)
}

fn publish_session_turn_continue_event(
    state: &ApiState,
    accepted: &crate::task_execution::SessionContinueAccepted,
    continued_at: UtcMillis,
) -> Result<EventId, ApiError> {
    let event_id = EventId::new(format!("event-session-turn-continue-{}", continued_at.0));
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.turn.continue.executed",
        json!({
            "session_id": accepted.session_id.to_string(),
            "mission_id": accepted.mission_id.to_string(),
            "root_task_id": accepted.root_task_id.to_string(),
            "execution_chain_ref": accepted.execution_chain_ref.clone(),
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
        .map_err(|err| ApiError::event_publish_failed("session turn 继续事件发布失败", err))?;
    Ok(event_id)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SwitchSessionRequest {
    session_id: String,
    #[serde(default, alias = "workspace_id")]
    workspace_id: Option<String>,
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
    if let Some(workspace_id) = request
        .workspace_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        let session = state
            .session_store
            .session(&session_id)
            .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
        if session.workspace_id.as_deref() != Some(workspace_id) {
            return Err(ApiError::InvalidInput(format!(
                "会话 {} 不属于 workspace {}",
                session_id, workspace_id
            )));
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ApiState, RuntimeStatePersistence};
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use magi_core::UtcMillis;
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use std::{fs, sync::Arc};
    use tower::ServiceExt;

    fn test_state() -> ApiState {
        ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
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

    #[tokio::test]
    async fn mark_all_notifications_read_persists_unknown_workspace_session_without_500() {
        let persistence_root = unique_temp_dir("magi-api-notification-orphan-workspace");
        let session_id = SessionId::new("session-notification-orphan-workspace");
        let state = test_state().with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
            persistence_root.join("sessions.json"),
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
        state.session_store.append_notification(
            session_id.clone(),
            "notification-orphan-workspace",
            "incident",
            "未知工作区通知",
        );

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/session/notifications/mark-all-read")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "sessionId": session_id.as_str() }).to_string(),
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
        assert_eq!(body["sessionId"], serde_json::json!(session_id.as_str()));
        assert_eq!(body["notifications"]["records"][0]["handled"], true);

        let persisted = fs::read_to_string(persistence_root.join("sessions.json"))
            .expect("orphan workspace session should persist globally");
        assert!(persisted.contains("session-notification-orphan-workspace"));
        assert!(persisted.contains("workspace-missing"));

        let _ = fs::remove_dir_all(persistence_root);
    }
}
