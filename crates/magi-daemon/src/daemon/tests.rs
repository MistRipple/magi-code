use super::{
    app::Daemon,
    config::DaemonConfig,
    events::ledger_status_payload,
    maintenance::{
        RuntimeMaintenance, RuntimeMaintenanceConfig, RuntimeMaintenancePolicy,
        RuntimeMaintenanceStepOutcome, session_sidecar_flush_due, workspace_sidecar_flush_due,
    },
    persistence::{RuntimeSidecarFlushReport, RuntimeSidecarPersistence, StateRepository},
    runtime::DaemonRuntime,
    types::{DaemonMaintenanceMode, DaemonMaintenancePolicyProfile},
};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use magi_core::{
    AbsolutePath, AccessProfile, EventId, ExecutionOwnership, LeaseId, MissionId, SessionId, Task,
    TaskId, TaskKind, TaskStatus, ThreadId, UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::{EventEnvelope, InMemoryEventBus, RuntimeLedgerSummary};
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::{
    ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
    ActiveExecutionTurn, ActiveExecutionTurnItem, ExecutionThread, ExecutionThreadStatus,
    SessionExecutionSidecarStatus, SessionSidecarFlushMetadata, SessionSidecarFlushReason,
    SessionStore,
};
use magi_worker_runtime::{
    WorkerBranchCheckpointState, WorkerExecutionBindingLifecycle, WorkerRuntime, WorkerStage,
};
use magi_workspace::{
    RecoveryStatus, WorkspaceRecoveryFlushMetadata, WorkspaceRecoveryFlushReason, WorkspaceStore,
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, sync::Arc};
use tokio::sync::broadcast;
use tokio::time::{Duration, Instant};
use tower::util::ServiceExt;

const BACKGROUND_TASK_PROJECTION_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_TEST_WORKSPACE_ID: &str = "test-workspace-001";

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

#[test]
fn workspace_session_loader_preserves_goals() {
    let state_root = temp_state_root("workspace-goal-load");
    let workspace_root = temp_state_root("workspace-goal-load-root");
    let repository = StateRepository::new(state_root.clone());
    let workspace_id = WorkspaceId::new("workspace-goal-load");
    let workspace_store = WorkspaceStore::new();
    workspace_store
        .register(
            workspace_id.clone(),
            AbsolutePath::new(workspace_root.to_string_lossy().to_string()),
        )
        .expect("workspace should register");
    repository
        .save_workspace_durable_state(&workspace_store.durable_state())
        .expect("workspace state should save");
    let session_store = SessionStore::default();
    let session_id = SessionId::new("session-goal-load");
    session_store
        .create_session_for_workspace(
            session_id.clone(),
            "goal load",
            Some(workspace_id.to_string()),
        )
        .expect("workspace session should create");
    let (_, thread_id) =
        session_store.ensure_session_mission(&session_id, UtcMillis::now(), || {
            MissionId::new("mission-goal-load")
        });
    let goal = session_store
        .create_goal(
            session_id.clone(),
            thread_id,
            "turn-goal-load",
            "恢复工作区目标",
            AccessProfile::Restricted,
            Some(256_000),
        )
        .expect("goal should create");
    let workspace_state = session_store.durable_state();
    repository
        .save_session_projection_state(
            &workspace_state,
            &session_store.execution_sidecar_store_state(),
        )
        .expect("workspace session state should save");

    let (loaded, _) = repository
        .load_session_projections(&[(workspace_id.to_string(), workspace_root.clone())])
        .expect("workspace sessions should load");

    assert_eq!(loaded.goals.len(), 1);
    assert_eq!(loaded.goals[0].goal_id, goal.goal_id);
    assert_eq!(loaded.goals[0].objective, "恢复工作区目标");

    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(workspace_root);
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
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should read");
    let parsed: Value = serde_json::from_slice(&bytes).unwrap_or_else(|err| {
        let snippet = String::from_utf8_lossy(&bytes);
        panic!("post_json({path}) status={status} failed to parse: {err}; body: {snippet}");
    });
    (status, parsed)
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
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should read");
    if status != StatusCode::OK {
        let snippet = String::from_utf8_lossy(&bytes);
        panic!("get_json({path}) returned {status}: {snippet}");
    }
    serde_json::from_slice(&bytes).expect("response should be valid json")
}

async fn wait_for_event_matching(
    receiver: &mut broadcast::Receiver<EventEnvelope>,
    description: &str,
    mut predicate: impl FnMut(&EventEnvelope) -> bool,
) -> EventEnvelope {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout(Duration::from_millis(250), receiver.recv()).await {
            Ok(Ok(event)) if predicate(&event) => return event,
            Ok(Ok(_)) => {}
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                panic!("event bus closed before receiving {description}");
            }
            Err(_) => {
                if Instant::now() >= deadline {
                    panic!("timed out waiting for {description}");
                }
            }
        }
    }
}

fn event_payload_contains_request_id(event: &EventEnvelope, request_id: &str) -> bool {
    event.payload.to_string().contains(request_id)
}

async fn get_agent_run_projection(
    app: axum::Router,
    root_task_id: &str,
    session_id: &str,
    workspace_id: &str,
) -> Value {
    get_json(
        app,
        &format!(
            "/api/agent-runs/projection/{root_task_id}?scope=workspace&workspaceId={workspace_id}&sessionId={session_id}"
        ),
    )
    .await
}

async fn wait_for_agent_run_projection_completed(
    app: axum::Router,
    root_task_id: &str,
    session_id: &str,
    workspace_id: &str,
) -> Value {
    let deadline = Instant::now() + BACKGROUND_TASK_PROJECTION_TIMEOUT;
    loop {
        let projection =
            get_agent_run_projection(app.clone(), root_task_id, session_id, workspace_id).await;
        let total_tasks = projection["progress_summary"]["total_tasks"]
            .as_u64()
            .unwrap_or(0);
        let completed_tasks = projection["progress_summary"]["completed_tasks"]
            .as_u64()
            .unwrap_or(0);
        // Dispatch：ExecutionChain 只挂一个 root task；coordinator 才会再 spawn 代理任务。
        if total_tasks >= 1
            && completed_tasks == total_tasks
            && projection["root_task"]["status"] == "completed"
        {
            // TaskStore 的 durable terminal 与 canonical Turn finalizer 由不同回调
            // 顺序完成；只有 Turn 也已经进入终态，后续同一 session 的请求才不会
            // 在这个等待窗口内被错误地判定为“已有活动 Turn”。
            let canonical_terminal = get_json(app.clone(), "/runtime/read-model").await["details"]
                ["sessions"]
                .as_array()
                .and_then(|sessions| {
                    sessions
                        .iter()
                        .find(|entry| entry["session_id"] == session_id)
                })
                .and_then(|entry| entry["current_turn"]["status"].as_str())
                .is_some_and(|status| {
                    matches!(
                        status,
                        "completed"
                            | "complete"
                            | "succeeded"
                            | "success"
                            | "failed"
                            | "error"
                            | "interrupted"
                            | "cancelled"
                            | "canceled"
                            | "blocked"
                            | "killed"
                            | "superseded"
                    )
                });
            if canonical_terminal {
                return projection;
            }
        }
        if Instant::now() >= deadline {
            return projection;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_agent_run_projection_settled(
    app: axum::Router,
    root_task_id: &str,
    session_id: &str,
    workspace_id: &str,
) -> Value {
    let deadline = Instant::now() + BACKGROUND_TASK_PROJECTION_TIMEOUT;
    loop {
        let projection =
            get_agent_run_projection(app.clone(), root_task_id, session_id, workspace_id).await;
        let total_tasks = projection["progress_summary"]["total_tasks"]
            .as_u64()
            .unwrap_or(0);
        let running_tasks = projection["progress_summary"]["running_tasks"]
            .as_u64()
            .unwrap_or(0);
        let root_is_terminal = matches!(
            projection["root_task"]["status"].as_str(),
            Some("completed" | "failed" | "killed" | "cancelled")
        );
        if total_tasks >= 1 && running_tasks == 0 && root_is_terminal {
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
    let deadline = Instant::now() + BACKGROUND_TASK_PROJECTION_TIMEOUT;
    loop {
        let read_model = get_json(app.clone(), "/runtime/read-model").await;
        if let Some(group) = read_model["details"]["execution_groups"]
            .as_array()
            .and_then(|groups| {
                groups
                    .iter()
                    .find(|entry| entry["mission_id"] == mission_id)
            })
            && (is_ready(group) || Instant::now() >= deadline)
        {
            return group.clone();
        }
        if Instant::now() >= deadline {
            panic!("execution group {mission_id} did not appear before timeout");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn assert_completed_two_agent_run_projection(projection: &Value) {
    let total_tasks = projection["progress_summary"]["total_tasks"]
        .as_u64()
        .expect("total_tasks should serialize as integer");
    let completed_tasks = projection["progress_summary"]["completed_tasks"]
        .as_u64()
        .expect("completed_tasks should serialize as integer");
    // ExecutionChain：单 worker 任务只产出 1 个 root task；coordinator 才会扩展为多任务。
    assert!(
        total_tasks >= 1,
        "agent run projection should include the root task"
    );
    assert_eq!(
        completed_tasks, total_tasks,
        "agent run projection should be fully completed, got summary={:?}, root_status={}, tasks={:?}",
        projection["progress_summary"], projection["root_task"]["status"], projection["tasks"]
    );
    assert_eq!(projection["progress_summary"]["failed_tasks"], 0);
    assert_eq!(projection["root_task"]["status"], "completed");
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
    let workspace_root = temp_workspace_absolute_path("runtime-sidecar-flush-workspace");
    let persistence = test_sidecar_persistence(
        repository.clone(),
        session_store.clone(),
        workspace_store.clone(),
    );

    session_store
        .create_session(SessionId::new("session-flush"), "flush session")
        .expect("session should be creatable");
    workspace_store
        .register(WorkspaceId::new("workspace-flush"), workspace_root.clone())
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
    assert!(
        PathBuf::from(workspace_root.as_str())
            .join(".magi/session-projections")
            .exists()
    );
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
    session_store.install_canonical_event_writer(Arc::new(repository.clone()));
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
    let (_mission_id, orchestrator_thread_id) =
        session_store.ensure_session_mission(&session_id, UtcMillis(5), || {
            MissionId::new("mission-flush-canonical")
        });
    let turn_id = "turn-flush-canonical";
    let coordinator = magi_conversation_runtime::SessionTurnCoordinator::new();
    let attempt = match coordinator
        .execute_command(
            &session_id,
            magi_conversation_runtime::TurnCommand::Start(
                magi_conversation_runtime::TurnAdmission {
                    turn_id: turn_id.to_string(),
                    request_id: "req-flush-canonical".to_string(),
                    request_fingerprint: "fingerprint-flush-canonical".to_string(),
                    profile: magi_conversation_runtime::ExecutionProfile::Conversation,
                },
            ),
        )
        .expect("flush canonical Turn should start")
    {
        magi_conversation_runtime::CoordinatorCommandResult::Admission(
            magi_conversation_runtime::CoordinatorAdmission::Accepted(attempt),
        ) => attempt,
        other => panic!("unexpected flush canonical admission: {other:?}"),
    };
    magi_conversation_runtime::CanonicalTurnEventSink::for_store(&session_store, None)
        .accept_conversation_turn_with_timeline_entry(
            session_id.clone(),
            Some(workspace_id.clone()),
            magi_session_store::TimelineEntryInput::new(
                "timeline-flush-canonical",
                magi_session_store::TimelineEntryKind::UserMessage,
                "关闭前端后继续执行",
                UtcMillis(10),
            ),
            ActiveExecutionTurn {
                turn_id: turn_id.to_string(),
                turn_seq: 10,
                accepted_at: UtcMillis(10),
                completed_at: None,
                status: "accepted".to_string(),
                user_message: Some("关闭前端后继续执行".to_string()),
                items: vec![ActiveExecutionTurnItem {
                    item_id: "turn-item-flush-canonical-assistant".to_string(),
                    item_seq: 1,
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
                    metadata: Default::default(),
                    timeline_entry_id: None,
                    source_thread_id: orchestrator_thread_id.clone(),
                }],
            },
        )
        .expect("flush canonical Turn should persist through sink");
    coordinator
        .execute_command(
            &session_id,
            magi_conversation_runtime::TurnCommand::SetStatus {
                attempt: attempt.clone(),
                status: magi_conversation_runtime::CoordinatorTurnStatus::Preparing,
            },
        )
        .expect("flush canonical Turn should prepare");
    coordinator
        .execute_command(
            &session_id,
            magi_conversation_runtime::TurnCommand::SetStatus {
                attempt,
                status: magi_conversation_runtime::CoordinatorTurnStatus::Running,
            },
        )
        .expect("flush canonical Turn should run");
    magi_conversation_runtime::CanonicalTurnEventSink::for_store(&session_store, None)
        .set_status_domain(&session_id, Some(turn_id), "running")
        .expect("flush canonical Turn running status should persist");

    let report = persistence
        .flush_runtime_sidecars()
        .expect("sidecar flush should also persist canonical turns");
    assert!(report.session_sidecars_flushed);

    let (session_durable, _) = repository
        .load_session_projections(&[(workspace_id.to_string(), workspace_root)])
        .expect("workspace sessions should reload");
    assert_eq!(session_durable.canonical_turns.len(), 1);
    assert_eq!(
        session_durable.canonical_turns[0].turn_id,
        "turn-flush-canonical"
    );
    assert_eq!(session_durable.canonical_turns[0].items.len(), 1);
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
        WorkerBranchCheckpointState {
            lease_id: Some("lease-worker-snapshot".to_string()),
            execution_intent_ref: Some("worker-intent-task-worker-snapshot".to_string()),
            binding_lifecycle: Some(WorkerExecutionBindingLifecycle::Requested),
            checkpoint_cursor: Some(magi_worker_runtime::WorkerExecutionCheckpointCursor {
                checkpoint_stage: WorkerStage::Execute,
                next_step_index: 1,
                checkpoint_at: UtcMillis::now(),
                resume_mode: magi_worker_runtime::WorkerCheckpointResumeMode::StepCheckpoint,
                resume_token: None,
            }),
        },
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
        WorkerBranchCheckpointState {
            lease_id: Some("lease-maintenance-worker".to_string()),
            execution_intent_ref: Some("worker-intent-task-maintenance-worker".to_string()),
            binding_lifecycle: Some(WorkerExecutionBindingLifecycle::Requested),
            checkpoint_cursor: Some(magi_worker_runtime::WorkerExecutionCheckpointCursor {
                checkpoint_stage: WorkerStage::Verify,
                next_step_index: 1,
                checkpoint_at: UtcMillis::now(),
                resume_mode: magi_worker_runtime::WorkerCheckpointResumeMode::StageRestart,
                resume_token: None,
            }),
        },
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
    let workspace_root = temp_workspace_absolute_path("recovery-sidecar-incremental-workspace");
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
        .register(workspace_id.clone(), workspace_root.clone())
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

    let (_, reloaded_session_sidecars) = repository
        .load_session_projections(&[(
            workspace_id.to_string(),
            PathBuf::from(workspace_root.as_str()),
        )])
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
    let runtime = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("runtime restore should load explicit test fixture");
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
        .workspace_registry
        .register(
            workspace_id.clone(),
            temp_workspace_absolute_path("router-recovery-workspace"),
        )
        .expect("workspace should be registrable");
    state
        .session_store
        .create_session_for_workspace(
            session_id.clone(),
            "router recovery session".to_string(),
            Some(workspace_id.to_string()),
        )
        .expect("session should be creatable");
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
                    thread_id: ThreadId::new("thread-router-recovery"),
                }],
                recovery_ref: Some("recovery-router-recovery".to_string()),
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: UtcMillis::now(),
                    entry_id: "timeline-router-recovery".to_string(),
                    trimmed_text: Some("resume parser after crash".to_string()),
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
    task_store
        .insert_task(Task {
            task_id: root_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: "recovery mission".to_string(),
            goal: "recovery mission".to_string(),
            status: TaskStatus::Running,
            dependency_ids: Vec::new(),
            required_children: vec![task_id.clone()],
            policy_snapshot: None,
            executor_binding: None,
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        })
        .expect("根任务应插入");
    task_store
        .insert_task(Task {
            task_id: task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id,
            parent_task_id: Some(TaskId::new("task-root-router-recovery")),
            kind: TaskKind::LocalAgent,
            title: "recovery task".to_string(),
            goal: "recovery task".to_string(),
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
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        })
        .expect("任务应插入");

    let expected_extraction_id =
        format!("extract-session-continue-{}", recovery_handle.recovery_id);
    let (status, recovery_body) = post_json(
        app.clone(),
        "/api/session/continue",
        json!({
            "sessionId": session_id.to_string(),
            "workspaceId": workspace_id.to_string(),
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

    // Continue 会启动同一执行链的新 runner；普通 followup 在 runner 仍运行时应进入
    // session 队列，而不是与恢复 runner 并发创建第二个 root task。等待恢复完成后，
    // 才验证 followup 的正常派发与恢复写回消费。
    let recovery_projection = wait_for_agent_run_projection_settled(
        app.clone(),
        "task-root-router-recovery",
        session_id.as_str(),
        workspace_id.as_str(),
    )
    .await;
    assert_eq!(
        recovery_projection["root_task"]["status"], "failed",
        "recovery preflight fixture should settle the unavailable executor as a terminal failure"
    );

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

    let detach_deadline = Instant::now() + BACKGROUND_TASK_PROJECTION_TIMEOUT;
    loop {
        let detached = state
            .session_store
            .runtime_sidecar(&session_id)
            .is_some_and(|sidecar| {
                matches!(sidecar.status, SessionExecutionSidecarStatus::Detached)
                    && sidecar.active_execution_chain.is_none()
                    && sidecar.current_turn.as_ref().is_some_and(|turn| {
                        matches!(
                            turn.status.trim().to_ascii_lowercase().as_str(),
                            "failed" | "cancelled" | "completed"
                        )
                    })
            });
        if detached {
            break;
        }
        if Instant::now() >= detach_deadline {
            panic!("恢复任务终态通知未在超时前释放 session execution chain");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
    assert_eq!(
        session_summary["current_status"], "detached",
        "恢复 runner 已进入失败终态后应释放执行链，后续普通 followup 再重新绑定新链"
    );
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

    let followup_request_id = "request-router-recovery-followup";
    let (status, followup_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "scope": "workspace",
            "sessionId": session_id.to_string(),
            "text": "修复恢复任务后续问题，完成后运行测试并汇总验证结果",
            "skillName": "resume",
            "images": [],
            "workspaceId": workspace_id.to_string(),
            "requestId": followup_request_id,
            "userMessageId": "user-router-recovery-followup",
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected followup response body: {followup_body:?}"
    );

    let _accepted_at = followup_body["acceptedAt"]
        .as_u64()
        .expect("accepted_at should serialize as integer");
    // session 一生一 mission：followup dispatch 复用 recovery 阶段已绑定的 mission_id
    let followup_mission_id = "mission-router-recovery".to_string();
    let followup_root_task_id = if let Some(root_task_id) = followup_body["rootTaskId"].as_str() {
        root_task_id.to_string()
    } else {
        assert_eq!(
            followup_body["queued"], true,
            "忙碌 session 的 followup 应进入队列: {followup_body:?}"
        );
        let deadline = Instant::now() + BACKGROUND_TASK_PROJECTION_TIMEOUT;
        loop {
            if let Some(turn) = state
                .session_store
                .canonical_turn_for_request_id(followup_request_id)
                && let Some(task_id) = turn
                    .items
                    .iter()
                    .find(|item| {
                        item.kind == magi_session_store::CanonicalTurnItemKind::UserMessage
                    })
                    .and_then(|item| item.worker.as_ref())
                    .and_then(|worker| worker.task_id.as_ref())
            {
                break task_id.to_string();
            }
            if Instant::now() >= deadline {
                panic!(
                    "queued recovery followup was not accepted into canonical Turn before timeout"
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let followup_execution_group =
        wait_for_execution_group(app.clone(), &followup_mission_id, |entry| {
            entry["context_memory_extraction_refs"]
                .as_array()
                .is_some_and(|refs| refs.iter().any(|value| value == &expected_extraction_id))
                && entry["context_used_memory_count"]
                    .as_u64()
                    .is_some_and(|count| count >= 1)
                && entry["context_extracted_memory_count"]
                    .as_u64()
                    .is_some_and(|count| count >= 1)
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
    let followup_projection = wait_for_agent_run_projection_completed(
        app,
        &followup_root_task_id,
        session_id.as_str(),
        workspace_id.as_str(),
    )
    .await;
    assert_completed_two_agent_run_projection(&followup_projection);
}

#[tokio::test]
async fn daemon_bootstrap_exports_session_action_context_summary_after_followup_dispatch() {
    let state_root = temp_state_root("router-bootstrap-context-summary");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    let runtime = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("runtime restore should load explicit test fixture");
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
            "scope": "workspace",
            "sessionId": "test-session-bootstrap",
            "text": "修复路由解析刷新问题，完成后运行测试并汇总验证结果",
            "skillName": "refactor",
            "images": [],
            "workspaceId": active_workspace_id.to_string(),
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
    let expected_extraction_id = format!(
        "extract-session-action-{}-{first_accepted_at}-timeline-{}-{first_accepted_at}",
        session_id.as_str(),
        session_id.as_str()
    );
    let first_root_task_id = first_body["rootTaskId"]
        .as_str()
        .expect("root_task_id should serialize as string");
    let first_projection = wait_for_agent_run_projection_completed(
        app.clone(),
        first_root_task_id,
        session_id.as_str(),
        active_workspace_id.as_str(),
    )
    .await;
    assert_completed_two_agent_run_projection(&first_projection);

    let second_request_id = "request-bootstrap-context-summary-followup";
    let (status, second_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "scope": "workspace",
            "sessionId": "test-session-bootstrap",
            "text": "继续修复路由解析刷新后续问题，完成后运行测试并汇总验证结果",
            "skillName": "refactor",
            "images": [],
            "workspaceId": active_workspace_id.to_string(),
            "requestId": second_request_id,
            "userMessageId": "user-bootstrap-context-summary-followup",
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected second body: {second_body:?}"
    );

    let _second_accepted_at = second_body["acceptedAt"]
        .as_u64()
        .expect("accepted_at should serialize as integer");
    // session 一生一 mission：第二次派发复用第一次派发创建的 mission_id
    let second_mission_id = format!("mission-session-action-{first_accepted_at}");
    let second_root_task_id = if let Some(root_task_id) = second_body["rootTaskId"].as_str() {
        root_task_id.to_string()
    } else {
        assert_eq!(
            second_body["queued"], true,
            "忙碌 session 的 followup 应进入队列: {second_body:?}"
        );
        let deadline = Instant::now() + BACKGROUND_TASK_PROJECTION_TIMEOUT;
        loop {
            if let Some(turn) = state
                .session_store
                .canonical_turn_for_request_id(second_request_id)
                && let Some(task_id) = turn
                    .items
                    .iter()
                    .find(|item| {
                        item.kind == magi_session_store::CanonicalTurnItemKind::UserMessage
                    })
                    .and_then(|item| item.worker.as_ref())
                    .and_then(|worker| worker.task_id.as_ref())
            {
                break task_id.to_string();
            }
            if Instant::now() >= deadline {
                panic!("queued followup was not accepted into canonical Turn before timeout");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let second_execution_group =
        wait_for_execution_group(app.clone(), &second_mission_id, |entry| {
            entry["context_memory_extraction_refs"] == json!([expected_extraction_id])
                && entry["context_used_memory_count"] == 1
                && entry["context_extracted_memory_count"] == 1
        })
        .await;
    assert_eq!(
        second_execution_group["context_memory_extraction_refs"],
        json!([expected_extraction_id])
    );
    let second_projection = wait_for_agent_run_projection_completed(
        app.clone(),
        &second_root_task_id,
        session_id.as_str(),
        active_workspace_id.as_str(),
    )
    .await;
    assert_completed_two_agent_run_projection(&second_projection);
    let bootstrap = get_json(
        app.clone(),
        "/bootstrap?scope=workspace&workspaceId=test-workspace-001",
    )
    .await;
    let bootstrap_execution_group = bootstrap["runtimeReadModel"]["details"]["execution_groups"]
        .as_array()
        .expect("bootstrap execution groups should be an array")
        .iter()
        .find(|entry| entry["mission_id"] == second_mission_id)
        .expect("second execution group should exist in bootstrap runtime read model");
    let extraction_refs = bootstrap_execution_group["context_memory_extraction_refs"]
        .as_array()
        .expect("context_memory_extraction_refs should be an array");
    assert!(
        extraction_refs
            .iter()
            .any(|value| value == &json!(expected_extraction_id)),
        "second mission should reference the first extraction id, got {:?}",
        extraction_refs
    );
}

#[tokio::test]
async fn daemon_bootstrap_exports_recovery_context_after_resume_and_followup_dispatch() {
    let state_root = temp_state_root("router-bootstrap-recovery-context");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    let runtime = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("runtime restore should load explicit test fixture");
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
            "scope": "workspace",
            "sessionId": "test-session-bootstrap-recovery",
            "text": "修复 bootstrap 恢复状态初始化问题，完成后运行测试并汇总验证结果",
            "skillName": "resume",
            "images": [],
            "workspaceId": active_workspace_id.to_string(),
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
    // session 一生一 mission：后续 followup dispatch 复用 seed 绑定的 mission_id
    let seed_accepted_at = seed_body["acceptedAt"]
        .as_u64()
        .expect("seed accepted_at should serialize as integer");

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
        .revoke_lease_and_set_task_terminal(
            &recovery_task_id,
            &recovery_task_id,
            None,
            TaskStatus::Failed,
            Vec::new(),
        )
        .expect("seed task should become recoverable")
        .then_some(())
        .expect("seed task should become recoverable");
    let snapshot = state.workspace_registry.append_execution_snapshot(
        workspace_id.clone(),
        ownership.clone(),
        "snapshot-bootstrap-recovery",
        "Bootstrap recovery snapshot",
    );
    let recovery = state.workspace_registry.prepare_recovery_entry(
        workspace_id.clone(),
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
            "workspaceId": workspace_id.to_string(),
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
    let seed_projection = wait_for_agent_run_projection_completed(
        app.clone(),
        &seed_root_task_id,
        session_id.as_str(),
        workspace_id.as_str(),
    )
    .await;
    assert_completed_two_agent_run_projection(&seed_projection);
    let after_resume_read_model = get_json(app.clone(), "/runtime/read-model").await;
    let after_resume_bootstrap = get_json(
        app.clone(),
        "/bootstrap?scope=workspace&workspaceId=test-workspace-001",
    )
    .await;
    // bootstrap 与独立 read-model 请求之间可能有后台终态事件到达；动态的
    // recent_event_count/latest_sequence 允许前进，但协议元数据必须保持一致。
    for key in [
        "contract_version",
        "contract_sections",
        "ordering_strategy",
        "section_ordering_rules",
    ] {
        assert_eq!(
            after_resume_bootstrap["runtimeReadModel"]["meta"][key],
            after_resume_read_model["meta"][key],
            "bootstrap 应保留稳定运行态元信息 {key}"
        );
    }
    assert!(
        after_resume_bootstrap["runtimeReadModel"]["details"]["sessions"]
            .as_array()
            .expect("bootstrap sessions should be array")
            .iter()
            .all(|entry| entry["session_id"] == session_id.as_str()),
        "bootstrap 首屏运行态应裁剪到当前会话"
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

    let followup_request_id = "request-bootstrap-recovery-followup";
    let (status, followup_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "scope": "workspace",
            "sessionId": "test-session-bootstrap-recovery",
            "text": "修复 resumed bootstrap memory 消费问题，完成后运行测试并汇总验证结果",
            "skillName": "resume",
            "images": [],
            "workspaceId": workspace_id.to_string(),
            "requestId": followup_request_id,
            "userMessageId": "user-bootstrap-recovery-followup",
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected followup body: {followup_body:?}"
    );

    let _followup_accepted_at = followup_body["acceptedAt"]
        .as_u64()
        .expect("accepted_at should serialize as integer");
    let followup_mission_id = format!("mission-session-action-{seed_accepted_at}");
    let followup_root_task_id = if let Some(root_task_id) = followup_body["rootTaskId"].as_str() {
        root_task_id.to_string()
    } else {
        assert_eq!(
            followup_body["queued"], true,
            "忙碌 session 的 followup 应进入队列: {followup_body:?}"
        );
        let deadline = Instant::now() + BACKGROUND_TASK_PROJECTION_TIMEOUT;
        loop {
            if let Some(turn) = state
                .session_store
                .canonical_turn_for_request_id(followup_request_id)
                && let Some(task_id) = turn
                    .items
                    .iter()
                    .find(|item| {
                        item.kind == magi_session_store::CanonicalTurnItemKind::UserMessage
                    })
                    .and_then(|item| item.worker.as_ref())
                    .and_then(|worker| worker.task_id.as_ref())
            {
                break task_id.to_string();
            }
            if Instant::now() >= deadline {
                panic!("queued followup was not accepted into canonical Turn before timeout");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let followup_execution_group =
        wait_for_execution_group(app.clone(), &followup_mission_id, |entry| {
            entry["context_memory_extraction_refs"]
                .as_array()
                .is_some_and(|refs| refs.iter().any(|value| value == expected_extraction_id))
                && entry["context_used_memory_count"]
                    .as_u64()
                    .is_some_and(|count| count >= 1)
                && entry["context_extracted_memory_count"]
                    .as_u64()
                    .is_some_and(|count| count >= 1)
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
    let followup_projection = wait_for_agent_run_projection_completed(
        app.clone(),
        &followup_root_task_id,
        session_id.as_str(),
        workspace_id.as_str(),
    )
    .await;
    assert_completed_two_agent_run_projection(&followup_projection);
    let bootstrap = get_json(
        app.clone(),
        "/bootstrap?scope=workspace&workspaceId=test-workspace-001",
    )
    .await;
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
    let workspace_root = temp_workspace_absolute_path("runtime-maintenance-workspace");
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
            workspace_root.clone(),
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
    assert!(
        PathBuf::from(workspace_root.as_str())
            .join(".magi/session-projections")
            .exists()
    );
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
                ledger_flush_interval: Duration::from_millis(1),
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

    // workspace 注册事实必须先于引用它的 session projection。
    repository
        .save_workspace_durable_state(&workspace_store.durable_state())
        .expect("workspace durable state should save");
    repository
        .save_session_projection_state(
            &session_store.durable_state(),
            &session_store.execution_sidecar_store_state(),
        )
        .expect("session projection should save");

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

    let (restarted_durable, restarted_sidecars) = repository
        .load_session_projections(&[(workspace_id.to_string(), workspace_root.clone())])
        .expect("session durable state should reload");
    let restarted_session_store = Arc::new(
        SessionStore::from_persisted_parts(restarted_durable, restarted_sidecars)
            .expect("重启后的会话存储持久化恢复应成功"),
    );
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
            .save_workspace_durable_state(&workspace_store.durable_state())
            .expect("durable workspace save should succeed");
        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("session projection save should succeed");

        let persistence =
            test_sidecar_persistence(repository.clone(), session_store, workspace_store);
        persistence
            .flush_runtime_sidecars()
            .expect("first-gen flush should succeed");
    }

    // ── Phase 2: Restart and mutate ──
    let (durable_2, sidecars_2) = repository
        .load_session_projections(&[(workspace_id.to_string(), workspace_root.clone())])
        .expect("load");
    let session_store_2 = Arc::new(
        SessionStore::from_persisted_parts(durable_2, sidecars_2)
            .expect("重启后的会话存储二次恢复应成功"),
    );
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

    let (durable_3, sidecars_3) = repository
        .load_session_projections(&[(workspace_id.to_string(), workspace_root.clone())])
        .expect("load");
    let session_store_3 = SessionStore::from_persisted_parts(durable_3, sidecars_3)
        .expect("二次重启后的会话存储持久化恢复应成功");
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

    // workspace 注册事实必须先于引用它的 session projection。
    repository
        .save_workspace_durable_state(&workspace_store.durable_state())
        .expect("durable save should succeed");
    repository
        .save_session_projection_state(
            &session_store.durable_state(),
            &session_store.execution_sidecar_store_state(),
        )
        .expect("session projection save should succeed");

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

    let (restarted_durable, restarted_sidecars) = repository
        .load_session_projections(&[(workspace_id.to_string(), workspace_root.clone())])
        .expect("load");
    let restarted_session = Arc::new(
        SessionStore::from_persisted_parts(restarted_durable, restarted_sidecars)
            .expect("维护周期后的会话存储持久化恢复应成功"),
    );
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
async fn session_turn_live_events_reach_multiple_subscribers() {
    let state_root = temp_state_root("e2e-session-live-event-subscribers");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    let runtime = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("runtime restore should load explicit test fixture");
    let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());

    let mut first_receiver = state.event_bus.subscribe();
    let mut second_receiver = state.event_bus.subscribe();

    let (status, body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "scope": "workspace",
            "text": "multi subscriber live event",
            "workspaceId": DEFAULT_TEST_WORKSPACE_ID,
            "requestId": "request-multi-subscriber-live",
            "userMessageId": "user-multi-subscriber-live",
            "placeholderMessageId": "placeholder-multi-subscriber-live",
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "session turn should accept: {body:?}"
    );
    let session_id = body["sessionId"]
        .as_str()
        .expect("session id should serialize as string");

    let first_event = wait_for_event_matching(
        &mut first_receiver,
        "first subscriber session.turn.conversation.accepted",
        |event| {
            event.event_type == "session.turn.conversation.accepted"
                && event_payload_contains_request_id(event, "request-multi-subscriber-live")
        },
    )
    .await;
    let second_event = wait_for_event_matching(
        &mut second_receiver,
        "second subscriber session.turn.conversation.accepted",
        |event| {
            event.event_type == "session.turn.conversation.accepted"
                && event_payload_contains_request_id(event, "request-multi-subscriber-live")
        },
    )
    .await;

    assert_eq!(
        first_event.session_id.as_ref().map(SessionId::as_str),
        Some(session_id)
    );
    assert_eq!(
        second_event.session_id.as_ref().map(SessionId::as_str),
        Some(session_id)
    );
    assert_eq!(
        first_event.workspace_id.as_ref().map(WorkspaceId::as_str),
        Some("test-workspace-001")
    );
    assert_eq!(
        second_event.workspace_id.as_ref().map(WorkspaceId::as_str),
        Some("test-workspace-001")
    );
}

#[tokio::test]
async fn session_turn_persists_without_live_subscriber_and_recovers_after_restart() {
    let state_root = temp_state_root("e2e-session-no-subscriber-recovery");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());

    let runtime = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("runtime restore should load explicit test fixture");
    let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());
    let (status, body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "scope": "workspace",
            "text": "no subscriber persistence",
            "workspaceId": "test-workspace-001",
            "requestId": "request-no-subscriber-recovery",
            "userMessageId": "user-no-subscriber-recovery",
            "placeholderMessageId": "placeholder-no-subscriber-recovery",
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "session turn should accept: {body:?}"
    );
    let session_id = body["sessionId"]
        .as_str()
        .expect("session id should serialize as string")
        .to_string();
    let turn_id = body["canonicalTurn"]["turnId"]
        .as_str()
        .expect("accepted response should carry canonical turn id")
        .to_string();
    let request_id = body["canonicalItem"]["metadata"]["requestId"]
        .as_str()
        .expect("accepted response should carry request id")
        .to_string();
    let user_message_id = body["canonicalItem"]["metadata"]["userMessageId"]
        .as_str()
        .expect("accepted response should carry user message id")
        .to_string();
    let initial_sidecar = state
        .session_store
        .runtime_sidecar(&SessionId::new(session_id.clone()))
        .expect("accepted turn should have a runtime sidecar");
    let initial_turn = initial_sidecar
        .current_turn
        .expect("accepted turn should be present before restart");
    assert_eq!(initial_turn.turn_id, turn_id);
    let initial_user_item = initial_turn
        .items
        .iter()
        .find(|item| item.item_id == user_message_id)
        .expect("accepted turn should contain the canonical user item");
    assert_eq!(
        initial_user_item.request_id.as_deref(),
        Some(request_id.as_str())
    );
    assert_eq!(
        initial_user_item.user_message_id.as_deref(),
        Some(user_message_id.as_str())
    );

    let event_root = state_root.join("session-events").join(&session_id);
    let event_path = fs::read_dir(&event_root)
        .expect("accepted canonical event directory should exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .expect("accepted canonical event transaction should exist");
    let event_payload: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(event_path).expect("accepted canonical event should be readable"),
    )
    .expect("accepted canonical event should be valid JSON");
    assert_eq!(
        event_payload["acceptance"]["session"]["canonicalTurn"]["turnId"], turn_id,
        "accepted facts must be stored in the same canonical event transaction"
    );
    assert!(
        event_payload["acceptance"]
            .to_string()
            .contains("request-no-subscriber-recovery"),
        "canonical event acceptance must be durable before any SSE subscriber is present"
    );
    assert!(
        !state_root.join("accepted-submissions.json").exists(),
        "new accepted submissions must not create the legacy journal"
    );

    drop(app);
    drop(runtime);

    let restarted_runtime =
        DaemonRuntime::restore(&config).expect("restart should recover persisted session state");
    let (restarted_app, restarted_state) =
        restarted_runtime.router_with_state_for_tests("daemon-test".to_string());
    let restarted_sidecar = restarted_state
        .session_store
        .runtime_sidecar(&SessionId::new(session_id.clone()))
        .expect("restart should recover the runtime sidecar");
    let restarted_turn = restarted_sidecar
        .current_turn
        .expect("restart should recover the current turn");
    assert_eq!(restarted_turn.turn_id, turn_id);
    let restarted_user_item = restarted_turn
        .items
        .iter()
        .find(|item| item.item_id == user_message_id)
        .expect("restart should recover the canonical user item");
    assert_eq!(
        restarted_user_item.request_id.as_deref(),
        Some(request_id.as_str())
    );
    assert_eq!(
        restarted_user_item.user_message_id.as_deref(),
        Some(user_message_id.as_str())
    );
    let bootstrap = get_json(
        restarted_app.clone(),
        &format!(
            "/bootstrap?scope=workspace&workspaceId=test-workspace-001&sessionId={session_id}"
        ),
    )
    .await;
    assert_eq!(bootstrap["currentSession"]["sessionId"], session_id);
    assert!(
        bootstrap["timeline"]
            .as_array()
            .expect("bootstrap timeline should be an array")
            .iter()
            .any(|entry| entry["message"] == "no subscriber persistence"),
        "bootstrap should recover the submitted message after restart"
    );

    let messages = get_json(
        restarted_app,
        &format!(
            "/api/messages?scope=workspace&workspaceId=test-workspace-001&sessionId={session_id}"
        ),
    )
    .await;
    assert!(
        messages["timeline"]
            .as_array()
            .expect("messages timeline should be an array")
            .iter()
            .any(|entry| entry["message"] == "no subscriber persistence"),
        "messages endpoint should recover the submitted message after restart"
    );
}

#[tokio::test]
async fn task_turn_replays_after_daemon_restart_without_duplicate_canonical_acceptance() {
    let state_root = temp_state_root("e2e-task-turn-restart-replay");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());
    let runtime = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("runtime restore should load explicit test fixture");
    let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());
    let request_id = "request-task-turn-restart-replay";
    let user_message_id = "user-task-turn-restart-replay";
    let request = json!({
        "scope": "workspace",
        "text": "执行任务并在 daemon 重启后验证同一 Turn 回放",
        "skillName": "code",
        "images": [],
        "workspaceId": DEFAULT_TEST_WORKSPACE_ID,
        "requestId": request_id,
        "userMessageId": user_message_id,
    });

    let (status, body) = post_json(app.clone(), "/api/session/turn", request.clone()).await;
    assert_eq!(status, StatusCode::OK, "task Turn should accept: {body:?}");
    let session_id = body["sessionId"]
        .as_str()
        .expect("task Turn should include session id")
        .to_string();
    let turn_id = body["canonicalTurn"]["turnId"]
        .as_str()
        .expect("task Turn should include canonical turn id")
        .to_string();
    let root_task_id = body["rootTaskId"]
        .as_str()
        .expect("task Turn should include root task id")
        .to_string();
    assert_eq!(
        body["canonicalItem"]["metadata"]["requestId"], request_id,
        "accepted task item must retain request identity"
    );

    drop(app);
    drop(state);
    drop(runtime);

    let restarted_runtime =
        DaemonRuntime::restore(&config).expect("daemon restart should recover task state");
    let (restarted_app, restarted_state) =
        restarted_runtime.router_with_state_for_tests("daemon-test".to_string());
    let restarted_session_id = SessionId::new(session_id.clone());
    let restarted_turn = restarted_state
        .session_store
        .canonical_turn_for_session_turn_id(&restarted_session_id, &turn_id)
        .expect("restart should recover the accepted task Turn");
    assert_eq!(restarted_turn.turn_id, turn_id);
    assert!(
        restarted_turn.items.iter().any(|item| {
            item.kind == magi_session_store::CanonicalTurnItemKind::UserMessage
                && item.item_id == user_message_id
                && item
                    .metadata
                    .get("requestId")
                    .and_then(serde_json::Value::as_str)
                    == Some(request_id)
                && item
                    .metadata
                    .get("userMessageId")
                    .and_then(serde_json::Value::as_str)
                    == Some(user_message_id)
        }),
        "restart must recover the canonical task user item and request identity"
    );

    let bootstrap = get_json(
        restarted_app.clone(),
        &format!(
            "/bootstrap?scope=workspace&workspaceId={DEFAULT_TEST_WORKSPACE_ID}&sessionId={session_id}"
        ),
    )
    .await;
    assert_eq!(bootstrap["currentSession"]["sessionId"], session_id);
    assert!(
        bootstrap["timeline"]
            .as_array()
            .expect("restarted task bootstrap timeline should be an array")
            .iter()
            .any(|entry| entry["message"] == "执行任务并在 daemon 重启后验证同一 Turn 回放"),
        "daemon restart bootstrap should replay the task user message"
    );

    let messages = get_json(
        restarted_app.clone(),
        &format!(
            "/api/messages?scope=workspace&workspaceId={DEFAULT_TEST_WORKSPACE_ID}&sessionId={session_id}"
        ),
    )
    .await;
    assert!(
        messages["timeline"]
            .as_array()
            .expect("restarted task messages timeline should be an array")
            .iter()
            .any(|entry| entry["message"] == "执行任务并在 daemon 重启后验证同一 Turn 回放"),
        "daemon restart messages should replay the task user message"
    );

    let (replay_status, replay_body) = post_json(restarted_app, "/api/session/turn", request).await;
    assert_eq!(
        replay_status,
        StatusCode::OK,
        "same task request should replay after daemon restart: {replay_body:?}"
    );
    assert_eq!(replay_body["sessionId"], session_id);
    assert_eq!(replay_body["canonicalTurn"]["turnId"], turn_id);
    assert_eq!(replay_body["rootTaskId"], root_task_id);
    assert_eq!(
        restarted_state
            .session_store
            .canonical_turns_for_session(&restarted_session_id)
            .iter()
            .filter(|turn| turn.turn_id == turn_id)
            .count(),
        1,
        "daemon restart replay must not append a duplicate canonical Turn"
    );
}

#[tokio::test]
async fn daemon_http_server_restart_replays_task_turn_without_duplicate_acceptance() {
    let state_root = temp_state_root("e2e-http-daemon-instance-restart");
    let workspace_root = temp_state_root("e2e-http-daemon-instance-restart-workspace");
    let config = DaemonConfig::new(
        "127.0.0.1",
        0,
        "daemon-http-restart-test",
        state_root.clone(),
    );
    let client = reqwest::Client::new();

    let daemon = Daemon::new(config.clone());
    let first = daemon
        .start()
        .await
        .expect("first daemon HTTP instance should start");
    let first_base = format!("http://{}", first.bound_addr());
    let register_response = client
        .post(format!("{first_base}/api/workspaces/register"))
        .json(&json!({ "path": workspace_root.to_string_lossy() }))
        .send()
        .await
        .expect("workspace registration request should reach first daemon");
    assert_eq!(register_response.status(), reqwest::StatusCode::OK);
    let workspace: Value = register_response
        .json()
        .await
        .expect("workspace registration response should be JSON");
    let workspace_id = workspace["workspaceId"]
        .as_str()
        .expect("workspace registration should return workspace id")
        .to_string();

    let request = json!({
        "scope": "workspace",
        "text": "执行任务并在 HTTP daemon 实例重启后回放同一 Turn",
        "skillName": "code",
        "images": [],
        "workspaceId": workspace_id,
        "requestId": "request-http-daemon-instance-restart",
        "userMessageId": "user-http-daemon-instance-restart",
    });
    let first_response = client
        .post(format!("{first_base}/api/session/turn"))
        .json(&request)
        .send()
        .await
        .expect("task request should reach first daemon");
    assert_eq!(first_response.status(), reqwest::StatusCode::OK);
    let first_body: Value = first_response
        .json()
        .await
        .expect("first accepted response should be JSON");
    let session_id = first_body["sessionId"]
        .as_str()
        .expect("accepted response should include session id")
        .to_string();
    let turn_id = first_body["canonicalTurn"]["turnId"]
        .as_str()
        .expect("accepted response should include canonical turn id")
        .to_string();
    let root_task_id = first_body["rootTaskId"]
        .as_str()
        .expect("accepted task response should include root task id")
        .to_string();

    first
        .shutdown("HTTP daemon instance restart probe")
        .expect("first daemon should accept shutdown");
    first
        .wait()
        .await
        .expect("first daemon should shut down cleanly");

    let restarted_daemon = Daemon::new(config);
    let second = restarted_daemon
        .start()
        .await
        .expect("second daemon HTTP instance should start from the same state root");
    let second_base = format!("http://{}", second.bound_addr());
    let bootstrap_response = client
        .get(format!(
            "{second_base}/bootstrap?scope=workspace&workspaceId={workspace_id}&sessionId={session_id}"
        ))
        .send()
        .await
        .expect("bootstrap should reach restarted daemon");
    assert_eq!(bootstrap_response.status(), reqwest::StatusCode::OK);
    let bootstrap: Value = bootstrap_response
        .json()
        .await
        .expect("restarted bootstrap should be JSON");
    assert_eq!(bootstrap["currentSession"]["sessionId"], session_id);
    assert!(
        bootstrap["timeline"]
            .as_array()
            .expect("restarted bootstrap timeline should be an array")
            .iter()
            .any(|entry| entry["message"] == "执行任务并在 HTTP daemon 实例重启后回放同一 Turn"),
        "restarted daemon must replay the accepted task user message"
    );

    let replay_response = client
        .post(format!("{second_base}/api/session/turn"))
        .json(&request)
        .send()
        .await
        .expect("duplicate task request should reach restarted daemon");
    assert_eq!(replay_response.status(), reqwest::StatusCode::OK);
    let replay: Value = replay_response
        .json()
        .await
        .expect("replayed response should be JSON");
    assert_eq!(replay["sessionId"], session_id);
    assert_eq!(replay["canonicalTurn"]["turnId"], turn_id);
    assert_eq!(replay["rootTaskId"], root_task_id);

    let messages_response = client
        .get(format!(
            "{second_base}/api/messages?scope=workspace&workspaceId={workspace_id}&sessionId={session_id}"
        ))
        .send()
        .await
        .expect("messages should reach restarted daemon");
    assert_eq!(messages_response.status(), reqwest::StatusCode::OK);
    let messages: Value = messages_response
        .json()
        .await
        .expect("restarted messages response should be JSON");
    let user_messages = messages["timeline"]
        .as_array()
        .expect("restarted messages timeline should be an array")
        .iter()
        .filter(|entry| entry["message"] == "执行任务并在 HTTP daemon 实例重启后回放同一 Turn")
        .count();
    assert_eq!(
        user_messages, 1,
        "HTTP daemon replay must not duplicate the user item"
    );

    second
        .shutdown("HTTP daemon instance restart probe complete")
        .expect("second daemon should accept shutdown");
    second
        .wait()
        .await
        .expect("second daemon should shut down cleanly");
}

#[tokio::test]
async fn workspace_sessions_and_events_stay_workspace_scoped() {
    let state_root = temp_state_root("e2e-workspace-session-isolation");
    let second_workspace_root = temp_state_root("e2e-workspace-session-isolation-second");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());
    let runtime = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("runtime restore should load explicit test fixture");
    let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());

    let (register_status, register_body) = post_json(
        app.clone(),
        "/api/workspaces/register",
        json!({ "path": second_workspace_root.to_string_lossy() }),
    )
    .await;
    assert_eq!(
        register_status,
        StatusCode::OK,
        "workspace register should succeed: {register_body:?}"
    );
    let second_workspace_id = register_body["workspaceId"]
        .as_str()
        .expect("second workspace id should serialize as string")
        .to_string();

    let (status, body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "scope": "workspace",
            "text": "workspace two isolated message",
            "workspaceId": second_workspace_id.clone(),
            "requestId": "request-workspace-two-isolated",
            "userMessageId": "user-workspace-two-isolated",
            "placeholderMessageId": "placeholder-workspace-two-isolated",
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "session turn should accept: {body:?}"
    );
    let session_id = body["sessionId"]
        .as_str()
        .expect("session id should serialize as string");

    let snapshot = state.event_bus.snapshot();
    let matching_events = snapshot
        .recent_events
        .iter()
        .filter(|event| event_payload_contains_request_id(event, "request-workspace-two-isolated"))
        .collect::<Vec<_>>();
    assert!(
        !matching_events.is_empty(),
        "event bus should contain workspace two events"
    );
    assert!(
        matching_events
            .iter()
            .all(|event| event.workspace_id.as_ref().map(WorkspaceId::as_str)
                == Some(second_workspace_id.as_str())),
        "workspace two request events must not be scoped to the bootstrap workspace"
    );

    let bootstrap_one = get_json(
        app.clone(),
        "/bootstrap?scope=workspace&workspaceId=test-workspace-001",
    )
    .await;
    assert!(
        !bootstrap_one["timeline"]
            .as_array()
            .expect("workspace one timeline should be an array")
            .iter()
            .any(|entry| entry["message"] == "workspace two isolated message"),
        "workspace one bootstrap must not include workspace two messages"
    );

    let bootstrap_two = get_json(
        app.clone(),
        &format!(
            "/bootstrap?scope=workspace&workspaceId={second_workspace_id}&sessionId={session_id}"
        ),
    )
    .await;
    assert_eq!(bootstrap_two["currentSession"]["sessionId"], session_id);
    assert!(
        bootstrap_two["timeline"]
            .as_array()
            .expect("workspace two timeline should be an array")
            .iter()
            .any(|entry| entry["message"] == "workspace two isolated message"),
        "workspace two bootstrap should include its own message"
    );

    let event_root = state_root.join("session-events").join(session_id);
    let event_payload = fs::read_dir(event_root)
        .expect("workspace accepted canonical event directory should exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .and_then(|path| fs::read_to_string(path).ok())
        .expect("workspace accepted canonical event transaction should be readable");
    assert!(
        event_payload.contains("request-workspace-two-isolated"),
        "workspace accepted facts must be in the canonical event transaction"
    );
    assert!(
        !state_root.join("accepted-submissions.json").exists(),
        "new workspace submissions must not create the legacy journal"
    );
}

#[tokio::test]
async fn restart_restores_last_selected_workspace_session() {
    let state_root = temp_state_root("e2e-last-selected-session-restart");
    let second_workspace_root = temp_state_root("e2e-last-selected-session-restart-second");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    let runtime = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("runtime restore should load explicit test fixture");
    let (app, _state) = runtime.router_with_state_for_tests("daemon-test".to_string());

    let (register_status, register_body) = post_json(
        app.clone(),
        "/api/workspaces/register",
        json!({ "path": second_workspace_root.to_string_lossy() }),
    )
    .await;
    assert_eq!(
        register_status,
        StatusCode::OK,
        "workspace register should succeed: {register_body:?}"
    );
    let second_workspace_id = register_body["workspaceId"]
        .as_str()
        .expect("second workspace id should serialize as string")
        .to_string();

    let (turn_status, turn_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "scope": "workspace",
            "text": "恢复最后打开的会话",
            "workspaceId": second_workspace_id.clone(),
            "requestId": "request-last-selected-session",
            "userMessageId": "user-last-selected-session",
            "placeholderMessageId": "placeholder-last-selected-session",
        }),
    )
    .await;
    assert_eq!(
        turn_status,
        StatusCode::OK,
        "session turn should accept: {turn_body:?}"
    );
    let selected_session_id = turn_body["sessionId"]
        .as_str()
        .expect("selected session id should serialize as string")
        .to_string();

    let (switch_status, switch_body) = post_json(
        app.clone(),
        "/api/session/navigation",
        json!({
            "target": "session",
            "scope": "workspace",
            "workspaceId": second_workspace_id,
            "sessionId": selected_session_id,
        }),
    )
    .await;
    assert_eq!(
        switch_status,
        StatusCode::OK,
        "session switch should persist selection: {switch_body:?}"
    );

    drop(app);
    drop(runtime);

    let restarted_runtime =
        DaemonRuntime::restore(&config).expect("restart should recover persisted session state");
    let (restarted_app, _restarted_state) =
        restarted_runtime.router_with_state_for_tests("daemon-test".to_string());
    let bootstrap = get_json(
        restarted_app,
        &format!("/bootstrap?scope=workspace&workspaceId={second_workspace_id}"),
    )
    .await;

    assert_eq!(
        bootstrap["currentSession"]["sessionId"],
        selected_session_id
    );
    assert!(
        bootstrap["sessions"]
            .as_array()
            .expect("sessions should be an array")
            .iter()
            .all(|session| session["workspaceId"] == switch_body["currentSession"]["workspaceId"]),
        "selected workspace bootstrap should only restore sessions from that workspace"
    );
}

#[tokio::test]
async fn orchestrator_settings_save_stays_global_when_session_scope_is_supplied() {
    let state_root = temp_state_root("e2e-orchestrator-global-settings");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());
    let runtime = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("runtime restore should load explicit test fixture");
    let (app, _state) = runtime.router_with_state_for_tests("daemon-test".to_string());

    let (status, body) = post_json(
        app,
        "/api/settings/orchestrator/save",
        json!({
            "config": {
                "baseUrl": "https://api.example.com/v1",
                "apiKey": "sk-real-test",
                "model": "global-main-model",
                "urlMode": "standard",
                "apiProtocol": "openai_chat",
                "sessionId": "session-scoped-should-not-save",
                "workspaceId": "test-workspace-001"
            }
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "orchestrator settings save should succeed: {body:?}"
    );

    let settings_payload =
        fs::read_to_string(state_root.join("settings.json")).expect("settings should persist");
    let settings_json: Value =
        serde_json::from_str(&settings_payload).expect("settings should be valid json");
    let orchestrator = settings_json
        .get("orchestrator")
        .expect("orchestrator section should persist");
    assert_eq!(
        orchestrator["baseUrl"], "https://api.example.com/v1",
        "global orchestrator settings should keep connection credentials"
    );
    assert!(
        orchestrator.get("model").is_none(),
        "main model selection is session-owned and must not persist in global settings"
    );
    assert!(
        !settings_payload.contains("__session__")
            && !settings_payload.contains("session-scoped-should-not-save"),
        "main model settings must remain global, not session-scoped"
    );
}

#[tokio::test]
async fn session_action_happy_path_creates_tasks_and_records_timeline_messages() {
    let state_root = temp_state_root("e2e-session-action-messages");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    let runtime = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("runtime restore should load explicit test fixture");
    let (app, _state) = runtime.router_with_state_for_tests("daemon-test".to_string());

    let (status, body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "scope": "workspace",
            "text": "修复集成测试消息链路问题，完成后运行测试并汇总验证结果",
            "skillName": "code",
            "images": [],
            "workspaceId": "test-workspace-001",
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
    assert!(
        !session_id.is_empty(),
        "session action should return a generated session id"
    );
    let accepted_at = body["acceptedAt"]
        .as_u64()
        .expect("accepted_at should serialize as integer");
    let mission_id = format!("mission-session-action-{accepted_at}");
    let root_task_id = body["rootTaskId"]
        .as_str()
        .expect("root_task_id should serialize as string");
    let projection = wait_for_agent_run_projection_completed(
        app.clone(),
        root_task_id,
        session_id,
        DEFAULT_TEST_WORKSPACE_ID,
    )
    .await;
    assert_completed_two_agent_run_projection(&projection);

    let messages_page = get_json(
        app.clone(),
        &format!("/api/messages?scope=workspace&workspaceId={DEFAULT_TEST_WORKSPACE_ID}&sessionId={session_id}"),
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
            .contains("修复集成测试消息链路问题"),
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

    let runtime = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("first runtime restore should load explicit test fixture");
    let (app, _state) = runtime.router_with_state_for_tests("daemon-test".to_string());

    let (status, body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "scope": "workspace",
            "text": "修复重启持久化验证问题，完成后运行测试并汇总验证结果",
            "skillName": "code",
            "images": [],
            "workspaceId": DEFAULT_TEST_WORKSPACE_ID,
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
        &format!("/api/messages?scope=workspace&workspaceId={DEFAULT_TEST_WORKSPACE_ID}&sessionId={session_id}"),
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
        &format!("/api/messages?scope=workspace&workspaceId={DEFAULT_TEST_WORKSPACE_ID}&sessionId={session_id}"),
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
                    thread_id: ThreadId::new("thread-stale-root-missing"),
                }],
                recovery_ref: None,
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: UtcMillis::now(),
                    entry_id: "timeline-stale-root-missing".to_string(),
                    trimmed_text: Some("stale root should detach".to_string()),
                    skill_name: None,
                },
                current_turn: None,
            },
        )
        .expect("active execution chain should persist to sidecar");
    repository
        .save_session_projection_state(
            &session_store.durable_state(),
            &session_store.execution_sidecar_store_state(),
        )
        .expect("session projection should save");
    repository
        .save_workspace_durable_state(&workspace_store.durable_state())
        .expect("workspace durable state should save");

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

    let (_, persisted_sidecars) = repository
        .load_session_projections(&[])
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

    let runtime = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("first runtime restore should load explicit test fixture");
    let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());
    // 这里直接建立可恢复的活动链，避免用即时完成的测试模型制造“刚接纳就已
    // 完成”的竞态。daemon 重启路径随后会真实处理 Running root 与当前 Turn。
    let session_id_text = "session-continue-restart-chain".to_string();
    let session_id = SessionId::new(session_id_text.clone());
    let workspace_id = WorkspaceId::new(DEFAULT_TEST_WORKSPACE_ID);
    let mission_id = MissionId::new("mission-session-continue-restart-chain");
    let root_task_id = TaskId::new("task-session-continue-restart-chain");
    let root_worker_id = WorkerId::new("worker-session-continue-restart-chain");
    let execution_chain_ref = "chain-session-continue-restart-chain".to_string();
    let task_store = state.task_store().expect("task store should be configured");
    let now = UtcMillis::now();
    state
        .session_store
        .create_session_for_workspace(
            session_id.clone(),
            "重启继续执行链路验收",
            Some(workspace_id.to_string()),
        )
        .expect("restart chain session should create");
    let (_, orchestrator_thread_id) =
        state
            .session_store
            .ensure_session_mission(&session_id, now, || mission_id.clone());
    task_store
        .insert_task(Task {
            task_id: root_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: "重启继续执行 root".to_string(),
            goal: "验证 daemon 重启后可继续执行链".to_string(),
            status: TaskStatus::Running,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: Some(workspace_id.to_string()),
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        })
        .expect("restart chain root task should insert");
    let request_id = "request-session-continue-restart-chain";
    let user_message_id = "user-session-continue-restart-chain";
    let user_item = ActiveExecutionTurnItem {
        item_id: user_message_id.to_string(),
        item_seq: 1,
        kind: "user_message".to_string(),
        status: "completed".to_string(),
        source: "orchestrator".to_string(),
        title: None,
        content: Some("修复重启继续执行链路问题，完成后运行测试并汇总验证结果".to_string()),
        task_id: None,
        worker_id: None,
        role_id: None,
        tool_call_id: None,
        tool_name: None,
        tool_status: None,
        tool_arguments: None,
        tool_result: None,
        tool_error: None,
        request_id: Some(request_id.to_string()),
        user_message_id: Some(user_message_id.to_string()),
        placeholder_message_id: None,
        metadata: std::collections::HashMap::from([
            ("route".to_string(), json!("execute")),
            ("executionProfile".to_string(), json!("task")),
            ("requestId".to_string(), json!(request_id)),
            (
                "requestFingerprint".to_string(),
                json!("fingerprint-session-continue-restart-chain"),
            ),
            ("userMessageId".to_string(), json!(user_message_id)),
        ]),
        timeline_entry_id: Some("timeline-session-continue-restart-chain".to_string()),
        source_thread_id: orchestrator_thread_id,
    };
    let primary_branch = ActiveExecutionBranch {
        task_id: root_task_id.clone(),
        worker_id: root_worker_id.clone(),
        stage: "execute".to_string(),
        lease_id: None,
        execution_intent_ref: Some("worker-intent-session-continue-restart-chain".to_string()),
        binding_lifecycle: Some("requested".to_string()),
        checkpoint_stage: Some("execute".to_string()),
        next_step_index: Some(1),
        checkpoint_at: Some(now),
        resume_mode: Some("step-checkpoint".to_string()),
        resume_token: None,
        use_tools: true,
        skill_name: Some("refactor".to_string()),
        is_primary: true,
        thread_id: ThreadId::new("thread-session-continue-restart-chain"),
    };
    state
        .session_store
        .register_thread(ExecutionThread {
            thread_id: primary_branch.thread_id.clone(),
            session_id: session_id.clone(),
            mission_id: mission_id.clone(),
            role_id: "executor".to_string(),
            worker_instance_id: primary_branch.worker_id.clone(),
            status: ExecutionThreadStatus::Active,
            created_at: now,
            last_used_at: now,
            observed_context_window_tokens: None,
            handled_task_ids: vec![root_task_id.clone()],
            message_history: Vec::new(),
        })
        .expect("restart chain root thread should register");
    let mut chain = ActiveExecutionChain {
        session_id: session_id.clone(),
        mission_id: mission_id.clone(),
        root_task_id: root_task_id.clone(),
        execution_chain_ref: execution_chain_ref.clone(),
        workspace_id: Some(workspace_id.clone()),
        active_branch_task_ids: vec![root_task_id.clone()],
        active_worker_bindings: vec![root_worker_id.clone()],
        branches: vec![primary_branch.clone()],
        recovery_ref: None,
        dispatch_context: ActiveExecutionDispatchContext {
            accepted_at: now,
            entry_id: "timeline-session-continue-restart-chain".to_string(),
            trimmed_text: Some(
                "修复重启继续执行链路问题，完成后运行测试并汇总验证结果".to_string(),
            ),
            skill_name: Some("refactor".to_string()),
        },
        current_turn: Some(ActiveExecutionTurn {
            turn_id: "turn-session-continue-restart-chain".to_string(),
            turn_seq: now.0,
            accepted_at: now,
            completed_at: None,
            status: "running".to_string(),
            user_message: Some(
                "修复重启继续执行链路问题，完成后运行测试并汇总验证结果".to_string(),
            ),
            items: vec![user_item],
        }),
    };
    state
        .session_store
        .accept_active_execution_chain_with_timeline_entry(
            session_id.clone(),
            magi_session_store::TimelineEntryInput::new(
                "timeline-session-continue-restart-chain",
                magi_session_store::TimelineEntryKind::UserMessage,
                "修复重启继续执行链路问题，完成后运行测试并汇总验证结果",
                now,
            ),
            chain.clone(),
        )
        .expect("restart chain should accept canonical turn");
    let admission = state
        .turn_coordinator()
        .execute_command(
            &session_id,
            magi_conversation_runtime::TurnCommand::Start(
                magi_conversation_runtime::TurnAdmission {
                    turn_id: "turn-session-continue-restart-chain".to_string(),
                    request_id: request_id.to_string(),
                    request_fingerprint: "fingerprint-session-continue-restart-chain".to_string(),
                    profile: magi_conversation_runtime::ExecutionProfile::Task,
                },
            ),
        )
        .expect("restart chain coordinator admission should succeed");
    assert!(matches!(
        admission,
        magi_conversation_runtime::CoordinatorCommandResult::Admission(
            magi_conversation_runtime::CoordinatorAdmission::Accepted(_)
        )
    ));
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
        task_store
            .insert_task(Task {
                task_id: TaskId::new(task_id),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                parent_task_id: Some(primary_branch.task_id.clone()),
                kind: TaskKind::LocalAgent,
                title: format!("restart branch {task_id}"),
                goal: format!("resume branch {task_id}"),
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
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                runtime_payload: magi_core::TaskRuntimePayload::default(),
                created_at: now,
                updated_at: now,
            })
            .expect("分支任务应插入");
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
            thread_id: ThreadId::new(format!("thread-{task_id}")),
        });
    }
    task_store
        .insert_task(Task {
            task_id: TaskId::new("task-restart-branch-completed"),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: Some(primary_branch.task_id.clone()),
            kind: TaskKind::LocalAgent,
            title: "restart branch completed".to_string(),
            goal: "completed branch should stay terminal".to_string(),
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
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        })
        .expect("已完成分支任务应插入");
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
        thread_id: ThreadId::new("thread-restart-branch-completed"),
    });
    let registered_thread_ids = state
        .session_store
        .thread_registry_snapshot(&session_id)
        .into_iter()
        .map(|thread| thread.thread_id)
        .collect::<std::collections::HashSet<_>>();
    for branch in &chain.branches {
        if registered_thread_ids.contains(&branch.thread_id) {
            continue;
        }
        state
            .session_store
            .register_thread(ExecutionThread {
                thread_id: branch.thread_id.clone(),
                session_id: session_id.clone(),
                mission_id: mission_id.clone(),
                role_id: "coordinator".to_string(),
                worker_instance_id: branch.worker_id.clone(),
                status: ExecutionThreadStatus::Active,
                created_at: now,
                last_used_at: now,
                observed_context_window_tokens: None,
                handled_task_ids: vec![branch.task_id.clone()],
                message_history: Vec::new(),
            })
            .expect("恢复 branch thread 测试数据应注册成功");
    }
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
                    TaskStatus::Failed | TaskStatus::Pending | TaskStatus::Running
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
            WorkerBranchCheckpointState {
                lease_id: branch.lease_id.as_ref().map(ToString::to_string),
                execution_intent_ref: branch.execution_intent_ref.clone(),
                binding_lifecycle: parse_binding_lifecycle(branch.binding_lifecycle.as_deref()),
                checkpoint_cursor: branch.checkpoint_stage.as_deref().map(|checkpoint_stage| {
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
            },
        );
    }

    fs::create_dir_all(&state_root).expect("state root should exist before task checkpoint");
    task_store
        .checkpoint_to_projection_directory(&state_root.join("task-store-projections"))
        .expect("task store checkpoint should persist");
    let flush_report = RuntimeSidecarPersistence::new(
        repository.clone(),
        state.session_store.clone(),
        state.workspace_registry.clone(),
        worker_runtime.clone(),
    )
    .flush_runtime_sidecars()
    .expect("runtime sidecars should flush");
    // Task completion 的主动通知可能已经在本次显式 flush 前完成同一 session
    // 的 checkpoint；此时 sidecar 已经是 clean，返回 false 只表示没有新增脏版本。
    // 后续 restart 断言会验证这份最新 chain 确实已落盘。
    let _ = flush_report.session_sidecars_flushed;
    assert!(flush_report.worker_runtime_snapshot_flushed);

    drop(app);
    drop(state);
    drop(runtime);

    let restarted_runtime = DaemonRuntime::restore(&config)
        .expect("second runtime restore should recover persisted execution chain");
    let (restarted_app, restarted_state) =
        restarted_runtime.router_with_state_for_tests("daemon-test".to_string());

    let interrupted_turn = restarted_state
        .session_store
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .expect("restart should retain the interrupted current turn");
    assert_eq!(
        interrupted_turn.status, "interrupted",
        "daemon restart must settle the previous UI turn instead of leaving it running"
    );
    assert!(
        interrupted_turn.items.iter().any(|item| {
            item.metadata.get("noticeKind").and_then(Value::as_str) == Some("session_interrupted")
                && item.metadata.get("recoveryState").and_then(Value::as_str) == Some("ready")
        }),
        "restart should append a visible recovery notice"
    );

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
            .filter(|entry| entry["status"] == "failed")
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
            "workspaceId": DEFAULT_TEST_WORKSPACE_ID,
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
    let resumed_turn = restarted_state
        .session_store
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .expect("continue should create a new running turn after interruption");
    assert_eq!(resumed_turn.status, "running");
    assert!(
        restarted_state
            .session_store
            .canonical_turns_for_session(&session_id)
            .iter()
            .any(|turn| {
                turn.status == magi_session_store::CanonicalTurnStatus::Interrupted
                    && turn.items.iter().any(|item| {
                        item.metadata.get("noticeKind").and_then(Value::as_str)
                            == Some("session_interrupted")
                            && item.metadata.get("recoveryState").and_then(Value::as_str)
                                == Some("claimed")
                    })
            }),
        "successful continue must consume the old recovery link"
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

    let task_store_projection_dir = state_root.join("task-store-projections");
    let restored_task_store =
        TaskStore::restore_from_projection_directory(&task_store_projection_dir)
            .expect("task store projection directory should remain readable")
            .expect("task store projection should have a committed manifest");
    let mission_ids = restored_task_store
        .all_tasks()
        .iter()
        .map(|task| task.mission_id.to_string())
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
async fn workspace_bound_session_continue_survives_runtime_restart() {
    let state_root = temp_state_root("e2e-workspace-bound-session-continue-restart");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());
    let repository = StateRepository::new(state_root.clone());

    let runtime = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("first runtime restore should load explicit test fixture");
    let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());

    // 不预先创建 session：直接发 turn，让 dispatch 在显式 workspace 下创建会话。
    let (action_status, action_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "scope": "workspace",
            "text": "修复工作区绑定会话重启恢复问题，完成后运行测试并汇总验证结果",
            "skillName": "refactor",
            "images": [],
            "workspaceId": DEFAULT_TEST_WORKSPACE_ID,
        }),
    )
    .await;
    assert_eq!(
        action_status,
        StatusCode::OK,
        "session action on workspace-bound session should succeed: {action_body:?}"
    );
    let session_id_text = action_body["sessionId"]
        .as_str()
        .expect("sessionId should serialize as string")
        .to_string();
    let session_id = SessionId::new(session_id_text.clone());

    let chain = state
        .session_store
        .active_execution_chain(&session_id)
        .expect("dispatch should create active execution chain for workspace-bound session");
    let mission_id = chain.mission_id.clone();
    let root_task_id = chain.root_task_id.clone();
    let execution_chain_ref = chain.execution_chain_ref.clone();
    let task_store = state.task_store().expect("task store should be configured");

    task_store
        .revoke_lease_and_set_task_terminal(
            &root_task_id,
            &root_task_id,
            None,
            TaskStatus::Failed,
            Vec::new(),
        )
        .expect("root task should become blocked")
        .then_some(())
        .expect("root task should become blocked");
    for branch in &chain.branches {
        let branch_status = task_store
            .get_task(&branch.task_id)
            .expect("branch task should remain in task store")
            .status;
        if branch_status != TaskStatus::Failed {
            task_store
                .revoke_lease_and_set_task_terminal(
                    &branch.task_id,
                    &root_task_id,
                    None,
                    TaskStatus::Failed,
                    Vec::new(),
                )
                .expect("branch task should become failed")
                .then_some(())
                .expect("branch task should become failed");
        }
        assert_eq!(
            task_store
                .get_task(&branch.task_id)
                .expect("branch task should remain after terminalization")
                .status,
            TaskStatus::Failed,
            "workspace-bound continue fixture must expose failed branches"
        );
    }

    state
        .persist_session_projection()
        .expect("workspace-bound session projection should persist");
    task_store
        .checkpoint_to_projection_directory(&state_root.join("task-store-projections"))
        .expect("task store checkpoint should persist");
    let worker_runtime = state
        .execution_pipeline()
        .expect("execution pipeline should exist")
        .execution_runtime
        .worker_runtime()
        .clone();
    let _flush_report = RuntimeSidecarPersistence::new(
        repository.clone(),
        state.session_store.clone(),
        state.workspace_registry.clone(),
        worker_runtime,
    )
    .flush_runtime_sidecars()
    .expect("runtime sidecars should flush");
    let workspace_roots = repository
        .workspace_projection_roots()
        .expect("workspace projection roots should load");
    let workspace_roots = workspace_roots.into_iter().collect::<Vec<_>>();
    let (persisted_durable, persisted_sidecars) = repository
        .load_session_projections(&workspace_roots)
        .expect("session sidecars should be persisted by checkpoint or explicit flush");
    assert!(
        persisted_sidecars
            .runtime_sidecars
            .iter()
            .any(|sidecar| sidecar.session_id == session_id),
        "session sidecar must be persisted even when checkpoint already flushed before explicit flush"
    );

    let _ = persisted_sidecars;
    assert!(
        persisted_durable.sessions.iter().any(|session| {
            session.session_id == session_id
                && session.workspace_id.as_deref() == Some(DEFAULT_TEST_WORKSPACE_ID)
        }),
        "workspace-bound session must persist in its workspace projection"
    );
    drop(app);
    drop(state);
    drop(runtime);

    let restarted_runtime = DaemonRuntime::restore(&config)
        .expect("restart should recover workspace-bound session state");
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
        .expect("restarted runtime should still export workspace-bound session summary");
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
            "workspaceId": DEFAULT_TEST_WORKSPACE_ID,
        }),
    )
    .await;
    assert_eq!(
        continue_status,
        StatusCode::OK,
        "restarted workspace-bound session continue should succeed: {continue_body:?}"
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
    let runtime = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("runtime restore should load explicit test fixture");
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
            "scope": "workspace",
            "sessionId": "session-e2e-events",
            "text": "修复事件总线任务事件发布问题，完成后运行测试并汇总验证结果",
            "skillName": "code",
            "images": [],
            "workspaceId": active_workspace_id.to_string(),
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
    let runtime = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("runtime restore should load explicit test fixture");
    let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());

    let (status, first_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "scope": "workspace",
            "text": "修复连续会话第一阶段问题，完成后运行测试并汇总验证结果",
            "skillName": "refactor",
            "images": [],
            "workspaceId": DEFAULT_TEST_WORKSPACE_ID,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let session_id = first_body["sessionId"].as_str().unwrap().to_string();
    let first_root_task_id = first_body["rootTaskId"]
        .as_str()
        .expect("first root task id should serialize as string")
        .to_string();
    let first_projection = wait_for_agent_run_projection_completed(
        app.clone(),
        &first_root_task_id,
        &session_id,
        DEFAULT_TEST_WORKSPACE_ID,
    )
    .await;
    assert_completed_two_agent_run_projection(&first_projection);

    let second_request_id = "request-sequential-session-actions-followup";
    let (status, second_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "scope": "workspace",
            "sessionId": session_id,
            "text": "修复连续会话第二阶段问题，完成后运行测试并汇总验证结果",
            "skillName": "refactor",
            "images": [],
            "workspaceId": DEFAULT_TEST_WORKSPACE_ID,
            "requestId": second_request_id,
            "userMessageId": "user-sequential-session-actions-followup",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!second_body["createdSession"].as_bool().unwrap_or(true));
    let second_root_task_id = if let Some(root_task_id) = second_body["rootTaskId"].as_str() {
        root_task_id.to_string()
    } else {
        assert_eq!(
            second_body["queued"], true,
            "忙碌 session 的 followup 应进入队列: {second_body}"
        );
        let deadline = Instant::now() + BACKGROUND_TASK_PROJECTION_TIMEOUT;
        loop {
            if let Some(turn) = state
                .session_store
                .canonical_turn_for_request_id(second_request_id)
                && let Some(task_id) = turn
                    .items
                    .iter()
                    .find(|item| {
                        item.kind == magi_session_store::CanonicalTurnItemKind::UserMessage
                    })
                    .and_then(|item| item.worker.as_ref())
                    .and_then(|worker| worker.task_id.as_ref())
            {
                break task_id.to_string();
            }
            if Instant::now() >= deadline {
                panic!("queued followup was not accepted into canonical Turn before timeout");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let second_projection = wait_for_agent_run_projection_completed(
        app.clone(),
        &second_root_task_id,
        &session_id,
        DEFAULT_TEST_WORKSPACE_ID,
    )
    .await;
    assert_completed_two_agent_run_projection(&second_projection);

    let messages_page = get_json(
        app.clone(),
        &format!("/api/messages?scope=workspace&workspaceId={DEFAULT_TEST_WORKSPACE_ID}&sessionId={session_id}"),
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
    let _second_accepted_at = second_body["acceptedAt"].as_u64().unwrap();
    // session 一生一 mission：两次派发聚合到同一 execution_group。
    let session_mission_id = format!("mission-session-action-{first_accepted_at}");

    let bootstrap = get_json(
        app,
        "/bootstrap?scope=workspace&workspaceId=test-workspace-001",
    )
    .await;
    let execution_groups = bootstrap["runtimeReadModel"]["details"]["execution_groups"]
        .as_array()
        .expect("execution groups should be an array");
    assert_eq!(
        execution_groups.len(),
        1,
        "两次派发必须聚合到同一 execution_group"
    );
    assert!(
        execution_groups
            .iter()
            .any(|m| m["mission_id"] == session_mission_id),
        "bootstrap should contain session-level execution group"
    );
}

#[tokio::test]
async fn daemon_handle_starts_serves_and_shuts_down_idempotently() {
    let state_root = temp_state_root("daemon-handle-lifecycle");
    let daemon = Daemon::new(
        DaemonConfig::new("127.0.0.1", 0, "daemon-handle-test", state_root).with_identity(
            "3.0.51",
            "build-test-1",
            "nonce-test-1",
        ),
    );

    let handle = daemon.start().await.expect("daemon should start");
    assert_ne!(handle.bound_addr().port(), 0);
    let health_url = handle.web_url().replace("/web.html", "/health");
    let response = reqwest::get(&health_url)
        .await
        .expect("health endpoint should be reachable");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let health: serde_json::Value = response
        .json()
        .await
        .expect("health response should be json");
    assert_eq!(health["serviceName"], "daemon-handle-test");
    assert_eq!(health["productVersion"], "3.0.51");
    assert_eq!(health["buildIdentity"], "build-test-1");
    assert_eq!(health["startupNonce"], "nonce-test-1");

    let version_url = handle.web_url().replace("/web.html", "/version");
    let version: serde_json::Value = reqwest::get(version_url)
        .await
        .expect("version endpoint should be reachable")
        .json()
        .await
        .expect("version response should be json");
    assert_eq!(version["serviceName"], "daemon-handle-test");
    assert_eq!(version["productVersion"], "3.0.51");
    assert_eq!(version["buildIdentity"], "build-test-1");
    assert_eq!(version["startupNonce"], "nonce-test-1");

    handle
        .shutdown("desktop exit")
        .expect("first shutdown request should succeed");
    handle
        .shutdown("duplicate desktop exit")
        .expect("duplicate shutdown request should be idempotent");
    handle.wait().await.expect("daemon should stop cleanly");

    assert!(
        reqwest::get(&health_url).await.is_err(),
        "daemon port should be released after shutdown"
    );
}

#[tokio::test]
async fn daemon_handle_force_shutdown_releases_the_listening_port() {
    let state_root = temp_state_root("daemon-handle-force-shutdown");
    let daemon = Daemon::new(DaemonConfig::new(
        "127.0.0.1",
        0,
        "daemon-handle-force-shutdown-test",
        state_root,
    ));

    let mut handle = daemon.start().await.expect("daemon should start");
    let health_url = handle.web_url().replace("/web.html", "/health");
    assert_eq!(
        reqwest::get(&health_url)
            .await
            .expect("health endpoint should be reachable")
            .status(),
        reqwest::StatusCode::OK
    );

    handle
        .force_shutdown("desktop runtime recovery timeout")
        .expect("forced shutdown should record runtime interruption");
    handle
        .wait_until_stopped()
        .await
        .expect_err("aborted daemon task should report forced termination");

    assert!(
        reqwest::get(&health_url).await.is_err(),
        "daemon port should be released after forced shutdown"
    );
}
