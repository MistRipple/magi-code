use crate::state::ApiState;
use magi_core::{SessionId, TaskId};

pub fn finalize_background_session_task_turn_if_root_completed(
    state: &ApiState,
    session_id: &SessionId,
    root_task_id: &TaskId,
) -> bool {
    magi_conversation_runtime::session_turn_finalize::finalize_background_session_task_turn_if_root_completed(
        state.session_store.as_ref(),
        &state.event_bus,
        state.task_store(),
        session_id,
        root_task_id,
    )
}

pub fn finalize_background_session_task_turn_if_root_terminal(
    state: &ApiState,
    session_id: &SessionId,
    root_task_id: &TaskId,
    runner_status: &str,
) -> bool {
    magi_conversation_runtime::session_turn_finalize::finalize_background_session_task_turn_if_root_terminal(
        state.session_store.as_ref(),
        &state.event_bus,
        state.task_store(),
        session_id,
        root_task_id,
        runner_status,
    )
}

pub fn reconcile_terminal_session_task_turns(state: &ApiState) -> usize {
    magi_conversation_runtime::session_turn_finalize::reconcile_terminal_session_task_turns(
        state.session_store.as_ref(),
        &state.event_bus,
        state.task_store(),
    )
}
