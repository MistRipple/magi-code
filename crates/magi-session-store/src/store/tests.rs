use super::*;
use crate::models::{
    ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
    ActiveExecutionTurn, ActiveExecutionTurnItem, CanonicalTurnStatus, ExecutionThread,
    ExecutionThreadStatus, GoalContinuationPhase, GoalContinuationState, GoalRevisionExpectation,
    GoalStatus, NotificationContext, NotificationRecord, NotificationScope, SessionDurableState,
    SessionExecutionSidecarStatus, SessionExecutionSidecarStoreState, SessionPlan,
    SessionSidecarFlushReason, SessionStoreState, ThreadChatMessage, ThreadChatToolCall,
    ThreadChatToolFunction, ThreadContextCheckpoint, ThreadFileFactVersion,
    ThreadModelProviderContext,
};
use magi_core::{
    AccessProfile, ExecutionOwnership, MissionId, PlanId, PlanItem, PlanItemId, PlanItemStatus,
    PlanState, RecoveryResumeInput, SessionId, TaskExecutionTarget, TaskId, ThreadId, UtcMillis,
    WorkerId, WorkspaceId,
};
use serde_json::json;
use std::{collections::HashMap, thread, time::Duration};

fn create_test_goal(
    store: &SessionStore,
    session_id: &SessionId,
    turn_id: &str,
    objective: &str,
    token_budget: Option<u64>,
) -> crate::models::SessionGoal {
    let (_, thread_id) = store.ensure_session_mission(session_id, UtcMillis::now(), || {
        MissionId::new(format!("mission-{session_id}"))
    });
    store
        .create_goal(
            session_id.clone(),
            thread_id,
            turn_id,
            objective,
            AccessProfile::Restricted,
            token_budget,
        )
        .expect("test goal should be creatable")
}

fn test_turn(turn_id: &str, status: &str, accepted_at: u64) -> ActiveExecutionTurn {
    ActiveExecutionTurn {
        turn_id: turn_id.to_string(),
        turn_seq: accepted_at,
        accepted_at: UtcMillis(accepted_at),
        status: status.to_string(),
        completed_at: None,
        user_message: Some(format!("message for {turn_id}")),
        items: Vec::new(),
    }
}

fn test_active_chain(
    session_id: &SessionId,
    chain_ref: &str,
    turn: Option<ActiveExecutionTurn>,
) -> ActiveExecutionChain {
    ActiveExecutionChain {
        session_id: session_id.clone(),
        mission_id: MissionId::new(format!("mission-{chain_ref}")),
        root_task_id: TaskId::new(format!("task-root-{chain_ref}")),
        execution_chain_ref: chain_ref.to_string(),
        workspace_id: None,
        active_branch_task_ids: Vec::new(),
        active_worker_bindings: Vec::new(),
        branches: Vec::new(),
        recovery_ref: None,
        dispatch_context: ActiveExecutionDispatchContext {
            accepted_at: UtcMillis(10),
            entry_id: format!("timeline-{chain_ref}"),
            trimmed_text: Some(format!("text for {chain_ref}")),
            skill_name: None,
        },
        current_turn: turn,
    }
}

fn test_turn_item(item_id: &str, content: &str) -> ActiveExecutionTurnItem {
    ActiveExecutionTurnItem {
        item_id: item_id.to_string(),
        item_seq: 0,
        kind: "user_message".to_string(),
        status: "completed".to_string(),
        source: "user".to_string(),
        title: None,
        content: Some(content.to_string()),
        task_id: None,
        worker_id: None,
        role_id: None,
        tool_call_id: None,
        tool_name: None,
        tool_status: None,
        tool_arguments: None,
        tool_result: None,
        tool_error: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        metadata: Default::default(),
        timeline_entry_id: None,
        source_thread_id: ThreadId::new("thread-main-default"),
    }
}

#[test]
fn unique_timeline_entry_id_appends_suffix_for_duplicate_base() {
    let session_id = SessionId::new("session-duplicate-entry");
    let occurred_at = UtcMillis(42);
    let mut timeline = vec![TimelineEntry {
        entry_id: "timeline-session-duplicate-entry-42".to_string(),
        session_id: session_id.clone(),
        kind: TimelineEntryKind::UserMessage,
        message: "第一条并发消息".to_string(),
        occurred_at,
    }];

    let next =
        unique_timeline_entry_id(&timeline, "timeline-session-duplicate-entry-42".to_string());
    assert_eq!(next, "timeline-session-duplicate-entry-42-1");

    timeline.push(TimelineEntry {
        entry_id: next,
        session_id,
        kind: TimelineEntryKind::UserMessage,
        message: "第二条并发消息".to_string(),
        occurred_at,
    });

    assert_eq!(
        unique_timeline_entry_id(&timeline, "timeline-session-duplicate-entry-42".to_string()),
        "timeline-session-duplicate-entry-42-2"
    );
}

#[test]
fn rejected_user_turn_is_durable_and_request_idempotent() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-rejected-user-turn");
    store
        .create_session(session_id.clone(), "Rejected user turn")
        .expect("session should be creatable");

    let request_id = "request-git-context-conflict".to_string();
    let user_message_id = "user-git-context-conflict".to_string();
    let placeholder_message_id = "assistant-placeholder-git-context-conflict".to_string();
    let source_thread_id = ThreadId::new("thread-rejected-user-turn");
    let mut user_item = test_turn_item(&user_message_id, "发送前 Git context 已漂移");
    user_item.request_id = Some(request_id.clone());
    user_item.user_message_id = Some(user_message_id.clone());
    user_item.placeholder_message_id = Some(placeholder_message_id.clone());
    user_item
        .metadata
        .insert("requestId".to_string(), json!(request_id.clone()));
    user_item.source_thread_id = source_thread_id.clone();

    let mut error_item = test_turn_item(
        "assistant-error-git-context-conflict",
        "Git context 发生高风险变化",
    );
    error_item.kind = "assistant_error".to_string();
    error_item.status = "failed".to_string();
    error_item.source = "orchestrator".to_string();
    error_item.request_id = Some(request_id.clone());
    error_item.user_message_id = Some(user_message_id.clone());
    error_item.placeholder_message_id = Some(placeholder_message_id.clone());
    error_item.source_thread_id = source_thread_id;

    let turn = ActiveExecutionTurn {
        turn_id: "turn-rejected-user-turn".to_string(),
        turn_seq: 100,
        accepted_at: UtcMillis(100),
        completed_at: Some(UtcMillis(100)),
        status: "failed".to_string(),
        user_message: Some("发送前 Git context 已漂移".to_string()),
        items: vec![user_item, error_item],
    };
    let timeline = TimelineEntryInput::new(
        "timeline-rejected-user-turn",
        TimelineEntryKind::UserMessage,
        "发送前 Git context 已漂移",
        UtcMillis(100),
    );

    store
        .record_rejected_user_turn_with_timeline_entry(session_id.clone(), timeline, turn.clone())
        .expect("rejected turn should be durable")
        .expect("first rejected turn should be recorded");

    let turns = store.canonical_turns_for_session(&session_id);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].status, CanonicalTurnStatus::Failed);
    assert_eq!(turns[0].items.len(), 2);
    assert_eq!(
        store
            .timeline_for_session(&session_id)
            .into_iter()
            .filter(|entry| entry.entry_id == "timeline-rejected-user-turn")
            .count(),
        1
    );

    store
        .record_rejected_user_turn_with_timeline_entry(
            session_id.clone(),
            TimelineEntryInput::new(
                "timeline-rejected-user-turn-retry",
                TimelineEntryKind::UserMessage,
                "重复提交不应生成第二条",
                UtcMillis(101),
            ),
            turn,
        )
        .expect("same request retry should be idempotent");
    assert_eq!(store.canonical_turns_for_session(&session_id).len(), 1);
    assert_eq!(
        store
            .timeline_for_session(&session_id)
            .into_iter()
            .filter(|entry| entry.entry_id.starts_with("timeline-rejected-user-turn"))
            .count(),
        1
    );
}

#[test]
fn append_timeline_entry_updates_session_timestamp_and_user_message_count() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-message-count");
    let created = store
        .create_session(session_id.clone(), "message count session")
        .expect("session should be creatable");

    thread::sleep(Duration::from_millis(2));
    store.append_timeline_entry(
        session_id.clone(),
        TimelineEntryKind::UserMessage,
        "第一条用户消息",
    );
    store.append_timeline_entry(
        session_id.clone(),
        TimelineEntryKind::AssistantMessage,
        "这条助手消息不计入用户消息数",
    );
    store.append_timeline_entry(
        session_id.clone(),
        TimelineEntryKind::UserMessage,
        "第二条用户消息",
    );

    let session = store
        .session(&session_id)
        .expect("session should still exist after timeline append");
    assert_eq!(session.message_count, Some(2));
    assert!(
        session.updated_at.0 > created.updated_at.0,
        "追加时间线后应该刷新会话更新时间"
    );
}

#[test]
fn selecting_current_session_is_durable_without_changing_business_history() {
    let store = SessionStore::new();
    let first_session_id = SessionId::new("session-select-first");
    let second_session_id = SessionId::new("session-select-second");
    let first = store
        .create_session(first_session_id.clone(), "First")
        .expect("first session should create");
    store
        .create_session(second_session_id, "Second")
        .expect("second session should create");
    let timeline_before =
        serde_json::to_value(store.timeline()).expect("timeline should serialize");

    let selected = store
        .select_current_session(&first_session_id)
        .expect("existing session should be selectable");

    assert_eq!(selected.session_id, first_session_id);
    assert_eq!(selected.updated_at, first.updated_at);
    assert_eq!(
        serde_json::to_value(store.timeline()).expect("timeline should serialize"),
        timeline_before
    );

    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        SessionExecutionSidecarStoreState::default(),
    );
    assert_eq!(
        restored
            .current_session()
            .expect("selected session should restore")
            .session_id,
        first_session_id
    );
}

#[test]
fn clearing_current_session_is_durable_without_changing_business_history() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-clear-current");
    store
        .create_session(session_id.clone(), "Current")
        .expect("session should create");
    let timeline_before =
        serde_json::to_value(store.timeline()).expect("timeline should serialize");

    store.clear_current_session();

    assert!(store.current_session().is_none());
    assert_eq!(
        serde_json::to_value(store.timeline()).expect("timeline should serialize"),
        timeline_before
    );
    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        SessionExecutionSidecarStoreState::default(),
    );
    assert!(restored.current_session().is_none());
    assert!(restored.session(&session_id).is_some());
}

#[test]
fn rename_session_validates_title_and_skips_noop_history() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-rename-validation");
    let created = store
        .create_session(session_id.clone(), "原始名称")
        .expect("session should create");

    let renamed = store
        .rename_session(&session_id, "  新名称  ")
        .expect("valid title should rename");
    assert_eq!(renamed.title, "新名称");
    assert_eq!(
        store
            .timeline()
            .iter()
            .filter(|entry| matches!(&entry.kind, TimelineEntryKind::SessionRenamed))
            .count(),
        1
    );

    let unchanged = store
        .rename_session(&session_id, "新名称")
        .expect("same title should be a successful noop");
    assert_eq!(unchanged.updated_at, renamed.updated_at);
    assert_eq!(
        store
            .timeline()
            .iter()
            .filter(|entry| matches!(&entry.kind, TimelineEntryKind::SessionRenamed))
            .count(),
        1,
        "相同标题不能重复写入审计记录"
    );

    for invalid_title in [
        "".to_string(),
        "\n".to_string(),
        "包含\n换行".to_string(),
        std::iter::repeat_n('字', SESSION_TITLE_MAX_CHARS + 1).collect(),
    ] {
        assert!(matches!(
            store.rename_session(&session_id, invalid_title),
            Err(DomainError::Validation { .. })
        ));
    }
    assert_eq!(
        store
            .session(&session_id)
            .expect("session should remain")
            .title,
        "新名称"
    );
    assert!(created.updated_at.0 <= renamed.updated_at.0);
}

#[test]
fn durable_state_persistence_serializes_snapshot_and_write_transactions() {
    let store = SessionStore::new();
    let first_session_id = SessionId::new("session-persist-order-first");
    let second_session_id = SessionId::new("session-persist-order-second");
    store
        .create_session(first_session_id.clone(), "First")
        .expect("first session should create");
    store
        .create_session(second_session_id.clone(), "Second")
        .expect("second session should create");

    let snapshots = Arc::new(Mutex::new(Vec::<SessionId>::new()));
    let (first_entered_tx, first_entered_rx) = std::sync::mpsc::channel();
    let (release_first_tx, release_first_rx) = std::sync::mpsc::channel();
    let first_store = store.clone();
    let first_snapshots = Arc::clone(&snapshots);
    let first_persist = thread::spawn(move || {
        first_store
            .persist_durable_state_with(|state| {
                first_entered_tx
                    .send(())
                    .expect("first persistence should signal entry");
                release_first_rx
                    .recv()
                    .expect("first persistence should be released");
                first_snapshots
                    .lock()
                    .expect("snapshot order lock should not be poisoned")
                    .push(
                        state
                            .current_session_id
                            .expect("first snapshot should have current session"),
                    );
                Ok::<(), ()>(())
            })
            .expect("first persistence should complete");
    });
    first_entered_rx
        .recv()
        .expect("first persistence should enter callback");

    store
        .select_current_session(&first_session_id)
        .expect("first session should become current");
    let second_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let second_store = store.clone();
    let second_snapshots = Arc::clone(&snapshots);
    let second_finished_flag = Arc::clone(&second_finished);
    let (second_started_tx, second_started_rx) = std::sync::mpsc::channel();
    let second_persist = thread::spawn(move || {
        second_started_tx
            .send(())
            .expect("second persistence should signal start");
        second_store
            .persist_durable_state_with(|state| {
                second_snapshots
                    .lock()
                    .expect("snapshot order lock should not be poisoned")
                    .push(
                        state
                            .current_session_id
                            .expect("second snapshot should have current session"),
                    );
                Ok::<(), ()>(())
            })
            .expect("second persistence should complete");
        second_finished_flag.store(true, std::sync::atomic::Ordering::Release);
    });
    second_started_rx
        .recv()
        .expect("second persistence should start");
    thread::sleep(Duration::from_millis(20));
    assert!(
        !second_finished.load(std::sync::atomic::Ordering::Acquire),
        "second persistence must wait for the first complete transaction"
    );

    release_first_tx
        .send(())
        .expect("first persistence should be releasable");
    first_persist.join().expect("first persistence should join");
    second_persist
        .join()
        .expect("second persistence should join");

    assert_eq!(
        *snapshots
            .lock()
            .expect("snapshot order lock should not be poisoned"),
        vec![second_session_id, first_session_id]
    );
}

#[test]
fn durable_partition_keeps_current_workspace_session_selection_global() {
    let store = SessionStore::new();
    let workspace_a = WorkspaceId::new("workspace-partition-a");
    let workspace_c = WorkspaceId::new("workspace-partition-c");
    let session_a = SessionId::new("session-partition-a");
    let session_n = SessionId::new("session-partition-n");
    store
        .create_session_for_workspace(session_a.clone(), "A 会话", Some(workspace_a.to_string()))
        .expect("workspace A session should create");
    store
        .create_session_for_workspace(session_n.clone(), "N 会话", Some(workspace_c.to_string()))
        .expect("workspace C session should create");
    store
        .select_current_session(&session_n)
        .expect("workspace C session should become current");

    let (global_state, workspace_states) = store.durable_state().partition_by_workspace();

    assert_eq!(global_state.current_session_id, Some(session_n));
    assert_eq!(
        workspace_states
            .get(workspace_a.as_str())
            .expect("workspace A state should exist")
            .current_session_id,
        None
    );
    assert_eq!(
        workspace_states
            .get(workspace_c.as_str())
            .expect("workspace C state should exist")
            .current_session_id,
        None
    );
}

#[test]
fn goal_store_rejects_second_unfinished_goal_for_same_session() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-singleton");
    store
        .create_session(session_id.clone(), "goal singleton")
        .expect("session should be creatable");

    let first = create_test_goal(
        &store,
        &session_id,
        "turn-goal-singleton-1",
        "完成项目级重构",
        Some(1_000),
    );
    assert_eq!(first.status, GoalStatus::Active);

    let err = store
        .create_goal(
            session_id.clone(),
            first.thread_id.clone(),
            "turn-goal-singleton-2",
            "另一个未结束目标",
            AccessProfile::Restricted,
            None,
        )
        .expect_err("second unfinished goal must be rejected");
    assert!(matches!(err, DomainError::InvalidState { .. }));

    let completed = store
        .complete_goal(
            &session_id,
            &first.goal_id,
            GoalRevisionExpectation::new(first.control_revision, None),
            "turn-goal-singleton-1",
            "第一个目标已完成",
            Vec::new(),
        )
        .expect("goal can be completed");
    assert_eq!(completed.status, GoalStatus::Complete);
    store
        .create_goal(
            session_id,
            completed.thread_id,
            "turn-goal-singleton-3",
            "完成下一阶段",
            AccessProfile::Restricted,
            None,
        )
        .expect("new goal after terminal status should be allowed");
}

#[test]
fn persisted_goal_normalization_keeps_latest_goal_without_resurrecting_older_work() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-latest-restore");
    store
        .create_session(session_id.clone(), "goal latest restore")
        .expect("session should be creatable");
    let older = create_test_goal(
        &store,
        &session_id,
        "turn-goal-latest-restore",
        "旧目标",
        None,
    );
    let mut latest = older.clone();
    latest.goal_id = magi_core::GoalId::new("goal-latest-terminal");
    latest.objective = "最新目标".to_string();
    latest.status = GoalStatus::Complete;
    latest.updated_at = UtcMillis(older.updated_at.0.saturating_add(1));
    let mut durable = store.durable_state();
    durable.goals = vec![older.clone(), latest.clone()];
    durable.plans = vec![SessionPlan {
        plan_id: PlanId::new("plan-goal-latest-restore"),
        session_id: session_id.clone(),
        goal_id: Some(magi_core::GoalId::new("goal-older")),
        revision: 1,
        language: "zh-CN".to_string(),
        state: PlanState::Completed,
        items: Vec::new(),
        task_bindings: HashMap::new(),
        task_statuses: HashMap::new(),
        updated_at: UtcMillis(older.updated_at.0),
    }];

    let restored =
        SessionStore::from_persisted_parts(durable, SessionExecutionSidecarStoreState::default());
    let current = restored
        .current_goal(&session_id)
        .expect("latest goal should restore");
    assert_eq!(current.goal_id, latest.goal_id);
    assert_eq!(current.status, GoalStatus::Complete);
    assert_eq!(current.objective, "最新目标");
    assert_eq!(
        restored
            .plan(&session_id)
            .expect("plan should restore")
            .goal_id,
        Some(latest.goal_id)
    );
}

#[test]
fn plan_is_session_scoped_and_survives_durable_restore() {
    let store = SessionStore::new();
    let session_a = SessionId::new("session-plan-a");
    let session_b = SessionId::new("session-plan-b");
    store
        .create_session(session_a.clone(), "plan a")
        .expect("session a should create");
    store
        .create_session(session_b.clone(), "plan b")
        .expect("session b should create");
    store
        .upsert_plan(
            &session_a,
            SessionPlan {
                plan_id: PlanId::new("plan-persistence"),
                session_id: session_a.clone(),
                goal_id: None,
                revision: 1,
                language: "zh-CN".to_string(),
                state: PlanState::Active,
                items: vec![PlanItem::new(
                    PlanItemId::new("verify-persistence"),
                    "验证持久化",
                    PlanItemStatus::InProgress,
                )],
                task_bindings: HashMap::new(),
                task_statuses: HashMap::new(),
                updated_at: UtcMillis::now(),
            },
            Some(0),
        )
        .expect("plan should write");

    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        SessionExecutionSidecarStoreState::default(),
    );
    let restored_plan = restored.plan(&session_a).expect("plan should restore");
    assert_eq!(restored_plan.items.len(), 1);
    assert_eq!(restored_plan.items[0].title, "验证持久化");
    assert!(restored.plan(&session_b).is_none());
}

#[test]
fn legacy_todo_list_payload_migrates_once_to_stable_session_plan() {
    let source = SessionStore::new();
    let session_id = SessionId::new("session-legacy-plan-migration");
    source
        .create_session(session_id.clone(), "legacy plan migration")
        .expect("session should create");
    let mut payload =
        serde_json::to_value(source.durable_state()).expect("durable state should serialize");
    let object = payload
        .as_object_mut()
        .expect("durable state should be object");
    object.remove("plans");
    object.insert(
        "todo_lists".to_string(),
        serde_json::json!([{
            "sessionId": session_id,
            "items": [{
                "content": "迁移旧任务清单",
                "activeForm": "正在迁移旧任务清单",
                "status": "in_progress"
            }],
            "updatedAt": 42
        }]),
    );
    let durable: SessionDurableState =
        serde_json::from_value(payload).expect("legacy payload should deserialize");
    let restored =
        SessionStore::from_persisted_parts(durable, SessionExecutionSidecarStoreState::default());
    let plan = restored
        .plan(&session_id)
        .expect("legacy plan should migrate");
    assert!(!plan.plan_id.as_str().is_empty());
    assert_eq!(plan.language, "zh-CN");
    assert_eq!(plan.items.len(), 1);
    assert!(!plan.items[0].item_id.as_str().is_empty());
    assert_eq!(plan.items[0].title, "迁移旧任务清单");
    assert_eq!(plan.items[0].status, PlanItemStatus::InProgress);
}

#[test]
fn goal_accounting_budget_limits_active_goal_without_cross_session_leakage() {
    let store = SessionStore::new();
    let session_a = SessionId::new("session-goal-a");
    let session_b = SessionId::new("session-goal-b");
    store
        .create_session(session_a.clone(), "goal a")
        .expect("session a should be creatable");
    store
        .create_session(session_b.clone(), "goal b")
        .expect("session b should be creatable");

    let goal_a = create_test_goal(&store, &session_a, "turn-goal-a", "分析并修复 A", Some(10));
    let goal_b = create_test_goal(&store, &session_b, "turn-goal-b", "分析并修复 B", Some(100));

    let limited = store
        .account_goal_token_usage(&session_a, &goal_a.goal_id, 11)
        .expect("accounting should update goal a");
    assert_eq!(limited.status, GoalStatus::BudgetLimited);
    assert_eq!(limited.tokens_used, 11);
    assert_eq!(limited.time_used_seconds, 0);

    let untouched_b = store
        .current_goal(&session_b)
        .expect("session b goal should still exist");
    assert_eq!(untouched_b.goal_id, goal_b.goal_id);
    assert_eq!(untouched_b.status, GoalStatus::Active);
    assert_eq!(untouched_b.tokens_used, 0);
}

#[test]
fn sessions_for_workspace_returns_user_message_count() {
    let store = SessionStore::new();
    let workspace_id = "workspace-message-count".to_string();
    let session_id = SessionId::new("session-workspace-message-count");
    store
        .create_session_for_workspace(
            session_id.clone(),
            "workspace message count session",
            Some(workspace_id.clone()),
        )
        .expect("session should be creatable");

    store.append_timeline_entry(
        session_id.clone(),
        TimelineEntryKind::UserMessage,
        "第一条用户消息",
    );
    store.append_timeline_entry(
        session_id.clone(),
        TimelineEntryKind::AssistantMessage,
        "助手消息不计入用户消息数",
    );
    store.append_timeline_entry(session_id, TimelineEntryKind::UserMessage, "第二条用户消息");

    let sessions = store.sessions_for_workspace(&workspace_id);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].message_count, Some(2));
}

#[test]
fn session_sidecar_store_keeps_status_and_recovery_export() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-1");
    store
        .create_session(session_id.clone(), "Session 1")
        .expect("session should be creatable");

    store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(WorkspaceId::new("workspace-1")),
            execution_chain_ref: Some("chain-1".to_string()),
            ..ExecutionOwnership::default()
        },
    );

    let sidecar = store
        .attach_recovery_id(&session_id, Some("recovery-1".to_string()))
        .expect("recovery id should be attachable");
    assert_eq!(sidecar.recovery_id.as_deref(), Some("recovery-1"));
    assert_eq!(
        sidecar.status,
        SessionExecutionSidecarStatus::RecoveryLinked
    );

    let state = store.export_state();
    let roundtrip: SessionStoreState =
        serde_json::from_str(&serde_json::to_string(&state).expect("serialize state"))
            .expect("deserialize state");
    assert_eq!(
        roundtrip
            .execution_sidecar_store
            .runtime_sidecars
            .first()
            .and_then(|sidecar| sidecar.recovery_id.as_deref()),
        Some("recovery-1")
    );
    assert_eq!(
        roundtrip
            .execution_sidecar_store
            .runtime_sidecars
            .first()
            .map(|sidecar| &sidecar.status),
        Some(&SessionExecutionSidecarStatus::RecoveryLinked)
    );

    let export = store
        .execution_sidecar_export(&session_id)
        .expect("sidecar export should exist");
    assert_eq!(export.session_id, session_id);
    assert_eq!(
        export.current_status,
        SessionExecutionSidecarStatus::RecoveryLinked
    );
    assert_eq!(export.recovery_ref.as_deref(), Some("recovery-1"));
    assert_eq!(export.execution_chain_ref.as_deref(), Some("chain-1"));
}

#[test]
fn bind_execution_ownership_backfills_workspace_into_active_chain() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-active-chain-workspace");
    let workspace_id = WorkspaceId::new("workspace-active-chain");
    store
        .create_session(session_id.clone(), "Active Chain Workspace")
        .expect("session should be creatable");
    store
        .upsert_active_execution_chain(
            session_id.clone(),
            ActiveExecutionChain {
                session_id: session_id.clone(),
                mission_id: MissionId::new("mission-active-chain"),
                root_task_id: TaskId::new("task-root-active-chain"),
                execution_chain_ref: "chain-active-workspace".to_string(),
                workspace_id: None,
                active_branch_task_ids: vec![TaskId::new("task-active-chain")],
                active_worker_bindings: vec![WorkerId::new("worker-active-chain")],
                branches: vec![ActiveExecutionBranch {
                    task_id: TaskId::new("task-active-chain"),
                    worker_id: WorkerId::new("worker-active-chain"),
                    stage: "finish".to_string(),
                    lease_id: None,
                    execution_intent_ref: None,
                    binding_lifecycle: None,
                    checkpoint_stage: None,
                    next_step_index: None,
                    checkpoint_at: None,
                    resume_mode: None,
                    resume_token: None,
                    use_tools: false,
                    skill_name: None,
                    is_primary: true,
                    thread_id: ThreadId::new("thread-active-chain"),
                }],
                recovery_ref: None,
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: UtcMillis::now(),
                    entry_id: "timeline-active-chain".to_string(),
                    trimmed_text: Some("active chain".to_string()),
                    skill_name: None,
                },
                current_turn: None,
            },
        )
        .expect("active execution chain should upsert");
    assert!(
        store
            .runtime_sidecar(&session_id)
            .expect("sidecar should exist")
            .ownership
            .workspace_id
            .is_none()
    );

    store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            execution_chain_ref: Some("chain-active-workspace".to_string()),
            ..ExecutionOwnership::default()
        },
    );

    let sidecar = store
        .runtime_sidecar(&session_id)
        .expect("sidecar should exist after binding");
    assert_eq!(sidecar.ownership.workspace_id, Some(workspace_id.clone()));
    assert_eq!(
        sidecar
            .active_execution_chain
            .as_ref()
            .and_then(|chain| chain.workspace_id.clone()),
        Some(workspace_id.clone())
    );
    assert_eq!(
        store
            .session(&session_id)
            .and_then(|session| session.workspace_id),
        Some(workspace_id.to_string())
    );
}

#[test]
fn active_execution_chain_turn_replaces_stale_session_turn() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-active-chain-turn-replace");
    store
        .create_session(session_id.clone(), "Active Chain Turn Replace")
        .expect("session should be creatable");

    store
        .upsert_current_turn(
            session_id.clone(),
            ActiveExecutionTurn {
                turn_id: "turn-chat".to_string(),
                turn_seq: 1,
                accepted_at: UtcMillis(1),
                status: "completed".to_string(),
                user_message: Some("普通问答".to_string()),
                items: Vec::new(),
                completed_at: None,
            },
        )
        .expect("chat turn should upsert");

    let task_turn = ActiveExecutionTurn {
        turn_id: "turn-task".to_string(),
        turn_seq: 2,
        accepted_at: UtcMillis(2),
        status: "accepted".to_string(),
        user_message: Some("创建产品级任务".to_string()),
        items: Vec::new(),
        completed_at: None,
    };

    store
        .upsert_active_execution_chain(
            session_id.clone(),
            ActiveExecutionChain {
                session_id: session_id.clone(),
                mission_id: MissionId::new("mission-active-chain-turn-replace"),
                root_task_id: TaskId::new("task-root-active-chain-turn-replace"),
                execution_chain_ref: "chain-active-chain-turn-replace".to_string(),
                workspace_id: None,
                active_branch_task_ids: Vec::new(),
                active_worker_bindings: Vec::new(),
                branches: Vec::new(),
                recovery_ref: None,
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: UtcMillis(2),
                    entry_id: "timeline-active-chain-turn-replace".to_string(),
                    trimmed_text: Some("创建产品级任务".to_string()),
                    skill_name: None,
                },
                current_turn: Some(task_turn.clone()),
            },
        )
        .expect("task chain should upsert");

    let sidecar = store
        .runtime_sidecar(&session_id)
        .expect("sidecar should exist");
    assert_eq!(
        sidecar
            .current_turn
            .as_ref()
            .map(|turn| turn.turn_id.as_str()),
        Some("turn-task")
    );
    assert_eq!(
        sidecar
            .active_execution_chain
            .as_ref()
            .and_then(|chain| chain.current_turn.as_ref())
            .map(|turn| turn.turn_id.as_str()),
        Some("turn-task")
    );
}

#[test]
fn active_execution_chain_does_not_reuse_turn_from_different_chain() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-active-chain-turn-isolated");
    store
        .create_session(session_id.clone(), "Active Chain Turn Isolated")
        .expect("session should be creatable");

    store
        .upsert_current_turn(
            session_id.clone(),
            ActiveExecutionTurn {
                turn_id: "turn-chat".to_string(),
                turn_seq: 1,
                accepted_at: UtcMillis(1),
                status: "completed".to_string(),
                user_message: Some("普通问答".to_string()),
                items: Vec::new(),
                completed_at: None,
            },
        )
        .expect("chat turn should upsert");

    store
        .upsert_active_execution_chain(
            session_id.clone(),
            ActiveExecutionChain {
                session_id: session_id.clone(),
                mission_id: MissionId::new("mission-active-chain-turn-isolated"),
                root_task_id: TaskId::new("task-root-active-chain-turn-isolated"),
                execution_chain_ref: "chain-active-chain-turn-isolated".to_string(),
                workspace_id: None,
                active_branch_task_ids: Vec::new(),
                active_worker_bindings: Vec::new(),
                branches: Vec::new(),
                recovery_ref: None,
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: UtcMillis(2),
                    entry_id: "timeline-active-chain-turn-isolated".to_string(),
                    trimmed_text: Some("创建产品级任务".to_string()),
                    skill_name: None,
                },
                current_turn: None,
            },
        )
        .expect("task chain should upsert");

    let sidecar = store
        .runtime_sidecar(&session_id)
        .expect("sidecar should exist");
    assert!(
        sidecar.current_turn.is_none(),
        "不同 execution chain 不能复用旧 turn，否则任务会挂到上一轮普通对话"
    );
    assert!(
        sidecar
            .active_execution_chain
            .as_ref()
            .and_then(|chain| chain.current_turn.as_ref())
            .is_none(),
        "active chain 内部也不能保留跨链 turn"
    );
}

#[test]
fn accept_current_turn_with_timeline_entry_rejects_running_turn_without_timeline_write() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-atomic-chat-reject");
    store
        .create_session(session_id.clone(), "Atomic Chat Reject")
        .expect("session should be creatable");
    store
        .upsert_current_turn(session_id.clone(), test_turn("turn-running", "running", 1))
        .expect("running turn should upsert");

    let result = store.accept_current_turn_with_timeline_entry(
        session_id.clone(),
        TimelineEntryInput::new(
            "timeline-rejected-chat",
            TimelineEntryKind::UserMessage,
            "不应写入的用户消息",
            UtcMillis(2),
        ),
        test_turn("turn-next", "running", 2),
    );

    assert!(matches!(
        result,
        Err(magi_core::DomainError::CurrentTurnConflict {
            session_id: ref conflicted_session_id,
            active_turn_id: ref conflicted_turn_id,
        }) if conflicted_session_id == session_id.as_str()
            && conflicted_turn_id == "turn-running"
    ));
    assert!(
        !store
            .timeline_for_session(&session_id)
            .iter()
            .any(|entry| entry.entry_id == "timeline-rejected-chat"),
        "拒绝新 turn 时不能留下用户 timeline"
    );
    assert_eq!(
        store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .map(|turn| turn.turn_id),
        Some("turn-running".to_string())
    );
}

#[test]
fn replacing_latest_user_interrupted_turn_is_atomic_and_rejects_stale_retry() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-replace-user-interrupted-turn");
    store
        .create_session(session_id.clone(), "Replace user interrupted turn")
        .expect("session should be creatable");

    let mut original_turn = test_turn("turn-original", "running", 10);
    original_turn.items = vec![test_turn_item("user-original", "原始消息")];
    store
        .accept_current_turn_with_timeline_entry(
            session_id.clone(),
            TimelineEntryInput::new(
                "timeline-original",
                TimelineEntryKind::UserMessage,
                "原始消息",
                UtcMillis(10),
            ),
            original_turn,
        )
        .expect("original turn should be accepted");
    store
        .interrupt_current_turn_by_user(&session_id)
        .expect("user interruption should succeed");

    let mut replacement_turn = test_turn("turn-replacement", "running", 20);
    let mut replacement_user_item = test_turn_item("user-replacement", "修改后的消息");
    replacement_user_item
        .metadata
        .insert("replacesTurnId".to_string(), json!("turn-original"));
    replacement_turn.items = vec![replacement_user_item];
    let (_, sidecar, superseded_turn) = store
        .replace_current_turn_with_timeline_entry(
            session_id.clone(),
            "turn-original",
            TimelineEntryInput::new(
                "timeline-replacement",
                TimelineEntryKind::UserMessage,
                "修改后的消息",
                UtcMillis(20),
            ),
            replacement_turn,
        )
        .expect("latest user-interrupted turn should be replaceable");

    assert_eq!(superseded_turn.status, CanonicalTurnStatus::Superseded);
    assert_eq!(
        superseded_turn
            .metadata
            .get("supersededReason")
            .and_then(serde_json::Value::as_str),
        Some("user_edit")
    );
    assert_eq!(
        sidecar.current_turn.map(|turn| turn.turn_id),
        Some("turn-replacement".to_string())
    );
    let turns = store.canonical_turns_for_session(&session_id);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].status, CanonicalTurnStatus::Superseded);
    assert_eq!(turns[1].status, CanonicalTurnStatus::Running);
    assert_eq!(
        turns[1]
            .metadata
            .get("replacesTurnId")
            .and_then(serde_json::Value::as_str),
        Some("turn-original")
    );

    let stale_result = store.replace_current_turn_with_timeline_entry(
        session_id.clone(),
        "turn-original",
        TimelineEntryInput::new(
            "timeline-stale-replacement",
            TimelineEntryKind::UserMessage,
            "过期编辑",
            UtcMillis(30),
        ),
        test_turn("turn-stale-replacement", "running", 30),
    );
    assert!(matches!(
        stale_result,
        Err(magi_core::DomainError::InvalidState { .. })
    ));
    assert!(
        !store
            .timeline_for_session(&session_id)
            .iter()
            .any(|entry| entry.entry_id == "timeline-stale-replacement"),
        "竞争失败不能留下新的 timeline 记录"
    );
}

#[test]
fn user_interruption_atomically_pauses_running_goal_and_plan() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-user-interruption");
    store
        .create_session(session_id.clone(), "Goal user interruption")
        .expect("session should be creatable");
    let goal = create_test_goal(
        &store,
        &session_id,
        "turn-create-goal-user-interruption",
        "验证用户中断释放 Goal continuation",
        None,
    );
    let chain = test_active_chain(
        &session_id,
        "goal-user-interruption",
        Some(test_turn("turn-goal-user-interruption", "running", 10)),
    );
    let continuation_turn_id = chain.root_task_id.to_string();
    store
        .upsert_plan_for_goal_progress(
            &session_id,
            SessionPlan {
                plan_id: PlanId::new("plan-goal-user-interruption"),
                session_id: session_id.clone(),
                goal_id: Some(goal.goal_id.clone()),
                revision: 1,
                language: "zh-CN".to_string(),
                state: PlanState::Active,
                items: vec![PlanItem::new(
                    PlanItemId::new("interruptible-step"),
                    "可中断步骤",
                    PlanItemStatus::InProgress,
                )],
                task_bindings: HashMap::new(),
                task_statuses: HashMap::new(),
                updated_at: UtcMillis(9),
            },
            Some(0),
            Some(goal.goal_id.clone()),
            Some(goal.control_revision),
        )
        .expect("goal plan should create");
    store
        .accept_goal_continuation_with_timeline_entry(
            session_id.clone(),
            &goal.goal_id,
            TimelineEntryInput::new(
                "timeline-goal-user-interruption",
                TimelineEntryKind::UserMessage,
                "继续 Goal",
                UtcMillis(10),
            ),
            chain,
        )
        .expect("goal continuation should start");
    assert_eq!(
        store
            .current_goal(&session_id)
            .expect("goal should exist")
            .continuation
            .turn_id
            .as_deref(),
        Some(continuation_turn_id.as_str())
    );

    store
        .interrupt_current_turn_by_user(&session_id)
        .expect("user interruption should succeed");

    let paused_goal = store.current_goal(&session_id).expect("goal should remain");
    assert_eq!(paused_goal.status, GoalStatus::Paused);
    assert_eq!(paused_goal.continuation, GoalContinuationState::default());
    assert_eq!(
        store
            .plan(&session_id)
            .expect("bound plan should remain")
            .state,
        PlanState::Paused
    );
}

#[test]
fn daemon_restart_releases_running_goal_continuation() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-daemon-interruption");
    store
        .create_session(session_id.clone(), "Goal daemon interruption")
        .expect("session should be creatable");
    let goal = create_test_goal(
        &store,
        &session_id,
        "turn-create-goal-daemon-interruption",
        "验证 daemon 重启释放 Goal continuation",
        None,
    );
    let chain = test_active_chain(
        &session_id,
        "goal-daemon-interruption",
        Some(test_turn("turn-goal-daemon-interruption", "running", 10)),
    );
    store
        .accept_goal_continuation_with_timeline_entry(
            session_id.clone(),
            &goal.goal_id,
            TimelineEntryInput::new(
                "timeline-goal-daemon-interruption",
                TimelineEntryKind::UserMessage,
                "继续 Goal",
                UtcMillis(10),
            ),
            chain,
        )
        .expect("goal continuation should start");

    store
        .interrupt_current_turn_by_daemon_restart(&session_id)
        .expect("daemon restart interruption should succeed");

    let continuation = store
        .current_goal(&session_id)
        .expect("goal should remain")
        .continuation;
    assert_eq!(continuation.phase, GoalContinuationPhase::Waiting);
    assert_eq!(continuation.turn_id, None);
    assert_eq!(
        continuation.reason.as_deref(),
        Some("daemon_restart_interrupted")
    );
}

#[test]
fn interrupted_execution_resume_restores_owned_goal_plan_and_live_timing() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-interrupted-goal-resume");
    store
        .create_session(session_id.clone(), "Interrupted Goal Resume")
        .expect("session should be creatable");
    let chain = test_active_chain(
        &session_id,
        "interrupted-goal-resume",
        Some(test_turn("turn-interrupted-goal-resume", "running", 10)),
    );
    let goal = create_test_goal(
        &store,
        &session_id,
        chain.root_task_id.as_str(),
        "验证异常中断恢复同步 Goal",
        None,
    );
    let plan = store
        .upsert_plan(
            &session_id,
            SessionPlan {
                plan_id: PlanId::new("plan-interrupted-goal-resume"),
                session_id: session_id.clone(),
                goal_id: Some(goal.goal_id.clone()),
                revision: 0,
                language: "zh-CN".to_string(),
                state: PlanState::Active,
                items: vec![PlanItem {
                    item_id: PlanItemId::new("resume"),
                    title: "恢复异常中断任务".to_string(),
                    status: PlanItemStatus::InProgress,
                }],
                task_bindings: HashMap::new(),
                task_statuses: HashMap::new(),
                updated_at: UtcMillis(10),
            },
            Some(0),
        )
        .expect("goal plan should persist");
    store
        .accept_goal_continuation_with_timeline_entry(
            session_id.clone(),
            &goal.goal_id,
            TimelineEntryInput::new(
                "timeline-interrupted-goal-resume",
                TimelineEntryKind::UserMessage,
                "推进 Goal",
                UtcMillis(10),
            ),
            chain,
        )
        .expect("goal continuation should start");
    assert_eq!(
        store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| turn.turn_id == "turn-interrupted-goal-resume")
            .and_then(|turn| turn.metadata.get("goalId").cloned())
            .and_then(|value| value.as_str().map(str::to_string))
            .as_deref(),
        Some(goal.goal_id.as_str())
    );
    store
        .interrupt_current_turn_by_daemon_restart(&session_id)
        .expect("daemon interruption should persist");
    let interrupted_goal = store
        .current_goal(&session_id)
        .expect("goal should remain after interruption");
    let (paused_goal, paused_plan) = store
        .pause_goal_with_plan(
            &session_id,
            &goal.goal_id,
            interrupted_goal.control_revision,
            Some(plan.revision),
        )
        .expect("goal and plan should pause");
    let interrupted_turn_id = store
        .claim_interrupted_recovery(&session_id)
        .expect("recovery should be claimable")
        .expect("daemon interruption should return turn id");

    let resumed_turn_id = "turn-session-continue-20";
    let checkpoint = store
        .resume_goal_for_interrupted_execution(
            &session_id,
            &interrupted_turn_id,
            resumed_turn_id,
            UtcMillis(20),
        )
        .expect("owned goal should resume")
        .expect("owned goal should produce rollback checkpoint");
    let resumed_goal = store.current_goal(&session_id).expect("goal should remain");
    assert_eq!(resumed_goal.status, GoalStatus::Active);
    assert_eq!(
        resumed_goal.control_revision,
        paused_goal.control_revision + 1
    );
    assert_eq!(
        resumed_goal.continuation.phase,
        GoalContinuationPhase::Running
    );
    assert_eq!(
        resumed_goal.continuation.turn_id.as_deref(),
        Some(resumed_turn_id)
    );
    let resumed_plan = store.plan(&session_id).expect("plan should remain");
    assert_eq!(resumed_plan.state, PlanState::Active);
    assert_eq!(
        resumed_plan.revision,
        paused_plan.expect("paused plan").revision + 1
    );

    store
        .rollback_interrupted_goal_resume(checkpoint)
        .expect("pre-runner failure should restore exact Goal snapshot");
    let rolled_back_goal = store.current_goal(&session_id).expect("goal should remain");
    assert_eq!(rolled_back_goal, paused_goal);
    assert_eq!(
        store.plan(&session_id).expect("plan should remain").state,
        PlanState::Paused
    );

    let checkpoint = store
        .resume_goal_for_interrupted_execution(
            &session_id,
            &interrupted_turn_id,
            resumed_turn_id,
            UtcMillis(20),
        )
        .expect("second owned resume should succeed")
        .expect("second owned resume should produce checkpoint");
    drop(checkpoint);
    store
        .upsert_current_turn(
            session_id.clone(),
            test_turn(resumed_turn_id, "running", 20),
        )
        .expect("resumed turn should become canonical");
    let running_goal = store.current_goal(&session_id).expect("goal should remain");
    assert_eq!(running_goal.timing_started_at, Some(UtcMillis(20)));
    assert_eq!(
        running_goal.timing_turn_id.as_deref(),
        Some(resumed_turn_id)
    );
}

#[test]
fn interrupted_execution_does_not_resume_unrelated_paused_goal() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-unrelated-interrupted-goal");
    store
        .create_session(session_id.clone(), "Unrelated Interrupted Goal")
        .expect("session should be creatable");
    let goal = create_test_goal(
        &store,
        &session_id,
        "task-unrelated-goal-owner",
        "保持无关 Goal 暂停",
        None,
    );
    let paused_goal = store
        .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, None)
        .expect("goal should pause")
        .0;
    store
        .accept_active_execution_chain_with_timeline_entry(
            session_id.clone(),
            TimelineEntryInput::new(
                "timeline-unrelated-interruption",
                TimelineEntryKind::UserMessage,
                "无关执行链",
                UtcMillis(10),
            ),
            test_active_chain(
                &session_id,
                "unrelated-interruption",
                Some(test_turn("turn-unrelated-interruption", "running", 10)),
            ),
        )
        .expect("unrelated chain should persist");
    store
        .interrupt_current_turn_by_daemon_restart(&session_id)
        .expect("daemon interruption should persist");
    let interrupted_turn_id = store
        .claim_interrupted_recovery(&session_id)
        .expect("recovery should be claimable")
        .expect("interruption should return turn id");

    assert!(
        store
            .resume_goal_for_interrupted_execution(
                &session_id,
                &interrupted_turn_id,
                "turn-session-continue-unrelated",
                UtcMillis(20),
            )
            .expect("unrelated recovery should be valid")
            .is_none()
    );
    assert_eq!(store.current_goal(&session_id), Some(paused_goal));
}

#[test]
fn restore_reconciles_terminal_goal_continuation() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-terminal-restore");
    store
        .create_session(session_id.clone(), "Goal terminal restore")
        .expect("session should be creatable");
    let goal = create_test_goal(
        &store,
        &session_id,
        "turn-create-goal-terminal-restore",
        "验证旧持久化 continuation 对账",
        None,
    );
    let mut turn = test_turn("turn-goal-terminal-restore", "running", 10);
    let mut task_item = test_turn_item("item-goal-terminal-restore", "继续 Goal");
    task_item.task_id = Some(TaskId::new("task-root-goal-terminal-restore"));
    turn.items.push(task_item);
    let chain = test_active_chain(&session_id, "goal-terminal-restore", Some(turn));
    let continuation_turn_id = chain.root_task_id.to_string();
    store
        .accept_goal_continuation_with_timeline_entry(
            session_id.clone(),
            &goal.goal_id,
            TimelineEntryInput::new(
                "timeline-goal-terminal-restore",
                TimelineEntryKind::UserMessage,
                "继续 Goal",
                UtcMillis(10),
            ),
            chain,
        )
        .expect("goal continuation should start");
    store
        .interrupt_current_turn_by_user(&session_id)
        .expect("turn should become terminal");

    let mut durable = store.durable_state();
    let persisted_goal = durable
        .goals
        .iter_mut()
        .find(|persisted| persisted.goal_id == goal.goal_id)
        .expect("persisted goal should exist");
    persisted_goal.status = GoalStatus::Active;
    persisted_goal.continuation = GoalContinuationState {
        phase: GoalContinuationPhase::Running,
        turn_id: Some(continuation_turn_id),
        reason: None,
    };
    let mut sidecars = store.execution_sidecar_store_state();
    let sidecar = sidecars
        .runtime_sidecars
        .iter_mut()
        .find(|sidecar| sidecar.session_id == session_id)
        .expect("persisted sidecar should exist");
    sidecar.active_execution_chain = None;
    sidecar.status = SessionExecutionSidecarStatus::Detached;
    let restored = SessionStore::from_persisted_parts(durable, sidecars);

    let continuation = restored
        .current_goal(&session_id)
        .expect("goal should restore")
        .continuation;
    assert_eq!(continuation.phase, GoalContinuationPhase::Waiting);
    assert_eq!(continuation.turn_id, None);
    assert_eq!(continuation.reason.as_deref(), Some("user_interrupted"));
}

#[test]
fn accept_active_execution_chain_with_timeline_entry_writes_timeline_and_turn_atomically() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-atomic-task-accept");
    store
        .create_session(session_id.clone(), "Atomic Task Accept")
        .expect("session should be creatable");
    let chain = test_active_chain(
        &session_id,
        "chain-atomic-task-accept",
        Some(test_turn("turn-task", "accepted", 3)),
    );

    let (entry_id, sidecar) = store
        .accept_active_execution_chain_with_timeline_entry(
            session_id.clone(),
            TimelineEntryInput::new(
                "timeline-atomic-task-accept",
                TimelineEntryKind::UserMessage,
                "任务用户消息",
                UtcMillis(3),
            ),
            chain,
        )
        .expect("task chain should be accepted");

    assert_eq!(entry_id, "timeline-atomic-task-accept");
    assert!(
        store
            .timeline_for_session(&session_id)
            .iter()
            .any(|entry| entry.entry_id == "timeline-atomic-task-accept"
                && entry.message == "任务用户消息")
    );
    assert_eq!(
        sidecar.current_turn.map(|turn| turn.turn_id),
        Some("turn-task".to_string())
    );
    assert!(
        sidecar
            .active_execution_chain
            .and_then(|chain| chain.current_turn)
            .is_some(),
        "active chain 内部必须同步携带当前 turn"
    );
}

#[test]
fn accept_active_execution_chain_rejects_invalid_canonical_turn_without_partial_writes() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-atomic-invalid-canonical-turn");
    store
        .create_session(session_id.clone(), "Atomic Invalid Canonical Turn")
        .expect("session should be creatable");
    let chain = test_active_chain(
        &session_id,
        "chain-atomic-invalid-canonical-turn",
        Some(test_turn("turn-invalid", "invalid", 4)),
    );

    let result = store.accept_active_execution_chain_with_timeline_entry(
        session_id.clone(),
        TimelineEntryInput::new(
            "timeline-invalid-canonical-turn",
            TimelineEntryKind::UserMessage,
            "不应写入的无效任务消息",
            UtcMillis(4),
        ),
        chain,
    );

    assert!(matches!(
        result,
        Err(magi_core::DomainError::InvalidState { .. })
    ));
    assert!(
        store
            .timeline_for_session(&session_id)
            .iter()
            .all(|entry| entry.entry_id != "timeline-invalid-canonical-turn")
    );
    assert!(store.canonical_turns_for_session(&session_id).is_empty());
    assert!(store.runtime_sidecar(&session_id).is_none());
}

#[test]
fn accept_active_execution_chain_rejects_running_turn_without_timeline_write() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-atomic-task-reject");
    store
        .create_session(session_id.clone(), "Atomic Task Reject")
        .expect("session should be creatable");
    store
        .upsert_current_turn(session_id.clone(), test_turn("turn-running", "running", 1))
        .expect("running turn should upsert");
    let chain = test_active_chain(
        &session_id,
        "chain-atomic-task-reject",
        Some(test_turn("turn-task", "accepted", 4)),
    );

    let result = store.accept_active_execution_chain_with_timeline_entry(
        session_id.clone(),
        TimelineEntryInput::new(
            "timeline-rejected-task",
            TimelineEntryKind::UserMessage,
            "不应写入的任务消息",
            UtcMillis(4),
        ),
        chain,
    );

    assert!(matches!(
        result,
        Err(magi_core::DomainError::CurrentTurnConflict {
            session_id: ref conflicted_session_id,
            active_turn_id: ref conflicted_turn_id,
        }) if conflicted_session_id == session_id.as_str()
            && conflicted_turn_id == "turn-running"
    ));
    assert!(
        !store
            .timeline_for_session(&session_id)
            .iter()
            .any(|entry| entry.entry_id == "timeline-rejected-task"),
        "任务入口冲突时也不能留下用户 timeline"
    );
    assert_eq!(
        store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .map(|turn| turn.turn_id),
        Some("turn-running".to_string())
    );
}

#[test]
fn upsert_active_execution_chain_rejects_different_running_turn() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-upsert-chain-running-reject");
    store
        .create_session(session_id.clone(), "Upsert Chain Running Reject")
        .expect("session should be creatable");
    store
        .upsert_current_turn(session_id.clone(), test_turn("turn-running", "running", 1))
        .expect("running turn should upsert");
    let chain = test_active_chain(
        &session_id,
        "chain-reject-running",
        Some(test_turn("turn-different", "accepted", 5)),
    );

    let result = store.upsert_active_execution_chain(session_id.clone(), chain);

    assert!(matches!(
        result,
        Err(magi_core::DomainError::CurrentTurnConflict {
            session_id: ref conflicted_session_id,
            active_turn_id: ref conflicted_turn_id,
        }) if conflicted_session_id == session_id.as_str()
            && conflicted_turn_id == "turn-running"
    ));
    assert_eq!(
        store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .map(|turn| turn.turn_id),
        Some("turn-running".to_string())
    );
}

#[test]
fn append_current_turn_item_with_timeline_entry_writes_item_and_timeline_atomically() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-append-item-timeline");
    store
        .create_session(session_id.clone(), "Append Item Timeline")
        .expect("session should be creatable");
    store
        .upsert_current_turn(session_id.clone(), test_turn("turn-running", "running", 1))
        .expect("running turn should upsert");

    let updated = store
        .append_current_turn_item_with_timeline_entry(
            &session_id,
            TimelineEntryInput::new(
                "timeline-append-item",
                TimelineEntryKind::UserMessage,
                "继续用户消息",
                UtcMillis(2),
            ),
            test_turn_item("turn-item-continue-user", "继续用户消息"),
        )
        .expect("append should succeed")
        .expect("current turn should exist");

    assert!(
        store
            .timeline_for_session(&session_id)
            .iter()
            .any(
                |entry| entry.entry_id == "timeline-append-item" && entry.message == "继续用户消息"
            )
    );
    assert!(
        updated
            .current_turn
            .expect("turn should remain")
            .items
            .iter()
            .any(|item| item.item_id == "turn-item-continue-user")
    );
}

#[test]
fn upsert_current_turn_item_allows_assistant_stream_to_final_canonical_update() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-canonical-assistant-update");
    store
        .create_session(session_id.clone(), "Canonical Assistant Update")
        .expect("session should be creatable");
    store
        .upsert_current_turn(session_id.clone(), test_turn("turn-running", "running", 1))
        .expect("running turn should upsert");

    let mut stream_item = test_turn_item("turn-item-assistant", "流式回复");
    stream_item.kind = "assistant_stream".to_string();
    stream_item.status = "running".to_string();
    store
        .upsert_current_turn_item(&session_id, stream_item)
        .expect("stream item should upsert");

    let mut final_item = test_turn_item("turn-item-assistant", "最终回复");
    final_item.kind = "assistant_final".to_string();
    final_item.status = "completed".to_string();
    let updated = store
        .upsert_current_turn_item(&session_id, final_item)
        .expect("assistant_text canonical update should be accepted")
        .expect("current turn should exist");

    let item = updated
        .current_turn
        .expect("turn should remain")
        .items
        .into_iter()
        .find(|item| item.item_id == "turn-item-assistant")
        .expect("assistant item should remain");
    assert_eq!(item.item_seq, 1);
    assert_eq!(item.kind, "assistant_final");
    assert_eq!(item.status, "completed");
    assert_eq!(item.content.as_deref(), Some("最终回复"));

    let canonical = store.canonical_turns_for_session(&session_id);
    let canonical_item = canonical
        .iter()
        .flat_map(|turn| &turn.items)
        .find(|item| item.item_id == "turn-item-assistant")
        .expect("canonical assistant item should remain");
    assert_eq!(
        canonical_item
            .metadata
            .get("assistantOutputKind")
            .and_then(serde_json::Value::as_str),
        Some("final")
    );

    store
        .update_current_turn_status(&session_id, "completed")
        .expect("turn should become terminal before restore");
    let mut durable = store.durable_state();
    durable
        .canonical_turns
        .iter_mut()
        .flat_map(|turn| &mut turn.items)
        .find(|item| item.item_id == "turn-item-assistant")
        .expect("durable canonical assistant should exist")
        .metadata
        .remove("assistantOutputKind");
    let restored =
        SessionStore::from_persisted_parts(durable, store.execution_sidecar_store_state());
    assert_eq!(
        restored
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .flat_map(|turn| turn.items)
            .find(|item| item.item_id == "turn-item-assistant")
            .and_then(|item| item.metadata.get("assistantOutputKind").cloned())
            .and_then(|value| value.as_str().map(str::to_string))
            .as_deref(),
        Some("final"),
        "restore must recover the raw final/progress identity from the durable sidecar"
    );
}

#[test]
fn active_goal_terminal_turn_is_not_a_user_response_duration_boundary() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-progress-duration");
    let turn_id = "turn-goal-progress-duration";
    store
        .create_session(session_id.clone(), "Goal Progress Duration")
        .expect("session should be creatable");
    create_test_goal(
        &store,
        &session_id,
        turn_id,
        "验证 Goal 中间 Turn 不展示总耗时",
        None,
    );
    store
        .upsert_current_turn(session_id.clone(), test_turn(turn_id, "running", 10))
        .expect("goal turn should start");
    store
        .update_current_turn_status(&session_id, "failed")
        .expect("goal progress turn should become terminal");

    let canonical = store
        .canonical_turns_for_session(&session_id)
        .into_iter()
        .find(|turn| turn.turn_id == turn_id)
        .expect("goal progress turn should be canonical");
    assert_eq!(
        canonical
            .metadata
            .get("responseDurationScope")
            .and_then(serde_json::Value::as_str),
        Some("goal_progress")
    );

    let mut durable = store.durable_state();
    durable
        .canonical_turns
        .iter_mut()
        .find(|turn| turn.turn_id == turn_id)
        .expect("durable goal progress turn should exist")
        .metadata
        .clear();
    let restored =
        SessionStore::from_persisted_parts(durable, store.execution_sidecar_store_state());
    assert_eq!(
        restored
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| turn.turn_id == turn_id)
            .and_then(|turn| turn.metadata.get("responseDurationScope").cloned())
            .and_then(|value| value.as_str().map(str::to_string))
            .as_deref(),
        Some("goal_progress"),
        "restore must repair historical goal turns that predate the scope metadata"
    );
}

#[test]
fn goal_time_uses_owned_canonical_turn_wall_clock_without_model_usage() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-canonical-time");
    let turn_id = "turn-goal-canonical-time";
    store
        .create_session(session_id.clone(), "Goal Canonical Time")
        .expect("session should be creatable");
    let goal = create_test_goal(
        &store,
        &session_id,
        turn_id,
        "验证 Goal 使用完整 Turn 墙钟时间",
        None,
    );
    let mut turn = test_turn(turn_id, "completed", 1_000);
    turn.completed_at = Some(UtcMillis(5_250));
    store
        .upsert_current_turn(session_id.clone(), turn)
        .expect("terminal goal turn should upsert");

    let current = store
        .current_goal(&session_id)
        .expect("goal should remain readable");
    assert_eq!(current.time_used_seconds, 4);
    assert_eq!(current.time_used_millis, 4_250);
    assert_eq!(current.timing_started_at, None);
    assert_eq!(current.timing_turn_id, None);
    let canonical = store
        .canonical_turns_for_session(&session_id)
        .into_iter()
        .find(|turn| turn.turn_id == turn_id)
        .expect("goal turn should be canonical");
    assert_eq!(
        canonical
            .metadata
            .get("goalId")
            .and_then(serde_json::Value::as_str),
        Some(goal.goal_id.as_str())
    );

    let mut durable = store.durable_state();
    durable
        .goals
        .iter_mut()
        .find(|candidate| candidate.goal_id == goal.goal_id)
        .expect("durable goal should exist")
        .time_used_seconds = 99;
    durable
        .canonical_turns
        .iter_mut()
        .find(|turn| turn.turn_id == turn_id)
        .expect("durable canonical turn should exist")
        .metadata
        .clear();
    let restored =
        SessionStore::from_persisted_parts(durable, store.execution_sidecar_store_state());
    assert_eq!(
        restored
            .current_goal(&session_id)
            .expect("restored goal should exist")
            .time_used_seconds,
        4,
        "restore must replace legacy model-only timing with canonical wall-clock timing"
    );
}

#[test]
fn running_goal_turn_exposes_live_timing_until_terminal_settlement() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-live-time");
    let turn_id = "turn-goal-live-time";
    store
        .create_session(session_id.clone(), "Goal Live Time")
        .expect("session should be creatable");
    create_test_goal(&store, &session_id, turn_id, "验证 Goal 运行中计时", None);
    store
        .upsert_current_turn(session_id.clone(), test_turn(turn_id, "running", 1_000))
        .expect("running goal turn should upsert");

    let running = store
        .current_goal(&session_id)
        .expect("running goal should remain readable");
    assert_eq!(running.time_used_millis, 0);
    assert_eq!(running.timing_started_at, Some(UtcMillis(1_000)));
    assert_eq!(running.timing_turn_id.as_deref(), Some(turn_id));

    let mut terminal_turn = test_turn(turn_id, "completed", 1_000);
    terminal_turn.completed_at = Some(UtcMillis(5_250));
    store
        .upsert_current_turn(session_id.clone(), terminal_turn.clone())
        .expect("terminal goal turn should settle timing");
    store
        .upsert_current_turn(session_id.clone(), terminal_turn)
        .expect("repeated terminal upsert should remain idempotent");

    let settled = store
        .current_goal(&session_id)
        .expect("settled goal should remain readable");
    assert_eq!(settled.time_used_millis, 4_250);
    assert_eq!(settled.time_used_seconds, 4);
    assert_eq!(settled.timing_started_at, None);
    assert_eq!(settled.timing_turn_id, None);
}

#[test]
fn paused_goal_hides_live_timing_and_completed_goal_keeps_final_turn_running() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-timing-status");
    let turn_id = "turn-goal-timing-status";
    store
        .create_session(session_id.clone(), "Goal Timing Status")
        .expect("session should be creatable");
    let goal = create_test_goal(&store, &session_id, turn_id, "验证 Goal 状态计时边界", None);
    store
        .upsert_current_turn(session_id.clone(), test_turn(turn_id, "running", 1_000))
        .expect("running goal turn should upsert");

    let (paused, _) = store
        .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, None)
        .expect("goal should pause");
    assert_eq!(paused.timing_started_at, None);
    assert_eq!(paused.timing_turn_id, None);

    let (resumed, _, _) = store
        .resume_goal_with_plan(
            &session_id,
            &goal.goal_id,
            paused.control_revision,
            None,
            None,
            None,
        )
        .expect("goal should resume");
    assert_eq!(resumed.continuation.phase, GoalContinuationPhase::Waiting);
    assert_eq!(resumed.timing_started_at, None);

    let completed = store
        .complete_goal(
            &session_id,
            &goal.goal_id,
            GoalRevisionExpectation::new(resumed.control_revision, None),
            turn_id,
            "目标完成，正在输出最终回复",
            Vec::new(),
        )
        .expect("goal should complete inside its final turn");
    assert_eq!(completed.status, GoalStatus::Complete);
    assert_eq!(completed.timing_started_at, Some(UtcMillis(1_000)));
    assert_eq!(completed.timing_turn_id.as_deref(), Some(turn_id));
}

#[test]
fn blocked_goal_turn_with_completion_timestamp_is_settled() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-blocked-time");
    let turn_id = "turn-goal-blocked-time";
    store
        .create_session(session_id.clone(), "Goal Blocked Time")
        .expect("session should be creatable");
    create_test_goal(&store, &session_id, turn_id, "验证阻塞 Turn 计时结算", None);
    let mut turn = test_turn(turn_id, "blocked", 1_000);
    turn.completed_at = Some(UtcMillis(5_250));
    store
        .upsert_current_turn(session_id.clone(), turn)
        .expect("blocked goal turn should upsert");

    let current = store
        .current_goal(&session_id)
        .expect("goal should remain readable");
    assert_eq!(current.time_used_millis, 4_250);
    assert_eq!(current.timing_started_at, None);
}

#[test]
fn completed_goal_terminal_turn_is_the_user_response_duration_boundary() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-complete-duration");
    let turn_id = "turn-goal-complete-duration";
    store
        .create_session(session_id.clone(), "Goal Complete Duration")
        .expect("session should be creatable");
    let goal = create_test_goal(
        &store,
        &session_id,
        turn_id,
        "验证 Goal 最终 Turn 展示总耗时",
        None,
    );
    store
        .upsert_current_turn(session_id.clone(), test_turn(turn_id, "running", 20))
        .expect("goal turn should start");
    store
        .complete_goal(
            &session_id,
            &goal.goal_id,
            GoalRevisionExpectation::new(goal.control_revision, None),
            turn_id,
            "目标已完成",
            Vec::new(),
        )
        .expect("goal should complete");
    store
        .update_current_turn_status(&session_id, "completed")
        .expect("completed goal turn should become terminal");

    let canonical = store
        .canonical_turns_for_session(&session_id)
        .into_iter()
        .find(|turn| turn.turn_id == turn_id)
        .expect("completed goal turn should be canonical");
    assert_eq!(canonical.metadata.get("responseDurationScope"), None);
}

#[test]
fn current_turn_writes_update_durable_canonical_turn_log() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-durable-canonical-log");
    store
        .create_session(session_id.clone(), "Durable Canonical Log")
        .expect("session should be creatable");
    store
        .upsert_current_turn(session_id.clone(), test_turn("turn-durable", "running", 10))
        .expect("running turn should upsert");

    let mut assistant_item = test_turn_item("turn-item-durable-assistant", "持久回复");
    assistant_item.kind = "assistant_stream".to_string();
    assistant_item.status = "running".to_string();
    store
        .upsert_current_turn_item(&session_id, assistant_item)
        .expect("assistant item should upsert");

    store
        .update_current_turn_status(&session_id, "completed")
        .expect("turn status should update");

    let durable = store.durable_state();
    let turn = durable
        .canonical_turns
        .iter()
        .find(|turn| turn.turn_id == "turn-durable")
        .expect("canonical turn should be durable");
    assert_eq!(turn.status, crate::models::CanonicalTurnStatus::Completed);
    assert_eq!(turn.items.len(), 1);
    assert_eq!(turn.items[0].item_id, "turn-item-durable-assistant");
    assert_eq!(
        turn.items[0].kind,
        crate::models::CanonicalTurnItemKind::AssistantText
    );
    assert_eq!(turn.items[0].content.as_deref(), Some("持久回复"));
}

#[test]
fn canonical_turn_request_id_lookup_survives_normalization_and_restore() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-request-id-lookup");
    store
        .create_session(session_id.clone(), "Request ID Lookup")
        .expect("session should be creatable");
    store
        .upsert_current_turn(
            session_id.clone(),
            test_turn("turn-request-id", "running", 10),
        )
        .expect("turn should be accepted");

    let mut item = test_turn_item("item-request-id", "幂等请求");
    item.request_id = Some("request-id-1".to_string());
    item.item_seq = 2;
    store
        .upsert_current_turn_item(&session_id, item)
        .expect("request item should be persisted");

    let found = store
        .canonical_turn_for_request_id(" request-id-1 ")
        .expect("request id should resolve canonical turn");
    assert_eq!(found.turn_id, "turn-request-id");
    assert_eq!(found.items[0].item_seq, 2);

    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    );
    assert_eq!(
        restored
            .canonical_turn_for_request_id("request-id-1")
            .map(|turn| turn.turn_id),
        Some("turn-request-id".to_string())
    );
    assert!(
        restored
            .canonical_turn_for_request_id("unknown-request")
            .is_none()
    );
}

#[test]
fn completed_current_turn_marks_session_completion_unread() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-unread-completion");
    store
        .create_session(session_id.clone(), "Unread Completion")
        .expect("session should be creatable");
    store
        .upsert_current_turn(
            session_id.clone(),
            test_turn("turn-unread-completion", "running", 10),
        )
        .expect("running turn should upsert");

    store
        .update_current_turn_status(&session_id, "completed")
        .expect("turn should complete");

    let session = store.session(&session_id).expect("session should exist");
    assert!(session.last_completed_at.is_some());
    assert!(session.has_unread_completion());
}

#[test]
fn completed_root_task_marks_session_completion_unread() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-root-task-unread-completion");
    store
        .create_session(session_id.clone(), "Root Task Unread Completion")
        .expect("session should be creatable");
    store
        .upsert_current_turn(
            session_id.clone(),
            test_turn("turn-root-task-unread-completion", "running", 10),
        )
        .expect("running turn should upsert");

    store
        .complete_current_turn_from_completed_root_task(&session_id)
        .expect("root task should complete current turn");

    let session = store.session(&session_id).expect("session should exist");
    assert!(session.last_completed_at.is_some());
    assert!(session.has_unread_completion());
}

#[test]
fn marking_session_viewed_clears_unread_completion_and_survives_restore() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-viewed-completion");
    store
        .create_session(session_id.clone(), "Viewed Completion")
        .expect("session should be creatable");
    store
        .upsert_current_turn(
            session_id.clone(),
            test_turn("turn-viewed-completion", "running", 10),
        )
        .expect("running turn should upsert");
    store
        .update_current_turn_status(&session_id, "completed")
        .expect("turn should complete");
    let completed_at = store
        .session(&session_id)
        .and_then(|session| session.last_completed_at)
        .expect("completion timestamp should exist");

    store
        .mark_session_viewed_at(&session_id, UtcMillis(completed_at.0.saturating_add(1)))
        .expect("session should be marked viewed");

    let session = store.session(&session_id).expect("session should exist");
    assert_eq!(
        session.last_viewed_at,
        Some(UtcMillis(completed_at.0.saturating_add(1)))
    );
    assert!(!session.has_unread_completion());

    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    );
    let restored_session = restored
        .session(&session_id)
        .expect("session should restore");
    assert_eq!(restored_session.last_completed_at, Some(completed_at));
    assert_eq!(restored_session.last_viewed_at, session.last_viewed_at);
    assert!(!restored_session.has_unread_completion());
}

#[test]
fn failed_current_turn_does_not_mark_successful_completion_unread() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-failed-no-unread-completion");
    store
        .create_session(session_id.clone(), "Failed Completion")
        .expect("session should be creatable");
    store
        .upsert_current_turn(
            session_id.clone(),
            test_turn("turn-failed-no-unread-completion", "running", 10),
        )
        .expect("running turn should upsert");

    store
        .update_current_turn_status(&session_id, "failed")
        .expect("turn should fail");

    let session = store.session(&session_id).expect("session should exist");
    assert_eq!(session.last_completed_at, None);
    assert!(!session.has_unread_completion());
}

#[test]
fn replaying_completed_current_turn_keeps_original_completion_read_state() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-completed-replay-idempotent");
    store
        .create_session(session_id.clone(), "Completed Replay")
        .expect("session should be creatable");
    store
        .upsert_current_turn(
            session_id.clone(),
            test_turn("turn-completed-replay", "running", 10),
        )
        .expect("running turn should upsert");
    store
        .update_current_turn_status(&session_id, "completed")
        .expect("turn should complete");
    let completed_at = store
        .session(&session_id)
        .and_then(|session| session.last_completed_at)
        .expect("completion timestamp should exist");
    store
        .mark_session_viewed_at(&session_id, UtcMillis(completed_at.0.saturating_add(1)))
        .expect("session should be marked viewed");

    store
        .update_current_turn_status(&session_id, "completed")
        .expect("completed replay should remain valid");

    let session = store.session(&session_id).expect("session should exist");
    assert_eq!(session.last_completed_at, Some(completed_at));
    assert!(!session.has_unread_completion());
}

#[test]
fn current_turn_hides_runtime_internal_tool_calls_in_durable_canonical_log() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-durable-internal-tool-hidden");
    store
        .create_session(session_id.clone(), "Durable Internal Tool Hidden")
        .expect("session should be creatable");
    store
        .upsert_current_turn(
            session_id.clone(),
            test_turn("turn-internal-tool", "running", 10),
        )
        .expect("running turn should upsert");

    let mut wait_item = test_turn_item("turn-item-agent-wait", "{\"status\":\"succeeded\"}");
    wait_item.kind = "tool_call_result".to_string();
    wait_item.status = "completed".to_string();
    wait_item.title = Some("agent_wait".to_string());
    wait_item.tool_call_id = Some("tool-call-agent-wait".to_string());
    wait_item.tool_name = Some("agent_wait".to_string());
    wait_item.tool_arguments = Some("{\"task_ids\":[\"task-1\"]}".to_string());
    wait_item.tool_result = Some("{\"status\":\"succeeded\"}".to_string());
    store
        .upsert_current_turn_item(&session_id, wait_item)
        .expect("agent_wait item should upsert");

    let mut spawn_item = test_turn_item("turn-item-agent-spawn", "{\"status\":\"started\"}");
    spawn_item.kind = "tool_call_result".to_string();
    spawn_item.status = "completed".to_string();
    spawn_item.title = Some("agent_spawn".to_string());
    spawn_item.tool_call_id = Some("tool-call-agent-spawn".to_string());
    spawn_item.tool_name = Some("agent_spawn".to_string());
    spawn_item.tool_arguments = Some(
        json!({
            "role": "explorer",
            "display_name": "目录探查代理",
            "goal": "读取目录结构"
        })
        .to_string(),
    );
    spawn_item.tool_result = Some("{\"status\":\"started\"}".to_string());
    store
        .upsert_current_turn_item(&session_id, spawn_item)
        .expect("agent_spawn item should upsert");

    let turn = store
        .canonical_turns_for_session(&session_id)
        .into_iter()
        .find(|turn| turn.turn_id == "turn-internal-tool")
        .expect("canonical turn should exist");
    let wait = turn
        .items
        .iter()
        .find(|item| item.item_id == "turn-item-agent-wait")
        .expect("agent_wait canonical item should exist");
    let spawn = turn
        .items
        .iter()
        .find(|item| item.item_id == "turn-item-agent-spawn")
        .expect("agent_spawn canonical item should exist");

    assert!(!wait.visibility.renderable);
    assert!(spawn.visibility.renderable);
}

#[test]
fn image_only_user_message_is_renderable_without_synthetic_text() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-image-only-message");
    store
        .create_session(session_id.clone(), "Image Only Message")
        .expect("session should be creatable");

    let mut turn = test_turn("turn-image-only-message", "running", 10);
    turn.items
        .push(test_turn_item("item-image-only-message", ""));
    let user_message = turn.items.first_mut().expect("user message must exist");
    user_message.content = Some(String::new());
    user_message.metadata.insert(
        "images".to_string(),
        json!([{
            "name": "diagram.png",
            "dataUrl": "data:image/png;base64,AAA"
        }]),
    );
    store
        .upsert_current_turn(session_id.clone(), turn)
        .expect("image-only turn should upsert");

    let user_message = store
        .canonical_turns_for_session(&session_id)
        .into_iter()
        .find(|turn| turn.turn_id == "turn-image-only-message")
        .and_then(|turn| turn.items.into_iter().next())
        .expect("canonical image-only user message should exist");
    assert!(user_message.visibility.renderable);
    assert_eq!(user_message.content.as_deref(), Some(""));
    assert_eq!(user_message.metadata["images"][0]["name"], "diagram.png");
}

#[test]
fn blocked_current_turn_is_terminal_in_canonical_log() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-blocked-terminal-canonical");
    store
        .create_session(session_id.clone(), "Blocked Terminal Canonical")
        .expect("session should be creatable");
    store
        .upsert_current_turn(
            session_id.clone(),
            test_turn("turn-blocked-terminal", "running", 10),
        )
        .expect("turn should upsert");

    let mut assistant_item = test_turn_item("turn-item-blocked-assistant", "等待用户处理");
    assistant_item.kind = "assistant_error".to_string();
    assistant_item.status = "running".to_string();
    store
        .upsert_current_turn_item(&session_id, assistant_item)
        .expect("assistant item should upsert");

    store
        .update_current_turn_status(&session_id, "blocked")
        .expect("blocked turn status should update");

    let turns = store.canonical_turns_for_session(&session_id);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].status, CanonicalTurnStatus::Blocked);
    assert!(
        turns[0].completed_at.is_some(),
        "blocked current turn should not keep the chat UI in running state",
    );
    assert_eq!(
        turns[0].items[0].status,
        crate::models::CanonicalTurnItemStatus::Blocked
    );
}

#[test]
fn killed_current_turn_status_is_stored_as_cancelled_terminal_turn() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-killed-terminal-canonical");
    store
        .create_session(session_id.clone(), "Killed Terminal Canonical")
        .expect("session should be creatable");
    store
        .upsert_current_turn(
            session_id.clone(),
            test_turn("turn-killed-terminal", "running", 10),
        )
        .expect("turn should upsert");

    let mut assistant_item = test_turn_item("turn-item-killed-assistant", "任务执行已终止");
    assistant_item.kind = "assistant_error".to_string();
    assistant_item.status = "running".to_string();
    store
        .upsert_current_turn_item(&session_id, assistant_item)
        .expect("assistant item should upsert");

    let updated = store
        .update_current_turn_status(&session_id, "killed")
        .expect("killed turn status should update")
        .expect("current turn should exist");
    let updated_turn = updated.current_turn.expect("turn should remain");
    assert_eq!(updated_turn.status, "cancelled");
    assert!(
        updated_turn.completed_at.is_some(),
        "killed runner alias should not leave the chat UI in running state",
    );
    assert_eq!(updated_turn.items[0].status, "cancelled");

    let turns = store.canonical_turns_for_session(&session_id);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].status, CanonicalTurnStatus::Cancelled);
    assert!(
        turns[0].completed_at.is_some(),
        "canonical cancelled turn should carry a terminal timestamp",
    );
    assert_eq!(
        turns[0].items[0].status,
        crate::models::CanonicalTurnItemStatus::Cancelled
    );
}

#[test]
fn daemon_restart_interruption_is_terminal_recoverable_and_single_claim() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-daemon-restart-interrupted");
    store
        .create_session(session_id.clone(), "Daemon Restart Interrupted")
        .expect("session should be creatable");

    let mut turn = test_turn("turn-daemon-restart-interrupted", "running", 10);
    let mut user_item = test_turn_item("turn-item-daemon-restart-user", "继续处理任务");
    user_item.item_seq = 1;
    let mut assistant_item = test_turn_item("turn-item-daemon-restart-assistant", "处理中");
    assistant_item.item_seq = 2;
    assistant_item.kind = "assistant_stream".to_string();
    assistant_item.status = "running".to_string();
    assistant_item.source = "orchestrator".to_string();
    turn.items = vec![user_item, assistant_item];
    let chain = test_active_chain(&session_id, "daemon-restart-interrupted", Some(turn));
    store
        .accept_active_execution_chain_with_timeline_entry(
            session_id.clone(),
            TimelineEntryInput::new(
                "timeline-daemon-restart-interrupted",
                TimelineEntryKind::UserMessage,
                "继续处理任务",
                UtcMillis(10),
            ),
            chain,
        )
        .expect("active chain should be accepted");

    {
        let mut state = store
            .state
            .write()
            .expect("session state write lock should hold");
        state
            .execution_sidecar_store
            .runtime_sidecars
            .iter_mut()
            .find(|sidecar| sidecar.session_id == session_id)
            .expect("runtime sidecar should exist")
            .updated_at = UtcMillis(2_500);
    }

    let interrupted = store
        .interrupt_current_turn_by_daemon_restart(&session_id)
        .expect("daemon restart should interrupt current turn")
        .expect("current turn should exist");
    let interrupted_turn = interrupted
        .current_turn
        .expect("turn should remain durable");
    assert_eq!(interrupted_turn.status, "interrupted");
    assert_eq!(interrupted_turn.completed_at, Some(UtcMillis(2_500)));
    assert_eq!(interrupted_turn.items[1].status, "cancelled");
    assert!(store.has_recovery_ready_interruption(&session_id));
    store
        .ensure_current_turn_acceptance_available(&session_id)
        .expect("interrupted turn must not block the next turn");

    let canonical_turn = store
        .canonical_turns_for_session(&session_id)
        .into_iter()
        .find(|turn| turn.turn_id == "turn-daemon-restart-interrupted")
        .expect("canonical interrupted turn should exist");
    assert_eq!(canonical_turn.status, CanonicalTurnStatus::Interrupted);
    assert!(canonical_turn.status.is_terminal());
    let notice = canonical_turn
        .items
        .iter()
        .find(|item| item.metadata.get("noticeKind") == Some(&json!("session_interrupted")))
        .expect("interruption notice should be appended");
    assert_eq!(canonical_turn.completed_at, Some(UtcMillis(2_500)));
    assert_eq!(notice.metadata.get("interruptedAt"), Some(&json!(2_500)));
    assert_eq!(
        notice.kind,
        crate::models::CanonicalTurnItemKind::SystemNotice
    );
    assert_eq!(
        notice.status,
        crate::models::CanonicalTurnItemStatus::Completed
    );
    assert_eq!(notice.metadata.get("recoveryState"), Some(&json!("ready")));

    let claimed_turn_id = store
        .claim_interrupted_recovery(&session_id)
        .expect("first recovery claim should succeed")
        .expect("interrupted recovery should be claimed");
    assert_eq!(claimed_turn_id, "turn-daemon-restart-interrupted");
    assert!(!store.has_recovery_ready_interruption(&session_id));
    assert!(
        store.claim_interrupted_recovery(&session_id).is_err(),
        "a second caller must not claim the same interrupted execution"
    );

    store
        .release_interrupted_recovery_claim(&session_id, &claimed_turn_id)
        .expect("failed recovery should release the claim");
    assert!(store.has_recovery_ready_interruption(&session_id));
}

#[test]
fn killed_task_status_item_is_written_as_cancelled_canonical_item() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-killed-task-status-item");
    store
        .create_session(session_id.clone(), "Killed Task Status Item")
        .expect("session should be creatable");
    store
        .upsert_current_turn(
            session_id.clone(),
            test_turn("turn-killed-task-status-item", "running", 10),
        )
        .expect("turn should upsert");

    let mut task_item = test_turn_item("turn-item-task-killed", "子任务已终止");
    task_item.kind = "task_status".to_string();
    task_item.status = "killed".to_string();
    task_item.source = "task".to_string();
    store
        .upsert_current_turn_item(&session_id, task_item)
        .expect("killed task status item should upsert");

    let turns = store.canonical_turns_for_session(&session_id);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].status, CanonicalTurnStatus::Running);
    assert_eq!(
        turns[0].items[0].status,
        crate::models::CanonicalTurnItemStatus::Cancelled
    );
}

#[test]
fn persisted_parts_keep_durable_terminal_turn_over_stale_sidecar_running_turn() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-sidecar-terminal-wins");
    store
        .create_session(session_id.clone(), "Sidecar Terminal Wins")
        .expect("session should be creatable");
    store
        .upsert_current_turn(
            session_id.clone(),
            test_turn("turn-terminal-wins", "running", 10),
        )
        .expect("turn should upsert");
    store
        .update_current_turn_status(&session_id, "completed")
        .expect("turn should complete");

    let durable_state = store.durable_state();
    let mut sidecar_store = store.execution_sidecar_store_state();
    let stale_turn = sidecar_store.runtime_sidecars[0]
        .current_turn
        .as_mut()
        .expect("sidecar current turn should exist");
    stale_turn.status = "running".to_string();
    stale_turn.completed_at = None;

    let restored = SessionStore::from_persisted_parts(durable_state, sidecar_store);
    let turns = restored.canonical_turns_for_session(&session_id);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].status, CanonicalTurnStatus::Completed);
}

#[test]
fn persisted_parts_repairs_terminal_turn_with_stale_active_items() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-terminal-active-item-repair");
    store
        .create_session(session_id.clone(), "Terminal Active Item Repair")
        .expect("session should be creatable");
    store
        .upsert_current_turn(
            session_id.clone(),
            test_turn("turn-terminal-active-item-repair", "running", 10),
        )
        .expect("turn should upsert");

    let mut assistant_item = test_turn_item("turn-item-terminal-active", "任务执行需要处理");
    assistant_item.kind = "assistant_error".to_string();
    assistant_item.status = "running".to_string();
    store
        .upsert_current_turn_item(&session_id, assistant_item)
        .expect("assistant item should upsert");
    store
        .update_current_turn_status(&session_id, "blocked")
        .expect("turn should become blocked");

    let mut durable_state = store.durable_state();
    durable_state.canonical_turns[0].items[0].status =
        crate::models::CanonicalTurnItemStatus::Running;
    let sidecar_store = store.execution_sidecar_store_state();

    let restored = SessionStore::from_persisted_parts(durable_state, sidecar_store);
    let turns = restored.canonical_turns_for_session(&session_id);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].status, CanonicalTurnStatus::Blocked);
    assert_eq!(
        turns[0].items[0].status,
        crate::models::CanonicalTurnItemStatus::Blocked
    );
}

#[test]
fn upsert_current_turn_item_rejects_canonical_immutable_field_conflict() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-canonical-conflict");
    store
        .create_session(session_id.clone(), "Canonical Conflict")
        .expect("session should be creatable");
    store
        .upsert_current_turn(session_id.clone(), test_turn("turn-running", "running", 1))
        .expect("running turn should upsert");

    let mut stream_item = test_turn_item("turn-item-conflict", "流式回复");
    stream_item.kind = "assistant_stream".to_string();
    stream_item.status = "running".to_string();
    store
        .upsert_current_turn_item(&session_id, stream_item)
        .expect("stream item should upsert");

    let mut conflicting_item = test_turn_item("turn-item-conflict", "工具调用");
    conflicting_item.kind = "tool_call_started".to_string();
    conflicting_item.status = "running".to_string();
    conflicting_item.tool_call_id = Some("tool-conflict".to_string());
    conflicting_item.tool_name = Some("shell_exec".to_string());
    let result = store.upsert_current_turn_item(&session_id, conflicting_item);

    assert!(matches!(
        result,
        Err(magi_core::DomainError::InvalidState { .. })
    ));
    let stored_item = store
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .and_then(|turn| {
            turn.items
                .into_iter()
                .find(|item| item.item_id == "turn-item-conflict")
        })
        .expect("original item should remain");
    assert_eq!(stored_item.kind, "assistant_stream");
    assert_eq!(stored_item.content.as_deref(), Some("流式回复"));
}

#[test]
fn upsert_current_turn_item_rejects_canonical_status_regression() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-canonical-status-regression");
    store
        .create_session(session_id.clone(), "Canonical Status Regression")
        .expect("session should be creatable");
    store
        .upsert_current_turn(session_id.clone(), test_turn("turn-running", "running", 1))
        .expect("running turn should upsert");

    let mut final_item = test_turn_item("turn-item-status", "最终回复");
    final_item.kind = "assistant_final".to_string();
    final_item.status = "completed".to_string();
    store
        .upsert_current_turn_item(&session_id, final_item)
        .expect("final item should upsert");

    let mut failed_item = test_turn_item("turn-item-status", "失败回复");
    failed_item.kind = "assistant_error".to_string();
    failed_item.status = "failed".to_string();
    let result = store.upsert_current_turn_item(&session_id, failed_item);

    assert!(matches!(
        result,
        Err(magi_core::DomainError::InvalidState { .. })
    ));
    let stored_item = store
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .and_then(|turn| {
            turn.items
                .into_iter()
                .find(|item| item.item_id == "turn-item-status")
        })
        .expect("completed item should remain");
    assert_eq!(stored_item.status, "completed");
    assert_eq!(stored_item.content.as_deref(), Some("最终回复"));
}

#[test]
fn upsert_current_turn_item_for_turn_rejects_a_stale_turn_owner() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-turn-owner-check");
    store
        .create_session(session_id.clone(), "Turn Owner Check")
        .expect("session should be creatable");
    store
        .upsert_current_turn(session_id.clone(), test_turn("turn-current", "running", 1))
        .expect("running turn should upsert");

    let result = store.upsert_current_turn_item_for_turn(
        &session_id,
        Some("turn-stale"),
        test_turn_item("browser-tool-stale", "stale browser result"),
    );

    assert!(matches!(
        result,
        Err(magi_core::DomainError::CurrentTurnConflict { .. })
    ));
    assert!(
        store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .is_some_and(|turn| turn.items.is_empty())
    );
}

#[test]
fn append_current_turn_item_with_timeline_entry_does_not_write_timeline_without_turn() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-append-item-no-turn");
    store
        .create_session(session_id.clone(), "Append Item No Turn")
        .expect("session should be creatable");
    store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            ..ExecutionOwnership::default()
        },
    );

    let updated = store
        .append_current_turn_item_with_timeline_entry(
            &session_id,
            TimelineEntryInput::new(
                "timeline-no-turn",
                TimelineEntryKind::UserMessage,
                "不应写入的继续消息",
                UtcMillis(2),
            ),
            test_turn_item("turn-item-no-turn", "不应写入的继续消息"),
        )
        .expect("missing current turn is a non-mutating no-op");

    assert!(updated.is_none());
    assert!(
        !store
            .timeline_for_session(&session_id)
            .iter()
            .any(|entry| entry.entry_id == "timeline-no-turn"),
        "current_turn 不存在时不能留下 continue 用户 timeline"
    );
}

#[test]
fn sidecar_rejects_legacy_recovery_ref_json() {
    let legacy_payload = json!({
        "current_session_id": null,
        "sessions": [],
        "timeline": [],
        "notifications": [],
        "runtime_sidecars": [{
            "session_id": "session-legacy",
            "ownership": {
                "session_id": "session-legacy",
                "workspace_id": null,
                "mission_id": null,
                "task_id": null,
                "worker_id": null,
                "execution_chain_ref": "chain-legacy"
            },
            "recovery_ref": "recovery-legacy",
            "updated_at": 1
        }]
    });

    serde_json::from_value::<SessionStoreState>(legacy_payload)
        .expect_err("legacy recovery_ref 字段必须拒绝，避免恢复链路静默丢失 recovery_id");

    let canonical_payload = json!({
        "current_session_id": null,
        "sessions": [],
        "timeline": [],
        "notifications": [],
        "runtime_sidecars": [{
            "session_id": "session-canonical",
            "ownership": {
                "session_id": "session-canonical",
                "workspace_id": null,
                "mission_id": null,
                "task_id": null,
                "worker_id": null,
                "execution_chain_ref": "chain-canonical"
            },
            "recovery_id": "recovery-canonical",
            "updated_at": 1
        }]
    });

    let state: SessionStoreState =
        serde_json::from_value(canonical_payload).expect("canonical payload");
    let sidecar = state
        .execution_sidecar_store
        .runtime_sidecars
        .first()
        .expect("sidecar should exist");
    assert_eq!(sidecar.recovery_id.as_deref(), Some("recovery-canonical"));
    assert_eq!(sidecar.status, SessionExecutionSidecarStatus::Detached);
}

#[test]
fn durable_records_reject_legacy_snake_case_fields() {
    let legacy_payload = json!({
        "current_session_id": "session-legacy-record",
        "sessions": [{
            "session_id": "session-legacy-record",
            "title": "legacy session",
            "status": "Active",
            "created_at": 1,
            "updated_at": 2,
            "message_count": 1,
            "workspace_id": "workspace-legacy"
        }],
        "timeline": [{
            "entry_id": "entry-legacy",
            "session_id": "session-legacy-record",
            "kind": "UserMessage",
            "message": "legacy",
            "occurred_at": 3
        }],
        "runtime_sidecars": [],
        "notifications": [{
            "notification_id": "notification-legacy",
            "session_id": "session-legacy-record",
            "kind": "info",
            "message": "legacy notification",
            "created_at": 4,
            "handled": false
        }]
    });

    serde_json::from_value::<SessionStoreState>(legacy_payload)
        .expect_err("session 持久化外层 record 不得继续接受 legacy snake_case 字段");

    let canonical_payload = json!({
        "current_session_id": "session-canonical-record",
        "sessions": [{
            "sessionId": "session-canonical-record",
            "title": "canonical session",
            "status": "Active",
            "createdAt": 1,
            "updatedAt": 2,
            "messageCount": 1,
            "workspaceId": "workspace-canonical"
        }],
        "timeline": [{
            "entryId": "entry-canonical",
            "sessionId": "session-canonical-record",
            "kind": "UserMessage",
            "message": "canonical",
            "occurredAt": 3
        }],
        "runtime_sidecars": [],
        "notifications": [{
            "notificationId": "notification-canonical",
            "sessionId": "session-canonical-record",
            "kind": "info",
            "message": "canonical notification",
            "createdAt": 4,
            "handled": false
        }]
    });

    let state: SessionStoreState =
        serde_json::from_value(canonical_payload).expect("canonical durable records");
    assert_eq!(
        state.sessions[0].session_id.as_str(),
        "session-canonical-record"
    );
    assert_eq!(state.timeline[0].entry_id, "entry-canonical");
    assert_eq!(
        state.notifications[0].notification_id,
        "notification-canonical"
    );
}

#[test]
fn persisted_parts_round_trip_preserves_sidecars() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-persisted");
    store
        .create_session(session_id.clone(), "Persisted Session")
        .expect("session should be creatable");
    store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(WorkspaceId::new("workspace-persisted")),
            execution_chain_ref: Some("chain-persisted".to_string()),
            ..ExecutionOwnership::default()
        },
    );
    store
        .attach_recovery_id(&session_id, Some("recovery-persisted".to_string()))
        .expect("recovery id should be attachable");

    let durable_state = store.durable_state();
    let sidecar_store = store.execution_sidecar_store_state();
    let restored = SessionStore::from_persisted_parts(durable_state, sidecar_store);

    let export = restored
        .execution_sidecar_export(&session_id)
        .expect("restored sidecar export should exist");
    assert_eq!(
        export.current_status,
        SessionExecutionSidecarStatus::RecoveryLinked
    );
    assert_eq!(
        export.execution_chain_ref.as_deref(),
        Some("chain-persisted")
    );
    assert_eq!(export.recovery_ref.as_deref(), Some("recovery-persisted"));
}

#[test]
fn incident_notifications_are_scoped_and_legacy_audits_are_removed() {
    fn incident(
        id: &str,
        scope: NotificationScope,
        workspace_id: Option<&str>,
        session_id: Option<&str>,
    ) -> NotificationRecord {
        NotificationRecord {
            notification_id: id.to_string(),
            session_id: session_id.map(SessionId::new),
            workspace_id: workspace_id.map(str::to_string),
            scope,
            kind: "incident".to_string(),
            level: Some("error".to_string()),
            title: None,
            message: id.to_string(),
            detail: None,
            error_code: None,
            failure_stage: None,
            task_id: None,
            request_id: None,
            source: Some("test".to_string()),
            created_at: UtcMillis(10),
            handled: false,
            action_required: true,
            count_unread: true,
            fingerprint: format!("fingerprint-{id}"),
            occurrence_count: 1,
            resolved: false,
        }
    }

    let session_id = SessionId::new("session-notification-scope");
    let personal_session_id = SessionId::new("session-personal-notification-scope");
    let durable = SessionDurableState {
        notifications: vec![
            incident("app-incident", NotificationScope::App, None, None),
            incident(
                "workspace-incident",
                NotificationScope::Workspace,
                Some("workspace-a"),
                None,
            ),
            incident(
                "session-incident",
                NotificationScope::Session,
                Some("workspace-a"),
                Some(session_id.as_str()),
            ),
            incident(
                "personal-session-incident",
                NotificationScope::Session,
                None,
                Some(personal_session_id.as_str()),
            ),
            NotificationRecord {
                kind: "audit".to_string(),
                ..incident(
                    "legacy-audit",
                    NotificationScope::Session,
                    Some("workspace-a"),
                    Some(session_id.as_str()),
                )
            },
        ],
        ..SessionDurableState::default()
    };
    let store =
        SessionStore::from_persisted_parts(durable, SessionExecutionSidecarStoreState::default());

    let workspace_context = NotificationContext::workspace("workspace-a", Some(session_id.clone()));
    let records = store.notifications_for_context(&workspace_context);
    assert_eq!(records.len(), 3);
    assert!(
        records
            .iter()
            .any(|item| item.notification_id == "app-incident")
    );
    assert!(
        records
            .iter()
            .any(|item| item.notification_id == "workspace-incident")
    );
    assert!(
        records
            .iter()
            .any(|item| item.notification_id == "session-incident")
    );
    assert!(
        store
            .notifications()
            .iter()
            .all(|item| item.kind == "incident")
    );
    assert!(
        store
            .notifications_for_context(&NotificationContext::workspace(
                "workspace-b",
                Some(SessionId::new("session-notification-other")),
            ))
            .iter()
            .all(|item| item.notification_id != "session-incident"),
        "session incident must not cross session boundaries"
    );

    store
        .resolve_notification_for_context(&workspace_context, "session-incident")
        .expect("session incident should resolve in its context");
    let resolved = store
        .notifications_for_context(&workspace_context)
        .into_iter()
        .find(|item| item.notification_id == "session-incident")
        .expect("resolved incident should remain visible");
    assert!(resolved.resolved);
    assert!(resolved.handled);

    let personal_context = NotificationContext::personal(Some(personal_session_id.clone()));
    let personal_records = store.notifications_for_context(&personal_context);
    assert!(
        personal_records
            .iter()
            .any(|item| item.notification_id == "personal-session-incident")
    );
    assert!(
        personal_records
            .iter()
            .all(|item| item.workspace_id.is_none())
    );
    assert!(
        store
            .notifications_for_context(&NotificationContext::personal(Some(session_id.clone())))
            .iter()
            .all(|item| item.notification_id != "personal-session-incident")
    );

    let mut first_occurrence = incident(
        "failure-occurrence-1",
        NotificationScope::Session,
        Some("workspace-a"),
        Some(session_id.as_str()),
    );
    first_occurrence.fingerprint = "same-runtime-failure".to_string();
    first_occurrence.message = "provider timeout on attempt 1".to_string();
    let mut second_occurrence = incident(
        "failure-occurrence-2",
        NotificationScope::Session,
        Some("workspace-a"),
        Some(session_id.as_str()),
    );
    second_occurrence.fingerprint = "same-runtime-failure".to_string();
    second_occurrence.message = "provider timeout on attempt 2".to_string();

    store
        .append_incident_record(first_occurrence)
        .expect("first failure occurrence should persist");
    store
        .append_incident_record(second_occurrence)
        .expect("second failure occurrence should persist");

    let occurrences = store
        .notifications_for_context(&workspace_context)
        .into_iter()
        .filter(|item| item.fingerprint == "same-runtime-failure")
        .collect::<Vec<_>>();
    assert_eq!(occurrences.len(), 2);
    assert_ne!(
        occurrences[0].notification_id,
        occurrences[1].notification_id
    );
    assert_eq!(occurrences[0].occurrence_count, 1);
    assert_eq!(occurrences[1].occurrence_count, 1);

    for index in 0..=MAX_INCIDENT_NOTIFICATION_RECORDS {
        let mut record = incident(
            &format!("retained-failure-{index}"),
            NotificationScope::App,
            None,
            None,
        );
        record.created_at = UtcMillis(100 + index as u64);
        store
            .append_incident_record(record)
            .expect("retained failure should persist");
    }
    let retained = store.notifications();
    assert_eq!(retained.len(), MAX_INCIDENT_NOTIFICATION_RECORDS);
    assert!(
        retained
            .iter()
            .all(|record| record.notification_id != "session-incident"),
        "已解决的旧记录应优先被保留策略清理"
    );
    assert!(
        retained
            .iter()
            .any(|record| record.notification_id == "retained-failure-1000"),
        "最新错误记录必须保留"
    );
}

#[test]
fn execution_sidecar_flush_metadata_tracks_recovery_apply_and_resume() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-metadata");
    let workspace_id = WorkspaceId::new("workspace-metadata");
    store
        .create_session(session_id.clone(), "metadata session")
        .expect("session should be creatable");

    store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            execution_chain_ref: Some("chain-metadata".to_string()),
            ..ExecutionOwnership::default()
        },
    );
    let bound_metadata = store.execution_sidecar_flush_metadata();
    assert_eq!(bound_metadata.current_version, 1);
    assert_eq!(bound_metadata.flushed_version, 0);
    assert_eq!(
        bound_metadata.last_dirty_reason,
        Some(SessionSidecarFlushReason::BindExecutionOwnership)
    );
    assert!(bound_metadata.last_dirty_at.is_some());
    assert_eq!(bound_metadata.next_flush_hint, bound_metadata.last_dirty_at);

    store
        .apply_recovery_resume_input(
            session_id.clone(),
            RecoveryResumeInput {
                recovery_id: "recovery-metadata".to_string(),
                snapshot_id: "snapshot-metadata".to_string(),
                ownership: ExecutionOwnership {
                    session_id: Some(session_id.clone()),
                    workspace_id: Some(workspace_id.clone()),
                    execution_chain_ref: Some("chain-metadata".to_string()),
                    ..ExecutionOwnership::default()
                },
                diagnostic_summary: Some("diagnostic metadata".to_string()),
                created_at: UtcMillis::now(),
                updated_at: UtcMillis::now(),
            },
        )
        .expect("recovery input should apply");
    let recovery_metadata = store.execution_sidecar_flush_metadata();
    assert_eq!(recovery_metadata.current_version, 2);
    assert_eq!(
        recovery_metadata.last_dirty_reason,
        Some(SessionSidecarFlushReason::ApplyRecoveryResumeInput)
    );
    assert!(recovery_metadata.last_dirty_at.is_some());
    assert_eq!(
        recovery_metadata.next_flush_hint,
        recovery_metadata.last_dirty_at
    );

    let updated = store
        .apply_resume_execution_target(
            &session_id,
            &TaskExecutionTarget {
                mission_id: MissionId::new("mission-metadata"),
                root_task_id: TaskId::new("task-root-metadata"),
                task_id: TaskId::new("todo-metadata"),
                requested_worker_id: Some(WorkerId::new("worker-metadata")),
                recovery_id: Some("recovery-metadata".to_string()),
                execution_chain_ref: Some("chain-metadata".to_string()),
            },
        )
        .expect("resume execution target should apply");
    assert_eq!(updated.status, SessionExecutionSidecarStatus::Resumed);
    let resume_metadata = store.execution_sidecar_flush_metadata();
    assert_eq!(resume_metadata.current_version, 3);
    assert_eq!(
        resume_metadata.last_dirty_reason,
        Some(SessionSidecarFlushReason::ApplyResumeExecutionTarget)
    );
    assert!(resume_metadata.last_dirty_at.is_some());
    assert_eq!(
        resume_metadata.next_flush_hint,
        resume_metadata.last_dirty_at
    );

    let mut flushes = Vec::new();
    assert!(
        store
            .flush_execution_sidecars_with(|state| {
                flushes.push(state.runtime_sidecars.len());
                Ok::<_, std::io::Error>(())
            })
            .expect("dirty sidecar flush should succeed")
    );
    assert_eq!(flushes, vec![1]);
    let flushed_metadata = store.execution_sidecar_flush_metadata();
    assert_eq!(
        flushed_metadata.current_version,
        flushed_metadata.flushed_version
    );
    assert!(flushed_metadata.last_flush_at.is_some());
    assert_eq!(flushed_metadata.next_flush_hint, None);
}

#[test]
fn full_recovery_lifecycle_bind_resume_input_dispatch_with_consistency_checks() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-recovery-full");
    let workspace_id = WorkspaceId::new("workspace-recovery-full");
    store
        .create_session(session_id.clone(), "Recovery Lifecycle")
        .expect("session should be creatable");

    store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            execution_chain_ref: Some("chain-recovery-full".to_string()),
            ..ExecutionOwnership::default()
        },
    );
    let sidecar = store
        .runtime_sidecar(&session_id)
        .expect("sidecar should exist after bind");
    assert_eq!(sidecar.status, SessionExecutionSidecarStatus::Bound);
    assert!(sidecar.recovery_id.is_none());
    assert_eq!(
        sidecar.ownership.execution_chain_ref.as_deref(),
        Some("chain-recovery-full")
    );

    let export = store
        .execution_sidecar_export(&session_id)
        .expect("export should exist");
    assert_eq!(export.current_status, SessionExecutionSidecarStatus::Bound);
    assert_eq!(
        export.execution_chain_ref.as_deref(),
        Some("chain-recovery-full")
    );
    assert!(export.recovery_ref.is_none());
    let projection = store.projection_input();
    assert_eq!(projection.current_session_id, Some(session_id.clone()));
    assert_eq!(projection.sessions.len(), 1);

    store
        .apply_recovery_resume_input(
            session_id.clone(),
            RecoveryResumeInput {
                recovery_id: "recovery-full".to_string(),
                snapshot_id: "snapshot-full".to_string(),
                ownership: ExecutionOwnership {
                    session_id: Some(session_id.clone()),
                    workspace_id: Some(workspace_id.clone()),
                    execution_chain_ref: Some("chain-recovery-full".to_string()),
                    ..ExecutionOwnership::default()
                },
                diagnostic_summary: Some("test diagnostic".to_string()),
                created_at: UtcMillis::now(),
                updated_at: UtcMillis::now(),
            },
        )
        .expect("recovery input should apply");
    let sidecar = store
        .runtime_sidecar(&session_id)
        .expect("sidecar should exist after recovery input");
    assert_eq!(
        sidecar.status,
        SessionExecutionSidecarStatus::RecoveryLinked
    );
    assert_eq!(sidecar.recovery_id.as_deref(), Some("recovery-full"));
    assert_eq!(
        sidecar.ownership.execution_chain_ref.as_deref(),
        Some("chain-recovery-full")
    );

    let export = store
        .execution_sidecar_export(&session_id)
        .expect("export should exist after recovery link");
    assert_eq!(
        export.current_status,
        SessionExecutionSidecarStatus::RecoveryLinked
    );
    assert_eq!(export.recovery_ref.as_deref(), Some("recovery-full"));

    let resumed = store
        .apply_resume_execution_target(
            &session_id,
            &TaskExecutionTarget {
                mission_id: MissionId::new("mission-full"),
                root_task_id: TaskId::new("task-root-full"),
                task_id: TaskId::new("todo-full"),
                requested_worker_id: Some(WorkerId::new("worker-full")),
                recovery_id: Some("recovery-full".to_string()),
                execution_chain_ref: Some("chain-recovery-full".to_string()),
            },
        )
        .expect("resume execution target should apply");
    assert_eq!(resumed.status, SessionExecutionSidecarStatus::Resumed);
    assert_eq!(
        resumed.ownership.mission_id,
        Some(MissionId::new("mission-full"))
    );
    assert_eq!(resumed.ownership.task_id, Some(TaskId::new("todo-full")));
    assert_eq!(
        resumed.ownership.worker_id,
        Some(WorkerId::new("worker-full"))
    );
    assert_eq!(resumed.ownership.session_id, Some(session_id.clone()));
    assert_eq!(resumed.ownership.workspace_id, Some(workspace_id.clone()));
    assert_eq!(
        resumed.ownership.execution_chain_ref.as_deref(),
        Some("chain-recovery-full")
    );
    assert_eq!(resumed.recovery_id.as_deref(), Some("recovery-full"));

    let export = store
        .execution_sidecar_export(&session_id)
        .expect("export should exist after resume");
    assert_eq!(
        export.current_status,
        SessionExecutionSidecarStatus::Resumed
    );
    assert_eq!(export.recovery_ref.as_deref(), Some("recovery-full"));
    assert_eq!(
        export.execution_chain_ref.as_deref(),
        Some("chain-recovery-full")
    );
    assert_eq!(
        export.ownership.mission_id,
        Some(MissionId::new("mission-full"))
    );

    let active = store.active_execution_sidecars();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].session_id, session_id);
    assert_eq!(active[0].status, SessionExecutionSidecarStatus::Resumed);
}

#[test]
fn resumed_status_survives_follow_up_binding_and_chain_refresh() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-resume-preserve");
    let workspace_id = WorkspaceId::new("workspace-resume-preserve");
    let mission_id = MissionId::new("mission-resume-preserve");
    let root_task_id = TaskId::new("task-root-resume-preserve");
    let worker_id = WorkerId::new("worker-resume-preserve");
    let execution_chain_ref = "chain-resume-preserve".to_string();

    store
        .create_session(session_id.clone(), "Resume Preserve")
        .expect("session should be creatable");
    store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            execution_chain_ref: Some(execution_chain_ref.clone()),
            ..ExecutionOwnership::default()
        },
    );
    store
        .apply_recovery_resume_input(
            session_id.clone(),
            RecoveryResumeInput {
                recovery_id: "recovery-resume-preserve".to_string(),
                snapshot_id: "snapshot-resume-preserve".to_string(),
                ownership: ExecutionOwnership {
                    session_id: Some(session_id.clone()),
                    workspace_id: Some(workspace_id.clone()),
                    execution_chain_ref: Some(execution_chain_ref.clone()),
                    ..ExecutionOwnership::default()
                },
                diagnostic_summary: None,
                created_at: UtcMillis::now(),
                updated_at: UtcMillis::now(),
            },
        )
        .expect("recovery input should apply");
    store
        .apply_resume_execution_target(
            &session_id,
            &TaskExecutionTarget {
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                task_id: TaskId::new("task-resume-preserve"),
                requested_worker_id: Some(worker_id.clone()),
                recovery_id: Some("recovery-resume-preserve".to_string()),
                execution_chain_ref: Some(execution_chain_ref.clone()),
            },
        )
        .expect("resume execution target should apply");

    store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            mission_id: Some(mission_id.clone()),
            task_id: Some(TaskId::new("task-resume-preserve-follow-up")),
            worker_id: Some(WorkerId::new("worker-resume-preserve-follow-up")),
            execution_chain_ref: Some(execution_chain_ref.clone()),
        },
    );
    assert_eq!(
        store
            .runtime_sidecar(&session_id)
            .expect("sidecar should exist after follow-up bind")
            .status,
        SessionExecutionSidecarStatus::Resumed
    );

    store
        .upsert_active_execution_chain(
            session_id.clone(),
            ActiveExecutionChain {
                session_id: session_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                execution_chain_ref: execution_chain_ref.clone(),
                workspace_id: Some(workspace_id.clone()),
                active_branch_task_ids: vec![TaskId::new("task-resume-preserve-follow-up")],
                active_worker_bindings: vec![WorkerId::new("worker-resume-preserve-follow-up")],
                branches: vec![ActiveExecutionBranch {
                    task_id: TaskId::new("task-resume-preserve-follow-up"),
                    worker_id: WorkerId::new("worker-resume-preserve-follow-up"),
                    stage: "execute".to_string(),
                    lease_id: None,
                    execution_intent_ref: None,
                    binding_lifecycle: None,
                    checkpoint_stage: None,
                    next_step_index: None,
                    checkpoint_at: None,
                    resume_mode: None,
                    resume_token: None,
                    use_tools: false,
                    skill_name: None,
                    is_primary: true,
                    thread_id: ThreadId::new("thread-resume-preserve-follow-up"),
                }],
                recovery_ref: None,
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: UtcMillis::now(),
                    entry_id: "timeline-resume-preserve".to_string(),
                    trimmed_text: Some("resume preserve".to_string()),
                    skill_name: None,
                },
                current_turn: None,
            },
        )
        .expect("active execution chain should upsert");
    assert_eq!(
        store
            .runtime_sidecar(&session_id)
            .expect("sidecar should exist after chain refresh")
            .status,
        SessionExecutionSidecarStatus::Resumed
    );
}

#[test]
fn clear_ownership_after_resume_resets_to_recovery_linked_or_detached() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-clear-ownership");
    let workspace_id = WorkspaceId::new("workspace-clear-ownership");
    store
        .create_session(session_id.clone(), "Clear Ownership")
        .expect("session should be creatable");

    store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            execution_chain_ref: Some("chain-clear".to_string()),
            ..ExecutionOwnership::default()
        },
    );
    store
        .apply_recovery_resume_input(
            session_id.clone(),
            RecoveryResumeInput {
                recovery_id: "recovery-clear".to_string(),
                snapshot_id: "snapshot-clear".to_string(),
                ownership: ExecutionOwnership {
                    session_id: Some(session_id.clone()),
                    workspace_id: Some(workspace_id.clone()),
                    execution_chain_ref: Some("chain-clear".to_string()),
                    ..ExecutionOwnership::default()
                },
                diagnostic_summary: None,
                created_at: UtcMillis::now(),
                updated_at: UtcMillis::now(),
            },
        )
        .expect("recovery input should apply");
    store
        .apply_resume_execution_target(
            &session_id,
            &TaskExecutionTarget {
                mission_id: MissionId::new("mission-clear"),
                root_task_id: TaskId::new("task-root-clear"),
                task_id: TaskId::new("todo-clear"),
                requested_worker_id: None,
                recovery_id: Some("recovery-clear".to_string()),
                execution_chain_ref: Some("chain-clear".to_string()),
            },
        )
        .expect("resume execution target should apply");

    store
        .clear_execution_ownership(&session_id)
        .expect("clear should succeed");
    let sidecar = store
        .runtime_sidecar(&session_id)
        .expect("sidecar should exist after clear");
    assert_eq!(
        sidecar.status,
        SessionExecutionSidecarStatus::RecoveryLinked
    );
    assert!(sidecar.ownership.session_id.is_none());
    assert!(sidecar.ownership.workspace_id.is_none());
    assert!(sidecar.ownership.mission_id.is_none());
    assert!(sidecar.ownership.task_id.is_none());
    assert!(sidecar.ownership.worker_id.is_none());
    assert!(sidecar.ownership.execution_chain_ref.is_none());
    assert_eq!(sidecar.recovery_id.as_deref(), Some("recovery-clear"));

    let active = store.active_execution_sidecars();
    assert!(active.is_empty());

    store
        .attach_recovery_id(&session_id, None)
        .expect("detach recovery should succeed");
    let sidecar = store
        .runtime_sidecar(&session_id)
        .expect("sidecar should exist after detach");
    assert_eq!(sidecar.status, SessionExecutionSidecarStatus::Detached);
    assert!(sidecar.recovery_id.is_none());
}

#[test]
fn archive_active_execution_chain_detaches_task_panel_without_deleting_history() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-archive-chain");
    let workspace_id = WorkspaceId::new("workspace-archive-chain");
    let mission_id = MissionId::new("mission-archive-chain");
    let root_task_id = TaskId::new("task-root-archive-chain");
    let execution_chain_ref = "chain-archive-chain".to_string();

    store
        .create_session(session_id.clone(), "Archive Chain")
        .expect("session should be creatable");
    store
        .upsert_active_execution_chain(
            session_id.clone(),
            ActiveExecutionChain {
                session_id: session_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                execution_chain_ref: execution_chain_ref.clone(),
                workspace_id: Some(workspace_id.clone()),
                active_branch_task_ids: vec![root_task_id.clone()],
                active_worker_bindings: vec![WorkerId::new("worker-archive-chain")],
                branches: vec![ActiveExecutionBranch {
                    task_id: root_task_id.clone(),
                    worker_id: WorkerId::new("worker-archive-chain"),
                    stage: "execute".to_string(),
                    lease_id: None,
                    execution_intent_ref: None,
                    binding_lifecycle: None,
                    checkpoint_stage: None,
                    next_step_index: None,
                    checkpoint_at: None,
                    resume_mode: None,
                    resume_token: None,
                    use_tools: true,
                    skill_name: None,
                    is_primary: true,
                    thread_id: ThreadId::new("thread-archive-chain"),
                }],
                recovery_ref: Some("recovery-archive-chain".to_string()),
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: UtcMillis::now(),
                    entry_id: "timeline-archive-chain".to_string(),
                    trimmed_text: Some("archive chain".to_string()),
                    skill_name: None,
                },
                current_turn: Some(test_turn("turn-archive-chain", "completed", 42)),
            },
        )
        .expect("active execution chain should upsert");

    store
        .archive_active_execution_chain(&session_id, &root_task_id)
        .expect("archive should succeed");

    let sidecar = store
        .runtime_sidecar(&session_id)
        .expect("sidecar should remain as the session fact source");
    assert_eq!(sidecar.status, SessionExecutionSidecarStatus::Detached);
    assert!(sidecar.active_execution_chain.is_none());
    assert!(sidecar.ownership.mission_id.is_none());
    assert!(sidecar.ownership.task_id.is_none());
    assert!(sidecar.ownership.worker_id.is_none());
    assert!(sidecar.ownership.execution_chain_ref.is_none());
    assert_eq!(sidecar.ownership.session_id.as_ref(), Some(&session_id));
    assert_eq!(sidecar.ownership.workspace_id.as_ref(), Some(&workspace_id));
    assert!(sidecar.current_turn.is_some());
    assert_eq!(
        store.execution_sidecar_flush_metadata().last_dirty_reason,
        Some(SessionSidecarFlushReason::ArchiveActiveExecutionChain)
    );
}

#[test]
fn recovery_resume_rejects_mismatched_recovery_id() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-mismatch-recovery");
    store
        .create_session(session_id.clone(), "Mismatch Recovery")
        .expect("session should be creatable");

    store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            ..ExecutionOwnership::default()
        },
    );
    store
        .attach_recovery_id(&session_id, Some("recovery-A".to_string()))
        .expect("attach recovery_id should succeed");

    let err = store
        .apply_recovery_resume_input(
            session_id.clone(),
            RecoveryResumeInput {
                recovery_id: "recovery-B".to_string(),
                snapshot_id: "snapshot-B".to_string(),
                ownership: ExecutionOwnership::default(),
                diagnostic_summary: None,
                created_at: UtcMillis::now(),
                updated_at: UtcMillis::now(),
            },
        )
        .expect_err("mismatched recovery_id should be rejected");
    assert!(matches!(err, magi_core::DomainError::InvalidState { .. }));

    let err = store
        .apply_resume_execution_target(
            &session_id,
            &TaskExecutionTarget {
                mission_id: MissionId::new("mission-mismatch"),
                root_task_id: TaskId::new("task-root-mismatch"),
                task_id: TaskId::new("todo-mismatch"),
                requested_worker_id: None,
                recovery_id: Some("recovery-B".to_string()),
                execution_chain_ref: None,
            },
        )
        .expect_err("mismatched recovery_id in execution target should be rejected");
    assert!(matches!(err, magi_core::DomainError::InvalidState { .. }));
}

#[test]
fn recovery_resume_rejects_mismatched_execution_chain_ref() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-mismatch-chain");
    store
        .create_session(session_id.clone(), "Mismatch Chain")
        .expect("session should be creatable");

    store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            execution_chain_ref: Some("chain-A".to_string()),
            ..ExecutionOwnership::default()
        },
    );

    let err = store
        .apply_recovery_resume_input(
            session_id.clone(),
            RecoveryResumeInput {
                recovery_id: "recovery-chain".to_string(),
                snapshot_id: "snapshot-chain".to_string(),
                ownership: ExecutionOwnership {
                    execution_chain_ref: Some("chain-B".to_string()),
                    ..ExecutionOwnership::default()
                },
                diagnostic_summary: None,
                created_at: UtcMillis::now(),
                updated_at: UtcMillis::now(),
            },
        )
        .expect_err("mismatched execution_chain_ref should be rejected");
    assert!(matches!(err, magi_core::DomainError::InvalidState { .. }));
}

#[test]
fn multi_session_recovery_sidecars_are_isolated() {
    let store = SessionStore::new();
    let session_a = SessionId::new("session-iso-a");
    let session_b = SessionId::new("session-iso-b");
    let workspace = WorkspaceId::new("workspace-iso");
    store
        .create_session(session_a.clone(), "Session A")
        .expect("session A creatable");
    store
        .create_session(session_b.clone(), "Session B")
        .expect("session B creatable");

    store.bind_execution_ownership(
        session_a.clone(),
        ExecutionOwnership {
            session_id: Some(session_a.clone()),
            workspace_id: Some(workspace.clone()),
            execution_chain_ref: Some("chain-a".to_string()),
            ..ExecutionOwnership::default()
        },
    );
    store
        .apply_recovery_resume_input(
            session_a.clone(),
            RecoveryResumeInput {
                recovery_id: "recovery-a".to_string(),
                snapshot_id: "snapshot-a".to_string(),
                ownership: ExecutionOwnership {
                    session_id: Some(session_a.clone()),
                    workspace_id: Some(workspace.clone()),
                    execution_chain_ref: Some("chain-a".to_string()),
                    ..ExecutionOwnership::default()
                },
                diagnostic_summary: None,
                created_at: UtcMillis::now(),
                updated_at: UtcMillis::now(),
            },
        )
        .expect("session A recovery should apply");

    store.bind_execution_ownership(
        session_b.clone(),
        ExecutionOwnership {
            session_id: Some(session_b.clone()),
            workspace_id: Some(workspace.clone()),
            ..ExecutionOwnership::default()
        },
    );

    let sidecar_a = store.runtime_sidecar(&session_a).expect("sidecar A exists");
    let sidecar_b = store.runtime_sidecar(&session_b).expect("sidecar B exists");
    assert_eq!(
        sidecar_a.status,
        SessionExecutionSidecarStatus::RecoveryLinked
    );
    assert_eq!(sidecar_b.status, SessionExecutionSidecarStatus::Bound);
    assert_eq!(sidecar_a.recovery_id.as_deref(), Some("recovery-a"));
    assert!(sidecar_b.recovery_id.is_none());

    let exports = store.execution_sidecar_exports();
    assert_eq!(exports.len(), 2);
    let export_a = exports
        .iter()
        .find(|export| export.session_id == session_a)
        .expect("export A");
    let export_b = exports
        .iter()
        .find(|export| export.session_id == session_b)
        .expect("export B");
    assert_eq!(
        export_a.current_status,
        SessionExecutionSidecarStatus::RecoveryLinked
    );
    assert_eq!(
        export_b.current_status,
        SessionExecutionSidecarStatus::Bound
    );

    let metadata = store.execution_sidecar_flush_metadata();
    assert_eq!(metadata.current_version, 3);
}

#[test]
fn sidecar_flush_scheduling_with_intermediate_flushes() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-flush-schedule");
    let workspace_id = WorkspaceId::new("workspace-flush-schedule");
    store
        .create_session(session_id.clone(), "Flush Schedule")
        .expect("session should be creatable");

    store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            execution_chain_ref: Some("chain-sched".to_string()),
            ..ExecutionOwnership::default()
        },
    );
    let m1 = store.execution_sidecar_flush_metadata();
    assert_eq!(m1.current_version, 1);
    assert!(m1.next_flush_hint.is_some());

    let flushed = store
        .flush_execution_sidecars_with(|_| Ok::<_, std::io::Error>(()))
        .expect("flush should succeed");
    assert!(flushed);
    let m1f = store.execution_sidecar_flush_metadata();
    assert_eq!(m1f.flushed_version, 1);
    assert!(m1f.next_flush_hint.is_none());
    assert!(m1f.last_flush_at.is_some());

    store
        .apply_recovery_resume_input(
            session_id.clone(),
            RecoveryResumeInput {
                recovery_id: "recovery-sched".to_string(),
                snapshot_id: "snapshot-sched".to_string(),
                ownership: ExecutionOwnership {
                    session_id: Some(session_id.clone()),
                    workspace_id: Some(workspace_id.clone()),
                    execution_chain_ref: Some("chain-sched".to_string()),
                    ..ExecutionOwnership::default()
                },
                diagnostic_summary: None,
                created_at: UtcMillis::now(),
                updated_at: UtcMillis::now(),
            },
        )
        .expect("recovery input should apply");
    let m2 = store.execution_sidecar_flush_metadata();
    assert_eq!(m2.current_version, 2);
    assert_eq!(m2.flushed_version, 1);
    assert!(m2.next_flush_hint.is_some());

    store
        .apply_resume_execution_target(
            &session_id,
            &TaskExecutionTarget {
                mission_id: MissionId::new("mission-sched"),
                root_task_id: TaskId::new("task-root-sched"),
                task_id: TaskId::new("todo-sched"),
                requested_worker_id: None,
                recovery_id: Some("recovery-sched".to_string()),
                execution_chain_ref: Some("chain-sched".to_string()),
            },
        )
        .expect("resume execution target should apply");
    let m3 = store.execution_sidecar_flush_metadata();
    assert_eq!(m3.current_version, 3);
    assert_eq!(m3.flushed_version, 1);

    let flushed = store
        .flush_execution_sidecars_with(|state| {
            assert_eq!(state.runtime_sidecars.len(), 1);
            assert_eq!(
                state.runtime_sidecars[0].status,
                SessionExecutionSidecarStatus::Resumed
            );
            Ok::<_, std::io::Error>(())
        })
        .expect("flush should succeed");
    assert!(flushed);
    let m3f = store.execution_sidecar_flush_metadata();
    assert_eq!(m3f.flushed_version, 3);
    assert!(m3f.next_flush_hint.is_none());

    let flushed = store
        .flush_execution_sidecars_with(|_| Ok::<_, std::io::Error>(()))
        .expect("no-op flush should succeed");
    assert!(!flushed);
}

#[test]
fn persisted_parts_restore_after_recovery_and_resume_preserves_all_fields() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-restore");
    let workspace_id = WorkspaceId::new("workspace-restore");
    store
        .create_session(session_id.clone(), "Restore Session")
        .expect("session should be creatable");
    store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            execution_chain_ref: Some("chain-restore".to_string()),
            ..ExecutionOwnership::default()
        },
    );
    store
        .apply_recovery_resume_input(
            session_id.clone(),
            RecoveryResumeInput {
                recovery_id: "recovery-restore".to_string(),
                snapshot_id: "snapshot-restore".to_string(),
                ownership: ExecutionOwnership {
                    session_id: Some(session_id.clone()),
                    workspace_id: Some(workspace_id.clone()),
                    execution_chain_ref: Some("chain-restore".to_string()),
                    ..ExecutionOwnership::default()
                },
                diagnostic_summary: Some("restore diag".to_string()),
                created_at: UtcMillis::now(),
                updated_at: UtcMillis::now(),
            },
        )
        .expect("recovery input should apply");
    store
        .apply_resume_execution_target(
            &session_id,
            &TaskExecutionTarget {
                mission_id: MissionId::new("mission-restore"),
                root_task_id: TaskId::new("task-root-restore"),
                task_id: TaskId::new("todo-restore"),
                requested_worker_id: Some(WorkerId::new("worker-restore")),
                recovery_id: Some("recovery-restore".to_string()),
                execution_chain_ref: Some("chain-restore".to_string()),
            },
        )
        .expect("resume execution target should apply");

    let durable_state = store.durable_state();
    let sidecar_store = store.execution_sidecar_store_state();
    let restored = SessionStore::from_persisted_parts(durable_state, sidecar_store);

    let sidecar = restored
        .runtime_sidecar(&session_id)
        .expect("restored sidecar should exist");
    assert_eq!(sidecar.status, SessionExecutionSidecarStatus::Resumed);
    assert_eq!(sidecar.recovery_id.as_deref(), Some("recovery-restore"));
    assert_eq!(sidecar.ownership.session_id, Some(session_id.clone()));
    assert_eq!(sidecar.ownership.workspace_id, Some(workspace_id.clone()));
    assert_eq!(
        sidecar.ownership.mission_id,
        Some(MissionId::new("mission-restore"))
    );
    assert_eq!(sidecar.ownership.task_id, Some(TaskId::new("todo-restore")));
    assert_eq!(
        sidecar.ownership.worker_id,
        Some(WorkerId::new("worker-restore"))
    );
    assert_eq!(
        sidecar.ownership.execution_chain_ref.as_deref(),
        Some("chain-restore")
    );

    let export = restored
        .execution_sidecar_export(&session_id)
        .expect("restored export should exist");
    assert_eq!(
        export.current_status,
        SessionExecutionSidecarStatus::Resumed
    );
    assert_eq!(export.recovery_ref.as_deref(), Some("recovery-restore"));
    assert_eq!(export.execution_chain_ref.as_deref(), Some("chain-restore"));

    let durable = restored.durable_state();
    assert_eq!(durable.sessions.len(), 1);
    assert_eq!(durable.current_session_id, Some(session_id.clone()));

    let metadata = restored.execution_sidecar_flush_metadata();
    assert_eq!(metadata.current_version, 0);
    assert_eq!(metadata.flushed_version, 0);
}

#[test]
fn delete_session_cleans_up_sidecar_and_marks_dirty() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-delete-sidecar");
    store
        .create_session(session_id.clone(), "Delete Sidecar")
        .expect("session should be creatable");
    store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            execution_chain_ref: Some("chain-del".to_string()),
            ..ExecutionOwnership::default()
        },
    );
    store
        .attach_recovery_id(&session_id, Some("recovery-del".to_string()))
        .expect("attach recovery should succeed");

    store
        .flush_execution_sidecars_with(|_| Ok::<_, std::io::Error>(()))
        .expect("flush should succeed");
    let metadata_pre = store.execution_sidecar_flush_metadata();
    assert_eq!(metadata_pre.current_version, metadata_pre.flushed_version);

    store
        .delete_session(&session_id)
        .expect("delete should succeed");
    assert!(store.runtime_sidecar(&session_id).is_none());
    let metadata_post = store.execution_sidecar_flush_metadata();
    assert!(metadata_post.current_version > metadata_post.flushed_version);
    assert_eq!(
        metadata_post.last_dirty_reason,
        Some(SessionSidecarFlushReason::DeleteSession)
    );
}

#[test]
fn delete_session_removes_canonical_turns_and_execution_threads() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-delete-runtime-history");
    let mission_id = MissionId::new("mission-delete-runtime-history");
    store
        .create_session(session_id.clone(), "Delete Runtime History")
        .expect("session should be creatable");
    store.ensure_session_mission(&session_id, UtcMillis(10), || mission_id);
    store
        .upsert_current_turn(
            session_id.clone(),
            test_turn("turn-delete-runtime-history", "completed", 11),
        )
        .expect("canonical turn should persist");

    assert_eq!(store.thread_registry_snapshot(&session_id).len(), 1);
    assert_eq!(store.canonical_turns_for_session(&session_id).len(), 1);

    store
        .delete_session(&session_id)
        .expect("delete should succeed");

    assert!(store.thread_registry_snapshot(&session_id).is_empty());
    assert!(store.canonical_turns_for_session(&session_id).is_empty());
    assert!(
        store
            .durable_state()
            .canonical_turns
            .iter()
            .all(|turn| turn.session_id != session_id)
    );
}

#[test]
fn execution_task_ids_are_recoverable_from_durable_canonical_turns() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-durable-task-ids");
    let task_id = TaskId::new("task-durable-task-ids");
    store
        .create_session(session_id.clone(), "Durable Task Ids")
        .expect("session should be creatable");
    let mut turn = test_turn("turn-durable-task-ids", "completed", 20);
    let mut item = test_turn_item("item-durable-task-ids", "task item");
    item.task_id = Some(task_id.clone());
    turn.items.push(item);
    store
        .upsert_current_turn(session_id.clone(), turn)
        .expect("canonical turn should persist");

    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        SessionExecutionSidecarStoreState::default(),
    );

    assert_eq!(
        restored.execution_task_ids_for_session(&session_id),
        vec![task_id]
    );
    assert!(restored.thread_registry_snapshot(&session_id).is_empty());
}

#[test]
fn execution_sidecar_flush_hook_only_persists_dirty_sidecars() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-flush");
    store
        .create_session(session_id.clone(), "flush session")
        .expect("session should be creatable");

    let mut flushes = Vec::new();
    assert!(
        !store
            .flush_execution_sidecars_with(|state| {
                flushes.push(state.runtime_sidecars.len());
                Ok::<_, std::io::Error>(())
            })
            .expect("empty sidecar flush should succeed")
    );
    assert!(flushes.is_empty());

    store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(WorkspaceId::new("workspace-flush")),
            execution_chain_ref: Some("chain-flush".to_string()),
            ..ExecutionOwnership::default()
        },
    );
    assert!(
        store
            .flush_execution_sidecars_with(|state| {
                flushes.push(state.runtime_sidecars.len());
                Ok::<_, std::io::Error>(())
            })
            .expect("dirty sidecar flush should succeed")
    );
    assert_eq!(flushes, vec![1]);
    assert!(
        !store
            .flush_execution_sidecars_with(|_| Ok::<_, std::io::Error>(()))
            .expect("clean sidecar flush should be skipped")
    );
}
// --- P6a Thread registry tests

fn sample_thread(
    thread_id: &str,
    session_id: &SessionId,
    mission_id: &MissionId,
    role: &str,
    now: UtcMillis,
    status: ExecutionThreadStatus,
) -> ExecutionThread {
    ExecutionThread {
        thread_id: ThreadId::new(thread_id),
        session_id: session_id.clone(),
        mission_id: mission_id.clone(),
        role_id: role.to_string(),
        worker_instance_id: WorkerId::new(format!("worker-{role}")),
        status,
        created_at: now,
        last_used_at: now,
        observed_context_window_tokens: None,
        handled_task_ids: Vec::new(),
        message_history: Vec::new(),
    }
}

#[test]
fn thread_registry_round_trip_preserves_task_binding_and_message_history() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-thread-round-trip");
    let mission_id = MissionId::new("mission-thread-round-trip");
    let thread_id = ThreadId::new("thread-worker-round-trip");
    let task_id = TaskId::new("task-thread-round-trip");
    store
        .create_session_for_workspace(
            session_id.clone(),
            "Thread Round Trip",
            Some("workspace-round-trip".to_string()),
        )
        .expect("session should be creatable");

    let mut thread = sample_thread(
        thread_id.as_str(),
        &session_id,
        &mission_id,
        "executor",
        UtcMillis(1_000),
        ExecutionThreadStatus::Active,
    );
    thread.handled_task_ids.push(task_id.clone());
    store.register_thread(thread);
    store.record_thread_context_window_tokens(&thread_id, 128_000, UtcMillis(1_100));
    store.append_thread_messages(
        &thread_id,
        vec![
            ThreadChatMessage {
                role: "assistant".to_string(),
                content: None,
                images: Vec::new(),
                tool_calls: vec![ThreadChatToolCall {
                    id: "call-shell-round-trip".to_string(),
                    kind: "function".to_string(),
                    function: ThreadChatToolFunction {
                        name: "shell_exec".to_string(),
                        arguments: r#"{"command":"cargo test"}"#.to_string(),
                    },
                }],
                tool_call_id: None,
                provider_context: vec![ThreadModelProviderContext {
                    provider: "anthropic".to_string(),
                    kind: "thinking".to_string(),
                    data: json!({
                        "type": "thinking",
                        "thinking": "检查测试命令",
                        "signature": "persisted-signature"
                    }),
                }],
            },
            ThreadChatMessage {
                role: "tool".to_string(),
                content: Some("exit code 1: compilation failed".to_string()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: Some("call-shell-round-trip".to_string()),
                provider_context: Vec::new(),
            },
            ThreadChatMessage {
                role: "user".to_string(),
                content: Some("继续".to_string()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: Vec::new(),
            },
        ],
        UtcMillis(2_000),
    );
    store.install_thread_context_checkpoint(
        &thread_id,
        ThreadContextCheckpoint {
            thread_id: thread_id.clone(),
            checkpoint_id: "checkpoint-round-trip".to_string(),
            source_message_count: 2,
            summary_message: ThreadChatMessage {
                role: "system".to_string(),
                content: Some("已执行测试，编译失败。".to_string()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: Vec::new(),
            },
            reason: "context_window_pressure".to_string(),
            original_token_estimate: 12_000,
            checkpoint_token_estimate: 400,
            created_at: UtcMillis(2_100),
            generation: 1,
            source_fingerprint: String::new(),
            model_provider: None,
            model: None,
            binding_revision: None,
            projected_request_tokens: 0,
            context_window_limit_tokens: None,
            preserved_tail_message_count: 0,
            file_fact_versions: vec![ThreadFileFactVersion {
                path: "/tmp/example.rs".to_string(),
                content_hash: "hash-example".to_string(),
            }],
        },
        UtcMillis(2_100),
    );

    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        SessionExecutionSidecarStoreState::default(),
    );
    let restored_threads = restored.thread_registry_snapshot(&session_id);
    assert_eq!(restored_threads.len(), 1);
    assert_eq!(restored_threads[0].thread_id, thread_id);
    assert_eq!(restored_threads[0].mission_id, mission_id);
    assert_eq!(restored_threads[0].handled_task_ids, vec![task_id]);
    assert_eq!(restored_threads[0].status, ExecutionThreadStatus::Active);
    assert_eq!(
        restored_threads[0].observed_context_window_tokens,
        Some(128_000)
    );
    assert_eq!(
        restored.thread_context_window_tokens(&restored_threads[0].thread_id),
        Some(128_000)
    );

    let history = restored.thread_message_history(&restored_threads[0].thread_id);
    assert_eq!(history.len(), 3);
    assert_eq!(history[0].role, "assistant");
    assert_eq!(history[0].tool_calls[0].function.name, "shell_exec");
    assert_eq!(
        history[0].provider_context[0].data["signature"],
        "persisted-signature"
    );
    assert_eq!(
        history[1].content.as_deref(),
        Some("exit code 1: compilation failed")
    );
    assert_eq!(history[2].content.as_deref(), Some("继续"));
    let context_history = restored.thread_context_history(&restored_threads[0].thread_id);
    assert_eq!(context_history.len(), 2);
    assert_eq!(context_history[0].role, "system");
    assert_eq!(context_history[1].content.as_deref(), Some("继续"));
    let checkpoint = restored
        .thread_context_checkpoint(&restored_threads[0].thread_id)
        .expect("checkpoint should survive persistence");
    assert_eq!(checkpoint.source_message_count, 2);
    assert_eq!(checkpoint.file_fact_versions.len(), 1);

    restored.replace_thread_messages(&restored_threads[0].thread_id, history, UtcMillis(3_000));
    assert!(
        restored
            .thread_context_checkpoint(&restored_threads[0].thread_id)
            .is_none()
    );
}

#[test]
fn thread_registry_partition_follows_session_workspace_ownership() {
    let store = SessionStore::new();
    let global_session_id = SessionId::new("session-thread-global");
    let workspace_a_session_id = SessionId::new("session-thread-workspace-a");
    let workspace_b_session_id = SessionId::new("session-thread-workspace-b");
    let mission_id = MissionId::new("mission-thread-partition");
    store
        .create_session(global_session_id.clone(), "Global Session")
        .expect("global session should be creatable");
    store
        .create_session_for_workspace(
            workspace_a_session_id.clone(),
            "Workspace A Session",
            Some("workspace-a".to_string()),
        )
        .expect("workspace A session should be creatable");
    store
        .create_session_for_workspace(
            workspace_b_session_id.clone(),
            "Workspace B Session",
            Some("workspace-b".to_string()),
        )
        .expect("workspace B session should be creatable");

    for (thread_id, session_id) in [
        ("thread-global", &global_session_id),
        ("thread-workspace-a", &workspace_a_session_id),
        ("thread-workspace-b", &workspace_b_session_id),
    ] {
        store.register_thread(sample_thread(
            thread_id,
            session_id,
            &mission_id,
            "executor",
            UtcMillis(1_000),
            ExecutionThreadStatus::Idle,
        ));
    }

    let (global_state, workspace_states) = store.durable_state().partition_by_workspace();
    assert_eq!(global_state.thread_registry.len(), 1);
    assert_eq!(
        global_state.thread_registry[0].session_id,
        global_session_id
    );
    let workspace_a_state = workspace_states
        .get("workspace-a")
        .expect("workspace A durable state should exist");
    assert_eq!(workspace_a_state.thread_registry.len(), 1);
    assert_eq!(
        workspace_a_state.thread_registry[0].session_id,
        workspace_a_session_id
    );
    let workspace_b_state = workspace_states
        .get("workspace-b")
        .expect("workspace B durable state should exist");
    assert_eq!(workspace_b_state.thread_registry.len(), 1);
    assert_eq!(
        workspace_b_state.thread_registry[0].session_id,
        workspace_b_session_id
    );
}

#[test]
fn thread_registry_activation_tracks_task_ids_without_reuse_lookup() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-thread-activation");
    let mission_id = MissionId::new("mission-thread-activation");
    let now = UtcMillis(1_000);
    let thread_id = ThreadId::new("thread-backend-1");

    store.register_thread(sample_thread(
        thread_id.as_str(),
        &session_id,
        &mission_id,
        "executor",
        now,
        ExecutionThreadStatus::Idle,
    ));

    let task_a = TaskId::new("task-a");
    store.activate_thread(&thread_id, &task_a, UtcMillis(2_000));
    assert_eq!(store.mark_task_threads_idle(&task_a, UtcMillis(3_000)), 1);

    let snapshot = store.thread_registry_snapshot(&session_id);
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].status, ExecutionThreadStatus::Idle);
    assert_eq!(
        snapshot[0].handled_task_ids,
        vec![task_a],
        "activate_thread 必须累积 task_id"
    );
}

#[test]
fn thread_registry_marks_only_active_threads_for_terminal_task_idle() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-task-terminal");
    let mission_id = MissionId::new("mission-task-terminal");
    let target_task_id = TaskId::new("task-terminal-target");
    let other_task_id = TaskId::new("task-terminal-other");

    for (thread_id, task_id, status) in [
        (
            "thread-terminal-target",
            &target_task_id,
            ExecutionThreadStatus::Active,
        ),
        (
            "thread-terminal-other",
            &other_task_id,
            ExecutionThreadStatus::Active,
        ),
        (
            "thread-terminal-retired",
            &target_task_id,
            ExecutionThreadStatus::Retired,
        ),
    ] {
        let thread_id = ThreadId::new(thread_id);
        let mut thread = sample_thread(
            thread_id.as_str(),
            &session_id,
            &mission_id,
            "reviewer",
            UtcMillis(1_000),
            status,
        );
        thread.handled_task_ids.push(task_id.clone());
        store.register_thread(thread);
    }

    assert_eq!(
        store.mark_task_threads_idle(&target_task_id, UtcMillis(3_000)),
        1
    );

    let snapshot = store.thread_registry_snapshot(&session_id);
    let status = |thread_id: &str| {
        snapshot
            .iter()
            .find(|thread| thread.thread_id.as_str() == thread_id)
            .map(|thread| thread.status)
            .expect("thread should exist")
    };
    assert_eq!(
        status("thread-terminal-target"),
        ExecutionThreadStatus::Idle
    );
    assert_eq!(
        status("thread-terminal-other"),
        ExecutionThreadStatus::Active
    );
    assert_eq!(
        status("thread-terminal-retired"),
        ExecutionThreadStatus::Retired
    );
}

#[test]
fn thread_registry_snapshot_is_scoped_per_session() {
    let store = SessionStore::new();
    let session_a = SessionId::new("session-a");
    let session_b = SessionId::new("session-b");
    let mission_id = MissionId::new("mission-iso");
    let now = UtcMillis(1_000);

    store.register_thread(sample_thread(
        "thread-a-backend",
        &session_a,
        &mission_id,
        "executor",
        now,
        ExecutionThreadStatus::Idle,
    ));
    store.register_thread(sample_thread(
        "thread-b-backend",
        &session_b,
        &mission_id,
        "executor",
        now,
        ExecutionThreadStatus::Idle,
    ));

    let a_threads = store.thread_registry_snapshot(&session_a);
    assert_eq!(a_threads.len(), 1);
    assert_eq!(a_threads[0].thread_id.as_str(), "thread-a-backend");

    let b_threads = store.thread_registry_snapshot(&session_b);
    assert_eq!(b_threads.len(), 1);
    assert_eq!(b_threads[0].thread_id.as_str(), "thread-b-backend");
}

#[test]
fn thread_registry_retires_on_session_retirement() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-retire");
    let mission_id = MissionId::new("mission-retire");
    let now = UtcMillis(1_000);

    store.register_thread(sample_thread(
        "thread-r-1",
        &session_id,
        &mission_id,
        "reviewer",
        now,
        ExecutionThreadStatus::Idle,
    ));
    store.register_thread(sample_thread(
        "thread-r-2",
        &session_id,
        &mission_id,
        "architect",
        now,
        ExecutionThreadStatus::Active,
    ));

    store.retire_session_threads(&session_id, UtcMillis(2_000));

    let snapshot = store.thread_registry_snapshot(&session_id);
    assert_eq!(snapshot.len(), 2);
    for thread in &snapshot {
        assert_eq!(
            thread.status,
            ExecutionThreadStatus::Retired,
            "retire_session_threads 必须把 session 下所有 thread 标记为 Retired"
        );
    }

    assert!(
        snapshot
            .iter()
            .all(|thread| thread.status == ExecutionThreadStatus::Retired)
    );
}
