use crate::state::ApiState;
use magi_browser_authority::BrowserLeaseEndReason;
use magi_conversation_runtime::session_writeback::SessionStatePersistCallback;
use magi_core::{SessionId, TaskId, public_runtime_excerpt};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

const INTERMEDIATE_PERSIST_DEBOUNCE: Duration = Duration::from_millis(100);

type PersistSessionState = dyn Fn(&str) -> Result<(), String> + Send + Sync;

#[derive(Default)]
struct SessionStatePersistenceState {
    pending: Option<(&'static str, u64)>,
    worker_active: bool,
    generation: u64,
}

struct SessionStatePersistenceScheduler {
    persist: Arc<PersistSessionState>,
    state: Mutex<SessionStatePersistenceState>,
    idle: Condvar,
    write_lock: Mutex<()>,
}

impl SessionStatePersistenceScheduler {
    fn new(state: &ApiState) -> Arc<Self> {
        let state = state.clone();
        Self::with_persist(Arc::new(move |checkpoint| {
            state
                .persist_session_state_checkpoint(checkpoint)
                .map_err(|error| format!("{error:?}"))
        }))
    }

    fn with_persist(persist: Arc<PersistSessionState>) -> Arc<Self> {
        Arc::new(Self {
            persist,
            state: Mutex::new(SessionStatePersistenceState::default()),
            idle: Condvar::new(),
            write_lock: Mutex::new(()),
        })
    }

    fn request(self: &Arc<Self>, checkpoint: &'static str) {
        let mut state = self
            .state
            .lock()
            .expect("session persistence state lock poisoned");
        state.pending = Some((checkpoint, state.generation));
        if state.worker_active {
            return;
        }
        state.worker_active = true;
        drop(state);

        let scheduler = Arc::clone(self);
        if let Err(error) = thread::Builder::new()
            .name("magi-session-persist".to_string())
            .spawn(move || scheduler.run_worker())
        {
            tracing::warn!(%error, "启动 session 状态持久化线程失败，改为当前线程落盘");
            let pending = {
                let mut state = self
                    .state
                    .lock()
                    .expect("session persistence state lock poisoned");
                state.worker_active = false;
                let pending = state.pending.take();
                self.idle.notify_all();
                pending
            };
            if let Some((checkpoint, generation)) = pending {
                self.persist_intermediate(checkpoint, generation);
            }
        }
    }

    fn run_worker(self: Arc<Self>) {
        loop {
            thread::sleep(INTERMEDIATE_PERSIST_DEBOUNCE);
            let checkpoint = {
                let mut state = self
                    .state
                    .lock()
                    .expect("session persistence state lock poisoned");
                if state.pending.is_none() {
                    state.worker_active = false;
                    self.idle.notify_all();
                    return;
                }
                state.pending.take()
            };

            if let Some((checkpoint, generation)) = checkpoint {
                self.persist_intermediate(checkpoint, generation);
            }
        }
    }

    fn persist_intermediate(&self, checkpoint: &'static str, generation: u64) {
        let _write_guard = self
            .write_lock
            .lock()
            .expect("session persistence write lock poisoned");
        if self
            .state
            .lock()
            .expect("session persistence state lock poisoned")
            .generation
            != generation
        {
            return;
        }
        if let Err(error) = (self.persist)(checkpoint) {
            tracing::warn!(checkpoint, %error, "异步 session 状态持久化失败");
        }
    }

    fn persist_terminal(&self, checkpoint: &'static str) {
        {
            let mut state = self
                .state
                .lock()
                .expect("session persistence state lock poisoned");
            state.generation = state.generation.saturating_add(1);
            state.pending = None;
        }
        let _write_guard = self
            .write_lock
            .lock()
            .expect("session persistence write lock poisoned");
        if let Err(error) = (self.persist)(checkpoint) {
            tracing::warn!(checkpoint, %error, "session task turn 终态持久化失败");
        }
    }

    #[cfg(test)]
    fn wait_until_idle(&self, timeout: Duration) -> bool {
        let state = self
            .state
            .lock()
            .expect("session persistence state lock poisoned");
        let (state, _) = self
            .idle
            .wait_timeout_while(state, timeout, |state| state.worker_active)
            .expect("session persistence idle wait lock poisoned");
        !state.worker_active
    }
}

pub fn session_state_persist_callback(state: &ApiState) -> Arc<SessionStatePersistCallback> {
    let scheduler = SessionStatePersistenceScheduler::new(state);
    Arc::new(move |checkpoint: &str| {
        let checkpoint: &'static str = match checkpoint {
            "session_turn_completed"
            | "session_turn_failed"
            | "session_task_chain_archived"
            | "session_turn_final_item" => {
                // 这些 checkpoint 是终态或用户可见最终结果，必须在回调返回前落盘。
                match checkpoint {
                    "session_turn_completed" => "session_turn_completed",
                    "session_turn_failed" => "session_turn_failed",
                    "session_task_chain_archived" => "session_task_chain_archived",
                    "session_turn_final_item" => "session_turn_final_item",
                    _ => unreachable!(),
                }
            }
            other => {
                // 工具开始/结果、思考流和进度通知会在一个 Turn 内高频产生；将它们
                // 合并到 100ms 窗口内，避免每个事件都同步序列化全量 sessions.json。
                let checkpoint = match other {
                    "session_turn_tool_result" => "session_turn_tool_result",
                    "session_turn_tool" => "session_turn_tool",
                    "session_goal_tool" => "session_goal_tool",
                    "task_turn_tool_started" => "task_turn_tool_started",
                    "task_turn_tool_result" => "task_turn_tool_result",
                    "task_turn_final_item" => "task_turn_final_item",
                    "task_turn_completed" => "task_turn_completed",
                    "task_turn_failed" => "task_turn_failed",
                    "session_task_turn_completed" => "session_task_turn_completed",
                    "session_task_turn_failed" => "session_task_turn_failed",
                    "context_compaction_notice" => "context_compaction_notice",
                    "session_turn_thread_user" => "session_turn_thread_user",
                    "session_turn_stream_interrupted_content" => {
                        "session_turn_stream_interrupted_content"
                    }
                    _ => "session_turn_progress",
                };
                scheduler.request(checkpoint);
                return;
            }
        };
        scheduler.persist_terminal(checkpoint);
    })
}

pub fn finalize_background_session_task_turn_if_root_completed(
    state: &ApiState,
    session_id: &SessionId,
    root_task_id: &TaskId,
) -> bool {
    release_terminal_browser_resources(state, session_id, root_task_id);
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
        state.release_session_git_execution_lease(session_id);
        crate::routes::sessions::record_active_goal_turn_success(
            state,
            session_id,
            root_task_id.as_str(),
        );
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
    release_terminal_browser_resources(state, session_id, root_task_id);
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
        let owns_active_plan = state
            .session_store
            .active_plan_for_execution_owner(session_id, root_task_id.as_str())
            .is_some();
        state.release_session_git_execution_lease(session_id);
        let root_completed = state
            .task_store()
            .and_then(|task_store| task_store.get_task(root_task_id))
            .is_some_and(|task| task.status == magi_core::TaskStatus::Completed);
        if root_completed {
            crate::routes::sessions::record_active_goal_turn_success(
                state,
                session_id,
                root_task_id.as_str(),
            );
        } else {
            let failure_reason = state
                .task_store()
                .and_then(|task_store| task_store.get_task(root_task_id))
                .and_then(|task| {
                    task.output_refs
                        .into_iter()
                        .find(|value| !value.trim().is_empty())
                })
                .map(|value| public_runtime_excerpt(&value, 4096))
                .unwrap_or_else(|| runner_status.to_string());
            crate::routes::sessions::record_active_goal_turn_failure(
                state,
                session_id,
                root_task_id.as_str(),
                &failure_reason,
            );
        }
        if runner_status != "completed" && !root_completed && owns_active_plan {
            let plan_store =
                magi_plan::PlanStore::new(state.session_store.clone(), session_id.clone());
            match plan_store.pause() {
                Ok(Some(plan)) => {
                    let workspace_id = state
                        .session_store
                        .session(session_id)
                        .and_then(|session| session.workspace_id)
                        .map(magi_core::WorkspaceId::new);
                    magi_plan::publish_plan_event(
                        &state.event_bus,
                        magi_plan::plan_event_type(&plan),
                        &plan,
                        workspace_id.as_ref(),
                        Some(root_task_id),
                        None,
                    );
                    if let Err(error) =
                        state.persist_session_state_checkpoint("session_task_turn_plan_paused")
                    {
                        tracing::warn!(?error, "任务失败后计划暂停状态持久化失败");
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(session_id = %session_id, %error, "任务失败后暂停计划失败");
                }
            }
        }
        schedule_next_queued_session_turn(state, session_id);
    }
    finalized
}

fn release_terminal_browser_resources(
    state: &ApiState,
    session_id: &SessionId,
    root_task_id: &TaskId,
) {
    // Turn 终态只结束代理控制权。BrowserAuthority 中的会话、标签和当前 URL
    // 属于用户可继续查看的任务结果，只有显式关闭操作才能销毁。
    let report = state.cancel_execution_resources(
        Some(session_id),
        None,
        None,
        BrowserLeaseEndReason::TaskFinished,
    );
    if report.browser_lease_count > 0 {
        tracing::debug!(
            %session_id,
            %root_task_id,
            browser_lease_count = report.browser_lease_count,
            "session turn entered terminal state, released browser leases"
        );
    }
}

fn schedule_next_queued_session_turn(state: &ApiState, session_id: &SessionId) {
    let workspace_id = state
        .session_store
        .session(session_id)
        .and_then(|session| state.session_workspace_id(&session));
    crate::routes::sessions::schedule_next_queued_regular_session_turn(
        state.clone(),
        session_id.clone(),
        workspace_id,
    );
}

pub fn schedule_restored_session_turn_queues(state: &ApiState) -> usize {
    let mut session_ids = state.queued_regular_session_ids();
    for session_id in state.session_store.waiting_goal_session_ids() {
        if !session_ids.contains(&session_id) {
            session_ids.push(session_id);
        }
    }
    for session_id in &session_ids {
        schedule_next_queued_session_turn(state, session_id);
    }
    session_ids.len()
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
    use magi_core::{
        AccessProfile, MissionId, Task, TaskKind, TaskRuntimePayload, TaskStatus, UtcMillis,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_orchestrator::task_store::TaskStore;
    use magi_session_store::{
        ActiveExecutionChain, ActiveExecutionDispatchContext, ActiveExecutionTurn,
        GoalContinuationPhase, SessionStore, TimelineEntryInput, TimelineEntryKind,
    };
    use magi_workspace::WorkspaceStore;

    #[test]
    fn persistence_scheduler_coalesces_burst_into_one_write() {
        let writes = Arc::new(Mutex::new(Vec::<String>::new()));
        let writes_for_persist = Arc::clone(&writes);
        let scheduler =
            SessionStatePersistenceScheduler::with_persist(Arc::new(move |checkpoint| {
                writes_for_persist
                    .lock()
                    .expect("writes lock poisoned")
                    .push(checkpoint.to_string());
                Ok(())
            }));

        for _ in 0..50 {
            scheduler.request("task_turn_tool_result");
        }

        assert!(scheduler.wait_until_idle(Duration::from_secs(2)));
        assert_eq!(
            writes.lock().expect("writes lock poisoned").as_slice(),
            ["task_turn_tool_result"]
        );
    }

    #[test]
    fn terminal_write_cancels_older_pending_checkpoint() {
        let writes = Arc::new(Mutex::new(Vec::<String>::new()));
        let writes_for_persist = Arc::clone(&writes);
        let scheduler =
            SessionStatePersistenceScheduler::with_persist(Arc::new(move |checkpoint| {
                writes_for_persist
                    .lock()
                    .expect("writes lock poisoned")
                    .push(checkpoint.to_string());
                Ok(())
            }));

        for _ in 0..50 {
            scheduler.request("task_turn_tool_result");
        }
        scheduler.persist_terminal("session_turn_completed");

        assert!(scheduler.wait_until_idle(Duration::from_secs(2)));
        assert_eq!(
            writes.lock().expect("writes lock poisoned").as_slice(),
            ["session_turn_completed"]
        );
    }

    #[tokio::test]
    async fn restored_waiting_goal_is_included_in_startup_scheduling() {
        let session_store = Arc::new(SessionStore::new());
        let session_id = SessionId::new("session-restored-waiting-goal");
        session_store
            .create_session(session_id.clone(), "restored waiting goal")
            .expect("session should create");
        let (_, thread_id) =
            session_store.ensure_session_mission(&session_id, UtcMillis::now(), || {
                MissionId::new("mission-restored-waiting-goal")
            });
        let goal = session_store
            .create_goal(
                session_id.clone(),
                thread_id,
                "turn-restored-waiting-goal",
                "验证启动时恢复 waiting Goal",
                AccessProfile::Restricted,
                None,
            )
            .expect("goal should create");
        session_store
            .mark_goal_continuation_waiting(&session_id, &goal.goal_id, "resume_requested")
            .expect("goal should enter waiting");
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(16)),
            session_store,
            Arc::new(WorkspaceStore::new()),
            Arc::new(GovernanceService::default()),
        );

        assert_eq!(schedule_restored_session_turn_queues(&state), 1);
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn failed_task_terminalization_pauses_session_plan() {
        let session_store = Arc::new(SessionStore::new());
        let session_id = SessionId::new("session-failed-task-plan-pause");
        let root_task_id = TaskId::new("task-failed-task-plan-pause");
        let mission_id = MissionId::new("mission-failed-task-plan-pause");
        let now = UtcMillis::now();
        session_store
            .create_session(session_id.clone(), "failed task plan pause")
            .expect("session should create");
        session_store.ensure_session_mission(&session_id, now, || mission_id.clone());
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-failed-task-plan-pause".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![root_task_id.clone()],
                    active_worker_bindings: Vec::new(),
                    branches: Vec::new(),
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "entry-failed-task-plan-pause".to_string(),
                        trimmed_text: Some("执行失败任务".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-failed-task-plan-pause".to_string(),
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
        let plan_store = magi_plan::PlanStore::new(session_store.clone(), session_id.clone());
        plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: None,
                expected_goal_control_revision: None,
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("execute-current-step".to_string()),
                    step: "执行当前步骤".to_string(),
                    status: magi_core::PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should persist");
        let task_store = Arc::new(TaskStore::new());
        task_store.insert_task(Task {
            task_id: root_task_id.clone(),
            mission_id,
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: "失败任务".to_string(),
            goal: "验证失败后计划收敛".to_string(),
            status: TaskStatus::Failed,
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
        let plan = plan_store.snapshot().expect("plan should remain visible");
        assert_eq!(plan.state, magi_core::PlanState::Paused);
        assert_eq!(plan.items[0].status, magi_core::PlanItemStatus::InProgress);
    }

    #[tokio::test]
    async fn fast_completed_goal_turn_releases_continuation_before_scheduling_next_turn() {
        let session_store = Arc::new(SessionStore::new());
        let session_id = SessionId::new("session-fast-goal-completion");
        let root_task_id = TaskId::new("task-fast-goal-completion");
        let mission_id = MissionId::new("mission-fast-goal-completion");
        let now = UtcMillis::now();
        session_store
            .create_session(session_id.clone(), "fast goal completion")
            .expect("session should create");
        let (_, orchestrator_thread_id) =
            session_store.ensure_session_mission(&session_id, now, || mission_id.clone());
        let goal = session_store
            .create_goal(
                session_id.clone(),
                orchestrator_thread_id,
                "task-goal-creator",
                "验证快速完成续跑",
                AccessProfile::Restricted,
                None,
            )
            .expect("goal should create");
        session_store
            .accept_goal_continuation_with_timeline_entry(
                session_id.clone(),
                &goal.goal_id,
                TimelineEntryInput::new(
                    "timeline-fast-goal-completion",
                    TimelineEntryKind::NotificationPublished,
                    "目标自动推进",
                    now,
                ),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-fast-goal-completion".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![root_task_id.clone()],
                    active_worker_bindings: Vec::new(),
                    branches: Vec::new(),
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-fast-goal-completion".to_string(),
                        trimmed_text: None,
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-fast-goal-completion".to_string(),
                        turn_seq: now.0,
                        accepted_at: now,
                        status: "running".to_string(),
                        completed_at: None,
                        user_message: None,
                        items: Vec::new(),
                    }),
                },
            )
            .expect("goal continuation should be accepted");
        assert_eq!(
            session_store
                .current_goal(&session_id)
                .expect("goal should remain")
                .continuation
                .phase,
            GoalContinuationPhase::Running
        );

        let task_store = Arc::new(TaskStore::new());
        task_store.insert_task(Task {
            task_id: root_task_id.clone(),
            mission_id,
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: "快速完成任务".to_string(),
            goal: "验证快速完成路径释放 Goal continuation".to_string(),
            status: TaskStatus::Completed,
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
            output_refs: vec!["快速完成路径已验证".to_string()],
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        });
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            session_store.clone(),
            Arc::new(WorkspaceStore::new()),
            Arc::new(GovernanceService::default()),
        )
        .with_task_store(task_store);

        assert!(finalize_background_session_task_turn_if_root_completed(
            &state,
            &session_id,
            &root_task_id,
        ));
        let goal = session_store
            .current_goal(&session_id)
            .expect("goal should remain active");
        assert_eq!(goal.continuation.phase, GoalContinuationPhase::Idle);
        assert!(goal.continuation.turn_id.is_none());
    }
}
