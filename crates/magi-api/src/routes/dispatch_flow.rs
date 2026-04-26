use magi_core::{EventId, SessionId, TaskStatus, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_session_store::{ActiveExecutionTurn, TimelineEntryKind};
use serde_json::json;

use super::{
    monotonic_accepted_at, new_session_id,
    session_scope::{resolve_session_workspace_binding, session_workspace_id},
};
use crate::{
    dto::SessionTurnRequestDto,
    errors::ApiError,
    session_turn_writeback::build_completed_turn_timeline_snapshot,
    state::ApiState,
    task_execution::{
        DispatchSubmissionAccepted, DispatchSubmissionRequest, drive_shadow_dispatch_submission,
        submit_shadow_dispatch_submission,
    },
};

pub(super) fn accept_session_task_submission(
    state: &ApiState,
    request: &SessionTurnRequestDto,
    task_title: Option<String>,
    execution_goal: Option<String>,
) -> Result<(DispatchSubmissionAccepted, EventId), ApiError> {
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
        request.requested_session_id(),
        request.requested_workspace_id().map(WorkspaceId::new),
        mission_title,
        message,
        trimmed_text,
        execution_goal,
        request.deep_task,
        request.skill_name.clone(),
        None,
        request,
    )
}

fn execute_dispatch_submission(
    state: &ApiState,
    requested_session_id: Option<SessionId>,
    requested_workspace_id: Option<WorkspaceId>,
    mission_title: String,
    message: String,
    trimmed_text: Option<String>,
    execution_goal: Option<String>,
    deep_task: bool,
    skill_name: Option<String>,
    target_role: Option<String>,
    request: &SessionTurnRequestDto,
) -> Result<(DispatchSubmissionAccepted, EventId), ApiError> {
    let accepted_at = monotonic_accepted_at();
    let (session_id, created_session, workspace_id) = resolve_dispatch_session(
        state,
        requested_session_id,
        requested_workspace_id,
        &mission_title,
        accepted_at,
    )?;
    append_session_user_message(state, &session_id, accepted_at, &message);
    let action_task_title = format!("执行: {}", mission_title);

    let dispatch = DispatchSubmissionRequest {
        accepted_at,
        session_id: session_id.clone(),
        workspace_id: workspace_id.clone(),
        entry_id: format!("timeline-{}-{}", session_id, accepted_at.0),
        created_session,
        mission_title,
        task_title: action_task_title,
        trimmed_text,
        execution_goal,
        deep_task,
        skill_name,
        target_role,
    };
    let accepted = submit_shadow_dispatch_submission(state, dispatch)?;
    let event_id = publish_session_turn_task_accepted_event(state, request, &accepted)?;
    state.persist_session_durable_state()?;
    Ok((accepted, event_id))
}

pub(super) fn spawn_session_task_dispatch(state: ApiState, accepted: DispatchSubmissionAccepted) {
    tokio::task::spawn_blocking(move || {
        let mut accepted = accepted;
        if let Err(error) = drive_shadow_dispatch_submission(&state, &mut accepted) {
            tracing::error!(
                session_id = %accepted.session_id,
                root_task_id = %accepted.root_task_id,
                action_task_id = %accepted.action_task_id,
                ?error,
                "session turn task background dispatch failed"
            );
            let _ = state.persist_session_durable_state();
            return;
        }
        append_dispatch_assistant_message(&state, &accepted);
        if let Err(error) = state.persist_session_durable_state() {
            tracing::error!(
                session_id = %accepted.session_id,
                root_task_id = %accepted.root_task_id,
                ?error,
                "session turn task background dispatch persist failed"
            );
        }
    });
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
            "workspace_id": request.requested_workspace_id(),
            "text": request.trimmed_text(),
            "skill_name": request.skill_name.clone(),
            "image_count": request.images.len(),
            "deep_task": request.deep_task,
            "created_session": accepted.created_session,
            "root_task_id": accepted.root_task_id.to_string(),
            "action_task_id": accepted.action_task_id.to_string(),
            "runner_started": accepted.runner_started,
        }),
    )
    .with_context(EventContext {
        session_id: Some(accepted.session_id.clone()),
        workspace_id,
        ..EventContext::default()
    });
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("事件发布失败", err))?;
    Ok(event_id)
}

pub(super) fn resolve_dispatch_session(
    state: &ApiState,
    requested_session_id: Option<SessionId>,
    requested_workspace_id: Option<WorkspaceId>,
    mission_title: &str,
    _accepted_at: UtcMillis,
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

    if let Some(session) = state.session_store.current_session() {
        if requested_workspace_id.as_ref().is_none_or(|workspace_id| {
            session_workspace_id(state, &session).as_ref() == Some(workspace_id)
        }) {
            let workspace_id = resolve_session_workspace_binding(
                state,
                &session,
                requested_workspace_id.as_ref(),
            )?;
            return Ok((session.session_id, false, workspace_id));
        }
    }

    let session_id = new_session_id();
    state
        .session_store
        .create_session_for_workspace(
            session_id.clone(),
            mission_title.to_string(),
            requested_workspace_id.as_ref().map(ToString::to_string),
        )
        .map_err(|err| ApiError::internal_assembly("创建会话失败", err))?;
    Ok((session_id, true, requested_workspace_id))
}

pub(super) fn append_session_user_message(
    state: &ApiState,
    session_id: &SessionId,
    accepted_at: UtcMillis,
    message: &str,
) {
    state.session_store.append_timeline_entry(
        session_id.clone(),
        TimelineEntryKind::UserMessage,
        message.to_string(),
    );

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
            ..EventContext::default()
        }),
    );
}

pub(super) fn append_dispatch_assistant_message(
    state: &ApiState,
    accepted: &DispatchSubmissionAccepted,
) {
    let Some(task_store) = state.task_store() else {
        return;
    };
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
    let current_turn_matches = current_turn
        .as_ref()
        .is_some_and(|turn| turn_matches_accepted_dispatch(turn, accepted));
    let is_followup_on_existing_turn = current_turn
        .as_ref()
        .is_some_and(|turn| current_turn_matches && turn.accepted_at != accepted.accepted_at);
    let response_text = current_turn_matches
        .then(|| {
            current_turn
                .clone()
                .and_then(|turn| assistant_final_from_turn(turn, accepted))
        })
        .flatten();

    let Some(response_text) = response_text else {
        return;
    };

    // 首次执行使用任务维度的稳定 entry，继续同一任务链时使用轮次维度 entry，
    // 避免覆盖中断前已经完成的主线回复。
    let streaming_entry_id = if is_followup_on_existing_turn {
        format!(
            "timeline-streaming-{}-{}",
            accepted.action_task_id, accepted.accepted_at.0
        )
    } else {
        format!("timeline-streaming-{}", accepted.action_task_id)
    };
    let _ = state
        .session_store
        .update_current_turn_status(&accepted.session_id, "completed");
    let timeline_message = build_completed_turn_timeline_snapshot(
        state.session_store.as_ref(),
        &accepted.session_id,
        Some(&response_text),
        Some(&streaming_entry_id),
    )
    .unwrap_or_else(|| response_text.clone());
    state.session_store.upsert_timeline_entry(
        accepted.session_id.clone(),
        &streaming_entry_id,
        TimelineEntryKind::AssistantMessage,
        timeline_message,
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
) -> Option<String> {
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
                .map(|content| (item.item_seq, content))
        })
        .max_by_key(|(item_seq, _)| *item_seq)
        .map(|(_, content)| content)
}
