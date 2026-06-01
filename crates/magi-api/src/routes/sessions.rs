use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_conversation_runtime::session_turn_execution::SessionTurnExecutionRequest;
use magi_conversation_runtime::session_writeback::publish_current_session_turn_item_event;
use magi_conversation_runtime::{
    MailboxAuthor, MailboxKind, RuntimeSignal, public_builtin_tool_reference_aliases,
    task_execution_registry::TaskExecutionPlan, tool_reference_position,
};
use magi_core::TaskStatus;
use magi_core::{
    DomainError, EventId, MissionId, SessionId, Task, TaskId, TaskTier, UtcMillis, WorkerId,
    WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_session_store::{
    ActiveExecutionTurn, ActiveExecutionTurnItem, CANONICAL_TURN_SCHEMA_VERSION, CanonicalTurn,
    NotificationRecord, SessionRecord, ThreadChatMessage, TimelineEntryKind,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use std::sync::atomic::{AtomicU64, Ordering};

use super::session_scope::{
    parse_session_id, require_registered_workspace_id, require_session_record_in_workspace,
    session_workspace_id,
};
use crate::{
    dto::{
        BootstrapDto, SessionNotificationsResponseDto, SessionTurnRequestDto,
        SessionTurnResponseDto, SessionTurnRouteDto,
    },
    errors::ApiError,
    session_continue::{
        SessionContinueAccepted, active_execution_branch_is_continue_recoverable,
        continue_execution_chain,
    },
    state::ApiState,
    task_dispatch::DispatchSubmissionAccepted,
    task_turn_finalize::finalize_background_session_task_turn_if_root_terminal,
};

pub fn routes() -> Router<ApiState> {
    Router::new()
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

async fn submit_session_turn(
    State(state): State<ApiState>,
    Json(request): Json<SessionTurnRequestDto>,
) -> Result<Json<SessionTurnResponseDto>, ApiError> {
    validate_session_turn_input(&request)?;
    let accepted_at = super::monotonic_accepted_at();
    let requested_workspace_id = request.requested_workspace_id();
    let workspace_id = require_registered_workspace_id(&state, requested_workspace_id.as_deref())?;
    if request.supplement_context {
        return submit_supplement_context_turn(&state, &request, &workspace_id, accepted_at)
            .await
            .map(Json);
    }
    let decision = decide_session_turn_with_task_planner(&state, &request)?;
    match decision.route {
        SessionTurnRouteDto::Chat | SessionTurnRouteDto::Execute => {
            submit_regular_session_turn(state, request, workspace_id, accepted_at, decision)
                .await
                .map(Json)
        }
        SessionTurnRouteDto::Task => {
            let (accepted, event_id) = super::accept_session_task_submission(
                &state,
                &request,
                workspace_id.clone(),
                decision.task_title.clone(),
                decision.execution_goal.clone(),
                decision.task_tier,
            )
            .await?;
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
        SessionTurnRouteDto::SupplementContext => {
            unreachable!("supplement_context route should be handled before classifier")
        }
        SessionTurnRouteDto::Continue => {
            let session_id = request
                .requested_session_id()
                .ok_or_else(|| ApiError::InvalidInput("继续会话需要明确的 session".to_string()))?;
            require_session_record_in_workspace(&state, &session_id, Some(workspace_id.as_str()))?;
            // S1：user 信号经 Conversation Mailbox 入栈，下游一律读 signal.* 不再读 request.*
            let signal = super::ingest_user_input_to_conversation(
                &state,
                &session_id,
                &request,
                accepted_at,
            );
            let prompt_text = signal.text.clone();
            let accepted = continue_execution_chain(&state, &session_id, &[])?;
            let (_, orchestrator_thread_id) =
                state
                    .session_store
                    .ensure_session_mission(&session_id, accepted_at, || {
                        accepted.mission_id.clone()
                    });
            let (entry_id, user_message_item_id) = write_continue_user_message(
                &state,
                &accepted,
                prompt_text.as_deref(),
                accepted_at,
                signal.request_id,
                signal.user_message_id,
                signal.placeholder_message_id,
                orchestrator_thread_id,
            )?;
            state
                .ensure_snapshot_session_for_workspace_id(&session_id, &Some(workspace_id))
                .await?;
            finalize_continue_session(state.clone(), accepted.clone(), accepted_at);
            state.persist_runtime_durable_state_for_api()?;
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
    task_tier: TaskTier,
    tool_intent: Option<String>,
    forced_tool_name: Option<String>,
    required_tool_chain: Vec<String>,
    confidence: f64,
    reason_code: Option<String>,
    route_reason: Option<String>,
    task_evidence: Vec<String>,
}

static SUPPLEMENT_SIGNAL_COUNTER: AtomicU64 = AtomicU64::new(1);

async fn submit_supplement_context_turn(
    state: &ApiState,
    request: &SessionTurnRequestDto,
    workspace_id: &WorkspaceId,
    accepted_at: UtcMillis,
) -> Result<SessionTurnResponseDto, ApiError> {
    let session_id = parse_session_id(request.session_id.as_deref())?;
    require_session_record_in_workspace(state, &session_id, Some(workspace_id.as_str()))?;
    state
        .ensure_snapshot_session_for_workspace_id(&session_id, &Some(workspace_id.clone()))
        .await?;
    // S1：user 信号经 Conversation Mailbox 入栈，下游一律读 signal.* 不再读 request.*
    let signal = super::ingest_user_input_to_conversation(state, &session_id, request, accepted_at);
    let message = signal
        .text
        .clone()
        .ok_or_else(|| ApiError::InvalidInput("运行时 followup 消息不能为空".to_string()))?;
    let store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("supplement context", "task_store 未配置"))?;

    let ownership = state
        .session_store
        .execution_ownership(&session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    let mission_id = ownership
        .mission_id
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有活跃任务".to_string()))?;

    let root_task = store
        .get_tasks_by_mission(&mission_id)
        .into_iter()
        .find(|task| task.parent_task_id.is_none())
        .ok_or_else(|| ApiError::InvalidInput("当前 Mission 没有根任务".to_string()))?;
    let target_task = resolve_supplement_target_task(
        store,
        &mission_id,
        &root_task,
        request.target_task_id.as_deref(),
    )?;
    let target_task_id = target_task.task_id.clone();

    let mailbox_signal_ref = format!(
        "mailbox-signal-{}-{}",
        UtcMillis::now().0,
        SUPPLEMENT_SIGNAL_COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    let thread_id = match state.task_execution_registry().get(&target_task_id) {
        Some(TaskExecutionPlan::Dispatch { thread_id, .. }) => Some(thread_id),
        None if target_task_id == root_task.task_id => state
            .session_store
            .orchestrator_thread_for_session(&session_id)
            .map(|thread| thread.thread_id),
        None => None,
    }
    .ok_or_else(|| {
        ApiError::InvalidInput(format!(
            "任务 {} 尚未注册执行 thread，无法投递运行时输入",
            target_task_id
        ))
    })?;

    let signal_payload = json!({
        "signal_ref": mailbox_signal_ref,
        "text": message,
        "target_task_id": target_task_id.to_string(),
    });
    state
        .conversation_registry
        .conversation_for_task(&session_id, &target_task_id)
        .lock()
        .expect("target task Conversation mutex poisoned")
        .ingest_runtime_signal(RuntimeSignal {
            author: MailboxAuthor::User,
            kind: MailboxKind::Followup,
            trigger_turn: true,
            payload: signal_payload.clone(),
            enqueued_at: accepted_at,
        });
    state.session_store.append_thread_messages(
        &thread_id,
        vec![ThreadChatMessage {
            role: "system".to_string(),
            content: Some(format!(
                "[mailbox]\nauthor=user\nkind=followup\npayload={}",
                signal_payload
            )),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }],
        accepted_at,
    );
    let entry_id = format!("timeline-{}-{}", session_id, accepted_at.0);
    state.persist_session_state_checkpoint("session_supplement_context")?;

    let event_id = EventId::new(format!(
        "event-session-supplement-context-{}-{}",
        session_id, accepted_at.0
    ));

    Ok(SessionTurnResponseDto::new(
        session_id,
        entry_id,
        event_id,
        accepted_at,
        false,
        SessionTurnRouteDto::SupplementContext,
        None,
        None,
        None,
        None,
    )
    .with_supplement_signal(mailbox_signal_ref, target_task_id.to_string()))
}

fn resolve_supplement_target_task(
    store: &magi_orchestrator::task_store::TaskStore,
    mission_id: &MissionId,
    root_task: &Task,
    target_task_id: Option<&str>,
) -> Result<Task, ApiError> {
    let Some(raw) = target_task_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(root_task.clone());
    };
    let task_id = TaskId::new(raw);
    let task = store
        .get_task(&task_id)
        .ok_or_else(|| ApiError::InvalidInput(format!("目标任务不存在: {raw}")))?;
    if task.mission_id != *mission_id {
        return Err(ApiError::InvalidInput(format!("任务 {raw} 不属于当前会话")));
    }
    Ok(task)
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
    let requested_session_id = request.requested_session_id();
    let has_recoverable_chain = requested_session_id
        .as_ref()
        .map(|session_id| session_has_recoverable_chain(state, session_id))
        .unwrap_or(false);
    let requests_continuation = session_turn_requests_continue_existing_task(request);
    if has_recoverable_chain && requests_continuation {
        return Ok(SessionTurnIntentDecision {
            route: SessionTurnRouteDto::Continue,
            task_title: None,
            execution_goal: None,
            task_tier: TaskTier::ExecutionChain,
            tool_intent: None,
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            confidence: 1.0,
            reason_code: Some("continue_requested".to_string()),
            route_reason: Some("用户明确要求继续当前可恢复执行链。".to_string()),
            task_evidence: Vec::new(),
        });
    }
    if requests_continuation
        && requested_session_id
            .as_ref()
            .is_some_and(|session_id| session_has_completed_long_mission_chain(state, session_id))
    {
        let task_text = request
            .trimmed_text()
            .unwrap_or_else(|| request.timeline_message(None));
        return Ok(SessionTurnIntentDecision {
            route: SessionTurnRouteDto::Task,
            task_title: Some(request.mission_title(Some(&task_text))),
            execution_goal: Some(task_text),
            task_tier: TaskTier::LongMission,
            tool_intent: None,
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            confidence: 1.0,
            reason_code: Some("next_mission_phase_requested".to_string()),
            route_reason: Some(
                "用户要求继续已完成的复杂任务，创建同一 Mission 下的下一条执行链。".to_string(),
            ),
            task_evidence: vec!["已完成 LongMission 的后续阶段请求".to_string()],
        });
    }

    let decision = normalize_session_turn_decision(
        local_session_turn_intent_decision(request, has_recoverable_chain),
        request,
    );
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
    let worker_runtime_handle = state
        .execution_pipeline()
        .map(|pipeline| pipeline.execution_runtime.worker_runtime());
    chain.branches.iter().any(|branch| {
        active_execution_branch_is_continue_recoverable(
            worker_runtime_handle,
            state.task_store(),
            &chain,
            branch,
        )
    })
}

fn session_has_completed_long_mission_chain(state: &ApiState, session_id: &SessionId) -> bool {
    let Some(chain) = state.session_store.active_execution_chain(session_id) else {
        return false;
    };
    let Some(task_store) = state.task_store() else {
        return false;
    };
    let Some(root_task) = task_store.get_task(&chain.root_task_id) else {
        return false;
    };
    root_task.mission_id == chain.mission_id
        && root_task.status == TaskStatus::Completed
        && root_task
            .policy_snapshot
            .as_ref()
            .is_some_and(|policy| policy.task_tier == TaskTier::LongMission)
}

fn local_session_turn_intent_decision(
    request: &SessionTurnRequestDto,
    has_recoverable_chain: bool,
) -> SessionTurnIntentDecision {
    let skill_name = request
        .skill_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("");
    let requests_explicit_task_or_agent =
        session_turn_requests_explicit_task_or_agent_mode(request);
    let requests_simple_execution = session_turn_requests_simple_execution_by_local_rules(request)
        || session_turn_requested_public_builtin_tools(request).is_some();
    let route = if has_recoverable_chain && session_turn_requests_continue_existing_task(request) {
        SessionTurnRouteDto::Continue
    } else if requests_explicit_task_or_agent
        || !skill_name.is_empty()
        || !request.images.is_empty()
        || (session_turn_requests_task_by_local_rules(request) && !requests_simple_execution)
    {
        SessionTurnRouteDto::Task
    } else if requests_simple_execution || session_turn_requests_execute_by_local_rules(request) {
        SessionTurnRouteDto::Execute
    } else {
        SessionTurnRouteDto::Chat
    };
    let task_tier = if matches!(route, SessionTurnRouteDto::Task)
        && session_turn_requests_explicit_long_mission(request)
    {
        TaskTier::LongMission
    } else {
        TaskTier::ExecutionChain
    };
    let task_text = request
        .trimmed_text()
        .unwrap_or_else(|| request.timeline_message(None));
    let task_evidence = if matches!(route, SessionTurnRouteDto::Task) {
        vec!["本地路由判定需要结构化任务执行".to_string()]
    } else {
        Vec::new()
    };
    SessionTurnIntentDecision {
        route,
        task_title: matches!(route, SessionTurnRouteDto::Task)
            .then(|| request.mission_title(Some(&task_text))),
        execution_goal: matches!(route, SessionTurnRouteDto::Task).then_some(task_text.clone()),
        task_tier,
        tool_intent: matches!(route, SessionTurnRouteDto::Execute).then_some(task_text),
        forced_tool_name: None,
        required_tool_chain: Vec::new(),
        confidence: 0.9,
        reason_code: Some(
            match route {
                SessionTurnRouteDto::Continue => "continue_requested",
                SessionTurnRouteDto::Task => "explicit_task_request",
                SessionTurnRouteDto::Execute => "tool_request",
                SessionTurnRouteDto::Chat | SessionTurnRouteDto::SupplementContext => "plain_chat",
            }
            .to_string(),
        ),
        route_reason: Some(
            match route {
                SessionTurnRouteDto::Continue => "用户要求继续且存在可恢复链",
                SessionTurnRouteDto::Task => "用户请求需要结构化任务执行",
                SessionTurnRouteDto::Execute => "用户请求需要工具执行但不需要任务投影",
                SessionTurnRouteDto::Chat | SessionTurnRouteDto::SupplementContext => "普通对话",
            }
            .to_string(),
        ),
        task_evidence,
    }
}

fn normalize_session_turn_decision(
    mut decision: SessionTurnIntentDecision,
    request: &SessionTurnRequestDto,
) -> SessionTurnIntentDecision {
    if !matches!(decision.route, SessionTurnRouteDto::Continue)
        && session_turn_requests_explicit_task_or_agent_mode(request)
    {
        let task_text = request
            .trimmed_text()
            .unwrap_or_else(|| request.timeline_message(None));
        decision.route = SessionTurnRouteDto::Task;
        decision.task_tier = if session_turn_requests_explicit_long_mission(request) {
            TaskTier::LongMission
        } else {
            TaskTier::ExecutionChain
        };
        decision.task_title = decision
            .task_title
            .take()
            .or_else(|| Some(request.mission_title(Some(&task_text))));
        decision.execution_goal = Some(task_text.clone());
        decision.tool_intent = None;
        decision.forced_tool_name = None;
        decision.required_tool_chain.clear();
        decision.confidence = decision.confidence.max(0.95);
        decision.reason_code = Some("explicit_task_request".to_string());
        decision.route_reason =
            Some("用户明确要求任务化执行，必须创建任务投影并由任务执行链处理。".to_string());
        if decision.task_evidence.is_empty() {
            decision
                .task_evidence
                .push("显式复杂任务/代理编排请求".to_string());
        }
    }
    let requests_direct_execution = session_turn_requests_simple_execution_by_local_rules(request)
        || session_turn_requested_public_builtin_tools(request).is_some();
    if matches!(decision.route, SessionTurnRouteDto::Task)
        && !session_turn_requests_explicit_task_or_agent_mode(request)
        && requests_direct_execution
    {
        let task_text = request
            .trimmed_text()
            .unwrap_or_else(|| request.timeline_message(None));
        decision.route = SessionTurnRouteDto::Execute;
        decision.task_title = None;
        decision.execution_goal = None;
        decision.task_tier = TaskTier::ExecutionChain;
        decision.tool_intent = Some(task_text);
        decision.forced_tool_name = None;
        decision.required_tool_chain.clear();
        decision.confidence = decision.confidence.max(0.9);
        decision.reason_code = Some("simple_execution_request".to_string());
        decision.route_reason = Some("用户请求是小范围一次性执行，不创建任务投影。".to_string());
        decision.task_evidence.clear();
    }
    if matches!(decision.route, SessionTurnRouteDto::Task)
        && !session_turn_task_route_has_creation_evidence(&decision)
    {
        decision.route = SessionTurnRouteDto::Chat;
        decision.task_title = None;
        decision.execution_goal = None;
        decision.task_tier = TaskTier::ExecutionChain;
        decision.tool_intent = None;
        decision.required_tool_chain.clear();
    }
    if !matches!(
        decision.route,
        SessionTurnRouteDto::Continue | SessionTurnRouteDto::Task
    ) && request
        .trimmed_text()
        .as_deref()
        .map(|text| {
            session_turn_requests_workspace_inspection_by_local_rules(&text.to_ascii_lowercase())
        })
        .unwrap_or(false)
    {
        let task_text = request
            .trimmed_text()
            .unwrap_or_else(|| request.timeline_message(None));
        decision.route = SessionTurnRouteDto::Execute;
        decision.task_title = None;
        decision.execution_goal = None;
        decision.task_tier = TaskTier::ExecutionChain;
        decision.tool_intent = Some(workspace_inspection_tool_intent(&task_text));
        decision.forced_tool_name = None;
        decision.required_tool_chain.clear();
        decision.confidence = decision.confidence.max(0.95);
        decision.reason_code = Some("workspace_inspection_request".to_string());
        decision.route_reason =
            Some("用户请求理解当前工作区，必须通过工具读取真实项目内容后再回答。".to_string());
        decision.task_evidence.clear();
    }
    if !matches!(
        decision.route,
        SessionTurnRouteDto::Continue | SessionTurnRouteDto::Task
    ) && let Some(tool_request) = session_turn_requested_public_builtin_tools(request)
    {
        match tool_request {
            RequestedBuiltinTools::Single(tool_name) => {
                decision.route = SessionTurnRouteDto::Execute;
                decision.task_title = None;
                decision.execution_goal = None;
                decision.task_tier = TaskTier::ExecutionChain;
                if decision.forced_tool_name.as_deref() != Some(tool_name) {
                    decision.tool_intent = Some(explicit_builtin_tool_intent(tool_name));
                    decision.forced_tool_name = Some(tool_name.to_string());
                    decision.required_tool_chain.clear();
                    decision.route_reason =
                        Some(format!("用户明确要求调用公开内置工具 {tool_name}。"));
                }
                decision.confidence = decision.confidence.max(0.95);
                decision.reason_code = Some("tool_request".to_string());
                decision.task_evidence.clear();
            }
            RequestedBuiltinTools::Multiple(tool_names) => {
                decision.route = SessionTurnRouteDto::Execute;
                decision.task_title = None;
                decision.execution_goal = None;
                decision.task_tier = TaskTier::ExecutionChain;
                decision.tool_intent = Some(multi_builtin_tool_intent(&tool_names));
                decision.forced_tool_name = None;
                decision.required_tool_chain =
                    tool_names.iter().map(|tool| tool.to_string()).collect();
                decision.confidence = decision.confidence.max(0.95);
                decision.reason_code = Some("tool_request".to_string());
                decision.route_reason = Some(format!(
                    "用户明确要求串联调用多个公开内置工具：{}。",
                    tool_names.join(", ")
                ));
                decision.task_evidence.clear();
            }
        }
    }
    decision
}

fn session_turn_requests_task_by_local_rules(request: &SessionTurnRequestDto) -> bool {
    let Some(text) = request.trimmed_text() else {
        return false;
    };
    let normalized = text.to_ascii_lowercase();
    [
        "分析并拆分",
        "拆分任务",
        "重新规划",
        "实现",
        "开发",
        "修复",
        "重构",
        "收口",
        "迭代",
        "推进",
        "中等任务",
        "任务编排",
        "多代理",
        "多 agent",
        "multi-agent",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
        || normalized_contains_agent_dispatch_request(&normalized)
}

fn session_turn_requests_simple_execution_by_local_rules(request: &SessionTurnRequestDto) -> bool {
    let Some(text) = request.trimmed_text() else {
        return false;
    };
    let normalized = text.to_ascii_lowercase();
    if session_turn_requests_explicit_task_or_agent_mode(request)
        || session_turn_has_structured_task_scope(&normalized)
    {
        return false;
    }
    let has_direct_work = [
        "修复",
        "修改",
        "改一下",
        "调整",
        "更新",
        "写入",
        "创建",
        "删除",
        "替换",
        "读取",
        "查看",
        "打开",
        "运行",
        "执行",
        "列出",
        "生成",
        "画",
        "绘制",
        "fix",
        "edit",
        "update",
        "write",
        "create",
        "delete",
        "run",
        "read",
    ]
    .iter()
    .any(|marker| normalized.contains(marker));
    let has_small_scope = [
        "简单",
        "小范围",
        "一次性",
        "只",
        "直接",
        "顺手",
        "这个文件",
        "单文件",
        "一处",
        "一行",
        "错别字",
        "不用任务",
        "不要创建任务",
        "不需要任务",
        "无需任务",
        "simple",
        "one-off",
        "single file",
    ]
    .iter()
    .any(|marker| normalized.contains(marker));
    has_direct_work && has_small_scope
}

fn session_turn_has_structured_task_scope(normalized: &str) -> bool {
    [
        "并验证",
        "完成后",
        "拆分",
        "规划",
        "多阶段",
        "多轮",
        "可恢复",
        "代理",
        "agent",
        "任务编排",
        "中等任务",
        "复杂任务",
        "长期任务",
        "重构",
        "迁移",
        "架构",
        "全量",
        "完整",
        "端到端",
        "e2e",
        "验收",
        "运行测试",
        "测试并",
        "validate",
        "verify",
        "migration",
        "refactor",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn session_turn_requests_execute_by_local_rules(request: &SessionTurnRequestDto) -> bool {
    let Some(text) = request.trimmed_text() else {
        return false;
    };
    let normalized = text.to_ascii_lowercase();
    if session_turn_requests_workspace_inspection_by_local_rules(&normalized) {
        return true;
    }
    [
        "搜索",
        "查找",
        "查询",
        "读取",
        "打开",
        "查看",
        "执行",
        "运行",
        "列出",
        "画",
        "绘制",
        "生成图",
        "渲染图",
        "mermaid",
        "dot",
        "graphviz",
        "search",
        "find",
        "grep",
        "rg",
        "ls",
        "cat",
        "git",
        "npm",
        "cargo",
        "test",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn session_turn_requests_workspace_inspection_by_local_rules(normalized: &str) -> bool {
    let workspace_target = [
        "当前项目",
        "当前工程",
        "当前仓库",
        "本项目",
        "本工程",
        "本仓库",
        "这个项目",
        "这个工程",
        "这个仓库",
        "项目结构",
        "仓库结构",
        "工程结构",
        "目录结构",
        "代码结构",
        "current project",
        "current repo",
        "current repository",
        "current codebase",
        "this project",
        "this repo",
        "this repository",
        "codebase",
    ]
    .iter()
    .any(|marker| normalized.contains(marker));
    if !workspace_target {
        return false;
    }

    [
        "分析",
        "检查",
        "审查",
        "查看",
        "看看",
        "看下",
        "读取",
        "梳理",
        "总结",
        "介绍",
        "说明",
        "是什么",
        "有什么",
        "如何",
        "analyze",
        "inspect",
        "review",
        "read",
        "summarize",
        "explain",
        "what is",
        "what's",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn session_turn_requests_continue_existing_task(request: &SessionTurnRequestDto) -> bool {
    let Some(text) = request.trimmed_text() else {
        return false;
    };
    let normalized = text.trim().to_ascii_lowercase();
    [
        "继续",
        "继续推进",
        "继续执行",
        "继续跑",
        "接着",
        "接着做",
        "接着推进",
        "往下做",
        "恢复任务",
        "恢复执行",
        "从刚才",
        "从上次",
        "下一步",
        "下一阶段",
        "下个阶段",
        "继续剩余",
        "推进剩余",
        "resume",
        "continue",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn session_turn_requests_explicit_long_mission(request: &SessionTurnRequestDto) -> bool {
    let Some(text) = request.trimmed_text() else {
        return false;
    };
    let normalized = text.to_ascii_lowercase();
    normalized.contains("复杂任务模式")
        || normalized.contains("复杂任务")
        || normalized.contains("复杂长期任务")
        || normalized.contains("深度任务")
        || normalized.contains("long mission")
        || normalized.contains("longmission")
        || normalized.contains("长期任务")
        || normalized.contains("跨多轮")
        || normalized.contains("多阶段")
        || normalized.contains("可恢复")
        || normalized.contains("人审")
        || normalized.contains("审计")
}

fn session_turn_requests_explicit_task_or_agent_mode(request: &SessionTurnRequestDto) -> bool {
    let Some(text) = request.trimmed_text() else {
        return false;
    };
    let normalized = text.to_ascii_lowercase();
    normalized.contains("复杂任务模式")
        || normalized.contains("复杂任务")
        || normalized.contains("复杂长期任务")
        || normalized.contains("深度任务")
        || normalized.contains("long mission")
        || normalized.contains("longmission")
        || normalized.contains("长期任务")
        || normalized.contains("中等任务")
        || normalized.contains("任务编排")
        || normalized.contains("任务模式完成")
        || normalized.contains("以任务模式")
        || normalized.contains("派发代理")
        || normalized.contains("分派代理")
        || normalized.contains("分配代理")
        || normalized.contains("代理执行")
        || normalized.contains("代理任务")
        || normalized.contains("代理角色")
        || normalized.contains("agent 必须")
        || normalized.contains("agent 调用")
        || normalized.contains("agent 执行")
        || normalized.contains("子任务")
        || normalized_contains_agent_dispatch_request(&normalized)
}

fn normalized_contains_agent_dispatch_request(normalized: &str) -> bool {
    let has_agent_target = normalized.contains("代理") || normalized.contains("agent");
    let has_dispatch_verb = ["派发", "分派", "分配", "启动", "创建", "调用", "spawn"]
        .iter()
        .any(|verb| normalized.contains(verb));
    has_agent_target && has_dispatch_verb
}

enum RequestedBuiltinTools {
    Single(&'static str),
    Multiple(Vec<&'static str>),
}

fn session_turn_requested_public_builtin_tools(
    request: &SessionTurnRequestDto,
) -> Option<RequestedBuiltinTools> {
    let normalized = request.trimmed_text()?.to_ascii_lowercase();
    let mut matches: Vec<(&'static str, usize)> = Vec::new();
    for (alias, canonical_name) in public_builtin_tool_reference_aliases() {
        let Some(position) = tool_reference_position(&normalized, alias) else {
            continue;
        };
        if let Some((_, existing_position)) =
            matches.iter_mut().find(|(name, _)| *name == canonical_name)
        {
            *existing_position = (*existing_position).min(position);
        } else {
            matches.push((canonical_name, position));
        }
    }
    matches.sort_by_key(|(_, position)| *position);
    let tool_names = matches
        .into_iter()
        .map(|(tool_name, _)| tool_name)
        .collect::<Vec<_>>();
    match tool_names.as_slice() {
        [] => None,
        [tool_name] => Some(RequestedBuiltinTools::Single(tool_name)),
        _ => Some(RequestedBuiltinTools::Multiple(tool_names)),
    }
}

fn explicit_builtin_tool_intent(tool_name: &str) -> String {
    format!(
        "用户明确要求调用公开内置工具 {tool_name}。必须直接调用 {tool_name} 工具，并从用户原始输入中提取参数；不要创建任务，不要改用其它工具，不要只输出文字说明。工具完成后只基于该工具结果给出简短回复。"
    )
}

fn workspace_inspection_tool_intent(user_text: &str) -> String {
    format!(
        "用户请求理解当前工作区：{user_text}。必须使用可用工具读取当前工作区的真实目录、README、配置或关键源码后再回答；不要在未调用工具时声称已经读取文件、执行命令或输出 tool_call_block。工具完成后只基于实际工具结果总结。"
    )
}

fn multi_builtin_tool_intent(tool_names: &[&str]) -> String {
    format!(
        "用户明确要求串联调用多个公开内置工具：{}。必须按用户原始输入描述的依赖顺序选择并调用这些工具；每个工具的 path/source/destination/command/query/content/patch/diff 参数必须从对应编号步骤原文提取。如果用户已经指定文件名、目录名或命令，禁止改名为 probe、tmp、placeholder 或其它自造临时名；最终回复只能描述工具实际结果。不要创建任务，不要只输出文字说明。某一步失败时应原位展示失败工具，并基于已执行结果给出简短说明。",
        tool_names.join(", ")
    )
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

fn build_user_message_turn_item(
    accepted_at: UtcMillis,
    message: &str,
    entry_id: &str,
    request_id: Option<String>,
    user_message_id: Option<String>,
    placeholder_message_id: Option<String>,
    task_id: Option<magi_core::TaskId>,
    source_thread_id: magi_core::ThreadId,
) -> (String, ActiveExecutionTurnItem) {
    let user_message_item_id = user_message_id
        .clone()
        .unwrap_or_else(|| format!("turn-item-user-{}", accepted_at.0));
    (
        user_message_item_id.clone(),
        ActiveExecutionTurnItem {
            item_id: user_message_item_id,
            item_seq: 1,
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
            // P7：user_message 由前端用户发起，归属到 orchestrator thread，走主线可见性。
            source_thread_id,
        },
    )
}

async fn submit_regular_session_turn(
    state: ApiState,
    request: SessionTurnRequestDto,
    requested_workspace_id: WorkspaceId,
    accepted_at: UtcMillis,
    decision: SessionTurnIntentDecision,
) -> Result<SessionTurnResponseDto, ApiError> {
    let message = request.timeline_message(request.trimmed_text().as_deref());
    let placeholder_title = crate::session_title::NEW_SESSION_PLACEHOLDER_TITLE;
    let (session_id, created_session, workspace_id) = super::resolve_dispatch_session(
        &state,
        request.requested_session_id(),
        Some(requested_workspace_id),
        placeholder_title,
        accepted_at,
    )?;
    // S1：user 信号经 Conversation Mailbox 入栈，下游一律读 signal.* 不再读 request.*
    let signal =
        super::ingest_user_input_to_conversation(&state, &session_id, &request, accepted_at);
    let workspace_root_path = state
        .workspace_root_path(&workspace_id)
        .map(|path| path.display().to_string());
    state
        .ensure_snapshot_session_for_workspace_id(&session_id, &workspace_id)
        .await?;
    let entry_id = format!("timeline-{}-{}", session_id, accepted_at.0);
    let request_id = signal.request_id.clone();
    let user_message_id = signal.user_message_id.clone();
    let requested_placeholder_message_id = signal.placeholder_message_id.clone();
    // P7：所有 turn item 必须携带 source_thread_id，由 ensure_session_mission 提供 orchestrator thread。
    let (_mission_id, orchestrator_thread_id) =
        state
            .session_store
            .ensure_session_mission(&session_id, accepted_at, || {
                magi_core::MissionId::new(format!("mission-session-chat-{}", accepted_at.0))
            });
    // P7：placeholder_message_id 不再作为 turn item 在 accept 阶段预占 item_seq。
    // 历史方案曾把 assistant_stream placeholder 以 item_seq=2 写入 turn.items，
    // 这样首轮 thinking 走 max(item_seq)+1 后存储顺序变成 text(2) → thinking(3)，
    // 与 Anthropic 协议输出顺序（thinking → text）倒挂，迫使前端 projection 层做
    // peer-matching 补丁。现在只把 id 字符串传给 runtime：首个 text delta upsert
    // 时自然走 max+1，存储顺序天然匹配协议顺序，projection 层也不再需要补偿逻辑。
    let assistant_placeholder_item_id = requested_placeholder_message_id
        .clone()
        .unwrap_or_else(|| format!("turn-item-assistant-stream-{}-0", accepted_at.0));
    let placeholder_message_id = Some(assistant_placeholder_item_id.clone());
    // 使用前端传入的 userMessageId 作为 canonical item_id，确保前端乐观节点与后端流式更新使用同一 ID
    let (user_message_item_id, user_message_item) = build_user_message_turn_item(
        accepted_at,
        &message,
        &entry_id,
        request_id.clone(),
        user_message_id.clone(),
        placeholder_message_id.clone(),
        None,
        orchestrator_thread_id.clone(),
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
    state.persist_session_state_checkpoint("session_turn_accepted")?;
    publish_session_user_message_created_event(
        &state,
        &session_id,
        workspace_id.clone(),
        accepted_at,
        &message,
    );
    let accepted_canonical_turn = state
        .session_store
        .canonical_turns_for_session(&session_id)
        .into_iter()
        .find(|turn| turn.turn_id == turn_id);
    let accepted_canonical_item = accepted_canonical_turn
        .as_ref()
        .and_then(|turn| {
            turn.items
                .iter()
                .find(|item| item.item_id == assistant_placeholder_item_id)
        })
        .cloned();
    let event_id = publish_regular_session_turn_accepted_event(
        &state,
        &session_id,
        workspace_id.as_ref(),
        accepted_at,
        created_session,
        decision.route,
        accepted_canonical_turn.as_ref(),
        Some(&assistant_placeholder_item_id),
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
            access_profile: request.requested_access_profile(),
            skill_name: request.skill_name.clone(),
            request_id: request_id.clone(),
            user_message_id: user_message_id.clone(),
            placeholder_message_id: placeholder_message_id.clone(),
            forced_tool_name: decision.forced_tool_name.clone(),
            required_tool_chain: decision.required_tool_chain.clone(),
            workspace_root_path,
        },
        accepted_at,
        decision.route,
        created_session,
    );

    if created_session {
        crate::session_title::spawn_new_session_title_refinement(
            &state,
            &session_id,
            &message,
            placeholder_title,
        );
    }

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
    )
    .with_canonical_event(
        "turn_started",
        accepted_canonical_turn,
        accepted_canonical_item,
    ))
}

fn spawn_regular_session_turn_execution(
    state: ApiState,
    execution_request: SessionTurnExecutionRequest,
    accepted_at: UtcMillis,
    route: SessionTurnRouteDto,
    created_session: bool,
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

        if let Err(error) = super::begin_session_turn(&state, &session_id) {
            tracing::error!(
                session_id = %session_id,
                ?error,
                "regular session turn background execution rejected: active turn already exists"
            );
            let _ = state
                .session_store
                .update_current_turn_status(&session_id, "failed");
            let _ = state.persist_session_durable_state();
            return;
        }
        let outcome = dispatcher.execute_session_turn(execution_request);
        super::finalize_session_turn(&state, &session_id, outcome.is_ok());
        match outcome {
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
                        session_turn_failed_event_payload(&session_id, route),
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

fn session_turn_failed_event_payload(
    session_id: &SessionId,
    route: SessionTurnRouteDto,
) -> serde_json::Value {
    json!({
        "session_id": session_id.to_string(),
        "route": route,
        "error": "session_turn_failed",
        "error_code": "session_turn_failed",
    })
}

fn publish_regular_session_turn_accepted_event(
    state: &ApiState,
    session_id: &SessionId,
    workspace_id: Option<&magi_core::WorkspaceId>,
    accepted_at: UtcMillis,
    created_session: bool,
    route: SessionTurnRouteDto,
    canonical_turn: Option<&CanonicalTurn>,
    canonical_item_id: Option<&str>,
) -> Result<EventId, ApiError> {
    let event_id = EventId::new(format!("event-session-turn-accepted-{}", accepted_at.0));
    let canonical_item = canonical_turn
        .and_then(|turn| {
            canonical_item_id
                .and_then(|item_id| turn.items.iter().find(|item| item.item_id == item_id))
        })
        .cloned();
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.turn.accepted",
        json!({
            "session_id": session_id.to_string(),
            "workspace_id": workspace_id.map(ToString::to_string),
            "created_session": created_session,
            "route": route,
            "canonical_schema_version": CANONICAL_TURN_SCHEMA_VERSION,
            "canonical_event_kind": "turn_started",
            "canonical_turn": canonical_turn,
            "canonical_item": canonical_item,
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
    let workspace_id = session_workspace_for_event(state, &accepted.session_id);
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.turn.continue.executed",
        json!({
            "session_id": accepted.session_id.to_string(),
            "workspace_id": workspace_id.as_ref().map(ToString::to_string),
            "mission_id": accepted.mission_id.to_string(),
            "root_task_id": accepted.root_task_id.to_string(),
            "execution_chain_ref": accepted.execution_chain_ref.clone(),
            "resumed_branch_count": accepted.resumed_branch_count,
            "runner_started": accepted.runner_started,
        }),
    )
    .with_context(EventContext {
        session_id: Some(accepted.session_id.clone()),
        workspace_id,
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
    #[serde(default, alias = "workspace_id")]
    workspace_id: Option<String>,
    prompt_text: Option<String>,
    #[serde(default)]
    requested_agent_ids: Vec<String>,
    #[serde(alias = "request_id")]
    request_id: Option<String>,
    #[serde(alias = "user_message_id")]
    user_message_id: Option<String>,
    #[serde(alias = "placeholder_message_id")]
    placeholder_message_id: Option<String>,
}

impl ContinueSessionRequest {
    fn requested_workspace_id(&self) -> Option<&str> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }
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
    workspace_id: String,
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
    workspace_id: Option<String>,
    turn_id: Option<String>,
    event_id: String,
    requested_at: UtcMillis,
    cancelled_tool_process_count: usize,
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
    let workspace_id = require_registered_workspace_id(state, request.requested_workspace_id())?;
    let session_id = request
        .requested_session_id()
        .ok_or_else(|| ApiError::InvalidInput("sessionId 不能为空".to_string()))?;
    require_session_record_in_workspace(state, &session_id, Some(workspace_id.as_str()))
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

fn write_continue_user_message(
    state: &ApiState,
    accepted: &SessionContinueAccepted,
    prompt_text: Option<&str>,
    continued_at: UtcMillis,
    request_id: Option<String>,
    user_message_id: Option<String>,
    placeholder_message_id: Option<String>,
    orchestrator_thread_id: magi_core::ThreadId,
) -> Result<(String, Option<String>), ApiError> {
    let entry_id = format!("timeline-{}-{}", accepted.session_id, continued_at.0);
    let Some(prompt_text) = prompt_text else {
        return Ok((entry_id, None));
    };
    let (user_message_item_id, user_message_item) = build_user_message_turn_item(
        continued_at,
        prompt_text,
        &entry_id,
        request_id,
        user_message_id,
        placeholder_message_id,
        Some(accepted.action_task_id.clone()),
        orchestrator_thread_id,
    );
    let append_to_current_turn = state
        .session_store
        .runtime_sidecar(&accepted.session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .is_some_and(|turn| turn_status_is_interruptible(&turn.status));
    if append_to_current_turn {
        let updated = state
            .session_store
            .append_current_turn_item_with_timeline_entry(
                &accepted.session_id,
                entry_id.clone(),
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
    } else {
        let mut turn = ActiveExecutionTurn {
            turn_id: format!("turn-session-continue-{}", continued_at.0),
            turn_seq: continued_at.0 as u64,
            accepted_at: continued_at,
            status: "running".to_string(),
            completed_at: None,
            user_message: Some(prompt_text.to_string()),
            items: vec![user_message_item],
        };
        turn.normalize();
        state
            .session_store
            .accept_current_turn_with_timeline_entry(
                accepted.session_id.clone(),
                entry_id.clone(),
                TimelineEntryKind::UserMessage,
                prompt_text,
                continued_at,
                turn,
            )
            .map_err(|error| ApiError::internal_assembly("写入 continue 用户消息失败", error))?;
    }
    state.persist_session_state_checkpoint("session_continue_user_message")?;
    publish_session_user_message_created_event(
        state,
        &accepted.session_id,
        session_workspace_for_event(state, &accepted.session_id),
        continued_at,
        prompt_text,
    );
    Ok((entry_id, Some(user_message_item_id)))
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
    let terminal_root_finalized =
        finalize_terminal_root_before_interrupt(&state, &session_id, current_turn.as_ref());
    let interrupted = current_turn
        .as_ref()
        .is_some_and(|turn| turn_status_is_interruptible(&turn.status))
        && !terminal_root_finalized;
    let streaming_entry_ids = if interrupted {
        current_turn_streaming_timeline_entry_ids(&state, &session_id)
    } else {
        Vec::new()
    };

    let cancelled_tool_process_count = if interrupted {
        state.cancel_active_tool_executions(Some(&session_id), workspace_id.as_ref(), None)
    } else {
        0
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

    state.persist_session_state_checkpoint("session_turn_interrupted")?;
    let event_id = EventId::new(format!("event-session-turn-interrupt-{}", now.0));
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.turn.interrupted",
        json!({
            "session_id": session_id.to_string(),
            "workspace_id": workspace_id.as_ref().map(ToString::to_string),
            "turn_id": turn_id.clone(),
            "interrupted": interrupted,
            "cancelled_tool_process_count": cancelled_tool_process_count,
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
        workspace_id: workspace_id.as_ref().map(ToString::to_string),
        turn_id,
        event_id: event_id.to_string(),
        requested_at: now,
        cancelled_tool_process_count,
        removed_timeline_entry_ids: streaming_entry_ids,
    }))
}

fn finalize_terminal_root_before_interrupt(
    state: &ApiState,
    session_id: &SessionId,
    current_turn: Option<&ActiveExecutionTurn>,
) -> bool {
    if !current_turn.is_some_and(|turn| turn_status_is_interruptible(&turn.status)) {
        return false;
    }
    let Some(task_store) = state.task_store() else {
        return false;
    };
    let Some(chain) = state
        .session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.active_execution_chain)
    else {
        return false;
    };
    let Some(root_task) = task_store.get_task(&chain.root_task_id) else {
        return false;
    };
    let runner_status = match root_task.status {
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "error",
        TaskStatus::Killed => "killed",
        _ => return false,
    };
    finalize_background_session_task_turn_if_root_terminal(
        state,
        session_id,
        &chain.root_task_id,
        runner_status,
    )
}

async fn switch_session(
    State(state): State<ApiState>,
    Json(request): Json<SwitchSessionRequest>,
) -> Result<Json<SessionSelectionResponseDto>, ApiError> {
    let session_id = SessionId::new(&request.session_id);
    let workspace_id = require_registered_workspace_id(&state, request.requested_workspace_id())?;
    require_session_record_in_workspace(&state, &session_id, Some(workspace_id.as_str()))?;
    state
        .session_store
        .switch_session(&session_id)
        .map_err(|e| ApiError::internal_assembly("切换会话失败", e))?;
    state.persist_session_durable_state_for_api()?;
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
    let workspace_id = require_registered_workspace_id(&state, request.requested_workspace_id())?;
    require_session_record_in_workspace(&state, &session_id, Some(workspace_id.as_str()))?;
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
    let requested_agent_ids = request
        .requested_agent_ids
        .into_iter()
        .map(|agent_id| agent_id.trim().to_string())
        .filter(|agent_id| !agent_id.is_empty())
        .map(WorkerId::new)
        .collect::<Vec<_>>();
    let continued_at = UtcMillis::now();
    let accepted = continue_execution_chain(&state, &session_id, &requested_agent_ids)?;
    let (_, orchestrator_thread_id) =
        state
            .session_store
            .ensure_session_mission(&session_id, continued_at, || accepted.mission_id.clone());
    let _ = write_continue_user_message(
        &state,
        &accepted,
        prompt_text.as_deref(),
        continued_at,
        request_id,
        user_message_id,
        placeholder_message_id,
        orchestrator_thread_id,
    )?;
    finalize_continue_session(state.clone(), accepted.clone(), continued_at);
    state.persist_runtime_durable_state_for_api()?;
    let event_id = EventId::new(format!("event-session-continue-{}", continued_at.0));
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.continue.executed",
        json!({
            "session_id": accepted.session_id.to_string(),
            "workspace_id": workspace_id.to_string(),
            "mission_id": accepted.mission_id.to_string(),
            "root_task_id": accepted.root_task_id.to_string(),
            "execution_chain_ref": accepted.execution_chain_ref,
            "resumed_branch_count": accepted.resumed_branch_count,
            "runner_started": accepted.runner_started,
        }),
    )
    .with_context(EventContext {
        session_id: Some(accepted.session_id.clone()),
        workspace_id: Some(workspace_id.clone()),
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
        workspace_id: workspace_id.to_string(),
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
    let Some(_task_store) = state.task_store() else {
        return;
    };

    // 所有 tier 的 dispatch 驱动统一交给后台 RunnerManager：runner 已在
    // `continue_execution_chain` 中重新启动，此处不再补一次同步驱动，避免双轨竞争。终态汇聚由
    // `RunnerManager::with_terminal_observer` 监听 root task 完成事件触发，
    // `append_dispatch_assistant_message` 在 root 尚未完成时安全 no-op。

    super::append_dispatch_assistant_message(
        &state,
        &DispatchSubmissionAccepted {
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
    let workspace_id = require_registered_workspace_id(&state, request.requested_workspace_id())?;
    let session_id = SessionId::new(&request.session_id);
    require_session_record_in_workspace(&state, &session_id, Some(workspace_id.as_str()))?;
    state
        .session_store
        .delete_session(&session_id)
        .map_err(|e| ApiError::internal_assembly("删除会话失败", e))?;
    state.persist_session_durable_state_for_api()?;
    Ok(Json(state.bootstrap_dto_for_workspace_session(
        Some(workspace_id.as_str()),
        None,
    )?))
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
    let workspace_id = require_registered_workspace_id(&state, request.requested_workspace_id())?;
    let session_id = SessionId::new(&request.session_id);
    require_session_record_in_workspace(&state, &session_id, Some(workspace_id.as_str()))?;
    state
        .session_store
        .rename_session(&session_id, &request.name)
        .map_err(|e| ApiError::internal_assembly("重命名会话失败", e))?;
    state.persist_session_durable_state_for_api()?;
    Ok(Json(state.bootstrap_dto_for_workspace_session(
        Some(workspace_id.as_str()),
        Some(&session_id),
    )?))
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
    let workspace_id = require_registered_workspace_id(&state, request.requested_workspace_id())?;
    let session_id = SessionId::new(&request.session_id);
    require_session_record_in_workspace(&state, &session_id, Some(workspace_id.as_str()))?;
    state
        .session_store
        .archive_session(&session_id)
        .map_err(|e| ApiError::internal_assembly("关闭会话失败", e))?;
    if let Some(manager) = state.runner_manager() {
        manager.unbind_session(&session_id);
    }
    state.persist_session_durable_state_for_api()?;
    Ok(Json(state.bootstrap_dto_for_workspace_session(
        Some(workspace_id.as_str()),
        None,
    )?))
}

async fn save_session(
    State(state): State<ApiState>,
    Json(request): Json<SaveSessionRequest>,
) -> Result<Json<BootstrapDto>, ApiError> {
    let workspace_id = require_registered_workspace_id(&state, request.requested_workspace_id())?;
    let selected_session_id = if let Some(session_id) = request.requested_session_id() {
        require_session_record_in_workspace(&state, &session_id, Some(workspace_id.as_str()))?;
        Some(session_id)
    } else {
        None
    };
    state.persist_session_durable_state_for_api()?;
    Ok(Json(state.bootstrap_dto_for_workspace_session(
        Some(workspace_id.as_str()),
        selected_session_id.as_ref(),
    )?))
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
    let workspace_id = require_registered_workspace_id(&state, query.requested_workspace_id())?;
    let session_id =
        require_notifications_session_id(&state, query.requested_session_id(), &workspace_id)?;
    Ok(Json(build_notifications_response(
        &state,
        &session_id,
        &workspace_id,
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
    let workspace_id = require_registered_workspace_id(&state, request.requested_workspace_id())?;
    let session_id =
        require_notifications_session_id(&state, request.requested_session_id(), &workspace_id)?;
    if request.persist_to_center == Some(false) {
        return Ok(Json(build_notifications_response(
            &state,
            &session_id,
            &workspace_id,
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
    state.persist_session_durable_state_for_api()?;
    Ok(Json(build_notifications_response(
        &state,
        &session_id,
        &workspace_id,
    )))
}

async fn mark_all_notifications_read(
    State(state): State<ApiState>,
    Json(request): Json<NotificationScopeRequest>,
) -> Result<Json<SessionNotificationsResponseDto>, ApiError> {
    let workspace_id = require_registered_workspace_id(&state, request.requested_workspace_id())?;
    let session_id =
        require_notifications_session_id(&state, request.requested_session_id(), &workspace_id)?;
    state
        .session_store
        .mark_notifications_handled_for_session(&session_id);
    state.persist_session_durable_state_for_api()?;
    Ok(Json(build_notifications_response(
        &state,
        &session_id,
        &workspace_id,
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
    let workspace_id = require_registered_workspace_id(&state, request.requested_workspace_id())?;
    let session_id =
        require_notifications_session_id(&state, request.requested_session_id(), &workspace_id)?;
    state
        .session_store
        .clear_notifications_for_session(&session_id);
    state.persist_session_durable_state_for_api()?;
    Ok(Json(build_notifications_response(
        &state,
        &session_id,
        &workspace_id,
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
    let workspace_id = require_registered_workspace_id(&state, request.requested_workspace_id())?;
    let session_id =
        require_notifications_session_id(&state, request.requested_session_id(), &workspace_id)?;
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
    state.persist_session_durable_state_for_api()?;
    Ok(Json(build_notifications_response(
        &state,
        &session_id,
        &workspace_id,
    )))
}

fn build_notifications_response(
    state: &ApiState,
    session_id: &SessionId,
    requested_workspace_id: &WorkspaceId,
) -> SessionNotificationsResponseDto {
    let workspace_id = state
        .session_store
        .session(session_id)
        .and_then(|session| session_workspace_id(state, &session))
        .map(|workspace_id| workspace_id.to_string())
        .unwrap_or_else(|| requested_workspace_id.to_string());
    SessionNotificationsResponseDto::from_records(
        session_id,
        Some(workspace_id),
        state.session_store.notifications_for_session(session_id),
    )
}

fn require_notifications_session_id(
    state: &ApiState,
    requested_session_id: Option<SessionId>,
    requested_workspace_id: &WorkspaceId,
) -> Result<SessionId, ApiError> {
    let session_id = requested_session_id
        .ok_or_else(|| ApiError::InvalidInput("sessionId 不能为空".to_string()))?;
    require_session_record_in_workspace(state, &session_id, Some(requested_workspace_id.as_str()))?;
    Ok(session_id)
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
    use magi_core::{
        AbsolutePath, ExecutionOwnership, MissionId, Task, TaskId, TaskKind, TaskPolicy,
        TaskRuntimePayload, TaskStatus, UtcMillis, WorkspaceId,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_orchestrator::task_store::TaskStore;
    use magi_session_store::{
        ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext, SessionStore,
    };
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

    #[test]
    fn session_turn_failed_event_payload_hides_runtime_error_detail() {
        let payload = session_turn_failed_event_payload(
            &SessionId::new("session-failed-redaction"),
            SessionTurnRouteDto::Chat,
        );

        assert_eq!(payload["session_id"], json!("session-failed-redaction"));
        assert_eq!(payload["error"], json!("session_turn_failed"));
        assert_eq!(payload["error_code"], json!("session_turn_failed"));
        assert!(
            !payload
                .to_string()
                .contains("/Users/xie/.mcp/server failed: ENOENT")
        );
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

    fn session_turn_request(text: &str) -> SessionTurnRequestDto {
        SessionTurnRequestDto {
            session_id: None,
            workspace_id: None,
            text: Some(text.to_string()),
            skill_name: None,
            images: Vec::new(),
            access_profile: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            supplement_context: false,
            target_task_id: None,
        }
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

    fn long_mission_policy() -> TaskPolicy {
        TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            access_profile: magi_core::AccessProfile::Restricted,
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 1,
            validation_profile: Some("required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            task_tier: TaskTier::LongMission,
            background_allowed: true,
            escalation_conditions: vec!["human_checkpoint".to_string()],
        }
    }

    fn completed_long_mission_root_task(task_id: &str, mission_id: &MissionId) -> Task {
        let mut task = test_root_task(task_id, mission_id.as_str());
        task.status = TaskStatus::Completed;
        task.policy_snapshot = Some(long_mission_policy());
        task
    }

    fn completed_long_mission_chain(
        session_id: &SessionId,
        mission_id: &MissionId,
        root_task_id: &TaskId,
        now: UtcMillis,
    ) -> ActiveExecutionChain {
        let worker_id = WorkerId::new("worker-long-mission-completed");
        let thread_id = magi_core::ThreadId::new("thread-long-mission-completed");
        ActiveExecutionChain {
            session_id: session_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            execution_chain_ref: "chain-long-mission-completed".to_string(),
            workspace_id: None,
            active_branch_task_ids: vec![root_task_id.clone()],
            active_worker_bindings: vec![worker_id.clone()],
            branches: vec![ActiveExecutionBranch {
                task_id: root_task_id.clone(),
                worker_id,
                stage: "finish".to_string(),
                lease_id: None,
                execution_intent_ref: None,
                binding_lifecycle: None,
                checkpoint_stage: Some("finish".to_string()),
                next_step_index: None,
                checkpoint_at: Some(now),
                resume_mode: Some("stage-restart".to_string()),
                resume_token: None,
                use_tools: true,
                skill_name: None,
                is_primary: true,
                thread_id,
            }],
            recovery_ref: None,
            dispatch_context: ActiveExecutionDispatchContext {
                accepted_at: now,
                entry_id: "timeline-long-mission-completed".to_string(),
                trimmed_text: Some("上一阶段".to_string()),
                skill_name: None,
            },
            current_turn: None,
        }
    }

    fn classifier_chat_decision() -> SessionTurnIntentDecision {
        SessionTurnIntentDecision {
            route: SessionTurnRouteDto::Chat,
            task_title: None,
            execution_goal: None,
            task_tier: TaskTier::ExecutionChain,
            tool_intent: None,
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            confidence: 0.86,
            reason_code: Some("plain_chat".to_string()),
            route_reason: Some("classifier returned chat".to_string()),
            task_evidence: Vec::new(),
        }
    }

    #[test]
    fn explicit_continue_text_is_detected_before_classifier() {
        let request = session_turn_request("继续推进刚才未完成的任务");

        assert!(session_turn_requests_continue_existing_task(&request));

        let next_phase_request = session_turn_request("进入下一阶段，继续剩余验收");
        assert!(session_turn_requests_continue_existing_task(
            &next_phase_request
        ));
    }

    #[test]
    fn session_turn_routing_does_not_require_model_bridge_for_plain_message() {
        let state = test_state();
        let request = session_turn_request("你好，解释一下当前状态");

        let decision = decide_session_turn_with_task_planner(&state, &request)
            .expect("本地路由不应依赖外部模型");

        assert!(matches!(decision.route, SessionTurnRouteDto::Chat));
    }

    #[tokio::test]
    async fn session_turn_rejects_missing_workspace_scope() {
        let (status, body) = post_json(
            test_state(),
            "/session/turn",
            serde_json::json!({
                "text": "你好",
                "images": [],
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

    #[test]
    fn completed_long_mission_continue_routes_to_next_mission_phase() {
        let task_store = Arc::new(TaskStore::new());
        let state = test_state().with_task_store(task_store.clone());
        let session_id = SessionId::new("session-completed-long-mission-followup");
        let mission_id = MissionId::new("mission-completed-long-mission-followup");
        let root_task =
            completed_long_mission_root_task("task-completed-long-mission-followup", &mission_id);
        let now = UtcMillis::now();
        task_store.insert_task(root_task.clone());
        state
            .session_store
            .create_session(session_id.clone(), "复杂任务")
            .expect("session should create");
        state
            .session_store
            .ensure_session_mission(&session_id, now, || mission_id.clone());
        state
            .session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                completed_long_mission_chain(&session_id, &mission_id, &root_task.task_id, now),
            )
            .expect("active chain should persist");

        let mut request = session_turn_request("继续下一阶段，推进剩余验证");
        request.session_id = Some(session_id.to_string());
        let decision = decide_session_turn_with_task_planner(&state, &request)
            .expect("completed long mission continuation should route");

        assert!(matches!(decision.route, SessionTurnRouteDto::Task));
        assert_eq!(decision.task_tier, TaskTier::LongMission);
        assert_eq!(
            decision.reason_code.as_deref(),
            Some("next_mission_phase_requested")
        );
        assert_eq!(
            decision.execution_goal.as_deref(),
            Some("继续下一阶段，推进剩余验证")
        );
    }

    #[tokio::test]
    async fn completed_long_mission_followup_dispatch_reuses_mission() {
        let task_store = Arc::new(TaskStore::new());
        let state = test_state().with_task_store(task_store.clone());
        let workspace_id = register_workspace(
            &state,
            "workspace-long-mission-next-chain",
            "long-mission-next-chain",
        );
        let session_id = SessionId::new("session-long-mission-next-chain");
        let mission_id = MissionId::new("mission-long-mission-next-chain");
        let root_task =
            completed_long_mission_root_task("task-long-mission-finished-chain", &mission_id);
        let now = UtcMillis::now();
        task_store.insert_task(root_task.clone());
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "复杂任务",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        state
            .session_store
            .ensure_session_mission(&session_id, now, || mission_id.clone());
        state
            .session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                completed_long_mission_chain(&session_id, &mission_id, &root_task.task_id, now),
            )
            .expect("active chain should persist");

        let mut request = session_turn_request("继续下一阶段，派发代理完成剩余检查");
        request.session_id = Some(session_id.to_string());
        request.workspace_id = Some(workspace_id.to_string());
        let decision = decide_session_turn_with_task_planner(&state, &request)
            .expect("followup should route to next mission phase");
        let (accepted, _) = super::super::accept_session_task_submission(
            &state,
            &request,
            workspace_id,
            decision.task_title.clone(),
            decision.execution_goal.clone(),
            decision.task_tier,
        )
        .await
        .expect("followup dispatch should be accepted");

        assert_ne!(accepted.root_task_id, root_task.task_id);
        let next_root = task_store
            .get_task(&accepted.root_task_id)
            .expect("next phase root task should exist");
        assert_eq!(next_root.mission_id, mission_id);
        assert_eq!(
            next_root
                .policy_snapshot
                .as_ref()
                .map(|policy| policy.task_tier),
            Some(TaskTier::LongMission)
        );
    }

    #[test]
    fn continue_user_message_opens_new_running_turn_after_terminal_turn() {
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
        state
            .session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-old-failed".to_string(),
                    turn_seq: 1,
                    accepted_at: now,
                    status: "failed".to_string(),
                    completed_at: Some(now),
                    user_message: Some("旧任务".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("failed turn should persist");

        let accepted = SessionContinueAccepted {
            session_id: session_id.clone(),
            mission_id,
            root_task_id,
            action_task_id: action_task_id.clone(),
            execution_chain_ref: "chain-continue-new-turn".to_string(),
            resumed_branch_count: 1,
            runner_started: true,
        };
        let (_, user_item_id) = write_continue_user_message(
            &state,
            &accepted,
            Some("继续推进"),
            UtcMillis(now.0 + 1),
            None,
            None,
            None,
            magi_core::ThreadId::new("thread-orchestrator-continue-new-turn"),
        )
        .expect("continue message should write");

        let turn = state
            .session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("current turn should exist");
        assert_eq!(turn.status, "running");
        assert_ne!(turn.turn_id, "turn-old-failed");
        assert_eq!(turn.user_message.as_deref(), Some("继续推进"));
        assert_eq!(turn.items.len(), 1);
        assert_eq!(turn.items[0].task_id.as_ref(), Some(&action_task_id));
        assert_eq!(
            user_item_id.as_deref(),
            Some(turn.items[0].item_id.as_str())
        );
    }

    #[tokio::test]
    async fn supplement_context_turn_enqueues_followup_mailbox_signal() {
        let task_store = Arc::new(TaskStore::new());
        let state = test_state().with_task_store(task_store.clone());
        let workspace_id =
            register_workspace(&state, "workspace-supplement-mailbox", "supplement-mailbox");
        let session_id = SessionId::new("session-supplement-mailbox");
        let mission_id = MissionId::new("mission-supplement-mailbox");
        let root_task = test_root_task("task-root-supplement", mission_id.as_str());
        task_store.insert_task(root_task.clone());
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "Supplement Mailbox",
                Some(workspace_id.to_string()),
            )
            .expect("session should be creatable");
        state.session_store.bind_execution_ownership(
            session_id.clone(),
            ExecutionOwnership {
                session_id: Some(session_id.clone()),
                workspace_id: Some(workspace_id.clone()),
                mission_id: Some(mission_id.clone()),
                task_id: Some(root_task.task_id.clone()),
                execution_chain_ref: Some("chain-supplement-mailbox".to_string()),
                ..ExecutionOwnership::default()
            },
        );
        let (_, orchestrator_thread_id) =
            state
                .session_store
                .ensure_session_mission(&session_id, UtcMillis::now(), || mission_id.clone());

        let mut request = session_turn_request("请优先处理这个 followup");
        request.session_id = Some(session_id.to_string());
        request.workspace_id = Some(workspace_id.to_string());
        request.supplement_context = true;
        let response =
            submit_supplement_context_turn(&state, &request, &workspace_id, UtcMillis::now())
                .await
                .expect("supplement should enqueue mailbox signal");

        assert_eq!(response.route, SessionTurnRouteDto::SupplementContext);
        assert!(
            response
                .signal_ref
                .as_deref()
                .is_some_and(|id| id.starts_with("mailbox-signal-"))
        );
        assert_eq!(
            response.target_task_id.as_deref(),
            Some(root_task.task_id.as_str())
        );

        let pending = state
            .conversation_registry
            .conversation_for_task(&session_id, &root_task.task_id)
            .lock()
            .expect("task conversation lock")
            .drain_mailbox_items();
        assert_eq!(pending.len(), 1);
        match &pending[0] {
            magi_conversation_runtime::MailboxItem::Runtime(signal) => {
                assert_eq!(signal.author, MailboxAuthor::User);
                assert_eq!(signal.kind, MailboxKind::Followup);
                assert!(signal.trigger_turn);
                assert_eq!(
                    signal.payload["text"].as_str(),
                    Some("请优先处理这个 followup")
                );
            }
            magi_conversation_runtime::MailboxItem::User(_) => {
                panic!("supplement must enqueue a runtime followup signal")
            }
        }

        let history = state
            .session_store
            .thread_message_history(&orchestrator_thread_id);
        assert_eq!(history.len(), 1);
        assert!(
            history[0]
                .content
                .as_deref()
                .unwrap_or_default()
                .contains("kind=followup")
        );
    }

    #[test]
    fn keeps_plain_diagram_explanation_as_chat() {
        let request = session_turn_request("解释一下流程图是什么");
        let decision = normalize_session_turn_decision(classifier_chat_decision(), &request);

        assert!(matches!(decision.route, SessionTurnRouteDto::Chat));
        assert_eq!(decision.task_tier, TaskTier::ExecutionChain);
        assert!(decision.forced_tool_name.is_none());
    }

    #[test]
    fn current_project_analysis_routes_to_execute_tools() {
        let request = session_turn_request("分析当前项目");
        let decision = normalize_session_turn_decision(classifier_chat_decision(), &request);

        assert!(matches!(decision.route, SessionTurnRouteDto::Execute));
        assert!(decision.tool_intent.is_some());
        assert!(decision.forced_tool_name.is_none());
    }

    #[test]
    fn project_identity_question_routes_to_execute_tools() {
        let request = session_turn_request("这个项目是什么");
        let decision = normalize_session_turn_decision(classifier_chat_decision(), &request);

        assert!(matches!(decision.route, SessionTurnRouteDto::Execute));
        assert!(decision.tool_intent.is_some());
        assert!(decision.forced_tool_name.is_none());
    }

    #[test]
    fn normalizes_explicit_public_builtin_tool_to_forced_execution() {
        let request = session_turn_request("请只调用 file_mkdir 工具创建目录");
        let decision = normalize_session_turn_decision(classifier_chat_decision(), &request);

        assert!(matches!(decision.route, SessionTurnRouteDto::Execute));
        assert_eq!(decision.forced_tool_name.as_deref(), Some("file_mkdir"));
        let tool_intent = decision.tool_intent.as_deref().unwrap_or_default();
        assert!(tool_intent.contains("file_mkdir"));
        assert!(tool_intent.contains("不要只输出文字说明"));
    }

    #[test]
    fn normalizes_multi_builtin_tool_chain_to_required_execution_chain() {
        let request = session_turn_request(
            "请依次调用 file_write、file_read、file_patch、diff_preview 和 diagram_render 完成验收",
        );
        let decision = normalize_session_turn_decision(classifier_chat_decision(), &request);

        assert!(matches!(decision.route, SessionTurnRouteDto::Execute));
        assert!(decision.forced_tool_name.is_none());
        assert_eq!(
            decision.required_tool_chain,
            vec![
                "file_write".to_string(),
                "file_read".to_string(),
                "file_patch".to_string(),
                "diff_preview".to_string(),
                "diagram_render".to_string()
            ]
        );
        let tool_intent = decision.tool_intent.as_deref().unwrap_or_default();
        assert!(tool_intent.contains("多个公开内置工具"));
        assert!(tool_intent.contains("file_write"));
        assert!(tool_intent.contains("file_read"));
        assert!(tool_intent.contains("file_patch"));
        assert!(tool_intent.contains("diff_preview"));
        assert!(tool_intent.contains("diagram_render"));
        assert!(tool_intent.contains("对应编号步骤原文提取"));
        assert!(tool_intent.contains("禁止改名为 probe"));
    }

    #[test]
    fn normalizes_public_builtin_alias_to_canonical_tool() {
        let request = session_turn_request("请调用 file_view 工具查看 /tmp/a.txt");
        let decision = normalize_session_turn_decision(classifier_chat_decision(), &request);

        assert!(matches!(decision.route, SessionTurnRouteDto::Execute));
        assert_eq!(decision.forced_tool_name.as_deref(), Some("file_read"));
    }

    #[test]
    fn explicit_public_tool_with_fix_word_routes_to_execute_not_task() {
        let state = test_state();
        let request = session_turn_request("请调用 file_patch 修复 /tmp/a.txt 中的拼写问题");

        let decision = decide_session_turn_with_task_planner(&state, &request)
            .expect("explicit public tool should route locally");

        assert!(matches!(decision.route, SessionTurnRouteDto::Execute));
        assert_eq!(decision.forced_tool_name.as_deref(), Some("file_patch"));
        assert!(decision.task_evidence.is_empty());
    }

    #[test]
    fn simple_one_shot_fix_routes_to_execute_without_task_projection() {
        let state = test_state();
        let request = session_turn_request("直接修复这个文件里的错别字，不需要创建任务");

        let decision = decide_session_turn_with_task_planner(&state, &request)
            .expect("simple one-shot work should route locally");

        assert!(matches!(decision.route, SessionTurnRouteDto::Execute));
        assert!(decision.execution_goal.is_none());
        assert!(decision.task_evidence.is_empty());
    }

    #[test]
    fn medium_fix_with_validation_stays_structured_task() {
        let state = test_state();
        let request = session_turn_request("修复登录流程问题，完成后运行测试并汇总验证结果");

        let decision = decide_session_turn_with_task_planner(&state, &request)
            .expect("medium fix should route locally");

        assert!(matches!(decision.route, SessionTurnRouteDto::Task));
        assert_eq!(decision.task_tier, TaskTier::ExecutionChain);
        assert!(decision.execution_goal.is_some());
        assert!(!decision.task_evidence.is_empty());
    }

    #[test]
    fn explicit_complex_agent_request_is_task_even_when_shell_tool_is_named() {
        let request = session_turn_request(
            "请以复杂任务模式完成，代理必须调用 shell_exec 执行 printf ok，最后总结。",
        );
        let decision = normalize_session_turn_decision(classifier_chat_decision(), &request);

        assert!(matches!(decision.route, SessionTurnRouteDto::Task));
        assert!(decision.forced_tool_name.is_none());
        assert!(decision.required_tool_chain.is_empty());
        assert_eq!(
            decision.reason_code.as_deref(),
            Some("explicit_task_request")
        );
        assert_eq!(decision.task_tier, TaskTier::LongMission);
        assert!(decision.execution_goal.is_some());
        assert!(!decision.task_evidence.is_empty());
    }

    #[test]
    fn explicit_long_mission_request_is_task_even_when_execute_words_are_present() {
        let request = session_turn_request(
            "以复杂长期任务 LongMission 模式执行稳定性验收，按步骤读取配置、派发代理并创建 checkpoint。",
        );
        let decision = normalize_session_turn_decision(classifier_chat_decision(), &request);

        assert!(matches!(decision.route, SessionTurnRouteDto::Task));
        assert!(decision.forced_tool_name.is_none());
        assert!(decision.required_tool_chain.is_empty());
        assert_eq!(decision.task_tier, TaskTier::LongMission);
        assert_eq!(
            decision.reason_code.as_deref(),
            Some("explicit_task_request")
        );
        assert_eq!(
            decision.execution_goal.as_deref(),
            Some(
                "以复杂长期任务 LongMission 模式执行稳定性验收，按步骤读取配置、派发代理并创建 checkpoint。"
            )
        );
    }

    #[test]
    fn explicit_medium_agent_dispatch_request_is_task_even_when_read_words_are_present() {
        let request = session_turn_request(
            "请作为中等任务进行角色匹配冒烟：同时派发两个只读代理，explorer display_name「角色目录代理」只做根目录巡检；reviewer display_name「角色配置代理」只读取 package.json。",
        );
        let decision = normalize_session_turn_decision(classifier_chat_decision(), &request);

        assert!(matches!(decision.route, SessionTurnRouteDto::Task));
        assert_eq!(decision.task_tier, TaskTier::ExecutionChain);
        assert!(decision.forced_tool_name.is_none());
        assert!(decision.required_tool_chain.is_empty());
        assert_eq!(
            decision.reason_code.as_deref(),
            Some("explicit_task_request")
        );
    }

    #[test]
    fn explicit_agent_request_uses_execution_chain_without_long_mission() {
        let request = session_turn_request("请分派代理修复这个明确问题，完成后汇总验证结果。");
        let decision = normalize_session_turn_decision(classifier_chat_decision(), &request);

        assert!(matches!(decision.route, SessionTurnRouteDto::Task));
        assert_eq!(decision.task_tier, TaskTier::ExecutionChain);
    }

    #[test]
    fn explicit_complex_agent_request_preserves_raw_user_goal_as_execution_goal() {
        let raw_goal = "【具体任务推进验收】请以复杂任务模式完成，必须由代理在当前工作区创建文件 task-system-e2e.md，文件内容必须包含三行：title: task concrete progress、marker: TASK_E2E、status: completed。创建后代理必须读取该文件验证内容。";
        let request = session_turn_request(raw_goal);
        let mut classifier_decision = classifier_chat_decision();
        classifier_decision.route = SessionTurnRouteDto::Task;
        classifier_decision.task_title = Some("创建并验证 task-system-e2e.md".to_string());
        classifier_decision.execution_goal = Some("创建并验证 task-system-e2e.md".to_string());
        classifier_decision
            .task_evidence
            .push("classifier saw a task".to_string());

        let decision = normalize_session_turn_decision(classifier_decision, &request);

        assert!(matches!(decision.route, SessionTurnRouteDto::Task));
        assert_eq!(decision.task_tier, TaskTier::LongMission);
        assert_eq!(decision.execution_goal.as_deref(), Some(raw_goal));
    }

    #[test]
    fn does_not_force_internal_builtin_tool_names() {
        let request = session_turn_request("请调用 process_launch 启动后台进程");
        let decision = normalize_session_turn_decision(classifier_chat_decision(), &request);

        assert!(matches!(decision.route, SessionTurnRouteDto::Chat));
        assert!(decision.forced_tool_name.is_none());
    }

    #[test]
    fn does_not_treat_substrings_as_explicit_tool_names() {
        let request = session_turn_request("profile_mkdir 是一个普通变量名");
        let decision = normalize_session_turn_decision(classifier_chat_decision(), &request);

        assert!(matches!(decision.route, SessionTurnRouteDto::Chat));
        assert!(decision.forced_tool_name.is_none());
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

    /// §P7：accept 阶段不再为 assistant 预占 turn item，避免 thinking → text
    /// 在 Anthropic 协议顺序下被错误抢到更小的 `item_seq`，导致 presentationSeq
    /// 与协议顺序倒挂、流式计时器跳到消息上方。
    ///
    /// 新契约：
    /// - canonical_turn.items 在 accept 时只包含 user_message（不再预先 push 占位）
    /// - placeholder_message_id 仍透传给下游（用于首帧 upsert 时复用 item_id）
    /// - canonical_item 此时为空，前端 normalize 兼容
    #[tokio::test]
    async fn regular_session_turn_accept_does_not_pre_reserve_assistant_placeholder_item() {
        let state = test_state();
        let workspace_id = register_workspace(
            &state,
            "workspace-canonical-first-frame",
            "canonical-first-frame",
        );
        let accepted_at = UtcMillis(1777000000000);
        let response = submit_regular_session_turn(
            state.clone(),
            SessionTurnRequestDto {
                session_id: None,
                workspace_id: Some(workspace_id.to_string()),
                text: Some("请只回复一句话".to_string()),
                skill_name: None,
                images: Vec::new(),
                access_profile: None,
                request_id: Some("request-canonical-first-frame".to_string()),
                user_message_id: Some("user-canonical-first-frame".to_string()),
                placeholder_message_id: Some("assistant-canonical-first-frame".to_string()),
                supplement_context: false,
                target_task_id: None,
            },
            workspace_id,
            accepted_at,
            SessionTurnIntentDecision {
                route: SessionTurnRouteDto::Chat,
                task_title: None,
                execution_goal: None,
                task_tier: TaskTier::ExecutionChain,
                tool_intent: None,
                forced_tool_name: None,
                required_tool_chain: Vec::new(),
                confidence: 1.0,
                reason_code: Some("plain_chat".to_string()),
                route_reason: Some("test".to_string()),
                task_evidence: Vec::new(),
            },
        )
        .await
        .expect("regular turn should be accepted");

        assert_eq!(
            response.user_message_item_id.as_deref(),
            Some("user-canonical-first-frame")
        );
        let accepted_event = state
            .event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_id.to_string() == response.event_id)
            .expect("accepted event should be published");
        assert_eq!(
            accepted_event.payload["canonical_event_kind"],
            "turn_started"
        );
        let items = accepted_event.payload["canonical_turn"]["items"]
            .as_array()
            .expect("canonical turn items should be present");
        // accept 阶段 turn 内只有 user_message；assistant item 由首帧 upsert 创建。
        assert_eq!(items.len(), 1, "accept 阶段不应预占 assistant item");
        assert_eq!(items[0]["itemId"], "user-canonical-first-frame");
        assert_eq!(items[0]["kind"], "user_message");
        // canonical_item 此时找不到（item 尚未创建），载荷为 null。
        assert!(
            accepted_event.payload["canonical_item"].is_null(),
            "canonical_item 在 accept 阶段应为空，等待首帧创建"
        );
    }

    /// §3.1 端到端验收：simple task 路径（chat 路由）
    ///
    /// 闸门属性：chat 路由进入 `submit_regular_session_turn` 后，
    /// - 不在 TaskStore 创建任何 Task；
    /// - 不在 session sidecar 写入 `execution_chain_ref`；
    /// - session timeline 仍由现有 canonical turn 测试覆盖 user_message + assistant 占位。
    ///
    /// 任何在 chat 路径上私自落入 TaskStore / 绑定 execution chain 的代码改动
    /// 都会让本测试失败——这是 §3.1 "不创建 TaskGraph、不创建 Mission" 的可观察行为闸门。
    #[tokio::test]
    async fn simple_chat_route_does_not_create_task_or_execution_chain() {
        let task_store = Arc::new(TaskStore::new());
        let state = test_state().with_task_store(task_store.clone());
        let workspace_id = register_workspace(&state, "workspace-simple-chat", "simple-chat");
        let request = SessionTurnRequestDto {
            session_id: None,
            workspace_id: Some(workspace_id.to_string()),
            text: Some("解释一下流程图的概念".to_string()),
            skill_name: None,
            images: Vec::new(),
            access_profile: None,
            request_id: Some("request-simple-chat".to_string()),
            user_message_id: Some("user-simple-chat".to_string()),
            placeholder_message_id: Some("assistant-simple-chat".to_string()),
            supplement_context: false,
            target_task_id: None,
        };
        let accepted_at = UtcMillis(1_700_000_000_000);
        let response = submit_regular_session_turn(
            state.clone(),
            request,
            workspace_id,
            accepted_at,
            classifier_chat_decision(),
        )
        .await
        .expect("chat route should accept");

        assert!(matches!(response.route, SessionTurnRouteDto::Chat));
        let session_id = SessionId::new(&response.session_id);
        let orchestrator_thread = state
            .session_store
            .orchestrator_thread_for_session(&session_id)
            .expect("orchestrator thread should be registered");
        let tasks = task_store.get_tasks_by_mission(&orchestrator_thread.mission_id);
        assert!(
            tasks.is_empty(),
            "chat 路由不应在 TaskStore 中创建任务: {tasks:?}"
        );
        let sidecar = state
            .session_store
            .runtime_sidecar(&session_id)
            .expect("sidecar should exist");
        assert!(
            sidecar.ownership.execution_chain_ref.is_none(),
            "chat 路由不应绑定 execution_chain_ref: {:?}",
            sidecar.ownership.execution_chain_ref,
        );
    }

    /// §3.1 端到端验收：simple task 路径（execute 路由强制工具）
    ///
    /// 与 chat 路由共用 `submit_regular_session_turn`，因此同样的"不入 TaskStore /
    /// 不绑定 execution chain"闸门必须成立——execute 路由的语义是"在主线程内调用
    /// 一次工具"，而不是创建任务投影。
    #[tokio::test]
    async fn simple_execute_route_does_not_create_task_or_execution_chain() {
        let task_store = Arc::new(TaskStore::new());
        let state = test_state().with_task_store(task_store.clone());
        let workspace_id = register_workspace(&state, "workspace-simple-execute", "simple-execute");
        let request = SessionTurnRequestDto {
            session_id: None,
            workspace_id: Some(workspace_id.to_string()),
            text: Some("请调用 file_mkdir 工具创建目录".to_string()),
            skill_name: None,
            images: Vec::new(),
            access_profile: None,
            request_id: Some("request-simple-execute".to_string()),
            user_message_id: Some("user-simple-execute".to_string()),
            placeholder_message_id: Some("assistant-simple-execute".to_string()),
            supplement_context: false,
            target_task_id: None,
        };
        let accepted_at = UtcMillis(1_700_000_001_000);
        let mut decision = classifier_chat_decision();
        decision.route = SessionTurnRouteDto::Execute;
        decision.forced_tool_name = Some("file_mkdir".to_string());
        decision.tool_intent = Some("显式调用 file_mkdir".to_string());
        let response = submit_regular_session_turn(
            state.clone(),
            request,
            workspace_id,
            accepted_at,
            decision,
        )
        .await
        .expect("execute route should accept");

        assert!(matches!(response.route, SessionTurnRouteDto::Execute));
        let session_id = SessionId::new(&response.session_id);
        let orchestrator_thread = state
            .session_store
            .orchestrator_thread_for_session(&session_id)
            .expect("orchestrator thread should be registered");
        let tasks = task_store.get_tasks_by_mission(&orchestrator_thread.mission_id);
        assert!(
            tasks.is_empty(),
            "execute 路由不应在 TaskStore 中创建任务: {tasks:?}"
        );
        let sidecar = state
            .session_store
            .runtime_sidecar(&session_id)
            .expect("sidecar should exist");
        assert!(
            sidecar.ownership.execution_chain_ref.is_none(),
            "execute 路由不应绑定 execution_chain_ref: {:?}",
            sidecar.ownership.execution_chain_ref,
        );
    }

    #[tokio::test]
    async fn delete_session_returns_workspace_scoped_bootstrap() {
        let state = test_state();
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
        let selected_root = unique_temp_dir("session-save-scoped-a");
        let foreign_root = unique_temp_dir("session-save-scoped-b");
        state
            .workspace_registry
            .register(
                WorkspaceId::new("workspace-a"),
                AbsolutePath::new(selected_root.display().to_string()),
            )
            .expect("workspace a should register");
        state
            .workspace_registry
            .register(
                WorkspaceId::new("workspace-b"),
                AbsolutePath::new(foreign_root.display().to_string()),
            )
            .expect("workspace b should register");
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
        // bootstrap 现在按"会话是否有用户消息"过滤——补一条用户消息让 selected_session_id 在响应中可见
        state.session_store.append_timeline_entry(
            selected_session_id.clone(),
            TimelineEntryKind::UserMessage,
            "hello",
        );
        state
            .snapshot_manager
            .start_session(selected_session_id.as_str().to_string(), selected_root)
            .await
            .expect("snapshot session should start");

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
        register_workspace(&state, "workspace-a", "save-mismatch-a");
        register_workspace(&state, "workspace-b", "save-mismatch-b");
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
    async fn notifications_require_explicit_workspace_and_session_scope() {
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
        state.session_store.append_notification(
            session_id.clone(),
            "notification-explicit-scope",
            "incident",
            "必须显式指定 workspace 和 session",
        );

        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/session/notifications?sessionId={}",
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
                    .uri("/session/notifications?workspaceId=workspace-a")
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
                .contains("sessionId 不能为空"),
            "unexpected body: {body}"
        );

        let (status, body) = post_json(
            state,
            "/session/notifications/mark-all-read",
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
        state.session_store.append_notification(
            session_id.clone(),
            "notification-owned-current",
            "incident",
            "应按 execution ownership 归属加载",
        );

        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/session/notifications?workspaceId=workspace-owned-notifications&sessionId={}",
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
    async fn mark_all_notifications_read_rejects_unregistered_workspace_scope() {
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
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/session/notifications/mark-all-read")
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
        assert_eq!(
            state.session_store.notifications_for_session(&session_id)[0].handled,
            false
        );

        let _ = fs::remove_dir_all(persistence_root);
    }
}
