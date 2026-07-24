use magi_core::{EventId, SessionId, TaskStatus, TaskTier, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_session_store::ActiveExecutionTurn;
use serde_json::json;

use super::{
    conversation_bridge::ingest_user_input_to_conversation, monotonic_accepted_at, new_session_id,
    session_scope::resolve_session_workspace_binding,
};
use crate::{
    dto::SessionTurnRequestDto,
    errors::ApiError,
    state::ApiState,
    task_dispatch::{
        DispatchSubmissionAccepted, DispatchSubmissionRequest, DispatchTurnOrigin,
        drive_dispatch_submission, submit_dispatch_submission,
    },
};
use magi_conversation_runtime::session_images::SessionTurnImage;
use magi_conversation_runtime::session_writeback::{
    SessionTurnErrorInput, append_session_turn_error_item, publish_current_session_turn_item_event,
};
use magi_session_store::{
    CANONICAL_TURN_SCHEMA_VERSION, CanonicalTurn, CanonicalTurnItem, CanonicalTurnItemKind,
    SessionGoal,
};

pub(super) async fn accept_session_task_submission(
    state: &ApiState,
    request: &SessionTurnRequestDto,
    images: Vec<SessionTurnImage>,
    workspace_id: WorkspaceId,
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
            accepted_at: monotonic_accepted_at(),
            required_tool_chain: Vec::new(),
        },
    )
    .await
}

pub(super) struct SessionTaskSubmissionInput {
    pub images: Vec<SessionTurnImage>,
    pub workspace_id: WorkspaceId,
    pub task_title: Option<String>,
    pub execution_goal: Option<String>,
    pub task_tier: TaskTier,
    pub accepted_at: UtcMillis,
    pub required_tool_chain: Vec<String>,
}

pub(super) async fn accept_session_task_submission_at(
    state: &ApiState,
    request: &SessionTurnRequestDto,
    input: SessionTaskSubmissionInput,
) -> Result<(DispatchSubmissionAccepted, EventId), ApiError> {
    let SessionTaskSubmissionInput {
        images,
        workspace_id,
        task_title,
        execution_goal,
        task_tier,
        accepted_at,
        required_tool_chain,
    } = input;
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
            skill_name: request.skill_name.clone(),
            target_role: None,
            images,
            request,
            accepted_at,
            required_tool_chain,
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
    state
        .ensure_snapshot_session_for_workspace_id(&session_id, &workspace_id)
        .await?;
    state
        .ensure_session_code_context(&session_id, &workspace_id)
        .await?;
    let entry_id = format!(
        "timeline-goal-continuation-{}-{}",
        session_id, accepted_at.0
    );
    let dispatch = DispatchSubmissionRequest {
        accepted_at,
        session_id: session_id.clone(),
        workspace_id,
        entry_id,
        timeline_message: format!("目标自动推进: {}", goal.objective),
        images: Vec::new(),
        context_references: Vec::new(),
        created_session: false,
        mission_title: "目标自动推进".to_string(),
        task_title: "执行: 目标自动推进".to_string(),
        trimmed_text: None,
        execution_goal: Some(execution_goal),
        task_tier: TaskTier::ExecutionChain,
        access_profile: goal.access_profile,
        skill_name: None,
        goal_mode: true,
        target_role: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        replace_turn_id: None,
        required_tool_chain: vec!["get_goal".to_string()],
        turn_origin: DispatchTurnOrigin::GoalContinuation,
    };
    let accepted = submit_dispatch_submission(state, dispatch)?;
    if let Err(error) = state.persist_session_state_checkpoint("goal_continuation_task_accepted") {
        fail_accepted_task_submission(state, &accepted);
        return Err(error);
    }
    Ok(accepted)
}

struct ExecuteDispatchSubmissionInput<'a> {
    requested_session_id: Option<SessionId>,
    requested_workspace_id: WorkspaceId,
    mission_title: String,
    message: String,
    trimmed_text: Option<String>,
    execution_goal: Option<String>,
    task_tier: TaskTier,
    skill_name: Option<String>,
    target_role: Option<String>,
    images: Vec<SessionTurnImage>,
    request: &'a SessionTurnRequestDto,
    accepted_at: UtcMillis,
    required_tool_chain: Vec<String>,
}

async fn execute_dispatch_submission(
    state: &ApiState,
    input: ExecuteDispatchSubmissionInput<'_>,
) -> Result<(DispatchSubmissionAccepted, EventId), ApiError> {
    let ExecuteDispatchSubmissionInput {
        requested_session_id,
        requested_workspace_id,
        mission_title,
        message,
        trimmed_text,
        execution_goal,
        task_tier,
        skill_name,
        target_role,
        images,
        request,
        accepted_at,
        required_tool_chain,
    } = input;
    let placeholder_title = crate::session_title::NEW_SESSION_PLACEHOLDER_TITLE;
    let (session_id, created_session, workspace_id) = resolve_dispatch_session(
        state,
        requested_session_id,
        Some(requested_workspace_id),
        placeholder_title,
        accepted_at,
    )?;
    if let Some(config) = request.orchestrator_session_config.as_ref() {
        super::settings::save_orchestrator_session_override_for_session(
            state,
            &session_id,
            config,
        )?;
        super::settings::require_orchestrator_session_model(state, &session_id)?;
    }
    state
        .session_store
        .set_active_goal_access_profile(&session_id, request.requested_access_profile())
        .map_err(|error| ApiError::internal_assembly("更新 active goal 访问模式失败", error))?;
    state
        .ensure_snapshot_session_for_workspace_id(&session_id, &workspace_id)
        .await?;
    state
        .ensure_session_code_context(&session_id, &workspace_id)
        .await?;
    let user_timeline_entry_id = format!("timeline-{}-{}", session_id, accepted_at.0);
    let action_task_title = format_action_task_title(&mission_title);

    let dispatch = DispatchSubmissionRequest {
        accepted_at,
        session_id: session_id.clone(),
        workspace_id: workspace_id.clone(),
        entry_id: user_timeline_entry_id,
        timeline_message: message.clone(),
        images,
        context_references: request.context_references(),
        created_session,
        mission_title,
        task_title: action_task_title,
        trimmed_text,
        execution_goal,
        task_tier,
        access_profile: request.requested_access_profile(),
        skill_name,
        goal_mode: request.goal_mode,
        target_role,
        request_id: request.request_id(),
        user_message_id: request.user_message_id(),
        placeholder_message_id: request.placeholder_message_id(),
        replace_turn_id: request.replace_turn_id(),
        required_tool_chain,
        turn_origin: DispatchTurnOrigin::User,
    };
    let accepted = match submit_dispatch_submission(state, dispatch) {
        Ok(accepted) => accepted,
        Err(error) => {
            state.release_session_git_execution_lease(&session_id);
            return Err(error);
        }
    };
    if let Err(error) = state.persist_session_state_checkpoint("session_task_turn_accepted") {
        fail_accepted_task_submission(state, &accepted);
        return Err(error);
    }
    ingest_user_input_to_conversation(state, &session_id, request, accepted_at);
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
    let event_id = publish_session_turn_task_accepted_event(state, request, &accepted)?;
    if created_session {
        crate::session_title::spawn_new_session_title_refinement(
            state,
            &session_id,
            &message,
            placeholder_title,
        );
    }
    Ok((accepted, event_id))
}

pub(super) fn dispatch_accepted_canonical_event(
    state: &ApiState,
    accepted: &DispatchSubmissionAccepted,
) -> (Option<CanonicalTurn>, Option<CanonicalTurnItem>) {
    let current_turn_id = state
        .session_store
        .runtime_sidecar(&accepted.session_id)
        .and_then(|sidecar| sidecar.current_turn.map(|turn| turn.turn_id));
    let canonical_turn = state
        .session_store
        .canonical_turns_for_session(&accepted.session_id)
        .into_iter()
        .find(|turn| {
            current_turn_id
                .as_ref()
                .is_some_and(|turn_id| &turn.turn_id == turn_id)
                || (turn.accepted_at == accepted.accepted_at
                    && turn.items.iter().any(|item| {
                        item.worker
                            .as_ref()
                            .and_then(|worker| worker.task_id.as_ref())
                            == Some(&accepted.action_task_id)
                    }))
        });
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

pub(super) fn publish_goal_continuation_task_accepted_event(
    state: &ApiState,
    accepted: &DispatchSubmissionAccepted,
) -> EventId {
    let workspace_id = state
        .session_store
        .execution_ownership(&accepted.session_id)
        .and_then(|ownership| ownership.workspace_id);
    let (canonical_turn, canonical_item) = dispatch_accepted_canonical_event(state, accepted);
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
            "workspace_id": workspace_id.as_ref().map(ToString::to_string),
            "text": serde_json::Value::Null,
            "skill_name": serde_json::Value::Null,
            "request_id": serde_json::Value::Null,
            "user_message_id": serde_json::Value::Null,
            "placeholder_message_id": serde_json::Value::Null,
            "image_count": 0,
            "created_session": false,
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

pub(super) async fn finalize_session_task_dispatch(
    state: ApiState,
    accepted: DispatchSubmissionAccepted,
) {
    let mut accepted = accepted;
    if let Err(error) = drive_dispatch_submission(&state, &mut accepted).await {
        tracing::error!(
            session_id = %accepted.session_id,
            root_task_id = %accepted.root_task_id,
            action_task_id = %accepted.action_task_id,
            ?error,
            "session turn task dispatch failed"
        );
        fail_accepted_task_submission(&state, &accepted);
        return;
    }
    append_dispatch_assistant_message(&state, &accepted);
}

fn fail_accepted_task_submission(state: &ApiState, accepted: &DispatchSubmissionAccepted) {
    if let Some(task_store) = state.task_store()
        && task_store.get_task(&accepted.root_task_id).is_some()
    {
        task_store.set_output_refs(
            &accepted.root_task_id,
            vec!["任务执行启动失败，可直接重试。".to_string()],
        );
        let _ = task_store.update_status(&accepted.root_task_id, TaskStatus::Failed);
        if crate::task_turn_finalize::finalize_background_session_task_turn_if_root_terminal(
            state,
            &accepted.session_id,
            &accepted.root_task_id,
            "error",
        ) {
            let _ = state.persist_session_state_checkpoint("session_task_turn_failed");
            return;
        }
    }

    if let Some(thread) = state
        .session_store
        .orchestrator_thread_for_session(&accepted.session_id)
    {
        let workspace_id = state
            .session_store
            .execution_ownership(&accepted.session_id)
            .and_then(|ownership| ownership.workspace_id);
        append_session_turn_error_item(
            &state.event_bus,
            &state.session_store,
            SessionTurnErrorInput {
                session_id: &accepted.session_id,
                workspace_id: &workspace_id,
                task_id: Some(&accepted.root_task_id),
                request_id: None,
                user_message_id: accepted.user_message_item_id.as_deref(),
                placeholder_message_id: None,
                error_text: "任务执行启动失败，可直接重试。",
                streaming_entry_id: None,
                source_thread_id: thread.thread_id,
                persist_session_state: None,
            },
        );
    } else {
        let _ = state
            .session_store
            .update_current_turn_status(&accepted.session_id, "failed");
    }
    let _ = state.persist_session_state_checkpoint("session_task_turn_failed");
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
) -> Result<EventId, ApiError> {
    let workspace_id = state
        .session_store
        .execution_ownership(&accepted.session_id)
        .and_then(|ownership| ownership.workspace_id)
        .or_else(|| request.requested_workspace_id().map(WorkspaceId::new));
    let workspace_id_payload = workspace_id.as_ref().map(ToString::to_string);
    let (canonical_turn, canonical_item) = dispatch_accepted_canonical_event(state, accepted);
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
            "workspace_id": workspace_id_payload,
            "text": request.trimmed_text(),
            "skill_name": request.skill_name.clone(),
            "request_id": request.request_id(),
            "user_message_id": request.user_message_id(),
            "placeholder_message_id": request.placeholder_message_id(),
            "image_count": request.images.len(),
            "created_session": accepted.created_session,
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
    state.event_bus.publish(event);
    Ok(event_id)
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

fn publish_session_user_message_event(
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

pub(super) fn append_dispatch_assistant_message(
    state: &ApiState,
    accepted: &DispatchSubmissionAccepted,
) {
    if crate::task_turn_finalize::finalize_background_session_task_turn_if_root_completed(
        state,
        &accepted.session_id,
        &accepted.root_task_id,
    ) {
        return;
    }

    let Some(task_store) = state.task_store() else {
        return;
    };
    let Some(root_task) = task_store.get_task(&accepted.root_task_id) else {
        return;
    };
    if root_task.status != TaskStatus::Completed {
        return;
    }
    let Some(task) = task_store.get_task(&accepted.action_task_id) else {
        return;
    };
    if task.status != TaskStatus::Completed {
        return;
    }
    let current_turn = state
        .session_store
        .runtime_sidecar(&accepted.session_id)
        .and_then(|sidecar| sidecar.current_turn);
    if current_turn.as_ref().is_some_and(|turn| {
        turn.status != "completed" && turn.status != "running" && turn.status != "accepted"
    }) {
        return;
    }
    let current_turn_matches = current_turn
        .as_ref()
        .is_some_and(|turn| turn_matches_accepted_dispatch(turn, accepted));
    let response = current_turn_matches
        .then(|| {
            current_turn
                .clone()
                .and_then(|turn| assistant_final_from_turn(turn, accepted))
        })
        .flatten();

    let Some((response_text, final_item_id)) = response else {
        return;
    };
    let _ = state
        .session_store
        .update_current_turn_status(&accepted.session_id, "completed");
    if let Err(error) = state.persist_session_state_checkpoint("session_task_turn_completed") {
        tracing::error!(
            session_id = %accepted.session_id,
            final_item_id = %final_item_id,
            ?error,
            "session task turn terminal persist failed before event publish"
        );
    }
    let workspace_id = state
        .session_store
        .execution_ownership(&accepted.session_id)
        .and_then(|ownership| ownership.workspace_id);
    publish_current_session_turn_item_event(
        &state.event_bus,
        state.session_store.as_ref(),
        &accepted.session_id,
        &workspace_id,
        &final_item_id,
        state.task_store(),
    );
    let _ = state.event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-message-assistant-{}", UtcMillis::now().0)),
            "message.created",
            json!({
                "session_id": accepted.session_id.to_string(),
                "role": "assistant",
                "content": response_text,
            }),
        )
        .with_context(EventContext {
            session_id: Some(accepted.session_id.clone()),
            ..EventContext::default()
        }),
    );
}

fn turn_matches_accepted_dispatch(
    turn: &ActiveExecutionTurn,
    accepted: &DispatchSubmissionAccepted,
) -> bool {
    turn.accepted_at == accepted.accepted_at
        || turn
            .items
            .iter()
            .any(|item| item.task_id.as_ref() == Some(&accepted.action_task_id))
}

fn assistant_final_from_turn(
    turn: ActiveExecutionTurn,
    accepted: &DispatchSubmissionAccepted,
) -> Option<(String, String)> {
    turn.items
        .into_iter()
        .filter(|item| item.kind == "assistant_final")
        .filter(|item| {
            item.task_id
                .as_ref()
                .is_none_or(|task_id| task_id == &accepted.action_task_id)
        })
        .filter_map(|item| {
            item.content
                .filter(|content| !content.trim().is_empty())
                .map(|content| (item.item_seq, content, item.item_id))
        })
        .max_by_key(|(item_seq, _, _)| *item_seq)
        .map(|(_, content, item_id)| (content, item_id))
}

#[cfg(test)]
mod tests {
    use super::{format_action_task_title, resolve_dispatch_session};
    use crate::state::ApiState;
    use magi_core::{AbsolutePath, SessionId, UtcMillis, WorkspaceId};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
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
