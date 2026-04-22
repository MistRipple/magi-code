use super::*;
use crate::models::{SessionExecutionSidecarStatus, SessionSidecarFlushReason, SessionStoreState};
use magi_core::{
    ExecutionOwnership, MissionId, RecoveryResumeInput, SessionId, TaskExecutionTarget, TaskId,
    UtcMillis, WorkerId, WorkspaceId,
};
use serde_json::json;

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
    assert_eq!(sidecar.status, SessionExecutionSidecarStatus::RecoveryLinked);

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
    assert_eq!(export.execution_chain_ref.as_deref(), Some("chain-persisted"));
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
    assert_eq!(recovery_metadata.next_flush_hint, recovery_metadata.last_dirty_at);

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
    assert_eq!(resume_metadata.next_flush_hint, resume_metadata.last_dirty_at);

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
    assert_eq!(sidecar.status, SessionExecutionSidecarStatus::RecoveryLinked);
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
    assert_eq!(resumed.ownership.mission_id, Some(MissionId::new("mission-full")));
    assert_eq!(resumed.ownership.task_id, Some(TaskId::new("todo-full")));
    assert_eq!(resumed.ownership.worker_id, Some(WorkerId::new("worker-full")));
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
    assert_eq!(export.ownership.mission_id, Some(MissionId::new("mission-full")));

    let active = store.active_execution_sidecars();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].session_id, session_id);
    assert_eq!(active[0].status, SessionExecutionSidecarStatus::Resumed);
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
    assert_eq!(export_b.current_status, SessionExecutionSidecarStatus::Bound);

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
