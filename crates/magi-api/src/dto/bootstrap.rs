use crate::{
    change_projection::PendingChangeDto,
    dto::{
        AuditUsageLedgerDto, BridgePreflightSnapshotDto, BridgeServicesSnapshotDto,
        RuntimeReadModelDto, ServiceInfo, runtime_read_model_dto,
    },
    state::ApiState,
};
use magi_core::{SessionId, UtcMillis};
use magi_event_bus::{EventEnvelope, EventStreamSnapshot, RuntimeReadModelInput};
use magi_session_store::{
    CanonicalTurn, NotificationRecord, SessionProjectionInput, SessionRecord,
    SessionRuntimeSidecarExport, TimelineEntry,
};
use magi_workspace::{
    RecoveryHandle, SnapshotRecord, WorkspaceProjectionInput, WorkspaceRecord,
    WorkspaceRecoverySidecarExport,
};
use serde::Serialize;

const BOOTSTRAP_TIMELINE_PAGE_SIZE: usize = 50;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapAgentDto {
    pub runtime_epoch: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapDto {
    pub agent: BootstrapAgentDto,
    pub service: ServiceInfo,
    pub generated_at: UtcMillis,
    pub current_session: Option<SessionRecord>,
    pub sessions: Vec<SessionRecord>,
    pub timeline: Vec<TimelineEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub canonical_turns: Vec<CanonicalTurn>,
    pub workspaces: Vec<WorkspaceRecord>,
    pub snapshots: Vec<SnapshotRecord>,
    pub recovery_handles: Vec<RecoveryHandle>,
    pub runtime_read_model: RuntimeReadModelDto,
    pub audit_usage_ledger: AuditUsageLedgerDto,
    pub bridge_services: BridgeServicesSnapshotDto,
    pub bridge_preflight: BridgePreflightSnapshotDto,
    pub notifications: Vec<NotificationRecord>,
    pub recent_events: Vec<EventEnvelope>,
    pub has_more_before: bool,
    pub before_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_changes: Vec<PendingChangeDto>,
}

impl BootstrapDto {
    pub fn from_state(state: &ApiState) -> Self {
        Self::from_state_with_selected_session(state, None)
    }

    pub fn from_state_with_selected_session(
        state: &ApiState,
        requested_session_id: Option<&SessionId>,
    ) -> Self {
        Self::from_state_with_session_projection(
            state,
            select_session_projection(state.session_store.projection_input(), requested_session_id),
        )
    }

    pub(crate) fn from_state_with_session_projection(
        state: &ApiState,
        session_projection: SessionProjectionInput,
    ) -> Self {
        let mut dto = Self::from_projection(
            state.runtime_epoch().to_string(),
            state.service_info.clone(),
            session_projection,
            state.workspace_registry.projection_input(),
            state.session_store.execution_sidecar_exports(),
            state.workspace_registry.recovery_sidecar_exports(),
            state.event_bus.snapshot(),
            state.event_bus.runtime_read_model_input(),
            state.audit_usage_ledger_dto(),
            state.bridge_services_dto(),
            state.bridge_preflight_dto(),
            state.task_store(),
        );
        if let Some(current_session) = dto.current_session.as_ref() {
            dto.pending_changes = crate::change_projection::collect_session_pending_changes(
                state,
                &current_session.session_id,
                current_session.workspace_id.as_deref(),
            )
            .unwrap_or_else(|error| {
                tracing::warn!(
                    session_id = %current_session.session_id,
                    "bootstrap pending changes 收集失败: {:?}",
                    error
                );
                Vec::new()
            });
        }
        dto.truncate_initial_timeline_page();
        dto
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_projection(
        runtime_epoch: String,
        service: ServiceInfo,
        session_projection: SessionProjectionInput,
        workspace_projection: WorkspaceProjectionInput,
        session_sidecar_exports: Vec<SessionRuntimeSidecarExport>,
        workspace_sidecar_exports: Vec<WorkspaceRecoverySidecarExport>,
        event_snapshot: EventStreamSnapshot,
        runtime_read_model: RuntimeReadModelInput,
        audit_usage_ledger: AuditUsageLedgerDto,
        bridge_services: BridgeServicesSnapshotDto,
        bridge_preflight: BridgePreflightSnapshotDto,
        task_store: Option<&magi_orchestrator::task_store::TaskStore>,
    ) -> Self {
        let current_session =
            session_projection
                .current_session_id
                .as_ref()
                .and_then(|session_id| {
                    session_projection
                        .sessions
                        .iter()
                        .find(|session| &session.session_id == session_id)
                        .cloned()
                });
        let runtime_read_model = runtime_read_model_dto(
            runtime_read_model,
            &session_sidecar_exports,
            &workspace_sidecar_exports,
            audit_usage_ledger.clone(),
            task_store,
        );

        Self {
            agent: BootstrapAgentDto { runtime_epoch },
            service,
            generated_at: UtcMillis::now(),
            current_session,
            sessions: session_projection.sessions,
            timeline: session_projection.timeline,
            canonical_turns: session_projection.canonical_turns,
            workspaces: workspace_projection.workspaces,
            snapshots: workspace_projection.snapshots,
            recovery_handles: workspace_projection.recovery_handles,
            runtime_read_model,
            audit_usage_ledger,
            bridge_services,
            bridge_preflight,
            notifications: session_projection.notifications,
            recent_events: event_snapshot.recent_events,
            has_more_before: false,
            before_cursor: None,
            pending_changes: Vec::new(),
        }
    }

    fn truncate_initial_timeline_page(&mut self) {
        if self.timeline.len() <= BOOTSTRAP_TIMELINE_PAGE_SIZE {
            self.has_more_before = false;
            self.before_cursor = self.timeline.first().map(|entry| entry.entry_id.clone());
            return;
        }
        let start = self
            .timeline
            .len()
            .saturating_sub(BOOTSTRAP_TIMELINE_PAGE_SIZE);
        self.timeline = self.timeline.split_off(start);
        self.has_more_before = true;
        self.before_cursor = self.timeline.first().map(|entry| entry.entry_id.clone());
    }
}

fn select_session_projection(
    mut session_projection: SessionProjectionInput,
    requested_session_id: Option<&SessionId>,
) -> SessionProjectionInput {
    let Some(requested_session_id) = requested_session_id else {
        return session_projection;
    };
    if session_projection
        .sessions
        .iter()
        .any(|session| session.session_id == *requested_session_id)
    {
        session_projection.current_session_id = Some(requested_session_id.clone());
        session_projection
            .timeline
            .retain(|entry| entry.session_id == *requested_session_id);
        session_projection
            .canonical_turns
            .retain(|turn| turn.session_id == *requested_session_id);
        session_projection
            .notifications
            .retain(|notification| notification.session_id == *requested_session_id);
    }
    session_projection
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::ledger_dto;
    use magi_core::{ExecutionOwnership, SessionId, WorkspaceId};
    use magi_event_bus::{
        AuditUsageLedgerStatus, RuntimeExecutorSummary, RuntimeMaintenanceSummary,
    };
    use magi_governance::GovernanceService;
    use magi_session_store::SessionExecutionSidecarStatus;
    use magi_session_store::SessionStore;
    use magi_workspace::{RecoveryStatus, WorkspaceStore};
    use std::sync::Arc;

    fn service_info() -> ServiceInfo {
        ServiceInfo {
            service_name: "magi".to_string(),
            api_version: "v0-shadow".to_string(),
        }
    }

    fn runtime_epoch() -> String {
        "runtime-test".to_string()
    }

    fn empty_session_projection() -> SessionProjectionInput {
        SessionProjectionInput {
            current_session_id: None,
            sessions: Vec::new(),
            timeline: Vec::new(),
            canonical_turns: Vec::new(),
            notifications: Vec::new(),
        }
    }

    fn empty_workspace_projection() -> WorkspaceProjectionInput {
        WorkspaceProjectionInput {
            active_workspace_id: None,
            workspaces: Vec::new(),
            worktree_allocations: Vec::new(),
            snapshots: Vec::new(),
            recovery_handles: Vec::new(),
        }
    }

    #[test]
    fn bootstrap_from_state_matches_projection_based_construction() {
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let state = ApiState::new(
            "magi",
            event_bus,
            session_store,
            workspace_store,
            governance,
        );

        let bootstrap = BootstrapDto::from_state(&state);

        assert_eq!(bootstrap.service.service_name, "magi");
        assert_eq!(bootstrap.service.api_version, "v0-shadow");
        assert!(bootstrap.sessions.is_empty());
        assert!(bootstrap.workspaces.is_empty());
        assert_eq!(
            bootstrap.audit_usage_ledger.schema_version,
            bootstrap.runtime_read_model.meta.ledger.schema_version
        );
        assert_eq!(
            bootstrap.audit_usage_ledger.audit_count,
            bootstrap.runtime_read_model.meta.ledger.audit_count
        );
        assert!(bootstrap.bridge_services.services.is_empty());
        assert!(bootstrap.bridge_preflight.services.is_empty());
    }

    #[test]
    fn bootstrap_from_projection_merges_sidecar_exports_into_runtime_read_model() {
        let bootstrap = BootstrapDto::from_projection(
            runtime_epoch(),
            service_info(),
            empty_session_projection(),
            empty_workspace_projection(),
            vec![SessionRuntimeSidecarExport {
                session_id: SessionId::new("session-1"),
                current_status: SessionExecutionSidecarStatus::Resumed,
                last_update: UtcMillis::now(),
                ownership: ExecutionOwnership {
                    mission_id: Some(magi_core::MissionId::new("mission-1")),
                    task_id: Some(magi_core::TaskId::new("task-1")),
                    ..ExecutionOwnership::default()
                },
                execution_chain_ref: Some("chain-1".to_string()),
                recovery_ref: Some("recovery-1".to_string()),
                current_turn: None,
                active_execution_chain: None,
            }],
            vec![WorkspaceRecoverySidecarExport {
                recovery_ref: "recovery-1".to_string(),
                workspace_id: WorkspaceId::new("workspace-1"),
                current_status: RecoveryStatus::Ready,
                last_update: UtcMillis::now(),
                ownership: ExecutionOwnership {
                    session_id: Some(SessionId::new("session-1")),
                    mission_id: Some(magi_core::MissionId::new("mission-1")),
                    task_id: Some(magi_core::TaskId::new("task-1")),
                    ..ExecutionOwnership::default()
                },
                execution_chain_ref: Some("chain-1".to_string()),
                snapshot_id: "snapshot-1".to_string(),
                diagnostic_summary: Some("resume".to_string()),
                consumed_at: None,
            }],
            EventStreamSnapshot::default(),
            RuntimeReadModelInput::default(),
            AuditUsageLedgerDto::default(),
            BridgeServicesSnapshotDto::default(),
            BridgePreflightSnapshotDto::default(),
            None,
        );

        assert_eq!(bootstrap.runtime_read_model.details.sessions.len(), 1);
        assert_eq!(
            bootstrap.runtime_read_model.details.sessions[0]
                .recovery_ref
                .as_deref(),
            Some("recovery-1")
        );
        assert_eq!(
            bootstrap.runtime_read_model.details.workspaces[0]
                .recovery_ref
                .as_deref(),
            Some("recovery-1")
        );
        assert_eq!(
            bootstrap.runtime_read_model.recovery.active_recovery_ids,
            vec!["recovery-1".to_string()]
        );
    }

    #[test]
    fn bootstrap保留runtime_maintenance状态() {
        let mut read_model = RuntimeReadModelInput::default();
        read_model.meta.maintenance = RuntimeMaintenanceSummary {
            maintenance_mode: Some("active".to_string()),
            policy_profile: Some("aggressive".to_string()),
            ..RuntimeMaintenanceSummary::default()
        };

        let bootstrap = BootstrapDto::from_projection(
            runtime_epoch(),
            service_info(),
            empty_session_projection(),
            empty_workspace_projection(),
            Vec::new(),
            Vec::new(),
            EventStreamSnapshot::default(),
            read_model,
            AuditUsageLedgerDto::default(),
            BridgeServicesSnapshotDto::default(),
            BridgePreflightSnapshotDto::default(),
            None,
        );

        assert_eq!(
            bootstrap
                .runtime_read_model
                .meta
                .maintenance
                .maintenance_mode
                .as_deref(),
            Some("active")
        );
        assert_eq!(
            bootstrap
                .runtime_read_model
                .meta
                .maintenance
                .policy_profile
                .as_deref(),
            Some("aggressive")
        );
    }

    #[test]
    fn bootstrap保留runtime_executor状态() {
        let mut read_model = RuntimeReadModelInput::default();
        read_model.meta.executor = RuntimeExecutorSummary {
            executor_id: Some("executor-1".to_string()),
            observation_status: Some("healthy".to_string()),
            ..RuntimeExecutorSummary::default()
        };

        let bootstrap = BootstrapDto::from_projection(
            runtime_epoch(),
            service_info(),
            empty_session_projection(),
            empty_workspace_projection(),
            Vec::new(),
            Vec::new(),
            EventStreamSnapshot::default(),
            read_model,
            AuditUsageLedgerDto::default(),
            BridgeServicesSnapshotDto::default(),
            BridgePreflightSnapshotDto::default(),
            None,
        );

        assert_eq!(
            bootstrap
                .runtime_read_model
                .meta
                .executor
                .executor_id
                .as_deref(),
            Some("executor-1")
        );
        assert_eq!(
            bootstrap
                .runtime_read_model
                .meta
                .executor
                .observation_status
                .as_deref(),
            Some("healthy")
        );
    }

    #[test]
    fn bootstrap会强制runtime_ledger与ledger状态保持一致() {
        let mut read_model = RuntimeReadModelInput::default();
        read_model.meta.ledger.schema_version = "stale".to_string();
        read_model.meta.ledger.audit_count = 1;
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

        let bootstrap = BootstrapDto::from_projection(
            runtime_epoch(),
            service_info(),
            empty_session_projection(),
            empty_workspace_projection(),
            Vec::new(),
            Vec::new(),
            EventStreamSnapshot::default(),
            read_model,
            audit_usage_ledger,
            BridgeServicesSnapshotDto::default(),
            BridgePreflightSnapshotDto::default(),
            None,
        );

        assert_eq!(
            bootstrap.runtime_read_model.meta.ledger.schema_version,
            "shadow-audit-usage-ledger-v1"
        );
        assert_eq!(bootstrap.runtime_read_model.meta.ledger.audit_count, 3);
        assert_eq!(bootstrap.runtime_read_model.meta.ledger.usage_count, 4);
        assert_eq!(
            bootstrap
                .runtime_read_model
                .meta
                .ledger
                .last_persist_error
                .as_deref(),
            Some("blocked")
        );
        assert!(bootstrap.runtime_read_model.meta.ledger.pending_flush);
        assert!(
            bootstrap
                .runtime_read_model
                .meta
                .ledger
                .last_persisted_at
                .is_some()
        );
    }

    #[test]
    fn bootstrap_consumes_usage_ledger_updates_into_runtime_read_model() {
        let bootstrap = BootstrapDto::from_projection(
            runtime_epoch(),
            service_info(),
            empty_session_projection(),
            empty_workspace_projection(),
            Vec::new(),
            Vec::new(),
            EventStreamSnapshot::default(),
            RuntimeReadModelInput::default(),
            ledger_dto(AuditUsageLedgerStatus {
                schema_version: "shadow-audit-usage-ledger-v1".to_string(),
                next_sequence: 12,
                audit_count: 5,
                usage_count: 7,
                persistence_path: None,
                last_persist_error: None,
            }),
            BridgeServicesSnapshotDto::default(),
            BridgePreflightSnapshotDto::default(),
            None,
        );

        assert_eq!(bootstrap.audit_usage_ledger.next_sequence, 12);
        assert_eq!(bootstrap.runtime_read_model.meta.ledger.next_sequence, 12);
        assert_eq!(bootstrap.runtime_read_model.meta.ledger.usage_count, 7);
    }

    #[test]
    fn bootstrap_from_state_with_selected_session_filters_timeline_and_notifications() {
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let state = ApiState::new(
            "magi",
            event_bus,
            session_store.clone(),
            workspace_store,
            governance,
        );

        let session_a = SessionId::new("session-a");
        let session_b = SessionId::new("session-b");
        session_store
            .create_session(session_a.clone(), "Session A")
            .expect("session a should be creatable");
        session_store
            .create_session(session_b.clone(), "Session B")
            .expect("session b should be creatable");
        session_store.append_timeline_entry(
            session_a.clone(),
            magi_session_store::TimelineEntryKind::UserMessage,
            "message-a",
        );
        session_store.append_timeline_entry(
            session_b.clone(),
            magi_session_store::TimelineEntryKind::UserMessage,
            "message-b",
        );
        session_store.append_notification(session_a.clone(), "notification-a", "toast", "notify-a");
        session_store.append_notification(session_b.clone(), "notification-b", "toast", "notify-b");

        let bootstrap = BootstrapDto::from_state_with_selected_session(&state, Some(&session_a));

        assert_eq!(
            bootstrap
                .current_session
                .as_ref()
                .map(|session| session.session_id.clone()),
            Some(session_a.clone())
        );
        assert!(
            bootstrap
                .timeline
                .iter()
                .all(|entry| entry.session_id == session_a)
        );
        assert!(
            bootstrap
                .notifications
                .iter()
                .all(|notification| notification.session_id == session_a)
        );
        assert!(
            bootstrap
                .timeline
                .iter()
                .any(|entry| entry.message == "message-a")
        );
        assert!(
            bootstrap
                .timeline
                .iter()
                .all(|entry| entry.message != "message-b")
        );
        assert!(
            bootstrap
                .notifications
                .iter()
                .any(|notification| notification.message == "notify-a")
        );
        assert!(
            bootstrap
                .notifications
                .iter()
                .all(|notification| notification.message != "notify-b")
        );
    }

    #[test]
    fn bootstrap_selected_session_returns_initial_timeline_page_only() {
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let state = ApiState::new(
            "magi",
            event_bus,
            session_store.clone(),
            workspace_store,
            governance,
        );

        let session_id = SessionId::new("session-paged-bootstrap");
        session_store
            .create_session(session_id.clone(), "Paged Session")
            .expect("session should be creatable");
        for index in 0..60 {
            session_store.append_timeline_entry(
                session_id.clone(),
                magi_session_store::TimelineEntryKind::UserMessage,
                format!("message-{index}"),
            );
        }

        let bootstrap = BootstrapDto::from_state_with_selected_session(&state, Some(&session_id));

        assert_eq!(bootstrap.timeline.len(), BOOTSTRAP_TIMELINE_PAGE_SIZE);
        assert!(bootstrap.has_more_before);
        assert_eq!(
            bootstrap.before_cursor.as_deref(),
            bootstrap
                .timeline
                .first()
                .map(|entry| entry.entry_id.as_str())
        );
    }
}
