use super::*;
use crate::models::{
    ActiveExecutionBranch, ActiveExecutionBranchSnapshotUpdate, ActiveExecutionChain,
    ActiveExecutionDispatchContext, ActiveExecutionTurn, ActiveExecutionTurnItem,
    CanonicalTurnStatus, ExecutionThread, ExecutionThreadStatus, GoalContinuationPhase,
    GoalContinuationState, GoalRevisionExpectation, GoalStatus, NotificationContext,
    NotificationRecord, NotificationScope, SessionAcceptanceRecord, SessionDurableState,
    SessionExecutionSidecarStatus, SessionExecutionSidecarStoreState, SessionPlan,
    SessionSidecarFlushReason, SessionStoreState, ThreadChatMessage, ThreadChatToolCall,
    ThreadChatToolFunction, ThreadContextCheckpoint, ThreadFileFactVersion,
    ThreadModelProviderContext,
};
use magi_core::{
    AccessProfile, DomainError, ExecutionOwnership, MissionId, PlanId, PlanItem, PlanItemId,
    PlanItemStatus, PlanState, RecoveryResumeInput, SessionId, TaskExecutionTarget, TaskId,
    ThreadId, UtcMillis, WorkerId, WorkspaceId,
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, thread, time::Duration};

struct RejectingCanonicalWriter;

#[derive(Default)]
struct RecordingCanonicalWriter {
    transactions: std::sync::Mutex<Vec<Vec<crate::CanonicalTurnMutation>>>,
}

impl crate::SessionEventWriter for RecordingCanonicalWriter {
    fn append_session_transaction(
        &self,
        _: &SessionId,
        turns: &[crate::CanonicalTurnMutation],
        _: &[crate::SessionInputMutation],
        _: Option<(&SessionAcceptanceRecord, Option<&magi_core::Task>)>,
    ) -> magi_core::DomainResult<()> {
        self.transactions.lock().unwrap().push(turns.to_vec());
        Ok(())
    }
}

impl crate::SessionEventWriter for RejectingCanonicalWriter {
    fn append_session_transaction(
        &self,
        _: &SessionId,
        _: &[crate::CanonicalTurnMutation],
        _: &[crate::SessionInputMutation],
        _: Option<(&SessionAcceptanceRecord, Option<&magi_core::Task>)>,
    ) -> magi_core::DomainResult<()> {
        Err(DomainError::Persistence {
            message: "test canonical writer failure".into(),
        })
    }
}

struct BlockingCanonicalWriter {
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl crate::SessionEventWriter for BlockingCanonicalWriter {
    fn append_session_transaction(
        &self,
        _: &SessionId,
        _: &[crate::CanonicalTurnMutation],
        _: &[crate::SessionInputMutation],
        _: Option<(&SessionAcceptanceRecord, Option<&magi_core::Task>)>,
    ) -> magi_core::DomainResult<()> {
        self.entered
            .send(())
            .expect("blocking writer should signal entry");
        self.release
            .lock()
            .expect("blocking writer release lock should not be poisoned")
            .recv()
            .expect("blocking writer should be released");
        Ok(())
    }
}

type StoreMutationSnapshot = (
    serde_json::Value,
    serde_json::Value,
    crate::models::SessionSidecarFlushMetadata,
);

fn store_mutation_snapshot(store: &SessionStore) -> StoreMutationSnapshot {
    (
        serde_json::to_value(store.durable_state()).expect("durable state should serialize"),
        serde_json::to_value(store.execution_sidecar_store_state())
            .expect("sidecar state should serialize"),
        store.execution_sidecar_flush_metadata(),
    )
}

fn assert_store_mutation_snapshot_unchanged(store: &SessionStore, before: &StoreMutationSnapshot) {
    assert_eq!(
        serde_json::to_value(store.durable_state()).expect("durable state should serialize"),
        before.0,
        "canonical writer 失败后 durable projection 不能变化"
    );
    assert_eq!(
        serde_json::to_value(store.execution_sidecar_store_state())
            .expect("sidecar state should serialize"),
        before.1,
        "canonical writer 失败后 sidecar projection 不能变化"
    );
    assert_eq!(
        store.execution_sidecar_flush_metadata(),
        before.2,
        "canonical writer 失败后 flush 状态不能推进"
    );
}

fn reject_canonical_writes(store: &SessionStore) {
    store.install_session_event_writer(Arc::new(RejectingCanonicalWriter));
}

fn create_test_goal(
    store: &SessionStore,
    session_id: &SessionId,
    turn_id: &str,
    objective: &str,
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

fn accept_test_turn(store: &SessionStore, session_id: &SessionId, turn: ActiveExecutionTurn) {
    let turn_id = turn.turn_id.clone();
    let message = turn
        .user_message
        .clone()
        .unwrap_or_else(|| format!("message for {turn_id}"));
    store
        .accept_current_turn_with_timeline_entry(
            session_id.clone(),
            TimelineEntryInput::new(
                format!("timeline-test-{turn_id}-{}", turn.accepted_at.0),
                TimelineEntryKind::UserMessage,
                message,
                turn.accepted_at,
            ),
            turn,
        )
        .expect("test turn should be accepted through the canonical boundary");
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
fn active_turn_request_id_prefers_root_user_item() {
    let mut turn = test_turn("turn-request-identity", "running", 10);
    let mut assistant_item = test_turn_item("assistant-request", "assistant");
    assistant_item.kind = "assistant_stream".to_string();
    assistant_item.item_seq = 2;
    assistant_item.request_id = Some("assistant-request-id".to_string());
    let mut user_item = test_turn_item("user-request", "user");
    user_item.item_seq = 1;
    user_item
        .metadata
        .insert("requestId".to_string(), json!(" user-request-id "));
    turn.items = vec![assistant_item, user_item];

    assert_eq!(
        crate::models::active_execution_turn_request_id(&turn).as_deref(),
        Some("user-request-id")
    );

    turn.items.retain(|item| item.kind != "user_message");
    assert_eq!(
        crate::models::active_execution_turn_request_id(&turn).as_deref(),
        Some("assistant-request-id")
    );
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
    error_item.item_seq = 1;
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
fn external_tool_session_does_not_take_over_current_session_and_round_trips_kind() {
    let store = SessionStore::new();
    let user_id = SessionId::new("session-user");
    store
        .create_session(user_id.clone(), "User")
        .expect("user session should create");

    let external = store
        .create_external_tool_session(
            SessionId::new("session-external"),
            "cursor · demo",
            "workspace-1".to_string(),
        )
        .expect("external session should create");

    assert_eq!(external.kind, crate::SessionKind::ExternalTool);
    assert_eq!(
        store.current_session().expect("current session").session_id,
        user_id,
        "外部工具会话不得抢占用户当前会话"
    );
    assert!(
        store
            .create_external_tool_session(
                SessionId::new("session-external"),
                "dup",
                "workspace-1".to_string(),
            )
            .is_err()
    );

    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        SessionExecutionSidecarStoreState::default(),
    )
    .expect("持久化恢复应成功");
    let kinds = restored
        .sessions()
        .into_iter()
        .map(|session| (session.session_id.to_string(), session.kind))
        .collect::<HashMap<_, _>>();
    assert_eq!(kinds["session-external"], crate::SessionKind::ExternalTool);
    assert_eq!(kinds["session-user"], crate::SessionKind::User);
    let user_json = serde_json::to_value(
        restored
            .sessions()
            .into_iter()
            .find(|session| session.session_id == user_id)
            .unwrap(),
    )
    .unwrap();
    assert!(
        user_json.get("kind").is_none(),
        "用户会话不写 kind，旧数据格式不变"
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
    )
    .expect("当前会话选择状态的持久化恢复应成功");
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
    )
    .expect("当前会话清空状态的持久化恢复应成功");
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
fn rename_session_transaction_keeps_title_and_timeline_when_persistence_fails() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-rename-transaction");
    store
        .create_session(session_id.clone(), "旧标题")
        .expect("session should create");
    let before = serde_json::to_value(store.durable_state()).expect("state should serialize");

    let result =
        store.rename_session_with_persistence(&session_id, "新标题", |_durable, _sidecars| {
            Err::<(), _>("projection write failed")
        });

    assert!(matches!(
        result,
        Err(SessionMutationTransactionError::Persistence(
            "projection write failed"
        ))
    ));
    assert_eq!(
        serde_json::to_value(store.durable_state()).expect("state should serialize"),
        before,
        "projection 失败不得提交标题或重命名事件"
    );
    assert_eq!(
        store
            .session(&session_id)
            .expect("session should exist")
            .title,
        "旧标题"
    );
    assert_eq!(
        store
            .timeline()
            .iter()
            .filter(|entry| matches!(&entry.kind, TimelineEntryKind::SessionRenamed))
            .count(),
        0,
        "projection 失败不得新增重命名事件"
    );
}

#[test]
fn session_projection_persistence_serializes_snapshot_and_write_transactions() {
    let store = SessionStore::new();
    let first_session_id = SessionId::new("session-persist-order-first");
    let second_session_id = SessionId::new("session-persist-order-second");
    store
        .create_session(first_session_id.clone(), "First")
        .expect("first session should create");
    store
        .create_session(second_session_id.clone(), "Second")
        .expect("second session should create");
    store.bind_execution_ownership(
        second_session_id.clone(),
        ExecutionOwnership {
            session_id: Some(second_session_id.clone()),
            workspace_id: Some(WorkspaceId::new("workspace-persist-order")),
            ..ExecutionOwnership::default()
        },
    );

    let snapshots = Arc::new(Mutex::new(Vec::<SessionId>::new()));
    let (first_entered_tx, first_entered_rx) = std::sync::mpsc::channel();
    let (release_first_tx, release_first_rx) = std::sync::mpsc::channel();
    let first_store = store.clone();
    let first_snapshots = Arc::clone(&snapshots);
    let first_persist = thread::spawn(move || {
        first_store
            .persist_projection_with(|state, sidecars| {
                assert_eq!(
                    state.sessions[1].workspace_id.as_deref(),
                    sidecars.runtime_sidecars[0]
                        .ownership
                        .workspace_id
                        .as_ref()
                        .map(WorkspaceId::as_str),
                    "durable 与 sidecar 必须来自同一状态快照"
                );
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
                            .clone()
                            .expect("first snapshot should have current session"),
                    );
                Ok::<(), ()>(())
            })
            .expect("first persistence should complete");
    });
    first_entered_rx
        .recv()
        .expect("first persistence should enter callback");

    let selection_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let selection_store = store.clone();
    let selection_session_id = first_session_id.clone();
    let selection_finished_flag = Arc::clone(&selection_finished);
    let (selection_started_tx, selection_started_rx) = std::sync::mpsc::channel();
    let selection = thread::spawn(move || {
        selection_started_tx
            .send(())
            .expect("selection should signal start");
        selection_store
            .select_current_session(&selection_session_id)
            .expect("first session should become current");
        selection_finished_flag.store(true, std::sync::atomic::Ordering::Release);
    });
    selection_started_rx.recv().expect("selection should start");
    thread::sleep(Duration::from_millis(20));
    assert!(
        !selection_finished.load(std::sync::atomic::Ordering::Acquire),
        "session mutation must wait for the projection callback"
    );
    release_first_tx
        .send(())
        .expect("first persistence should be releasable");
    first_persist.join().expect("first persistence should join");
    selection.join().expect("session selection should join");

    store
        .persist_projection_with(|state, sidecars| {
            assert_eq!(
                state.sessions[1].workspace_id.as_deref(),
                sidecars.runtime_sidecars[0]
                    .ownership
                    .workspace_id
                    .as_ref()
                    .map(WorkspaceId::as_str),
                "durable 与 sidecar 必须来自同一状态快照"
            );
            snapshots
                .lock()
                .expect("snapshot order lock should not be poisoned")
                .push(
                    state
                        .current_session_id
                        .clone()
                        .expect("second snapshot should have current session"),
                );
            Ok::<(), ()>(())
        })
        .expect("second persistence should complete");

    assert_eq!(
        *snapshots
            .lock()
            .expect("snapshot order lock should not be poisoned"),
        vec![second_session_id, first_session_id]
    );
}

#[test]
fn navigation_persistence_hands_over_only_the_target_session_slice() {
    let store = SessionStore::new();
    let target = SessionId::new("session-navigation-target");
    let other = SessionId::new("session-navigation-other");
    store
        .create_session(target.clone(), "Navigation target")
        .expect("target session should create");
    store
        .create_session(other.clone(), "Navigation other")
        .expect("other session should create");
    store.append_timeline_entry(
        target.clone(),
        TimelineEntryKind::UserMessage,
        "目标会话的事实",
    );
    store.append_timeline_entry(
        other.clone(),
        TimelineEntryKind::UserMessage,
        "其它会话的事实，导航不应该拿到",
    );
    store
        .select_current_session(&target)
        .expect("target should be selectable");

    // 只交出当前指针与目标会话的切片，不克隆其它会话的任何数据。
    let (current, session_ids, timeline_sessions) = store
        .persist_navigation_projection_with(Some(&target), |durable, sidecars| {
            assert!(
                sidecars
                    .runtime_sidecars
                    .iter()
                    .all(|s| s.session_id == target)
            );
            Ok::<_, ()>((
                durable.current_session_id.clone(),
                durable
                    .sessions
                    .iter()
                    .map(|session| session.session_id.clone())
                    .collect::<Vec<_>>(),
                durable
                    .timeline
                    .iter()
                    .map(|entry| entry.session_id.clone())
                    .collect::<Vec<_>>(),
            ))
        })
        .expect("navigation persistence should run");
    assert_eq!(current, Some(target.clone()));
    assert_eq!(session_ids, vec![target.clone()]);
    assert!(!timeline_sessions.is_empty());
    assert!(timeline_sessions.iter().all(|id| id == &target));

    // 草稿导航：没有目标会话，只带当前指针，其余为空。
    store.clear_current_session();
    let (current, sessions_len, timeline_len) = store
        .persist_navigation_projection_with(None, |durable, sidecars| {
            assert!(sidecars.runtime_sidecars.is_empty());
            Ok::<_, ()>((
                durable.current_session_id.clone(),
                durable.sessions.len(),
                durable.timeline.len(),
            ))
        })
        .expect("draft navigation persistence should run");
    assert_eq!(current, None);
    assert_eq!(sessions_len, 0);
    assert_eq!(timeline_len, 0);
}

#[test]
fn persistence_callback_holds_projection_snapshot_until_write_finishes() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-persistence-recapture");
    store
        .create_session(session_id.clone(), "Persistence recapture")
        .expect("session should create");
    let (callback_entered_tx, callback_entered_rx) = std::sync::mpsc::channel();
    let (release_callback_tx, release_callback_rx) = std::sync::mpsc::channel();
    let persist_store = store.clone();
    let persist = thread::spawn(move || {
        persist_store
            .persist_projection_with(|durable, _sidecars| {
                callback_entered_tx
                    .send(())
                    .expect("projection callback should signal entry");
                release_callback_rx
                    .recv()
                    .expect("projection callback should be released");
                Ok::<usize, ()>(durable.timeline.len())
            })
            .expect("projection should persist");
    });
    callback_entered_rx
        .recv()
        .expect("projection callback should enter");

    let mutation_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mutation_finished_flag = Arc::clone(&mutation_finished);
    let mutation_store = store.clone();
    let mutation_session_id = session_id.clone();
    let mutation = thread::spawn(move || {
        mutation_store.append_timeline_entry(
            mutation_session_id,
            TimelineEntryKind::UserMessage,
            "发生在持久化回调期间的事实",
        );
        mutation_finished_flag.store(true, std::sync::atomic::Ordering::Release);
    });
    thread::sleep(Duration::from_millis(20));
    assert!(
        !mutation_finished.load(std::sync::atomic::Ordering::Acquire),
        "timeline mutation must wait until the projection write finishes"
    );

    release_callback_tx
        .send(())
        .expect("projection callback should be released");
    persist.join().expect("projection persistence should join");
    mutation.join().expect("timeline mutation should join");

    assert_eq!(store.timeline().len(), 2);
}

#[test]
fn persistence_waits_for_canonical_commit_before_capturing_projection() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-persistence-canonical-order");
    let turn_id = "turn-persistence-canonical-order";
    store
        .create_session(session_id.clone(), "Persistence canonical order")
        .expect("session should create");
    accept_test_turn(&store, &session_id, test_turn(turn_id, "running", 10));

    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    store.install_session_event_writer(Arc::new(BlockingCanonicalWriter {
        entered: entered_tx,
        release: std::sync::Mutex::new(release_rx),
    }));

    let mutation_store = store.clone();
    let mutation_session_id = session_id.clone();
    let mutation = thread::spawn(move || {
        mutation_store
            .set_current_turn_status_for_test(&mutation_session_id, Some(turn_id), "completed")
            .expect("canonical mutation should complete");
    });
    entered_rx
        .recv()
        .expect("canonical writer should be entered");

    let (persist_entered_tx, persist_entered_rx) = std::sync::mpsc::channel();
    let persist_store = store.clone();
    let persist = thread::spawn(move || {
        persist_store
            .persist_projection_with(|_, _| {
                persist_entered_tx
                    .send(())
                    .expect("projection callback should signal entry");
                Ok::<(), ()>(())
            })
            .expect("projection should persist");
    });
    assert!(
        persist_entered_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "projection callback must wait for the canonical event commit"
    );

    release_tx
        .send(())
        .expect("canonical writer should be released");
    mutation.join().expect("canonical mutation should join");
    persist.join().expect("projection persistence should join");
    persist_entered_rx
        .try_recv()
        .expect("projection callback should run after canonical commit");
}

#[test]
fn sidecar_flush_waits_for_canonical_commit_before_capturing_projection() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-sidecar-canonical-order");
    let turn_id = "turn-sidecar-canonical-order";
    store
        .create_session(session_id.clone(), "Sidecar canonical order")
        .expect("session should create");
    accept_test_turn(&store, &session_id, test_turn(turn_id, "running", 10));
    store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(WorkspaceId::new("workspace-sidecar-canonical-order")),
            execution_chain_ref: Some("chain-sidecar-canonical-order".to_string()),
            ..ExecutionOwnership::default()
        },
    );

    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    store.install_session_event_writer(Arc::new(BlockingCanonicalWriter {
        entered: entered_tx,
        release: std::sync::Mutex::new(release_rx),
    }));

    let mutation_store = store.clone();
    let mutation_session_id = session_id.clone();
    let mutation = thread::spawn(move || {
        mutation_store
            .set_current_turn_status_for_test(&mutation_session_id, Some(turn_id), "completed")
            .expect("canonical mutation should complete");
    });
    entered_rx
        .recv()
        .expect("canonical writer should be entered");

    let (flush_entered_tx, flush_entered_rx) = std::sync::mpsc::channel();
    let flush_store = store.clone();
    let flush = thread::spawn(move || {
        flush_store
            .flush_execution_sidecar_snapshot_with(|durable, _, ids| {
                let turn = durable
                    .canonical_turns
                    .iter()
                    .find(|turn| turn.turn_id == turn_id)
                    .expect("canonical turn should exist in flush snapshot");
                flush_entered_tx
                    .send(turn.status)
                    .expect("sidecar flush callback should signal entry");
                Ok::<_, ()>(ids.to_vec())
            })
            .expect("sidecar flush should succeed")
    });
    assert!(
        flush_entered_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "sidecar flush callback must wait for the canonical event commit"
    );

    release_tx
        .send(())
        .expect("canonical writer should be released");
    mutation.join().expect("canonical mutation should join");
    assert!(flush.join().expect("sidecar flush should join"));
    assert_eq!(
        flush_entered_rx
            .try_recv()
            .expect("sidecar flush should capture post-commit state"),
        CanonicalTurnStatus::Completed
    );
}

#[test]
fn persistence_returns_stable_callback_error_without_retrying() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-persistence-stable-error");
    store
        .create_session(session_id, "Persistence stable error")
        .expect("session should create");
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let callback_attempts = Arc::clone(&attempts);

    let error = store
        .persist_projection_with(move |_durable, _sidecars| {
            callback_attempts.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            Err::<(), _>("stable persistence failure")
        })
        .expect_err("stable persistence failure should be returned");

    assert_eq!(error, "stable persistence failure");
    assert_eq!(
        attempts.load(std::sync::atomic::Ordering::Acquire),
        1,
        "没有状态变化时不得重复执行持久化回调"
    );
}

#[test]
fn canonical_event_commit_does_not_hold_state_lock_until_event_writer_finishes() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-canonical-commit-atomicity");
    let turn_id = "turn-canonical-commit-atomicity";
    store
        .create_session(session_id.clone(), "Canonical commit atomicity")
        .expect("session should create");
    accept_test_turn(&store, &session_id, test_turn(turn_id, "running", 10));

    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    store.install_session_event_writer(Arc::new(BlockingCanonicalWriter {
        entered: entered_tx,
        release: std::sync::Mutex::new(release_rx),
    }));

    let mutation_store = store.clone();
    let mutation_session_id = session_id.clone();
    let mutation = thread::spawn(move || {
        mutation_store
            .set_current_turn_status_for_test(&mutation_session_id, Some(turn_id), "completed")
            .expect("canonical mutation should complete");
    });
    entered_rx
        .recv()
        .expect("canonical writer should be entered");

    let (read_tx, read_rx) = std::sync::mpsc::channel();
    let read_store = store.clone();
    let read_session_id = session_id.clone();
    let read = thread::spawn(move || {
        let status = read_store
            .canonical_turn_for_session_turn_id(&read_session_id, turn_id)
            .expect("canonical turn should remain readable during event write")
            .status;
        read_tx
            .send(status)
            .expect("state read should report status");
    });
    assert_eq!(
        read_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("state reads must not wait for canonical fsync"),
        CanonicalTurnStatus::Running
    );

    release_tx
        .send(())
        .expect("canonical writer should be released");
    mutation.join().expect("canonical mutation should join");
    read.join().expect("state read should join");
    assert_eq!(
        store
            .canonical_turn_for_session_turn_id(&session_id, turn_id)
            .expect("canonical turn should be committed after event write")
            .status,
        CanonicalTurnStatus::Completed
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
    );
    assert_eq!(first.status, GoalStatus::Active);

    let err = store
        .create_goal(
            session_id.clone(),
            first.thread_id.clone(),
            "turn-goal-singleton-2",
            "另一个未结束目标",
            AccessProfile::Restricted,
        )
        .expect_err("second unfinished goal must be rejected");
    assert!(matches!(
        err,
        DomainError::GoalRejected {
            reason: magi_core::GoalRejection::AlreadyUnfinished,
            ..
        }
    ));

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
    let older = create_test_goal(&store, &session_id, "turn-goal-latest-restore", "旧目标");
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
        updated_at: UtcMillis(older.updated_at.0),
    }];

    let restored =
        SessionStore::from_persisted_parts(durable, SessionExecutionSidecarStoreState::default())
            .expect("最新目标归一化后的持久化恢复应成功");
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
                updated_at: UtcMillis::now(),
            },
            Some(0),
        )
        .expect("plan should write");

    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        SessionExecutionSidecarStoreState::default(),
    )
    .expect("会话计划的持久化恢复应成功");
    let restored_plan = restored.plan(&session_a).expect("plan should restore");
    assert_eq!(restored_plan.items.len(), 1);
    assert_eq!(restored_plan.items[0].title, "验证持久化");
    assert!(restored.plan(&session_b).is_none());
}

#[test]
fn v2_store_rejects_legacy_todo_list_payload() {
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
    serde_json::from_value::<SessionDurableState>(payload)
        .expect_err("持久化状态只接受当前格式，不得接受旧 todo_lists");
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

    accept_test_turn(
        &store,
        &session_id,
        ActiveExecutionTurn {
            turn_id: "turn-chat".to_string(),
            turn_seq: 1,
            accepted_at: UtcMillis(1),
            status: "completed".to_string(),
            user_message: Some("普通问答".to_string()),
            items: Vec::new(),
            completed_at: None,
        },
    );

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

    accept_test_turn(
        &store,
        &session_id,
        ActiveExecutionTurn {
            turn_id: "turn-chat".to_string(),
            turn_seq: 1,
            accepted_at: UtcMillis(1),
            status: "completed".to_string(),
            user_message: Some("普通问答".to_string()),
            items: Vec::new(),
            completed_at: None,
        },
    );

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
    accept_test_turn(&store, &session_id, test_turn("turn-running", "running", 1));

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
fn finalize_current_turn_for_continue_closes_active_turn_and_chain_atomically() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-finalize-continue");
    store
        .create_session(session_id.clone(), "Finalize Continue")
        .expect("session should be creatable");
    accept_test_turn(
        &store,
        &session_id,
        test_turn("turn-before-continue", "running", 1),
    );

    let mut tool_item = test_turn_item("tool-before-continue", "执行中的工具");
    tool_item.kind = "tool_call_started".to_string();
    tool_item.status = "running".to_string();
    tool_item.tool_call_id = Some("tool-call-before-continue".to_string());
    tool_item.tool_name = Some("shell_exec".to_string());
    tool_item.tool_arguments = Some("{}".to_string());
    store
        .upsert_current_turn_item_for_turn(&session_id, Some("turn-before-continue"), tool_item)
        .expect("active tool item should upsert");
    let mut chain_turn = test_turn("turn-before-continue", "running", 1);
    chain_turn.items.push(
        store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .and_then(|turn| turn.items.into_iter().next())
            .expect("active tool item should be visible in the current turn"),
    );
    store
        .upsert_active_execution_chain(
            session_id.clone(),
            test_active_chain(&session_id, "chain-before-continue", Some(chain_turn)),
        )
        .expect("active chain should upsert");

    let finalized = store
        .finalize_current_turn_for_continue(&session_id, "turn-before-continue")
        .expect("continue should finalize the observed turn")
        .expect("current turn should exist");
    let turn = finalized.current_turn.expect("finalized turn should exist");
    assert_eq!(turn.status, "failed");
    assert!(turn.completed_at.is_some());
    assert_eq!(turn.items[0].status, "failed");
    assert_eq!(
        finalized
            .active_execution_chain
            .and_then(|chain| chain.current_turn)
            .map(|turn| turn.status),
        Some("failed".to_string())
    );
    assert_eq!(
        store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| turn.turn_id == "turn-before-continue")
            .map(|turn| turn.status),
        Some(CanonicalTurnStatus::Failed)
    );

    let repeated = store
        .finalize_current_turn_for_continue(&session_id, "turn-before-continue")
        .expect("finalizing an already terminal turn should be idempotent")
        .expect("terminal current turn should remain visible");
    assert_eq!(
        repeated
            .current_turn
            .as_ref()
            .and_then(|turn| turn.completed_at),
        turn.completed_at
    );
}

#[test]
fn finalize_current_turn_for_continue_rejects_a_stale_turn_owner() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-finalize-continue-stale");
    store
        .create_session(session_id.clone(), "Finalize Continue Stale")
        .expect("session should be creatable");
    accept_test_turn(&store, &session_id, test_turn("turn-current", "running", 1));

    let result = store.finalize_current_turn_for_continue(&session_id, "turn-stale");
    assert!(matches!(
        result,
        Err(magi_core::DomainError::CurrentTurnConflict {
            ref active_turn_id,
            ..
        }) if active_turn_id == "turn-current"
    ));
    assert_eq!(
        store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .map(|turn| (turn.turn_id, turn.status)),
        Some(("turn-current".to_string(), "running".to_string()))
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
    accept_test_turn(
        &store,
        &session_id,
        test_turn(resumed_turn_id, "running", 20),
    );
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
    let restored = SessionStore::from_persisted_parts(durable, sidecars)
        .expect("目标续跑状态的持久化恢复应成功");

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
    accept_test_turn(&store, &session_id, test_turn("turn-running", "running", 1));
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
    accept_test_turn(&store, &session_id, test_turn("turn-running", "running", 1));
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
    accept_test_turn(&store, &session_id, test_turn("turn-running", "running", 1));

    let updated = store
        .append_current_turn_item_with_timeline_entry_for_turn(
            &session_id,
            Some("turn-running"),
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
    accept_test_turn(&store, &session_id, test_turn("turn-running", "running", 1));

    let mut stream_item = test_turn_item("turn-item-assistant", "流式回复");
    stream_item.kind = "assistant_stream".to_string();
    stream_item.status = "running".to_string();
    store
        .upsert_current_turn_item_for_turn(&session_id, Some("turn-running"), stream_item)
        .expect("stream item should upsert");
    let running_canonical_item = store
        .canonical_turns_for_session(&session_id)
        .into_iter()
        .flat_map(|turn| turn.items)
        .find(|item| item.item_id == "turn-item-assistant")
        .expect("running assistant item should be canonical");
    assert_eq!(running_canonical_item.item_version, Some(1));

    let mut final_item = test_turn_item("turn-item-assistant", "最终回复");
    final_item.kind = "assistant_final".to_string();
    final_item.status = "completed".to_string();
    let updated = store
        .upsert_current_turn_item_for_turn(&session_id, Some("turn-running"), final_item)
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
    assert_eq!(
        canonical_item.item_version,
        Some(2),
        "assistant item 的终态事实必须递增版本，不能复用流式版本 1"
    );

    store
        .set_current_turn_status_for_test(&session_id, Some("turn-running"), "completed")
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
        SessionStore::from_persisted_parts(durable, store.execution_sidecar_store_state())
            .expect("助手输出类型缺失时的持久化恢复应成功");
    assert_eq!(
        restored
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .flat_map(|turn| turn.items)
            .find(|item| item.item_id == "turn-item-assistant")
            .and_then(|item| item.metadata.get("assistantOutputKind").cloned())
            .and_then(|value| value.as_str().map(str::to_string))
            .as_deref(),
        None,
        "canonical durable 缺失的内容字段不能由 sidecar 反向补写"
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
    );
    accept_test_turn(&store, &session_id, test_turn(turn_id, "running", 10));
    store
        .set_current_turn_status_for_test(&session_id, Some(turn_id), "failed")
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
        SessionStore::from_persisted_parts(durable, store.execution_sidecar_store_state())
            .expect("助手输出元数据清空后的持久化恢复应成功");
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
    );
    let mut turn = test_turn(turn_id, "completed", 1_000);
    turn.completed_at = Some(UtcMillis(5_250));
    accept_test_turn(&store, &session_id, turn);

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
        SessionStore::from_persisted_parts(durable, store.execution_sidecar_store_state())
            .expect("目标状态恢复后的持久化读取应成功");
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
    create_test_goal(&store, &session_id, turn_id, "验证 Goal 运行中计时");
    accept_test_turn(&store, &session_id, test_turn(turn_id, "running", 1_000));

    let running = store
        .current_goal(&session_id)
        .expect("running goal should remain readable");
    assert_eq!(running.time_used_millis, 0);
    assert_eq!(running.timing_started_at, Some(UtcMillis(1_000)));
    assert_eq!(running.timing_turn_id.as_deref(), Some(turn_id));

    let mut terminal_turn = test_turn(turn_id, "completed", 1_000);
    terminal_turn.completed_at = Some(UtcMillis(5_250));
    store
        .settle_current_turn_at_for_test(
            &session_id,
            Some(turn_id),
            terminal_turn.status.clone(),
            terminal_turn
                .completed_at
                .expect("terminal time should exist"),
        )
        .expect("terminal goal turn should settle timing");
    store
        .settle_current_turn_at_for_test(
            &session_id,
            Some(turn_id),
            terminal_turn.status,
            terminal_turn
                .completed_at
                .expect("terminal time should exist"),
        )
        .expect("repeated terminal status should remain idempotent");

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
    let goal = create_test_goal(&store, &session_id, turn_id, "验证 Goal 状态计时边界");
    accept_test_turn(&store, &session_id, test_turn(turn_id, "running", 1_000));

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
    create_test_goal(&store, &session_id, turn_id, "验证阻塞 Turn 计时结算");
    let mut turn = test_turn(turn_id, "blocked", 1_000);
    turn.completed_at = Some(UtcMillis(5_250));
    accept_test_turn(&store, &session_id, turn);

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
    );
    accept_test_turn(&store, &session_id, test_turn(turn_id, "running", 20));
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
        .set_current_turn_status_for_test(&session_id, Some(turn_id), "completed")
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
    let mut running_turn = test_turn("turn-durable", "running", 10);
    let mut user_item = test_turn_item("turn-item-durable-user", "持久用户消息");
    user_item.item_seq = 1;
    user_item.request_id = Some("request-durable-turn".to_string());
    running_turn.items.push(user_item);
    accept_test_turn(&store, &session_id, running_turn);

    let mut assistant_item = test_turn_item("turn-item-durable-assistant", "持久回复");
    assistant_item.kind = "assistant_stream".to_string();
    assistant_item.item_seq = 2;
    assistant_item.status = "running".to_string();
    store
        .upsert_current_turn_item_for_turn(&session_id, Some("turn-durable"), assistant_item)
        .expect("assistant item should upsert");

    store
        .set_current_turn_status_for_test(&session_id, Some("turn-durable"), "completed")
        .expect("turn status should update");

    let durable = store.durable_state();
    let turn = durable
        .canonical_turns
        .iter()
        .find(|turn| turn.turn_id == "turn-durable")
        .expect("canonical turn should be durable");
    assert_eq!(turn.status, crate::models::CanonicalTurnStatus::Completed);
    assert_eq!(
        turn.metadata.get("requestId"),
        Some(&json!("request-durable-turn"))
    );
    assert_eq!(turn.items.len(), 2);
    assert_eq!(turn.items[1].item_id, "turn-item-durable-assistant");
    assert_eq!(
        turn.items[1].kind,
        crate::models::CanonicalTurnItemKind::AssistantText
    );
    assert_eq!(turn.items[1].content.as_deref(), Some("持久回复"));
}

#[test]
fn canonical_writer_failure_keeps_completion_goal_plan_and_flush_unchanged() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-writer-failure-completion");
    let turn_id = "turn-writer-failure-completion";
    store
        .create_session(session_id.clone(), "Writer Failure Completion")
        .expect("session should be creatable");
    let goal = create_test_goal(
        &store,
        &session_id,
        turn_id,
        "canonical writer 失败时目标不应变化",
    );
    store
        .upsert_plan(
            &session_id,
            SessionPlan {
                plan_id: PlanId::new("plan-writer-failure-completion"),
                session_id: session_id.clone(),
                goal_id: Some(goal.goal_id.clone()),
                revision: 1,
                language: "zh-CN".to_string(),
                state: PlanState::Active,
                items: Vec::new(),
                task_bindings: HashMap::new(),
                updated_at: UtcMillis(10),
            },
            Some(0),
        )
        .expect("plan should be creatable");
    accept_test_turn(&store, &session_id, test_turn(turn_id, "running", 10));

    let before = store_mutation_snapshot(&store);
    reject_canonical_writes(&store);

    let error = store
        .set_current_turn_status_for_test(&session_id, Some(turn_id), "completed")
        .expect_err("canonical writer failure should reject completion");
    assert!(matches!(error, DomainError::Persistence { .. }));
    assert_store_mutation_snapshot_unchanged(&store, &before);
    assert_eq!(
        store
            .session(&session_id)
            .expect("session should remain")
            .last_completed_at,
        None,
        "completion notification 不能先于 canonical commit 写入"
    );
}

#[test]
fn canonical_writer_failure_keeps_root_task_completion_unchanged() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-writer-failure-root-completion");
    let turn_id = "turn-writer-failure-root-completion";
    store
        .create_session(session_id.clone(), "Writer Failure Root Completion")
        .expect("session should be creatable");
    accept_test_turn(&store, &session_id, test_turn(turn_id, "running", 10));

    let before = store_mutation_snapshot(&store);
    reject_canonical_writes(&store);

    let error = store
        .complete_current_turn_from_completed_root_task_for_turn(&session_id, Some(turn_id))
        .expect_err("canonical writer failure should reject root task completion");
    assert!(matches!(error, DomainError::Persistence { .. }));
    assert_store_mutation_snapshot_unchanged(&store, &before);
    assert_eq!(
        store
            .session(&session_id)
            .expect("session should remain")
            .last_completed_at,
        None,
        "root task completion失败时不能写 session completion"
    );
}

#[test]
fn canonical_writer_failure_keeps_cancelled_turn_and_goal_plan_unchanged() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-writer-failure-cancel");
    let turn_id = "turn-writer-failure-cancel";
    store
        .create_session(session_id.clone(), "Writer Failure Cancel")
        .expect("session should be creatable");
    let goal = create_test_goal(
        &store,
        &session_id,
        turn_id,
        "canonical writer 失败时取消不应暂停目标",
    );
    store
        .upsert_plan(
            &session_id,
            SessionPlan {
                plan_id: PlanId::new("plan-writer-failure-cancel"),
                session_id: session_id.clone(),
                goal_id: Some(goal.goal_id.clone()),
                revision: 1,
                language: "zh-CN".to_string(),
                state: PlanState::Active,
                items: Vec::new(),
                task_bindings: HashMap::new(),
                updated_at: UtcMillis(10),
            },
            Some(0),
        )
        .expect("plan should be creatable");
    accept_test_turn(&store, &session_id, test_turn(turn_id, "running", 10));

    let before = store_mutation_snapshot(&store);
    reject_canonical_writes(&store);

    let error = store
        .interrupt_current_turn_by_user(&session_id)
        .expect_err("canonical writer failure should reject cancellation");
    assert!(matches!(error, DomainError::Persistence { .. }));
    assert_store_mutation_snapshot_unchanged(&store, &before);
    assert_eq!(
        store
            .current_goal(&session_id)
            .expect("goal should remain")
            .status,
        GoalStatus::Active,
        "取消失败时目标不能被提前暂停"
    );
    assert_eq!(
        store.plan(&session_id).expect("plan should remain").state,
        PlanState::Active,
        "取消失败时 plan 不能被提前暂停"
    );
}

#[test]
fn canonical_writer_failure_keeps_daemon_restart_interruption_unchanged() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-writer-failure-restart");
    let goal_turn_id = "turn-writer-failure-restart";
    store
        .create_session(session_id.clone(), "Writer Failure Restart")
        .expect("session should be creatable");
    let goal = create_test_goal(
        &store,
        &session_id,
        goal_turn_id,
        "canonical writer 失败时重启中断不应释放目标",
    );
    let chain = test_active_chain(
        &session_id,
        "chain-writer-failure-restart",
        Some(test_turn(goal_turn_id, "running", 10)),
    );
    store
        .accept_goal_continuation_with_timeline_entry(
            session_id.clone(),
            &goal.goal_id,
            TimelineEntryInput::new(
                "timeline-writer-failure-restart",
                TimelineEntryKind::UserMessage,
                "重启中断",
                UtcMillis(10),
            ),
            chain,
        )
        .expect("goal continuation should be stored");

    let before = store_mutation_snapshot(&store);
    reject_canonical_writes(&store);

    let error = store
        .interrupt_current_turn_by_daemon_restart(&session_id)
        .expect_err("canonical writer failure should reject restart interruption");
    assert!(matches!(error, DomainError::Persistence { .. }));
    assert_store_mutation_snapshot_unchanged(&store, &before);
    let continuation = store
        .current_goal(&session_id)
        .expect("goal should remain")
        .continuation;
    assert_eq!(continuation.phase, GoalContinuationPhase::Running);
    assert_eq!(
        continuation.turn_id.as_deref(),
        Some("task-root-chain-writer-failure-restart")
    );
}

#[test]
fn canonical_writer_failure_keeps_interrupted_recovery_claim_unchanged() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-writer-failure-claim");
    store
        .create_session(session_id.clone(), "Writer Failure Claim")
        .expect("session should be creatable");
    store
        .upsert_active_execution_chain(
            session_id.clone(),
            test_active_chain(
                &session_id,
                "chain-writer-failure-claim",
                Some(test_turn("turn-writer-failure-claim", "running", 10)),
            ),
        )
        .expect("active execution chain should be stored");
    store
        .interrupt_current_turn_by_daemon_restart(&session_id)
        .expect("daemon restart interruption should be stored");

    let before = store_mutation_snapshot(&store);
    reject_canonical_writes(&store);

    let error = store
        .claim_interrupted_recovery(&session_id)
        .expect_err("canonical writer failure should reject recovery claim");
    assert!(matches!(error, DomainError::Persistence { .. }));
    assert_store_mutation_snapshot_unchanged(&store, &before);
    assert!(store.has_recovery_ready_interruption(&session_id));
    assert!(!store.has_claimed_interrupted_recovery(&session_id));
}

#[test]
fn canonical_writer_failure_keeps_interrupted_recovery_release_unchanged() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-writer-failure-release");
    let turn_id = "turn-writer-failure-release";
    store
        .create_session(session_id.clone(), "Writer Failure Release")
        .expect("session should be creatable");
    store
        .upsert_active_execution_chain(
            session_id.clone(),
            test_active_chain(
                &session_id,
                "chain-writer-failure-release",
                Some(test_turn(turn_id, "running", 10)),
            ),
        )
        .expect("active execution chain should be stored");
    store
        .interrupt_current_turn_by_daemon_restart(&session_id)
        .expect("daemon restart interruption should be stored");
    let claimed_turn_id = store
        .claim_interrupted_recovery(&session_id)
        .expect("recovery should be claimable")
        .expect("interrupted turn should be returned");
    assert_eq!(claimed_turn_id, turn_id);

    let before = store_mutation_snapshot(&store);
    reject_canonical_writes(&store);

    let error = store
        .release_interrupted_recovery_claim(&session_id, &claimed_turn_id)
        .expect_err("canonical writer failure should reject recovery release");
    assert!(matches!(error, DomainError::Persistence { .. }));
    assert_store_mutation_snapshot_unchanged(&store, &before);
    assert!(!store.has_recovery_ready_interruption(&session_id));
    assert!(store.has_claimed_interrupted_recovery(&session_id));
}

#[test]
fn canonical_writer_failure_keeps_branch_snapshot_unchanged() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-writer-failure-branch");
    let task_id = TaskId::new("task-writer-failure-branch");
    let worker_id = WorkerId::new("worker-writer-failure-branch");
    store
        .create_session(session_id.clone(), "Writer Failure Branch")
        .expect("session should be creatable");
    let mut chain = test_active_chain(
        &session_id,
        "chain-writer-failure-branch",
        Some(test_turn("turn-writer-failure-branch", "running", 10)),
    );
    chain.active_branch_task_ids = vec![task_id.clone()];
    chain.active_worker_bindings = vec![worker_id.clone()];
    chain.branches = vec![ActiveExecutionBranch {
        task_id: task_id.clone(),
        worker_id: worker_id.clone(),
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
        thread_id: ThreadId::new("thread-writer-failure-branch"),
    }];
    store
        .upsert_active_execution_chain(session_id.clone(), chain)
        .expect("active execution chain should be stored");

    // 模拟运行态 sidecar 已收到新 item，而 canonical 尚未收到该事实；分支快照
    // 不能在 canonical writer 失败时把候选 branch 或 sidecar 提前写入内存。
    let mut stale_sidecar = store
        .runtime_sidecar(&session_id)
        .expect("sidecar should exist");
    let mut sidecar_turn = stale_sidecar
        .current_turn
        .clone()
        .expect("current turn should exist");
    sidecar_turn
        .items
        .push(test_turn_item("item-branch-delta", "运行中"));
    sidecar_turn.normalize();
    stale_sidecar.current_turn = Some(sidecar_turn.clone());
    if let Some(chain) = stale_sidecar.active_execution_chain.as_mut() {
        chain.current_turn = Some(sidecar_turn);
    }
    store.upsert_runtime_sidecar(stale_sidecar);

    let before = store_mutation_snapshot(&store);
    reject_canonical_writes(&store);

    let error = store
        .update_active_execution_branch_snapshot(ActiveExecutionBranchSnapshotUpdate {
            task_id,
            worker_id,
            stage: "verify".to_string(),
            lease_id: None,
            execution_intent_ref: None,
            binding_lifecycle: Some("active".to_string()),
            checkpoint_stage: Some("checkpoint".to_string()),
            next_step_index: Some(1),
            checkpoint_at: Some(UtcMillis(20)),
            resume_mode: Some("resume".to_string()),
            resume_token: Some("token".to_string()),
        })
        .expect_err("canonical writer failure should reject branch snapshot");
    assert!(matches!(error, DomainError::Persistence { .. }));
    assert_store_mutation_snapshot_unchanged(&store, &before);
    assert_eq!(
        store
            .runtime_sidecar(&session_id)
            .expect("sidecar should remain")
            .active_execution_chain
            .expect("execution chain should remain")
            .branches[0]
            .stage,
        "execute"
    );
}

#[test]
fn canonical_turn_request_id_lookup_survives_normalization_and_restore() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-request-id-lookup");
    store
        .create_session(session_id.clone(), "Request ID Lookup")
        .expect("session should be creatable");
    accept_test_turn(
        &store,
        &session_id,
        test_turn("turn-request-id", "running", 10),
    );

    let mut item = test_turn_item("item-request-id", "幂等请求");
    item.request_id = Some("request-id-1".to_string());
    item.item_seq = 2;
    store
        .upsert_current_turn_item_for_turn(&session_id, Some("turn-request-id"), item)
        .expect("request item should be persisted");

    let found = store
        .canonical_turn_for_request_id(" request-id-1 ")
        .expect("request id should resolve canonical turn");
    assert_eq!(found.turn_id, "turn-request-id");
    assert_eq!(found.items[0].item_seq, 2);

    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .expect("请求 ID 索引的持久化恢复应成功");
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
    accept_test_turn(
        &store,
        &session_id,
        test_turn("turn-unread-completion", "running", 10),
    );

    store
        .set_current_turn_status_for_test(&session_id, Some("turn-unread-completion"), "completed")
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
    accept_test_turn(
        &store,
        &session_id,
        test_turn("turn-root-task-unread-completion", "running", 10),
    );

    store
        .complete_current_turn_from_completed_root_task_for_turn(
            &session_id,
            Some("turn-root-task-unread-completion"),
        )
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
    accept_test_turn(
        &store,
        &session_id,
        test_turn("turn-viewed-completion", "running", 10),
    );
    store
        .set_current_turn_status_for_test(&session_id, Some("turn-viewed-completion"), "completed")
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
    )
    .expect("未读完成状态的持久化恢复应成功");
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
    accept_test_turn(
        &store,
        &session_id,
        test_turn("turn-failed-no-unread-completion", "running", 10),
    );

    store
        .set_current_turn_status_for_test(
            &session_id,
            Some("turn-failed-no-unread-completion"),
            "failed",
        )
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
    accept_test_turn(
        &store,
        &session_id,
        test_turn("turn-completed-replay", "running", 10),
    );
    store
        .set_current_turn_status_for_test(&session_id, Some("turn-completed-replay"), "completed")
        .expect("turn should complete");
    let completed_at = store
        .session(&session_id)
        .and_then(|session| session.last_completed_at)
        .expect("completion timestamp should exist");
    store
        .mark_session_viewed_at(&session_id, UtcMillis(completed_at.0.saturating_add(1)))
        .expect("session should be marked viewed");

    store
        .set_current_turn_status_for_test(&session_id, Some("turn-completed-replay"), "completed")
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
    accept_test_turn(
        &store,
        &session_id,
        test_turn("turn-internal-tool", "running", 10),
    );

    let mut wait_item = test_turn_item("turn-item-agent-wait", "{\"status\":\"succeeded\"}");
    wait_item.kind = "tool_call_result".to_string();
    wait_item.status = "completed".to_string();
    wait_item.title = Some("agent_wait".to_string());
    wait_item.tool_call_id = Some("tool-call-agent-wait".to_string());
    wait_item.tool_name = Some("agent_wait".to_string());
    wait_item.tool_arguments = Some("{\"task_ids\":[\"task-1\"]}".to_string());
    wait_item.tool_result = Some("{\"status\":\"succeeded\"}".to_string());
    store
        .upsert_current_turn_item_for_turn(&session_id, Some("turn-internal-tool"), wait_item)
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
        .upsert_current_turn_item_for_turn(&session_id, Some("turn-internal-tool"), spawn_item)
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
    accept_test_turn(&store, &session_id, turn);

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
fn command_only_user_message_is_renderable_after_acceptance_and_reload() {
    for (key, value) in [
        (crate::SESSION_COMMAND_METADATA_KEY, json!("compact")),
        ("skillName", json!("review")),
        ("goalMode", json!(true)),
    ] {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-command-only-message");
        store
            .create_session(session_id.clone(), "Command Only Message")
            .expect("session should create");
        let mut turn = test_turn("turn-command-only-message", "running", 10);
        turn.user_message = Some(String::new());
        let mut user = test_turn_item("user-command-only", "");
        user.metadata.insert(key.to_string(), value.clone());
        turn.items.push(user);
        accept_test_turn(&store, &session_id, turn);

        let assert_command = |store: &SessionStore| {
            let turns = store.canonical_turns_for_session(&session_id);
            assert_eq!(turns.len(), 1);
            assert_eq!(
                turns[0].is_session_command(),
                key == crate::SESSION_COMMAND_METADATA_KEY
            );
            let user = &turns[0].items[0];
            assert_eq!(user.content.as_deref(), Some(""));
            assert_eq!(user.metadata[key], value);
            assert!(user.visibility.renderable, "命令本身就是可见内容");
        };
        assert_command(&store);
        store
            .set_current_turn_status_for_test(
                &session_id,
                Some("turn-command-only-message"),
                "completed",
            )
            .expect("turn should complete");
        assert_command(&store);
        let durable = serde_json::from_slice(
            &serde_json::to_vec(&store.durable_state()).expect("serialize durable state"),
        )
        .expect("deserialize durable state");
        let restored =
            SessionStore::from_persisted_parts(durable, store.execution_sidecar_store_state())
                .expect("restore command turn");
        assert_command(&restored);
    }
}

#[test]
fn blocked_current_turn_is_terminal_in_canonical_log() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-blocked-terminal-canonical");
    store
        .create_session(session_id.clone(), "Blocked Terminal Canonical")
        .expect("session should be creatable");
    accept_test_turn(
        &store,
        &session_id,
        test_turn("turn-blocked-terminal", "running", 10),
    );

    let mut assistant_item = test_turn_item("turn-item-blocked-assistant", "等待用户处理");
    assistant_item.kind = "assistant_error".to_string();
    assistant_item.status = "running".to_string();
    store
        .upsert_current_turn_item_for_turn(
            &session_id,
            Some("turn-blocked-terminal"),
            assistant_item,
        )
        .expect("assistant item should upsert");

    store
        .set_current_turn_status_for_test(&session_id, Some("turn-blocked-terminal"), "blocked")
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
    accept_test_turn(
        &store,
        &session_id,
        test_turn("turn-killed-terminal", "running", 10),
    );

    let mut assistant_item = test_turn_item("turn-item-killed-assistant", "任务执行已终止");
    assistant_item.kind = "assistant_error".to_string();
    assistant_item.status = "running".to_string();
    store
        .upsert_current_turn_item_for_turn(
            &session_id,
            Some("turn-killed-terminal"),
            assistant_item,
        )
        .expect("assistant item should upsert");

    let updated = store
        .set_current_turn_status_for_test(&session_id, Some("turn-killed-terminal"), "killed")
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
    accept_test_turn(
        &store,
        &session_id,
        test_turn("turn-killed-task-status-item", "running", 10),
    );

    let mut task_item = test_turn_item("turn-item-task-killed", "子任务已终止");
    task_item.kind = "task_status".to_string();
    task_item.status = "killed".to_string();
    task_item.source = "task".to_string();
    store
        .upsert_current_turn_item_for_turn(
            &session_id,
            Some("turn-killed-task-status-item"),
            task_item,
        )
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
    accept_test_turn(
        &store,
        &session_id,
        test_turn("turn-terminal-wins", "running", 10),
    );
    store
        .set_current_turn_status_for_test(&session_id, Some("turn-terminal-wins"), "completed")
        .expect("turn should complete");

    let durable_state = store.durable_state();
    let mut sidecar_store = store.execution_sidecar_store_state();
    let stale_turn = sidecar_store.runtime_sidecars[0]
        .current_turn
        .as_mut()
        .expect("sidecar current turn should exist");
    stale_turn.status = "running".to_string();
    stale_turn.completed_at = None;

    let restored = SessionStore::from_persisted_parts(durable_state, sidecar_store)
        .expect("陈旧执行 Turn 清理后的持久化恢复应成功");
    let turns = restored.canonical_turns_for_session(&session_id);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].status, CanonicalTurnStatus::Completed);
    let sidecar = restored
        .runtime_sidecar(&session_id)
        .expect("terminal sidecar should remain");
    let sidecar_turn = sidecar
        .current_turn
        .expect("terminal sidecar turn should remain");
    assert_eq!(sidecar_turn.status, "completed");
    assert_eq!(sidecar_turn.completed_at, turns[0].completed_at);
}

#[test]
fn persisted_parts_rejects_non_terminal_canonical_and_sidecar_item_mismatch() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-terminal-active-item-repair");
    store
        .create_session(session_id.clone(), "Terminal Active Item Repair")
        .expect("session should be creatable");
    accept_test_turn(
        &store,
        &session_id,
        test_turn("turn-terminal-active-item-repair", "running", 10),
    );

    let mut assistant_item = test_turn_item("turn-item-terminal-active", "任务执行需要处理");
    assistant_item.kind = "assistant_error".to_string();
    assistant_item.status = "running".to_string();
    store
        .upsert_current_turn_item_for_turn(
            &session_id,
            Some("turn-terminal-active-item-repair"),
            assistant_item,
        )
        .expect("assistant item should upsert");
    store
        .set_current_turn_status_for_test(
            &session_id,
            Some("turn-terminal-active-item-repair"),
            "blocked",
        )
        .expect("turn should become blocked");

    let mut durable_state = store.durable_state();
    durable_state.canonical_turns[0].items[0].status =
        crate::models::CanonicalTurnItemStatus::Running;
    let sidecar_store = store.execution_sidecar_store_state();

    let error = match SessionStore::from_persisted_parts(durable_state, sidecar_store) {
        Ok(_) => panic!("非终态 canonical 与 sidecar item 状态不一致时必须拒绝恢复"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("非终态 canonical 状态不一致"));
}

#[test]
fn persisted_parts_rejects_sidecar_turn_without_canonical_fact() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-sidecar-without-canonical");
    store
        .create_session(session_id.clone(), "Sidecar Without Canonical")
        .expect("session should be creatable");
    accept_test_turn(
        &store,
        &session_id,
        test_turn("turn-sidecar-without-canonical", "running", 10),
    );
    let mut durable = store.durable_state();
    durable.canonical_turns.clear();

    let error =
        match SessionStore::from_persisted_parts(durable, store.execution_sidecar_store_state()) {
            Ok(_) => panic!("缺少 canonical turn 的 sidecar 必须拒绝恢复"),
            Err(error) => error,
        };
    assert!(error.to_string().contains("缺少对应 canonical turn"));
}

#[test]
fn upsert_current_turn_item_rejects_canonical_immutable_field_conflict() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-canonical-conflict");
    store
        .create_session(session_id.clone(), "Canonical Conflict")
        .expect("session should be creatable");
    accept_test_turn(&store, &session_id, test_turn("turn-running", "running", 1));

    let mut stream_item = test_turn_item("turn-item-conflict", "流式回复");
    stream_item.kind = "assistant_stream".to_string();
    stream_item.status = "running".to_string();
    store
        .upsert_current_turn_item_for_turn(&session_id, Some("turn-running"), stream_item)
        .expect("stream item should upsert");

    let mut conflicting_item = test_turn_item("turn-item-conflict", "工具调用");
    conflicting_item.kind = "tool_call_started".to_string();
    conflicting_item.status = "running".to_string();
    conflicting_item.tool_call_id = Some("tool-conflict".to_string());
    conflicting_item.tool_name = Some("shell_exec".to_string());
    let result = store.upsert_current_turn_item_for_turn(
        &session_id,
        Some("turn-running"),
        conflicting_item,
    );

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
    accept_test_turn(&store, &session_id, test_turn("turn-running", "running", 1));

    let mut final_item = test_turn_item("turn-item-status", "最终回复");
    final_item.kind = "assistant_final".to_string();
    final_item.status = "completed".to_string();
    store
        .upsert_current_turn_item_for_turn(&session_id, Some("turn-running"), final_item)
        .expect("final item should upsert");

    let mut failed_item = test_turn_item("turn-item-status", "失败回复");
    failed_item.kind = "assistant_error".to_string();
    failed_item.status = "failed".to_string();
    let result =
        store.upsert_current_turn_item_for_turn(&session_id, Some("turn-running"), failed_item);

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
    accept_test_turn(&store, &session_id, test_turn("turn-current", "running", 1));

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
fn stale_turn_writeback_cannot_mutate_a_replacement_turn_or_resurrect_cancelled_turn() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-stale-writeback-replacement");
    store
        .create_session(session_id.clone(), "Stale Writeback Replacement")
        .expect("session should be creatable");
    accept_test_turn(&store, &session_id, test_turn("turn-old", "running", 1));
    store
        .interrupt_current_turn_by_user(&session_id)
        .expect("old turn should be interruptible");
    accept_test_turn(&store, &session_id, test_turn("turn-new", "running", 2));

    let stale_item = store.append_current_turn_item_for_turn(
        &session_id,
        Some("turn-old"),
        test_turn_item("turn-item-stale", "迟到的旧轮次写回"),
    );
    let stale_status =
        store.set_current_turn_status_for_test(&session_id, Some("turn-old"), "failed");

    assert!(matches!(
        stale_item,
        Err(magi_core::DomainError::CurrentTurnConflict { .. })
    ));
    assert!(matches!(
        stale_status,
        Err(magi_core::DomainError::CurrentTurnConflict { .. })
    ));
    let current_turn = store
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .expect("replacement turn should remain");
    assert_eq!(current_turn.turn_id, "turn-new");
    assert_eq!(current_turn.status, "running");
    assert!(current_turn.items.is_empty());
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
        .append_current_turn_item_with_timeline_entry_for_turn(
            &session_id,
            Some("turn-no-current"),
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
    let restored = SessionStore::from_persisted_parts(durable_state, sidecar_store)
        .expect("执行侧车导出的持久化恢复应成功");

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
        SessionStore::from_persisted_parts(durable, SessionExecutionSidecarStoreState::default())
            .expect("通知记录的持久化恢复应成功");

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
fn partial_sidecar_flush_keeps_uncommitted_and_concurrently_changed_sessions_dirty() {
    let store = SessionStore::new();
    let first = SessionId::new("checkpoint-first");
    let second = SessionId::new("checkpoint-second");
    for id in [&first, &second] {
        store.create_session(id.clone(), "checkpoint").unwrap();
        store.bind_execution_ownership(id.clone(), ExecutionOwnership::default());
    }
    let mut changed = store.runtime_sidecar(&second).unwrap();
    changed.updated_at = UtcMillis(changed.updated_at.0 + 1);
    assert!(
        store
            .flush_execution_sidecar_snapshot_with(|_, _, _| {
                store.upsert_runtime_sidecar(changed.clone());
                Ok::<_, ()>(vec![second.clone()])
            })
            .unwrap()
    );
    assert_eq!(
        store.pending_execution_sidecar_session_ids(),
        vec![first.clone(), second.clone()]
    );
    assert!(
        store
            .flush_execution_sidecar_snapshot_with(|_, _, _| Err::<Vec<SessionId>, _>(
                "write failed"
            ))
            .is_err()
    );
    assert_eq!(
        store.pending_execution_sidecar_session_ids(),
        vec![first.clone(), second.clone()]
    );
    assert!(
        store
            .flush_execution_sidecar_snapshot_with(|_, _, _| Ok::<_, ()>(vec![second.clone()]))
            .unwrap()
    );
    assert_eq!(
        store.pending_execution_sidecar_session_ids(),
        vec![first.clone()]
    );
    assert!(
        !store
            .flush_execution_sidecar_snapshot_with(|_, _, _| Ok::<_, ()>(Vec::new()))
            .unwrap()
    );
    assert!(
        store
            .execution_sidecar_flush_metadata()
            .next_flush_hint
            .is_some()
    );
    assert!(
        store
            .flush_execution_sidecar_snapshot_with(|_, _, ids| Ok::<_, ()>(ids.to_vec()))
            .unwrap()
    );
    assert!(store.pending_execution_sidecar_session_ids().is_empty());
    let metadata = store.execution_sidecar_flush_metadata();
    assert_eq!(metadata.current_version, metadata.flushed_version);
    assert!(metadata.next_flush_hint.is_none());
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
    assert_eq!(
        store.pending_execution_sidecar_session_ids(),
        vec![session_id.clone()]
    );

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
    assert_eq!(
        store.pending_execution_sidecar_session_ids(),
        vec![session_id.clone()]
    );

    let mut flushes = Vec::new();
    assert!(
        store
            .flush_execution_sidecar_snapshot_with(|_, state, ids| {
                flushes.push(state.runtime_sidecars.len());
                Ok::<_, std::io::Error>(ids.to_vec())
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
    assert!(store.pending_execution_sidecar_session_ids().is_empty());
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
        .flush_execution_sidecar_snapshot_with(|_, _, ids| Ok::<_, std::io::Error>(ids.to_vec()))
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
        .flush_execution_sidecar_snapshot_with(|_, state, ids| {
            assert_eq!(state.runtime_sidecars.len(), 1);
            assert_eq!(
                state.runtime_sidecars[0].status,
                SessionExecutionSidecarStatus::Resumed
            );
            Ok::<_, std::io::Error>(ids.to_vec())
        })
        .expect("flush should succeed");
    assert!(flushed);
    let m3f = store.execution_sidecar_flush_metadata();
    assert_eq!(m3f.flushed_version, 3);
    assert!(m3f.next_flush_hint.is_none());

    let flushed = store
        .flush_execution_sidecar_snapshot_with(|_, _, ids| Ok::<_, std::io::Error>(ids.to_vec()))
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
    let restored = SessionStore::from_persisted_parts(durable_state, sidecar_store)
        .expect("运行时侧车的持久化恢复应成功");

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
        .flush_execution_sidecar_snapshot_with(|_, _, ids| Ok::<_, std::io::Error>(ids.to_vec()))
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
    accept_test_turn(
        &store,
        &session_id,
        test_turn("turn-delete-runtime-history", "completed", 11),
    );

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
fn delete_session_transaction_keeps_memory_state_when_persistence_fails() {
    let store = SessionStore::new();
    let deleted_id = SessionId::new("session-delete-transaction-target");
    let replacement_id = SessionId::new("session-delete-transaction-replacement");
    store
        .create_session(deleted_id.clone(), "target")
        .expect("target session should create");
    store
        .create_session(replacement_id.clone(), "replacement")
        .expect("replacement session should create");
    store
        .select_current_session(&deleted_id)
        .expect("target session should become current");
    let before = serde_json::to_value(store.durable_state()).expect("state should serialize");

    let result = store.delete_session_with_persistence(
        &deleted_id,
        Some(&replacement_id),
        |_durable, _sidecars| Err::<(), _>("projection write failed"),
    );

    assert!(matches!(
        result,
        Err(SessionMutationTransactionError::Persistence(
            "projection write failed"
        ))
    ));
    assert_eq!(
        serde_json::to_value(store.durable_state()).expect("state should serialize"),
        before,
        "projection 失败不得提交内存删除或 current 替换"
    );
    assert!(store.session(&deleted_id).is_some());
    assert_eq!(store.current_session_id(), Some(deleted_id));
}

#[test]
fn delete_session_transaction_preserves_non_current_and_replaces_current() {
    let store = SessionStore::new();
    let current_id = SessionId::new("session-delete-transaction-current");
    let other_id = SessionId::new("session-delete-transaction-other");
    store
        .create_session(current_id.clone(), "current")
        .expect("current session should create");
    store
        .create_session(other_id.clone(), "other")
        .expect("other session should create");
    store
        .select_current_session(&current_id)
        .expect("current session should select");

    store
        .delete_session_with_persistence(&other_id, None, |_durable, _sidecars| Ok::<_, ()>(()))
        .expect("non-current session should delete");
    assert!(store.session(&other_id).is_none());
    assert_eq!(store.current_session_id(), Some(current_id.clone()));

    store
        .create_session(other_id.clone(), "other again")
        .expect("replacement session should recreate");
    store
        .select_current_session(&current_id)
        .expect("current session should be reselected");
    store
        .delete_session_with_persistence(&current_id, Some(&other_id), |_durable, _sidecars| {
            Ok::<_, ()>(())
        })
        .expect("current session should delete with replacement");
    assert!(store.session(&current_id).is_none());
    assert_eq!(store.current_session_id(), Some(other_id));
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
    accept_test_turn(&store, &session_id, turn);

    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        SessionExecutionSidecarStoreState::default(),
    )
    .expect("执行任务索引的持久化恢复应成功");

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
            .flush_execution_sidecar_snapshot_with(|_, state, ids| {
                flushes.push(state.runtime_sidecars.len());
                Ok::<_, std::io::Error>(ids.to_vec())
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
            .flush_execution_sidecar_snapshot_with(|_, state, ids| {
                flushes.push(state.runtime_sidecars.len());
                Ok::<_, std::io::Error>(ids.to_vec())
            })
            .expect("dirty sidecar flush should succeed")
    );
    assert_eq!(flushes, vec![1]);
    assert!(
        !store
            .flush_execution_sidecar_snapshot_with(|_, _, ids| Ok::<_, std::io::Error>(
                ids.to_vec()
            ))
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
    store
        .register_thread(thread)
        .expect("thread should register");
    store.record_thread_context_window_tokens(&thread_id, 128_000, UtcMillis(1_100));
    store.append_thread_messages(
        &thread_id,
        vec![
            ThreadChatMessage {
                input_request_id: None,
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
                input_request_id: None,
                role: "tool".to_string(),
                content: Some("exit code 1: compilation failed".to_string()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: Some("call-shell-round-trip".to_string()),
                provider_context: Vec::new(),
            },
            ThreadChatMessage {
                input_request_id: None,
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
                input_request_id: None,
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
    )
    .expect("线程注册表的持久化恢复应成功");
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
fn thread_registry_activation_tracks_task_ids_without_reuse_lookup() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-thread-activation");
    let mission_id = MissionId::new("mission-thread-activation");
    let now = UtcMillis(1_000);
    let thread_id = ThreadId::new("thread-backend-1");

    store
        .register_thread(sample_thread(
            thread_id.as_str(),
            &session_id,
            &mission_id,
            "executor",
            now,
            ExecutionThreadStatus::Idle,
        ))
        .expect("thread should register");

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
        store
            .register_thread(thread)
            .expect("thread should register");
    }

    let flush_version_before_settle = store.execution_sidecar_flush_metadata().current_version;
    assert_eq!(
        store.mark_task_threads_idle(&target_task_id, UtcMillis(3_000)),
        1
    );
    let flush_after_settle = store.execution_sidecar_flush_metadata();
    assert!(flush_after_settle.current_version > flush_version_before_settle);
    assert_eq!(
        flush_after_settle.last_dirty_reason,
        Some(SessionSidecarFlushReason::SettleThread)
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

    store
        .register_thread(sample_thread(
            "thread-a-backend",
            &session_a,
            &mission_id,
            "executor",
            now,
            ExecutionThreadStatus::Idle,
        ))
        .expect("thread should register");
    store
        .register_thread(sample_thread(
            "thread-b-backend",
            &session_b,
            &mission_id,
            "executor",
            now,
            ExecutionThreadStatus::Idle,
        ))
        .expect("thread should register");

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

    store
        .register_thread(sample_thread(
            "thread-r-1",
            &session_id,
            &mission_id,
            "reviewer",
            now,
            ExecutionThreadStatus::Idle,
        ))
        .expect("thread should register");
    store
        .register_thread(sample_thread(
            "thread-r-2",
            &session_id,
            &mission_id,
            "architect",
            now,
            ExecutionThreadStatus::Active,
        ))
        .expect("thread should register");

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

#[test]
fn rebuilding_thread_projection_while_assistant_text_streams_does_not_duplicate_history() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-thread-stream-rebuild");
    store
        .create_session(session_id.clone(), "Thread Stream Rebuild")
        .expect("session should be creatable");
    let (_mission_id, thread_id) = store.ensure_session_mission(&session_id, UtcMillis(1), || {
        MissionId::new("mission-thread-stream-rebuild")
    });
    accept_test_turn(
        &store,
        &session_id,
        test_turn("turn-thread-stream-rebuild", "running", 10),
    );
    let mut user = test_turn_item("user-thread-stream", "分析当前项目");
    user.item_seq = 1;
    user.source_thread_id = thread_id.clone();
    store
        .upsert_current_turn_item_for_turn(&session_id, Some("turn-thread-stream-rebuild"), user)
        .expect("user item should upsert");

    let full_text = "我先建立项目结构图，再抽样读取关键文件验证架构。";
    let chars = full_text.chars().collect::<Vec<_>>();
    let mut lengths = Vec::new();
    for step in 1..=chars.len() {
        let mut assistant = test_turn_item("assistant-thread-stream", "");
        assistant.kind = "assistant_stream".to_string();
        assistant.source = "assistant".to_string();
        assistant.status = "running".to_string();
        assistant.item_seq = 2;
        assistant.content = Some(chars[..step].iter().collect());
        assistant.source_thread_id = thread_id.clone();
        store
            .upsert_current_turn_item_for_turn(
                &session_id,
                Some("turn-thread-stream-rebuild"),
                assistant,
            )
            .expect("assistant item should upsert");
        store
            .rebuild_thread_message_projection(&thread_id, UtcMillis(100 + step as u64))
            .expect("projection should rebuild");
        lengths.push(store.thread_message_history(&thread_id).len());
    }

    let history = store.thread_message_history(&thread_id);
    assert_eq!(
        lengths.iter().copied().max(),
        Some(2),
        "流式输出期间反复重建，历史长度必须稳定在 user + assistant: {lengths:?}"
    );
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].content.as_deref(), Some(full_text));
}

fn thread_message(role: &str, content: &str) -> ThreadChatMessage {
    ThreadChatMessage {
        input_request_id: None,
        role: role.to_string(),
        content: Some(content.to_string()),
        images: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        provider_context: Vec::new(),
    }
}

fn thread_tool_call(call_id: &str) -> ThreadChatMessage {
    ThreadChatMessage {
        input_request_id: None,
        role: "assistant".to_string(),
        content: None,
        images: Vec::new(),
        tool_calls: vec![ThreadChatToolCall {
            id: call_id.to_string(),
            kind: "function".to_string(),
            function: ThreadChatToolFunction {
                name: "file_read".to_string(),
                arguments: "{}".to_string(),
            },
        }],
        tool_call_id: None,
        provider_context: Vec::new(),
    }
}

fn thread_tool_result(call_id: &str, content: &str) -> ThreadChatMessage {
    ThreadChatMessage {
        input_request_id: None,
        role: "tool".to_string(),
        content: Some(content.to_string()),
        images: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: Some(call_id.to_string()),
        provider_context: Vec::new(),
    }
}

#[test]
fn thread_history_merge_is_idempotent_and_keeps_non_canonical_messages() {
    use super::sidecar::merge_thread_history_with_projection as merge;

    let projection = vec![
        thread_message("user", "分析项目"),
        thread_tool_call("call-1"),
        thread_tool_result("call-1", "结果一"),
        thread_message("assistant", "完成"),
    ];

    // 没有 canonical 对应物的前缀保留在最前面，重复合并不再增长。
    let legacy = vec![thread_message("user", "旧迁移消息")];
    let once = merge(&legacy, projection.clone());
    assert_eq!(once.len(), 1 + projection.len());
    assert_eq!(once[0], legacy[0]);
    assert_eq!(merge(&once, projection.clone()), once);

    // 工具结果尚未落盘时的占位，在真实结果到达后被替换而不是追加。
    let pending = vec![
        thread_message("user", "分析项目"),
        thread_tool_call("call-1"),
        thread_tool_result("call-1", "canonical_projection_missing_tool_result"),
    ];
    let resolved = merge(&pending, projection.clone());
    assert_eq!(resolved, projection);

    // 流式输出：assistant 文本变长时原地更新。
    let streaming = vec![
        thread_message("user", "分析项目"),
        thread_message("assistant", "完"),
    ];
    assert_eq!(merge(&streaming, projection[..1].to_vec()).len(), 2);
    let grown = vec![
        thread_message("user", "分析项目"),
        thread_message("assistant", "完成了"),
    ];
    let merged = merge(&streaming, grown.clone());
    assert_eq!(merged, grown);

    // 恢复流程在历史中间插入的消息保持原位，且反复合并不再复制后续内容。
    let with_inserted = vec![
        thread_message("user", "分析项目"),
        thread_tool_call("call-1"),
        thread_message("system", "恢复流程插入"),
        thread_tool_result("call-1", "结果一"),
    ];
    let first = merge(&with_inserted, projection.clone());
    assert_eq!(first.len(), projection.len() + 1);
    assert!(first.iter().any(|message| message.role == "system"));
    assert_eq!(merge(&first, projection.clone()), first);

    // 空 projection 不改动已有历史。
    assert_eq!(merge(&legacy, Vec::new()), legacy);
}

#[test]
fn rename_and_delete_hand_the_persistence_callback_only_the_affected_sessions_history() {
    let store = SessionStore::new();
    let target_id = SessionId::new("session-slice-target");
    let other_id = SessionId::new("session-slice-other");
    store
        .create_session(target_id.clone(), "target")
        .expect("target session should create");
    store
        .create_session(other_id.clone(), "other")
        .expect("other session should create");
    accept_test_turn(
        &store,
        &target_id,
        test_turn("turn-slice-target", "completed", 10),
    );
    accept_test_turn(
        &store,
        &other_id,
        test_turn("turn-slice-other", "completed", 20),
    );
    store
        .select_current_session(&target_id)
        .expect("target should be current");

    // 重命名：会话列表是完整的（持久化层靠它判断哪些会话不再保留），但只带被重命名会话的回合。
    let mut seen = None;
    store
        .rename_session_with_persistence(&target_id, "renamed", |durable, _| {
            seen = Some((
                durable
                    .sessions
                    .iter()
                    .map(|session| (session.session_id.clone(), session.title.clone()))
                    .collect::<Vec<_>>(),
                durable
                    .canonical_turns
                    .iter()
                    .map(|turn| turn.session_id.clone())
                    .collect::<Vec<_>>(),
            ));
            Ok::<_, ()>(())
        })
        .expect("rename should persist");
    let (sessions, turn_owners) = seen.expect("callback ran");
    assert_eq!(sessions.len(), 2);
    assert!(sessions.contains(&(target_id.clone(), "renamed".to_string())));
    assert!(turn_owners.iter().all(|owner| owner == &target_id));
    assert!(!turn_owners.is_empty());

    // 删除：回调看到的是「删除之后」的视图——被删会话不在列表里，current 已指向替代会话，
    // 替代会话的回合整段在内；回调成功后内存才真正删除。
    let mut view = None;
    store
        .delete_session_with_persistence(&target_id, Some(&other_id), |durable, _| {
            view = Some((
                durable
                    .sessions
                    .iter()
                    .map(|session| session.session_id.clone())
                    .collect::<Vec<_>>(),
                durable.current_session_id.clone(),
                durable
                    .canonical_turns
                    .iter()
                    .map(|turn| turn.session_id.clone())
                    .collect::<Vec<_>>(),
            ));
            Ok::<_, ()>(())
        })
        .expect("delete should persist");
    let (sessions, current, turn_owners) = view.expect("callback ran");
    assert_eq!(sessions, vec![other_id.clone()]);
    assert_eq!(current, Some(other_id.clone()));
    assert!(turn_owners.iter().all(|owner| owner == &other_id));
    assert!(store.session(&target_id).is_none());
    assert_eq!(store.current_session_id(), Some(other_id));
}

#[test]
fn indeterminate_item_status_is_terminal_and_not_rewritable() {
    use crate::models::CanonicalTurnItemStatus as Status;
    assert!(Status::Indeterminate.is_terminal());
    for from in [Status::Pending, Status::Running, Status::Blocked] {
        assert!(from.allows_transition_to(Status::Indeterminate));
    }
    for next in [
        Status::Running,
        Status::Completed,
        Status::Failed,
        Status::Cancelled,
    ] {
        assert!(!Status::Indeterminate.allows_transition_to(next));
    }
    assert_eq!(
        serde_json::to_value(Status::Indeterminate).unwrap(),
        json!("indeterminate")
    );
}

#[test]
fn indeterminate_tool_result_can_be_written_and_restored_without_failing_the_turn() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-indeterminate-browser");
    store
        .create_session(session_id.clone(), "Browser result")
        .unwrap();
    accept_test_turn(
        &store,
        &session_id,
        test_turn("turn-browser", "running", 10),
    );
    let mut item = test_turn_item("browser-click", "浏览器点击");
    item.kind = "tool_call_started".to_string();
    item.status = "running".to_string();
    item.tool_call_id = Some("call-browser".to_string());
    item.tool_name = Some("browser_click".to_string());
    item.tool_arguments = Some("{}".to_string());
    store
        .upsert_current_turn_item_for_turn(&session_id, Some("turn-browser"), item.clone())
        .unwrap();
    item.status = "indeterminate".to_string();
    item.tool_status = Some("indeterminate".to_string());
    item.tool_result = Some("页面导航，点击结果需确认".to_string());
    store
        .upsert_current_turn_item_for_turn(&session_id, Some("turn-browser"), item)
        .unwrap();
    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    let turn = restored
        .runtime_sidecar(&session_id)
        .unwrap()
        .current_turn
        .unwrap();
    assert_eq!(turn.status, "running");
    let tool = turn
        .items
        .iter()
        .find(|item| item.item_id == "browser-click")
        .unwrap();
    assert_eq!(tool.status, "indeterminate");
    assert_eq!(tool.tool_status.as_deref(), Some("indeterminate"));
}

fn test_model_step(id: &str, input_request_ids: Vec<String>) -> crate::ModelStep {
    crate::ModelStep {
        attempts: vec![crate::ModelAttempt {
            attempt_id: format!("{id}:attempt:1"),
            started_at: UtcMillis(10),
            settled_at: None,
            status: crate::ModelStepStatus::Started,
        }],
        step_id: id.into(),
        task_id: None,
        thread_id: ThreadId::new("thread-step"),
        goal_id: None,
        usage_tokens: None,
        goal_tokens_after: None,
        checkpoint_generation: 2,
        request_fingerprint: "a".repeat(64),
        binding: None,
        executor_capabilities: None,
        purpose: None,
        provider: "test".into(),
        message_count: 1,
        tool_count: 0,
        max_output_tokens: None,
        input_request_ids,
        started_at: UtcMillis(10),
        settled_at: None,
        status: crate::ModelStepStatus::Started,
        response_fingerprint: None,
    }
}

#[test]
fn model_steps_survive_stream_writes_and_restart_without_replaying_unknown_calls() {
    let store = SessionStore::new();
    let session = SessionId::new("session-model-step");
    store.create_session(session.clone(), "step").unwrap();
    accept_test_turn(&store, &session, test_turn("turn-step", "running", 10));
    let step = test_model_step("call-1", vec![]);
    store
        .begin_model_step(&session, "turn-step", step.clone())
        .unwrap();
    // No new user input does not mean there was no model Step.
    store
        .append_current_turn_item_for_turn(
            &session,
            Some("turn-step"),
            ActiveExecutionTurnItem {
                item_id: "stream".into(),
                item_seq: 0,
                kind: "assistant_stream".into(),
                status: "running".into(),
                source: "orchestrator".into(),
                title: None,
                content: Some("partial".into()),
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
                metadata: HashMap::new(),
                timeline_entry_id: None,
                source_thread_id: ThreadId::new("thread-step"),
            },
        )
        .unwrap();
    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    assert_eq!(
        restored
            .canonical_turn_for_session_turn_id(&session, "turn-step")
            .unwrap()
            .model_steps,
        vec![step.clone()]
    );
    assert!(
        restored
            .begin_model_step(&session, "turn-step", step)
            .is_err(),
        "Started cannot authorize replay after a crash"
    );
    restored
        .settle_model_step(
            &session,
            "turn-step",
            "call-1",
            Some("b".repeat(64)),
            None,
            None,
        )
        .unwrap();
    let settled = restored
        .canonical_turn_for_session_turn_id(&session, "turn-step")
        .unwrap()
        .model_steps
        .remove(0);
    assert_eq!(settled.status, crate::ModelStepStatus::ResponseReceived);
    assert!(
        restored
            .settle_model_step(&session, "turn-step", "call-1", None, None, None,)
            .is_err()
    );
}

#[test]
fn executor_failure_settles_with_original_step_and_survives_terminal_turn_reload() {
    use magi_core::{
        ExecutorCommandEvidence, ExecutorCommandStatus, ExecutorFailureEvidence, ExecutorReceipt,
    };
    for terminal in [false, true] {
        let store = SessionStore::new();
        let session = SessionId::new(format!("executor-failure-{terminal}"));
        let task = magi_core::TaskId::new("executor-task");
        store.create_session(session.clone(), "executor").unwrap();
        accept_test_turn(&store, &session, test_turn("executor-turn", "running", 10));
        let mut step = test_model_step("executor-call", vec![]);
        step.task_id = Some(task.clone());
        let caps: magi_app_server_protocol::ExecutorCapabilities =
            serde_json::from_value(serde_json::json!({
                "inputModalities":["text"],"steer":"none","resume":"none","tools":"executor_owned",
                "approvals":"unavailable","cancellation":"settled","context":"executor_owned",
                "structuredOutput":false,"background":false,"inspection":true,"outputLimit":false
            }))
            .unwrap();
        step.executor_capabilities = Some(caps);
        store
            .begin_model_step(&session, "executor-turn", step.clone())
            .unwrap();
        if terminal {
            store
                .update_current_turn_status_for_turn_with_change(
                    &session,
                    Some("executor-turn"),
                    "cancelled",
                )
                .unwrap();
            accept_test_turn(
                &store,
                &session,
                test_turn("next-current-turn", "running", 20),
            );
        }
        let evidence = ExecutorFailureEvidence {
            receipts: vec![ExecutorReceipt {
                executor: "fixture".into(),
                execution_id: "external-original".into(),
                commands: vec![ExecutorCommandEvidence {
                    command: "inspect".into(),
                    exit_code: None,
                    status: ExecutorCommandStatus::OutcomeUnknown,
                    output: "partial observation".into(),
                }],
            }],
            usage: Some(serde_json::json!({"input_tokens":3,"output_tokens":2})),
            items: vec![serde_json::json!({"type":"command_execution","command":"inspect"})],
            resources_settled: true,
        };
        let before = store_mutation_snapshot(&store);
        reject_canonical_writes(&store);
        assert!(
            store
                .settle_model_step(
                    &session,
                    "executor-turn",
                    "executor-call",
                    None,
                    Some(5),
                    Some(&evidence),
                )
                .is_err()
        );
        assert_store_mutation_snapshot_unchanged(&store, &before);
        let writer = Arc::new(RecordingCanonicalWriter::default());
        store.install_session_event_writer(writer.clone());
        store
            .settle_model_step(
                &session,
                "executor-turn",
                "executor-call",
                None,
                Some(5),
                Some(&evidence),
            )
            .unwrap();
        {
            let transactions = writer.transactions.lock().unwrap();
            assert_eq!(transactions.len(), 1);
            assert_eq!(transactions[0].len(), 1);
            let next = &transactions[0][0].next;
            assert_eq!(
                next.model_steps[0].status,
                crate::ModelStepStatus::OutcomeUnknown
            );
            assert!(
                next.items
                    .iter()
                    .any(|item| item.metadata.contains_key("executorFailure"))
            );
        }
        let restored = SessionStore::from_persisted_parts(
            store.durable_state(),
            store.execution_sidecar_store_state(),
        )
        .unwrap();
        assert_eq!(
            restored.executor_failures_for_task(&session, &task),
            vec![("executor-call".into(), evidence.clone())]
        );
        let turn = restored
            .canonical_turn_for_session_turn_id(&session, "executor-turn")
            .unwrap();
        assert_eq!(turn.model_steps.len(), 1);
        assert_eq!(
            turn.model_steps[0].status,
            crate::ModelStepStatus::OutcomeUnknown
        );
        assert_eq!(turn.model_steps[0].usage_tokens, Some(5));
        assert_eq!(turn.status.is_terminal(), terminal);
        if terminal {
            assert_eq!(
                restored
                    .runtime_sidecar(&session)
                    .unwrap()
                    .current_turn
                    .unwrap()
                    .turn_id,
                "next-current-turn"
            );
            assert!(
                restored
                    .canonical_turn_for_session_turn_id(&session, "next-current-turn")
                    .unwrap()
                    .model_steps
                    .is_empty()
            );
        }
        assert!(
            restored
                .begin_model_step(&session, "executor-turn", step)
                .is_err()
        );
        assert!(
            restored
                .settle_model_step(
                    &session,
                    "executor-turn",
                    "executor-call",
                    None,
                    Some(5),
                    Some(&evidence)
                )
                .is_err()
        );
        assert!(
            restored
                .executor_failures_for_task(&SessionId::new("other-session"), &task)
                .is_empty()
        );
        let mut tampered = turn.clone();
        let failure = tampered
            .items
            .iter_mut()
            .find(|item| item.metadata.contains_key("executorFailure"))
            .unwrap();
        failure.source_thread_id = ThreadId::new("wrong-thread");
        assert!(tampered.validate_model_steps().is_err());
        if !terminal {
            restored
                .update_current_turn_status_for_turn_with_change(
                    &session,
                    Some("executor-turn"),
                    "failed",
                )
                .unwrap();
            let settled_turn = restored
                .canonical_turn_for_session_turn_id(&session, "executor-turn")
                .unwrap();
            let failure = settled_turn
                .items
                .iter()
                .find(|item| item.metadata.contains_key("executorFailure"))
                .unwrap();
            assert!(!failure.visibility.renderable);
            assert_eq!(
                restored.executor_failures_for_task(&session, &task),
                vec![("executor-call".into(), evidence)]
            );
        }
    }
}

#[test]
fn model_step_and_input_consumption_commit_atomically_and_reject_late_results() {
    let store = SessionStore::new();
    let session = SessionId::new("session-step-input");
    store.create_session(session.clone(), "step input").unwrap();
    let mut turn = test_turn("turn-step-input", "running", 10);
    let mut item = crate::ActiveExecutionTurnItem {
        item_id: "user-step".into(),
        item_seq: 0,
        kind: "user_message".into(),
        status: "completed".into(),
        source: "user".into(),
        title: None,
        content: Some("work".into()),
        task_id: None,
        worker_id: None,
        role_id: None,
        tool_call_id: None,
        tool_name: None,
        tool_status: None,
        tool_arguments: None,
        tool_result: None,
        tool_error: None,
        request_id: Some("request-step".into()),
        user_message_id: Some("user-step".into()),
        placeholder_message_id: None,
        metadata: HashMap::new(),
        timeline_entry_id: None,
        source_thread_id: ThreadId::new("thread-step"),
    };
    item.metadata
        .insert("requestId".into(), json!("request-step"));
    item.metadata
        .insert("userMessageId".into(), json!("user-step"));
    item.metadata
        .insert("requestFingerprint".into(), json!("original"));
    turn.items.push(item);
    accept_test_turn(&store, &session, turn);
    let step = test_model_step("call-input", vec!["request-step".into()]);
    let observed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let notices = observed.clone();
    store.install_model_step_commit_observer(Arc::new(move |_| {
        notices.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    let before = store_mutation_snapshot(&store);
    reject_canonical_writes(&store);
    assert!(
        store
            .begin_model_step(&session, "turn-step-input", step.clone())
            .is_err()
    );
    assert_store_mutation_snapshot_unchanged(&store, &before);
    assert_eq!(observed.load(std::sync::atomic::Ordering::SeqCst), 0);
    let store = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    let read_store = store.clone();
    let notices = observed.clone();
    store.install_model_step_commit_observer(Arc::new(move |notice| {
        assert_eq!(notice.turn.turn_id, "turn-step-input");
        assert!(notice.step.goal_id.is_none(), "普通任务也发布消费回执");
        assert_eq!(
            read_store.input_for_request("request-step").unwrap().state,
            crate::SessionInputState::Consumed
        );
        read_store
            .persist_projection_with(|_, _| Ok::<_, ()>(()))
            .unwrap();
        notices.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    store
        .begin_model_step(&session, "turn-step-input", step)
        .unwrap();
    let input = store.input_for_request("request-step").unwrap();
    assert_eq!(input.state, crate::SessionInputState::Consumed);
    assert_eq!(input.consumption.unwrap().step_id, "call-input");
    assert_eq!(observed.load(std::sync::atomic::Ordering::SeqCst), 1);
    store
        .update_current_turn_status_for_turn_with_change(
            &session,
            Some("turn-step-input"),
            "cancelled",
        )
        .unwrap();
    store
        .settle_model_step(
            &session,
            "turn-step-input",
            "call-input",
            Some("b".repeat(64)),
            None,
            None,
        )
        .unwrap();
    assert_eq!(observed.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(
        store
            .begin_model_step(&session, "turn-step-input", test_model_step("late", vec![]))
            .is_err()
    );
}

#[test]
fn goal_step_accounting_is_shared_durable_and_resumes_without_resetting_usage() {
    let store = SessionStore::new();
    let session = SessionId::new("session-shared-goal-budget");
    store
        .create_session(session.clone(), "shared budget")
        .unwrap();
    accept_test_turn(&store, &session, test_turn("turn-shared", "running", 10));
    let goal = create_test_goal(&store, &session, "turn-shared", "shared");
    let before_response = store.durable_state();
    let mut child = test_model_step("child", vec![]);
    child.task_id = Some(TaskId::new("child-task"));
    child.thread_id = ThreadId::new("child-thread");
    for step in [test_model_step("parent", vec![]), child] {
        store
            .begin_model_step(&session, "turn-shared", step)
            .unwrap();
    }
    store
        .settle_model_step(
            &session,
            "turn-shared",
            "parent",
            Some("b".repeat(64)),
            Some(10),
            None,
        )
        .unwrap();
    assert_eq!(
        store.current_goal(&session).unwrap().status,
        GoalStatus::Active
    );
    // 用量只做统计；父子请求继续执行，不触发预算暂停。
    store
        .settle_model_step(
            &session,
            "turn-shared",
            "child",
            Some("c".repeat(64)),
            Some(7),
            None,
        )
        .unwrap();
    assert!(
        store
            .begin_model_step(&session, "turn-shared", test_model_step("next", vec![]))
            .is_ok()
    );
    let mut replayed = before_response;
    replayed.canonical_turns = store.durable_state().canonical_turns;
    let restored =
        SessionStore::from_persisted_parts(replayed, store.execution_sidecar_store_state())
            .unwrap();
    let limited = restored.current_goal(&session).unwrap();
    assert_eq!(limited.goal_id, goal.goal_id);
    assert_eq!(limited.status, GoalStatus::Active);
    assert_eq!(limited.tokens_used, 17);
    assert!(
        restored
            .settle_model_step(
                &session,
                "turn-shared",
                "parent",
                Some("b".repeat(64)),
                Some(10),
                None,
            )
            .is_err()
    );
    let restored = SessionStore::from_persisted_parts(
        restored.durable_state(),
        restored.execution_sidecar_store_state(),
    )
    .unwrap();
    assert_eq!(restored.current_goal(&session).unwrap().tokens_used, 17);
    assert!(
        restored
            .resume_goal_with_plan(
                &session,
                &goal.goal_id,
                limited.control_revision,
                None,
                None
            )
            .is_err()
    );
    let paused = restored
        .pause_goal_with_plan(&session, &goal.goal_id, limited.control_revision, None)
        .unwrap()
        .0;
    assert!(!restored.turn_execution_is_active(&session, "turn-shared"));
    restored
        .resume_goal_with_plan(&session, &goal.goal_id, paused.control_revision, None, None)
        .unwrap();
    assert!(restored.turn_execution_is_active(&session, "turn-shared"));
    restored
        .begin_model_step(&session, "turn-shared", test_model_step("resumed", vec![]))
        .unwrap();
    restored
        .settle_model_step(
            &session,
            "turn-shared",
            "resumed",
            Some("d".repeat(64)),
            Some(4),
            None,
        )
        .unwrap();
    assert_eq!(restored.current_goal(&session).unwrap().tokens_used, 21);
}

#[test]
fn failed_goal_step_settlement_cannot_publish_usage_or_release_result() {
    let store = SessionStore::new();
    let session = SessionId::new("session-goal-budget-write-failure");
    store
        .create_session(session.clone(), "budget failure")
        .unwrap();
    accept_test_turn(
        &store,
        &session,
        test_turn("turn-budget-failure", "running", 10),
    );
    create_test_goal(&store, &session, "turn-budget-failure", "budget");
    store
        .begin_model_step(
            &session,
            "turn-budget-failure",
            test_model_step("call", vec![]),
        )
        .unwrap();
    let notices = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = notices.clone();
    store.install_model_step_commit_observer(Arc::new(move |_| {
        observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    reject_canonical_writes(&store);
    assert!(
        store
            .settle_model_step(
                &session,
                "turn-budget-failure",
                "call",
                Some("b".repeat(64)),
                Some(11),
                None,
            )
            .is_err()
    );
    assert_eq!(store.current_goal(&session).unwrap().tokens_used, 0);
    assert_eq!(notices.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        store
            .canonical_turn_for_session_turn_id(&session, "turn-budget-failure")
            .unwrap()
            .model_steps[0]
            .status,
        crate::ModelStepStatus::Started
    );
}

#[test]
fn parallel_goal_settlements_serialize_watermarks_and_keep_legacy_usage_base() {
    let store = SessionStore::new();
    let session = SessionId::new("session-parallel-budget");
    store.create_session(session.clone(), "parallel").unwrap();
    accept_test_turn(&store, &session, test_turn("turn-parallel", "running", 10));
    let goal = create_test_goal(&store, &session, "turn-parallel", "parallel");
    // layout 4 迁移保留此前已经结算的用量基数。
    store.state.write().unwrap().goals[0].tokens_used = 37;
    for index in 0..8 {
        store
            .begin_model_step(
                &session,
                "turn-parallel",
                test_model_step(&format!("parallel-{index}"), vec![]),
            )
            .unwrap();
    }
    let gate = Arc::new(std::sync::Barrier::new(8));
    std::thread::scope(|scope| {
        for index in 0..8 {
            let gate = gate.clone();
            let store = &store;
            let session = &session;
            scope.spawn(move || {
                gate.wait();
                store
                    .settle_model_step(
                        session,
                        "turn-parallel",
                        &format!("parallel-{index}"),
                        Some("b".repeat(64)),
                        Some(11),
                        None,
                    )
                    .unwrap();
            });
        }
    });
    assert_eq!(store.current_goal(&session).unwrap().tokens_used, 125);
    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    let limited = restored.current_goal(&session).unwrap();
    assert_eq!(limited.goal_id, goal.goal_id);
    assert_eq!(limited.tokens_used, 125);
    assert_eq!(limited.status, GoalStatus::Active);
}

#[test]
fn late_step_usage_cannot_charge_a_replacement_goal() {
    let store = SessionStore::new();
    let session = SessionId::new("session-replaced-budget");
    store
        .create_session(session.clone(), "replaced budget")
        .unwrap();
    accept_test_turn(&store, &session, test_turn("turn-old-goal", "running", 10));
    let first = create_test_goal(&store, &session, "turn-old-goal", "first");
    store
        .begin_model_step(
            &session,
            "turn-old-goal",
            test_model_step("late-old", vec![]),
        )
        .unwrap();
    store
        .complete_goal(
            &session,
            &first.goal_id,
            crate::GoalRevisionExpectation::new(first.control_revision, None),
            "turn-old-goal",
            "done",
            vec![],
        )
        .unwrap();
    store
        .update_current_turn_status_for_turn_with_change(
            &session,
            Some("turn-old-goal"),
            "completed",
        )
        .unwrap();
    accept_test_turn(&store, &session, test_turn("turn-new-goal", "running", 20));
    let second = create_test_goal(&store, &session, "turn-new-goal", "second");
    store
        .begin_model_step(
            &session,
            "turn-new-goal",
            test_model_step("new-call", vec![]),
        )
        .unwrap();
    store
        .settle_model_step(
            &session,
            "turn-new-goal",
            "new-call",
            Some("b".repeat(64)),
            Some(3),
            None,
        )
        .unwrap();
    store
        .settle_model_step(
            &session,
            "turn-old-goal",
            "late-old",
            Some("c".repeat(64)),
            Some(11),
            None,
        )
        .unwrap();
    let current = store.current_goal(&session).unwrap();
    assert_eq!(current.goal_id, second.goal_id);
    assert_eq!(current.status, GoalStatus::Active);
    assert_eq!(current.tokens_used, 3);
    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    assert_eq!(restored.current_goal(&session).unwrap().tokens_used, 3);
}

#[test]
fn goal_usage_notification_follows_durable_commit_and_allows_reentrant_reads() {
    let store = SessionStore::new();
    let session = SessionId::new("session-budget-notice");
    store.create_session(session.clone(), "notice").unwrap();
    accept_test_turn(
        &store,
        &session,
        test_turn("turn-budget-notice", "running", 10),
    );
    create_test_goal(&store, &session, "turn-budget-notice", "notice");
    store
        .begin_model_step(
            &session,
            "turn-budget-notice",
            test_model_step("notice", vec![]),
        )
        .unwrap();
    let read_store = store.clone();
    let notices = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = notices.clone();
    store.install_model_step_commit_observer(Arc::new(move |notice| {
        let goal = read_store.current_goal(&notice.session_id).unwrap();
        assert_eq!(goal.tokens_used, 11);
        assert_eq!(Some(goal.goal_id.clone()), notice.step.goal_id);
        assert_eq!(notice.step.step_id, "notice");
        assert_eq!(goal.status, GoalStatus::Active);
        // 检查点获取写屏障；通知不能持有 canonical 提交锁。
        read_store
            .persist_projection_with(|_, _| Ok::<_, ()>(()))
            .unwrap();
        observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    store
        .settle_model_step(
            &session,
            "turn-budget-notice",
            "notice",
            Some("b".repeat(64)),
            Some(11),
            None,
        )
        .unwrap();
    assert_eq!(notices.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn retired_task_budget_fields_restore_as_paused_without_losing_usage() {
    let store = SessionStore::new();
    let session = SessionId::new("retired-task-budget");
    store
        .create_session(session.clone(), "legacy goal")
        .unwrap();
    let goal = create_test_goal(&store, &session, "legacy-turn", "finish original objective");
    for (retired_status, retired_reason) in [
        ("budget_limited", "token_budget_exhausted"),
        ("usage_limited", "system_usage_limit"),
    ] {
        let mut durable = serde_json::to_value(store.durable_state()).unwrap();
        let old = &mut durable["goals"][0];
        old["status"] = json!(retired_status);
        old["tokensUsed"] = json!(1234);
        old["tokenBudget"] = json!(1000);
        old["tokensReserved"] = json!(500);
        old["budgetRequiredTokens"] = json!(600);
        old["budgetActiveMillis"] = json!(9999);
        old["runBudget"] = json!({"maxModelAttempts":1,"maxActiveSeconds":1,"maxOutputTokens":1,"maxActiveAgents":1});
        old["continuation"] = json!({"phase":"waiting","reason":retired_reason});
        let decoded: crate::SessionDurableState = serde_json::from_value(durable).unwrap();
        let restored =
            SessionStore::from_persisted_parts(decoded, store.execution_sidecar_store_state())
                .unwrap();
        let paused = restored.current_goal(&session).unwrap();
        assert_eq!(paused.status, GoalStatus::Paused);
        assert_eq!(
            serde_json::to_value(paused.status).unwrap(),
            json!("paused")
        );
        assert_eq!(paused.tokens_used, 1234);
        assert_eq!(paused.goal_id, goal.goal_id);
        assert_eq!(paused.objective, goal.objective);
        assert!(paused.continuation.reason.is_none());
        let resumed = restored
            .resume_goal_with_plan(&session, &goal.goal_id, paused.control_revision, None, None)
            .unwrap()
            .0;
        assert_eq!(resumed.status, GoalStatus::Active);
        assert_eq!(resumed.tokens_used, 1234);
        let mut current = serde_json::to_value(&resumed).unwrap();
        for field in [
            "tokenBudget",
            "tokensReserved",
            "budgetRequiredTokens",
            "budgetActiveMillis",
            "runBudget",
        ] {
            assert!(current.get(field).is_none(), "{field}");
        }
        current["unexpectedField"] = json!(true);
        assert!(serde_json::from_value::<crate::SessionGoal>(current).is_err());
    }
}

#[test]
fn retired_step_reservation_is_read_only_and_preserves_request_identity() {
    let step = test_model_step("legacy-step", vec![]);
    let mut old = serde_json::to_value(&step).unwrap();
    old["reservationTokens"] = json!(10000);
    let restored: crate::ModelStep = serde_json::from_value(old).unwrap();
    restored.validate_snapshot().unwrap();
    assert_eq!(restored, step);
    let mut current = serde_json::to_value(&restored).unwrap();
    assert!(current.get("reservationTokens").is_none());
    current["unexpectedField"] = json!(true);
    assert!(serde_json::from_value::<crate::ModelStep>(current).is_err());
}

#[test]
fn shared_transport_safety_limit_blocks_new_steps_and_retries_after_restart() {
    let store = SessionStore::new();
    let session = SessionId::new("transport-safety");
    store.create_session(session.clone(), "safety").unwrap();
    accept_test_turn(&store, &session, test_turn("safety-turn", "running", 10));
    let mut durable = store.durable_state();
    let mut step = test_model_step("safety-step", vec![]);
    step.attempts = (1..=10_000)
        .map(|index| crate::ModelAttempt {
            attempt_id: format!("safety-step:attempt:{index}"),
            started_at: UtcMillis(10),
            settled_at: (index < 10_000).then_some(UtcMillis(10)),
            status: if index < 10_000 {
                crate::ModelStepStatus::OutcomeUnknown
            } else {
                crate::ModelStepStatus::Started
            },
        })
        .collect();
    durable.canonical_turns[0].model_steps.push(step);
    let restored =
        SessionStore::from_persisted_parts(durable, store.execution_sidecar_store_state()).unwrap();
    assert!(matches!(
        restored.begin_model_step(&session, "safety-turn", test_model_step("new", vec![])),
        Err(magi_core::DomainError::RunSafetyLimit { .. })
    ));
    assert!(matches!(
        restored.start_model_step_retry(&session, "safety-turn", "safety-step"),
        Err(magi_core::DomainError::RunSafetyLimit { .. })
    ));
    assert_eq!(
        restored
            .canonical_turn_for_session_turn_id(&session, "safety-turn")
            .unwrap()
            .model_steps[0]
            .attempts
            .len(),
        10_000
    );
}

fn user_question_fixture() -> (SessionStore, crate::user_question::PendingUserQuestion) {
    let store = SessionStore::new();
    let session = SessionId::new("durable-question-session");
    store.create_session(session.clone(), "交互恢复").unwrap();
    let (_, thread) = store.ensure_session_mission(&session, UtcMillis(10), || {
        MissionId::new("durable-question-mission")
    });
    let mut turn = test_turn("durable-question-turn", "running", 10);
    let mut item = test_turn_item("durable-question-item", "");
    item.kind = "tool_call_started".into();
    item.status = "running".into();
    item.source_thread_id = thread;
    item.task_id = Some(TaskId::new("durable-question-task"));
    item.tool_name = Some("ask_user_question".into());
    item.tool_call_id = Some("durable-question-call".into());
    item.tool_result =
        Some(json!({"tool": "ask_user_question", "status": "awaiting_user_input"}).to_string());
    turn.items.push(item);
    accept_test_turn(&store, &session, turn.clone());
    store
        .upsert_active_execution_chain(
            session.clone(),
            test_active_chain(&session, "question-chain", Some(turn)),
        )
        .unwrap();
    let request = crate::user_question::PendingUserQuestion {
        question_id: "durable-question".into(), session_id: session,
        task_id: Some(TaskId::new("durable-question-task")), turn_id: "durable-question-turn".into(),
        tool_call_id: "durable-question-call".into(), requested_at: UtcMillis(10), can_skip: true,
        questions: crate::user_question::parse_questions(&json!({"questions": [{
            "header": "方案", "question": "选哪个？", "options": [{"label": "A"}, {"label": "B"}]
        }]}).to_string()).unwrap(),
    };
    (store, request)
}

#[test]
fn user_question_survives_restart_and_response_precedes_tool_completion() {
    let (store, request) = user_question_fixture();
    store.register_user_question(request.clone()).unwrap();
    let stale_sidecar = store.runtime_sidecar(&request.session_id).unwrap();
    // 工具进度的旧写回不能擦掉持久交互。
    store
        .upsert_active_execution_chain(
            request.session_id.clone(),
            test_active_chain(
                &request.session_id,
                "question-chain",
                stale_sidecar.current_turn,
            ),
        )
        .unwrap();
    assert_eq!(
        store.pending_user_questions(&request.session_id).unwrap(),
        vec![request.clone()]
    );
    store
        .interrupt_current_turn_by_daemon_restart(&request.session_id)
        .unwrap();
    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    assert_eq!(
        restored
            .pending_user_questions(&request.session_id)
            .unwrap(),
        vec![request.clone()]
    );
    let response = crate::user_question::UserQuestionResponse::Answered {
        answers: vec![crate::user_question::UserQuestionAnswer {
            selected: vec!["B".into()],
            other: None,
        }],
    };
    let committed = restored
        .resolve_user_question(&request.session_id, &request.question_id, response.clone())
        .unwrap();
    assert_eq!(committed.response, Some(response));
    let canonical = restored
        .canonical_turn_for_session_turn_id(&request.session_id, &request.turn_id)
        .unwrap();
    assert_eq!(
        canonical
            .items
            .iter()
            .find(|item| item.item_id == "durable-question-item")
            .unwrap()
            .tool
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap()["status"],
        "answered"
    );
    let thread = restored.thread_registry_snapshot(&request.session_id)[0]
        .thread_id
        .clone();
    restored
        .rebuild_thread_message_projection(&thread, UtcMillis::now())
        .unwrap();
    assert!(
        restored
            .thread_message_history(&thread)
            .iter()
            .any(|message| message.role == "tool"
                && message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("B")))
    );
    assert_eq!(
        committed.result_payload().unwrap()["answers"][0]["answer"],
        json!(["B"])
    );
    assert!(
        restored
            .pending_user_questions(&request.session_id)
            .unwrap()
            .is_empty()
    );
    assert!(
        restored
            .resolve_user_question(
                &request.session_id,
                &request.question_id,
                crate::user_question::UserQuestionResponse::Skipped
            )
            .is_err()
    );
    let reloaded = SessionStore::from_persisted_parts(
        restored.durable_state(),
        restored.execution_sidecar_store_state(),
    )
    .unwrap();
    assert_eq!(
        reloaded
            .user_question_state(&request.session_id, &request.question_id)
            .unwrap(),
        Some(committed)
    );
}

#[test]
fn user_question_registration_write_failure_is_atomic() {
    let (store, request) = user_question_fixture();
    let before = store_mutation_snapshot(&store);
    reject_canonical_writes(&store);
    assert!(store.register_user_question(request.clone()).is_err());
    assert_store_mutation_snapshot_unchanged(&store, &before);
}

#[test]
fn user_question_response_write_failure_is_atomic() {
    let (store, request) = user_question_fixture();
    store.register_user_question(request.clone()).unwrap();
    let before = store_mutation_snapshot(&store);
    reject_canonical_writes(&store);
    assert!(
        store
            .resolve_user_question(
                &request.session_id,
                &request.question_id,
                crate::user_question::UserQuestionResponse::Skipped
            )
            .is_err()
    );
    assert_store_mutation_snapshot_unchanged(&store, &before);
    assert_eq!(
        store.pending_user_questions(&request.session_id).unwrap(),
        vec![request]
    );
}

#[test]
fn user_question_cannot_be_answered_after_cancellation_or_recovery_claim() {
    for claim in [false, true] {
        let (store, request) = user_question_fixture();
        store.register_user_question(request.clone()).unwrap();
        assert!(
            store
                .resolve_user_question(
                    &SessionId::new("other-session"),
                    &request.question_id,
                    crate::user_question::UserQuestionResponse::Skipped
                )
                .is_err()
        );
        if claim {
            store
                .interrupt_current_turn_by_daemon_restart(&request.session_id)
                .unwrap();
            assert!(
                store
                    .claim_interrupted_recovery(&request.session_id)
                    .is_err()
            );
            store
                .resolve_user_question(
                    &request.session_id,
                    &request.question_id,
                    crate::user_question::UserQuestionResponse::Skipped,
                )
                .unwrap();
            store
                .claim_interrupted_recovery(&request.session_id)
                .unwrap();
        } else {
            store
                .update_current_turn_status_for_turn_with_change(
                    &request.session_id,
                    Some(&request.turn_id),
                    "cancelled",
                )
                .unwrap();
        }
        assert!(
            store
                .pending_user_questions(&request.session_id)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .resolve_user_question(
                    &request.session_id,
                    &request.question_id,
                    crate::user_question::UserQuestionResponse::Skipped
                )
                .is_err()
        );
    }
}

#[test]
fn cancelled_user_question_does_not_resurrect_after_restart_or_mutate_late_tool_result() {
    let (store, request) = user_question_fixture();
    store.register_user_question(request.clone()).unwrap();
    let stale = store
        .runtime_sidecar(&request.session_id)
        .unwrap()
        .current_turn
        .unwrap();
    store
        .cancel_user_question(&request.session_id, &request.question_id)
        .unwrap();
    store
        .upsert_active_execution_chain(
            request.session_id.clone(),
            test_active_chain(&request.session_id, "question-chain", Some(stale)),
        )
        .unwrap();
    store
        .interrupt_current_turn_by_daemon_restart(&request.session_id)
        .unwrap();
    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    assert!(
        restored
            .pending_user_questions(&request.session_id)
            .unwrap()
            .is_empty()
    );
    assert!(
        restored
            .user_question_state(&request.session_id, &request.question_id)
            .unwrap()
            .unwrap()
            .cancelled_at
            .is_some()
    );
    assert!(
        restored
            .resolve_user_question(
                &request.session_id,
                &request.question_id,
                crate::user_question::UserQuestionResponse::Skipped
            )
            .is_err()
    );
}

fn durable_approval_fixture() -> (
    SessionStore,
    crate::tool_approval::PendingToolApproval,
    std::path::PathBuf,
) {
    let store = SessionStore::new();
    let session = SessionId::new("durable-approval-session");
    store.create_session(session.clone(), "审批恢复").unwrap();
    let now = UtcMillis::now();
    let mut turn = test_turn("durable-approval-turn", "running", now.0);
    let mut item = test_turn_item("durable-approval-item", "");
    item.kind = "tool_call_started".into();
    item.status = "running".into();
    item.task_id = Some(TaskId::new("durable-approval-task"));
    item.tool_call_id = Some("durable-approval-call".into());
    item.tool_name = Some("shell_exec".into());
    item.tool_arguments = Some(r#"{"command":"publish","background":false}"#.into());
    turn.items.push(item);
    accept_test_turn(&store, &session, turn.clone());
    store
        .upsert_active_execution_chain(
            session.clone(),
            test_active_chain(&session, "durable-approval-chain", Some(turn)),
        )
        .unwrap();
    let request = crate::tool_approval::PendingToolApproval {
        approval_id: "durable-approval".into(),
        session_id: session,
        task_id: TaskId::new("durable-approval-task"),
        turn_id: "durable-approval-turn".into(),
        tool_call_id: "durable-approval-call".into(),
        tool_name: "shell_exec".into(),
        reason: "发布需要审批".into(),
        requested_at: now,
        agent: None,
    };
    (store, request, std::env::temp_dir())
}

fn accept_approval_recovery_turn(store: &SessionStore, session: &SessionId) {
    let mut turn = test_turn("resumed-approval-turn", "running", UtcMillis::now().0 + 1);
    let mut item = test_turn_item("resumed-approval-item", "");
    item.kind = "tool_call_started".into();
    item.status = "running".into();
    item.task_id = Some(TaskId::new("durable-approval-task"));
    item.tool_call_id = Some("resumed-approval-call".into());
    item.tool_name = Some("shell_exec".into());
    item.tool_arguments = Some(r#"{"background":false,"command":"publish"}"#.into());
    turn.items.push(item);
    accept_test_turn(store, session, turn);
}

#[test]
fn durable_approval_restores_waiting_and_claims_only_original_operation_once() {
    use crate::tool_approval::ToolApprovalDecision;
    let (store, request, root) = durable_approval_fixture();
    let policy = json!({"scope":"original"});
    store
        .register_tool_approval(request.clone(), Some(root.clone()), policy.clone(), None)
        .unwrap();
    store
        .interrupt_current_turn_by_daemon_restart(&request.session_id)
        .unwrap();
    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    assert_eq!(
        restored
            .pending_tool_approvals(&request.session_id)
            .unwrap(),
        vec![request.clone()]
    );
    assert!(
        restored
            .claim_interrupted_recovery(&request.session_id)
            .is_err()
    );
    restored
        .resolve_tool_approvals(
            &request.session_id,
            std::slice::from_ref(&request.approval_id),
            ToolApprovalDecision::AllowOnce,
        )
        .unwrap();
    assert!(
        restored
            .resolve_tool_approvals(
                &request.session_id,
                std::slice::from_ref(&request.approval_id),
                ToolApprovalDecision::AllowOnce
            )
            .is_err()
    );
    restored
        .claim_interrupted_recovery(&request.session_id)
        .unwrap();
    accept_approval_recovery_turn(&restored, &request.session_id);
    let claim = |path: &std::path::Path, policy: &Value, sources: &[TaskId], arguments: &str| {
        restored
            .claim_tool_approval(
                &request.session_id,
                &request.task_id,
                "resumed-approval-call",
                "shell_exec",
                arguments,
                Some(path),
                policy,
                sources,
                None,
            )
            .map(|receipt| receipt.map(|receipt| receipt.decision))
    };
    assert_eq!(
        claim(
            &root,
            &policy,
            &[TaskId::new("sibling")],
            r#"{"command":"publish","background":false}"#
        )
        .unwrap(),
        None
    );
    assert_eq!(
        claim(
            &root,
            &json!({"scope":"changed"}),
            std::slice::from_ref(&request.task_id),
            r#"{"command":"publish","background":false}"#
        )
        .unwrap(),
        None
    );
    assert_eq!(
        claim(
            &root.join("other"),
            &policy,
            std::slice::from_ref(&request.task_id),
            r#"{"command":"publish","background":false}"#
        )
        .unwrap(),
        None
    );
    assert!(
        claim(
            &root,
            &policy,
            std::slice::from_ref(&request.task_id),
            r#"{"command":"delete","background":false}"#
        )
        .is_err()
    );
    assert_eq!(
        claim(
            &root,
            &policy,
            std::slice::from_ref(&request.task_id),
            r#"{ "background":false, "command":"publish" }"#
        )
        .unwrap(),
        Some(ToolApprovalDecision::AllowOnce)
    );
    assert_eq!(
        claim(
            &root,
            &policy,
            std::slice::from_ref(&request.task_id),
            r#"{"command":"publish","background":false}"#
        )
        .unwrap(),
        None
    );
    let restored = SessionStore::from_persisted_parts(
        restored.durable_state(),
        restored.execution_sidecar_store_state(),
    )
    .unwrap();
    let source = restored
        .canonical_turn_for_session_turn_id(&request.session_id, &request.turn_id)
        .unwrap();
    let approval = source.items[0].tool_approval().unwrap().unwrap();
    assert_eq!(approval.execution.unwrap().item_id, "resumed-approval-item");
    let mut broken = restored.durable_state();
    broken
        .canonical_turns
        .iter_mut()
        .find(|turn| turn.turn_id == "resumed-approval-turn")
        .unwrap()
        .items[0]
        .metadata
        .remove("approvalSource");
    assert!(
        SessionStore::from_persisted_parts(broken, restored.execution_sidecar_store_state())
            .is_err()
    );
    let target = restored
        .canonical_turn_for_session_turn_id(&request.session_id, "resumed-approval-turn")
        .unwrap();
    assert_eq!(
        target.items[0]
            .tool
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap()["status"],
        "running"
    );
}

#[test]
fn durable_approval_decision_and_execution_claim_fail_atomically() {
    use crate::tool_approval::ToolApprovalDecision;
    for fail_on_claim in [false, true] {
        let (store, request, root) = durable_approval_fixture();
        let policy = json!({"scope":"original"});
        store
            .register_tool_approval(request.clone(), Some(root.clone()), policy.clone(), None)
            .unwrap();
        if fail_on_claim {
            store
                .resolve_tool_approvals(
                    &request.session_id,
                    std::slice::from_ref(&request.approval_id),
                    ToolApprovalDecision::AllowOnce,
                )
                .unwrap();
        }
        let before = store_mutation_snapshot(&store);
        reject_canonical_writes(&store);
        let failed = if fail_on_claim {
            store
                .claim_tool_approval(
                    &request.session_id,
                    &request.task_id,
                    &request.tool_call_id,
                    "shell_exec",
                    r#"{"command":"publish","background":false}"#,
                    Some(&root),
                    &policy,
                    std::slice::from_ref(&request.task_id),
                    Some(&request.approval_id),
                )
                .map(|_| ())
        } else {
            store.resolve_tool_approvals(
                &request.session_id,
                std::slice::from_ref(&request.approval_id),
                ToolApprovalDecision::AllowOnce,
            )
        };
        assert!(failed.is_err());
        assert_store_mutation_snapshot_unchanged(&store, &before);
    }
}

#[test]
fn durable_approval_cancellation_expiry_and_refusal_never_grant_execution() {
    use crate::tool_approval::ToolApprovalDecision;
    for mode in ["cancelled", "expired", "denied"] {
        let (store, mut request, root) = durable_approval_fixture();
        if mode == "expired" {
            request.requested_at = UtcMillis(1);
        }
        store
            .register_tool_approval(request.clone(), Some(root.clone()), Value::Null, None)
            .unwrap();
        if mode == "cancelled" {
            store
                .cancel_tool_approval(&request.session_id, &request.approval_id)
                .unwrap();
        }
        if mode == "denied" {
            store
                .resolve_tool_approvals(
                    &request.session_id,
                    std::slice::from_ref(&request.approval_id),
                    ToolApprovalDecision::Deny,
                )
                .unwrap();
        }
        assert!(
            store
                .pending_tool_approvals(&request.session_id)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .resolve_tool_approvals(
                    &request.session_id,
                    std::slice::from_ref(&request.approval_id),
                    ToolApprovalDecision::AllowOnce
                )
                .is_err()
        );
        let result = store
            .claim_tool_approval(
                &request.session_id,
                &request.task_id,
                &request.tool_call_id,
                "shell_exec",
                r#"{"command":"publish","background":false}"#,
                Some(&root),
                &Value::Null,
                std::slice::from_ref(&request.task_id),
                Some(&request.approval_id),
            )
            .unwrap()
            .map(|receipt| receipt.decision);
        assert_eq!(
            result,
            if mode == "denied" {
                Some(ToolApprovalDecision::Deny)
            } else {
                None
            }
        );
    }
}

#[test]
fn approval_allow_for_turn_never_reuses_a_different_execution_root() {
    use crate::tool_approval::ToolApprovalDecision;
    let (store, request, root) = durable_approval_fixture();
    store
        .register_tool_approval(request.clone(), Some(root.clone()), Value::Null, None)
        .unwrap();
    let turn = store
        .runtime_sidecar(&request.session_id)
        .unwrap()
        .current_turn
        .unwrap();
    let mut sibling = turn.items[0].clone();
    sibling.item_id = "approval-other-root-item".into();
    sibling.item_seq = 2;
    sibling.tool_call_id = Some("approval-other-root-call".into());
    sibling.metadata.clear();
    store
        .append_current_turn_item_for_turn(&request.session_id, Some(&request.turn_id), sibling)
        .unwrap();
    let mut other = request.clone();
    other.approval_id = "approval-other-root".into();
    other.tool_call_id = "approval-other-root-call".into();
    store
        .register_tool_approval(other.clone(), Some(root.join("other")), Value::Null, None)
        .unwrap();
    store
        .resolve_tool_approvals(
            &request.session_id,
            std::slice::from_ref(&request.approval_id),
            ToolApprovalDecision::AllowForTurn,
        )
        .unwrap();
    assert_eq!(
        store.pending_tool_approvals(&request.session_id).unwrap(),
        vec![other]
    );
}

#[test]
fn durable_approval_concurrent_claims_choose_one_target() {
    use crate::tool_approval::ToolApprovalDecision;
    let (store, request, root) = durable_approval_fixture();
    store
        .register_tool_approval(request.clone(), Some(root.clone()), Value::Null, None)
        .unwrap();
    store
        .resolve_tool_approvals(
            &request.session_id,
            std::slice::from_ref(&request.approval_id),
            ToolApprovalDecision::AllowOnce,
        )
        .unwrap();
    store
        .interrupt_current_turn_by_daemon_restart(&request.session_id)
        .unwrap();
    accept_approval_recovery_turn(&store, &request.session_id);
    let barrier = std::sync::Barrier::new(3);
    let results = thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..2 {
            handles.push(scope.spawn(|| {
                barrier.wait();
                store
                    .claim_tool_approval(
                        &request.session_id,
                        &request.task_id,
                        "resumed-approval-call",
                        "shell_exec",
                        r#"{"command":"publish","background":false}"#,
                        Some(&root),
                        &Value::Null,
                        std::slice::from_ref(&request.task_id),
                        None,
                    )
                    .unwrap()
                    .is_some()
            }));
        }
        barrier.wait();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.into_iter().filter(|claimed| *claimed).count(), 1);
}

#[test]
fn interrupt_replacement_survives_reload_and_blocks_other_turns_until_atomic_attachment() {
    use crate::{InputInterruption, SessionInput, SessionInputState};
    let store = SessionStore::new();
    let session = SessionId::new("replacement-barrier");
    store
        .create_session(session.clone(), "replacement")
        .unwrap();
    accept_test_turn(&store, &session, test_turn("original", "running", 10));
    let pending = SessionInput {
        input_id: "replacement-input".into(),
        session_id: session.clone(),
        request_id: "replacement-request".into(),
        user_message_id: "replacement-user".into(),
        request_fingerprint: "replacement-fingerprint".into(),
        accepted_at: UtcMillis(20),
        revision: 1,
        state: SessionInputState::Pending,
        attached_turn_id: None,
        consumption: None,
        interruption: Some(InputInterruption {
            turn_id: "original".into(),
            root_task_id: None,
            settled_at: None,
            error: None,
        }),
        payload: json!({"text":"replacement"}),
    };
    let before = store_mutation_snapshot(&store);
    reject_canonical_writes(&store);
    assert!(store.commit_input(pending.clone(), None).is_err());
    assert_store_mutation_snapshot_unchanged(&store, &before);
    let store = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    store.commit_input(pending.clone(), None).unwrap();
    // 崩溃发生在取消前；重载后原始来源与替代输入保持相同身份。
    let store = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    assert_eq!(
        store.input_for_request("replacement-request"),
        Some(pending.clone())
    );
    let mut settled = pending.clone();
    settled.revision = 2;
    settled.interruption.as_mut().unwrap().settled_at = Some(UtcMillis(21));
    assert!(
        store.commit_input(settled.clone(), Some(1)).is_err(),
        "原轮仍运行时不得确认结算"
    );
    store.interrupt_current_turn_by_user(&session).unwrap();
    let before = store_mutation_snapshot(&store);
    assert!(
        store
            .accept_current_turn_with_timeline_entry(
                session.clone(),
                TimelineEntryInput::new(
                    "unrelated",
                    TimelineEntryKind::UserMessage,
                    "unrelated",
                    UtcMillis(30)
                ),
                test_turn("unrelated", "running", 30)
            )
            .is_err()
    );
    assert_store_mutation_snapshot_unchanged(&store, &before);
    reject_canonical_writes(&store);
    assert!(store.commit_input(settled.clone(), Some(1)).is_err());
    assert_store_mutation_snapshot_unchanged(&store, &before);
    // 崩溃发生在原轮结束、结算回执提交前。必须保留屏障直到回执提交。
    let store = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    store.commit_input(settled, Some(1)).unwrap();
    let mut replacement = test_turn("replacement", "running", 30);
    let mut user = test_turn_item("replacement-user", "replacement");
    user.request_id = Some("replacement-request".into());
    user.user_message_id = Some("replacement-user".into());
    user.metadata
        .insert("requestId".into(), json!("replacement-request"));
    user.metadata
        .insert("userMessageId".into(), json!("replacement-user"));
    user.metadata.insert(
        "requestFingerprint".into(),
        json!("replacement-fingerprint"),
    );
    replacement.items.push(user);
    assert!(
        store
            .accept_current_turn_with_timeline_entry(
                session.clone(),
                TimelineEntryInput::new(
                    "unrelated",
                    TimelineEntryKind::UserMessage,
                    "unrelated",
                    UtcMillis(30)
                ),
                test_turn("unrelated", "running", 30)
            )
            .is_err(),
        "完成结算仍必须优先接纳替代输入"
    );
    let before = store_mutation_snapshot(&store);
    reject_canonical_writes(&store);
    assert!(
        store
            .accept_current_turn_with_timeline_entry(
                session.clone(),
                TimelineEntryInput::new(
                    "replacement",
                    TimelineEntryKind::UserMessage,
                    "replacement",
                    UtcMillis(30)
                ),
                replacement.clone()
            )
            .is_err()
    );
    assert_store_mutation_snapshot_unchanged(&store, &before);
    let store = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    accept_test_turn(&store, &session, replacement);
    let attached = store.input_for_request("replacement-request").unwrap();
    assert_eq!(attached.state, SessionInputState::Attached);
    assert_eq!(attached.attached_turn_id.as_deref(), Some("replacement"));
    assert!(store.commit_input(pending, None).is_err());
    assert_eq!(store.canonical_turns_for_session(&session).len(), 2);
}

fn agent_turn_fixture() -> (SessionStore, SessionId, magi_core::Task, ThreadId) {
    let store = SessionStore::new();
    let session = SessionId::new("agent-turn-session");
    store
        .create_session(session.clone(), "agent execution")
        .unwrap();
    let mission = MissionId::new("agent-turn-mission");
    store.ensure_session_mission(&session, UtcMillis(10), || mission.clone());
    let mut parent = test_turn("agent-source-turn", "running", 10);
    let mut item = test_turn_item("agent-source-user", "parent");
    item.task_id = Some(TaskId::new("agent-root-task"));
    item.item_seq = 1;
    parent.items.push(item);
    accept_test_turn(&store, &session, parent);
    let child_id = TaskId::new("agent-child-task");
    let child = magi_core::Task {
        task_id: child_id.clone(),
        mission_id: mission.clone(),
        root_task_id: TaskId::new("agent-root-task"),
        parent_task_id: Some(TaskId::new("agent-root-task")),
        kind: magi_core::TaskKind::LocalAgent,
        title: "child".into(),
        goal: "child goal".into(),
        status: magi_core::TaskStatus::Pending,
        dependency_ids: vec![],
        required_children: vec![],
        policy_snapshot: None,
        executor_binding: Some(magi_core::TaskExecutorBinding {
            agent_activation: Some(magi_core::AgentActivation {
                agent_id: child_id.clone(),
                session_id: session.clone(),
                lifetime: magi_core::AgentLifetime::Session,
                generation: 1,
                previous_task_id: None,
            }),
            ..Default::default()
        }),
        completion_contract: Default::default(),
        recovery_checkpoint: None,
        knowledge_refs: vec![],
        workspace_scope: None,
        write_scope: None,
        input_refs: vec![],
        output_refs: vec![],
        evidence_refs: vec![],
        retry_count: 0,
        runtime_payload: magi_core::TaskRuntimePayload::None,
        created_at: UtcMillis(11),
        updated_at: UtcMillis(11),
    };
    let mut thread = sample_thread(
        "agent-owned-thread",
        &session,
        &mission,
        "explorer",
        UtcMillis(11),
        ExecutionThreadStatus::Active,
    );
    thread.handled_task_ids.push(child_id);
    let thread_id = thread.thread_id.clone();
    store.register_thread(thread).unwrap();
    (store, session, child, thread_id)
}

fn agent_turn_tool(task: &magi_core::Task, thread: &ThreadId) -> ActiveExecutionTurnItem {
    let mut item = test_turn_item("agent-owned-tool", "start");
    item.kind = "tool_call_started".into();
    item.status = "running".into();
    item.task_id = Some(task.task_id.clone());
    item.source_thread_id = thread.clone();
    item.role_id = Some("explorer".into());
    item.tool_call_id = Some("agent-provider-call".into());
    item.tool_name = Some("file_write".into());
    item.tool_arguments = Some("{\"path\":\"result.txt\",\"content\":\"done\"}".into());
    item
}

fn transferred_agent_task(task: &magi_core::Task) -> magi_core::Task {
    let mut task = task.clone();
    let transfer = magi_core::AgentRunTransfer {
        operation_id: "agent-transfer".into(),
        session_id: task.agent_activation().unwrap().session_id.clone(),
        source_turn_id: "agent-source-turn".into(),
        transferred_by: task.parent_task_id.clone().unwrap(),
        activation_generation: task.agent_activation().unwrap().generation,
        owner: magi_core::AgentRunOwner::Session,
        transferred_at: UtcMillis::now(),
    };
    task.executor_binding.as_mut().unwrap().agent_run_transfer = Some(transfer);
    task
}

fn install_agent_transfer(store: &SessionStore, task: magi_core::Task) {
    store.set_agent_run_transfer_resolver(Arc::new(move |_| Ok(Some(task.clone()))));
}

#[test]
fn transferred_agent_skill_state_survives_mainline_change_tool_writeback_and_reload() {
    let (store, session, mut task, thread) = agent_turn_fixture();
    task.executor_binding.as_mut().unwrap().active_skill_id = Some("initial-skill".into());
    let owned = store
        .create_agent_turn(&task, "agent-source-turn", &thread)
        .unwrap();
    assert_eq!(
        owned.agent_active_skill_id().unwrap().as_deref(),
        Some("initial-skill")
    );
    install_agent_transfer(&store, transferred_agent_task(&task));
    store
        .update_current_turn_status_for_turn_with_change(
            &session,
            Some("agent-source-turn"),
            "completed",
        )
        .unwrap();
    accept_test_turn(
        &store,
        &session,
        test_turn("later-mainline", "running", UtcMillis::now().0 + 1),
    );
    let main_before = serde_json::to_value(store.execution_sidecar_store_state()).unwrap();
    store
        .update_agent_turn_active_skill(&session, &task.task_id, &owned.turn_id, "closed-skill")
        .unwrap();
    assert_eq!(
        serde_json::to_value(store.execution_sidecar_store_state()).unwrap(),
        main_before
    );
    store
        .upsert_agent_turn_item(
            &session,
            &task.task_id,
            &owned.turn_id,
            agent_turn_tool(&task, &thread),
        )
        .unwrap();
    let updated = store
        .agent_turn_for_task(&session, &task.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        updated.agent_active_skill_id().unwrap().as_deref(),
        Some("closed-skill")
    );
    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    assert_eq!(
        restored
            .agent_turn_for_task(&session, &task.task_id)
            .unwrap()
            .unwrap()
            .agent_active_skill_id()
            .unwrap()
            .as_deref(),
        Some("closed-skill")
    );
    let mut forged = store.durable_state();
    forged
        .canonical_turns
        .iter_mut()
        .find(|turn| turn.turn_id == owned.turn_id)
        .unwrap()
        .metadata
        .insert("agentActiveSkillId".into(), json!({"skill":"invalid"}));
    assert!(
        SessionStore::from_persisted_parts(forged, store.execution_sidecar_store_state()).is_err()
    );
}

#[test]
fn agent_skill_activation_failure_or_wrong_source_preserves_all_facts() {
    let (store, session, task, thread) = agent_turn_fixture();
    let owned = store
        .create_agent_turn(&task, "agent-source-turn", &thread)
        .unwrap();
    let before = store_mutation_snapshot(&store);
    for (target, turn, skill) in [
        (task.task_id.clone(), "stale-turn", "skill"),
        (TaskId::new("foreign-task"), owned.turn_id.as_str(), "skill"),
        (task.task_id.clone(), owned.turn_id.as_str(), " "),
    ] {
        assert!(
            store
                .update_agent_turn_active_skill(&session, &target, turn, skill)
                .is_err()
        );
        assert_store_mutation_snapshot_unchanged(&store, &before);
    }
    reject_canonical_writes(&store);
    assert!(
        store
            .update_agent_turn_active_skill(&session, &task.task_id, &owned.turn_id, "skill")
            .is_err()
    );
    assert_store_mutation_snapshot_unchanged(&store, &before);
}

fn agent_test_step(task: &magi_core::Task, thread: &ThreadId, id: &str) -> crate::ModelStep {
    let mut step = test_model_step(id, vec![]);
    step.task_id = Some(task.task_id.clone());
    step.thread_id = thread.clone();
    step
}

#[test]
fn transferred_agent_session_admission_keeps_source_and_frozen_step_accounting() {
    let (store, session, task, thread) = agent_turn_fixture();
    let goal = create_test_goal(&store, &session, "agent-source-turn", "old goal");
    let owned = store
        .create_agent_turn(&task, "agent-source-turn", &thread)
        .unwrap();
    store
        .begin_model_step(
            &session,
            &owned.turn_id,
            agent_test_step(&task, &thread, "before-transfer"),
        )
        .unwrap();
    let transferred = transferred_agent_task(&task);
    install_agent_transfer(&store, transferred.clone());
    store
        .update_current_turn_status_for_turn_with_change(
            &session,
            Some("agent-source-turn"),
            "cancelled",
        )
        .unwrap();
    accept_test_turn(
        &store,
        &session,
        test_turn("another-mainline", "running", UtcMillis::now().0 + 1),
    );
    let main_before = serde_json::to_value(store.execution_sidecar_store_state()).unwrap();
    assert!(store.turn_execution_is_active(&session, &owned.turn_id));
    assert!(!store.turn_execution_was_cancelled(&session, &owned.turn_id));
    assert!(store.model_step_goal(&session, &owned.turn_id).is_none());
    store
        .begin_model_step(
            &session,
            &owned.turn_id,
            agent_test_step(&task, &thread, "after-transfer"),
        )
        .unwrap();
    store
        .start_model_step_retry(&session, &owned.turn_id, "after-transfer")
        .unwrap();
    store
        .upsert_agent_turn_item(
            &session,
            &task.task_id,
            &owned.turn_id,
            agent_turn_tool(&task, &thread),
        )
        .unwrap();
    let approval = crate::tool_approval::PendingToolApproval {
        approval_id: "transferred-session-approval".into(),
        session_id: session.clone(),
        task_id: task.task_id.clone(),
        turn_id: owned.turn_id.clone(),
        tool_call_id: "agent-provider-call".into(),
        tool_name: "file_write".into(),
        reason: "转交后的独立交互".into(),
        requested_at: UtcMillis::now(),
        agent: None,
    };
    store
        .register_tool_approval(approval.clone(), None, json!({}), None)
        .unwrap();
    assert_eq!(
        store.pending_tool_approvals(&session).unwrap(),
        std::slice::from_ref(&approval)
    );
    store
        .resolve_tool_approvals(
            &session,
            std::slice::from_ref(&approval.approval_id),
            crate::tool_approval::ToolApprovalDecision::AllowOnce,
        )
        .unwrap();
    assert!(
        store
            .claim_tool_approval(
                &session,
                &task.task_id,
                "agent-provider-call",
                "file_write",
                r#"{"path":"result.txt","content":"done"}"#,
                None,
                &json!({}),
                std::slice::from_ref(&task.task_id),
                Some(&approval.approval_id),
            )
            .unwrap()
            .is_some(),
        "原父轮关闭和主轮切换不得把批准路由到其他 Task"
    );
    for (id, used) in [("before-transfer", 7), ("after-transfer", 11)] {
        store
            .settle_model_step(
                &session,
                &owned.turn_id,
                id,
                Some("b".repeat(64)),
                Some(used),
                None,
            )
            .unwrap();
    }
    let updated = store
        .agent_turn_for_task(&session, &task.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(updated.model_steps[0].goal_id.as_ref(), Some(&goal.goal_id));
    assert!(updated.model_steps[1].goal_id.is_none());
    assert_eq!(store.current_goal(&session).unwrap().tokens_used, 7);
    assert_eq!(
        updated.metadata, owned.metadata,
        "转交不得改写 canonical 来源或复制拥有方"
    );
    assert_eq!(
        serde_json::to_value(store.execution_sidecar_store_state()).unwrap(),
        main_before
    );
    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    assert!(
        !restored.turn_execution_is_active(&session, &owned.turn_id),
        "未接线时不能推测转交权"
    );
    install_agent_transfer(&restored, transferred);
    assert!(restored.turn_execution_is_active(&session, &owned.turn_id));
}

#[test]
fn transferred_agent_rejects_missing_query_or_mismatched_task_without_mutation() {
    for mutation in 0..13 {
        let (store, session, task, thread) = agent_turn_fixture();
        let owned = store
            .create_agent_turn(&task, "agent-source-turn", &thread)
            .unwrap();
        store
            .begin_model_step(
                &session,
                &owned.turn_id,
                agent_test_step(&task, &thread, "already-sent"),
            )
            .unwrap();
        let mut transferred = transferred_agent_task(&task);
        match mutation {
            0 => {
                store.set_agent_run_transfer_resolver(Arc::new(|_| {
                    Err(magi_core::DomainError::NotFound {
                        entity: "AgentTask",
                    })
                }));
            }
            1 => {
                transferred
                    .executor_binding
                    .as_mut()
                    .unwrap()
                    .agent_run_transfer = None
            }
            2 => {
                transferred
                    .executor_binding
                    .as_mut()
                    .unwrap()
                    .agent_activation = None
            }
            3 => {
                transferred
                    .executor_binding
                    .as_mut()
                    .unwrap()
                    .agent_run_transfer
                    .as_mut()
                    .unwrap()
                    .activation_generation += 1
            }
            4 => {
                transferred
                    .executor_binding
                    .as_mut()
                    .unwrap()
                    .agent_run_transfer
                    .as_mut()
                    .unwrap()
                    .session_id = SessionId::new("foreign")
            }
            5 => transferred.root_task_id = TaskId::new("foreign-root"),
            6 => transferred.mission_id = MissionId::new("foreign-mission"),
            7 => transferred.parent_task_id = Some(TaskId::new("foreign-parent")),
            8 => {
                transferred
                    .executor_binding
                    .as_mut()
                    .unwrap()
                    .agent_run_transfer
                    .as_mut()
                    .unwrap()
                    .source_turn_id = "foreign-turn".into()
            }
            9 => {
                transferred
                    .executor_binding
                    .as_mut()
                    .unwrap()
                    .agent_run_transfer
                    .as_mut()
                    .unwrap()
                    .transferred_at = UtcMillis(1)
            }
            10 => transferred.kind = magi_core::TaskKind::LocalWorkflow,
            11 => transferred.task_id = TaskId::new("foreign-agent"),
            _ => {
                transferred
                    .executor_binding
                    .as_mut()
                    .unwrap()
                    .agent_run_transfer
                    .as_mut()
                    .unwrap()
                    .owner = magi_core::AgentRunOwner::Goal {
                    goal_id: magi_core::GoalId::new("missing"),
                    control_revision: 1,
                }
            }
        }
        if mutation != 0 {
            install_agent_transfer(&store, transferred);
        }
        let before = store_mutation_snapshot(&store);
        assert!(
            !store.turn_execution_is_active(&session, &owned.turn_id),
            "{mutation}"
        );
        assert!(
            store
                .begin_model_step(
                    &session,
                    &owned.turn_id,
                    agent_test_step(&task, &thread, "must-not-send")
                )
                .is_err(),
            "{mutation}"
        );
        assert!(
            store
                .start_model_step_retry(&session, &owned.turn_id, "already-sent")
                .is_err(),
            "{mutation}"
        );
        assert!(
            store
                .upsert_agent_turn_item(
                    &session,
                    &task.task_id,
                    &owned.turn_id,
                    agent_turn_tool(&task, &thread)
                )
                .is_err(),
            "{mutation}"
        );
        assert_store_mutation_snapshot_unchanged(&store, &before);
        // 查询失效阻止继续执行，不能阻止在途响应按原身份结算用量。
        store
            .settle_model_step(
                &session,
                &owned.turn_id,
                "already-sent",
                Some("c".repeat(64)),
                Some(5),
                None,
            )
            .unwrap();
    }
}

#[test]
fn transferred_agent_goal_revision_revokes_steps_tools_and_interaction_responses() {
    use crate::tool_approval::{PendingToolApproval, ToolApprovalDecision};
    use crate::user_question::{PendingUserQuestion, UserQuestionResponse};
    for interaction in ["question", "approval"] {
        let (store, session, mut task, thread) = agent_turn_fixture();
        let goal = create_test_goal(&store, &session, "agent-source-turn", "owned goal");
        task.executor_binding
            .as_mut()
            .unwrap()
            .agent_activation
            .as_mut()
            .unwrap()
            .lifetime = magi_core::AgentLifetime::Goal {
            goal_id: goal.goal_id.clone(),
        };
        let owned = store
            .create_agent_turn(&task, "agent-source-turn", &thread)
            .unwrap();
        let mut transferred = transferred_agent_task(&task);
        transferred
            .executor_binding
            .as_mut()
            .unwrap()
            .agent_run_transfer
            .as_mut()
            .unwrap()
            .owner = magi_core::AgentRunOwner::Goal {
            goal_id: goal.goal_id.clone(),
            control_revision: goal.control_revision,
        };
        install_agent_transfer(&store, transferred.clone());
        store
            .begin_model_step(
                &session,
                &owned.turn_id,
                agent_test_step(&task, &thread, "owned-step"),
            )
            .unwrap();
        assert_eq!(
            store
                .model_step_goal(&session, &owned.turn_id)
                .unwrap()
                .goal_id,
            goal.goal_id
        );
        let mut item = agent_turn_tool(&task, &thread);
        if interaction == "question" {
            item.tool_name = Some("ask_user_question".into());
        }
        store
            .upsert_agent_turn_item(&session, &task.task_id, &owned.turn_id, item)
            .unwrap();
        let question = PendingUserQuestion {
            question_id: "transfer-question".into(), session_id: session.clone(), task_id: Some(task.task_id.clone()),
            turn_id: owned.turn_id.clone(), tool_call_id: "agent-provider-call".into(), requested_at: UtcMillis::now(), can_skip: true,
            questions: crate::user_question::parse_questions(&json!({"questions":[{"header":"选择","question":"继续？","options":[{"label":"A"},{"label":"B"}]}]}).to_string()).unwrap(),
        };
        let approval = PendingToolApproval {
            approval_id: "transfer-approval".into(),
            session_id: session.clone(),
            task_id: task.task_id.clone(),
            turn_id: owned.turn_id.clone(),
            tool_call_id: "agent-provider-call".into(),
            tool_name: "file_write".into(),
            reason: "需要授权".into(),
            requested_at: UtcMillis::now(),
            agent: None,
        };
        if interaction == "question" {
            store.register_user_question(question.clone()).unwrap();
        } else {
            store
                .register_tool_approval(approval.clone(), None, json!({}), None)
                .unwrap();
        }
        // 同一 Goal 仍是 Active，但控制版本变化已撤销旧转交。
        store
            .update_goal_objective_if_revision(
                &session,
                &goal.goal_id,
                "changed goal",
                Some(goal.control_revision),
            )
            .unwrap();
        let before = store_mutation_snapshot(&store);
        assert!(!store.turn_execution_is_active(&session, &owned.turn_id));
        assert!(store.model_step_goal(&session, &owned.turn_id).is_none());
        assert!(store.turn_execution_was_cancelled(&session, &owned.turn_id));
        assert!(
            store
                .begin_model_step(
                    &session,
                    &owned.turn_id,
                    agent_test_step(&task, &thread, "revoked")
                )
                .is_err()
        );
        assert!(
            store
                .start_model_step_retry(&session, &owned.turn_id, "owned-step")
                .is_err()
        );
        assert!(
            store
                .upsert_agent_turn_item(
                    &session,
                    &task.task_id,
                    &owned.turn_id,
                    agent_turn_tool(&task, &thread)
                )
                .is_err()
        );
        if interaction == "question" {
            assert!(
                store
                    .resolve_user_question(
                        &session,
                        &question.question_id,
                        UserQuestionResponse::Skipped
                    )
                    .is_err()
            );
        } else {
            assert!(
                store
                    .resolve_tool_approvals(
                        &session,
                        std::slice::from_ref(&approval.approval_id),
                        ToolApprovalDecision::AllowOnce
                    )
                    .is_err()
            );
        }
        assert_store_mutation_snapshot_unchanged(&store, &before);
        store
            .settle_model_step(
                &session,
                &owned.turn_id,
                "owned-step",
                Some("d".repeat(64)),
                Some(9),
                None,
            )
            .unwrap();
        assert_eq!(store.current_goal(&session).unwrap().tokens_used, 9);
        // 重启的 Interrupted 交互也不能借原主轮 ready 标记越过已撤销的拥有方。
        let parent = store.runtime_sidecar(&session).unwrap().current_turn;
        let mut chain = test_active_chain(&session, "transfer-restart", parent);
        chain.root_task_id = task.root_task_id.clone();
        chain.mission_id = task.mission_id.clone();
        store
            .upsert_active_execution_chain(session.clone(), chain)
            .unwrap();
        store
            .interrupt_current_turn_by_daemon_restart(&session)
            .unwrap();
        let restored = SessionStore::from_persisted_parts(
            store.durable_state(),
            store.execution_sidecar_store_state(),
        )
        .unwrap();
        install_agent_transfer(&restored, transferred);
        assert!(
            restored
                .pending_user_questions(&session)
                .unwrap()
                .is_empty()
        );
        assert!(
            restored
                .pending_tool_approvals(&session)
                .unwrap()
                .is_empty()
        );
        if interaction == "question" {
            assert!(
                restored
                    .resolve_user_question(
                        &session,
                        &question.question_id,
                        UserQuestionResponse::Skipped
                    )
                    .is_err()
            );
        } else {
            assert!(
                restored
                    .resolve_tool_approvals(
                        &session,
                        &[approval.approval_id],
                        ToolApprovalDecision::AllowOnce
                    )
                    .is_err()
            );
        }
    }
}

#[test]
fn independent_agent_restart_preserves_interactions_without_original_mainline_pointer() {
    use crate::tool_approval::{PendingToolApproval, ToolApprovalDecision};
    use crate::user_question::{PendingUserQuestion, UserQuestionResponse};
    use magi_core::TaskStatus;
    for (interaction, source_status) in [
        ("question", "completed"),
        ("approval", "completed"),
        ("question", "interrupted"),
        ("approval", "interrupted"),
    ] {
        let (store, session, mut task, thread) = agent_turn_fixture();
        let owned = store
            .create_agent_turn(&task, "agent-source-turn", &thread)
            .unwrap();
        let mut item = agent_turn_tool(&task, &thread);
        if interaction == "question" {
            item.tool_name = Some("ask_user_question".into());
        }
        store
            .upsert_agent_turn_item(&session, &task.task_id, &owned.turn_id, item)
            .unwrap();
        let question = PendingUserQuestion {
            question_id: "independent-restart-question".into(), session_id: session.clone(), task_id: Some(task.task_id.clone()),
            turn_id: owned.turn_id.clone(), tool_call_id: "agent-provider-call".into(), requested_at: UtcMillis::now(), can_skip: true,
            questions: crate::user_question::parse_questions(&json!({"questions":[{"header":"选择","question":"继续？","options":[{"label":"A"},{"label":"B"}]}]}).to_string()).unwrap(),
        };
        let approval = PendingToolApproval {
            approval_id: "independent-restart-approval".into(),
            session_id: session.clone(),
            task_id: task.task_id.clone(),
            turn_id: owned.turn_id.clone(),
            tool_call_id: "agent-provider-call".into(),
            tool_name: "file_write".into(),
            reason: "授权".into(),
            requested_at: UtcMillis::now(),
            agent: None,
        };
        if interaction == "question" {
            store.register_user_question(question.clone()).unwrap();
        } else {
            store
                .register_tool_approval(approval.clone(), None, json!({}), None)
                .unwrap();
        }
        let mut transferred = transferred_agent_task(&task);
        install_agent_transfer(&store, transferred.clone());
        if source_status == "interrupted" {
            let parent = store.runtime_sidecar(&session).unwrap().current_turn;
            let mut chain = test_active_chain(&session, "independent-restart", parent);
            chain.root_task_id = task.root_task_id.clone();
            chain.mission_id = task.mission_id.clone();
            store
                .upsert_active_execution_chain(session.clone(), chain)
                .unwrap();
            store
                .interrupt_current_turn_by_daemon_restart(&session)
                .unwrap();
            assert_eq!(
                store
                    .agent_turn_for_task(&session, &task.task_id)
                    .unwrap()
                    .unwrap()
                    .status,
                CanonicalTurnStatus::Running,
                "原主线的中断不结束已转交代理"
            );
        } else {
            store
                .update_current_turn_status_for_turn_with_change(
                    &session,
                    Some("agent-source-turn"),
                    "completed",
                )
                .unwrap();
        }
        accept_test_turn(
            &store,
            &session,
            test_turn(
                "new-mainline-during-agent",
                "running",
                UtcMillis::now().0 + 1,
            ),
        );
        let current_before = serde_json::to_value(store.execution_sidecar_store_state()).unwrap();
        // Task/lease 先提交重启结算；canonical writer 失败不能先更新 Agent Turn。
        task.status = TaskStatus::Failed;
        let before = store_mutation_snapshot(&store);
        reject_canonical_writes(&store);
        assert!(
            store
                .reconcile_agent_turns_after_restart(&session, &[task.clone()])
                .is_err()
        );
        assert_store_mutation_snapshot_unchanged(&store, &before);
        let writer = Arc::new(RecordingCanonicalWriter::default());
        store.install_session_event_writer(writer.clone());
        assert_eq!(
            store
                .reconcile_agent_turns_after_restart(&session, &[task.clone()])
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .reconcile_agent_turns_after_restart(&session, &[task.clone()])
                .unwrap(),
            0
        );
        assert_eq!(writer.transactions.lock().unwrap().len(), 1);
        let restored = SessionStore::from_persisted_parts(
            store.durable_state(),
            store.execution_sidecar_store_state(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(restored.execution_sidecar_store_state()).unwrap(),
            current_before
        );
        assert!(!restored.turn_execution_is_active(&session, &owned.turn_id));
        assert!(
            restored
                .pending_user_questions(&session)
                .unwrap()
                .is_empty()
        );
        assert!(
            restored
                .pending_tool_approvals(&session)
                .unwrap()
                .is_empty()
        );
        transferred.status = TaskStatus::Failed;
        install_agent_transfer(&restored, transferred.clone());
        assert_eq!(
            restored
                .agent_turn_for_task(&session, &task.task_id)
                .unwrap()
                .unwrap()
                .status,
            CanonicalTurnStatus::Interrupted
        );
        if interaction == "question" {
            assert_eq!(
                restored.pending_user_questions(&session).unwrap(),
                std::slice::from_ref(&question)
            );
            restored
                .resolve_user_question(
                    &session,
                    &question.question_id,
                    UserQuestionResponse::Skipped,
                )
                .unwrap();
        } else {
            assert_eq!(
                restored.pending_tool_approvals(&session).unwrap(),
                std::slice::from_ref(&approval)
            );
            restored
                .resolve_tool_approvals(
                    &session,
                    &[approval.approval_id],
                    ToolApprovalDecision::AllowOnce,
                )
                .unwrap();
        }
        assert!(
            !restored.turn_execution_is_active(&session, &owned.turn_id),
            "答复不是重新执行授权"
        );
        assert!(
            restored
                .begin_model_step(
                    &session,
                    &owned.turn_id,
                    agent_test_step(&task, &thread, "must-not-restart")
                )
                .is_err()
        );
        assert_eq!(
            serde_json::to_value(restored.execution_sidecar_store_state()).unwrap(),
            current_before
        );
        let operation = "续".repeat(200);
        let before_continuation = store_mutation_snapshot(&restored);
        assert!(
            restored
                .prepare_transferred_agent_continuation(
                    &session,
                    std::slice::from_ref(&transferred),
                    &owned.turn_id,
                    &"x".repeat(201)
                )
                .is_err()
        );
        assert!(
            restored
                .prepare_transferred_agent_continuation(
                    &session,
                    std::slice::from_ref(&transferred),
                    &owned.turn_id,
                    &"续".repeat(201)
                )
                .is_err(),
            "operationId 上限按字符计数，不能因 UTF-8 字节数误判"
        );
        assert_store_mutation_snapshot_unchanged(&restored, &before_continuation);
        let continued = restored
            .prepare_transferred_agent_continuation(
                &session,
                &[transferred.clone()],
                &owned.turn_id,
                &operation,
            )
            .unwrap();
        assert_eq!(continued.len(), 1);
        let binding = continued[0].agent_turn_binding().unwrap().unwrap();
        assert_eq!(
            binding.continuation_operation_id.as_deref(),
            Some(operation.as_str())
        );
        assert_eq!(
            binding.previous_turn_id.as_deref(),
            Some(owned.turn_id.as_str())
        );
        assert_eq!(binding.thread_id, thread);
        assert_eq!(continued[0].status, CanonicalTurnStatus::Running);
        let after = store_mutation_snapshot(&restored);
        assert!(
            restored
                .prepare_transferred_agent_continuation(
                    &session,
                    &[transferred],
                    &owned.turn_id,
                    "another-continue"
                )
                .is_err()
        );
        assert!(
            restored
                .finish_agent_turn(
                    &session,
                    &task.task_id,
                    &owned.turn_id,
                    CanonicalTurnStatus::Completed
                )
                .is_err(),
            "旧执行的迟到结果不能重写中断记录"
        );
        assert_store_mutation_snapshot_unchanged(&restored, &after);
        let reloaded = SessionStore::from_persisted_parts(
            restored.durable_state(),
            restored.execution_sidecar_store_state(),
        )
        .unwrap();
        assert_eq!(
            reloaded
                .agent_continuation_for_operation(&session, &task.task_id, &operation)
                .unwrap()
                .unwrap(),
            continued[0]
        );
        assert_eq!(
            serde_json::to_value(reloaded.execution_sidecar_store_state()).unwrap(),
            current_before,
            "独立继续不能改变新主线指针"
        );
    }
}

#[test]
fn independent_agent_restart_requires_settled_matching_task_and_preserves_terminal_results() {
    use magi_core::TaskStatus;
    for status in [
        TaskStatus::Pending,
        TaskStatus::Running,
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Killed,
    ] {
        let (store, session, mut task, thread) = agent_turn_fixture();
        let owned = store
            .create_agent_turn(&task, "agent-source-turn", &thread)
            .unwrap();
        task.status = status;
        let before = store_mutation_snapshot(&store);
        assert!(
            store
                .reconcile_agent_turns_after_restart(&session, &[])
                .is_err()
        );
        assert_store_mutation_snapshot_unchanged(&store, &before);
        let mut foreign = task.clone();
        foreign.mission_id = MissionId::new("foreign");
        assert!(
            store
                .reconcile_agent_turns_after_restart(&session, &[foreign])
                .is_err()
        );
        assert_store_mutation_snapshot_unchanged(&store, &before);
        let result = store.reconcile_agent_turns_after_restart(&session, &[task.clone()]);
        if matches!(status, TaskStatus::Pending | TaskStatus::Running) {
            assert!(result.is_err());
            assert_store_mutation_snapshot_unchanged(&store, &before);
        } else {
            assert_eq!(result.unwrap(), 1);
            let expected = match status {
                TaskStatus::Completed => CanonicalTurnStatus::Completed,
                TaskStatus::Killed => CanonicalTurnStatus::Cancelled,
                _ => CanonicalTurnStatus::Interrupted,
            };
            assert_eq!(
                store
                    .canonical_turn_for_session_turn_id(&session, &owned.turn_id)
                    .unwrap()
                    .status,
                expected
            );
            assert_eq!(
                store
                    .reconcile_agent_turns_after_restart(&session, &[task])
                    .unwrap(),
                0
            );
        }
    }
}

#[test]
fn independent_agent_restart_retains_unknown_side_effect_and_original_step_usage() {
    use crate::CanonicalTurnItemStatus;
    let (store, session, mut task, thread) = agent_turn_fixture();
    let goal = create_test_goal(&store, &session, "agent-source-turn", "original accounting");
    let owned = store
        .create_agent_turn(&task, "agent-source-turn", &thread)
        .unwrap();
    store
        .begin_model_step(
            &session,
            &owned.turn_id,
            agent_test_step(&task, &thread, "sent-before-restart"),
        )
        .unwrap();
    store
        .upsert_agent_turn_item(
            &session,
            &task.task_id,
            &owned.turn_id,
            agent_turn_tool(&task, &thread),
        )
        .unwrap();
    let transferred = transferred_agent_task(&task);
    install_agent_transfer(&store, transferred.clone());
    store
        .update_current_turn_status_for_turn_with_change(
            &session,
            Some("agent-source-turn"),
            "completed",
        )
        .unwrap();
    task.status = magi_core::TaskStatus::Failed;
    store
        .reconcile_agent_turns_after_restart(&session, &[task.clone()])
        .unwrap();
    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    install_agent_transfer(&restored, transferred);
    let interrupted = restored
        .agent_turn_for_task(&session, &task.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        interrupted.items[0].status,
        CanonicalTurnItemStatus::Indeterminate
    );
    assert_eq!(
        interrupted.items[0]
            .tool
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap()["status"],
        "outcome_unknown"
    );
    assert_eq!(
        interrupted.model_steps[0].status,
        crate::ModelStepStatus::Started
    );
    assert!(
        restored
            .start_model_step_retry(&session, &owned.turn_id, "sent-before-restart")
            .is_err()
    );
    assert!(
        restored
            .begin_model_step(
                &session,
                &owned.turn_id,
                agent_test_step(&task, &thread, "must-not-resend")
            )
            .is_err()
    );
    restored
        .settle_model_step(
            &session,
            &owned.turn_id,
            "sent-before-restart",
            Some("a".repeat(64)),
            Some(13),
            None,
        )
        .unwrap();
    assert!(
        restored
            .settle_model_step(
                &session,
                &owned.turn_id,
                "sent-before-restart",
                Some("a".repeat(64)),
                Some(13),
                None,
            )
            .is_err()
    );
    assert_eq!(
        restored.current_goal(&session).unwrap().goal_id,
        goal.goal_id
    );
    assert_eq!(restored.current_goal(&session).unwrap().tokens_used, 13);
    assert_eq!(
        restored
            .agent_turn_for_task(&session, &task.task_id)
            .unwrap()
            .unwrap()
            .model_steps
            .len(),
        1
    );
    assert!(!restored.turn_execution_is_active(&session, &owned.turn_id));
}

#[test]
fn transferred_agent_boundary_preserves_nested_parent_and_job_admission() {
    let (store, session, task, thread) = agent_turn_fixture();
    let parent = store
        .create_agent_turn(&task, "agent-source-turn", &thread)
        .unwrap();
    let mut leaf = task.clone();
    leaf.task_id = TaskId::new("transfer-leaf");
    leaf.parent_task_id = Some(task.task_id.clone());
    leaf.executor_binding
        .as_mut()
        .unwrap()
        .agent_activation
        .as_mut()
        .unwrap()
        .agent_id = leaf.task_id.clone();
    let mut leaf_thread = sample_thread(
        "transfer-leaf-thread",
        &session,
        &task.mission_id,
        "explorer",
        UtcMillis(12),
        ExecutionThreadStatus::Active,
    );
    leaf_thread.handled_task_ids.push(leaf.task_id.clone());
    let leaf_thread_id = leaf_thread.thread_id.clone();
    store.register_thread(leaf_thread).unwrap();
    let owned = store
        .create_agent_turn(&leaf, &parent.turn_id, &leaf_thread_id)
        .unwrap();
    install_agent_transfer(&store, transferred_agent_task(&task));
    store
        .update_current_turn_status_for_turn_with_change(
            &session,
            Some("agent-source-turn"),
            "completed",
        )
        .unwrap();
    assert!(store.turn_execution_is_active(&session, &owned.turn_id));
    let mut item = agent_turn_tool(&leaf, &leaf_thread_id);
    item.tool_name = Some("shell_exec".into());
    item.tool_arguments = Some(r#"{"command":"sleep 1","background":true}"#.into());
    store
        .upsert_agent_turn_item(&session, &leaf.task_id, &owned.turn_id, item)
        .unwrap();
    let store = Arc::new(store);
    let journal =
        store.background_job_journal(std::env::temp_dir().join("agent-transfer-job-test"));
    let context = magi_tool_runtime::ToolExecutionContext {
        session_id: Some(session.clone()),
        task_id: Some(leaf.task_id.clone()),
        caller_scope: magi_tool_runtime::ToolCallerScope::DelegatedTask,
        ..Default::default()
    };
    let mut job = journal
        .begin(&context, "agent-provider-call", "sleep 1", "/workspace")
        .unwrap();
    store
        .finish_agent_turn(
            &session,
            &task.task_id,
            &parent.turn_id,
            CanonicalTurnStatus::Failed,
        )
        .unwrap();
    assert!(
        !store.turn_execution_is_active(&session, &owned.turn_id),
        "转交不得越过子树内父轮的关闭"
    );
    assert!(
        store
            .begin_model_step(
                &session,
                &owned.turn_id,
                agent_test_step(&leaf, &leaf_thread_id, "late-leaf")
            )
            .is_err()
    );
    // 已有进程的停止结算保留原 Job；禁止借已有子树权继续创建工作。
    job.revision += 1;
    job.status = magi_tool_runtime::BackgroundJobStatus::Cancelled;
    job.settled_at = Some(UtcMillis::now());
    journal.commit(&job).unwrap();
    assert_eq!(journal.list(&context).unwrap(), [job]);
}

#[test]
fn agent_turn_writes_and_model_steps_use_owned_stream_without_changing_mainline() {
    let (store, session, task, thread) = agent_turn_fixture();
    let sidecar_before = serde_json::to_value(store.execution_sidecar_store_state()).unwrap();
    let owned = store
        .create_agent_turn(&task, "agent-source-turn", &thread)
        .unwrap();
    assert_eq!(
        store
            .create_agent_turn(&task, "agent-source-turn", &thread)
            .unwrap(),
        owned
    );
    let item = agent_turn_tool(&task, &thread);
    let started = store
        .upsert_agent_turn_item(&session, &task.task_id, &owned.turn_id, item.clone())
        .unwrap();
    assert_eq!(started.items[0].item_seq, 1);
    assert_eq!(
        started.items[0].worker.as_ref().unwrap().task_id,
        Some(task.task_id.clone())
    );
    let mut step = test_model_step("agent-step", vec![]);
    step.task_id = Some(task.task_id.clone());
    step.thread_id = thread.clone();
    store
        .begin_model_step(&session, &owned.turn_id, step)
        .unwrap();
    assert!(
        store
            .finish_agent_turn(
                &session,
                &task.task_id,
                &owned.turn_id,
                CanonicalTurnStatus::Completed
            )
            .is_err(),
        "不能把未结算工具或请求报告完成"
    );
    store
        .settle_model_step(
            &session,
            &owned.turn_id,
            "agent-step",
            Some("b".repeat(64)),
            Some(12),
            None,
        )
        .unwrap();
    let mut result = item;
    result.kind = "tool_call_result".into();
    result.status = "completed".into();
    result.tool_result = Some("{\"status\":\"succeeded\"}".into());
    store
        .upsert_agent_turn_item(&session, &task.task_id, &owned.turn_id, result.clone())
        .unwrap();
    let (completed, changed) = store
        .finish_agent_turn(
            &session,
            &task.task_id,
            &owned.turn_id,
            CanonicalTurnStatus::Completed,
        )
        .unwrap()
        .unwrap();
    assert!(changed);
    assert_eq!(
        completed.items[0]
            .tool
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap()["status"],
        "succeeded"
    );
    assert_eq!(completed.model_steps.len(), 1);
    assert_eq!(
        serde_json::to_value(store.execution_sidecar_store_state()).unwrap(),
        sidecar_before
    );
    assert!(
        !store
            .finish_agent_turn(
                &session,
                &task.task_id,
                &owned.turn_id,
                CanonicalTurnStatus::Completed
            )
            .unwrap()
            .unwrap()
            .1
    );
    assert!(
        store
            .upsert_agent_turn_item(&session, &task.task_id, &owned.turn_id, result)
            .is_err()
    );
    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    assert_eq!(
        restored
            .agent_turn_for_task(&session, &task.task_id)
            .unwrap(),
        Some(completed)
    );
}

#[test]
fn agent_cancellation_follows_original_parent_after_session_moves_to_another_turn() {
    let (store, session, task, thread) = agent_turn_fixture();
    let owned = store
        .create_agent_turn(&task, "agent-source-turn", &thread)
        .unwrap();
    assert!(!store.turn_execution_was_cancelled(&session, &owned.turn_id));
    store
        .update_current_turn_status_for_turn_with_change(
            &session,
            Some("agent-source-turn"),
            "cancelled",
        )
        .unwrap();
    accept_test_turn(
        &store,
        &session,
        test_turn("new-mainline-turn", "running", 100),
    );
    assert!(store.turn_execution_is_active(&session, "new-mainline-turn"));
    assert!(!store.turn_execution_is_active(&session, &owned.turn_id));
    assert!(store.turn_execution_was_cancelled(&session, &owned.turn_id));
    assert!(!store.turn_execution_was_cancelled(&session, "new-mainline-turn"));
    assert!(
        !store.turn_execution_was_cancelled(&SessionId::new("foreign-session"), &owned.turn_id)
    );
}

#[test]
fn agent_turn_parent_close_blocks_requests_and_retries_but_allows_settlement() {
    let (store, session, task, thread) = agent_turn_fixture();
    let owned = store
        .create_agent_turn(&task, "agent-source-turn", &thread)
        .unwrap();
    assert!(store.turn_execution_is_active(&session, &owned.turn_id));
    let item = agent_turn_tool(&task, &thread);
    store
        .upsert_agent_turn_item(&session, &task.task_id, &owned.turn_id, item.clone())
        .unwrap();
    let mut step = test_model_step("in-flight-agent-step", vec![]);
    step.task_id = Some(task.task_id.clone());
    step.thread_id = thread.clone();
    store
        .begin_model_step(&session, &owned.turn_id, step.clone())
        .unwrap();
    store
        .update_current_turn_status_for_turn_with_change(
            &session,
            Some("agent-source-turn"),
            "completed",
        )
        .unwrap();
    assert!(!store.turn_execution_is_active(&session, &owned.turn_id));
    assert!(
        store
            .start_model_step_retry(&session, &owned.turn_id, &step.step_id)
            .is_err()
    );
    step.step_id = "must-not-send".into();
    step.attempts[0].attempt_id = "must-not-send:attempt:1".into();
    assert!(
        store
            .begin_model_step(&session, &owned.turn_id, step)
            .is_err()
    );
    assert!(
        store
            .upsert_agent_turn_item(&session, &task.task_id, &owned.turn_id, item)
            .is_err()
    );
    store
        .settle_model_step(
            &session,
            &owned.turn_id,
            "in-flight-agent-step",
            Some("c".repeat(64)),
            Some(9),
            None,
        )
        .unwrap();
    let closed = store
        .finish_agent_turn(
            &session,
            &task.task_id,
            &owned.turn_id,
            CanonicalTurnStatus::Cancelled,
        )
        .unwrap()
        .unwrap()
        .0;
    assert_eq!(
        closed.items[0].status,
        crate::CanonicalTurnItemStatus::Indeterminate
    );
    assert_eq!(
        closed.items[0]
            .tool
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap()["status"],
        "outcome_unknown"
    );
    assert_eq!(closed.model_steps[0].usage_tokens, Some(9));
    assert!(
        store
            .finish_agent_turn(
                &session,
                &task.task_id,
                &owned.turn_id,
                CanonicalTurnStatus::Completed
            )
            .is_err()
    );
}

#[test]
fn agent_job_parent_close_blocks_start_but_keeps_original_settlement() {
    for already_started in [false, true] {
        let (store, session, task, thread) = agent_turn_fixture();
        let store = Arc::new(store);
        let owned = store
            .create_agent_turn(&task, "agent-source-turn", &thread)
            .unwrap();
        let mut item = agent_turn_tool(&task, &thread);
        item.tool_name = Some("shell_exec".into());
        item.tool_arguments = Some(r#"{"command":"sleep 1","background":true}"#.into());
        store
            .upsert_agent_turn_item(&session, &task.task_id, &owned.turn_id, item)
            .unwrap();
        let journal =
            store.background_job_journal(std::env::temp_dir().join("agent-job-admission-test"));
        let context = magi_tool_runtime::ToolExecutionContext {
            session_id: Some(session.clone()),
            task_id: Some(task.task_id.clone()),
            caller_scope: magi_tool_runtime::ToolCallerScope::DelegatedTask,
            ..Default::default()
        };
        let started = already_started.then(|| {
            journal
                .begin(&context, "agent-provider-call", "sleep 1", "/workspace")
                .unwrap()
        });
        store
            .update_current_turn_status_for_turn_with_change(
                &session,
                Some("agent-source-turn"),
                "completed",
            )
            .unwrap();
        assert!(!store.turn_execution_is_active(&session, &owned.turn_id));
        if let Some(mut job) = started {
            job.revision += 1;
            job.status = magi_tool_runtime::BackgroundJobStatus::Cancelled;
            job.settled_at = Some(UtcMillis::now());
            journal.commit(&job).unwrap();
            assert_eq!(journal.list(&context).unwrap(), vec![job]);
        } else {
            let before = store_mutation_snapshot(&store);
            assert!(
                journal
                    .begin(&context, "agent-provider-call", "sleep 1", "/workspace")
                    .is_err()
            );
            assert_store_mutation_snapshot_unchanged(&store, &before);
            assert!(journal.list(&context).unwrap().is_empty());
        }
    }
}

#[test]
fn agent_turn_rejects_cross_task_thread_input_and_source_tampering_without_mutation() {
    let (store, session, task, thread) = agent_turn_fixture();
    let owned = store
        .create_agent_turn(&task, "agent-source-turn", &thread)
        .unwrap();
    let original = store_mutation_snapshot(&store);
    let mut bad_task = task.clone();
    bad_task.parent_task_id = Some(TaskId::new("other-parent"));
    assert!(
        store
            .create_agent_turn(&bad_task, "agent-source-turn", &thread)
            .is_err()
    );
    let mut wrong_item = agent_turn_tool(&task, &thread);
    wrong_item.task_id = Some(TaskId::new("other-task"));
    assert!(
        store
            .upsert_agent_turn_item(&session, &task.task_id, &owned.turn_id, wrong_item)
            .is_err()
    );
    let mut step = test_model_step("cross-input", vec!["parent-user-request".into()]);
    step.task_id = Some(task.task_id.clone());
    step.thread_id = thread.clone();
    assert!(
        store
            .begin_model_step(&session, &owned.turn_id, step)
            .is_err()
    );
    let mut wrong_thread = test_model_step("cross-thread", vec![]);
    wrong_thread.task_id = Some(task.task_id.clone());
    assert!(
        store
            .begin_model_step(&session, &owned.turn_id, wrong_thread)
            .is_err()
    );
    assert_store_mutation_snapshot_unchanged(&store, &original);
    let mut changed = owned.clone();
    changed.metadata.get_mut("agentExecution").unwrap()["parentTaskId"] = json!("other-parent");
    assert!(changed.validate_update_from(&owned).is_err());
    let mut durable = store.durable_state();
    durable
        .canonical_turns
        .iter_mut()
        .find(|turn| turn.turn_id == owned.turn_id)
        .unwrap()
        .metadata
        .get_mut("agentExecution")
        .unwrap()["parentTurnId"] = json!("missing-parent");
    assert!(
        SessionStore::from_persisted_parts(durable, store.execution_sidecar_store_state()).is_err()
    );
}

#[test]
fn agent_turn_failed_commits_do_not_publish_creation_item_or_completion() {
    for phase in ["create", "item", "finish"] {
        let (store, session, task, thread) = agent_turn_fixture();
        let owned = if phase != "create" {
            Some(
                store
                    .create_agent_turn(&task, "agent-source-turn", &thread)
                    .unwrap(),
            )
        } else {
            None
        };
        let before = store_mutation_snapshot(&store);
        reject_canonical_writes(&store);
        let failed = match phase {
            "create" => store
                .create_agent_turn(&task, "agent-source-turn", &thread)
                .is_err(),
            "item" => store
                .upsert_agent_turn_item(
                    &session,
                    &task.task_id,
                    &owned.unwrap().turn_id,
                    agent_turn_tool(&task, &thread),
                )
                .is_err(),
            _ => store
                .finish_agent_turn(
                    &session,
                    &task.task_id,
                    &owned.as_ref().unwrap().turn_id,
                    CanonicalTurnStatus::Failed,
                )
                .is_err(),
        };
        assert!(failed, "{phase}");
        assert_store_mutation_snapshot_unchanged(&store, &before);
    }
}

#[test]
fn agent_turn_journal_identity_restores_missing_thread_and_rejects_conflicting_projection() {
    let (store, session, task, thread) = agent_turn_fixture();
    store
        .create_agent_turn(&task, "agent-source-turn", &thread)
        .unwrap();
    let mut durable = store.durable_state();
    durable
        .thread_registry
        .retain(|entry| entry.thread_id != thread);
    let restored =
        SessionStore::from_persisted_parts(durable, store.execution_sidecar_store_state()).unwrap();
    let restored_thread = restored
        .thread_registry_snapshot(&session)
        .into_iter()
        .find(|entry| entry.thread_id == thread)
        .unwrap();
    assert_eq!(
        restored_thread.handled_task_ids,
        std::slice::from_ref(&task.task_id)
    );
    let mut bad = store.durable_state();
    bad.thread_registry
        .iter_mut()
        .find(|entry| entry.thread_id == thread)
        .unwrap()
        .handled_task_ids
        .push(TaskId::new("sibling"));
    assert!(
        SessionStore::from_persisted_parts(bad, store.execution_sidecar_store_state()).is_err()
    );
}

#[test]
fn agent_turn_continue_creates_new_execution_without_reopening_history() {
    let (store, session, task, thread) = agent_turn_fixture();
    let parent = store.runtime_sidecar(&session).unwrap().current_turn;
    let mut chain = test_active_chain(&session, "agent-recovery", parent);
    chain.root_task_id = task.root_task_id.clone();
    chain.mission_id = task.mission_id.clone();
    store
        .upsert_active_execution_chain(session.clone(), chain)
        .unwrap();
    let first = store
        .create_agent_turn(&task, "agent-source-turn", &thread)
        .unwrap();
    store
        .interrupt_current_turn_by_daemon_restart(&session)
        .unwrap();
    let first = store
        .canonical_turn_for_session_turn_id(&session, &first.turn_id)
        .unwrap();
    let mut resumed_parent = test_turn("agent-resumed-parent", "running", UtcMillis::now().0);
    let mut user = test_turn_item("agent-continue-user", "continue");
    user.task_id = Some(task.root_task_id.clone());
    user.metadata.insert("route".into(), json!("continue"));
    resumed_parent.items.push(user);
    let invalid = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    accept_test_turn(&invalid, &session, resumed_parent.clone());
    assert!(
        invalid
            .create_agent_turn(&task, &resumed_parent.turn_id, &thread)
            .is_err(),
        "缺少原父 Turn 来源不得改绑"
    );
    resumed_parent.items[0]
        .metadata
        .insert("recoverySourceTurnId".into(), json!("agent-source-turn"));
    accept_test_turn(&store, &session, resumed_parent.clone());
    let mut stale = resumed_parent.items[0].clone();
    stale.metadata.remove("recoverySourceTurnId");
    let source_before = store_mutation_snapshot(&store);
    assert!(
        store
            .upsert_current_turn_item_for_turn(&session, Some(&resumed_parent.turn_id), stale)
            .is_err()
    );
    assert_store_mutation_snapshot_unchanged(&store, &source_before);
    let before = store_mutation_snapshot(&store);
    reject_canonical_writes(&store);
    assert!(
        store
            .create_agent_turn(&task, &resumed_parent.turn_id, &thread)
            .is_err()
    );
    assert_store_mutation_snapshot_unchanged(&store, &before);
    let store = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    let resumed = store
        .create_agent_turn(&task, &resumed_parent.turn_id, &thread)
        .unwrap();
    assert_ne!(resumed.turn_id, first.turn_id);
    assert_eq!(
        resumed
            .agent_turn_binding()
            .unwrap()
            .unwrap()
            .previous_turn_id,
        Some(first.turn_id.clone())
    );
    assert_eq!(
        store
            .create_agent_turn(&task, &resumed_parent.turn_id, &thread)
            .unwrap(),
        resumed
    );
    assert_eq!(
        store.agent_turn_for_task(&session, &task.task_id).unwrap(),
        Some(resumed.clone())
    );
    assert_eq!(
        store
            .canonical_turn_for_session_turn_id(&session, &first.turn_id)
            .unwrap(),
        first
    );
    assert!(
        store
            .upsert_agent_turn_item(
                &session,
                &task.task_id,
                &first.turn_id,
                agent_turn_tool(&task, &thread)
            )
            .is_err()
    );
    assert!(
        store
            .finish_agent_turn(
                &session,
                &task.task_id,
                &first.turn_id,
                CanonicalTurnStatus::Failed
            )
            .is_err(),
        "迟到结算不能影响新轮"
    );
    assert!(store.turn_execution_is_active(&session, &resumed.turn_id));
    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    assert_eq!(
        restored
            .agent_turn_for_task(&session, &task.task_id)
            .unwrap(),
        Some(resumed)
    );
    let mut bad = store.durable_state();
    bad.canonical_turns
        .iter_mut()
        .find(|turn| turn.turn_id == resumed_parent.turn_id)
        .unwrap()
        .items[0]
        .metadata
        .remove("recoverySourceTurnId");
    assert!(
        SessionStore::from_persisted_parts(bad, store.execution_sidecar_store_state()).is_err()
    );
}

#[test]
fn agent_turn_restart_commits_parent_and_child_atomically_and_restores_interactions() {
    use crate::tool_approval::{PendingToolApproval, ToolApprovalDecision};
    use crate::user_question::{PendingUserQuestion, UserQuestionResponse};
    for interaction in ["question", "approval"] {
        let (store, session, task, thread) = agent_turn_fixture();
        let parent = store.runtime_sidecar(&session).unwrap().current_turn;
        let mut chain = test_active_chain(&session, "agent-restart", parent);
        chain.root_task_id = task.root_task_id.clone();
        chain.mission_id = task.mission_id.clone();
        store
            .upsert_active_execution_chain(session.clone(), chain)
            .unwrap();
        let owned = store
            .create_agent_turn(&task, "agent-source-turn", &thread)
            .unwrap();
        let mut item = agent_turn_tool(&task, &thread);
        let call_id = item.tool_call_id.clone().unwrap();
        if interaction == "question" {
            item.tool_name = Some("ask_user_question".into());
            item.tool_arguments = Some(json!({"questions":[{"header":"选择","question":"继续？","options":[{"label":"A"},{"label":"B"}]}]}).to_string());
        }
        store
            .upsert_agent_turn_item(&session, &task.task_id, &owned.turn_id, item.clone())
            .unwrap();
        let question = PendingUserQuestion {
            question_id: "agent-restart-question".into(), session_id: session.clone(),
            task_id: Some(task.task_id.clone()), turn_id: owned.turn_id.clone(),
            tool_call_id: call_id.clone(), requested_at: UtcMillis::now(), can_skip: true,
            questions: crate::user_question::parse_questions(&json!({"questions":[{"header":"选择","question":"继续？","options":[{"label":"A"},{"label":"B"}]}]}).to_string()).unwrap(),
        };
        let approval = PendingToolApproval {
            approval_id: "agent-restart-approval".into(),
            session_id: session.clone(),
            task_id: task.task_id.clone(),
            turn_id: owned.turn_id.clone(),
            tool_call_id: call_id,
            tool_name: "file_write".into(),
            reason: "需要授权".into(),
            requested_at: UtcMillis::now(),
            agent: None,
        };
        if interaction == "question" {
            store.register_user_question(question.clone()).unwrap();
            assert_eq!(
                store.pending_user_questions(&session).unwrap(),
                std::slice::from_ref(&question)
            );
        } else {
            store
                .register_tool_approval(approval.clone(), None, json!({}), None)
                .unwrap();
            assert_eq!(
                store.pending_tool_approvals(&session).unwrap(),
                std::slice::from_ref(&approval)
            );
        }
        let before = store_mutation_snapshot(&store);
        reject_canonical_writes(&store);
        assert!(
            store
                .interrupt_current_turn_by_daemon_restart(&session)
                .is_err()
        );
        assert_store_mutation_snapshot_unchanged(&store, &before);
        let writer = Arc::new(RecordingCanonicalWriter::default());
        store.install_session_event_writer(writer.clone());
        store
            .interrupt_current_turn_by_daemon_restart(&session)
            .unwrap();
        let transactions = writer.transactions.lock().unwrap();
        assert_eq!(transactions.len(), 1, "父轮与子轮必须一次提交");
        for turn_id in ["agent-source-turn", owned.turn_id.as_str()] {
            assert!(
                transactions[0]
                    .iter()
                    .any(|mutation| mutation.next.turn_id == turn_id
                        && mutation.next.status == CanonicalTurnStatus::Interrupted)
            );
        }
        let interrupted = store
            .agent_turn_for_task(&session, &task.task_id)
            .unwrap()
            .unwrap();
        assert_eq!(interrupted.status, CanonicalTurnStatus::Interrupted);
        assert_eq!(
            interrupted.items[0]
                .tool
                .as_ref()
                .unwrap()
                .result
                .as_ref()
                .unwrap()["status"],
            if interaction == "question" {
                "awaiting_user_input"
            } else {
                "awaiting_approval"
            }
        );
        assert!(!store.turn_execution_is_active(&session, &owned.turn_id));
        let restored = SessionStore::from_persisted_parts(
            store.durable_state(),
            store.execution_sidecar_store_state(),
        )
        .unwrap();
        if interaction == "question" {
            assert_eq!(
                restored.pending_user_questions(&session).unwrap(),
                std::slice::from_ref(&question)
            );
            restored
                .resolve_user_question(
                    &session,
                    &question.question_id,
                    UserQuestionResponse::Skipped,
                )
                .unwrap();
            assert!(
                restored
                    .pending_user_questions(&session)
                    .unwrap()
                    .is_empty()
            );
        } else {
            assert_eq!(
                restored.pending_tool_approvals(&session).unwrap(),
                std::slice::from_ref(&approval)
            );
            restored
                .resolve_tool_approvals(
                    &session,
                    &[approval.approval_id],
                    ToolApprovalDecision::AllowOnce,
                )
                .unwrap();
            assert!(
                restored
                    .pending_tool_approvals(&session)
                    .unwrap()
                    .is_empty()
            );
        }
        assert_eq!(
            restored
                .runtime_sidecar(&session)
                .unwrap()
                .current_turn
                .unwrap()
                .turn_id,
            "agent-source-turn"
        );
    }
}

fn execution_steps_params(revision: u64) -> crate::execution_steps::UpdateExecutionStepsParams {
    serde_json::from_value(json!({"expectedRevision":revision,"steps":[
        {"stepId":"read","title":"读取资料","status":"running","dependsOn":[],"evidenceRefs":[],"taskIds":[]},
        {"stepId":"verify","title":"验证结果","status":"pending","dependsOn":["read"],"evidenceRefs":[],"taskIds":[]}
    ]})).unwrap()
}

fn execution_steps_store() -> (SessionStore, SessionId, TaskId) {
    let store = SessionStore::new();
    let session = SessionId::new("session-execution-steps");
    let task = TaskId::new("task-execution-steps");
    store.create_session(session.clone(), "执行步骤").unwrap();
    accept_test_turn(
        &store,
        &session,
        test_turn("turn-execution-steps", "running", 1),
    );
    let mut source = test_turn_item("source", "分析任务");
    source.kind = "assistant_stream".into();
    source.source = "orchestrator".into();
    source.task_id = Some(task.clone());
    store
        .append_current_turn_item_for_turn(&session, Some("turn-execution-steps"), source)
        .unwrap();
    (store, session, task)
}

#[test]
fn execution_steps_survive_following_tool_write_and_durable_reload() {
    let (store, session, task) = execution_steps_store();
    let first = store
        .update_execution_steps(
            &session,
            "turn-execution-steps",
            &task,
            execution_steps_params(0),
        )
        .unwrap();
    assert_eq!(first.item.metadata["executionSteps"]["revision"], 1);
    assert!(store.current_goal(&session).is_none());
    assert!(store.plan(&session).is_none());
    let mut tool = test_turn_item("proof", "实际工具结果");
    tool.kind = "tool_call_result".into();
    tool.source = "orchestrator".into();
    tool.task_id = Some(task.clone());
    tool.tool_name = Some("file_read".into());
    tool.tool_call_id = Some("read-call".into());
    tool.tool_status = Some("completed".into());
    tool.tool_result = Some("{\"status\":\"succeeded\"}".into());
    store
        .append_current_turn_item_for_turn(&session, Some("turn-execution-steps"), tool)
        .unwrap();
    let mut next = execution_steps_params(1);
    next.steps[0].status = crate::execution_steps::ExecutionStepStatus::Completed;
    next.steps[0].evidence_refs = vec!["read-call".into()];
    next.steps[1].status = crate::execution_steps::ExecutionStepStatus::Running;
    store
        .update_execution_steps(&session, "turn-execution-steps", &task, next)
        .unwrap();
    let snapshot = store
        .execution_steps_for_task(&session, "turn-execution-steps", &task)
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.revision, 2);
    assert_eq!(snapshot.steps[0].evidence_refs, ["proof"]);
    let restored = SessionStore::from_state(store.state.read().unwrap().clone());
    assert_eq!(
        restored
            .execution_steps_for_task(&session, "turn-execution-steps", &task)
            .unwrap(),
        Some(snapshot)
    );
    store
        .update_current_turn_status_for_turn_with_change(
            &session,
            Some("turn-execution-steps"),
            "cancelled",
        )
        .unwrap();
    let stopped = store
        .execution_steps_for_task(&session, "turn-execution-steps", &task)
        .unwrap()
        .unwrap();
    assert_eq!(
        stopped.steps[0].status,
        crate::execution_steps::ExecutionStepStatus::Completed
    );
    assert_eq!(
        stopped.steps[1].status,
        crate::execution_steps::ExecutionStepStatus::Canceled
    );
    assert!(
        store
            .update_execution_steps(
                &session,
                "turn-execution-steps",
                &task,
                execution_steps_params(3)
            )
            .is_err()
    );
}

#[test]
fn execution_steps_reject_stale_revision_foreign_task_and_unverified_completion() {
    let (store, session, task) = execution_steps_store();
    store
        .update_execution_steps(
            &session,
            "turn-execution-steps",
            &task,
            execution_steps_params(0),
        )
        .unwrap();
    assert!(
        store
            .update_execution_steps(
                &session,
                "turn-execution-steps",
                &task,
                execution_steps_params(0)
            )
            .is_err()
    );
    assert!(
        store
            .update_execution_steps(
                &session,
                "turn-execution-steps",
                &TaskId::new("foreign"),
                execution_steps_params(0)
            )
            .is_err()
    );
    let mut input = execution_steps_params(1);
    input.steps[0].status = crate::execution_steps::ExecutionStepStatus::Completed;
    assert!(
        store
            .update_execution_steps(&session, "turn-execution-steps", &task, input)
            .is_err()
    );
    let mut input = execution_steps_params(1);
    input.steps[1].status = crate::execution_steps::ExecutionStepStatus::Running;
    assert!(
        store
            .update_execution_steps(&session, "turn-execution-steps", &task, input)
            .is_err()
    );
    assert_eq!(
        store
            .execution_steps_for_task(&session, "turn-execution-steps", &task)
            .unwrap()
            .unwrap()
            .revision,
        1
    );
}

#[test]
fn execution_steps_persistence_failure_does_not_advance_snapshot_or_sidecar() {
    let (store, session, task) = execution_steps_store();
    store
        .update_execution_steps(
            &session,
            "turn-execution-steps",
            &task,
            execution_steps_params(0),
        )
        .unwrap();
    let before = store_mutation_snapshot(&store);
    reject_canonical_writes(&store);
    assert!(
        store
            .update_execution_steps(
                &session,
                "turn-execution-steps",
                &task,
                execution_steps_params(1)
            )
            .is_err()
    );
    assert_store_mutation_snapshot_unchanged(&store, &before);
}
