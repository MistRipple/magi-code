use crate::state::ApiState;
use magi_conversation_runtime::session_writeback::SessionStatePersistCallback;
use magi_core::{SessionId, TaskId};
use std::sync::Arc;

fn session_state_persist_callback(state: &ApiState) -> Arc<SessionStatePersistCallback> {
    let state_for_persist = state.clone();
    Arc::new(move |checkpoint: &str| {
        if let Err(error) = state_for_persist.persist_session_state_checkpoint(checkpoint) {
            tracing::warn!(checkpoint, ?error, "session task turn 终态持久化失败");
        }
    })
}

pub fn finalize_background_session_task_turn_if_root_completed(
    state: &ApiState,
    session_id: &SessionId,
    root_task_id: &TaskId,
) -> bool {
    let persist_session_state = session_state_persist_callback(state);
    magi_conversation_runtime::session_turn_finalize::finalize_background_session_task_turn_if_root_completed(
        state.session_store.as_ref(),
        &state.event_bus,
        state.task_store(),
        session_id,
        root_task_id,
        Some(persist_session_state.as_ref()),
    )
}

pub fn finalize_background_session_task_turn_if_root_terminal(
    state: &ApiState,
    session_id: &SessionId,
    root_task_id: &TaskId,
    runner_status: &str,
) -> bool {
    let persist_session_state = session_state_persist_callback(state);
    magi_conversation_runtime::session_turn_finalize::finalize_background_session_task_turn_if_root_terminal(
        state.session_store.as_ref(),
        &state.event_bus,
        state.task_store(),
        session_id,
        root_task_id,
        runner_status,
        Some(persist_session_state.as_ref()),
    )
}

pub fn reconcile_terminal_session_task_turns(state: &ApiState) -> usize {
    magi_conversation_runtime::session_turn_finalize::reconcile_terminal_session_task_turns(
        state.session_store.as_ref(),
        &state.event_bus,
        state.task_store(),
    )
}
