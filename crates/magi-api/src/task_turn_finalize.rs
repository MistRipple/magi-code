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
    let finalized = magi_conversation_runtime::session_turn_finalize::finalize_background_session_task_turn_if_root_completed(
        state.session_store.as_ref(),
        &state.event_bus,
        state.task_store(),
        session_id,
        root_task_id,
        Some(persist_session_state.as_ref()),
    );
    if finalized {
        schedule_next_queued_session_turn(state, session_id);
    }
    finalized
}

pub fn finalize_background_session_task_turn_if_root_terminal(
    state: &ApiState,
    session_id: &SessionId,
    root_task_id: &TaskId,
    runner_status: &str,
) -> bool {
    let persist_session_state = session_state_persist_callback(state);
    let finalized = magi_conversation_runtime::session_turn_finalize::finalize_background_session_task_turn_if_root_terminal(
        state.session_store.as_ref(),
        &state.event_bus,
        state.task_store(),
        session_id,
        root_task_id,
        runner_status,
        Some(persist_session_state.as_ref()),
    );
    if finalized {
        let root_completed = state
            .task_store()
            .and_then(|task_store| task_store.get_task(root_task_id))
            .is_some_and(|task| task.status == magi_core::TaskStatus::Completed);
        if runner_status != "completed" && !root_completed {
            let todo_ledger =
                magi_todo_ledger::TodoLedger::new(state.session_store.clone(), session_id.clone());
            match todo_ledger.pause_in_progress() {
                Ok(true) => {
                    if let Err(error) =
                        state.persist_session_state_checkpoint("session_task_turn_todo_paused")
                    {
                        tracing::warn!(?error, "任务失败后 TodoLedger 暂停状态持久化失败");
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(session_id = %session_id, %error, "任务失败后暂停 TodoLedger 进行项失败");
                }
            }
        }
        schedule_next_queued_session_turn(state, session_id);
    }
    finalized
}

fn schedule_next_queued_session_turn(state: &ApiState, session_id: &SessionId) {
    let workspace_id = state
        .session_store
        .execution_ownership(session_id)
        .and_then(|ownership| ownership.workspace_id);
    crate::routes::sessions::schedule_next_queued_regular_session_turn(
        state.clone(),
        session_id.clone(),
        workspace_id,
    );
}

pub fn reconcile_terminal_session_task_turns(state: &ApiState) -> usize {
    magi_conversation_runtime::session_turn_finalize::reconcile_terminal_session_task_turns(
        state.session_store.as_ref(),
        &state.event_bus,
        state.task_store(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{MissionId, Task, TaskKind, TaskRuntimePayload, TaskStatus, UtcMillis};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_orchestrator::task_store::TaskStore;
    use magi_session_store::{
        ActiveExecutionChain, ActiveExecutionDispatchContext, ActiveExecutionTurn, SessionStore,
    };
    use magi_workspace::WorkspaceStore;

    #[tokio::test]
    async fn failed_task_terminalization_pauses_session_todo() {
        let session_store = Arc::new(SessionStore::new());
        let session_id = SessionId::new("session-failed-task-todo-pause");
        let root_task_id = TaskId::new("task-failed-task-todo-pause");
        let mission_id = MissionId::new("mission-failed-task-todo-pause");
        let now = UtcMillis::now();
        session_store
            .create_session(session_id.clone(), "failed task todo pause")
            .expect("session should create");
        session_store.ensure_session_mission(&session_id, now, || mission_id.clone());
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-failed-task-todo-pause".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![root_task_id.clone()],
                    active_worker_bindings: Vec::new(),
                    branches: Vec::new(),
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "entry-failed-task-todo-pause".to_string(),
                        trimmed_text: Some("执行失败任务".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-failed-task-todo-pause".to_string(),
                        turn_seq: now.0,
                        accepted_at: now,
                        status: "running".to_string(),
                        completed_at: None,
                        user_message: Some("执行失败任务".to_string()),
                        items: Vec::new(),
                    }),
                },
            )
            .expect("active chain should persist");
        let todo_ledger =
            magi_todo_ledger::TodoLedger::new(session_store.clone(), session_id.clone());
        todo_ledger
            .write(vec![magi_core::TodoItem::new(
                "执行当前步骤",
                "正在执行当前步骤",
                magi_core::TodoStatus::InProgress,
            )])
            .expect("todo should persist");
        let task_store = Arc::new(TaskStore::new());
        task_store.insert_task(Task {
            task_id: root_task_id.clone(),
            mission_id,
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: "失败任务".to_string(),
            goal: "验证失败后 Todo 收敛".to_string(),
            status: TaskStatus::Failed,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: vec!["模型请求未完成".to_string()],
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        });
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            session_store,
            Arc::new(WorkspaceStore::new()),
            Arc::new(GovernanceService::default()),
        )
        .with_task_store(task_store);

        assert!(finalize_background_session_task_turn_if_root_terminal(
            &state,
            &session_id,
            &root_task_id,
            "error",
        ));
        assert_eq!(
            todo_ledger.snapshot()[0].status,
            magi_core::TodoStatus::Pending
        );
    }
}
