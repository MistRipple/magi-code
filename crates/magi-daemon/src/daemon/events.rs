use super::types::DaemonRuntimeStatus;
use magi_core::EventId;
use magi_event_bus::{EventEnvelope, InMemoryEventBus, RuntimeLedgerSummary};
use std::sync::Arc;

pub(crate) fn publish_ledger_status_event(
    event_bus: &Arc<InMemoryEventBus>,
    event_id: &str,
    event_type: &str,
) {
    let status = event_bus.runtime_ledger_summary();
    let _ = event_bus.publish(EventEnvelope::system(
        EventId::new(event_id),
        event_type,
        ledger_status_payload(&status),
    ));
}

pub(crate) fn ledger_status_payload(status: &RuntimeLedgerSummary) -> serde_json::Value {
    serde_json::json!({
        "schema_version": status.schema_version,
        "audit_count": status.audit_count,
        "usage_count": status.usage_count,
        "next_sequence": status.next_sequence,
        "persistence_path": status.persistence_path,
        "last_persist_error": status.last_persist_error,
        "is_persist_healthy": status.is_persist_healthy,
        "last_persisted_at": status.last_persisted_at,
        "pending_flush": status.pending_flush,
        "readiness": {
            "is_ready": status.readiness.is_ready,
            "blocking_issue_count": status.readiness.blocking_issue_count,
            "blocking_issues": status.readiness.blocking_issues,
        },
        "cutover_readiness": {
            "is_ready": status.cutover_readiness.is_ready,
            "blocking_issue_count": status.cutover_readiness.blocking_issue_count,
            "blocking_issues": status.cutover_readiness.blocking_issues,
        },
    })
}

pub(crate) fn runtime_status_payload(status: &DaemonRuntimeStatus) -> serde_json::Value {
    serde_json::json!({
        "maintenance_mode": status.maintenance_mode.as_str(),
        "policy_profile": status.policy_profile.as_str(),
        "mode_reason": status.mode_reason,
        "shutdown_requested_at": status.shutdown_requested_at,
        "shutdown_completed_at": status.shutdown_completed_at,
        "last_tick_at": status.last_tick_at,
        "last_sidecar_outcome": status.last_sidecar_outcome,
        "last_ledger_outcome": status.last_ledger_outcome,
        "tick_interval_millis": status.tick_interval_millis,
        "sidecar_flush_enabled": status.sidecar_flush_enabled,
        "ledger_refresh_enabled": status.ledger_refresh_enabled,
        "eager_flush_dirty_sidecars": status.eager_flush_dirty_sidecars,
        "refresh_ledger_when_unhealthy": status.refresh_ledger_when_unhealthy,
        "refresh_ledger_when_never_persisted": status.refresh_ledger_when_never_persisted,
    })
}
