use crate::{
    change_projection::{PendingChangeDto, PendingChangesStateDto},
    dto::{
        AuditUsageLedgerDto, BridgePreflightSnapshotDto, BridgeServicesSnapshotDto,
        RuntimeReadModelDto, ServiceInfo, runtime_read_model_dto_with_usage,
    },
    errors::ApiError,
    public_canonical::{public_canonical_turn, public_event_envelope},
    state::ApiState,
};
use magi_core::{SessionId, UtcMillis};
use magi_event_bus::{
    EventEnvelope, EventStreamSnapshot, RuntimeReadModelInput, SessionRuntimeUsageObservation,
};
use magi_session_store::{
    CanonicalTurn, NotificationRecord, SessionProjectionInput, SessionRecord,
    SessionRuntimeSidecarExport, TimelineEntry,
};
use magi_workspace::{
    RecoveryHandle, SnapshotRecord, WorkspaceProjectionInput, WorkspaceRecord,
    WorkspaceRecoverySidecarExport,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

const BOOTSTRAP_TIMELINE_PAGE_SIZE: usize = 50;
const BOOTSTRAP_RECENT_EVENT_PAGE_SIZE: usize = 200;
const RUNTIME_MAINTENANCE_STATUS_EVENT: &str = "system.runtime.maintenance.status";

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
    pub event_stream_next_sequence: u64,
    pub recent_events: Vec<EventEnvelope>,
    pub has_more_before: bool,
    pub before_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_changes: Vec<PendingChangeDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_changes_state: Option<PendingChangesStateDto>,
}

impl BootstrapDto {
    pub fn from_state(state: &ApiState) -> Result<Self, ApiError> {
        Self::from_state_with_selected_session(state, None)
    }

    pub fn from_state_with_selected_session(
        state: &ApiState,
        requested_session_id: Option<&SessionId>,
    ) -> Result<Self, ApiError> {
        let event_snapshot = state.event_bus.snapshot();
        Self::from_state_with_session_projection(
            state,
            select_session_projection(state.session_store.projection_input(), requested_session_id),
            event_snapshot,
        )
    }

    pub(crate) fn from_state_with_session_projection(
        state: &ApiState,
        session_projection: SessionProjectionInput,
        event_snapshot: EventStreamSnapshot,
    ) -> Result<Self, ApiError> {
        let mut dto = Self::from_projection_with_usage(
            state.runtime_epoch().to_string(),
            state.service_info.clone(),
            session_projection,
            state.workspace_registry.projection_input(),
            state.session_store.execution_sidecar_exports(),
            state.workspace_registry.recovery_sidecar_exports(),
            event_snapshot,
            state.event_bus.runtime_read_model_input(),
            state.audit_usage_ledger_dto(),
            state.bridge_services_dto(),
            state.bridge_preflight_dto(),
            state.task_store(),
            &state.ledger_usage_observations(),
        );
        if let Some(current_session) = dto.current_session.as_ref() {
            let pending_projection =
                crate::change_projection::collect_session_pending_changes_with_state(
                    state,
                    &current_session.session_id,
                    current_session.workspace_id.as_deref(),
                )?;
            dto.pending_changes = pending_projection.pending_changes;
            dto.pending_changes_state = Some(pending_projection.state);
        }
        dto.truncate_initial_timeline_page();
        Ok(dto)
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
        Self::from_projection_with_usage(
            runtime_epoch,
            service,
            session_projection,
            workspace_projection,
            session_sidecar_exports,
            workspace_sidecar_exports,
            event_snapshot,
            runtime_read_model,
            audit_usage_ledger,
            bridge_services,
            bridge_preflight,
            task_store,
            &BTreeMap::new(),
        )
    }

    /// 与 [`BootstrapDto::from_projection`] 相同,但额外接收一份从持久化用量账本
    /// 回放出的每会话用量观测值,在装配运行时读模型时用于回填重启后缺失的预算。
    #[allow(clippy::too_many_arguments)]
    pub fn from_projection_with_usage(
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
        ledger_usage_observations: &BTreeMap<String, SessionRuntimeUsageObservation>,
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
        let current_session_id = current_session.as_ref().map(|session| &session.session_id);
        let recent_events =
            select_bootstrap_recent_events(event_snapshot.recent_events, current_session_id)
                .into_iter()
                .map(public_event_envelope)
                .map(public_bootstrap_event_envelope)
                .collect();
        let runtime_read_model = runtime_read_model_dto_with_usage(
            runtime_read_model,
            &session_sidecar_exports,
            &workspace_sidecar_exports,
            audit_usage_ledger.clone(),
            task_store,
            ledger_usage_observations,
        );

        let mut dto = Self {
            agent: BootstrapAgentDto { runtime_epoch },
            service,
            generated_at: UtcMillis::now(),
            current_session,
            sessions: session_projection.sessions,
            timeline: session_projection.timeline,
            canonical_turns: session_projection
                .canonical_turns
                .into_iter()
                .map(public_canonical_turn)
                .filter(|turn| !turn.items.is_empty())
                .collect(),
            workspaces: workspace_projection.workspaces,
            snapshots: workspace_projection.snapshots,
            recovery_handles: workspace_projection.recovery_handles,
            runtime_read_model,
            audit_usage_ledger,
            bridge_services,
            bridge_preflight,
            notifications: session_projection.notifications,
            event_stream_next_sequence: event_snapshot.next_sequence,
            recent_events,
            has_more_before: false,
            before_cursor: None,
            pending_changes: Vec::new(),
            pending_changes_state: None,
        };
        dto.prune_initial_load_runtime_details();
        dto
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

    fn prune_initial_load_runtime_details(&mut self) {
        let current_session_id = self
            .current_session
            .as_ref()
            .map(|session| session.session_id.to_string());
        self.runtime_read_model.details.sessions.retain(|entry| {
            current_session_id
                .as_deref()
                .is_some_and(|session_id| entry.session_id == session_id)
        });
        let current_runtime_session = self.runtime_read_model.details.sessions.first();
        let current_recovery_ids = current_runtime_session
            .map(|entry| entry.recovery_ids.clone())
            .unwrap_or_default();
        let current_recovery_ref =
            current_runtime_session.and_then(|entry| entry.recovery_ref.clone());
        let current_mission_id = current_runtime_session.and_then(|entry| entry.mission_id.clone());
        let current_chain_ref =
            current_runtime_session.and_then(|entry| entry.execution_chain_ref.clone());
        self.runtime_read_model
            .recovery
            .summaries
            .retain(|summary| {
                current_session_id.as_deref().is_some_and(|session_id| {
                    summary
                        .session_id
                        .as_ref()
                        .is_some_and(|summary_session_id| summary_session_id.as_str() == session_id)
                }) || current_recovery_ids
                    .iter()
                    .any(|recovery_id| recovery_id == &summary.recovery_id)
                    || current_recovery_ref
                        .as_deref()
                        .is_some_and(|recovery_id| summary.recovery_id == recovery_id)
                    || current_mission_id.as_deref().is_some_and(|mission_id| {
                        summary
                            .mission_id
                            .as_ref()
                            .is_some_and(|summary_mission_id| {
                                summary_mission_id.as_str() == mission_id
                            })
                    })
                    || current_chain_ref.as_deref().is_some_and(|chain_ref| {
                        summary.execution_chain_ref.as_deref() == Some(chain_ref)
                    })
            });
        self.runtime_read_model.recovery.entries.retain(|entry| {
            current_session_id.as_deref().is_some_and(|session_id| {
                entry
                    .session_id
                    .as_ref()
                    .is_some_and(|entry_session_id| entry_session_id.as_str() == session_id)
            }) || current_recovery_ids
                .iter()
                .any(|recovery_id| recovery_id == &entry.recovery_id)
                || current_recovery_ref
                    .as_deref()
                    .is_some_and(|recovery_id| entry.recovery_id == recovery_id)
                || current_mission_id.as_deref().is_some_and(|mission_id| {
                    entry
                        .mission_id
                        .as_ref()
                        .is_some_and(|entry_mission_id| entry_mission_id.as_str() == mission_id)
                })
                || current_chain_ref.as_deref().is_some_and(|chain_ref| {
                    entry.execution_chain_ref.as_deref() == Some(chain_ref)
                })
        });
        let visible_recovery_ids = self
            .runtime_read_model
            .recovery
            .summaries
            .iter()
            .map(|summary| summary.recovery_id.clone())
            .collect::<Vec<_>>();
        self.runtime_read_model
            .recovery
            .active_recovery_ids
            .retain(|recovery_id| visible_recovery_ids.contains(recovery_id));
        for session in &mut self.runtime_read_model.details.sessions {
            session.current_turn = None;
            session.turn_items.clear();
        }
    }
}

fn select_bootstrap_recent_events(
    events: Vec<EventEnvelope>,
    current_session_id: Option<&SessionId>,
) -> Vec<EventEnvelope> {
    let Some(current_session_id) = current_session_id else {
        return Vec::new();
    };
    let mut selected = events
        .into_iter()
        .filter(|event| {
            event.event_type != RUNTIME_MAINTENANCE_STATUS_EVENT
                && event.session_id.as_ref() == Some(current_session_id)
        })
        .collect::<Vec<_>>();
    if selected.len() > BOOTSTRAP_RECENT_EVENT_PAGE_SIZE {
        let start = selected.len() - BOOTSTRAP_RECENT_EVENT_PAGE_SIZE;
        selected = selected.split_off(start);
    }
    selected
}

fn public_bootstrap_event_envelope(mut event: EventEnvelope) -> EventEnvelope {
    prune_bootstrap_event_payload(&mut event.payload);
    event
}

fn prune_bootstrap_event_payload(payload: &mut Value) {
    let Value::Object(object) = payload else {
        return;
    };
    // canonicalTurns 已经是 bootstrap 的权威会话投影；事件中的 canonical_turn
    // 只是同一份累计快照的重复副本。流式过程中每个事件都携带越来越大的快照，
    // 首屏重放若逐个反序列化会形成 O(事件数 × 累计消息体) 的 CPU 与响应体放大。
    object.remove("canonical_turn");
    object.remove("canonicalTurn");
    object.remove("turn_items");
    object.remove("turnItems");
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
        session_projection.notifications.retain(|notification| {
            matches!(
                notification.scope,
                magi_session_store::NotificationScope::App
            ) || notification.session_id.as_ref() == Some(requested_session_id)
        });
    }
    session_projection
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::ledger_dto;
    use magi_core::{
        AbsolutePath, EventId, ExecutionOwnership, SessionId, SessionLifecycleStatus, TaskId,
        ThreadId, UtcMillis, WorkerId, WorkspaceId,
    };
    use magi_event_bus::{
        AuditUsageLedgerStatus, RUNTIME_LEDGER_PERSIST_ERROR_SUMMARY, RuntimeExecutorSummary,
        RuntimeMaintenanceSummary,
    };
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_session_store::{
        ActiveExecutionTurn, ActiveExecutionTurnItem, CanonicalTurn, CanonicalTurnItem,
        CanonicalTurnItemKind, CanonicalTurnItemStatus, CanonicalTurnStatus, CanonicalWorkerRef,
        SessionExecutionSidecarStatus,
    };
    use magi_workspace::{RecoveryStatus, WorkspaceStore};
    use serde_json::json;
    use std::{fs, sync::Arc};

    fn service_info() -> ServiceInfo {
        ServiceInfo {
            service_name: "magi".to_string(),
            api_version: "v0".to_string(),
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

    fn session_record(session_id: &str) -> SessionRecord {
        let now = UtcMillis::now();
        SessionRecord {
            session_id: SessionId::new(session_id),
            title: session_id.to_string(),
            status: SessionLifecycleStatus::Active,
            created_at: now,
            updated_at: now,
            message_count: None,
            workspace_id: None,
            last_completed_at: None,
            last_viewed_at: None,
        }
    }

    fn session_projection_with_current(session_id: &str) -> SessionProjectionInput {
        SessionProjectionInput {
            current_session_id: Some(SessionId::new(session_id)),
            sessions: vec![session_record(session_id)],
            timeline: Vec::new(),
            canonical_turns: Vec::new(),
            notifications: Vec::new(),
        }
    }

    fn active_turn(turn_id: &str, content: &str) -> ActiveExecutionTurn {
        let now = UtcMillis::now();
        ActiveExecutionTurn {
            turn_id: turn_id.to_string(),
            turn_seq: now.0,
            accepted_at: now,
            completed_at: Some(now),
            status: "completed".to_string(),
            user_message: Some("用户输入".to_string()),
            items: vec![ActiveExecutionTurnItem {
                item_id: format!("{turn_id}-item"),
                item_seq: 1,
                kind: "assistant_final".to_string(),
                status: "completed".to_string(),
                source: "orchestrator".to_string(),
                title: Some("总结".to_string()),
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
                source_thread_id: ThreadId::new("thread-test-orchestrator"),
            }],
        }
    }

    fn canonical_item(
        session_id: &SessionId,
        turn_id: &str,
        item_seq: usize,
        worker: Option<CanonicalWorkerRef>,
    ) -> CanonicalTurnItem {
        let now = UtcMillis::now();
        CanonicalTurnItem {
            session_id: session_id.clone(),
            turn_id: turn_id.to_string(),
            turn_seq: 1,
            item_id: format!("item-{item_seq}"),
            item_seq,
            kind: CanonicalTurnItemKind::AssistantText,
            created_at: now,
            status: CanonicalTurnItemStatus::Completed,
            item_version: None,
            updated_at: now,
            title: None,
            content: Some(format!("content-{item_seq}")),
            blocks: Vec::new(),
            tool: None,
            worker,
            source_thread_id: ThreadId::new("thread-bootstrap-test"),
            visibility: Default::default(),
            metadata: Default::default(),
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

        let bootstrap = BootstrapDto::from_state(&state).expect("bootstrap should build");

        assert_eq!(bootstrap.service.service_name, "magi");
        assert_eq!(bootstrap.service.api_version, "v0");
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
            session_projection_with_current("session-1"),
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
    fn bootstrap首屏会裁掉非当前会话运行态明细() {
        let session_id = SessionId::new("session-current");
        let other_session_id = SessionId::new("session-other");
        let now = UtcMillis::now();
        let canonical_turn = CanonicalTurn {
            session_id: session_id.clone(),
            turn_id: "canonical-turn-1".to_string(),
            turn_seq: 1,
            accepted_at: now,
            completed_at: Some(now),
            status: CanonicalTurnStatus::Completed,
            response_duration_ms: Some(1),
            usage: None,
            items: Vec::new(),
            metadata: Default::default(),
        };
        let event_snapshot = EventStreamSnapshot {
            next_sequence: 3,
            recent_events: vec![
                EventEnvelope::system(
                    EventId::new("maintenance-1"),
                    RUNTIME_MAINTENANCE_STATUS_EVENT,
                    json!({"tick": 1}),
                ),
                EventEnvelope::system(
                    EventId::new("system-started-1"),
                    "system.started",
                    json!({"status": "ready"}),
                ),
                EventEnvelope::domain(
                    EventId::new("domain-1"),
                    "task.created",
                    json!({"task_title": "保留当前会话事件"}),
                )
                .with_context(magi_event_bus::EventContext {
                    session_id: Some(session_id.clone()),
                    ..magi_event_bus::EventContext::default()
                }),
            ],
        };
        let bootstrap = BootstrapDto::from_projection(
            runtime_epoch(),
            service_info(),
            SessionProjectionInput {
                current_session_id: Some(session_id.clone()),
                sessions: vec![session_record(session_id.as_str())],
                timeline: Vec::new(),
                canonical_turns: vec![canonical_turn],
                notifications: Vec::new(),
            },
            empty_workspace_projection(),
            vec![
                SessionRuntimeSidecarExport {
                    session_id: session_id.clone(),
                    current_status: SessionExecutionSidecarStatus::Bound,
                    last_update: now,
                    ownership: ExecutionOwnership::default(),
                    execution_chain_ref: None,
                    recovery_ref: None,
                    current_turn: Some(active_turn("turn-current", "current detail")),
                    active_execution_chain: None,
                },
                SessionRuntimeSidecarExport {
                    session_id: other_session_id.clone(),
                    current_status: SessionExecutionSidecarStatus::Bound,
                    last_update: now,
                    ownership: ExecutionOwnership::default(),
                    execution_chain_ref: None,
                    recovery_ref: None,
                    current_turn: Some(active_turn("turn-other", "other detail")),
                    active_execution_chain: None,
                },
            ],
            vec![WorkspaceRecoverySidecarExport {
                recovery_ref: "recovery-other".to_string(),
                workspace_id: WorkspaceId::new("workspace-other"),
                current_status: RecoveryStatus::Ready,
                last_update: now,
                ownership: ExecutionOwnership {
                    session_id: Some(other_session_id),
                    ..ExecutionOwnership::default()
                },
                execution_chain_ref: Some("chain-other".to_string()),
                snapshot_id: "snapshot-other".to_string(),
                diagnostic_summary: Some("other session recovery".to_string()),
                consumed_at: None,
            }],
            event_snapshot,
            RuntimeReadModelInput::default(),
            AuditUsageLedgerDto::default(),
            BridgeServicesSnapshotDto::default(),
            BridgePreflightSnapshotDto::default(),
            None,
        );

        assert_eq!(bootstrap.event_stream_next_sequence, 3);
        assert_eq!(bootstrap.recent_events.len(), 1);
        assert_eq!(bootstrap.recent_events[0].event_type, "task.created");
        assert!(bootstrap.canonical_turns.is_empty());
        assert_eq!(bootstrap.runtime_read_model.details.sessions.len(), 1);
        let session = &bootstrap.runtime_read_model.details.sessions[0];
        assert_eq!(session.session_id, "session-current");
        assert!(session.current_turn.is_none());
        assert!(session.turn_items.is_empty());
        assert!(bootstrap.runtime_read_model.recovery.entries.is_empty());
        assert!(bootstrap.runtime_read_model.recovery.summaries.is_empty());
        assert!(
            bootstrap
                .runtime_read_model
                .recovery
                .active_recovery_ids
                .is_empty()
        );
    }

    #[test]
    fn bootstrap事件先按会话和页大小筛选再做公开投影() {
        let current_session_id = SessionId::new("session-current");
        let other_session_id = SessionId::new("session-other");
        let mut events = (0..250)
            .map(|index| {
                EventEnvelope::domain(
                    EventId::new(format!("current-{index}")),
                    "message.updated",
                    json!({"index": index}),
                )
                .with_context(magi_event_bus::EventContext {
                    session_id: Some(current_session_id.clone()),
                    ..magi_event_bus::EventContext::default()
                })
            })
            .collect::<Vec<_>>();
        events.push(
            EventEnvelope::domain(
                EventId::new("other-session"),
                "message.updated",
                json!({"index": 999}),
            )
            .with_context(magi_event_bus::EventContext {
                session_id: Some(other_session_id),
                ..magi_event_bus::EventContext::default()
            }),
        );

        let selected = select_bootstrap_recent_events(events, Some(&current_session_id));

        assert_eq!(selected.len(), BOOTSTRAP_RECENT_EVENT_PAGE_SIZE);
        assert_eq!(selected[0].payload["index"], json!(50));
        assert_eq!(selected.last().unwrap().payload["index"], json!(249));
    }

    #[test]
    fn bootstrap首屏保留完整canonical并裁掉事件重型回放() {
        let session_id = SessionId::new("session-bootstrap-mainline-only");
        let now = UtcMillis::now();
        let turn_id = "canonical-turn-mainline";
        let canonical_turn = CanonicalTurn {
            session_id: session_id.clone(),
            turn_id: turn_id.to_string(),
            turn_seq: 1,
            accepted_at: now,
            completed_at: Some(now),
            status: CanonicalTurnStatus::Completed,
            response_duration_ms: Some(1),
            usage: None,
            items: vec![
                canonical_item(&session_id, turn_id, 1, None),
                canonical_item(
                    &session_id,
                    turn_id,
                    2,
                    Some(CanonicalWorkerRef {
                        task_id: Some(TaskId::new("task-child")),
                        worker_id: Some(WorkerId::new("worker-child")),
                        role_id: Some("executor".to_string()),
                        title: Some("shell_exec".to_string()),
                    }),
                ),
            ],
            metadata: Default::default(),
        };
        let event_snapshot = EventStreamSnapshot {
            next_sequence: 9,
            recent_events: vec![
                EventEnvelope::domain(
                    EventId::new("message-heavy"),
                    "message.created",
                    json!({
                        "root_task_id": "task-root",
                        "turn_items": [{"tool_result": "large worker payload"}],
                        "canonical_turn": canonical_turn.clone(),
                    }),
                )
                .with_context(magi_event_bus::EventContext {
                    session_id: Some(session_id.clone()),
                    ..magi_event_bus::EventContext::default()
                }),
            ],
        };

        let bootstrap = BootstrapDto::from_projection(
            runtime_epoch(),
            service_info(),
            SessionProjectionInput {
                current_session_id: Some(session_id.clone()),
                sessions: vec![session_record(session_id.as_str())],
                timeline: Vec::new(),
                canonical_turns: vec![canonical_turn],
                notifications: Vec::new(),
            },
            empty_workspace_projection(),
            Vec::new(),
            Vec::new(),
            event_snapshot,
            RuntimeReadModelInput::default(),
            AuditUsageLedgerDto::default(),
            BridgeServicesSnapshotDto::default(),
            BridgePreflightSnapshotDto::default(),
            None,
        );

        assert_eq!(bootstrap.canonical_turns.len(), 1);
        assert_eq!(bootstrap.canonical_turns[0].items.len(), 2);
        assert!(bootstrap.canonical_turns[0].items[1].worker.is_some());
        let event_payload = &bootstrap.recent_events[0].payload;
        assert!(event_payload.get("turn_items").is_none());
        assert!(event_payload.get("canonical_turn").is_none());
        assert_eq!(event_payload["root_task_id"], json!("task-root"));
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
            schema_version: "audit-usage-ledger-v1".to_string(),
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
            "audit-usage-ledger-v1"
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
            Some(RUNTIME_LEDGER_PERSIST_ERROR_SUMMARY)
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
                schema_version: "audit-usage-ledger-v1".to_string(),
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
    fn bootstrap_from_state_with_selected_session_keeps_app_and_selected_session_incidents() {
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
            .create_session_for_workspace(
                session_a.clone(),
                "Session A",
                Some("workspace-a".to_string()),
            )
            .expect("session a should be creatable");
        session_store
            .create_session_for_workspace(
                session_b.clone(),
                "Session B",
                Some("workspace-a".to_string()),
            )
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
        for (notification_id, scope, session_id, message) in [
            (
                "notification-app",
                magi_session_store::NotificationScope::App,
                None,
                "notify-app",
            ),
            (
                "notification-a",
                magi_session_store::NotificationScope::Session,
                Some(session_a.clone()),
                "notify-a",
            ),
            (
                "notification-b",
                magi_session_store::NotificationScope::Session,
                Some(session_b.clone()),
                "notify-b",
            ),
        ] {
            session_store
                .append_incident_record(NotificationRecord {
                    notification_id: notification_id.to_string(),
                    scope,
                    workspace_id: (scope == magi_session_store::NotificationScope::Session)
                        .then(|| "workspace-a".to_string()),
                    session_id,
                    kind: "incident".to_string(),
                    level: Some("error".to_string()),
                    title: None,
                    message: message.to_string(),
                    source: Some("test".to_string()),
                    created_at: UtcMillis::now(),
                    handled: false,
                    action_required: true,
                    count_unread: true,
                    fingerprint: notification_id.to_string(),
                    occurrence_count: 1,
                    resolved: false,
                })
                .expect("incident should append");
        }

        let bootstrap = BootstrapDto::from_state_with_selected_session(&state, Some(&session_a))
            .expect("bootstrap should build");

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
        assert_eq!(bootstrap.notifications.len(), 2);
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
                .any(|notification| notification.message == "notify-app")
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

        let bootstrap = BootstrapDto::from_state_with_selected_session(&state, Some(&session_id))
            .expect("bootstrap should build");

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

    #[tokio::test]
    async fn bootstrap_pending_changes_uses_summary_payload() {
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let workspace_root = std::env::temp_dir().join(format!(
            "magi-bootstrap-pending-summary-{}",
            UtcMillis::now().0
        ));
        fs::create_dir_all(&workspace_root).expect("workspace root should create");
        fs::write(workspace_root.join("alpha.txt"), "alpha\n").expect("baseline file should write");
        workspace_store
            .register(
                WorkspaceId::new("workspace-pending-summary"),
                AbsolutePath::new(workspace_root.to_string_lossy().to_string()),
            )
            .expect("workspace should register");
        let state = ApiState::new(
            "magi",
            event_bus,
            session_store.clone(),
            workspace_store,
            governance,
        );

        let session_id = SessionId::new("session-pending-summary");
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "Pending Summary",
                Some("workspace-pending-summary".to_string()),
            )
            .expect("session should be creatable");
        let snapshot = state
            .snapshot_manager
            .start_session(session_id.as_str().to_string(), workspace_root.clone())
            .await
            .expect("snapshot should start");
        fs::write(workspace_root.join("alpha.txt"), "alpha changed\n")
            .expect("changed file should write");
        snapshot.reconcile().expect("snapshot should reconcile");

        let bootstrap = BootstrapDto::from_state_with_selected_session(&state, Some(&session_id))
            .expect("bootstrap should build");

        assert_eq!(bootstrap.pending_changes.len(), 1);
        let change = &bootstrap.pending_changes[0];
        assert_eq!(change.file_path, "alpha.txt");
        assert_eq!(change.additions, 1);
        assert_eq!(change.deletions, 1);
        assert!(change.diff.is_empty());
        assert!(change.original_content.is_none());
        assert!(change.preview_content.is_none());

        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn bootstrap_marks_pending_changes_not_ready_when_snapshot_session_is_missing() {
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let workspace_root = std::env::temp_dir().join(format!(
            "magi-bootstrap-pending-state-{}",
            UtcMillis::now().0
        ));
        fs::create_dir_all(&workspace_root).expect("workspace root should create");
        workspace_store
            .register(
                WorkspaceId::new("workspace-pending-state"),
                AbsolutePath::new(workspace_root.to_string_lossy().to_string()),
            )
            .expect("workspace should register");
        let state = ApiState::new(
            "magi",
            event_bus,
            session_store.clone(),
            workspace_store,
            governance,
        );

        let session_id = SessionId::new("session-pending-state");
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "Pending State",
                Some("workspace-pending-state".to_string()),
            )
            .expect("session should be creatable");

        let bootstrap = BootstrapDto::from_state_with_selected_session(&state, Some(&session_id))
            .expect("bootstrap should build");

        assert!(bootstrap.pending_changes.is_empty());
        let state = bootstrap
            .pending_changes_state
            .expect("bootstrap should expose pending changes state");
        assert_eq!(state.status, "not_ready");
        assert_eq!(state.reason_code.as_deref(), Some("changes_preparing"));
        assert_eq!(state.pending_count, 0);
        assert_eq!(state.session_id.as_deref(), Some("session-pending-state"));
        assert_eq!(
            state.workspace_id.as_deref(),
            Some("workspace-pending-state")
        );

        let _ = fs::remove_dir_all(workspace_root);
    }
}
