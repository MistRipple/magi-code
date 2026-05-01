use super::*;
use crate::models::{
    ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
    ActiveExecutionTurn, ActiveExecutionTurnItem, SessionExecutionSidecarStatus,
    SessionSidecarFlushReason, SessionStoreState,
};
use magi_core::{
    ExecutionOwnership, MissionId, RecoveryResumeInput, SessionId, TaskExecutionTarget, TaskId,
    UtcMillis, WorkerId, WorkspaceId,
};
use serde_json::json;
use std::{thread, time::Duration};

fn test_turn(turn_id: &str, status: &str, accepted_at: u64) -> ActiveExecutionTurn {
    ActiveExecutionTurn {
        turn_id: turn_id.to_string(),
        turn_seq: accepted_at,
        accepted_at: UtcMillis(accepted_at),
        status: status.to_string(),
        completed_at: None,
        user_message: Some(format!("message for {turn_id}")),
        items: Vec::new(),
        worker_lanes: Vec::new(),
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
            deep_task: false,
            skill_name: None,
        },
        current_turn: turn,
    }
}

fn test_turn_item(item_id: &str, content: &str) -> ActiveExecutionTurnItem {
    ActiveExecutionTurnItem {
        item_id: item_id.to_string(),
        item_seq: 0,
        lane_id: None,
        lane_seq: None,
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
        timeline_entry_id: None,
        thread_visible: true,
        worker_visible: false,
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
fn session_sidecar_store_keeps_status_and_recovery_alias() {
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
                }],
                recovery_ref: None,
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: UtcMillis::now(),
                    entry_id: "timeline-active-chain".to_string(),
                    trimmed_text: Some("active chain".to_string()),
                    deep_task: false,
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
                worker_lanes: Vec::new(),
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
        worker_lanes: Vec::new(),
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
                    deep_task: true,
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
                worker_lanes: Vec::new(),
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
                    deep_task: true,
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
        "timeline-rejected-chat",
        TimelineEntryKind::UserMessage,
        "不应写入的用户消息",
        UtcMillis(2),
        test_turn("turn-next", "running", 2),
    );

    assert!(matches!(
        result,
        Err(magi_core::DomainError::InvalidState { .. })
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
            "timeline-atomic-task-accept",
            TimelineEntryKind::UserMessage,
            "任务用户消息",
            UtcMillis(3),
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
        "timeline-rejected-task",
        TimelineEntryKind::UserMessage,
        "不应写入的任务消息",
        UtcMillis(4),
        chain,
    );

    assert!(matches!(
        result,
        Err(magi_core::DomainError::InvalidState { .. })
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
        Err(magi_core::DomainError::InvalidState { .. })
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
            "timeline-append-item",
            TimelineEntryKind::UserMessage,
            "继续用户消息",
            UtcMillis(2),
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
    conflicting_item.tool_name = Some("shell".to_string());
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
fn append_current_turn_item_with_timeline_entry_rejects_conflict_without_timeline_write() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-canonical-conflict-timeline");
    store
        .create_session(session_id.clone(), "Canonical Conflict Timeline")
        .expect("session should be creatable");
    store
        .upsert_current_turn(session_id.clone(), test_turn("turn-running", "running", 1))
        .expect("running turn should upsert");

    let mut base_item = test_turn_item("turn-item-lane-conflict", "主线回复");
    base_item.kind = "assistant_stream".to_string();
    base_item.status = "running".to_string();
    store
        .upsert_current_turn_item(&session_id, base_item)
        .expect("base item should upsert");

    let mut conflicting_item = test_turn_item("turn-item-lane-conflict", "worker 回复");
    conflicting_item.kind = "assistant_stream".to_string();
    conflicting_item.status = "running".to_string();
    conflicting_item.lane_id = Some("worker-lane".to_string());
    let result = store.append_current_turn_item_with_timeline_entry(
        &session_id,
        "timeline-conflict-should-not-write",
        TimelineEntryKind::AssistantMessage,
        "不应写入的 timeline",
        UtcMillis(2),
        conflicting_item,
    );

    assert!(matches!(
        result,
        Err(magi_core::DomainError::InvalidState { .. })
    ));
    assert!(
        !store
            .timeline_for_session(&session_id)
            .iter()
            .any(|entry| entry.entry_id == "timeline-conflict-should-not-write"),
        "canonical item 冲突时不能留下 timeline 写入"
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
            "timeline-no-turn",
            TimelineEntryKind::UserMessage,
            "不应写入的继续消息",
            UtcMillis(2),
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
fn legacy_recovery_ref_json_deserializes() {
    let payload = json!({
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

    let state: SessionStoreState = serde_json::from_value(payload).expect("legacy payload");
    let sidecar = state
        .execution_sidecar_store
        .runtime_sidecars
        .first()
        .expect("sidecar should exist");
    assert_eq!(sidecar.recovery_id.as_deref(), Some("recovery-legacy"));
    assert_eq!(sidecar.status, SessionExecutionSidecarStatus::Detached);
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
                }],
                recovery_ref: None,
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: UtcMillis::now(),
                    entry_id: "timeline-resume-preserve".to_string(),
                    trimmed_text: Some("resume preserve".to_string()),
                    deep_task: true,
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
