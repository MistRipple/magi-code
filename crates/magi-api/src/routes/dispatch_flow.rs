use magi_core::{EventId, SessionId, TaskStatus, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_session_store::ActiveExecutionTurn;
use serde_json::json;

use super::{
    conversation_bridge::ingest_user_input_to_conversation,
    monotonic_accepted_at, new_session_id,
    session_scope::{resolve_session_workspace_binding, session_workspace_id},
};
use crate::{
    dto::SessionTurnRequestDto,
    errors::ApiError,
    state::ApiState,
    task_dispatch::{
        DispatchSubmissionAccepted, DispatchSubmissionRequest, drive_dispatch_submission,
        submit_dispatch_submission,
    },
};
use magi_conversation_runtime::session_writeback::publish_current_session_turn_item_event;

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
        state.resolve_workspace_id_from_request(
            request.requested_workspace_id().map(WorkspaceId::new),
            request.requested_workspace_path().as_deref(),
        ),
        mission_title,
        message,
        trimmed_text,
        execution_goal,
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
    let signal = ingest_user_input_to_conversation(state, &session_id, request, accepted_at);
    let user_timeline_entry_id = format!("timeline-{}-{}", session_id, accepted_at.0);
    let action_task_title = format_action_task_title(&mission_title);

    let dispatch = DispatchSubmissionRequest {
        accepted_at,
        session_id: session_id.clone(),
        workspace_id: workspace_id.clone(),
        entry_id: user_timeline_entry_id,
        timeline_message: message.clone(),
        created_session,
        mission_title,
        task_title: action_task_title,
        trimmed_text,
        execution_goal,
        skill_name,
        target_role,
        request_id: signal.request_id.clone(),
        user_message_id: signal.user_message_id.clone(),
        placeholder_message_id: signal.placeholder_message_id.clone(),
    };
    let accepted = submit_dispatch_submission(state, dispatch)?;
    publish_session_user_message_event(
        state,
        &session_id,
        workspace_id.clone(),
        accepted_at,
        &message,
    );
    let event_id = publish_session_turn_task_accepted_event(state, request, &accepted)?;
    state.persist_session_durable_state()?;
    Ok((accepted, event_id))
}

pub(super) fn finalize_session_task_dispatch(
    state: ApiState,
    accepted: DispatchSubmissionAccepted,
) {
    let mut accepted = accepted;
    if let Err(error) = drive_dispatch_submission(&state, &mut accepted) {
        tracing::error!(
            session_id = %accepted.session_id,
            root_task_id = %accepted.root_task_id,
            action_task_id = %accepted.action_task_id,
            ?error,
            "session turn task dispatch failed"
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
            "session turn task dispatch persist failed"
        );
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

#[cfg(test)]
mod tests {
    use super::format_action_task_title;

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
            "request_id": request.request_id(),
            "user_message_id": request.user_message_id(),
            "placeholder_message_id": request.placeholder_message_id(),
            "image_count": request.images.len(),
            "created_session": accepted.created_session,
            "route": "task",
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
