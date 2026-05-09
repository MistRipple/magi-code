use super::{
    config::DaemonConfig,
    events::ledger_status_payload,
    maintenance::{
        RuntimeMaintenanceStepOutcome, RuntimeMaintenance, RuntimeMaintenanceConfig,
        RuntimeMaintenancePolicy, session_sidecar_flush_due, workspace_sidecar_flush_due,
    },
    persistence::{
        RuntimeSidecarFlushReport, RuntimeSidecarPersistence, StateRepository,
    },
    runtime::DaemonRuntime,
    types::{DaemonMaintenanceMode, DaemonMaintenancePolicyProfile},
};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use magi_core::{
    AbsolutePath, EventId, ExecutionOwnership, LeaseId, MissionId, SessionId, Task, TaskId,
    TaskKind, TaskStatus, UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::{EventEnvelope, InMemoryEventBus, RuntimeLedgerSummary};
use magi_session_store::{
    ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
    ActiveExecutionTurn, ActiveExecutionTurnItem, SessionExecutionSidecarStatus,
    SessionSidecarFlushMetadata, SessionSidecarFlushReason, SessionStore,
};
use magi_worker_runtime::{WorkerExecutionBindingLifecycle, WorkerRuntime, WorkerStage};
use magi_workspace::{
    RecoveryStatus, WorkspaceRecoveryFlushMetadata, WorkspaceRecoveryFlushReason, WorkspaceStore,
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, sync::Arc};
use tokio::time::{Duration, Instant};
use tower::util::ServiceExt;

fn temp_state_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("magi-daemon-test-{name}-{}", UtcMillis::now().0));
    fs::create_dir_all(&root).expect("temp state root should be creatable");
    root
}

fn temp_workspace_absolute_path(name: &str) -> AbsolutePath {
    AbsolutePath::new(temp_state_root(name).to_string_lossy().to_string())
}

fn test_sidecar_persistence(
    repository: StateRepository,
    session_store: Arc<SessionStore>,
    workspace_store: Arc<WorkspaceStore>,
) -> RuntimeSidecarPersistence {
    test_sidecar_persistence_with_worker_runtime(
        repository,
        session_store,
        workspace_store,
        WorkerRuntime::new_compare(Arc::new(InMemoryEventBus::new(64))),
    )
}

fn test_sidecar_persistence_with_worker_runtime(
    repository: StateRepository,
    session_store: Arc<SessionStore>,
    workspace_store: Arc<WorkspaceStore>,
    worker_runtime: WorkerRuntime,
) -> RuntimeSidecarPersistence {
    RuntimeSidecarPersistence::new(repository, session_store, workspace_store, worker_runtime)
}

async fn post_json(app: axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request should build"),
        )
        .await
        .expect("router should respond");
    let status = response.status();
    let body = serde_json::from_slice(
        &to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should read"),
    )
    .expect("response should be valid json");
    (status, body)
}

async fn get_json(app: axum::Router, path: &str) -> Value {
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond");
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(
        &to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should read"),
    )
    .expect("response should be valid json")
}

async fn get_task_projection(app: axum::Router, root_task_id: &str, session_id: &str) -> Value {
    get_json(
        app,
        &format!("/api/tasks/graph/{root_task_id}?sessionId={session_id}"),
    )
    .await
}

async fn wait_for_task_projection_completed(
    app: axum::Router,
    root_task_id: &str,
    session_id: &str,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let projection = get_task_projection(app.clone(), root_task_id, session_id).await;
        let total_tasks = projection["progress_summary"]["total_tasks"]
            .as_u64()
            .unwrap_or(0);
        let completed_tasks = projection["progress_summary"]["completed_tasks"]
            .as_u64()
            .unwrap_or(0);
        if total_tasks >= 2
            && completed_tasks == total_tasks
            && projection["root_task"]["status"] == "Completed"
        {
            return projection;
        }
        if Instant::now() >= deadline {
            return projection;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_execution_group(
    app: axum::Router,
    mission_id: &str,
    mut is_ready: impl FnMut(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let read_model = get_json(app.clone(), "/runtime/read-model").await;
        if let Some(group) = read_model["details"]["execution_groups"]
            .as_array()
            .and_then(|groups| {
                groups
                    .iter()
                    .find(|entry| entry["mission_id"] == mission_id)
            })
        {
            if is_ready(group) || Instant::now() >= deadline {
                return group.clone();
            }
        }
        if Instant::now() >= deadline {
            panic!("execution group {mission_id} did not appear before timeout");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn assert_completed_two_task_projection(projection: &Value) {
    let total_tasks = projection["progress_summary"]["total_tasks"]
        .as_u64()
        .expect("total_tasks should serialize as integer");
    let completed_tasks = projection["progress_summary"]["completed_tasks"]
        .as_u64()
        .expect("completed_tasks should serialize as integer");
    assert!(
        total_tasks >= 2,
        "task projection should include at least root + action"
    );
    assert_eq!(
        completed_tasks, total_tasks,
        "task projection should be fully completed, got summary={:?}, root_status={}, tasks={:?}",
        projection["progress_summary"], projection["root_task"]["status"], projection["tasks"]
    );
    assert_eq!(projection["progress_summary"]["failed_tasks"], 0);
    assert_eq!(projection["root_task"]["status"], "Completed");
}

#[test]
fn legacy_session_file_can_seed_sidecar_file_loading() {
    let state_root = temp_state_root("legacy-session-sidecar");
    let repository = StateRepository::new(state_root.clone());
    let legacy_payload = serde_json::json!({
        "current_session_id": "session-1",
        "sessions": [],
        "timeline": [],
        "notifications": [],
        "runtime_sidecars": [{
            "session_id": "session-1",
            "ownership": {
                "session_id": "session-1",
                "workspace_id": null,
                "mission_id": null,
                "task_id": null,
                "worker_id": null,
                "execution_chain_ref": "chain-1"
            },
            "recovery_id": "recovery-1",
            "status": "RecoveryLinked",
            "updated_at": 1
        }]
    });
    fs::write(
        state_root.join("sessions.json"),
        serde_json::to_vec_pretty(&legacy_payload).expect("legacy payload should serialize"),
    )
    .expect("legacy session state should be writable");

    let sidecars = repository
        .load_session_sidecars()
        .expect("legacy session sidecars should load");
    assert_eq!(sidecars.runtime_sidecars.len(), 1);
    assert_eq!(
        sidecars
            .runtime_sidecars
            .first()
            .map(|sidecar| &sidecar.status),
        Some(&SessionExecutionSidecarStatus::RecoveryLinked)
    );
}

#[test]
fn legacy_workspace_file_can_seed_recovery_sidecar_loading() {
    let state_root = temp_state_root("legacy-workspace-sidecar");
    let repository = StateRepository::new(state_root.clone());
    let legacy_payload = serde_json::json!({
        "active_workspace_id": "workspace-1",
        "workspaces": [],
        "worktree_allocations": [],
        "snapshots": [],
        "recovery_handles": [{
            "recovery_id": "recovery-1",
            "workspace_id": "workspace-1",
            "ownership": {
                "session_id": "session-1",
                "workspace_id": "workspace-1",
                "mission_id": null,
                "task_id": null,
                "worker_id": null,
                "execution_chain_ref": "chain-1"
            },
            "snapshot_id": "snapshot-1",
            "diagnostic_summary": "legacy",
            "status": "Ready",
            "created_at": 1,
            "updated_at": 2,
            "consumed_at": null
        }]
    });
    fs::write(
        state_root.join("workspaces.json"),
        serde_json::to_vec_pretty(&legacy_payload).expect("legacy payload should serialize"),
    )
    .expect("legacy workspace state should be writable");

    let sidecars = repository
        .load_workspace_recovery_sidecars()
        .expect("legacy workspace sidecars should load");
    assert_eq!(sidecars.recovery_handles.len(), 1);
    assert_eq!(
        sidecars
            .recovery_handles
            .first()
            .map(|handle| &handle.status),
        Some(&RecoveryStatus::Ready)
    );
}

#[test]
fn ledger_status_payload会稳定暴露路径与持久化错误() {
    let payload = ledger_status_payload(&RuntimeLedgerSummary {
        schema_version: "audit-usage-ledger-v1".to_string(),
        next_sequence: 7,
        audit_count: 3,
        usage_count: 2,
        persistence_path: Some("/tmp/magi-test-ledger.json".to_string()),
        last_persist_error: Some("persist failed".to_string()),
        is_persist_healthy: false,
        last_persisted_at: Some(UtcMillis(11)),
        pending_flush: true,
        readiness: magi_event_bus::RuntimeLedgerReadinessSummary {
            is_ready: false,
            blocking_issue_count: 2,
            blocking_issues: vec![
                "ledger persistence path missing".to_string(),
                "ledger persistence is unhealthy".to_string(),
            ],
        },
        cutover_readiness: magi_event_bus::RuntimeLedgerReadinessSummary {
            is_ready: false,
            blocking_issue_count: 4,
            blocking_issues: vec![
                "ledger persistence path missing".to_string(),
                "ledger persistence is unhealthy".to_string(),
                "ledger has pending flush".to_string(),
                "ledger has not been persisted yet".to_string(),
            ],
        },
    });

    assert_eq!(payload["schema_version"], "audit-usage-ledger-v1");
    assert_eq!(payload["audit_count"], 3);
    assert_eq!(payload["usage_count"], 2);
    assert_eq!(payload["next_sequence"], 7);
    assert_eq!(payload["persistence_path"], "/tmp/magi-test-ledger.json");
    assert_eq!(payload["last_persist_error"], "persist failed");
    assert_eq!(payload["is_persist_healthy"], false);
    assert_eq!(payload["last_persisted_at"], 11);
    assert_eq!(payload["pending_flush"], true);
    assert_eq!(payload["readiness"]["is_ready"], false);
    assert_eq!(payload["readiness"]["blocking_issue_count"], 2);
    assert_eq!(payload["cutover_readiness"]["is_ready"], false);
    assert_eq!(payload["cutover_readiness"]["blocking_issue_count"], 4);
}

#[test]
fn sidecar_flush_due_uses_metadata_hint_and_dirty_state() {
    let now = UtcMillis(100);
    assert!(!session_sidecar_flush_due(
        &SessionSidecarFlushMetadata::default(),
        now
    ));
    assert!(session_sidecar_flush_due(
        &SessionSidecarFlushMetadata {
            current_version: 2,
            flushed_version: 1,
            last_dirty_reason: Some(SessionSidecarFlushReason::BindExecutionOwnership),
            last_dirty_at: Some(UtcMillis(90)),
            next_flush_hint: Some(UtcMillis(95)),
            last_flush_at: None,
        },
        now,
    ));
    assert!(!workspace_sidecar_flush_due(
        &WorkspaceRecoveryFlushMetadata {
            current_version: 2,
            flushed_version: 1,
            last_dirty_at: Some(UtcMillis(90)),
            last_dirty_reason: Some(WorkspaceRecoveryFlushReason::PrepareRecoveryEntry),
            last_flush_at: None,
            next_flush_hint: Some(UtcMillis(110)),
        },
        now,
    ));
}

#[test]
fn runtime_sidecar_flush_hook_only_persists_dirty_sidecars() {
    let state_root = temp_state_root("runtime-sidecar-flush");
    let repository = StateRepository::new(state_root);
    let session_store = Arc::new(SessionStore::new());
    let workspace_store = Arc::new(WorkspaceStore::new());
    let persistence = test_sidecar_persistence(
        repository.clone(),
        session_store.clone(),
        workspace_store.clone(),
    );

    session_store
        .create_session(SessionId::new("session-flush"), "flush session")
        .expect("session should be creatable");
    workspace_store
        .register(
            WorkspaceId::new("workspace-flush"),
            temp_workspace_absolute_path("runtime-sidecar-flush-workspace"),
        )
        .expect("workspace should be registrable");

    assert_eq!(
        persistence
            .flush_runtime_sidecars()
            .expect("clean sidecar flush should succeed"),
        RuntimeSidecarFlushReport::default()
    );

    session_store.bind_execution_ownership(
        SessionId::new("session-flush"),
        ExecutionOwnership {
            session_id: Some(SessionId::new("session-flush")),
            workspace_id: Some(WorkspaceId::new("workspace-flush")),
            execution_chain_ref: Some("chain-flush".to_string()),
            ..ExecutionOwnership::default()
        },
    );
    let snapshot = workspace_store.append_execution_snapshot(
        WorkspaceId::new("workspace-flush"),
        ExecutionOwnership {
            session_id: Some(SessionId::new("session-flush")),
            workspace_id: Some(WorkspaceId::new("workspace-flush")),
            execution_chain_ref: Some("chain-flush".to_string()),
            ..ExecutionOwnership::default()
        },
        "snapshot-flush",
        "flush snapshot",
    );
    workspace_store.prepare_recovery_entry(
        WorkspaceId::new("workspace-flush"),
        ExecutionOwnership {
            session_id: Some(SessionId::new("session-flush")),
            workspace_id: Some(WorkspaceId::new("workspace-flush")),
            execution_chain_ref: Some("chain-flush".to_string()),
            ..ExecutionOwnership::default()
        },
        snapshot.snapshot_id,
        "recovery-flush",
        None,
    );

    assert_eq!(
        persistence
            .flush_runtime_sidecars()
            .expect("dirty sidecar flush should succeed"),
        RuntimeSidecarFlushReport {
            session_sidecars_flushed: true,
            workspace_recovery_sidecars_flushed: true,
            worker_runtime_snapshot_flushed: false,
        }
    );
    assert!(repository.session_sidecars_path().exists());
    assert!(repository.workspace_recovery_sidecars_path().exists());
    assert_eq!(
        persistence
            .flush_runtime_sidecars()
            .expect("clean sidecar flush should be skipped"),
        RuntimeSidecarFlushReport::default()
    );
}

#[test]
fn runtime_sidecar_flush_persists_canonical_turns_to_session_durable_state() {
    let state_root = temp_state_root("runtime-sidecar-flush-canonical");
    let workspace_root = temp_state_root("runtime-sidecar-flush-canonical-workspace");
    let repository = StateRepository::new(state_root);
    let session_store = Arc::new(SessionStore::new());
    let workspace_store = Arc::new(WorkspaceStore::new());
    let persistence = test_sidecar_persistence(
        repository.clone(),
        session_store.clone(),
        workspace_store.clone(),
    );
    let session_id = SessionId::new("session-flush-canonical");
    let workspace_id = WorkspaceId::new("workspace-flush-canonical");

    workspace_store
        .register(
            workspace_id.clone(),
            AbsolutePath::new(workspace_root.to_string_lossy().to_string()),
        )
        .expect("workspace should be registrable");
    session_store
        .create_session_for_workspace(
            session_id.clone(),
            "flush canonical session",
            Some(workspace_id.to_string()),
        )
        .expect("workspace session should be creatable");
    session_store
        .upsert_current_turn(
            session_id.clone(),
            ActiveExecutionTurn {
                turn_id: "turn-flush-canonical".to_string(),
                turn_seq: 10,
                accepted_at: UtcMillis(10),
                completed_at: None,
                status: "running".to_string(),
                user_message: Some("关闭前端后继续执行".to_string()),
                items: vec![ActiveExecutionTurnItem {
                    item_id: "turn-item-flush-canonical-assistant".to_string(),
                    item_seq: 1,
                    lane_id: None,
                    lane_seq: None,
                    kind: "assistant_stream".to_string(),
                    status: "running".to_string(),
                    source: "orchestrator".to_string(),
                    title: Some("最终回复".to_string()),
                    content: Some("后台仍在生成".to_string()),
                    task_id: None,
                    worker_id: None,
                    role_id: None,
                    tool_call_id: None,
                    tool_name: None,
                    tool_status: None,
                    tool_arguments: None,
                    tool_result: None,
                    tool_error: None,
                    request_id: Some("req-flush-canonical".to_string()),
                    user_message_id: Some("msg-flush-canonical".to_string()),
                    placeholder_message_id: Some(
                        "assistant-placeholder-flush-canonical".to_string(),
                    ),
                    timeline_entry_id: None,
                    thread_visible: true,
                    worker_visible: false,
                }],
                worker_lanes: Vec::new(),
            },
        )
        .expect("current turn should upsert");

    let report = persistence
        .flush_runtime_sidecars()
        .expect("sidecar flush should also persist canonical turns");
    assert!(report.session_sidecars_flushed);

    let workspace_sessions = repository
        .load_workspace_session_state(&workspace_root)
        .expect("workspace sessions should reload");
    assert_eq!(workspace_sessions.canonical_turns.len(), 1);
    assert_eq!(
        workspace_sessions.canonical_turns[0].turn_id,
        "turn-flush-canonical"
    );
    assert_eq!(workspace_sessions.canonical_turns[0].items.len(), 1);
}

#[test]
fn runtime_sidecar_flush_hook_persists_dirty_worker_runtime_snapshot() {
    let state_root = temp_state_root("runtime-worker-snapshot-flush");
    let repository = StateRepository::new(state_root.clone());
    let session_store = Arc::new(SessionStore::new());
    let workspace_store = Arc::new(WorkspaceStore::new());
    let worker_runtime = WorkerRuntime::new_compare(Arc::new(InMemoryEventBus::new(64)));
    let persistence = test_sidecar_persistence_with_worker_runtime(
        repository.clone(),
        session_store,
        workspace_store,
        worker_runtime.clone(),
    );

    worker_runtime.record_branch_checkpoint(
        &TaskId::new("task-worker-snapshot"),
        &WorkerId::new("worker-worker-snapshot"),
        WorkerStage::Execute,
        Some("lease-worker-snapshot".to_string()),
        Some("worker-intent-task-worker-snapshot".to_string()),
        Some(WorkerExecutionBindingLifecycle::Requested),
        Some(magi_worker_runtime::WorkerExecutionCheckpointCursor {
            checkpoint_stage: WorkerStage::Execute,
            next_step_index: 1,
            checkpoint_at: UtcMillis::now(),
            resume_mode: magi_worker_runtime::WorkerCheckpointResumeMode::StepCheckpoint,
            resume_token: None,
        }),
    );

    assert_eq!(
        persistence
            .flush_runtime_sidecars()
            .expect("dirty worker snapshot flush should succeed"),
        RuntimeSidecarFlushReport {
            session_sidecars_flushed: false,
            workspace_recovery_sidecars_flushed: false,
            worker_runtime_snapshot_flushed: true,
        }
    );
    assert!(repository.worker_runtime_snapshot_path().exists());

    let reloaded = repository
        .load_worker_runtime_snapshot()
        .expect("worker runtime snapshot should reload");
    assert_eq!(reloaded.branches.len(), 1);
    assert_eq!(
        reloaded.branches[0].task_id,
        TaskId::new("task-worker-snapshot")
    );

    assert_eq!(
        persistence
            .flush_runtime_sidecars()
            .expect("clean worker snapshot flush should be skipped"),
        RuntimeSidecarFlushReport::default()
    );
}

#[test]
fn maintenance_tick_flushes_worker_snapshot_even_when_sidecars_are_clean() {
    let state_root = temp_state_root("maintenance-worker-snapshot-only");
    let repository = StateRepository::new(state_root.clone());
    let session_store = Arc::new(SessionStore::new());
    let workspace_store = Arc::new(WorkspaceStore::new());
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let worker_runtime = WorkerRuntime::new_compare(Arc::new(InMemoryEventBus::new(64)));
    let persistence = test_sidecar_persistence_with_worker_runtime(
        repository.clone(),
        session_store.clone(),
        workspace_store.clone(),
        worker_runtime.clone(),
    );

    worker_runtime.record_branch_checkpoint(
        &TaskId::new("task-maintenance-worker"),
        &WorkerId::new("worker-maintenance-worker"),
        WorkerStage::Verify,
        Some("lease-maintenance-worker".to_string()),
        Some("worker-intent-task-maintenance-worker".to_string()),
        Some(WorkerExecutionBindingLifecycle::Requested),
        Some(magi_worker_runtime::WorkerExecutionCheckpointCursor {
            checkpoint_stage: WorkerStage::Verify,
            next_step_index: 1,
            checkpoint_at: UtcMillis::now(),
            resume_mode: magi_worker_runtime::WorkerCheckpointResumeMode::StageRestart,
            resume_token: None,
        }),
    );

    let maintenance = RuntimeMaintenance::new(
        RuntimeMaintenanceConfig::default(),
        event_bus,
        persistence,
        session_store,
        workspace_store,
    );

    let report = maintenance
        .run_once()
        .expect("maintenance tick should flush dirty worker snapshot");
    assert_eq!(
        report.sidecar_report.outcome,
        RuntimeMaintenanceStepOutcome::DueAndFlushed
    );
    assert!(
        report
            .sidecar_report
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("worker_runtime_snapshot_flushed=true"),
        "maintenance detail should report worker snapshot flush"
    );
    assert!(repository.worker_runtime_snapshot_path().exists());
}

#[test]
fn recovery_consume_updates_sidecars_can_be_flushed_incrementally() {
    let state_root = temp_state_root("recovery-sidecar-incremental");
    let repository = StateRepository::new(state_root);
    let session_store = Arc::new(SessionStore::new());
    let workspace_store = Arc::new(WorkspaceStore::new());
    let persistence = test_sidecar_persistence(
        repository.clone(),
        session_store.clone(),
        workspace_store.clone(),
    );

    let session_id = SessionId::new("session-recovery");
    let workspace_id = WorkspaceId::new("workspace-recovery");
    session_store
        .create_session(session_id.clone(), "recovery session")
        .expect("session should be creatable");
    workspace_store
        .register(
            workspace_id.clone(),
            temp_workspace_absolute_path("recovery-sidecar-incremental-workspace"),
        )
        .expect("workspace should be registrable");

    session_store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            execution_chain_ref: Some("chain-recovery".to_string()),
            ..ExecutionOwnership::default()
        },
    );
    let snapshot = workspace_store.append_execution_snapshot(
        workspace_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            execution_chain_ref: Some("chain-recovery".to_string()),
            ..ExecutionOwnership::default()
        },
        "snapshot-recovery",
        "recovery snapshot",
    );
    let recovery = workspace_store.prepare_recovery_entry(
        workspace_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            execution_chain_ref: Some("chain-recovery".to_string()),
            ..ExecutionOwnership::default()
        },
        snapshot.snapshot_id,
        "recovery-sidecar",
        Some("diagnostic".to_string()),
    );
    session_store
        .attach_recovery_ref(&session_id, Some(recovery.recovery_id.clone()))
        .expect("recovery ref should be attachable");
    persistence
        .flush_runtime_sidecars()
        .expect("initial sidecar flush should succeed");

    workspace_store
        .mark_recovery_ready(&recovery.recovery_id)
        .expect("recovery should become ready");
    let resume_input = workspace_store
        .build_recovery_resume_input(&recovery.recovery_id)
        .expect("resume input should build");
    workspace_store
        .consume_recovery(&recovery.recovery_id)
        .expect("recovery should be consumable");
    session_store
        .apply_recovery_resume_input(session_id.clone(), resume_input)
        .expect("resume input should sync session sidecar");

    assert_eq!(
        persistence
            .flush_runtime_sidecars()
            .expect("incremental sidecar flush should succeed"),
        RuntimeSidecarFlushReport {
            session_sidecars_flushed: true,
            workspace_recovery_sidecars_flushed: true,
            worker_runtime_snapshot_flushed: false,
        }
    );

    let reloaded_session_sidecars = repository
        .load_session_sidecars()
        .expect("session sidecars should reload");
    let reloaded_workspace_sidecars = repository
        .load_workspace_recovery_sidecars()
        .expect("workspace sidecars should reload");
    assert_eq!(
        reloaded_session_sidecars
            .runtime_sidecars
            .first()
            .map(|sidecar| &sidecar.status),
        Some(&SessionExecutionSidecarStatus::RecoveryLinked)
    );
    assert_eq!(
        reloaded_workspace_sidecars
            .recovery_handles
            .first()
            .map(|handle| &handle.status),
        Some(&RecoveryStatus::Consumed)
    );
}

#[tokio::test]
async fn daemon_runtime_recovery_preflight_executes_and_followup_router_dispatch_consumes_writeback()
 {
    let state_root = temp_state_root("router-recovery-preflight");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    let runtime = DaemonRuntime::restore(&config)
        .expect("runtime restore should bootstrap empty state");
    let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());
    let pipeline = state
        .execution_pipeline()
        .expect("daemon runtime should expose execution pipeline");

    let mission_id = MissionId::new("mission-router-recovery");
    let task_id = TaskId::new("task-router-recovery");
    let session_id = SessionId::new("session-router-recovery");
    let workspace_id = WorkspaceId::new("workspace-router-recovery");
    let worker_id = WorkerId::new("worker-router-recovery");
    let execution_chain_ref = "chain-router-recovery".to_string();

    state
        .session_store
        .create_session(session_id.clone(), "router recovery session")
        .expect("session should be creatable");
    state
        .workspace_registry
        .register(
            workspace_id.clone(),
            temp_workspace_absolute_path("router-recovery-workspace"),
        )
        .expect("workspace should be registrable");
    state.session_store.bind_execution_ownership(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            mission_id: Some(mission_id.clone()),
            task_id: Some(task_id.clone()),
            worker_id: Some(worker_id.clone()),
            execution_chain_ref: Some(execution_chain_ref.clone()),
        },
    );
    state
        .session_store
        .upsert_active_execution_chain(
            session_id.clone(),
            ActiveExecutionChain {
                session_id: session_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: TaskId::new("task-root-router-recovery"),
                execution_chain_ref: execution_chain_ref.clone(),
                workspace_id: Some(workspace_id.clone()),
                active_branch_task_ids: vec![task_id.clone()],
                active_worker_bindings: vec![worker_id.clone()],
                branches: vec![ActiveExecutionBranch {
                    task_id: task_id.clone(),
                    worker_id: worker_id.clone(),
                    stage: "blocked".to_string(),
                    lease_id: None,
                    execution_intent_ref: None,
                    binding_lifecycle: Some("active".to_string()),
                    checkpoint_stage: Some("execute".to_string()),
                    next_step_index: Some(1),
                    checkpoint_at: Some(UtcMillis::now()),
                    resume_mode: Some("step-checkpoint".to_string()),
                    resume_token: None,
                    is_primary: true,
                    use_tools: true,
                    skill_name: Some("resume".to_string()),
                }],
                recovery_ref: Some("recovery-router-recovery".to_string()),
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: UtcMillis::now(),
                    entry_id: "timeline-router-recovery".to_string(),
                    trimmed_text: Some("resume parser after crash".to_string()),
                    deep_task: true,
                    skill_name: Some("resume".to_string()),
                },
                current_turn: None,
            },
        )
        .expect("active execution chain should attach");

    let recovery_handle = state.workspace_registry.prepare_recovery_entry(
        workspace_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            mission_id: Some(mission_id.clone()),
            task_id: Some(task_id.clone()),
            worker_id: Some(worker_id.clone()),
            execution_chain_ref: Some(execution_chain_ref.clone()),
        },
        "snapshot-router-recovery",
        "recovery-router-recovery",
        Some("resume parser after crash".to_string()),
    );
    state
        .workspace_registry
        .mark_recovery_ready(&recovery_handle.recovery_id)
        .expect("recovery should be ready");

    let task_store = state.task_store().expect("task store should be configured");
    let root_task_id = TaskId::new("task-root-router-recovery");
    let now = UtcMillis::now();
    task_store.insert_task(Task {
        task_id: root_task_id.clone(),
        mission_id: mission_id.clone(),
        root_task_id: root_task_id.clone(),
        parent_task_id: None,
        kind: TaskKind::Objective,
        title: "recovery mission".to_string(),
        goal: "recovery mission".to_string(),
        status: TaskStatus::Running,
        dependency_ids: Vec::new(),
        required_children: vec![task_id.clone()],
        policy_snapshot: None,
        executor_binding: None,
        context_refs: Vec::new(),
        knowledge_refs: Vec::new(),
        workspace_scope: None,
        write_scope: None,
        input_refs: Vec::new(),
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        repair_count: 0,
        decision_payload: None,
        created_at: now,
        updated_at: now,
    });
    task_store.insert_task(Task {
        task_id: task_id.clone(),
        mission_id: mission_id.clone(),
        root_task_id: root_task_id,
        parent_task_id: Some(TaskId::new("task-root-router-recovery")),
        kind: TaskKind::Action,
        title: "recovery task".to_string(),
        goal: "recovery task".to_string(),
        status: TaskStatus::Blocked,
        dependency_ids: Vec::new(),
        required_children: Vec::new(),
        policy_snapshot: None,
        executor_binding: None,
        context_refs: Vec::new(),
        knowledge_refs: Vec::new(),
        workspace_scope: None,
        write_scope: None,
        input_refs: Vec::new(),
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        repair_count: 0,
        decision_payload: None,
        created_at: now,
        updated_at: now,
    });

    let expected_extraction_id =
        format!("extract-session-continue-{}", recovery_handle.recovery_id);
    let (status, recovery_body) = post_json(
        app.clone(),
        "/api/session/continue",
        json!({
            "sessionId": session_id.to_string(),
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected recovery response body: {recovery_body:?}"
    );
    assert_eq!(recovery_body["sessionId"], session_id.to_string());
    assert_eq!(recovery_body["missionId"], mission_id.to_string());
    assert_eq!(
        recovery_body["rootTaskId"],
        TaskId::new("task-root-router-recovery").to_string()
    );
    assert_eq!(recovery_body["status"], "continued");

    let verification = pipeline
        .memory_store
        .verify_extraction_linkage(&expected_extraction_id)
        .expect("recovery writeback should persist extraction linkage");
    assert!(verification.is_consistent);
    let linkage = pipeline
        .memory_store
        .extraction_linkage(&expected_extraction_id)
        .expect("recovery extraction linkage should exist");
    assert_eq!(
        linkage.extraction.source_ref.as_deref(),
        Some("session-continue://recovery-router-recovery/snapshot/snapshot-router-recovery")
    );
    assert_eq!(
        linkage.produced_records[0].content,
        "resume parser after crash"
    );

    let first_read_model = get_json(app.clone(), "/runtime/read-model").await;
    let recovery_summary = first_read_model["recovery"]["summaries"]
        .as_array()
        .expect("recovery summaries should be an array")
        .iter()
        .find(|entry| entry["recovery_id"] == "recovery-router-recovery")
        .expect("recovery summary should exist");
    assert_eq!(recovery_summary["current_status"], "consumed");
    assert_eq!(
        recovery_summary["diagnostic_summary"],
        "resume parser after crash"
    );
    let session_summary = first_read_model["details"]["sessions"]
        .as_array()
        .expect("session summaries should be an array")
        .iter()
        .find(|entry| entry["session_id"] == session_id.to_string())
        .expect("session summary should exist");
    assert_eq!(session_summary["current_status"], "resumed");
    assert!(session_summary["recovery_ref"].is_null());
    let workspace_summary = first_read_model["details"]["workspaces"]
        .as_array()
        .expect("workspace summaries should be an array")
        .iter()
        .find(|entry| entry["workspace_id"] == workspace_id.to_string())
        .expect("workspace summary should exist");
    assert_eq!(workspace_summary["current_status"], "consumed");
    assert_eq!(
        workspace_summary["recovery_ref"],
        "recovery-router-recovery"
    );

    let (status, followup_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "session_id": session_id.to_string(),
            "text": "follow up recovery task",
            "deep_task": false,
            "skill_name": "resume",
            "images": [],
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected followup response body: {followup_body:?}"
    );

    let accepted_at = followup_body["acceptedAt"]
        .as_u64()
        .expect("accepted_at should serialize as integer");
    let followup_mission_id = format!("mission-session-action-{accepted_at}");
    let followup_root_task_id = followup_body["rootTaskId"]
        .as_str()
        .expect("root_task_id should serialize as string");
    let followup_execution_group =
        wait_for_execution_group(app.clone(), &followup_mission_id, |entry| {
            entry["context_memory_extraction_refs"]
                .as_array()
                .is_some_and(|refs| refs.iter().any(|value| value == &expected_extraction_id))
        })
        .await;
    assert!(
        followup_execution_group["context_used_memory_count"]
            .as_u64()
            .expect("used memory count should serialize as integer")
            >= 1
    );
    assert!(
        followup_execution_group["context_extracted_memory_count"]
            .as_u64()
            .expect("extracted memory count should serialize as integer")
            >= 1
    );
    assert!(
        followup_execution_group["context_memory_extraction_refs"]
            .as_array()
            .expect("context memory extraction refs should serialize as array")
            .iter()
            .any(|value| value == &expected_extraction_id)
    );
    let followup_projection =
        wait_for_task_projection_completed(app, followup_root_task_id, session_id.as_str()).await;
    assert_completed_two_task_projection(&followup_projection);
}

#[tokio::test]
async fn daemon_bootstrap_exports_session_action_context_summary_after_followup_dispatch() {
    let state_root = temp_state_root("router-bootstrap-context-summary");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    let runtime = DaemonRuntime::restore(&config)
        .expect("runtime restore should bootstrap empty state");
    let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());
    let session_id = SessionId::new("test-session-bootstrap");
    let active_workspace_id = state
        .workspace_registry
        .active_workspace_id()
        .expect("bootstrap workspace should exist");
    state
        .session_store
        .create_session_for_workspace(
            session_id.clone(),
            "bootstrap session".to_string(),
            Some(active_workspace_id.to_string()),
        )
        .expect("bootstrap session should be creatable");

    let (status, first_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "session_id": "test-session-bootstrap",
            "text": "Route parser refresh",
            "deep_task": false,
            "skill_name": "refactor",
            "images": [],
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected first body: {first_body:?}"
    );

    let first_accepted_at = first_body["acceptedAt"]
        .as_u64()
        .expect("accepted_at should serialize as integer");
    let expected_extraction_id = format!("extract-session-action-{first_accepted_at}");
    let first_root_task_id = first_body["rootTaskId"]
        .as_str()
        .expect("root_task_id should serialize as string");
    let first_projection =
        wait_for_task_projection_completed(app.clone(), first_root_task_id, session_id.as_str())
            .await;
    assert_completed_two_task_projection(&first_projection);

    let (status, second_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "session_id": "test-session-bootstrap",
            "text": "Route parser refresh followup",
            "deep_task": false,
            "skill_name": "refactor",
            "images": [],
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected second body: {second_body:?}"
    );

    let second_accepted_at = second_body["acceptedAt"]
        .as_u64()
        .expect("accepted_at should serialize as integer");
    let second_mission_id = format!("mission-session-action-{second_accepted_at}");
    let second_root_task_id = second_body["rootTaskId"]
        .as_str()
        .expect("root_task_id should serialize as string");
    let second_execution_group =
        wait_for_execution_group(app.clone(), &second_mission_id, |entry| {
            entry["context_memory_extraction_refs"] == json!([expected_extraction_id])
        })
        .await;
    assert_eq!(
        second_execution_group["context_memory_extraction_refs"],
        json!([expected_extraction_id])
    );
    let second_projection =
        wait_for_task_projection_completed(app.clone(), second_root_task_id, session_id.as_str())
            .await;
    assert_completed_two_task_projection(&second_projection);
    let bootstrap = get_json(app.clone(), "/bootstrap").await;
    let bootstrap_execution_group = bootstrap["runtimeReadModel"]["details"]["execution_groups"]
        .as_array()
        .expect("bootstrap execution groups should be an array")
        .iter()
        .find(|entry| entry["mission_id"] == second_mission_id)
        .expect("second execution group should exist in bootstrap runtime read model");
    assert_eq!(
        bootstrap_execution_group["context_memory_extraction_refs"],
        json!([expected_extraction_id])
    );
}

#[tokio::test]
async fn daemon_bootstrap_exports_recovery_context_after_resume_and_followup_dispatch() {
    let state_root = temp_state_root("router-bootstrap-recovery-context");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    let runtime = DaemonRuntime::restore(&config)
        .expect("runtime restore should bootstrap empty state");
    let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());
    let session_id = magi_core::SessionId::new("test-session-bootstrap-recovery");
    let active_workspace_id = state
        .workspace_registry
        .active_workspace_id()
        .expect("bootstrap workspace should exist");
    state
        .session_store
        .create_session_for_workspace(
            session_id.clone(),
            "bootstrap recovery session".to_string(),
            Some(active_workspace_id.to_string()),
        )
        .expect("bootstrap recovery session should be creatable");

    let (status, seed_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "session_id": "test-session-bootstrap-recovery",
            "text": "seed bootstrap recovery state",
            "deep_task": false,
            "skill_name": "resume",
            "images": [],
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected seed body: {seed_body:?}"
    );
    let seed_root_task_id = seed_body["rootTaskId"]
        .as_str()
        .expect("seed root task id should serialize as string")
        .to_string();

    let ownership = state
        .session_store
        .execution_ownership(&session_id)
        .expect("seed session action should bind execution ownership");
    let workspace_id = ownership
        .workspace_id
        .clone()
        .expect("seed execution ownership should include workspace");
    let recovery_task_id = ownership
        .task_id
        .clone()
        .expect("seed execution ownership should include task");
    state
        .task_store()
        .expect("task store should be configured")
        .update_status(&recovery_task_id, TaskStatus::Blocked)
        .expect("seed task should become recoverable");
    let snapshot = state.workspace_registry.append_execution_snapshot(
        workspace_id.clone(),
        ownership.clone(),
        "snapshot-bootstrap-recovery",
        "Bootstrap recovery snapshot",
    );
    let recovery = state.workspace_registry.prepare_recovery_entry(
        workspace_id,
        ownership,
        snapshot.snapshot_id,
        "recovery-bootstrap-route",
        Some("resume bootstrap route followup".to_string()),
    );
    state
        .workspace_registry
        .mark_recovery_ready(&recovery.recovery_id)
        .expect("recovery should become ready");
    state
        .session_store
        .attach_recovery_ref(&session_id, Some(recovery.recovery_id.clone()))
        .expect("recovery ref should attach to session");

    let (status, recovery_body) = post_json(
        app.clone(),
        "/api/session/continue",
        json!({
            "sessionId": session_id.to_string(),
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected recovery body: {recovery_body:?}"
    );
    assert_eq!(recovery_body["sessionId"], session_id.to_string());

    let expected_extraction_id = "extract-session-continue-recovery-bootstrap-route";
    let seed_projection =
        wait_for_task_projection_completed(app.clone(), &seed_root_task_id, session_id.as_str())
            .await;
    assert_completed_two_task_projection(&seed_projection);
    let after_resume_read_model = get_json(app.clone(), "/runtime/read-model").await;
    let after_resume_bootstrap = get_json(app.clone(), "/bootstrap").await;
    assert_eq!(
        after_resume_bootstrap["runtimeReadModel"],
        after_resume_read_model
    );
    let recovery_summary = after_resume_bootstrap["runtimeReadModel"]["recovery"]["summaries"]
        .as_array()
        .expect("bootstrap recovery summaries should be an array")
        .iter()
        .find(|entry| entry["recovery_id"] == "recovery-bootstrap-route")
        .expect("bootstrap recovery summary should exist");
    assert_eq!(recovery_summary["current_status"], "consumed");
    assert_eq!(
        recovery_summary["diagnostic_summary"],
        "resume bootstrap route followup"
    );

    let (status, followup_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "session_id": "test-session-bootstrap-recovery",
            "text": "consume resumed bootstrap memory",
            "deep_task": false,
            "skill_name": "resume",
            "images": [],
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected followup body: {followup_body:?}"
    );

    let followup_accepted_at = followup_body["acceptedAt"]
        .as_u64()
        .expect("accepted_at should serialize as integer");
    let followup_mission_id = format!("mission-session-action-{followup_accepted_at}");
    let followup_root_task_id = followup_body["rootTaskId"]
        .as_str()
        .expect("root_task_id should serialize as string");
    let followup_execution_group =
        wait_for_execution_group(app.clone(), &followup_mission_id, |entry| {
            entry["context_memory_extraction_refs"]
                .as_array()
                .is_some_and(|refs| refs.iter().any(|value| value == expected_extraction_id))
        })
        .await;
    assert!(
        followup_execution_group["context_used_memory_count"]
            .as_u64()
            .expect("used memory count should serialize as integer")
            >= 1
    );
    let extraction_refs = followup_execution_group["context_memory_extraction_refs"]
        .as_array()
        .expect("context memory extraction refs should serialize as array");
    assert!(
        extraction_refs
            .iter()
            .any(|value| value == expected_extraction_id),
        "bootstrap followup execution group should include recovery extraction ref, got {extraction_refs:?}"
    );
    let followup_projection =
        wait_for_task_projection_completed(app.clone(), followup_root_task_id, session_id.as_str())
            .await;
    assert_completed_two_task_projection(&followup_projection);
    let bootstrap = get_json(app.clone(), "/bootstrap").await;
    let bootstrap_execution_group = bootstrap["runtimeReadModel"]["details"]["execution_groups"]
        .as_array()
        .expect("bootstrap execution groups should be an array")
        .iter()
        .find(|entry| entry["mission_id"] == followup_mission_id)
        .expect("followup execution group should exist in bootstrap runtime read model");
    assert!(
        bootstrap_execution_group["context_memory_extraction_refs"]
            .as_array()
            .is_some_and(|refs| refs.iter().any(|value| value == expected_extraction_id)),
        "bootstrap followup execution group should include recovery extraction ref"
    );
}

#[test]
fn runtime_maintenance_tick_can_refresh_ledger_and_flush_due_sidecars() {
    let state_root = temp_state_root("runtime-maintenance");
    let repository = StateRepository::new(state_root.clone());
    let session_store = Arc::new(SessionStore::new());
    let workspace_store = Arc::new(WorkspaceStore::new());
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let blocking_parent = state_root.join("blocking-parent");
    fs::write(&blocking_parent, b"blocker").expect("blocking parent file should be writable");
    let invalid_ledger_path = blocking_parent.join("audit-usage-ledger.json");
    let valid_ledger_path = repository.audit_usage_ledger_path();
    event_bus.set_audit_usage_ledger_persistence(invalid_ledger_path);

    session_store
        .create_session(SessionId::new("session-maintenance"), "maintenance session")
        .expect("session should be creatable");
    workspace_store
        .register(
            WorkspaceId::new("workspace-maintenance"),
            temp_workspace_absolute_path("runtime-maintenance-workspace"),
        )
        .expect("workspace should be registrable");
    session_store.bind_execution_ownership(
        SessionId::new("session-maintenance"),
        ExecutionOwnership {
            session_id: Some(SessionId::new("session-maintenance")),
            workspace_id: Some(WorkspaceId::new("workspace-maintenance")),
            execution_chain_ref: Some("chain-maintenance".to_string()),
            ..ExecutionOwnership::default()
        },
    );
    let snapshot = workspace_store.append_execution_snapshot(
        WorkspaceId::new("workspace-maintenance"),
        ExecutionOwnership {
            session_id: Some(SessionId::new("session-maintenance")),
            workspace_id: Some(WorkspaceId::new("workspace-maintenance")),
            execution_chain_ref: Some("chain-maintenance".to_string()),
            ..ExecutionOwnership::default()
        },
        "snapshot-maintenance",
        "maintenance snapshot",
    );
    workspace_store.prepare_recovery_entry(
        WorkspaceId::new("workspace-maintenance"),
        ExecutionOwnership {
            session_id: Some(SessionId::new("session-maintenance")),
            workspace_id: Some(WorkspaceId::new("workspace-maintenance")),
            execution_chain_ref: Some("chain-maintenance".to_string()),
            ..ExecutionOwnership::default()
        },
        snapshot.snapshot_id,
        "recovery-maintenance",
        None,
    );

    let persistence = test_sidecar_persistence(
        repository.clone(),
        session_store.clone(),
        workspace_store.clone(),
    );
    let maintenance = RuntimeMaintenance::new(
        RuntimeMaintenanceConfig::default(),
        event_bus.clone(),
        persistence,
        session_store,
        workspace_store,
    );
    let _ = event_bus.publish(EventEnvelope::usage(
        EventId::new("usage-maintenance"),
        "tool.used",
        serde_json::json!({ "tool_name": "shell_exec", "status": "Succeeded" }),
    ));
    assert!(event_bus.runtime_ledger_summary().pending_flush);
    event_bus.set_audit_usage_ledger_persistence(valid_ledger_path);

    let report = maintenance
        .run_once()
        .expect("runtime maintenance tick should succeed");
    assert_eq!(
        report.sidecar_report.outcome,
        RuntimeMaintenanceStepOutcome::DueAndFlushed
    );
    assert_eq!(
        report.ledger_report.outcome,
        RuntimeMaintenanceStepOutcome::DueAndRefreshed
    );
    let ledger = event_bus.runtime_ledger_summary();
    assert!(ledger.is_persist_healthy);
    assert!(ledger.last_persisted_at.is_some());
    assert!(!ledger.pending_flush);
    assert!(repository.session_sidecars_path().exists());
    assert!(repository.workspace_recovery_sidecars_path().exists());
    assert!(repository.audit_usage_ledger_path().exists());
}

#[test]
fn runtime_maintenance_policy_can_skip_disabled_actions() {
    let state_root = temp_state_root("runtime-maintenance-disabled");
    let repository = StateRepository::new(state_root.clone());
    let session_store = Arc::new(SessionStore::new());
    let workspace_store = Arc::new(WorkspaceStore::new());
    let event_bus = Arc::new(InMemoryEventBus::new(32));

    session_store
        .create_session(SessionId::new("session-disabled"), "disabled session")
        .expect("session should be creatable");
    workspace_store
        .register(
            WorkspaceId::new("workspace-disabled"),
            temp_workspace_absolute_path("runtime-sidecar-disabled-workspace"),
        )
        .expect("workspace should be registrable");
    session_store.bind_execution_ownership(
        SessionId::new("session-disabled"),
        ExecutionOwnership {
            session_id: Some(SessionId::new("session-disabled")),
            workspace_id: Some(WorkspaceId::new("workspace-disabled")),
            execution_chain_ref: Some("chain-disabled".to_string()),
            ..ExecutionOwnership::default()
        },
    );
    let snapshot = workspace_store.append_execution_snapshot(
        WorkspaceId::new("workspace-disabled"),
        ExecutionOwnership {
            session_id: Some(SessionId::new("session-disabled")),
            workspace_id: Some(WorkspaceId::new("workspace-disabled")),
            execution_chain_ref: Some("chain-disabled".to_string()),
            ..ExecutionOwnership::default()
        },
        "snapshot-disabled",
        "disabled snapshot",
    );
    workspace_store.prepare_recovery_entry(
        WorkspaceId::new("workspace-disabled"),
        ExecutionOwnership {
            session_id: Some(SessionId::new("session-disabled")),
            workspace_id: Some(WorkspaceId::new("workspace-disabled")),
            execution_chain_ref: Some("chain-disabled".to_string()),
            ..ExecutionOwnership::default()
        },
        snapshot.snapshot_id,
        "recovery-disabled",
        None,
    );
    let _ = event_bus.publish(EventEnvelope::usage(
        EventId::new("usage-disabled"),
        "tool.used",
        serde_json::json!({ "tool_name": "shell_exec", "status": "Succeeded" }),
    ));
    let maintenance = RuntimeMaintenance::new(
        RuntimeMaintenanceConfig {
            policy: RuntimeMaintenancePolicy {
                profile: DaemonMaintenancePolicyProfile::Standard,
                tick_interval: Duration::from_millis(1),
                sidecar_flush_enabled: false,
                ledger_refresh_enabled: false,
                eager_flush_dirty_sidecars: false,
                refresh_ledger_when_unhealthy: false,
                refresh_ledger_when_never_persisted: false,
                force_flush_on_mode_transition: true,
                force_ledger_refresh_on_shutdown: true,
            },
        },
        event_bus,
        test_sidecar_persistence(repository, session_store.clone(), workspace_store.clone()),
        session_store,
        workspace_store,
    );

    let report = maintenance
        .run_once()
        .expect("disabled maintenance tick should succeed");
    assert_eq!(
        report.sidecar_report.outcome,
        RuntimeMaintenanceStepOutcome::Skipped
    );
    assert_eq!(
        report.ledger_report.outcome,
        RuntimeMaintenanceStepOutcome::Skipped
    );
    assert!(matches!(
        report.sidecar_report.detail.as_deref(),
        Some("policy disabled")
    ));
    assert!(matches!(
        report.ledger_report.detail.as_deref(),
        Some("policy disabled")
    ));
}

#[test]
fn runtime_maintenance_reports_failed_ledger_refresh_when_persistence_is_blocked() {
    let state_root = temp_state_root("runtime-maintenance-failed");
    let repository = StateRepository::new(state_root.clone());
    let session_store = Arc::new(SessionStore::new());
    let workspace_store = Arc::new(WorkspaceStore::new());
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let blocking_parent = state_root.join("blocking-parent");
    fs::write(&blocking_parent, b"blocker").expect("blocking parent file should be writable");
    let invalid_ledger_path = blocking_parent.join("audit-usage-ledger.json");
    event_bus.set_audit_usage_ledger_persistence(invalid_ledger_path);
    let _ = event_bus.publish(EventEnvelope::usage(
        EventId::new("usage-failed"),
        "tool.used",
        serde_json::json!({ "tool_name": "shell_exec", "status": "Succeeded" }),
    ));

    let maintenance = RuntimeMaintenance::new(
        RuntimeMaintenanceConfig::default(),
        event_bus.clone(),
        test_sidecar_persistence(repository, session_store, workspace_store),
        Arc::new(SessionStore::new()),
        Arc::new(WorkspaceStore::new()),
    );

    let report = maintenance
        .run_once()
        .expect("failed maintenance tick should return report");
    assert_eq!(
        report.sidecar_report.outcome,
        RuntimeMaintenanceStepOutcome::Skipped
    );
    assert_eq!(
        report.ledger_report.outcome,
        RuntimeMaintenanceStepOutcome::Failed
    );
    assert!(matches!(
        report.sidecar_report.detail.as_deref(),
        Some("not due")
    ));
    assert!(report.ledger_report.detail.is_some());
}

#[test]
fn runtime_status_export_reflects_maintenance_mode_and_profile() {
    let repository = StateRepository::new(temp_state_root("runtime-status-export"));
    let maintenance = RuntimeMaintenance::new(
        RuntimeMaintenanceConfig {
            policy: RuntimeMaintenancePolicy::from_profile(
                DaemonMaintenancePolicyProfile::PreCutoverDrain,
            ),
        },
        Arc::new(InMemoryEventBus::new(16)),
        test_sidecar_persistence(
            repository,
            Arc::new(SessionStore::new()),
            Arc::new(WorkspaceStore::new()),
        ),
        Arc::new(SessionStore::new()),
        Arc::new(WorkspaceStore::new()),
    );

    maintenance.enter_maintenance_mode("pre-cutover drain");
    let status = maintenance.runtime_status();

    assert_eq!(status.maintenance_mode, DaemonMaintenanceMode::CutoverPrep);
    assert_eq!(
        status.policy_profile,
        DaemonMaintenancePolicyProfile::PreCutoverDrain
    );
    assert_eq!(status.mode_reason.as_deref(), Some("pre-cutover drain"));
    assert_eq!(status.tick_interval_millis, 100);
    assert!(status.sidecar_flush_enabled);
    assert!(status.ledger_refresh_enabled);
    assert!(status.eager_flush_dirty_sidecars);
    assert!(status.refresh_ledger_when_unhealthy);
    assert!(status.refresh_ledger_when_never_persisted);
}

#[test]
fn aggressive_flush_profile_ignores_future_flush_hints_for_dirty_sidecars() {
    let state_root = temp_state_root("runtime-aggressive-flush");
    let repository = StateRepository::new(state_root.clone());
    let session_store = Arc::new(SessionStore::new());
    let workspace_store = Arc::new(WorkspaceStore::new());
    let event_bus = Arc::new(InMemoryEventBus::new(32));

    session_store
        .create_session(SessionId::new("session-aggressive"), "aggressive session")
        .expect("session should be creatable");
    session_store.bind_execution_ownership(
        SessionId::new("session-aggressive"),
        ExecutionOwnership {
            session_id: Some(SessionId::new("session-aggressive")),
            execution_chain_ref: Some("chain-aggressive".to_string()),
            ..ExecutionOwnership::default()
        },
    );

    let maintenance = RuntimeMaintenance::new(
        RuntimeMaintenanceConfig {
            policy: RuntimeMaintenancePolicy::from_profile(
                DaemonMaintenancePolicyProfile::AggressiveFlush,
            ),
        },
        event_bus,
        test_sidecar_persistence(repository, session_store.clone(), workspace_store),
        session_store,
        Arc::new(WorkspaceStore::new()),
    );

    let report = maintenance
        .run_once()
        .expect("aggressive maintenance tick should succeed");
    let status = maintenance.runtime_status();

    assert_eq!(
        status.maintenance_mode,
        DaemonMaintenanceMode::AggressiveFlush
    );
    assert_eq!(
        report.sidecar_report.outcome,
        RuntimeMaintenanceStepOutcome::DueAndFlushed
    );
}

#[test]
fn persistence_long_chain_boot_mutate_flush_restart_verifies_sidecar_integrity() {
    // T-106: Full long-chain validation —
    //   boot → populate → mutate → flush → RESTART (new store instances) → verify integrity
    let state_root = temp_state_root("persistence-long-chain");
    let workspace_root = temp_state_root("persistence-long-chain-workspace");
    let repository = StateRepository::new(state_root.clone());

    let session_id = SessionId::new("session-lc");
    let workspace_id = WorkspaceId::new("workspace-lc");
    let ownership = ExecutionOwnership {
        session_id: Some(session_id.clone()),
        workspace_id: Some(workspace_id.clone()),
        execution_chain_ref: Some("chain-lc".to_string()),
        ..ExecutionOwnership::default()
    };

    // ── Phase 1: Boot and populate ──
    let session_store = Arc::new(SessionStore::new());
    let workspace_store = Arc::new(WorkspaceStore::new());

    session_store
        .create_session(session_id.clone(), "long-chain session")
        .expect("session should be creatable");
    workspace_store
        .register(
            workspace_id.clone(),
            AbsolutePath::new(workspace_root.to_string_lossy().to_string()),
        )
        .expect("workspace should be registrable");

    // Bind execution ownership — makes session sidecar dirty
    session_store.bind_execution_ownership(session_id.clone(), ownership.clone());

    // Create snapshot + recovery entry — makes workspace sidecar dirty
    let snapshot = workspace_store.append_execution_snapshot(
        workspace_id.clone(),
        ownership.clone(),
        "snapshot-lc",
        "long-chain snapshot",
    );
    let recovery = workspace_store.prepare_recovery_entry(
        workspace_id.clone(),
        ownership.clone(),
        snapshot.snapshot_id.clone(),
        "recovery-lc",
        Some("long-chain diagnostic".to_string()),
    );
    session_store
        .attach_recovery_ref(&session_id, Some(recovery.recovery_id.clone()))
        .expect("recovery ref should be attachable");

    // Persist durable state (sessions.json + workspaces.json)
    repository
        .save_session_durable_state(&session_store.durable_state())
        .expect("session durable state should save");
    repository
        .save_workspace_durable_state(&workspace_store.durable_state())
        .expect("workspace durable state should save");

    // ── Phase 2: Flush sidecars ──
    let persistence = test_sidecar_persistence(
        repository.clone(),
        session_store.clone(),
        workspace_store.clone(),
    );
    let flush_report = persistence
        .flush_runtime_sidecars()
        .expect("initial sidecar flush should succeed");
    assert!(
        flush_report.session_sidecars_flushed,
        "session sidecars should be dirty and flushed"
    );
    assert!(
        flush_report.workspace_recovery_sidecars_flushed,
        "workspace sidecars should be dirty and flushed"
    );

    // Verify flush metadata is now clean
    let session_meta = session_store.execution_sidecar_flush_metadata();
    assert_eq!(
        session_meta.current_version, session_meta.flushed_version,
        "session sidecar flush versions should match after flush"
    );
    let workspace_meta = workspace_store.recovery_sidecar_flush_metadata();
    assert_eq!(
        workspace_meta.current_version, workspace_meta.flushed_version,
        "workspace sidecar flush versions should match after flush"
    );

    // Second flush should be no-op
    let second_flush = persistence
        .flush_runtime_sidecars()
        .expect("second flush should succeed");
    assert!(
        !second_flush.session_sidecars_flushed,
        "clean session sidecars should not re-flush"
    );
    assert!(
        !second_flush.workspace_recovery_sidecars_flushed,
        "clean workspace sidecars should not re-flush"
    );

    // ── Phase 3: RESTART — create entirely new store instances from persisted files ──
    drop(persistence);
    drop(session_store);
    drop(workspace_store);

    let restarted_session_store = Arc::new(SessionStore::from_persisted_parts(
        repository
            .load_sessions_from_workspaces(&[(workspace_id.to_string(), workspace_root.clone())])
            .expect("session durable state should reload"),
        repository
            .load_session_sidecars()
            .expect("session sidecars should reload"),
    ));
    let restarted_workspace_store = Arc::new(WorkspaceStore::from_persisted_parts(
        repository
            .load_workspace_durable_state()
            .expect("workspace durable state should reload"),
        repository
            .load_workspace_recovery_sidecars()
            .expect("workspace sidecars should reload"),
    ));

    // ── Phase 4: Verify restarted stores have correct state ──
    // Session durable state
    let restarted_session = restarted_session_store
        .current_session()
        .expect("restarted store should have current session");
    assert_eq!(restarted_session.session_id, session_id);

    // Session sidecar integrity
    let sidecar_exports = restarted_session_store.execution_sidecar_exports();
    assert_eq!(
        sidecar_exports.len(),
        1,
        "restarted session store should have 1 sidecar"
    );
    let sidecar = &sidecar_exports[0];
    assert_eq!(sidecar.session_id, session_id);
    assert_eq!(
        sidecar.current_status,
        SessionExecutionSidecarStatus::RecoveryLinked
    );
    assert_eq!(sidecar.ownership.session_id, Some(session_id.clone()));
    assert_eq!(sidecar.ownership.workspace_id, Some(workspace_id.clone()));
    assert_eq!(sidecar.execution_chain_ref.as_deref(), Some("chain-lc"));
    assert_eq!(sidecar.recovery_ref.as_deref(), Some("recovery-lc"));

    // Session flush metadata should be clean after restart
    let restarted_session_meta = restarted_session_store.execution_sidecar_flush_metadata();
    assert_eq!(
        restarted_session_meta.current_version, restarted_session_meta.flushed_version,
        "restarted session sidecar flush should be clean"
    );

    // Workspace recovery sidecar integrity
    let recovery_exports = restarted_workspace_store.recovery_sidecar_exports();
    assert_eq!(
        recovery_exports.len(),
        1,
        "restarted workspace store should have 1 recovery sidecar"
    );
    let ws_sidecar = &recovery_exports[0];
    assert_eq!(ws_sidecar.workspace_id, workspace_id);
    assert_eq!(ws_sidecar.current_status, RecoveryStatus::Prepared);
    assert_eq!(ws_sidecar.ownership.session_id, Some(session_id.clone()));
    assert_eq!(ws_sidecar.snapshot_id, snapshot.snapshot_id);
    assert_eq!(
        ws_sidecar.diagnostic_summary.as_deref(),
        Some("long-chain diagnostic")
    );

    // Workspace flush metadata should be clean after restart
    let restarted_ws_meta = restarted_workspace_store.recovery_sidecar_flush_metadata();
    assert_eq!(
        restarted_ws_meta.current_version, restarted_ws_meta.flushed_version,
        "restarted workspace sidecar flush should be clean"
    );

    // Workspace durable state
    let snapshots = restarted_workspace_store.snapshots();
    assert_eq!(
        snapshots.len(),
        1,
        "restarted workspace store should have 1 snapshot"
    );
    assert_eq!(snapshots[0].snapshot_id, snapshot.snapshot_id);
}

#[test]
fn persistence_long_chain_restart_mutate_flush_validates_incremental_across_boundaries() {
    // T-106: Second half — verify that mutations AFTER restart produce correct dirty
    //   tracking and flush correctly on the restarted instances.
    let state_root = temp_state_root("persistence-cross-boundary");
    let workspace_root = temp_state_root("persistence-cross-boundary-workspace");
    let repository = StateRepository::new(state_root.clone());

    let session_id = SessionId::new("session-cb");
    let workspace_id = WorkspaceId::new("workspace-cb");
    let ownership = ExecutionOwnership {
        session_id: Some(session_id.clone()),
        workspace_id: Some(workspace_id.clone()),
        execution_chain_ref: Some("chain-cb".to_string()),
        ..ExecutionOwnership::default()
    };

    // ── Phase 1: Bootstrap, flush, persist ──
    {
        let session_store = Arc::new(SessionStore::new());
        let workspace_store = Arc::new(WorkspaceStore::new());
        session_store
            .create_session(session_id.clone(), "cross-boundary session")
            .expect("session should be creatable");
        workspace_store
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_root.to_string_lossy().to_string()),
            )
            .expect("workspace should be registrable");

        session_store.bind_execution_ownership(session_id.clone(), ownership.clone());
        let snapshot = workspace_store.append_execution_snapshot(
            workspace_id.clone(),
            ownership.clone(),
            "snapshot-cb",
            "cross-boundary snapshot",
        );
        workspace_store.prepare_recovery_entry(
            workspace_id.clone(),
            ownership.clone(),
            snapshot.snapshot_id.clone(),
            "recovery-cb",
            None,
        );

        repository
            .save_session_durable_state(&session_store.durable_state())
            .expect("durable session save should succeed");
        repository
            .save_workspace_durable_state(&workspace_store.durable_state())
            .expect("durable workspace save should succeed");

        let persistence =
            test_sidecar_persistence(repository.clone(), session_store, workspace_store);
        persistence
            .flush_runtime_sidecars()
            .expect("first-gen flush should succeed");
    }

    // ── Phase 2: Restart and mutate ──
    let session_store_2 = Arc::new(SessionStore::from_persisted_parts(
        repository
            .load_sessions_from_workspaces(&[(workspace_id.to_string(), workspace_root.clone())])
            .expect("load"),
        repository.load_session_sidecars().expect("load"),
    ));
    let workspace_store_2 = Arc::new(WorkspaceStore::from_persisted_parts(
        repository.load_workspace_durable_state().expect("load"),
        repository.load_workspace_recovery_sidecars().expect("load"),
    ));

    // Perform recovery consumption on restarted stores
    let recovery_handles = workspace_store_2.active_recovery_handles(&workspace_id);
    assert_eq!(
        recovery_handles.len(),
        1,
        "restarted store should have 1 active recovery handle"
    );
    let recovery_id = &recovery_handles[0].recovery_id;

    workspace_store_2
        .mark_recovery_ready(recovery_id)
        .expect("mark ready should succeed on restarted store");
    let resume_input = workspace_store_2
        .build_recovery_resume_input(recovery_id)
        .expect("resume input should build on restarted store");
    workspace_store_2
        .consume_recovery(recovery_id)
        .expect("consume should succeed on restarted store");
    session_store_2
        .apply_recovery_resume_input(session_id.clone(), resume_input)
        .expect("resume input should sync session sidecar on restarted store");

    // Verify dirty tracking works across restart boundary
    let session_meta_2 = session_store_2.execution_sidecar_flush_metadata();
    assert_ne!(
        session_meta_2.current_version, session_meta_2.flushed_version,
        "session sidecar should be dirty after mutation on restarted store"
    );
    let workspace_meta_2 = workspace_store_2.recovery_sidecar_flush_metadata();
    assert_ne!(
        workspace_meta_2.current_version, workspace_meta_2.flushed_version,
        "workspace sidecar should be dirty after mutation on restarted store"
    );

    // ── Phase 3: Flush on restarted stores ──
    let persistence_2 = test_sidecar_persistence(
        repository.clone(),
        session_store_2.clone(),
        workspace_store_2.clone(),
    );
    let flush_report_2 = persistence_2
        .flush_runtime_sidecars()
        .expect("post-restart flush should succeed");
    assert!(
        flush_report_2.session_sidecars_flushed,
        "session sidecars should flush after mutation"
    );
    assert!(
        flush_report_2.workspace_recovery_sidecars_flushed,
        "workspace sidecars should flush after mutation"
    );

    // ── Phase 4: Restart AGAIN and verify final state ──
    drop(persistence_2);
    drop(session_store_2);
    drop(workspace_store_2);

    let session_store_3 = SessionStore::from_persisted_parts(
        repository
            .load_sessions_from_workspaces(&[(workspace_id.to_string(), workspace_root.clone())])
            .expect("load"),
        repository.load_session_sidecars().expect("load"),
    );
    let workspace_store_3 = WorkspaceStore::from_persisted_parts(
        repository.load_workspace_durable_state().expect("load"),
        repository.load_workspace_recovery_sidecars().expect("load"),
    );

    // Session sidecar should reflect RecoveryLinked state (apply_recovery_resume_input sets RecoveryLinked)
    let final_sidecar_exports = session_store_3.execution_sidecar_exports();
    assert_eq!(final_sidecar_exports.len(), 1);
    assert_eq!(
        final_sidecar_exports[0].current_status,
        SessionExecutionSidecarStatus::RecoveryLinked
    );

    // Workspace recovery should reflect consumed state
    let final_recovery_exports = workspace_store_3.recovery_sidecar_exports();
    assert_eq!(final_recovery_exports.len(), 1);
    assert_eq!(
        final_recovery_exports[0].current_status,
        RecoveryStatus::Consumed
    );

    // Both flush states should be clean
    let final_session_meta = session_store_3.execution_sidecar_flush_metadata();
    assert_eq!(
        final_session_meta.current_version, final_session_meta.flushed_version,
        "final session sidecar flush should be clean"
    );
    let final_ws_meta = workspace_store_3.recovery_sidecar_flush_metadata();
    assert_eq!(
        final_ws_meta.current_version, final_ws_meta.flushed_version,
        "final workspace sidecar flush should be clean"
    );
}

#[test]
fn persistence_long_chain_maintenance_tick_drives_full_restart_recovery_cycle() {
    // T-106: Maintenance-driven long chain — verify that the maintenance tick
    //   (not just manual flush) correctly persists all state, and a restart from
    //   that persisted state is fully self-consistent.
    let state_root = temp_state_root("persistence-maintenance-long-chain");
    let workspace_root = temp_state_root("maintenance-long-chain-workspace");
    let repository = StateRepository::new(state_root.clone());
    let event_bus = Arc::new(InMemoryEventBus::new(32));

    let session_id = SessionId::new("session-mlc");
    let workspace_id = WorkspaceId::new("workspace-mlc");
    let ownership = ExecutionOwnership {
        session_id: Some(session_id.clone()),
        workspace_id: Some(workspace_id.clone()),
        execution_chain_ref: Some("chain-mlc".to_string()),
        ..ExecutionOwnership::default()
    };

    // ── Phase 1: Bootstrap and populate ──
    let session_store = Arc::new(SessionStore::new());
    let workspace_store = Arc::new(WorkspaceStore::new());

    session_store
        .create_session(session_id.clone(), "maintenance long-chain session")
        .expect("session should be creatable");
    workspace_store
        .register(
            workspace_id.clone(),
            AbsolutePath::new(workspace_root.to_string_lossy().to_string()),
        )
        .expect("workspace should be registrable");

    session_store.bind_execution_ownership(session_id.clone(), ownership.clone());
    let snapshot = workspace_store.append_execution_snapshot(
        workspace_id.clone(),
        ownership.clone(),
        "snapshot-mlc",
        "maintenance long-chain snapshot",
    );
    workspace_store.prepare_recovery_entry(
        workspace_id.clone(),
        ownership.clone(),
        snapshot.snapshot_id.clone(),
        "recovery-mlc",
        Some("maintenance diagnostic".to_string()),
    );

    // Persist durable state
    repository
        .save_session_durable_state(&session_store.durable_state())
        .expect("durable save should succeed");
    repository
        .save_workspace_durable_state(&workspace_store.durable_state())
        .expect("durable save should succeed");

    // Emit a usage event so ledger has something to persist.
    // NOTE: set_audit_usage_ledger_persistence is called AFTER publish so that
    // the publish does not auto-persist (which would clear pending_flush before
    // the maintenance tick gets a chance to refresh it).
    let _ = event_bus.publish(EventEnvelope::usage(
        EventId::new("usage-mlc"),
        "tool.used",
        serde_json::json!({ "tool_name": "shell_exec", "status": "Succeeded" }),
    ));
    event_bus.set_audit_usage_ledger_persistence(repository.audit_usage_ledger_path());

    // ── Phase 2: Use AggressiveFlush maintenance to flush everything ──
    let persistence = test_sidecar_persistence(
        repository.clone(),
        session_store.clone(),
        workspace_store.clone(),
    );
    let maintenance = RuntimeMaintenance::new(
        RuntimeMaintenanceConfig {
            policy: RuntimeMaintenancePolicy::from_profile(
                DaemonMaintenancePolicyProfile::AggressiveFlush,
            ),
        },
        event_bus.clone(),
        persistence,
        session_store.clone(),
        workspace_store.clone(),
    );

    let report = maintenance
        .run_once()
        .expect("maintenance tick should succeed");
    assert_eq!(
        report.sidecar_report.outcome,
        RuntimeMaintenanceStepOutcome::DueAndFlushed
    );
    assert_eq!(
        report.ledger_report.outcome,
        RuntimeMaintenanceStepOutcome::DueAndRefreshed
    );

    // ── Phase 3: Full restart — new everything from persisted files ──
    drop(maintenance);
    drop(session_store);
    drop(workspace_store);

    let restarted_session = Arc::new(SessionStore::from_persisted_parts(
        repository
            .load_sessions_from_workspaces(&[(workspace_id.to_string(), workspace_root.clone())])
            .expect("load"),
        repository.load_session_sidecars().expect("load"),
    ));
    let restarted_workspace = Arc::new(WorkspaceStore::from_persisted_parts(
        repository.load_workspace_durable_state().expect("load"),
        repository.load_workspace_recovery_sidecars().expect("load"),
    ));

    // Verify all state survived the restart
    assert!(restarted_session.current_session().is_some());
    assert_eq!(restarted_session.execution_sidecar_exports().len(), 1);
    assert_eq!(restarted_workspace.recovery_sidecar_exports().len(), 1);
    assert_eq!(restarted_workspace.snapshots().len(), 1);

    // Verify flush metadata is clean — no orphan dirty state
    let session_meta = restarted_session.execution_sidecar_flush_metadata();
    assert_eq!(session_meta.current_version, session_meta.flushed_version);
    let ws_meta = restarted_workspace.recovery_sidecar_flush_metadata();
    assert_eq!(ws_meta.current_version, ws_meta.flushed_version);

    // A maintenance tick on restarted stores should see nothing to flush
    let persistence_2 = test_sidecar_persistence(
        repository.clone(),
        restarted_session.clone(),
        restarted_workspace.clone(),
    );
    let no_op_flush = persistence_2
        .flush_runtime_sidecars()
        .expect("clean restart flush should succeed");
    assert!(
        !no_op_flush.session_sidecars_flushed,
        "clean restart should not re-flush session sidecars"
    );
    assert!(
        !no_op_flush.workspace_recovery_sidecars_flushed,
        "clean restart should not re-flush workspace sidecars"
    );

    // Verify ledger also survived
    let ledger = repository
        .load_audit_usage_ledger()
        .expect("ledger should reload");
    assert!(
        ledger.audit_entries.len() + ledger.usage_entries.len() >= 1,
        "ledger should have at least one persisted entry"
    );
}

#[test]
fn graceful_shutdown_marks_runtime_status_complete_after_final_tick() {
    let state_root = temp_state_root("runtime-shutdown");
    let repository = StateRepository::new(state_root.clone());
    let session_store = Arc::new(SessionStore::new());
    let workspace_store = Arc::new(WorkspaceStore::new());
    let event_bus = Arc::new(InMemoryEventBus::new(32));

    session_store
        .create_session(SessionId::new("session-shutdown"), "shutdown session")
        .expect("session should be creatable");
    workspace_store
        .register(
            WorkspaceId::new("workspace-shutdown"),
            temp_workspace_absolute_path("runtime-shutdown-workspace"),
        )
        .expect("workspace should be registrable");
    session_store.bind_execution_ownership(
        SessionId::new("session-shutdown"),
        ExecutionOwnership {
            session_id: Some(SessionId::new("session-shutdown")),
            workspace_id: Some(WorkspaceId::new("workspace-shutdown")),
            execution_chain_ref: Some("chain-shutdown".to_string()),
            ..ExecutionOwnership::default()
        },
    );
    let snapshot = workspace_store.append_execution_snapshot(
        WorkspaceId::new("workspace-shutdown"),
        ExecutionOwnership {
            session_id: Some(SessionId::new("session-shutdown")),
            workspace_id: Some(WorkspaceId::new("workspace-shutdown")),
            execution_chain_ref: Some("chain-shutdown".to_string()),
            ..ExecutionOwnership::default()
        },
        "snapshot-shutdown",
        "shutdown snapshot",
    );
    workspace_store.prepare_recovery_entry(
        WorkspaceId::new("workspace-shutdown"),
        ExecutionOwnership {
            session_id: Some(SessionId::new("session-shutdown")),
            workspace_id: Some(WorkspaceId::new("workspace-shutdown")),
            execution_chain_ref: Some("chain-shutdown".to_string()),
            ..ExecutionOwnership::default()
        },
        snapshot.snapshot_id,
        "recovery-shutdown",
        None,
    );
    let _ = event_bus.publish(EventEnvelope::usage(
        EventId::new("usage-shutdown"),
        "tool.used",
        serde_json::json!({ "tool_name": "shell_exec", "status": "Succeeded" }),
    ));
    event_bus.set_audit_usage_ledger_persistence(repository.audit_usage_ledger_path());

    let maintenance = RuntimeMaintenance::new(
        RuntimeMaintenanceConfig {
            policy: RuntimeMaintenancePolicy::from_profile(
                DaemonMaintenancePolicyProfile::PreCutoverDrain,
            ),
        },
        event_bus.clone(),
        test_sidecar_persistence(repository, session_store.clone(), workspace_store.clone()),
        session_store,
        workspace_store,
    );

    maintenance.request_graceful_shutdown("unit-test shutdown");
    let report = maintenance
        .run_once()
        .expect("shutdown maintenance tick should succeed");
    let status = maintenance.runtime_status();

    assert_eq!(
        report.runtime_status.maintenance_mode,
        DaemonMaintenanceMode::ShutdownComplete
    );
    assert_eq!(
        status.maintenance_mode,
        DaemonMaintenanceMode::ShutdownComplete
    );
    assert_eq!(status.mode_reason.as_deref(), Some("unit-test shutdown"));
    assert!(status.shutdown_requested_at.is_some());
    assert!(status.shutdown_completed_at.is_some());
    assert_eq!(
        report.sidecar_report.outcome,
        RuntimeMaintenanceStepOutcome::DueAndFlushed
    );
    assert_eq!(
        report.ledger_report.outcome,
        RuntimeMaintenanceStepOutcome::DueAndRefreshed
    );
    assert!(!event_bus.runtime_ledger_summary().pending_flush);
}

// ═══════════════════════════════════════════════════════════════════
// 端到端集成测试 — 全链路 loop 验证
// ═══════════════════════════════════════════════════════════════════

#[tokio::test]
async fn session_action_happy_path_creates_tasks_and_records_timeline_messages() {
    let state_root = temp_state_root("e2e-session-action-messages");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    let runtime = DaemonRuntime::restore(&config)
        .expect("runtime restore should bootstrap empty state");
    let (app, _state) = runtime.router_with_state_for_tests("daemon-test".to_string());

    let (status, body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "text": "Hello integration test",
            "deep_task": false,
            "skill_name": "code",
            "images": [],
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "session action should succeed: {body:?}"
    );
    assert!(body["acceptedAt"].as_u64().is_some());
    assert!(body["rootTaskId"].is_string());

    let session_id = body["sessionId"].as_str().unwrap();
    assert_eq!(
        session_id, "test-session-001",
        "should use bootstrapped session"
    );
    let accepted_at = body["acceptedAt"]
        .as_u64()
        .expect("accepted_at should serialize as integer");
    let mission_id = format!("mission-session-action-{accepted_at}");
    let root_task_id = body["rootTaskId"]
        .as_str()
        .expect("root_task_id should serialize as string");
    let projection =
        wait_for_task_projection_completed(app.clone(), root_task_id, session_id).await;
    assert_completed_two_task_projection(&projection);

    let messages_page = get_json(
        app.clone(),
        &format!("/api/messages?sessionId={session_id}"),
    )
    .await;
    let timeline = messages_page["timeline"]
        .as_array()
        .expect("timeline should be an array");
    assert!(
        timeline.iter().any(|entry| entry["kind"] == "UserMessage"),
        "timeline should contain user message"
    );
    let user_msg = timeline
        .iter()
        .find(|entry| entry["kind"] == "UserMessage")
        .unwrap();
    assert!(
        user_msg["message"]
            .as_str()
            .unwrap()
            .contains("Hello integration test"),
        "user message should contain original text"
    );

    let read_model = get_json(app, "/runtime/read-model").await;
    let execution_groups = read_model["details"]["execution_groups"]
        .as_array()
        .expect("execution groups should be an array");
    assert!(
        execution_groups
            .iter()
            .any(|m| m["mission_id"] == mission_id),
        "read model should contain the execution group"
    );
    let session_summary = read_model["details"]["sessions"]
        .as_array()
        .expect("sessions should be an array")
        .iter()
        .find(|entry| entry["session_id"] == session_id)
        .expect("session summary should exist");
    let turn_items = session_summary["turn_items"]
        .as_array()
        .expect("turn items should be an array");
    assert!(
        turn_items
            .iter()
            .any(|item| item["kind"] == "assistant_final"),
        "turn-first items should contain assistant_final"
    );
}

#[tokio::test]
async fn session_action_messages_survive_runtime_restart_and_preserve_message_count() {
    let state_root = temp_state_root("e2e-session-action-restart-count");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());

    let runtime = DaemonRuntime::restore(&config)
        .expect("first runtime restore should bootstrap empty state");
    let (app, _state) = runtime.router_with_state_for_tests("daemon-test".to_string());

    let (status, body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "text": "Restart persistence verification",
            "deep_task": false,
            "skill_name": "code",
            "images": [],
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "session action should succeed: {body:?}"
    );
    let session_id = body["sessionId"]
        .as_str()
        .expect("session_id should serialize as string")
        .to_string();

    let before_restart_messages = get_json(
        app.clone(),
        &format!("/api/messages?sessionId={session_id}"),
    )
    .await;
    assert_eq!(
        before_restart_messages["timeline"]
            .as_array()
            .expect("timeline should be an array")
            .iter()
            .filter(|entry| entry["kind"] == "UserMessage")
            .count(),
        1,
        "restart 前应存在 1 条用户消息"
    );

    drop(app);
    drop(runtime);

    let restarted_runtime = DaemonRuntime::restore(&config)
        .expect("second runtime restore should recover persisted workspace sessions");
    let (restarted_app, _restarted_state) =
        restarted_runtime.router_with_state_for_tests("daemon-test".to_string());

    let after_restart_messages = get_json(
        restarted_app.clone(),
        &format!("/api/messages?sessionId={session_id}"),
    )
    .await;
    assert_eq!(
        after_restart_messages["timeline"]
            .as_array()
            .expect("timeline should be an array after restart")
            .iter()
            .filter(|entry| entry["kind"] == "UserMessage")
            .count(),
        1,
        "restart 后用户消息不应丢失"
    );

    let workspace_sessions = get_json(
        restarted_app,
        "/api/workspaces/sessions?workspaceId=test-workspace-001",
    )
    .await;
    let restored_session = workspace_sessions["sessions"]
        .as_array()
        .expect("workspace sessions should be an array")
        .iter()
        .find(|session| session["sessionId"] == session_id)
        .expect("restarted workspace sessions should contain restored session");
    assert_eq!(restored_session["messageCount"], 1);
}

#[tokio::test]
async fn runtime_restore_detaches_session_chain_when_root_task_checkpoint_is_missing() {
    let state_root = temp_state_root("stale-session-chain-root-missing");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());
    let workspace_root = state_root.join("workspace");
    let repository = StateRepository::new(state_root);
    let session_store = SessionStore::new();
    let workspace_store = WorkspaceStore::new();
    let session_id = SessionId::new("session-stale-chain-root-missing");
    let workspace_id = WorkspaceId::new("workspace-stale-chain-root-missing");
    let mission_id = MissionId::new("mission-stale-chain-root-missing");
    let root_task_id = TaskId::new("task-root-stale-chain-root-missing");
    let branch_task_id = TaskId::new("task-branch-stale-chain-root-missing");
    let worker_id = WorkerId::new("worker-stale-chain-root-missing");

    session_store
        .create_session(session_id.clone(), "stale chain")
        .expect("session should be creatable");
    workspace_store
        .register(
            workspace_id,
            AbsolutePath::new(workspace_root.to_string_lossy().to_string()),
        )
        .expect("workspace should be registrable");
    session_store
        .upsert_active_execution_chain(
            session_id.clone(),
            ActiveExecutionChain {
                session_id: session_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                execution_chain_ref: "chain-stale-root-missing".to_string(),
                workspace_id: None,
                active_branch_task_ids: vec![branch_task_id.clone()],
                active_worker_bindings: vec![worker_id.clone()],
                branches: vec![ActiveExecutionBranch {
                    task_id: branch_task_id,
                    worker_id,
                    stage: "execute".to_string(),
                    lease_id: None,
                    execution_intent_ref: Some("intent-stale-root-missing".to_string()),
                    binding_lifecycle: Some("requested".to_string()),
                    checkpoint_stage: Some("execute".to_string()),
                    next_step_index: Some(1),
                    checkpoint_at: Some(UtcMillis::now()),
                    resume_mode: Some("step-checkpoint".to_string()),
                    resume_token: None,
                    use_tools: true,
                    skill_name: None,
                    is_primary: true,
                }],
                recovery_ref: None,
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: UtcMillis::now(),
                    entry_id: "timeline-stale-root-missing".to_string(),
                    trimmed_text: Some("stale root should detach".to_string()),
                    deep_task: true,
                    skill_name: None,
                },
                current_turn: None,
            },
        )
        .expect("active execution chain should persist to sidecar");
    repository
        .save_session_durable_state(&session_store.durable_state())
        .expect("session durable state should save");
    repository
        .save_workspace_durable_state(&workspace_store.durable_state())
        .expect("workspace durable state should save");
    repository
        .save_session_sidecars(&session_store.execution_sidecar_store_state())
        .expect("session sidecars should save");

    let runtime =
        DaemonRuntime::restore(&config).expect("runtime restore should load stale sidecar");
    let (_app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());

    let sidecar = state
        .session_store
        .runtime_sidecar(&session_id)
        .expect("session sidecar should still exist");
    assert_eq!(sidecar.status, SessionExecutionSidecarStatus::Detached);
    assert!(sidecar.active_execution_chain.is_none());
    assert!(sidecar.ownership.mission_id.is_none());
    assert!(sidecar.ownership.task_id.is_none());
    assert!(sidecar.ownership.execution_chain_ref.is_none());

    let read_model = state.runtime_read_model_dto();
    let session_summary = read_model
        .details
        .sessions
        .iter()
        .find(|entry| entry.session_id == session_id.to_string())
        .expect("runtime read model should contain session summary");
    assert_eq!(session_summary.current_status.as_deref(), Some("detached"));
    assert!(session_summary.root_task_id.is_none());
    assert!(session_summary.active_task_ids.is_empty());
    assert!(session_summary.active_execution_group_ids.is_empty());

    let persisted_sidecars = repository
        .load_session_sidecars()
        .expect("reconciled sidecars should persist");
    let persisted = persisted_sidecars
        .runtime_sidecars
        .iter()
        .find(|sidecar| sidecar.session_id == session_id)
        .expect("persisted sidecar should exist");
    assert_eq!(persisted.status, SessionExecutionSidecarStatus::Detached);
    assert!(persisted.active_execution_chain.is_none());
}

#[tokio::test]
async fn session_continue_survives_runtime_restart_with_same_chain_and_worker_branches() {
    let state_root = temp_state_root("e2e-session-continue-restart-chain");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());
    let repository = StateRepository::new(state_root.clone());

    let runtime = DaemonRuntime::restore(&config)
        .expect("first runtime restore should bootstrap empty state");
    let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());

    let (status, body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "text": "Restart continue verification",
            "deep_task": false,
            "skill_name": "refactor",
            "images": [],
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "session action should succeed: {body:?}"
    );

    let session_id_text = body["sessionId"]
        .as_str()
        .expect("session_id should serialize as string")
        .to_string();
    let session_id = SessionId::new(session_id_text.clone());
    let mut chain = state
        .session_store
        .active_execution_chain(&session_id)
        .expect("seed dispatch should create active execution chain");
    let primary_branch = chain
        .branches
        .iter()
        .find(|branch| branch.is_primary)
        .cloned()
        .or_else(|| chain.branches.first().cloned())
        .expect("seed dispatch should contain at least one branch");
    let mission_id = chain.mission_id.clone();
    let root_task_id = chain.root_task_id.clone();
    let execution_chain_ref = chain.execution_chain_ref.clone();
    let task_store = state.task_store().expect("task store should be configured");

    task_store
        .update_status(&primary_branch.task_id, TaskStatus::Blocked)
        .expect("primary branch should become recoverable");

    let now = UtcMillis::now();
    let extra_branch_specs = [
        (
            "task-restart-branch-1",
            "worker-restart-branch-1",
            "lease-restart-branch-1",
        ),
        (
            "task-restart-branch-2",
            "worker-restart-branch-2",
            "lease-restart-branch-2",
        ),
    ];
    for (task_id, worker_id, lease_id) in extra_branch_specs {
        task_store.insert_task(Task {
            task_id: TaskId::new(task_id),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: Some(primary_branch.task_id.clone()),
            kind: TaskKind::Action,
            title: format!("restart branch {task_id}"),
            goal: format!("resume branch {task_id}"),
            status: TaskStatus::Blocked,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            context_refs: Vec::new(),
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            repair_count: 0,
            decision_payload: None,
            created_at: now,
            updated_at: now,
        });
        chain.branches.push(ActiveExecutionBranch {
            task_id: TaskId::new(task_id),
            worker_id: WorkerId::new(worker_id),
            stage: "execute".to_string(),
            lease_id: Some(LeaseId::new(lease_id)),
            execution_intent_ref: Some(format!("worker-intent-{task_id}")),
            binding_lifecycle: Some("requested".to_string()),
            checkpoint_stage: Some("execute".to_string()),
            next_step_index: Some(1),
            checkpoint_at: Some(now),
            resume_mode: Some("step-checkpoint".to_string()),
            resume_token: None,
            use_tools: true,
            skill_name: None,
            is_primary: false,
        });
    }
    task_store.insert_task(Task {
        task_id: TaskId::new("task-restart-branch-completed"),
        mission_id: mission_id.clone(),
        root_task_id: root_task_id.clone(),
        parent_task_id: Some(primary_branch.task_id.clone()),
        kind: TaskKind::Action,
        title: "restart branch completed".to_string(),
        goal: "completed branch should stay terminal".to_string(),
        status: TaskStatus::Completed,
        dependency_ids: Vec::new(),
        required_children: Vec::new(),
        policy_snapshot: None,
        executor_binding: None,
        context_refs: Vec::new(),
        knowledge_refs: Vec::new(),
        workspace_scope: None,
        write_scope: None,
        input_refs: Vec::new(),
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        repair_count: 0,
        decision_payload: None,
        created_at: now,
        updated_at: now,
    });
    chain.branches.push(ActiveExecutionBranch {
        task_id: TaskId::new("task-restart-branch-completed"),
        worker_id: WorkerId::new("worker-restart-branch-completed"),
        stage: "finish".to_string(),
        lease_id: None,
        execution_intent_ref: Some("worker-intent-task-restart-branch-completed".to_string()),
        binding_lifecycle: Some("bound".to_string()),
        checkpoint_stage: None,
        next_step_index: None,
        checkpoint_at: None,
        resume_mode: None,
        resume_token: None,
        use_tools: true,
        skill_name: None,
        is_primary: false,
    });
    chain.active_branch_task_ids = chain
        .branches
        .iter()
        .map(|branch| branch.task_id.clone())
        .collect();
    chain.active_worker_bindings = chain
        .branches
        .iter()
        .map(|branch| branch.worker_id.clone())
        .collect();
    let expected_active_branch_count = chain.branches.len();
    let expected_resumable_branch_count = chain
        .branches
        .iter()
        .filter(|branch| {
            task_store.get_task(&branch.task_id).is_some_and(|task| {
                matches!(
                    task.status,
                    TaskStatus::Blocked
                        | TaskStatus::Ready
                        | TaskStatus::Running
                        | TaskStatus::Verifying
                        | TaskStatus::Repairing
                )
            })
        })
        .count();
    state
        .session_store
        .upsert_active_execution_chain(session_id.clone(), chain.clone())
        .expect("augmented execution chain should persist to sidecar");

    let worker_runtime = state
        .execution_pipeline()
        .expect("execution pipeline should exist")
        .execution_runtime
        .worker_runtime()
        .clone();
    let parse_worker_stage = |stage: &str| match stage {
        "review" => WorkerStage::Review,
        "verify" => WorkerStage::Verify,
        "repair" => WorkerStage::Repair,
        "finish" => WorkerStage::Finish,
        _ => WorkerStage::Execute,
    };
    let parse_binding_lifecycle = |lifecycle: Option<&str>| match lifecycle {
        Some("bound") => Some(WorkerExecutionBindingLifecycle::Bound),
        Some("released") => Some(WorkerExecutionBindingLifecycle::Released),
        Some("none") => Some(WorkerExecutionBindingLifecycle::None),
        Some("requested") => Some(WorkerExecutionBindingLifecycle::Requested),
        None => None,
        Some(_) => Some(WorkerExecutionBindingLifecycle::Requested),
    };
    for branch in &chain.branches {
        worker_runtime.record_branch_checkpoint(
            &branch.task_id,
            &branch.worker_id,
            parse_worker_stage(&branch.stage),
            branch.lease_id.as_ref().map(ToString::to_string),
            branch.execution_intent_ref.clone(),
            parse_binding_lifecycle(branch.binding_lifecycle.as_deref()),
            branch.checkpoint_stage.as_deref().map(|checkpoint_stage| {
                magi_worker_runtime::WorkerExecutionCheckpointCursor {
                    checkpoint_stage: parse_worker_stage(checkpoint_stage),
                    next_step_index: branch.next_step_index.unwrap_or(0),
                    checkpoint_at: branch.checkpoint_at.unwrap_or(now),
                    resume_mode: match branch.resume_mode.as_deref() {
                        Some("step-checkpoint") => {
                            magi_worker_runtime::WorkerCheckpointResumeMode::StepCheckpoint
                        }
                        _ => magi_worker_runtime::WorkerCheckpointResumeMode::StageRestart,
                    },
                    resume_token: branch.resume_token.clone(),
                }
            }),
        );
    }

    fs::create_dir_all(&state_root).expect("state root should exist before task checkpoint");
    task_store
        .checkpoint_to_file(&state_root.join("task-store.json"))
        .expect("task store checkpoint should persist");
    let flush_report = RuntimeSidecarPersistence::new(
        repository.clone(),
        state.session_store.clone(),
        state.workspace_registry.clone(),
        worker_runtime.clone(),
    )
    .flush_runtime_sidecars()
    .expect("runtime sidecars should flush");
    assert!(flush_report.session_sidecars_flushed);
    assert!(flush_report.worker_runtime_snapshot_flushed);

    drop(app);
    drop(state);
    drop(runtime);

    let restarted_runtime = DaemonRuntime::restore(&config)
        .expect("second runtime restore should recover persisted execution chain");
    let (restarted_app, restarted_state) =
        restarted_runtime.router_with_state_for_tests("daemon-test".to_string());

    let before_continue_read_model = get_json(restarted_app.clone(), "/runtime/read-model").await;
    let session_summary = before_continue_read_model["details"]["sessions"]
        .as_array()
        .expect("session summaries should be an array")
        .iter()
        .find(|entry| entry["session_id"] == session_id_text)
        .expect("restarted runtime should contain target session summary");
    assert_eq!(session_summary["mission_id"], mission_id.to_string());
    assert_eq!(session_summary["root_task_id"], root_task_id.to_string());
    assert_eq!(session_summary["execution_chain_ref"], execution_chain_ref);
    assert_eq!(
        session_summary["recoverable_branch_count"],
        expected_resumable_branch_count as u64
    );
    assert_eq!(
        session_summary["active_branches"]
            .as_array()
            .expect("active_branches should be an array")
            .len(),
        expected_active_branch_count
    );
    let active_branches = session_summary["active_branches"]
        .as_array()
        .expect("active_branches should be an array");
    assert_eq!(
        active_branches
            .iter()
            .filter(|entry| entry["status"] == "blocked")
            .count(),
        expected_resumable_branch_count
    );
    assert!(
        active_branches
            .iter()
            .all(|entry| entry["status"] != "running"),
        "restart 后不应残留假活跃 running branch"
    );

    let worker_snapshot = repository
        .load_worker_runtime_snapshot()
        .expect("worker runtime snapshot should reload after restart");
    assert_eq!(worker_snapshot.branches.len(), expected_active_branch_count);
    let resumed_checkpoint_branch = worker_snapshot
        .branches
        .iter()
        .find(|branch| branch.task_id.as_str() == "task-restart-branch-1")
        .expect("checkpointed branch should survive restart");
    assert_eq!(resumed_checkpoint_branch.stage, WorkerStage::Execute);
    let resumed_checkpoint_cursor = resumed_checkpoint_branch
        .checkpoint_cursor
        .as_ref()
        .expect("checkpointed branch should retain checkpoint cursor");
    assert_eq!(
        resumed_checkpoint_cursor.checkpoint_stage,
        WorkerStage::Execute
    );
    assert_eq!(resumed_checkpoint_cursor.next_step_index, 1);
    assert_eq!(
        resumed_checkpoint_cursor.resume_mode,
        magi_worker_runtime::WorkerCheckpointResumeMode::StepCheckpoint
    );
    let completed_snapshot_branch = worker_snapshot
        .branches
        .iter()
        .find(|branch| branch.task_id.as_str() == "task-restart-branch-completed")
        .expect("completed branch snapshot should survive restart");
    assert_eq!(completed_snapshot_branch.stage, WorkerStage::Finish);
    assert_eq!(
        completed_snapshot_branch.binding_lifecycle,
        Some(WorkerExecutionBindingLifecycle::Bound)
    );
    assert!(
        completed_snapshot_branch.checkpoint_cursor.is_none(),
        "已完成 branch 不应保留可恢复 checkpoint"
    );

    let (continue_status, continue_body) = post_json(
        restarted_app.clone(),
        "/api/session/continue",
        json!({
            "sessionId": session_id_text,
        }),
    )
    .await;
    assert_eq!(
        continue_status,
        StatusCode::OK,
        "session continue after restart should succeed: {continue_body:?}"
    );
    assert_eq!(continue_body["missionId"], mission_id.to_string());
    assert_eq!(continue_body["rootTaskId"], root_task_id.to_string());
    assert_eq!(continue_body["executionChainRef"], execution_chain_ref);
    assert_eq!(
        continue_body["resumedBranchCount"],
        expected_resumable_branch_count as u64
    );

    let after_continue_read_model = get_json(restarted_app.clone(), "/runtime/read-model").await;
    let after_continue_summary = after_continue_read_model["details"]["sessions"]
        .as_array()
        .expect("session summaries should be an array after continue")
        .iter()
        .find(|entry| entry["session_id"] == session_id.to_string())
        .expect("session summary should still exist after continue");
    assert_eq!(after_continue_summary["current_status"], "resumed");
    assert_eq!(
        after_continue_summary["execution_chain_ref"],
        execution_chain_ref
    );
    assert_eq!(
        after_continue_summary["recoverable_branch_count"]
            .as_u64()
            .expect("recoverable_branch_count should serialize as integer"),
        0,
        "continue 后所有可恢复 branch 都应被消耗"
    );
    let after_continue_active_branches = after_continue_summary["active_branches"]
        .as_array()
        .expect("active_branches should remain an array after continue");
    let completed_branch_summary = after_continue_active_branches
        .iter()
        .find(|entry| entry["task_id"] == "task-restart-branch-completed")
        .expect("completed branch should still be visible after continue");
    assert_eq!(completed_branch_summary["status"], "completed");
    assert!(
        completed_branch_summary["checkpoint_stage"].is_null(),
        "已完成 branch 不应在 continue 后重新产生 checkpoint"
    );

    let task_store_json = fs::read_to_string(state_root.join("task-store.json"))
        .expect("task store checkpoint should remain readable");
    let task_store_value: Value =
        serde_json::from_str(&task_store_json).expect("task store checkpoint should be valid json");
    let mission_ids = task_store_value["tasks"]
        .as_array()
        .expect("checkpoint tasks should be an array")
        .iter()
        .filter_map(|task| task["mission_id"].as_str())
        .collect::<Vec<_>>();
    assert!(
        mission_ids
            .iter()
            .all(|mission_id| !mission_id.starts_with("mission-recovery-")),
        "continue 之后不应生成 recovery mission: {mission_ids:?}"
    );

    let resumed_chain = restarted_state
        .session_store
        .active_execution_chain(&session_id)
        .expect("active execution chain should still exist after continue");
    assert_eq!(resumed_chain.mission_id, mission_id);
    assert_eq!(resumed_chain.root_task_id, root_task_id);
    assert_eq!(resumed_chain.execution_chain_ref, execution_chain_ref);
    let completed_resumed_branch = resumed_chain
        .branches
        .iter()
        .find(|branch| branch.task_id.as_str() == "task-restart-branch-completed")
        .expect("completed branch should remain attached to resumed chain");
    assert_eq!(completed_resumed_branch.stage, "finish");
    assert!(
        completed_resumed_branch.checkpoint_stage.is_none(),
        "已完成 branch 不应在 resumed chain 中变成可恢复分支"
    );
    assert_eq!(
        restarted_state
            .task_store()
            .expect("task store should remain configured after restart")
            .get_task(&TaskId::new("task-restart-branch-completed"))
            .expect("completed branch task should remain in task store")
            .status,
        TaskStatus::Completed,
        "continue 不应重新激活已完成 branch"
    );
}

#[tokio::test]
async fn unbound_session_continue_survives_runtime_restart() {
    let state_root = temp_state_root("e2e-unbound-session-continue-restart");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());
    let repository = StateRepository::new(state_root.clone());

    let runtime = DaemonRuntime::restore(&config)
        .expect("first runtime restore should bootstrap empty state");
    let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());

    let (new_session_status, new_session_body) =
        post_json(app.clone(), "/api/session/new", json!({})).await;
    assert_eq!(
        new_session_status,
        StatusCode::OK,
        "new session should succeed: {new_session_body:?}"
    );
    let session_id_text = new_session_body["sessionId"]
        .as_str()
        .expect("sessionId should serialize as string")
        .to_string();
    let session_id = SessionId::new(session_id_text.clone());

    let (action_status, action_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "text": "Unbound session restart verification",
            "deep_task": false,
            "skill_name": "refactor",
            "images": [],
        }),
    )
    .await;
    assert_eq!(
        action_status,
        StatusCode::OK,
        "session action on unbound session should succeed: {action_body:?}"
    );

    let chain = state
        .session_store
        .active_execution_chain(&session_id)
        .expect("dispatch should create active execution chain for unbound session");
    let mission_id = chain.mission_id.clone();
    let root_task_id = chain.root_task_id.clone();
    let execution_chain_ref = chain.execution_chain_ref.clone();
    let task_store = state.task_store().expect("task store should be configured");

    task_store
        .update_status(&root_task_id, TaskStatus::Blocked)
        .expect("root task should become blocked");
    for branch in &chain.branches {
        task_store
            .update_status(&branch.task_id, TaskStatus::Blocked)
            .expect("branch task should become blocked");
    }

    state
        .persist_session_durable_state()
        .expect("unbound session durable state should persist globally");
    task_store
        .checkpoint_to_file(&state_root.join("task-store.json"))
        .expect("task store checkpoint should persist");
    let worker_runtime = state
        .execution_pipeline()
        .expect("execution pipeline should exist")
        .execution_runtime
        .worker_runtime()
        .clone();
    let flush_report = RuntimeSidecarPersistence::new(
        repository.clone(),
        state.session_store.clone(),
        state.workspace_registry.clone(),
        worker_runtime,
    )
    .flush_runtime_sidecars()
    .expect("runtime sidecars should flush");
    assert!(flush_report.session_sidecars_flushed);

    let global_session_state = repository
        .load_session_durable_state()
        .expect("global session durable state should reload");
    assert!(
        global_session_state
            .sessions
            .iter()
            .any(|session| session.session_id == session_id && session.workspace_id.is_none()),
        "unbound session must persist into global sessions.json"
    );

    drop(app);
    drop(state);
    drop(runtime);

    let restarted_runtime = DaemonRuntime::restore(&config)
        .expect("restart should recover unbound session state");
    let (restarted_app, restarted_state) =
        restarted_runtime.router_with_state_for_tests("daemon-test".to_string());

    assert!(
        restarted_state.session_store.session(&session_id).is_some(),
        "restart 后 session durable state 不应丢失"
    );
    let before_continue = get_json(restarted_app.clone(), "/runtime/read-model").await;
    let session_summary = before_continue["details"]["sessions"]
        .as_array()
        .expect("session summaries should be an array")
        .iter()
        .find(|entry| entry["session_id"] == session_id_text)
        .expect("restarted runtime should still export unbound session summary");
    assert_eq!(session_summary["mission_id"], mission_id.to_string());
    assert_eq!(session_summary["root_task_id"], root_task_id.to_string());
    assert_eq!(session_summary["execution_chain_ref"], execution_chain_ref);
    assert_eq!(
        session_summary["recoverable_branch_count"],
        chain.branches.len() as u64
    );

    let (continue_status, continue_body) = post_json(
        restarted_app.clone(),
        "/api/session/continue",
        json!({
            "sessionId": session_id_text,
        }),
    )
    .await;
    assert_eq!(
        continue_status,
        StatusCode::OK,
        "restarted unbound session continue should succeed: {continue_body:?}"
    );
    assert_eq!(continue_body["missionId"], mission_id.to_string());
    assert_eq!(continue_body["rootTaskId"], root_task_id.to_string());
    assert_eq!(continue_body["executionChainRef"], execution_chain_ref);
    assert_eq!(
        continue_body["resumedBranchCount"],
        chain.branches.len() as u64
    );
}

#[tokio::test]
async fn session_action_publishes_domain_event_on_event_bus() {
    let state_root = temp_state_root("e2e-session-action-events");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    let runtime = DaemonRuntime::restore(&config)
        .expect("runtime restore should bootstrap empty state");
    let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());
    let active_workspace_id = state
        .workspace_registry
        .active_workspace_id()
        .expect("bootstrap workspace should exist");
    state
        .session_store
        .create_session_for_workspace(
            SessionId::new("session-e2e-events"),
            "event session".to_string(),
            Some(active_workspace_id.to_string()),
        )
        .expect("event session should be creatable");

    let (status, body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "session_id": "session-e2e-events",
            "text": "event bus test",
            "deep_task": false,
            "skill_name": "code",
            "images": [],
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "session action should succeed: {body:?}"
    );

    let accepted_at = body["acceptedAt"].as_u64().unwrap();
    let expected_event_id = format!("event-session-turn-task-{accepted_at}");

    let snapshot = state.event_bus.snapshot();
    let action_event = snapshot
        .recent_events
        .iter()
        .find(|e| e.event_id.as_str() == expected_event_id);
    assert!(
        action_event.is_some(),
        "event bus should contain session.turn.task.accepted event (expected {expected_event_id}), found: {:?}",
        snapshot
            .recent_events
            .iter()
            .map(|e| e.event_id.as_str())
            .collect::<Vec<_>>()
    );

    let event = action_event.unwrap();
    assert_eq!(event.event_type, "session.turn.task.accepted");
    assert_eq!(
        event.payload["session_id"].as_str().unwrap(),
        "session-e2e-events"
    );
}

#[tokio::test]
async fn sequential_session_actions_share_session_and_accumulate_messages() {
    let state_root = temp_state_root("e2e-sequential-actions");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    let runtime = DaemonRuntime::restore(&config)
        .expect("runtime restore should bootstrap empty state");
    let (app, _state) = runtime.router_with_state_for_tests("daemon-test".to_string());

    let (status, first_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "text": "first action",
            "deep_task": false,
            "skill_name": "refactor",
            "images": [],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let session_id = first_body["sessionId"].as_str().unwrap().to_string();
    let first_root_task_id = first_body["rootTaskId"]
        .as_str()
        .expect("first root task id should serialize as string")
        .to_string();
    let first_projection =
        wait_for_task_projection_completed(app.clone(), &first_root_task_id, &session_id).await;
    assert_completed_two_task_projection(&first_projection);

    let (status, second_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "session_id": session_id,
            "text": "second action",
            "deep_task": false,
            "skill_name": "refactor",
            "images": [],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!second_body["createdSession"].as_bool().unwrap_or(true));
    let second_root_task_id = second_body["rootTaskId"]
        .as_str()
        .expect("second root task id should serialize as string")
        .to_string();
    let second_projection =
        wait_for_task_projection_completed(app.clone(), &second_root_task_id, &session_id).await;
    assert_completed_two_task_projection(&second_projection);

    let messages_page = get_json(
        app.clone(),
        &format!("/api/messages?sessionId={session_id}"),
    )
    .await;
    let timeline = messages_page["timeline"]
        .as_array()
        .expect("timeline should be an array");

    let user_messages: Vec<_> = timeline
        .iter()
        .filter(|entry| entry["kind"] == "UserMessage")
        .collect();
    assert_eq!(user_messages.len(), 2, "should have 2 user messages");

    let first_accepted_at = first_body["acceptedAt"].as_u64().unwrap();
    let second_accepted_at = second_body["acceptedAt"].as_u64().unwrap();
    let first_mission_id = format!("mission-session-action-{first_accepted_at}");
    let second_mission_id = format!("mission-session-action-{second_accepted_at}");

    let bootstrap = get_json(app, "/bootstrap").await;
    let execution_groups = bootstrap["runtimeReadModel"]["details"]["execution_groups"]
        .as_array()
        .expect("execution groups should be an array");
    assert!(
        execution_groups
            .iter()
            .any(|m| m["mission_id"] == first_mission_id),
        "bootstrap should contain first execution group"
    );
    assert!(
        execution_groups
            .iter()
            .any(|m| m["mission_id"] == second_mission_id),
        "bootstrap should contain second execution group"
    );
}
