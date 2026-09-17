use magi_core::{
    BrowserSessionId, BrowserTabId, CollaborationMode, EventId, SessionId, TaskCompletionContract,
    TaskRecoveryCheckpoint, TaskStatus, TaskTier, UtcMillis, WorkspaceId, public_runtime_excerpt,
};
use magi_event_bus::{EventContext, EventEnvelope};
use serde_json::json;

use super::{
    browser::resolve_browser_annotation_context, monotonic_accepted_at, new_session_id,
    session_scope::resolve_session_workspace_binding,
};
use crate::{
    dto::{SessionDirectoryEntryDto, SessionTurnRequestDto},
    errors::ApiError,
    performance::PerformanceTrace,
    state::ApiState,
    task_dispatch::{
        DispatchSubmissionAccepted, DispatchSubmissionRequest, DispatchTurnOrigin,
        drive_dispatch_submission_after_lifecycle_and_restart_lock,
        materialize_dispatch_submission_after_acceptance, submit_dispatch_submission,
    },
};
use magi_browser_authority::ValidateBrowserNodeSelection;
use magi_conversation_runtime::dispatch_submission::cleanup_materialized_dispatch_submission_if_not_started;
use magi_conversation_runtime::dispatch_submission::recover_dispatch_submission_request;
use magi_conversation_runtime::session_images::SessionTurnImage;
use magi_conversation_runtime::session_writeback::publish_current_session_turn_item_event;
use magi_session_store::{
    CANONICAL_TURN_SCHEMA_VERSION, CanonicalTurn, CanonicalTurnItem, CanonicalTurnItemKind,
    SessionGoal,
};

pub(super) fn session_turn_route_name(route: crate::dto::SessionTurnRouteDto) -> &'static str {
    match route {
        crate::dto::SessionTurnRouteDto::Chat => "chat",
        crate::dto::SessionTurnRouteDto::Execute => "execute",
        crate::dto::SessionTurnRouteDto::Task => "task",
        crate::dto::SessionTurnRouteDto::Continue => "continue",
        crate::dto::SessionTurnRouteDto::Steer => "steer",
    }
}

pub(super) async fn accept_session_task_submission(
    state: &ApiState,
    request: &SessionTurnRequestDto,
    images: Vec<SessionTurnImage>,
    workspace_id: Option<WorkspaceId>,
    task_title: Option<String>,
    execution_goal: Option<String>,
    task_tier: TaskTier,
) -> Result<(DispatchSubmissionAccepted, EventId), ApiError> {
    accept_session_task_submission_at(
        state,
        request,
        SessionTaskSubmissionInput {
            images,
            workspace_id,
            task_title,
            execution_goal,
            task_tier,
            collaboration_mode: CollaborationMode::Auto,
            use_tools: true,
            accepted_at: monotonic_accepted_at(),
            required_tool_chain: Vec::new(),
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: None,
            denied_tools: Vec::new(),
            user_message_metadata: Default::default(),
        },
    )
    .await
    .map(|(accepted, event_id, _event_seq, _occurred_at)| (accepted, event_id))
}

pub(super) struct SessionTaskSubmissionInput {
    pub images: Vec<SessionTurnImage>,
    pub workspace_id: Option<WorkspaceId>,
    pub task_title: Option<String>,
    pub execution_goal: Option<String>,
    pub task_tier: TaskTier,
    pub collaboration_mode: CollaborationMode,
    pub use_tools: bool,
    pub accepted_at: UtcMillis,
    pub required_tool_chain: Vec<String>,
    pub completion_contract: TaskCompletionContract,
    pub recovery_checkpoint: Option<TaskRecoveryCheckpoint>,
    pub denied_tools: Vec<String>,
    /// API 语义元数据与幂等指纹一起原子写入 canonical user item。
    pub user_message_metadata: std::collections::HashMap<String, serde_json::Value>,
}

pub(super) async fn accept_session_task_submission_at(
    state: &ApiState,
    request: &SessionTurnRequestDto,
    input: SessionTaskSubmissionInput,
) -> Result<(DispatchSubmissionAccepted, EventId, u64, UtcMillis), ApiError> {
    let SessionTaskSubmissionInput {
        images,
        workspace_id,
        task_title,
        execution_goal,
        task_tier,
        collaboration_mode,
        use_tools,
        accepted_at,
        required_tool_chain,
        completion_contract,
        recovery_checkpoint,
        denied_tools,
        mut user_message_metadata,
    } = input;
    let request_fingerprint = request
        .request_fingerprint()
        .map_err(ApiError::InvalidInput)?;
    let trace_id = request
        .request_id()
        .unwrap_or_else(|| format!("trace-turn-{}", accepted_at.0));
    user_message_metadata.insert(
        "requestFingerprint".to_string(),
        serde_json::Value::String(request_fingerprint),
    );
    user_message_metadata.insert("traceId".to_string(), serde_json::Value::String(trace_id));
    let trimmed_text = request.trimmed_text();
    let message = request.timeline_message(trimmed_text.as_deref());
    let mission_title = task_title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| request.mission_title(trimmed_text.as_deref()));
    let execution_goal = execution_goal
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    execute_dispatch_submission(
        state,
        ExecuteDispatchSubmissionInput {
            requested_session_id: request.requested_session_id(),
            requested_workspace_id: workspace_id,
            mission_title,
            message,
            trimmed_text,
            execution_goal,
            task_tier,
            collaboration_mode,
            use_tools,
            skill_name: request.skill_name.clone(),
            target_role: None,
            images,
            request,
            accepted_at,
            required_tool_chain,
            completion_contract,
            recovery_checkpoint,
            denied_tools,
            user_message_metadata,
        },
    )
    .await
}

/// Goal 自动续跑同样提交 root coordinator task，但不伪造用户消息。
///
/// 它和普通主线 Turn 共用 DispatchSubmission / Runner / ConversationLoop；仅以
/// `DispatchTurnOrigin::GoalContinuation` 让 canonical 时间线写入系统通知，并保证
/// 当前 Turn 不出现虚假的 user_message。
pub(super) async fn accept_goal_continuation_task_submission(
    state: &ApiState,
    session_id: SessionId,
    workspace_id: Option<WorkspaceId>,
    goal: &SessionGoal,
    execution_goal: String,
    accepted_at: UtcMillis,
) -> Result<DispatchSubmissionAccepted, ApiError> {
    let execution_root = if workspace_id.is_none() {
        Some(state.personal_session_execution_root_path(&session_id))
    } else {
        None
    };
    let entry_id = format!(
        "timeline-goal-continuation-{}-{}",
        session_id, accepted_at.0
    );
    let dispatch = DispatchSubmissionRequest {
        accepted_at,
        session_id: session_id.clone(),
        workspace_id,
        execution_root,
        orchestrator_session_config: None,
        entry_id,
        timeline_message: format!("目标自动推进: {}", goal.objective),
        images: Vec::new(),
        context_references: Vec::new(),
        browser_annotation_refs: Vec::new(),
        browser_node_selections: Vec::new(),
        created_session: false,
        mission_title: "目标自动推进".to_string(),
        task_title: "执行: 目标自动推进".to_string(),
        trimmed_text: None,
        execution_goal: Some(execution_goal),
        task_tier: TaskTier::ExecutionChain,
        collaboration_mode: CollaborationMode::Auto,
        access_profile: goal.access_profile,
        skill_name: None,
        goal_mode: true,
        use_tools: true,
        target_role: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        replace_turn_id: None,
        required_tool_chain: vec!["get_goal".to_string()],
        completion_contract: TaskCompletionContract::default(),
        recovery_checkpoint: None,
        denied_tools: Vec::new(),
        user_message_metadata: Default::default(),
        turn_origin: DispatchTurnOrigin::GoalContinuation(goal.goal_id.clone()),
    };
    let accepted = submit_dispatch_submission(state, dispatch)?;
    Ok(accepted)
}

struct ExecuteDispatchSubmissionInput<'a> {
    requested_session_id: Option<SessionId>,
    requested_workspace_id: Option<WorkspaceId>,
    mission_title: String,
    message: String,
    trimmed_text: Option<String>,
    execution_goal: Option<String>,
    task_tier: TaskTier,
    collaboration_mode: CollaborationMode,
    use_tools: bool,
    skill_name: Option<String>,
    target_role: Option<String>,
    images: Vec<SessionTurnImage>,
    request: &'a SessionTurnRequestDto,
    accepted_at: UtcMillis,
    required_tool_chain: Vec<String>,
    completion_contract: TaskCompletionContract,
    recovery_checkpoint: Option<TaskRecoveryCheckpoint>,
    denied_tools: Vec<String>,
    user_message_metadata: std::collections::HashMap<String, serde_json::Value>,
}

pub(super) fn initial_session_orchestrator_config(
    state: &ApiState,
    created_session: bool,
    config: Option<&serde_json::Value>,
) -> Result<Option<serde_json::Value>, ApiError> {
    if !created_session && config.is_some() {
        return Err(ApiError::InvalidInput(
            "已有会话的模型只能通过会话模型设置操作修改".to_string(),
        ));
    }
    if !created_session {
        return Ok(None);
    }
    let mut initial = super::settings::orchestrator_session_defaults(state);
    let has_default_model = initial
        .get("model")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    if config.is_none() && !has_default_model {
        return Ok(None);
    }
    if let Some(config) = config {
        let explicit = super::settings::orchestrator_session_override_request(
            &serde_json::json!({ "config": config }),
        )?;
        magi_conversation_runtime::model_config::merge_orchestrator_session_override(
            &mut initial,
            &explicit,
        );
    }
    Ok(Some(initial))
}

async fn execute_dispatch_submission(
    state: &ApiState,
    input: ExecuteDispatchSubmissionInput<'_>,
) -> Result<(DispatchSubmissionAccepted, EventId, u64, UtcMillis), ApiError> {
    let ExecuteDispatchSubmissionInput {
        requested_session_id,
        requested_workspace_id,
        mission_title,
        message,
        trimmed_text,
        execution_goal,
        task_tier,
        collaboration_mode,
        use_tools,
        skill_name,
        target_role,
        images,
        request,
        accepted_at,
        required_tool_chain,
        completion_contract,
        recovery_checkpoint,
        denied_tools,
        user_message_metadata,
    } = input;
    let placeholder_title = crate::session_title::NEW_SESSION_PLACEHOLDER_TITLE;
    let previous_current_session_id = state.session_store.current_session_id();
    let (session_id, created_session, workspace_id) = resolve_dispatch_session(
        state,
        requested_session_id,
        requested_workspace_id,
        placeholder_title,
        accepted_at,
    )?;
    let execution_root = workspace_id
        .as_ref()
        .and_then(|workspace_id| state.workspace_root_path(&Some(workspace_id.clone())))
        .or(if workspace_id.is_none() {
            Some(state.personal_session_execution_root_path(&session_id))
        } else {
            None
        });
    let browser_annotation_refs = match resolve_browser_annotation_context(
        state,
        &session_id,
        &request.browser_annotation_refs(),
    ) {
        Ok(refs) => refs,
        Err(error) if created_session => {
            return Err(rollback_created_session_on_dispatch_error(
                state,
                &session_id,
                previous_current_session_id.clone(),
                error,
            )
            .await);
        }
        Err(error) => return Err(error),
    };
    let browser_node_selection_result = {
        let browser_authority = state
            .browser_authority
            .lock()
            .expect("browser authority lock poisoned");
        request.validate_browser_node_selections_with(|index, selection| {
            let browser_session_id = BrowserSessionId::new(selection.browser_session_id.clone());
            let tab_id = BrowserTabId::new(selection.tab_id.clone());
            browser_authority
                .validate_browser_node_selection(ValidateBrowserNodeSelection {
                    session_id: &session_id,
                    browser_session_id: &browser_session_id,
                    tab_id: &tab_id,
                    surface_id: selection.surface_id.trim(),
                    navigation_revision: selection.navigation_revision,
                    page_url: selection.url.trim(),
                })
                .map_err(|error| format!("浏览器节点选择[{index}] 无效: {error}"))
        })
    };
    let browser_node_selections = match browser_node_selection_result {
        Ok(selections) => selections,
        Err(error) if created_session => {
            return Err(rollback_created_session_on_dispatch_error(
                state,
                &session_id,
                previous_current_session_id.clone(),
                ApiError::InvalidInput(error),
            )
            .await);
        }
        Err(error) => return Err(ApiError::InvalidInput(error)),
    };
    let user_timeline_entry_id = format!("timeline-{}-{}", session_id, accepted_at.0);
    let action_task_title = format_action_task_title(&mission_title);

    let dispatch = DispatchSubmissionRequest {
        accepted_at,
        session_id: session_id.clone(),
        workspace_id: workspace_id.clone(),
        execution_root,
        orchestrator_session_config: request.orchestrator_session_config.clone(),
        entry_id: user_timeline_entry_id,
        timeline_message: message.clone(),
        images,
        context_references: request.context_references(),
        browser_annotation_refs,
        browser_node_selections,
        created_session,
        mission_title,
        task_title: action_task_title,
        trimmed_text,
        execution_goal,
        task_tier,
        collaboration_mode,
        access_profile: request.requested_access_profile(),
        skill_name,
        goal_mode: request.goal_mode,
        use_tools,
        target_role,
        request_id: request.request_id(),
        user_message_id: request.user_message_id(),
        placeholder_message_id: request.placeholder_message_id(),
        replace_turn_id: request.replace_turn_id(),
        required_tool_chain,
        completion_contract,
        recovery_checkpoint,
        denied_tools,
        turn_origin: DispatchTurnOrigin::User,
        user_message_metadata,
    };
    let accepted = match submit_dispatch_submission(state, dispatch) {
        Ok(accepted) => accepted,
        Err(error) if created_session => {
            return Err(rollback_created_session_on_dispatch_error(
                state,
                &session_id,
                previous_current_session_id.clone(),
                error,
            )
            .await);
        }
        Err(error) => {
            state.release_session_git_execution_lease(&session_id);
            return Err(error);
        }
    };
    publish_session_user_message_event(
        state,
        &session_id,
        workspace_id.clone(),
        accepted_at,
        &message,
    );
    if let Some(superseded_turn) = accepted.superseded_turn.as_ref() {
        super::publish_superseded_turn_event(
            state,
            &session_id,
            workspace_id.as_ref(),
            accepted_at,
            superseded_turn,
        );
    }
    let (event_id, event_seq, event_occurred_at) =
        match publish_session_turn_task_accepted_event(state, request, &accepted) {
            Ok(event) => event,
            Err(error) if created_session => {
                return Err(rollback_created_session_on_dispatch_error(
                    state,
                    &session_id,
                    previous_current_session_id,
                    error,
                )
                .await);
            }
            Err(error) => return Err(error),
        };
    if created_session {
        crate::session_title::spawn_new_session_title_refinement(
            state,
            &session_id,
            &message,
            placeholder_title,
        );
    }
    Ok((accepted, event_id, event_seq, event_occurred_at))
}

/// 新会话只有在首条消息完成接纳并发布事实后才算成立。
///
/// `execute_dispatch_submission` 在创建新会话后仍可能失败于引用校验、任务接纳或
/// 事实发布。所有这些失败都必须删除同一会话的任务、sidecar、队列、执行注册和
/// 浏览器资源，并恢复创建前的 current；否则前端会看到“会话消失”，后台却留下
/// 一个污染后续导航的空会话。
async fn rollback_created_session_on_dispatch_error(
    state: &ApiState,
    session_id: &SessionId,
    previous_current_session_id: Option<SessionId>,
    error: ApiError,
) -> ApiError {
    let original_message = error.message().to_string();
    state.release_session_git_execution_lease(session_id);
    match state
        .rollback_created_session_after_navigation_lock(session_id, previous_current_session_id)
        .await
    {
        Ok(()) => error,
        Err(cleanup_error) => ApiError::internal_assembly(
            "创建会话失败且回滚失败",
            format!("{original_message}；回滚失败: {cleanup_error:?}"),
        ),
    }
}

pub(super) fn dispatch_accepted_canonical_event(
    accepted: &DispatchSubmissionAccepted,
) -> (Option<CanonicalTurn>, Option<CanonicalTurnItem>) {
    let canonical_turn = accepted.accepted_canonical_turn.clone();
    let canonical_item = canonical_turn
        .as_ref()
        .and_then(|turn| {
            turn.items.iter().find(|item| {
                accepted
                    .user_message_item_id
                    .as_ref()
                    .is_some_and(|item_id| item.item_id == *item_id)
            })
        })
        .or_else(|| {
            canonical_turn.as_ref().and_then(|turn| {
                turn.items
                    .iter()
                    .find(|item| item.kind == CanonicalTurnItemKind::UserMessage)
            })
        })
        .cloned();
    (canonical_turn, canonical_item)
}

pub(super) fn accepted_session_directory_entry(
    state: &ApiState,
    accepted: &DispatchSubmissionAccepted,
) -> Option<SessionDirectoryEntryDto> {
    if !accepted.created_session {
        return None;
    }
    let session = state.session_store.session(&accepted.session_id)?;
    Some(SessionDirectoryEntryDto::from_record(session, true, 1))
}

pub(super) fn publish_goal_continuation_task_accepted_event(
    state: &ApiState,
    accepted: &DispatchSubmissionAccepted,
) -> EventId {
    let workspace_id = state
        .session_store
        .execution_ownership(&accepted.session_id)
        .and_then(|ownership| ownership.workspace_id);
    let (canonical_turn, canonical_item) = dispatch_accepted_canonical_event(accepted);
    let metadata = canonical_turn.as_ref().map(|turn| &turn.metadata);
    let request_id = metadata
        .and_then(|metadata| metadata.get("requestId"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let request_fingerprint = metadata
        .and_then(|metadata| metadata.get("requestFingerprint"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let attempt_id = metadata
        .and_then(|metadata| metadata.get("attemptId"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let session_summary = accepted_session_directory_entry(state, accepted);
    let event_id = EventId::new(format!(
        "event-session-turn-task-{}",
        accepted.accepted_at.0
    ));
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.turn.task.accepted",
        json!({
            "session_id": accepted.session_id,
            "entry_id": accepted.entry_id,
            "accepted_at": accepted.accepted_at.0,
            "workspace_id": workspace_id.as_ref().map(ToString::to_string),
            "text": serde_json::Value::Null,
            "skill_name": serde_json::Value::Null,
            "request_id": request_id,
            "request_fingerprint": request_fingerprint,
            "attempt_id": attempt_id,
            "user_message_id": canonical_item
                .as_ref()
                .and_then(|item| item.metadata.get("userMessageId"))
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            "placeholder_message_id": canonical_item
                .as_ref()
                .and_then(|item| item.metadata.get("placeholderMessageId"))
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            "image_count": 0,
            "created_session": false,
            "session_summary": session_summary,
            "route": "task",
            "goal_continuation": true,
            "root_task_id": accepted.root_task_id.to_string(),
            "action_task_id": accepted.action_task_id.to_string(),
            "runner_started": accepted.runner_started,
            "canonical_schema_version": CANONICAL_TURN_SCHEMA_VERSION,
            "canonical_event_kind": "turn_started",
            "canonical_turn": canonical_turn,
            "canonical_item": canonical_item,
        }),
    )
    .with_context(EventContext {
        session_id: Some(accepted.session_id.clone()),
        workspace_id,
        ..EventContext::default()
    });
    state.event_bus.publish(event);
    event_id
}

/// 在 accepted 事件已经发出后执行所有可能访问磁盘、Git 或模型的准备工作。
///
/// 提交控制面不能等待这段流程：workspace snapshot、Git 观测和上下文准备都属于
/// 可恢复的执行面。失败仍然沿原 Turn 收口，不能再创建第二条错误消息链。
async fn prepare_session_task_dispatch(
    state: &ApiState,
    accepted: &DispatchSubmissionAccepted,
    coordinator_attempt: &magi_conversation_runtime::TurnAttempt,
) -> Result<(), ApiError> {
    let trace_id = state
        .session_store
        .runtime_sidecar(&accepted.session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .and_then(|turn| {
            turn.items.into_iter().find_map(|item| {
                item.metadata
                    .get("traceId")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
        });
    let trace = PerformanceTrace::new(trace_id.as_deref(), accepted.accepted_at);
    trace.mark(
        "preparation_started",
        accepted.session_id.as_str(),
        Some(&accepted.turn_id),
        None,
    );
    let coordinator_is_running = matches!(
        state
            .turn_coordinator()
            .current_status(&accepted.session_id, &accepted.turn_id),
        Ok(magi_conversation_runtime::CoordinatorTurnStatus::Running)
    );
    if !coordinator_is_running {
        state
            .turn_coordinator()
            .execute_command(
                &accepted.session_id,
                magi_conversation_runtime::TurnCommand::SetStatus {
                    attempt: coordinator_attempt.clone(),
                    status: magi_conversation_runtime::CoordinatorTurnStatus::Preparing,
                },
            )
            .map_err(|error| {
                ApiError::internal_assembly("更新任务 Coordinator 准备状态失败", error)
            })?;
        state
            .turn_event_sink()
            .set_status_domain(&accepted.session_id, Some(&accepted.turn_id), "preparing")
            .map_err(|error| ApiError::internal_assembly("更新任务准备状态失败", error))?
            .ok_or_else(|| ApiError::Conflict("当前任务 Turn 已被新的操作取代".to_string()))?;
    }
    trace.mark(
        "preparation_turn_status_written",
        accepted.session_id.as_str(),
        Some(&accepted.turn_id),
        None,
    );
    if let Some(item_id) = accepted.user_message_item_id.as_deref() {
        publish_current_session_turn_item_event(
            &state.event_bus,
            &state.session_store,
            &accepted.session_id,
            &state
                .session_store
                .execution_ownership(&accepted.session_id)
                .and_then(|ownership| ownership.workspace_id),
            item_id,
            state.task_store(),
        )
        .map_err(|error| ApiError::internal_assembly("发布任务用户消息事实失败", error))?;
    }
    trace.mark(
        "preparation_user_message_published",
        accepted.session_id.as_str(),
        Some(&accepted.turn_id),
        None,
    );

    if let Some(config) = initial_session_orchestrator_config(
        state,
        accepted.created_session,
        accepted.request.orchestrator_session_config.as_ref(),
    )? {
        super::settings::save_orchestrator_session_override_for_session(
            state,
            &accepted.session_id,
            &config,
        )?;
        super::settings::require_orchestrator_session_model(state, &accepted.session_id)?;
    }
    trace.mark(
        "preparation_model_configured",
        accepted.session_id.as_str(),
        Some(&accepted.turn_id),
        None,
    );
    if accepted.request.workspace_id.is_none() {
        state.personal_session_execution_root(&accepted.session_id)?;
    }
    if accepted.request.goal_mode {
        state
            .session_store
            .set_active_goal_access_profile(&accepted.session_id, accepted.request.access_profile)
            .map_err(|error| ApiError::internal_assembly("更新 active goal 访问模式失败", error))?;
    } else if let Some((_goal, plan)) = state
        .session_store
        .pause_active_goal_for_diversion(&accepted.session_id)
        .map_err(|error| ApiError::internal_assembly("切换任务时暂停当前 Goal 失败", error))?
        && let Some(plan) = plan.as_ref()
    {
        magi_plan::publish_plan_event(
            &state.event_bus,
            magi_plan::plan_event_type(plan),
            plan,
            accepted.request.workspace_id.as_ref(),
            None,
            None,
        );
    }
    trace.mark(
        "preparation_goal_state_updated",
        accepted.session_id.as_str(),
        Some(&accepted.turn_id),
        None,
    );

    // 普通 Chat 只需要文本模型调用，不会执行文件/Git 工具。SnapshotSession 的首次
    // 建立会同步扫描整个 workspace，Git context 也会触发一次仓库观测；把这两项
    // 放在 Chat 接受后的准备链上会让模型首包被无关的磁盘工作阻塞数秒。工具轮次
    // 仍在这里初始化，确保所有可能写入 workspace 的工具继续共享同一变更账本和
    // Git context，保持执行隔离与变更审计语义不变。
    if accepted.request.use_tools {
        let execution_workspace_id = state
            .session_store
            .execution_ownership(&accepted.session_id)
            .and_then(|ownership| ownership.workspace_id);
        state
            .ensure_snapshot_session_for_workspace_id(&accepted.session_id, &execution_workspace_id)
            .await?;
        trace.mark(
            "preparation_snapshot_ready",
            accepted.session_id.as_str(),
            Some(&accepted.turn_id),
            None,
        );
        state
            .ensure_session_code_context(&accepted.session_id, &execution_workspace_id)
            .await?;
        trace.mark(
            "preparation_code_context_ready",
            accepted.session_id.as_str(),
            Some(&accepted.turn_id),
            None,
        );
    } else {
        trace.mark(
            "preparation_workspace_context_skipped",
            accepted.session_id.as_str(),
            Some(&accepted.turn_id),
            None,
        );
    }
    materialize_dispatch_submission_after_acceptance(state, accepted)?;
    trace.mark(
        "preparation_execution_materialized",
        accepted.session_id.as_str(),
        Some(&accepted.turn_id),
        None,
    );
    state.persist_session_state_checkpoint("session_task_turn_prepared")?;
    trace.mark(
        "preparation_completed",
        accepted.session_id.as_str(),
        Some(&accepted.turn_id),
        None,
    );
    Ok(())
}

pub(super) async fn finalize_session_task_dispatch(
    state: ApiState,
    accepted: DispatchSubmissionAccepted,
) {
    let mut accepted = accepted;
    if state.runner_manager().is_none() {
        fail_accepted_task_submission(&state, &accepted, "runner_manager 未配置");
        return;
    }
    // accepted 事实写入后，执行面仍可能尚未开始。把 finalizer 纳入与 Continue/Restart
    // 相同的生命周期锁顺序，确保新旧 Turn 不会同时进入同一 session 的执行入口。
    let _session_lifecycle_guard = state.lock_session_lifecycle(&accepted.session_id).await;
    let manager = state
        .runner_manager()
        .expect("runner_manager 已在生命周期锁前校验存在");
    let _restart_guard = manager
        .lock_for_restart(accepted.root_task_id.as_str())
        .await;
    let current_turn_matches = state
        .session_store
        .runtime_sidecar(&accepted.session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .is_some_and(|turn| turn.turn_id == accepted.turn_id);
    if !current_turn_matches {
        tracing::info!(
            session_id = %accepted.session_id,
            root_task_id = %accepted.root_task_id,
            turn_id = %accepted.turn_id,
            "跳过已被更新 Turn 取代的后台 session finalizer"
        );
        return;
    }
    let coordinator_attempt = match state
        .turn_coordinator()
        .current_attempt(&accepted.session_id, &accepted.turn_id)
    {
        Ok(attempt) => attempt,
        Err(error) => {
            tracing::error!(
                session_id = %accepted.session_id,
                turn_id = %accepted.turn_id,
                %error,
                "任务准备前读取 Coordinator attempt 失败"
            );
            fail_accepted_task_submission(&state, &accepted, &error.to_string());
            return;
        }
    };
    if let Err(error) = prepare_session_task_dispatch(&state, &accepted, &coordinator_attempt).await
    {
        state.release_session_git_execution_lease(&accepted.session_id);
        tracing::error!(
            session_id = %accepted.session_id,
            root_task_id = %accepted.root_task_id,
            ?error,
            "session turn preparation failed"
        );
        fail_accepted_task_submission(&state, &accepted, error.message());
        return;
    }
    if let Err(error) = state.turn_coordinator().execute_command(
        &accepted.session_id,
        magi_conversation_runtime::TurnCommand::SetStatus {
            attempt: coordinator_attempt.clone(),
            status: magi_conversation_runtime::CoordinatorTurnStatus::Running,
        },
    ) {
        tracing::error!(
            session_id = %accepted.session_id,
            turn_id = %accepted.turn_id,
            %error,
            "更新任务 Coordinator 运行状态失败"
        );
        fail_accepted_task_submission(&state, &accepted, &error.to_string());
        return;
    }
    match state.turn_event_sink().set_status_domain(
        &accepted.session_id,
        Some(&accepted.turn_id),
        "running",
    ) {
        Ok(Some(_)) => {}
        Ok(None) => {
            let error = "当前任务 Turn 已被新的操作取代";
            tracing::error!(
                session_id = %accepted.session_id,
                root_task_id = %accepted.root_task_id,
                turn_id = %accepted.turn_id,
                "session turn running 状态写回时未找到当前 Turn"
            );
            fail_accepted_task_submission(&state, &accepted, error);
            return;
        }
        Err(error) => {
            tracing::error!(
                session_id = %accepted.session_id,
                root_task_id = %accepted.root_task_id,
                ?error,
                "session turn running 状态写回失败"
            );
            fail_accepted_task_submission(&state, &accepted, &error.to_string());
            return;
        }
    }
    if let Err(error) =
        drive_dispatch_submission_after_lifecycle_and_restart_lock(&state, &mut accepted)
    {
        tracing::error!(
            session_id = %accepted.session_id,
            root_task_id = %accepted.root_task_id,
            action_task_id = %accepted.action_task_id,
            ?error,
            "session turn task dispatch failed"
        );
        fail_accepted_task_submission(&state, &accepted, error.message());
    }
}

/// 只把执行面投递到后台；调用方已经完成最小 durable accepted 写入。
pub(super) fn schedule_session_task_dispatch(
    state: ApiState,
    accepted: DispatchSubmissionAccepted,
) {
    tokio::spawn(async move {
        finalize_session_task_dispatch(state, accepted).await;
    });
}

/// daemon 重启后恢复已经 durable accepted、但尚未开始 Runner 的主线 Turn。
///
/// 只恢复 `accepted/preparing + pending root task`，已经进入模型执行的轮次仍由
/// daemon 的中断收口逻辑处理，避免重复发送上游请求。
pub(crate) fn schedule_restored_session_task_dispatches(state: ApiState) {
    let Some(task_store) = state.task_store() else {
        return;
    };
    let restored = state
        .session_store
        .runtime_sidecars()
        .into_iter()
        .filter_map(|sidecar| {
            let turn = sidecar.current_turn.as_ref()?;
            if !matches!(turn.status.as_str(), "accepted" | "preparing") {
                return None;
            }
            let chain = sidecar.active_execution_chain.as_ref()?;
            let root_task = task_store.get_task(&chain.root_task_id)?;
            if root_task.status != TaskStatus::Pending {
                return None;
            }
            let request = match recover_dispatch_submission_request(&sidecar) {
                Ok(request) => request,
                Err(error) => {
                    tracing::error!(
                        session_id = %sidecar.session_id,
                        turn_id = %turn.turn_id,
                        %error,
                        "无法恢复 accepted Turn 的完整派发请求"
                    );
                    return None;
                }
            };
            let action_task_id = chain
                .branches
                .iter()
                .find(|branch| branch.is_primary)
                .map(|branch| branch.task_id.clone())
                .unwrap_or_else(|| chain.root_task_id.clone());
            let user_message_item_id = turn
                .items
                .iter()
                .find(|item| item.kind == "user_message")
                .map(|item| item.item_id.clone());
            Some(DispatchSubmissionAccepted {
                request,
                session_id: sidecar.session_id.clone(),
                entry_id: chain.dispatch_context.entry_id.clone(),
                accepted_at: chain.dispatch_context.accepted_at,
                created_session: false,
                root_task_id: chain.root_task_id.clone(),
                action_task_id,
                turn_id: turn.turn_id.clone(),
                user_message_item_id,
                accepted_canonical_turn: state
                    .session_store
                    .canonical_turn_for_session_turn_id(&sidecar.session_id, &turn.turn_id),
                runner_started: false,
                superseded_turn: None,
            })
        })
        .collect::<Vec<_>>();
    for accepted in restored {
        tracing::info!(
            session_id = %accepted.session_id,
            root_task_id = %accepted.root_task_id,
            "恢复 daemon 重启前已接纳但尚未启动的 session Turn"
        );
        schedule_session_task_dispatch(state.clone(), accepted);
    }
}

fn fail_accepted_task_submission(
    state: &ApiState,
    accepted: &DispatchSubmissionAccepted,
    direct_error: &str,
) {
    let mut direct_error = public_runtime_excerpt(direct_error, 4096);
    let Some(task_store) = state.task_store() else {
        tracing::error!(
            session_id = %accepted.session_id,
            root_task_id = %accepted.root_task_id,
            "accepted task 启动失败时 task_store 未配置，无法写入失败事实"
        );
        return;
    };
    let Some(task) = task_store.get_task(&accepted.root_task_id) else {
        tracing::error!(
            session_id = %accepted.session_id,
            root_task_id = %accepted.root_task_id,
            "accepted task 启动失败时根任务不存在，无法写入失败事实"
        );
        return;
    };
    if task.status == TaskStatus::Pending
        && let Err(error) = cleanup_materialized_dispatch_submission_if_not_started(
            &state.session_store,
            state.task_execution_registry(),
            &accepted.session_id,
            &accepted.root_task_id,
            &accepted.turn_id,
        )
    {
        direct_error = format!("{direct_error}；清理未启动执行资源失败: {error}");
    }
    let lease_id = task_store
        .get_active_lease(&accepted.root_task_id)
        .map(|lease| lease.lease_id);
    let terminalized = match task_store.revoke_lease_and_set_task_terminal(
        &accepted.root_task_id,
        &task.root_task_id,
        lease_id.as_ref(),
        TaskStatus::Failed,
        vec![direct_error],
    ) {
        Ok(changed) => changed,
        Err(error) => {
            tracing::error!(
                session_id = %accepted.session_id,
                root_task_id = %accepted.root_task_id,
                ?error,
                "accepted task 启动失败时写入 Failed 终态失败，等待 durable accepted 状态恢复"
            );
            return;
        }
    };
    let already_terminal = task_store
        .get_task(&accepted.root_task_id)
        .is_some_and(|current| {
            matches!(
                current.status,
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
            )
        });
    if !(terminalized || already_terminal) {
        tracing::error!(
            session_id = %accepted.session_id,
            root_task_id = %accepted.root_task_id,
            "accepted task 启动失败未能提交根任务终态，等待恢复"
        );
        return;
    }
    match crate::task_turn_finalize::finalize_background_session_task_turn_if_root_terminal_for_turn(
        state,
        &accepted.session_id,
        &accepted.root_task_id,
        "error",
        Some(&accepted.turn_id),
    ) {
        Ok(true) => {}
        Ok(false) => tracing::error!(
            session_id = %accepted.session_id,
            root_task_id = %accepted.root_task_id,
            turn_id = %accepted.turn_id,
            "accepted task 已进入终态，但当前 Turn 未能完成统一收口，等待恢复"
        ),
        Err(error) => tracing::error!(
            session_id = %accepted.session_id,
            root_task_id = %accepted.root_task_id,
            turn_id = %accepted.turn_id,
            %error,
            "accepted task 终态 Turn 统一收口失败，等待恢复"
        ),
    }
}

fn format_action_task_title(mission_title: &str) -> String {
    let title = mission_title.trim();
    if title.starts_with("执行:") || title.starts_with("执行：") {
        title.to_string()
    } else {
        format!("执行: {title}")
    }
}

fn publish_session_turn_task_accepted_event(
    state: &ApiState,
    request: &SessionTurnRequestDto,
    accepted: &DispatchSubmissionAccepted,
) -> Result<(EventId, u64, UtcMillis), ApiError> {
    let workspace_id = state
        .session_store
        .execution_ownership(&accepted.session_id)
        .and_then(|ownership| ownership.workspace_id)
        .or_else(|| request.requested_workspace_id().map(WorkspaceId::new));
    let workspace_id_payload = workspace_id.as_ref().map(ToString::to_string);
    let (canonical_turn, canonical_item) = dispatch_accepted_canonical_event(accepted);
    let session_summary = accepted_session_directory_entry(state, accepted);
    let event_id = EventId::new(format!(
        "event-session-turn-task-{}",
        accepted.accepted_at.0
    ));
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.turn.task.accepted",
        json!({
            "session_id": accepted.session_id,
            "entry_id": accepted.entry_id,
            "accepted_at": accepted.accepted_at.0,
            "workspace_id": workspace_id_payload,
            "text": request.trimmed_text(),
            "skill_name": request.skill_name.clone(),
            "request_id": request.request_id(),
            "user_message_id": request.user_message_id(),
            "placeholder_message_id": request.placeholder_message_id(),
            "image_count": request.images.len(),
            "created_session": accepted.created_session,
            "session_summary": session_summary,
            "route": "task",
            "root_task_id": accepted.root_task_id.to_string(),
            "action_task_id": accepted.action_task_id.to_string(),
            "runner_started": accepted.runner_started,
            "canonical_schema_version": CANONICAL_TURN_SCHEMA_VERSION,
            "canonical_event_kind": "turn_started",
            "canonical_turn": canonical_turn,
            "canonical_item": canonical_item,
        }),
    )
    .with_context(EventContext {
        session_id: Some(accepted.session_id.clone()),
        workspace_id,
        ..EventContext::default()
    });
    let event_occurred_at = event.occurred_at;
    let event_seq = state.event_bus.publish(event);
    Ok((event_id, event_seq, event_occurred_at))
}

pub(super) fn resolve_dispatch_session(
    state: &ApiState,
    requested_session_id: Option<SessionId>,
    requested_workspace_id: Option<WorkspaceId>,
    placeholder_title: &str,
    accepted_at: UtcMillis,
) -> Result<(SessionId, bool, Option<WorkspaceId>), ApiError> {
    if let Some(session_id) = requested_session_id {
        let session = state
            .session_store
            .session(&session_id)
            .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
        let workspace_id =
            resolve_session_workspace_binding(state, &session, requested_workspace_id.as_ref())?;
        return Ok((session_id, false, workspace_id));
    }

    let session_id = new_session_id();
    state
        .session_store
        .create_session_for_workspace_at(
            session_id.clone(),
            placeholder_title.to_string(),
            requested_workspace_id.as_ref().map(ToString::to_string),
            accepted_at,
        )
        .map_err(|err| ApiError::internal_assembly("创建会话失败", err))?;
    Ok((session_id, true, requested_workspace_id))
}

pub(crate) fn publish_session_user_message_event(
    state: &ApiState,
    session_id: &SessionId,
    workspace_id: Option<WorkspaceId>,
    accepted_at: UtcMillis,
    message: &str,
) {
    let _ = state.event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-message-user-{}", accepted_at.0)),
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

#[cfg(test)]
mod tests {
    use super::{
        fail_accepted_task_submission, format_action_task_title,
        initial_session_orchestrator_config, resolve_dispatch_session,
    };
    use crate::{
        errors::ApiError,
        state::ApiState,
        task_dispatch::{DispatchSubmissionAccepted, DispatchSubmissionRequest},
    };
    use magi_core::{
        AbsolutePath, MissionId, SessionId, Task, TaskId, TaskKind, TaskRuntimePayload, TaskStatus,
        UtcMillis, WorkspaceId,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_orchestrator::task_store::TaskStore;
    use magi_session_store::{SessionStore, TimelineEntryKind};
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;

    fn test_state() -> ApiState {
        ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
    }

    #[test]
    fn turn_payload_only_initializes_model_for_a_new_session() {
        let state = test_state();
        state
            .settings_store
            .set_section(
                magi_settings_store::ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
                serde_json::json!({
                    "model": "model-last-used",
                    "reasoningEffort": "high"
                }),
            )
            .unwrap();
        let config = serde_json::json!({ "model": "model-selected-by-user" });
        assert_eq!(
            initial_session_orchestrator_config(&state, true, Some(&config))
                .expect("new session config should be accepted"),
            Some(serde_json::json!({
                "model": "model-selected-by-user",
                "reasoningEffort": "high"
            })),
        );
        assert!(
            initial_session_orchestrator_config(&state, false, Some(&config)).is_err(),
            "ordinary turns must not be able to overwrite the persisted session model",
        );
        assert_eq!(
            initial_session_orchestrator_config(&state, false, None)
                .expect("existing session should use its persisted model"),
            None,
        );
    }

    #[test]
    fn new_session_without_explicit_config_uses_persisted_default() {
        let state = test_state();
        state
            .settings_store
            .set_section(
                magi_settings_store::ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
                serde_json::json!({ "model": "model-last-used" }),
            )
            .unwrap();

        assert_eq!(
            initial_session_orchestrator_config(&state, true, None)
                .expect("new session should inherit the persisted default"),
            Some(serde_json::json!({
                "model": "model-last-used",
                "reasoningEffort": "medium"
            })),
        );
    }

    #[test]
    fn action_task_title_does_not_repeat_execute_prefix() {
        assert_eq!(
            format_action_task_title("执行: 批量检查 README"),
            "执行: 批量检查 README"
        );
        assert_eq!(
            format_action_task_title("批量检查 README"),
            "执行: 批量检查 README"
        );
    }

    #[test]
    fn accepted_dispatch_failure_persists_sanitized_direct_error_on_task() {
        let task_store = Arc::new(TaskStore::new());
        let root_task_id = TaskId::new("task-dispatch-direct-error");
        let now = UtcMillis::now();
        task_store
            .insert_task(Task {
                task_id: root_task_id.clone(),
                mission_id: MissionId::new("mission-dispatch-direct-error"),
                root_task_id: root_task_id.clone(),
                parent_task_id: None,
                kind: TaskKind::LocalAgent,
                title: "派发失败诊断".to_string(),
                goal: "保留直接错误".to_string(),
                status: TaskStatus::Pending,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: None,
                executor_binding: None,
                completion_contract: magi_core::TaskCompletionContract::default(),
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
            })
            .expect("任务应插入");
        let state = test_state().with_task_store(task_store.clone());
        let accepted = DispatchSubmissionAccepted {
            request: DispatchSubmissionRequest {
                accepted_at: now,
                session_id: SessionId::new("session-dispatch-direct-error"),
                workspace_id: None,
                execution_root: None,
                orchestrator_session_config: None,
                entry_id: "entry-dispatch-direct-error".to_string(),
                timeline_message: "派发失败诊断".to_string(),
                images: Vec::new(),
                context_references: Vec::new(),
                browser_annotation_refs: Vec::new(),
                browser_node_selections: Vec::new(),
                created_session: false,
                mission_title: "派发失败诊断".to_string(),
                task_title: "派发失败诊断".to_string(),
                trimmed_text: Some("保留直接错误".to_string()),
                execution_goal: Some("保留直接错误".to_string()),
                task_tier: magi_core::TaskTier::ExecutionChain,
                collaboration_mode: magi_core::CollaborationMode::Auto,
                access_profile: magi_core::AccessProfile::Restricted,
                skill_name: None,
                goal_mode: false,
                use_tools: true,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
                replace_turn_id: None,
                required_tool_chain: Vec::new(),
                completion_contract: magi_core::TaskCompletionContract::default(),
                recovery_checkpoint: None,
                denied_tools: Vec::new(),
                user_message_metadata: Default::default(),
                turn_origin:
                    magi_conversation_runtime::dispatch_submission::DispatchTurnOrigin::User,
            },
            session_id: SessionId::new("session-dispatch-direct-error"),
            entry_id: "entry-dispatch-direct-error".to_string(),
            accepted_at: now,
            created_session: false,
            root_task_id: root_task_id.clone(),
            action_task_id: root_task_id.clone(),
            turn_id: "turn-dispatch-direct-error".to_string(),
            user_message_item_id: None,
            accepted_canonical_turn: None,
            runner_started: false,
            superseded_turn: None,
        };
        let error = ApiError::internal_assembly(
            "驱动任务派发",
            "provider timeout at /Users/xie/code/private.rs with sk-test-secret-value",
        );

        fail_accepted_task_submission(&state, &accepted, error.message());

        let failed_task = task_store
            .get_task(&root_task_id)
            .expect("failed task should remain available");
        assert_eq!(failed_task.status, TaskStatus::Failed);
        assert_eq!(failed_task.output_refs.len(), 1);
        let direct_error = &failed_task.output_refs[0];
        assert!(direct_error.contains("驱动任务派发"));
        assert!(direct_error.contains("provider timeout"));
        assert!(direct_error.contains("[path]"));
        assert!(direct_error.contains("sk-[redacted]"));
        assert!(!direct_error.contains("/Users/xie"));
        assert!(!direct_error.contains("test-secret-value"));
        assert!(!direct_error.contains("可直接重试"));
    }

    #[test]
    fn resolve_dispatch_session_ignores_empty_current_session_without_explicit_session() {
        let state = test_state();
        let workspace_id = WorkspaceId::new("workspace-dispatch-empty-current");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/tmp/magi-dispatch-empty-current"),
            )
            .expect("workspace should register");
        let empty_session_id = SessionId::new("session-empty-current");
        state
            .session_store
            .create_session_for_workspace(
                empty_session_id.clone(),
                "空白会话",
                Some(workspace_id.to_string()),
            )
            .expect("empty session should create");

        let (resolved_session_id, created_session, resolved_workspace_id) =
            resolve_dispatch_session(
                &state,
                None,
                Some(workspace_id.clone()),
                "真实首条消息",
                UtcMillis::now(),
            )
            .expect("dispatch session should resolve");

        assert_ne!(resolved_session_id, empty_session_id);
        assert!(created_session);
        assert_eq!(resolved_workspace_id, Some(workspace_id));
    }

    #[test]
    fn resolve_dispatch_session_uses_turn_accept_time_for_new_session_creation() {
        let state = test_state();
        let workspace_id = WorkspaceId::new("workspace-dispatch-accepted-at");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/tmp/magi-dispatch-accepted-at"),
            )
            .expect("workspace should register");
        let accepted_at = UtcMillis(1_779_700_000_000);

        let (resolved_session_id, created_session, _) =
            resolve_dispatch_session(&state, None, Some(workspace_id), "新会话", accepted_at)
                .expect("dispatch session should resolve");

        let session = state
            .session_store
            .session(&resolved_session_id)
            .expect("created session should exist");
        assert!(created_session);
        assert_eq!(session.created_at, accepted_at);
        assert_eq!(session.updated_at, accepted_at);
    }

    #[test]
    fn resolve_dispatch_session_creates_new_without_explicit_session_even_when_current_has_history()
    {
        let state = test_state();
        let workspace_id = WorkspaceId::new("workspace-dispatch-current-history");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/tmp/magi-dispatch-current-history"),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-current-history");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "已有历史",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        state.session_store.append_timeline_entry(
            session_id.clone(),
            TimelineEntryKind::UserMessage,
            "已有用户消息",
        );

        let (resolved_session_id, created_session, resolved_workspace_id) =
            resolve_dispatch_session(
                &state,
                None,
                Some(workspace_id.clone()),
                "后续消息",
                UtcMillis::now(),
            )
            .expect("dispatch session should resolve");

        assert_ne!(resolved_session_id, session_id);
        assert!(created_session);
        assert_eq!(resolved_workspace_id, Some(workspace_id));
    }
}
