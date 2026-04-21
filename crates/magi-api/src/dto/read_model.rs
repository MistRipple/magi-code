use magi_event_bus::{
    AuditUsageLedgerStatus, RecoveryActivityStage, RecoveryDiagnosticSummaryEntry,
    RuntimeLedgerSummary, RuntimeReadModelInput, SessionRuntimeSummaryEntry,
    WorkspaceRuntimeSummaryEntry,
};
use magi_session_store::{SessionExecutionSidecarStatus, SessionRuntimeSidecarExport};
use magi_workspace::{RecoveryStatus, WorkspaceRecoverySidecarExport};

pub type RuntimeReadModelDto = RuntimeReadModelInput;
pub type AuditUsageLedgerDto = RuntimeLedgerSummary;

pub fn runtime_read_model_dto(
    runtime_read_model: RuntimeReadModelInput,
    session_sidecar_exports: &[SessionRuntimeSidecarExport],
    workspace_sidecar_exports: &[WorkspaceRecoverySidecarExport],
    audit_usage_ledger: AuditUsageLedgerDto,
) -> RuntimeReadModelDto {
    let mut runtime_read_model = runtime_read_model;
    merge_session_sidecars(&mut runtime_read_model, session_sidecar_exports);
    merge_workspace_sidecars(&mut runtime_read_model, workspace_sidecar_exports);
    runtime_read_model.meta.ledger = audit_usage_ledger;
    runtime_read_model
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn ledger_dto(status: AuditUsageLedgerStatus) -> AuditUsageLedgerDto {
    RuntimeLedgerSummary::from(status)
}

fn merge_session_sidecars(
    runtime_read_model: &mut RuntimeReadModelInput,
    session_sidecar_exports: &[SessionRuntimeSidecarExport],
) {
    for export in session_sidecar_exports {
        let session_id = export.session_id.to_string();
        let entry = runtime_read_model
            .details
            .sessions
            .iter_mut()
            .find(|entry| entry.session_id == session_id);
        let entry = match entry {
            Some(entry) => entry,
            None => {
                runtime_read_model
                    .details
                    .sessions
                    .push(SessionRuntimeSummaryEntry {
                        session_id: session_id.clone(),
                        ..SessionRuntimeSummaryEntry::default()
                    });
                runtime_read_model
                    .details
                    .sessions
                    .last_mut()
                    .expect("session entry inserted above")
            }
        };

        entry.current_status = Some(match export.current_status {
            SessionExecutionSidecarStatus::Detached => "detached".to_string(),
            SessionExecutionSidecarStatus::Bound => "bound".to_string(),
            SessionExecutionSidecarStatus::RecoveryLinked => "recovery_linked".to_string(),
            SessionExecutionSidecarStatus::Resumed => "resumed".to_string(),
        });
        entry.last_update = Some(export.last_update);
        entry.execution_chain_ref = export.execution_chain_ref.clone();
        entry.recovery_ref = export.recovery_ref.clone();
        push_unique(
            &mut entry.active_mission_ids,
            export.ownership.mission_id.as_ref().map(ToString::to_string),
        );
        push_unique(
            &mut entry.active_task_ids,
            export.ownership.task_id.as_ref().map(ToString::to_string),
        );
        push_unique(&mut entry.recovery_ids, export.recovery_ref.clone());
    }

    runtime_read_model
        .details
        .sessions
        .sort_by(|left, right| left.session_id.cmp(&right.session_id));
    for entry in &mut runtime_read_model.details.sessions {
        entry.active_mission_ids.sort();
        entry.active_mission_ids.dedup();
        entry.active_task_ids.sort();
        entry.active_task_ids.dedup();
        entry.recovery_ids.sort();
        entry.recovery_ids.dedup();
    }
}

fn merge_workspace_sidecars(
    runtime_read_model: &mut RuntimeReadModelInput,
    workspace_sidecar_exports: &[WorkspaceRecoverySidecarExport],
) {
    for export in workspace_sidecar_exports {
        let workspace_id = export.workspace_id.to_string();
        let entry = runtime_read_model
            .details
            .workspaces
            .iter_mut()
            .find(|entry| entry.workspace_id == workspace_id);
        let entry = match entry {
            Some(entry) => entry,
            None => {
                runtime_read_model
                    .details
                    .workspaces
                    .push(WorkspaceRuntimeSummaryEntry {
                        workspace_id: workspace_id.clone(),
                        ..WorkspaceRuntimeSummaryEntry::default()
                    });
                runtime_read_model
                    .details
                    .workspaces
                    .last_mut()
                    .expect("workspace entry inserted above")
            }
        };

        entry.current_status = Some(match export.current_status {
            RecoveryStatus::Prepared => "prepared".to_string(),
            RecoveryStatus::Ready => "ready".to_string(),
            RecoveryStatus::Consumed => "consumed".to_string(),
        });
        entry.last_update = Some(export.last_update);
        entry.execution_chain_ref = export.execution_chain_ref.clone();
        entry.recovery_ref = Some(export.recovery_ref.clone());
        push_unique(
            &mut entry.active_mission_ids,
            export.ownership.mission_id.as_ref().map(ToString::to_string),
        );
        push_unique(
            &mut entry.active_task_ids,
            export.ownership.task_id.as_ref().map(ToString::to_string),
        );
        push_unique(&mut entry.recovery_ids, Some(export.recovery_ref.clone()));
        push_unique(
            &mut entry.execution_chain_refs,
            export.execution_chain_ref.clone(),
        );

        let summary = runtime_read_model
            .recovery
            .summaries
            .iter_mut()
            .find(|summary| summary.recovery_id == export.recovery_ref);
        let summary = match summary {
            Some(summary) => summary,
            None => {
                runtime_read_model
                    .recovery
                    .summaries
                    .push(RecoveryDiagnosticSummaryEntry {
                        recovery_id: export.recovery_ref.clone(),
                        event_count: 0,
                        latest_stage: recovery_stage_from_status(&export.current_status),
                        latest_event_type: "workspace.recovery.sidecar".to_string(),
                        latest_sequence: 0,
                        latest_occurred_at: export.last_update,
                        workspace_id: Some(export.workspace_id.clone()),
                        session_id: export.ownership.session_id.clone(),
                        mission_id: export.ownership.mission_id.clone(),
                        assignment_id: None,
                        task_id: export.ownership.task_id.clone(),
                        worker_id: export.ownership.worker_id.as_ref().map(ToString::to_string),
                        execution_chain_ref: export.execution_chain_ref.clone(),
                        diagnostic_summary: export.diagnostic_summary.clone(),
                        current_status: entry.current_status.clone().unwrap_or_default(),
                    });
                runtime_read_model
                    .recovery
                    .summaries
                    .last_mut()
                    .expect("recovery summary inserted above")
            }
        };
        if summary.workspace_id.is_none() {
            summary.workspace_id = Some(export.workspace_id.clone());
        }
        if summary.session_id.is_none() {
            summary.session_id = export.ownership.session_id.clone();
        }
        if summary.mission_id.is_none() {
            summary.mission_id = export.ownership.mission_id.clone();
        }
        if summary.task_id.is_none() {
            summary.task_id = export.ownership.task_id.clone();
        }
        if summary.worker_id.is_none() {
            summary.worker_id = export.ownership.worker_id.as_ref().map(ToString::to_string);
        }
        if summary.execution_chain_ref.is_none() {
            summary.execution_chain_ref = export.execution_chain_ref.clone();
        }
        if summary.diagnostic_summary.is_none() {
            summary.diagnostic_summary = export.diagnostic_summary.clone();
        }
        if summary.event_count == 0 {
            summary.latest_occurred_at = export.last_update;
            summary.current_status = entry.current_status.clone().unwrap_or_default();
        }
        if !matches!(export.current_status, RecoveryStatus::Consumed) {
            push_unique(
                &mut runtime_read_model.recovery.active_recovery_ids,
                Some(export.recovery_ref.clone()),
            );
        }
    }

    runtime_read_model
        .details
        .workspaces
        .sort_by(|left, right| left.workspace_id.cmp(&right.workspace_id));
    runtime_read_model
        .recovery
        .summaries
        .sort_by(|left, right| left.recovery_id.cmp(&right.recovery_id));
    runtime_read_model.recovery.active_recovery_ids.sort();
    runtime_read_model.recovery.active_recovery_ids.dedup();
    for entry in &mut runtime_read_model.details.workspaces {
        entry.active_mission_ids.sort();
        entry.active_mission_ids.dedup();
        entry.active_task_ids.sort();
        entry.active_task_ids.dedup();
        entry.recovery_ids.sort();
        entry.recovery_ids.dedup();
        entry.execution_chain_refs.sort();
        entry.execution_chain_refs.dedup();
    }
}

fn recovery_stage_from_status(status: &RecoveryStatus) -> RecoveryActivityStage {
    match status {
        RecoveryStatus::Prepared => RecoveryActivityStage::ResumeCommandCreated,
        RecoveryStatus::Ready => RecoveryActivityStage::ResumeDispatchCreated,
        RecoveryStatus::Consumed => RecoveryActivityStage::WorkerResumed,
    }
}

fn push_unique(values: &mut Vec<String>, value: Option<String>) {
    let Some(value) = value else {
        return;
    };
    if !values.contains(&value) {
        values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{ExecutionOwnership, SessionId, UtcMillis, WorkspaceId};

    #[test]
    fn runtime_read_model_merges_sidecars_and_ledger_summary() {
        let runtime_read_model = runtime_read_model_dto(
            RuntimeReadModelInput::default(),
            &[SessionRuntimeSidecarExport {
                session_id: SessionId::new("session-1"),
                current_status: SessionExecutionSidecarStatus::Resumed,
                last_update: UtcMillis::now(),
                ownership: ExecutionOwnership {
                    mission_id: Some(magi_core::MissionId::new("mission-1")),
                    task_id: Some(magi_core::TaskId::new("todo-1")),
                    ..ExecutionOwnership::default()
                },
                execution_chain_ref: Some("chain-1".to_string()),
                recovery_ref: Some("recovery-1".to_string()),
            }],
            &[WorkspaceRecoverySidecarExport {
                recovery_ref: "recovery-1".to_string(),
                workspace_id: WorkspaceId::new("workspace-1"),
                current_status: RecoveryStatus::Ready,
                last_update: UtcMillis::now(),
                ownership: ExecutionOwnership {
                    session_id: Some(SessionId::new("session-1")),
                    mission_id: Some(magi_core::MissionId::new("mission-1")),
                    task_id: Some(magi_core::TaskId::new("todo-1")),
                    ..ExecutionOwnership::default()
                },
                execution_chain_ref: Some("chain-1".to_string()),
                snapshot_id: "snapshot-1".to_string(),
                diagnostic_summary: Some("resume".to_string()),
                consumed_at: None,
            }],
            ledger_dto(AuditUsageLedgerStatus {
                schema_version: "shadow-audit-usage-ledger-v1".to_string(),
                next_sequence: 12,
                audit_count: 5,
                usage_count: 7,
                persistence_path: None,
                last_persist_error: None,
            }),
        );

        assert_eq!(runtime_read_model.details.sessions.len(), 1);
        assert_eq!(
            runtime_read_model.details.sessions[0].recovery_ref.as_deref(),
            Some("recovery-1")
        );
        assert_eq!(
            runtime_read_model.details.workspaces[0].recovery_ref.as_deref(),
            Some("recovery-1")
        );
        assert_eq!(
            runtime_read_model.recovery.active_recovery_ids,
            vec!["recovery-1".to_string()]
        );
        assert_eq!(runtime_read_model.meta.ledger.next_sequence, 12);
        assert_eq!(runtime_read_model.meta.ledger.usage_count, 7);
    }

    #[test]
    fn runtime_read_model_keeps_runtime_ledger_signals_from_exported_summary() {
        let mut audit_usage_ledger = ledger_dto(AuditUsageLedgerStatus {
            schema_version: "shadow-audit-usage-ledger-v1".to_string(),
            next_sequence: 9,
            audit_count: 3,
            usage_count: 4,
            persistence_path: None,
            last_persist_error: Some("blocked".to_string()),
        });
        audit_usage_ledger.pending_flush = true;
        audit_usage_ledger.last_persisted_at = Some(UtcMillis::now());
        audit_usage_ledger.refresh_readiness();

        let runtime_read_model = runtime_read_model_dto(
            RuntimeReadModelInput::default(),
            &[],
            &[],
            audit_usage_ledger.clone(),
        );

        assert_eq!(
            runtime_read_model.meta.ledger.schema_version,
            "shadow-audit-usage-ledger-v1"
        );
        assert_eq!(runtime_read_model.meta.ledger.audit_count, 3);
        assert_eq!(runtime_read_model.meta.ledger.usage_count, 4);
        assert_eq!(
            runtime_read_model.meta.ledger.last_persist_error.as_deref(),
            Some("blocked")
        );
        assert!(runtime_read_model.meta.ledger.pending_flush);
        assert!(runtime_read_model.meta.ledger.last_persisted_at.is_some());
        assert_eq!(
            runtime_read_model.meta.ledger.cutover_readiness.is_ready,
            audit_usage_ledger.cutover_readiness.is_ready
        );
    }

    #[test]
    fn runtime_read_model_excludes_consumed_recoveries_from_active_ids() {
        let runtime_read_model = runtime_read_model_dto(
            RuntimeReadModelInput::default(),
            &[],
            &[
                WorkspaceRecoverySidecarExport {
                    recovery_ref: "recovery-ready".to_string(),
                    workspace_id: WorkspaceId::new("workspace-1"),
                    current_status: RecoveryStatus::Ready,
                    last_update: UtcMillis::now(),
                    ownership: ExecutionOwnership::default(),
                    execution_chain_ref: None,
                    snapshot_id: "snapshot-ready".to_string(),
                    diagnostic_summary: None,
                    consumed_at: None,
                },
                WorkspaceRecoverySidecarExport {
                    recovery_ref: "recovery-consumed".to_string(),
                    workspace_id: WorkspaceId::new("workspace-1"),
                    current_status: RecoveryStatus::Consumed,
                    last_update: UtcMillis::now(),
                    ownership: ExecutionOwnership::default(),
                    execution_chain_ref: None,
                    snapshot_id: "snapshot-consumed".to_string(),
                    diagnostic_summary: Some("done".to_string()),
                    consumed_at: Some(UtcMillis::now()),
                },
            ],
            ledger_dto(AuditUsageLedgerStatus::default()),
        );

        assert_eq!(
            runtime_read_model.recovery.active_recovery_ids,
            vec!["recovery-ready".to_string()]
        );
        assert_eq!(runtime_read_model.recovery.summaries.len(), 2);
        assert_eq!(
            runtime_read_model.recovery.summaries[0].current_status,
            "consumed".to_string()
        );
        assert_eq!(
            runtime_read_model.recovery.summaries[1].current_status,
            "ready".to_string()
        );
    }

    #[test]
    fn runtime_read_model_keeps_event_sourced_recovery_worker_when_workspace_sidecar_is_stale() {
        let mut input = RuntimeReadModelInput::default();
        input.recovery.summaries.push(RecoveryDiagnosticSummaryEntry {
            recovery_id: "recovery-worker-1".to_string(),
            event_count: 2,
            latest_stage: RecoveryActivityStage::WorkerResumed,
            latest_event_type: "worker.resumed.from_recovery".to_string(),
            latest_sequence: 3,
            latest_occurred_at: UtcMillis::now(),
            workspace_id: Some(WorkspaceId::new("workspace-1")),
            session_id: Some(magi_core::SessionId::new("session-1")),
            mission_id: Some(magi_core::MissionId::new("mission-1")),
            assignment_id: None,
            task_id: Some(magi_core::TaskId::new("todo-1")),
            worker_id: Some("worker-actual".to_string()),
            execution_chain_ref: Some("chain-1".to_string()),
            diagnostic_summary: Some("resume".to_string()),
            current_status: "worker_resumed".to_string(),
        });

        let runtime_read_model = runtime_read_model_dto(
            input,
            &[],
            &[WorkspaceRecoverySidecarExport {
                recovery_ref: "recovery-worker-1".to_string(),
                workspace_id: WorkspaceId::new("workspace-1"),
                current_status: RecoveryStatus::Consumed,
                last_update: UtcMillis::now(),
                ownership: ExecutionOwnership {
                    worker_id: Some(magi_core::WorkerId::new("worker-stale")),
                    ..ExecutionOwnership::default()
                },
                execution_chain_ref: Some("chain-1".to_string()),
                snapshot_id: "snapshot-1".to_string(),
                diagnostic_summary: Some("resume".to_string()),
                consumed_at: Some(UtcMillis::now()),
            }],
            ledger_dto(AuditUsageLedgerStatus::default()),
        );

        assert_eq!(runtime_read_model.recovery.summaries.len(), 1);
        assert_eq!(
            runtime_read_model.recovery.summaries[0].worker_id.as_deref(),
            Some("worker-actual")
        );
    }

    #[test]
    fn runtime_read_model_preserves_event_sourced_recovery_outcome_when_workspace_sidecar_is_consumed_snapshot() {
        let mut input = RuntimeReadModelInput::default();
        input.recovery.summaries.push(RecoveryDiagnosticSummaryEntry {
            recovery_id: "recovery-outcome-1".to_string(),
            event_count: 2,
            latest_stage: RecoveryActivityStage::WorkerResumed,
            latest_event_type: "worker.resumed.from_recovery".to_string(),
            latest_sequence: 7,
            latest_occurred_at: UtcMillis(50),
            workspace_id: None,
            session_id: Some(magi_core::SessionId::new("session-outcome-1")),
            mission_id: Some(magi_core::MissionId::new("mission-outcome-1")),
            assignment_id: None,
            task_id: Some(magi_core::TaskId::new("todo-outcome-1")),
            worker_id: Some("worker-outcome-1".to_string()),
            execution_chain_ref: Some("chain-outcome-1".to_string()),
            diagnostic_summary: None,
            current_status: "worker_resumed".to_string(),
        });

        let runtime_read_model = runtime_read_model_dto(
            input,
            &[],
            &[WorkspaceRecoverySidecarExport {
                recovery_ref: "recovery-outcome-1".to_string(),
                workspace_id: WorkspaceId::new("workspace-outcome-1"),
                current_status: RecoveryStatus::Consumed,
                last_update: UtcMillis(99),
                ownership: ExecutionOwnership {
                    session_id: Some(magi_core::SessionId::new("session-outcome-1")),
                    mission_id: Some(magi_core::MissionId::new("mission-stale")),
                    task_id: Some(magi_core::TaskId::new("todo-stale")),
                    worker_id: Some(magi_core::WorkerId::new("worker-stale")),
                    ..ExecutionOwnership::default()
                },
                execution_chain_ref: Some("chain-stale".to_string()),
                snapshot_id: "snapshot-outcome-1".to_string(),
                diagnostic_summary: Some("resume detail".to_string()),
                consumed_at: Some(UtcMillis(99)),
            }],
            ledger_dto(AuditUsageLedgerStatus::default()),
        );

        assert_eq!(runtime_read_model.recovery.summaries.len(), 1);
        let summary = &runtime_read_model.recovery.summaries[0];
        assert_eq!(summary.latest_stage, RecoveryActivityStage::WorkerResumed);
        assert_eq!(summary.current_status, "worker_resumed");
        assert_eq!(summary.latest_occurred_at, UtcMillis(50));
        assert_eq!(summary.workspace_id, Some(WorkspaceId::new("workspace-outcome-1")));
        assert_eq!(summary.diagnostic_summary.as_deref(), Some("resume detail"));
        assert_eq!(summary.worker_id.as_deref(), Some("worker-outcome-1"));
        assert_eq!(summary.execution_chain_ref.as_deref(), Some("chain-outcome-1"));
    }
}
