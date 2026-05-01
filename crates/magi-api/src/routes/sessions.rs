use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_bridge_client::{
    ChatToolChoice, ChatToolDefinition, ChatToolFunctionDefinition, ModelInvocationRequest,
    SHADOW_MODEL_PROVIDER,
};
use magi_core::TaskStatus;
use magi_core::{DomainError, EventId, SessionId, UtcMillis, WorkerId, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_session_store::{
    ActiveExecutionTurn, ActiveExecutionTurnItem, NotificationRecord, SessionRecord,
    TimelineEntryKind,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::session_scope::{
    require_current_session_record_in_workspace, require_session_record_in_workspace,
    session_workspace_id,
};
use crate::{
    dto::{
        BootstrapDto, SessionNotificationsResponseDto, SessionTurnRequestDto,
        SessionTurnResponseDto, SessionTurnRouteDto,
    },
    errors::ApiError,
    execution_chain_recovery::{
        SessionContinueAccepted, active_execution_branch_is_continue_recoverable,
        continue_shadow_execution_chain,
    },
    session_turn_writeback::publish_current_session_turn_item_event,
    state::ApiState,
    task_execution::{SessionTurnExecutionRequest, drive_shadow_task_graph},
};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/session/new", post(create_session))
        .route("/session/turn", post(submit_session_turn))
        .route("/session/interrupt", post(interrupt_session_turn))
        .route("/session/continue", post(continue_session))
        .route("/session/switch", post(switch_session))
        .route("/session/delete", post(delete_session))
        .route("/session/rename", post(rename_session))
        .route("/session/close", post(close_session))
        .route("/session/save", post(save_session))
        .route("/session/notifications", get(get_notifications))
        .route(
            "/session/notifications/append",
            post(append_session_notification),
        )
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
    #[serde(default, alias = "workspace_id")]
    workspace_id: Option<String>,
}

impl DeleteSessionRequest {
    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }
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
    let decision = decide_session_turn_with_task_planner(&state, &request)?;
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
            super::finalize_session_task_dispatch(state.clone(), accepted.clone());
            let execution_chain_ref = state
                .session_store
                .runtime_sidecar(&accepted.session_id)
                .and_then(|sidecar| sidecar.ownership.execution_chain_ref);
            let user_message_item_id = request
                .user_message_id()
                .unwrap_or_else(|| format!("turn-item-user-{}", accepted.accepted_at.0));
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
                Some(user_message_item_id),
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
            let (entry_id, user_message_item_id) = match prompt_text.as_deref() {
                Some(prompt_text) => {
                    let entry_id = format!("timeline-{}-{}", session_id, accepted_at.0);
                    let (user_message_item_id, user_message_item) = build_user_message_turn_item(
                        accepted_at,
                        prompt_text,
                        &entry_id,
                        request.request_id(),
                        request.user_message_id(),
                        request.placeholder_message_id(),
                        Some(accepted.action_task_id.clone()),
                        true,
                    );
                    let updated = state
                        .session_store
                        .append_current_turn_item_with_timeline_entry(
                            &session_id,
                            entry_id.clone(),
                            TimelineEntryKind::UserMessage,
                            prompt_text,
                            accepted_at,
                            user_message_item,
                        )
                        .map_err(|error| {
                            ApiError::internal_assembly("写入 continue 用户消息失败", error)
                        })?;
                    if updated.is_none() {
                        return Err(ApiError::internal_assembly(
                            "写入 continue 用户消息失败",
                            "current_turn 不存在",
                        ));
                    }
                    publish_session_user_message_created_event(
                        &state,
                        &session_id,
                        session_workspace_for_event(&state, &session_id),
                        accepted_at,
                        prompt_text,
                    );
                    (entry_id, Some(user_message_item_id))
                }
                None => (format!("timeline-{}-{}", session_id, accepted_at.0), None),
            };
            finalize_continue_session(state.clone(), accepted.clone(), accepted_at);
            state.persist_runtime_durable_state()?;
            let event_id = publish_session_turn_continue_event(&state, &accepted, accepted_at)?;
            Ok(Json(SessionTurnResponseDto::new(
                accepted.session_id,
                entry_id,
                event_id,
                accepted_at,
                false,
                SessionTurnRouteDto::Continue,
                Some(accepted.root_task_id),
                Some(accepted.action_task_id),
                Some(accepted.execution_chain_ref),
                user_message_item_id,
            )))
        }
    }
}

#[derive(Debug)]
struct SessionTurnIntentDecision {
    route: SessionTurnRouteDto,
    task_title: Option<String>,
    execution_goal: Option<String>,
    required_workers: Vec<String>,
    tool_intent: Option<String>,
    confidence: f64,
    reason_code: Option<String>,
    route_reason: Option<String>,
    task_evidence: Vec<String>,
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

fn decide_session_turn_with_task_planner(
    state: &ApiState,
    request: &SessionTurnRequestDto,
) -> Result<SessionTurnIntentDecision, ApiError> {
    let client = state.task_planning_model_client().cloned().ok_or_else(|| {
        ApiError::InvalidInput("Session Turn 分类器未配置任务规划模型客户端".to_string())
    })?;
    let has_recoverable_chain = request
        .requested_session_id()
        .or_else(|| {
            state
                .session_store
                .current_session()
                .map(|session| session.session_id)
        })
        .map(|session_id| session_has_recoverable_chain(state, &session_id))
        .unwrap_or(false);
    let prompt = build_session_turn_classifier_prompt(request, has_recoverable_chain);
    let response = client
        .invoke(ModelInvocationRequest {
            provider: SHADOW_MODEL_PROVIDER.to_string(),
            prompt,
            messages: None,
            tools: Some(vec![session_turn_classifier_tool()]),
            tool_choice: Some(ChatToolChoice::force_function("classify_session_turn")),
        })
        .map_err(|error| ApiError::model_invocation_failed("Session Turn 分类失败", error))?;
    if !response.ok {
        return Err(ApiError::ModelInvocationFailed(
            "Session Turn 分类器返回失败状态".to_string(),
        ));
    }
    let decision = normalize_session_turn_decision(parse_session_turn_decision(&response.payload)?);
    if matches!(decision.route, SessionTurnRouteDto::Continue) && !has_recoverable_chain {
        return Err(ApiError::InvalidInput(
            "当前会话没有可继续的执行链".to_string(),
        ));
    }
    Ok(decision)
}

fn session_has_recoverable_chain(state: &ApiState, session_id: &SessionId) -> bool {
    let Some(chain) = state.session_store.active_execution_chain(session_id) else {
        return false;
    };
    chain
        .branches
        .iter()
        .any(|branch| active_execution_branch_is_continue_recoverable(state, &chain, branch))
}

fn build_session_turn_classifier_prompt(
    request: &SessionTurnRequestDto,
    has_recoverable_chain: bool,
) -> String {
    let user_text = request.trimmed_text().unwrap_or_default();
    let skill_name = request
        .skill_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("");
    format!(
        "Session Turn 编排分类器\n\
         请只调用 classify_session_turn 工具，输出本轮 route。\n\
         route 只能是 chat、execute、task、continue。\n\
         普通问答使用 chat；需要工具但不创建任务图使用 execute；需要产品级任务编排使用 task；用户要求继续且存在可恢复链时使用 continue。\n\
         如果选择 task，必须给出 confidence、reasonCode、routeReason 和至少 1 条 taskEvidence；只有明确需要多步骤结构化执行、实现/修复/重构、深度任务或多 worker 协作时才选择 task。\n\
         普通问答、状态追问、简单解释和寒暄必须选择 chat，不能只因为措辞模糊就创建任务图。\n\
         userText={user_text}\n\
         deepTask={}\n\
         skillName=\"{skill_name}\"\n\
         imageCount={}\n\
         hasRecoverableChain={has_recoverable_chain}",
        request.deep_task,
        request.images.len()
    )
}

fn session_turn_classifier_tool() -> ChatToolDefinition {
    ChatToolDefinition {
        kind: "function".to_string(),
        function: ChatToolFunctionDefinition {
            name: "classify_session_turn".to_string(),
            description: "判断当前 Session Turn 应进入普通对话、工具执行、任务编排或继续会话。"
                .to_string(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["route", "confidence", "reasonCode", "routeReason", "taskEvidence"],
                "properties": {
                    "route": {
                        "type": "string",
                        "enum": ["chat", "execute", "task", "continue"]
                    },
                    "confidence": {
                        "type": "number",
                        "minimum": 0,
                        "maximum": 1
                    },
                    "reasonCode": {
                        "type": ["string", "null"],
                        "enum": [
                            "plain_chat",
                            "tool_request",
                            "continue_requested",
                            "explicit_task_request",
                            "deep_task_requested",
                            "multi_step_task",
                            "implementation_or_fix",
                            "requires_structured_execution",
                            "image_task",
                            "skill_task",
                            null
                        ]
                    },
                    "routeReason": { "type": ["string", "null"] },
                    "taskEvidence": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "taskTitle": { "type": ["string", "null"] },
                    "executionGoal": { "type": ["string", "null"] },
                    "requiredWorkers": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "toolIntent": { "type": ["string", "null"] }
                }
            }),
        },
    }
}

fn parse_session_turn_decision(payload: &str) -> Result<SessionTurnIntentDecision, ApiError> {
    let normalized_payload = payload
        .trim()
        .strip_prefix("shadow-model::")
        .unwrap_or_else(|| payload.trim())
        .trim();
    let parsed =
        serde_json::from_str::<serde_json::Value>(normalized_payload).map_err(|error| {
            ApiError::InvalidInput(format!("Session Turn 分类器输出不是有效 JSON: {error}"))
        })?;
    let calls = parsed
        .get("tool_calls")
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            ApiError::InvalidInput(
                "Session Turn 分类器未调用 classify_session_turn 工具".to_string(),
            )
        })?;
    for call in calls {
        if let Some(arguments) = classifier_arguments_from_tool_call(call) {
            return session_turn_decision_from_value(arguments?);
        }
    }
    Err(ApiError::InvalidInput(
        "Session Turn 分类器未调用 classify_session_turn 工具".to_string(),
    ))
}

fn classifier_arguments_from_tool_call(
    call: &serde_json::Value,
) -> Option<Result<serde_json::Value, ApiError>> {
    let function = call.get("function")?;
    if function.get("name").and_then(|value| value.as_str())? != "classify_session_turn" {
        return None;
    }
    let Some(arguments) = function.get("arguments").and_then(|value| value.as_str()) else {
        return Some(Err(ApiError::InvalidInput(
            "Session Turn 分类器工具参数缺失".to_string(),
        )));
    };
    Some(serde_json::from_str(arguments).map_err(|error| {
        ApiError::InvalidInput(format!("Session Turn 分类器工具参数不是有效 JSON: {error}"))
    }))
}

fn session_turn_decision_from_value(
    value: serde_json::Value,
) -> Result<SessionTurnIntentDecision, ApiError> {
    let route = match value.get("route").and_then(|value| value.as_str()) {
        Some("chat") => SessionTurnRouteDto::Chat,
        Some("execute") => SessionTurnRouteDto::Execute,
        Some("task") => SessionTurnRouteDto::Task,
        Some("continue") => SessionTurnRouteDto::Continue,
        Some(other) => {
            return Err(ApiError::InvalidInput(format!(
                "Session Turn 分类器返回未知 route: {other}"
            )));
        }
        None => {
            return Err(ApiError::InvalidInput(
                "Session Turn 分类器缺少 route".to_string(),
            ));
        }
    };
    Ok(SessionTurnIntentDecision {
        route,
        task_title: optional_trimmed_string(&value, "taskTitle"),
        execution_goal: optional_trimmed_string(&value, "executionGoal"),
        required_workers: value
            .get("requiredWorkers")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        tool_intent: optional_trimmed_string(&value, "toolIntent"),
        confidence: value
            .get("confidence")
            .and_then(|value| value.as_f64())
            .filter(|value| value.is_finite())
            .unwrap_or(0.0)
            .clamp(0.0, 1.0),
        reason_code: optional_trimmed_string(&value, "reasonCode"),
        route_reason: optional_trimmed_string(&value, "routeReason"),
        task_evidence: value
            .get("taskEvidence")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn normalize_session_turn_decision(
    mut decision: SessionTurnIntentDecision,
) -> SessionTurnIntentDecision {
    if matches!(decision.route, SessionTurnRouteDto::Task)
        && !session_turn_task_route_has_creation_evidence(&decision)
    {
        decision.route = SessionTurnRouteDto::Chat;
        decision.task_title = None;
        decision.execution_goal = None;
        decision.required_workers.clear();
        decision.tool_intent = None;
    }
    decision
}

fn session_turn_task_route_has_creation_evidence(decision: &SessionTurnIntentDecision) -> bool {
    const MIN_TASK_CONFIDENCE: f64 = 0.72;
    if decision.confidence < MIN_TASK_CONFIDENCE {
        return false;
    }
    let Some(reason_code) = decision.reason_code.as_deref() else {
        return false;
    };
    if !matches!(
        reason_code,
        "explicit_task_request"
            | "deep_task_requested"
            | "multi_step_task"
            | "implementation_or_fix"
            | "requires_structured_execution"
            | "image_task"
            | "skill_task"
    ) {
        return false;
    }
    if decision.task_evidence.is_empty() {
        return false;
    }
    if decision.route_reason.is_none() {
        return false;
    }
    decision.task_title.is_some() || decision.execution_goal.is_some()
}

fn optional_trimmed_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn build_user_message_turn_item(
    accepted_at: UtcMillis,
    message: &str,
    entry_id: &str,
    request_id: Option<String>,
    user_message_id: Option<String>,
    placeholder_message_id: Option<String>,
    task_id: Option<magi_core::TaskId>,
    thread_visible: bool,
) -> (String, ActiveExecutionTurnItem) {
    let user_message_item_id = user_message_id
        .clone()
        .unwrap_or_else(|| format!("turn-item-user-{}", accepted_at.0));
    (
        user_message_item_id.clone(),
        ActiveExecutionTurnItem {
            item_id: user_message_item_id,
            item_seq: 1,
            lane_id: None,
            lane_seq: None,
            kind: "user_message".to_string(),
            status: "completed".to_string(),
            source: "user".to_string(),
            title: None,
            content: Some(message.to_string()),
            task_id,
            worker_id: None,
            role_id: None,
            tool_call_id: None,
            tool_name: None,
            tool_status: None,
            tool_arguments: None,
            tool_result: None,
            tool_error: None,
            request_id,
            user_message_id,
            placeholder_message_id,
            timeline_entry_id: Some(entry_id.to_string()),
            thread_visible,
            worker_visible: false,
        },
    )
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
    let request_id = request.request_id();
    let user_message_id = request.user_message_id();
    let placeholder_message_id = request.placeholder_message_id();
    // 使用前端传入的 userMessageId 作为 canonical item_id，确保前端乐观节点与后端流式更新使用同一 ID
    let (user_message_item_id, user_message_item) = build_user_message_turn_item(
        accepted_at,
        &message,
        &entry_id,
        request_id.clone(),
        user_message_id.clone(),
        placeholder_message_id.clone(),
        None,
        true,
    );
    let turn_id = format!("turn-session-{}", accepted_at.0);
    let mut turn = ActiveExecutionTurn {
        turn_id: turn_id.clone(),
        turn_seq: accepted_at.0 as u64,
        accepted_at,
        status: "running".to_string(),
        completed_at: None,
        user_message: Some(message.clone()),
        items: vec![user_message_item],
        worker_lanes: Vec::new(),
    };
    turn.normalize();
    let (entry_id, _) = state
        .session_store
        .accept_current_turn_with_timeline_entry(
            session_id.clone(),
            entry_id,
            TimelineEntryKind::UserMessage,
            message.clone(),
            accepted_at,
            turn,
        )
        .map_err(|error| map_current_turn_accept_error("接受 session turn 失败", error))?;
    publish_session_user_message_created_event(
        &state,
        &session_id,
        workspace_id.clone(),
        accepted_at,
        &message,
    );
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
            turn_id,
            workspace_id: workspace_id.clone(),
            prompt,
            use_tools: matches!(decision.route, SessionTurnRouteDto::Execute),
            skill_name: request.skill_name.clone(),
            request_id: request_id.clone(),
            user_message_id: user_message_id.clone(),
            placeholder_message_id: placeholder_message_id.clone(),
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
        Some(user_message_item_id),
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
                if let Err(error) = state.persist_session_durable_state() {
                    tracing::error!(
                        session_id = %session_id,
                        ?error,
                        "regular session turn background persist failed"
                    );
                }
                if output.interrupted {
                    return;
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
                            "error": error.message().to_string(),
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
    accepted: &SessionContinueAccepted,
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

impl SwitchSessionRequest {
    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveSessionRequest {
    session_id: Option<String>,
    #[serde(default, alias = "workspace_id")]
    workspace_id: Option<String>,
}

impl SaveSessionRequest {
    fn requested_session_id(&self) -> Option<SessionId> {
        parse_requested_session_id(self.session_id.as_deref())
    }

    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContinueSessionRequest {
    session_id: String,
    prompt_text: Option<String>,
    #[serde(default)]
    requested_worker_ids: Vec<String>,
    #[serde(alias = "request_id")]
    request_id: Option<String>,
    #[serde(alias = "user_message_id")]
    user_message_id: Option<String>,
    #[serde(alias = "placeholder_message_id")]
    placeholder_message_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InterruptSessionTurnRequest {
    session_id: Option<String>,
    #[serde(default, alias = "workspace_id")]
    workspace_id: Option<String>,
}

impl InterruptSessionTurnRequest {
    fn requested_session_id(&self) -> Option<SessionId> {
        parse_requested_session_id(self.session_id.as_deref())
    }

    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }
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
struct SessionInterruptResponseDto {
    interrupted: bool,
    session_id: String,
    turn_id: Option<String>,
    event_id: String,
    requested_at: UtcMillis,
    removed_timeline_entry_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionSelectionResponseDto {
    session_id: String,
    current_session: Option<SessionRecord>,
}

fn resolve_interrupt_session_record(
    state: &ApiState,
    request: &InterruptSessionTurnRequest,
) -> Result<SessionRecord, ApiError> {
    if let Some(session_id) = request.requested_session_id() {
        return require_session_record_in_workspace(
            state,
            &session_id,
            request.requested_workspace_id(),
        );
    }
    require_current_session_record_in_workspace(state, request.requested_workspace_id())
}

fn turn_status_is_interruptible(status: &str) -> bool {
    !matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "completed"
            | "complete"
            | "succeeded"
            | "success"
            | "failed"
            | "error"
            | "cancelled"
            | "canceled"
    )
}

fn map_current_turn_accept_error(context: &str, error: DomainError) -> ApiError {
    match error {
        DomainError::InvalidState { message } if message.contains("active current_turn") => {
            ApiError::conflict(context, &message)
        }
        other => ApiError::internal_assembly(context, other),
    }
}

fn session_workspace_for_event(state: &ApiState, session_id: &SessionId) -> Option<WorkspaceId> {
    state
        .session_store
        .session(session_id)
        .and_then(|session| session_workspace_id(state, &session))
}

fn publish_session_user_message_created_event(
    state: &ApiState,
    session_id: &SessionId,
    workspace_id: Option<WorkspaceId>,
    occurred_at: UtcMillis,
    message: &str,
) {
    let _ = state.event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-message-user-{}", occurred_at.0)),
            "message.created",
            json!({
                "session_id": session_id.to_string(),
                "role": "user",
                "content": message,
            }),
        )
        .with_context(EventContext {
            session_id: Some(session_id.clone()),
            workspace_id,
            ..EventContext::default()
        }),
    );
}

fn current_turn_streaming_timeline_entry_ids(
    state: &ApiState,
    session_id: &SessionId,
) -> Vec<String> {
    state
        .session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .map(|turn| {
            turn.items
                .into_iter()
                .filter(|item| item.kind == "assistant_stream")
                .filter_map(|item| {
                    item.timeline_entry_id
                        .filter(|entry_id| !entry_id.trim().is_empty())
                        .or_else(|| {
                            Some(item.item_id).filter(|entry_id| !entry_id.trim().is_empty())
                        })
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn interrupt_session_turn(
    State(state): State<ApiState>,
    Json(request): Json<InterruptSessionTurnRequest>,
) -> Result<Json<SessionInterruptResponseDto>, ApiError> {
    let session = resolve_interrupt_session_record(&state, &request)?;
    let session_id = session.session_id.clone();
    let now = UtcMillis::now();
    let workspace_id = session_workspace_id(&state, &session);
    let current_turn = state
        .session_store
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| sidecar.current_turn);
    let turn_id = current_turn.as_ref().map(|turn| turn.turn_id.clone());
    let interrupted = current_turn
        .as_ref()
        .is_some_and(|turn| turn_status_is_interruptible(&turn.status));
    let streaming_entry_ids = if interrupted {
        current_turn_streaming_timeline_entry_ids(&state, &session_id)
    } else {
        Vec::new()
    };

    if interrupted {
        let cancelled_item_id = state
            .session_store
            .cancel_current_turn(&session_id)
            .map_err(|error| ApiError::internal_assembly("中断 session turn 失败", error))?
            .and_then(|sidecar| sidecar.current_turn)
            .and_then(|turn| turn.items.last().map(|item| item.item_id.clone()));
        for entry_id in &streaming_entry_ids {
            state
                .session_store
                .remove_timeline_entry(&session_id, entry_id);
        }
        if let Some(item_id) = cancelled_item_id.as_deref() {
            // 聊天 UI 只接受 canonical turn 事实，interrupt 事件只做运行态通知。
            publish_current_session_turn_item_event(
                &state.event_bus,
                &state.session_store,
                &session_id,
                &workspace_id,
                item_id,
                None,
            );
        }
    }

    state.persist_session_durable_state()?;
    let event_id = EventId::new(format!("event-session-turn-interrupt-{}", now.0));
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.turn.interrupted",
        json!({
            "session_id": session_id.to_string(),
            "workspace_id": workspace_id.as_ref().map(ToString::to_string),
            "turn_id": turn_id.clone(),
            "interrupted": interrupted,
            "requested_at": now.0,
            "removed_timeline_entry_ids": streaming_entry_ids.clone(),
        }),
    )
    .with_context(EventContext {
        session_id: Some(session_id.clone()),
        workspace_id: workspace_id.clone(),
        ..EventContext::default()
    });
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("session turn 中断事件发布失败", err))?;

    Ok(Json(SessionInterruptResponseDto {
        interrupted,
        session_id: session_id.to_string(),
        turn_id,
        event_id: event_id.to_string(),
        requested_at: now,
        removed_timeline_entry_ids: streaming_entry_ids,
    }))
}

async fn switch_session(
    State(state): State<ApiState>,
    Json(request): Json<SwitchSessionRequest>,
) -> Result<Json<SessionSelectionResponseDto>, ApiError> {
    let session_id = SessionId::new(&request.session_id);
    let workspace_id = request
        .requested_workspace_id()
        .ok_or_else(|| ApiError::InvalidInput("workspaceId 不能为空".to_string()))?;
    require_session_record_in_workspace(&state, &session_id, Some(workspace_id))?;
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
    let request_id = trimmed_non_empty(request.request_id.as_deref()).map(str::to_string);
    let user_message_id = trimmed_non_empty(request.user_message_id.as_deref()).map(str::to_string);
    let placeholder_message_id =
        trimmed_non_empty(request.placeholder_message_id.as_deref()).map(str::to_string);
    let requested_worker_ids = request
        .requested_worker_ids
        .into_iter()
        .map(|worker_id| worker_id.trim().to_string())
        .filter(|worker_id| !worker_id.is_empty())
        .map(WorkerId::new)
        .collect::<Vec<_>>();
    let continued_at = UtcMillis::now();
    let accepted = continue_shadow_execution_chain(&state, &session_id, &requested_worker_ids)?;
    if let Some(prompt_text) = prompt_text.as_deref() {
        let entry_id = format!("timeline-{}-{}", session_id, continued_at.0);
        let (_, user_message_item) = build_user_message_turn_item(
            continued_at,
            prompt_text,
            &entry_id,
            request_id,
            user_message_id,
            placeholder_message_id,
            Some(accepted.action_task_id.clone()),
            true,
        );
        let updated = state
            .session_store
            .append_current_turn_item_with_timeline_entry(
                &session_id,
                entry_id,
                TimelineEntryKind::UserMessage,
                prompt_text,
                continued_at,
                user_message_item,
            )
            .map_err(|error| ApiError::internal_assembly("写入 continue 用户消息失败", error))?;
        if updated.is_none() {
            return Err(ApiError::internal_assembly(
                "写入 continue 用户消息失败",
                "current_turn 不存在",
            ));
        }
        publish_session_user_message_created_event(
            &state,
            &session_id,
            session_workspace_for_event(&state, &session_id),
            continued_at,
            prompt_text,
        );
    }
    finalize_continue_session(state.clone(), accepted.clone(), continued_at);
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

fn finalize_continue_session(
    state: ApiState,
    accepted: SessionContinueAccepted,
    continued_at: UtcMillis,
) {
    let Some(task_store) = state.task_store() else {
        return;
    };

    let background_allowed = task_store
        .get_task(&accepted.root_task_id)
        .and_then(|root| root.policy_snapshot)
        .map(|policy| policy.background_allowed)
        .unwrap_or(false);

    // 普通模式：同步 drive shadow task graph（后台 runner 未启动）
    // 深度模式：后台 runner 已在 continue_shadow_execution_chain 中重新启动，
    // 此处不再调用同步 drive，避免与后台 runner 竞争
    if !background_allowed {
        if let Err(error) = drive_shadow_task_graph(
            &state,
            &accepted.root_task_id,
            &accepted.action_task_id,
            "继续会话执行失败",
        ) {
            let interrupted = task_store
                .get_task(&accepted.action_task_id)
                .is_some_and(|task| task.status == TaskStatus::Blocked);
            if !interrupted {
                tracing::error!(
                    session_id = %accepted.session_id,
                    root_task_id = %accepted.root_task_id,
                    action_task_id = %accepted.action_task_id,
                    ?error,
                    "session continue graph drive failed"
                );
                return;
            }
        }
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
}

async fn delete_session(
    State(state): State<ApiState>,
    Json(request): Json<DeleteSessionRequest>,
) -> Result<Json<BootstrapDto>, ApiError> {
    let session_id = SessionId::new(&request.session_id);
    let session =
        require_session_record_in_workspace(&state, &session_id, request.requested_workspace_id())?;
    let response_workspace_id = request
        .requested_workspace_id()
        .map(str::to_string)
        .or_else(|| {
            state
                .session_workspace_id(&session)
                .map(|workspace_id| workspace_id.to_string())
        });
    state
        .session_store
        .delete_session(&session_id)
        .map_err(|e| ApiError::internal_assembly("删除会话失败", e))?;
    state.persist_session_durable_state()?;
    Ok(Json(state.bootstrap_dto_for_workspace_session(
        response_workspace_id.as_deref(),
        None,
    )))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameSessionRequest {
    session_id: String,
    name: String,
    #[serde(default, alias = "workspace_id")]
    workspace_id: Option<String>,
}

impl RenameSessionRequest {
    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }
}

async fn rename_session(
    State(state): State<ApiState>,
    Json(request): Json<RenameSessionRequest>,
) -> Result<Json<BootstrapDto>, ApiError> {
    let session_id = SessionId::new(&request.session_id);
    let session =
        require_session_record_in_workspace(&state, &session_id, request.requested_workspace_id())?;
    let response_workspace_id = request
        .requested_workspace_id()
        .map(str::to_string)
        .or_else(|| {
            state
                .session_workspace_id(&session)
                .map(|workspace_id| workspace_id.to_string())
        });
    state
        .session_store
        .rename_session(&session_id, &request.name)
        .map_err(|e| ApiError::internal_assembly("重命名会话失败", e))?;
    state.persist_session_durable_state()?;
    Ok(Json(state.bootstrap_dto_for_workspace_session(
        response_workspace_id.as_deref(),
        Some(&session_id),
    )))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloseSessionRequest {
    session_id: String,
    #[serde(default, alias = "workspace_id")]
    workspace_id: Option<String>,
}

impl CloseSessionRequest {
    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }
}

async fn close_session(
    State(state): State<ApiState>,
    Json(request): Json<CloseSessionRequest>,
) -> Result<Json<BootstrapDto>, ApiError> {
    let session_id = SessionId::new(&request.session_id);
    let session =
        require_session_record_in_workspace(&state, &session_id, request.requested_workspace_id())?;
    let response_workspace_id = request
        .requested_workspace_id()
        .map(str::to_string)
        .or_else(|| {
            state
                .session_workspace_id(&session)
                .map(|workspace_id| workspace_id.to_string())
        });
    state
        .session_store
        .archive_session(&session_id)
        .map_err(|e| ApiError::internal_assembly("关闭会话失败", e))?;
    if let Some(manager) = state.runner_manager() {
        manager.unbind_session(&session_id);
    }
    state.persist_session_durable_state()?;
    Ok(Json(state.bootstrap_dto_for_workspace_session(
        response_workspace_id.as_deref(),
        None,
    )))
}

async fn save_session(
    State(state): State<ApiState>,
    Json(request): Json<SaveSessionRequest>,
) -> Result<Json<BootstrapDto>, ApiError> {
    let workspace_id = request
        .requested_workspace_id()
        .ok_or_else(|| ApiError::InvalidInput("workspaceId 不能为空".to_string()))?;
    let selected_session_id = if let Some(session_id) = request.requested_session_id() {
        require_session_record_in_workspace(&state, &session_id, Some(workspace_id))?;
        Some(session_id)
    } else {
        None
    };
    state.persist_session_durable_state()?;
    Ok(Json(state.bootstrap_dto_for_workspace_session(
        Some(workspace_id),
        selected_session_id.as_ref(),
    )))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificationsQuery {
    session_id: Option<String>,
    #[serde(default, alias = "workspace_id")]
    workspace_id: Option<String>,
}

impl NotificationsQuery {
    fn requested_session_id(&self) -> Option<SessionId> {
        parse_requested_session_id(self.session_id.as_deref())
    }

    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }
}

async fn get_notifications(
    State(state): State<ApiState>,
    Query(query): Query<NotificationsQuery>,
) -> Result<Json<SessionNotificationsResponseDto>, ApiError> {
    let session_id = resolve_notifications_session_id(
        &state,
        query.requested_session_id(),
        query.requested_workspace_id(),
    )?;
    Ok(Json(build_notifications_response(
        &state,
        session_id.as_ref(),
        query.requested_workspace_id(),
    )))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificationScopeRequest {
    session_id: Option<String>,
    #[serde(default, alias = "workspace_id")]
    workspace_id: Option<String>,
}

impl NotificationScopeRequest {
    fn requested_session_id(&self) -> Option<SessionId> {
        parse_requested_session_id(self.session_id.as_deref())
    }

    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppendNotificationRequest {
    session_id: Option<String>,
    #[serde(default, alias = "workspace_id")]
    workspace_id: Option<String>,
    notification_id: Option<String>,
    kind: Option<String>,
    level: Option<String>,
    title: Option<String>,
    message: String,
    source: Option<String>,
    persist_to_center: Option<bool>,
    action_required: Option<bool>,
    count_unread: Option<bool>,
    display_mode: Option<String>,
    duration: Option<u64>,
}

impl AppendNotificationRequest {
    fn requested_session_id(&self) -> Option<SessionId> {
        parse_requested_session_id(self.session_id.as_deref())
    }

    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }

    fn requested_notification_id(&self) -> Option<String> {
        trimmed_non_empty(self.notification_id.as_deref()).map(str::to_string)
    }

    fn normalized_kind(&self) -> String {
        match trimmed_non_empty(self.kind.as_deref()) {
            Some("incident") => "incident".to_string(),
            Some("audit") | Some("center") | Some("toast") => "audit".to_string(),
            _ => "audit".to_string(),
        }
    }

    fn normalized_display_mode(&self) -> Option<String> {
        match trimmed_non_empty(self.display_mode.as_deref()) {
            Some("toast") => Some("toast".to_string()),
            Some("notification_center") => Some("notification_center".to_string()),
            Some("silent") => Some("silent".to_string()),
            _ => None,
        }
    }
}

async fn append_session_notification(
    State(state): State<ApiState>,
    Json(request): Json<AppendNotificationRequest>,
) -> Result<Json<SessionNotificationsResponseDto>, ApiError> {
    let session_id = require_notifications_session_id(
        &state,
        request.requested_session_id(),
        request.requested_workspace_id(),
    )?;
    if request.persist_to_center == Some(false) {
        return Ok(Json(build_notifications_response(
            &state,
            Some(&session_id),
            request.requested_workspace_id(),
        )));
    }
    let message = trimmed_non_empty(Some(request.message.as_str()))
        .ok_or_else(|| ApiError::InvalidInput("通知内容不能为空".to_string()))?
        .to_string();
    let kind = request.normalized_kind();
    let count_unread = request.count_unread.unwrap_or(kind == "incident");
    let notification_id = request
        .requested_notification_id()
        .unwrap_or_else(|| format!("notification-{}", UtcMillis::now().0));
    state
        .session_store
        .append_notification_record(NotificationRecord {
            notification_id,
            session_id: session_id.clone(),
            kind,
            level: trimmed_non_empty(request.level.as_deref()).map(str::to_string),
            title: trimmed_non_empty(request.title.as_deref()).map(str::to_string),
            message,
            source: trimmed_non_empty(request.source.as_deref()).map(str::to_string),
            created_at: UtcMillis::now(),
            handled: !count_unread,
            persist_to_center: Some(true),
            action_required: request.action_required,
            count_unread: Some(count_unread),
            display_mode: request.normalized_display_mode(),
            duration: request.duration,
        });
    state.persist_session_durable_state()?;
    Ok(Json(build_notifications_response(
        &state,
        Some(&session_id),
        request.requested_workspace_id(),
    )))
}

async fn mark_all_notifications_read(
    State(state): State<ApiState>,
    Json(request): Json<NotificationScopeRequest>,
) -> Result<Json<SessionNotificationsResponseDto>, ApiError> {
    let session_id = require_notifications_session_id(
        &state,
        request.requested_session_id(),
        request.requested_workspace_id(),
    )?;
    state
        .session_store
        .mark_notifications_handled_for_session(&session_id);
    state.persist_session_durable_state()?;
    Ok(Json(build_notifications_response(
        &state,
        Some(&session_id),
        request.requested_workspace_id(),
    )))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClearNotificationsRequest {
    session_id: Option<String>,
    #[serde(default, alias = "workspace_id")]
    workspace_id: Option<String>,
}

impl ClearNotificationsRequest {
    fn requested_session_id(&self) -> Option<SessionId> {
        parse_requested_session_id(self.session_id.as_deref())
    }

    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }
}

async fn clear_notifications(
    State(state): State<ApiState>,
    Json(request): Json<ClearNotificationsRequest>,
) -> Result<Json<SessionNotificationsResponseDto>, ApiError> {
    let session_id = require_notifications_session_id(
        &state,
        request.requested_session_id(),
        request.requested_workspace_id(),
    )?;
    state
        .session_store
        .clear_notifications_for_session(&session_id);
    state.persist_session_durable_state()?;
    Ok(Json(build_notifications_response(
        &state,
        Some(&session_id),
        request.requested_workspace_id(),
    )))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveNotificationRequest {
    session_id: Option<String>,
    #[serde(default, alias = "workspace_id")]
    workspace_id: Option<String>,
    notification_id: String,
}

impl RemoveNotificationRequest {
    fn requested_session_id(&self) -> Option<SessionId> {
        parse_requested_session_id(self.session_id.as_deref())
    }

    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }

    fn requested_notification_id(&self) -> Option<String> {
        trimmed_non_empty(Some(self.notification_id.as_str())).map(str::to_string)
    }
}

async fn remove_notification(
    State(state): State<ApiState>,
    Json(request): Json<RemoveNotificationRequest>,
) -> Result<Json<SessionNotificationsResponseDto>, ApiError> {
    let session_id = require_notifications_session_id(
        &state,
        request.requested_session_id(),
        request.requested_workspace_id(),
    )?;
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
        request.requested_workspace_id(),
    )))
}

fn build_notifications_response(
    state: &ApiState,
    session_id: Option<&SessionId>,
    requested_workspace_id: Option<&str>,
) -> SessionNotificationsResponseDto {
    let workspace_id = session_id
        .and_then(|session_id| state.session_store.session(session_id))
        .and_then(|session| session_workspace_id(state, &session))
        .map(|workspace_id| workspace_id.to_string())
        .or_else(|| trimmed_non_empty(requested_workspace_id).map(str::to_string));
    match session_id {
        Some(session_id) => SessionNotificationsResponseDto::from_records(
            session_id,
            workspace_id,
            state.session_store.notifications_for_session(session_id),
        ),
        None => SessionNotificationsResponseDto::empty(None, workspace_id),
    }
}

fn resolve_notifications_session_id(
    state: &ApiState,
    requested_session_id: Option<SessionId>,
    requested_workspace_id: Option<&str>,
) -> Result<Option<SessionId>, ApiError> {
    if let Some(session_id) = requested_session_id {
        return Ok(Some(
            require_session_record_in_workspace(state, &session_id, requested_workspace_id)?
                .session_id,
        ));
    }
    let Some(current_session) = state.session_store.current_session() else {
        return Ok(None);
    };
    if let Some(workspace_id) = requested_workspace_id
        && session_workspace_id(state, &current_session)
            .as_ref()
            .map(|current_workspace_id| current_workspace_id.as_str())
            != Some(workspace_id)
    {
        return Ok(None);
    }
    Ok(Some(current_session.session_id))
}

fn require_notifications_session_id(
    state: &ApiState,
    requested_session_id: Option<SessionId>,
    requested_workspace_id: Option<&str>,
) -> Result<SessionId, ApiError> {
    if requested_session_id.is_none() && requested_workspace_id.is_some() {
        return Ok(
            require_current_session_record_in_workspace(state, requested_workspace_id)?.session_id,
        );
    }
    resolve_notifications_session_id(state, requested_session_id, requested_workspace_id)?
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
    use magi_core::{ExecutionOwnership, UtcMillis, WorkspaceId};
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

    async fn post_json(
        state: ApiState,
        uri: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = routes()
            .with_state(state)
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
    async fn delete_session_rejects_workspace_mismatched_session() {
        let state = test_state();
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
    async fn delete_session_returns_workspace_scoped_bootstrap() {
        let state = test_state();
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
        assert_eq!(body["currentSession"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn rename_session_rejects_workspace_mismatched_session() {
        let state = test_state();
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
    async fn close_session_rejects_workspace_mismatched_session() {
        let state = test_state();
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
    async fn switch_session_requires_workspace_scope() {
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
            "/session/switch",
            serde_json::json!({
                "sessionId": session_id.as_str(),
            }),
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
    async fn save_session_returns_workspace_scoped_bootstrap() {
        let state = test_state();
        let selected_session_id = SessionId::new("session-save-scoped-a");
        let foreign_session_id = SessionId::new("session-save-scoped-b");
        state
            .session_store
            .create_session_for_workspace(
                selected_session_id.clone(),
                "保存 A",
                Some("workspace-a".to_string()),
            )
            .expect("session should create");
        state
            .session_store
            .create_session_for_workspace(
                foreign_session_id,
                "外部 B",
                Some("workspace-b".to_string()),
            )
            .expect("session should create");

        let (status, body) = post_json(
            state,
            "/session/save",
            serde_json::json!({
                "workspaceId": "workspace-a",
                "sessionId": selected_session_id.as_str(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(
            body["currentSession"]["sessionId"]
                .as_str()
                .unwrap_or_default(),
            selected_session_id.as_str()
        );
        let session_ids = body["sessions"]
            .as_array()
            .expect("sessions should be array")
            .iter()
            .map(|session| session["sessionId"].as_str().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(session_ids, vec![selected_session_id.as_str()]);
    }

    #[tokio::test]
    async fn save_session_rejects_workspace_mismatched_session() {
        let state = test_state();
        let session_id = SessionId::new("session-save-workspace-a");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "保存 workspace A",
                Some("workspace-a".to_string()),
            )
            .expect("session should create");

        let (status, body) = post_json(
            state,
            "/session/save",
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
    }

    #[tokio::test]
    async fn notifications_workspace_query_does_not_fall_back_to_foreign_current_session() {
        let state = test_state();
        let foreign_session_id = SessionId::new("session-notification-foreign-current");
        state
            .session_store
            .create_session_for_workspace(
                foreign_session_id.clone(),
                "外部工作区当前会话",
                Some("workspace-a".to_string()),
            )
            .expect("session should create");
        state.session_store.append_notification(
            foreign_session_id,
            "notification-foreign-current",
            "incident",
            "不应串到 workspace-b 的通知",
        );

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/session/notifications?workspaceId=workspace-b")
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
        assert_eq!(body["sessionId"], serde_json::json!(""));
        assert_eq!(
            body["notifications"]["records"]
                .as_array()
                .expect("records should be array")
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn notifications_workspace_query_uses_execution_ownership_for_current_session() {
        let state = test_state();
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
        state.session_store.append_notification(
            session_id.clone(),
            "notification-owned-current",
            "incident",
            "应按 execution ownership 归属加载",
        );

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/session/notifications?workspaceId=workspace-owned-notifications")
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
        let session_id = SessionId::new("session-notification-workspace-a");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "工作区 A 会话",
                Some("workspace-a".to_string()),
            )
            .expect("session should create");
        state.session_store.append_notification(
            session_id.clone(),
            "notification-workspace-a",
            "incident",
            "只能在 workspace-a 中处理",
        );

        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/session/notifications/mark-all-read")
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
        assert_eq!(
            state.session_store.notifications_for_session(&session_id)[0].handled,
            false
        );
    }

    #[tokio::test]
    async fn notifications_actions_accept_execution_owned_unbound_workspace_session() {
        let state = test_state();
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
            state.session_store.append_notification(
                session_id.clone(),
                notification_id,
                "incident",
                "应允许归属 workspace 的通知操作",
            );
        }

        let (status, body) = post_json(
            state.clone(),
            "/session/notifications/mark-all-read",
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
                .notifications_for_session(&session_id)
                .iter()
                .all(|notification| notification.handled)
        );

        let (status, body) = post_json(
            state.clone(),
            "/session/notifications/remove",
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
            "/session/notifications/clear",
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
                .notifications_for_session(&session_id)
                .is_empty()
        );
    }

    #[tokio::test]
    async fn append_session_notification_persists_backend_snapshot() {
        let state = test_state();
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
                    .uri("/session/notifications/append")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "workspaceId": "workspace-a",
                            "sessionId": session_id.as_str(),
                            "notificationId": "notification-append-audit",
                            "kind": "audit",
                            "level": "success",
                            "title": "保存完成",
                            "message": "设置已经保存",
                            "source": "web-action",
                            "persistToCenter": true,
                            "actionRequired": false,
                            "countUnread": false,
                            "displayMode": "toast",
                            "duration": 3000,
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
        assert_eq!(records[0]["notificationId"], "notification-append-audit");
        assert_eq!(records[0]["kind"], "audit");
        assert_eq!(records[0]["level"], "success");
        assert_eq!(records[0]["title"], "保存完成");
        assert_eq!(records[0]["source"], "web-action");
        assert_eq!(records[0]["read"], true);
        assert_eq!(records[0]["handled"], true);
        assert_eq!(records[0]["persistToCenter"], true);
        assert_eq!(records[0]["countUnread"], false);
        assert_eq!(records[0]["displayMode"], "toast");

        let stored = state.session_store.notifications_for_session(&session_id);
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].notification_id, "notification-append-audit");
        assert_eq!(stored[0].handled, true);
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
