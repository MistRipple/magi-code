use crate::{
    AuditUsageLedgerError, AuditUsageLedgerSnapshot, AuditUsageLedgerStatus, EventEnvelope,
    EventStreamSnapshot, RecoveryReadModelInput, RuntimeLedgerSummary, RuntimeReadModelInput,
};
use magi_core::UtcMillis;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicU64, Ordering},
};
use thiserror::Error;
use tokio::sync::broadcast;

#[derive(Debug, Error)]
pub enum EventBusError {
    #[error("事件总线发送失败: {0}")]
    Send(#[from] broadcast::error::SendError<EventEnvelope>),
}

#[derive(Clone, Debug)]
pub struct InMemoryEventBus {
    sender: broadcast::Sender<EventEnvelope>,
    sequence: Arc<AtomicU64>,
    recent_events: Arc<RwLock<Vec<EventEnvelope>>>,
    audit_usage_ledger: Arc<RwLock<AuditUsageLedgerSnapshot>>,
    audit_usage_ledger_path: Arc<RwLock<Option<PathBuf>>>,
    audit_usage_ledger_last_error: Arc<RwLock<Option<String>>>,
    audit_usage_ledger_pending_flush: Arc<RwLock<bool>>,
    audit_usage_ledger_last_persisted_at: Arc<RwLock<Option<UtcMillis>>>,
    retain_limit: usize,
}

impl InMemoryEventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self {
            sender,
            sequence: Arc::new(AtomicU64::new(0)),
            recent_events: Arc::new(RwLock::new(Vec::new())),
            audit_usage_ledger: Arc::new(RwLock::new(AuditUsageLedgerSnapshot::default())),
            audit_usage_ledger_path: Arc::new(RwLock::new(None)),
            audit_usage_ledger_last_error: Arc::new(RwLock::new(None)),
            audit_usage_ledger_pending_flush: Arc::new(RwLock::new(false)),
            audit_usage_ledger_last_persisted_at: Arc::new(RwLock::new(None)),
            retain_limit: capacity,
        }
    }

    pub fn publish(&self, mut event: EventEnvelope) -> Result<u64, EventBusError> {
        let mut recent_events = self
            .recent_events
            .write()
            .expect("event bus recent events write lock poisoned");
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        event.sequence = sequence;
        recent_events.push(event.clone());
        if recent_events.len() > self.retain_limit {
            let drain_count = recent_events.len() - self.retain_limit;
            recent_events.drain(0..drain_count);
        }
        {
            let mut ledger = self
                .audit_usage_ledger
                .write()
                .expect("event bus audit/usage ledger write lock poisoned");
            ledger.record_event(&event);
        }
        if matches!(
            event.category,
            crate::EventCategory::Audit | crate::EventCategory::Usage
        ) {
            self.mark_audit_usage_ledger_dirty();
        }
        self.refresh_audit_usage_ledger_persistence_if_configured();
        // Event retention and ledger updates are already committed above. The absence of a
        // live stream subscriber should not make the bus fail closed for normal API writes.
        let _ = self.sender.send(event);
        Ok(sequence)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.sender.subscribe()
    }

    pub fn snapshot_and_subscribe(
        &self,
    ) -> (EventStreamSnapshot, broadcast::Receiver<EventEnvelope>) {
        let recent_events = self
            .recent_events
            .read()
            .expect("event bus recent events read lock poisoned");
        let receiver = self.sender.subscribe();
        let snapshot = EventStreamSnapshot {
            next_sequence: self.sequence.load(Ordering::SeqCst) + 1,
            recent_events: recent_events.clone(),
        };
        (snapshot, receiver)
    }

    pub fn snapshot(&self) -> EventStreamSnapshot {
        EventStreamSnapshot {
            next_sequence: self.sequence.load(Ordering::SeqCst) + 1,
            recent_events: self
                .recent_events
                .read()
                .expect("event bus recent events read lock poisoned")
                .clone(),
        }
    }

    pub fn recovery_read_model_input(&self) -> RecoveryReadModelInput {
        let recent_events = self
            .recent_events
            .read()
            .expect("event bus recent events read lock poisoned");
        RecoveryReadModelInput::from_events(&recent_events)
    }

    pub fn runtime_read_model_input(&self) -> RuntimeReadModelInput {
        let recent_events = self
            .recent_events
            .read()
            .expect("event bus recent events read lock poisoned");
        let mut read_model = RuntimeReadModelInput::from_events(&recent_events);
        read_model.meta.ledger = self.runtime_ledger_summary();
        read_model
    }

    pub fn runtime_ledger_summary(&self) -> RuntimeLedgerSummary {
        let mut summary = RuntimeLedgerSummary::from(self.audit_usage_ledger_status());
        summary.is_persist_healthy = summary.last_persist_error.is_none();
        summary.pending_flush = *self
            .audit_usage_ledger_pending_flush
            .read()
            .expect("event bus audit/usage ledger pending flush read lock poisoned");
        summary.last_persisted_at = *self
            .audit_usage_ledger_last_persisted_at
            .read()
            .expect("event bus audit/usage ledger last persisted at read lock poisoned");
        summary.refresh_readiness();
        summary
    }

    pub fn audit_usage_ledger_snapshot(&self) -> AuditUsageLedgerSnapshot {
        self.audit_usage_ledger
            .read()
            .expect("event bus audit/usage ledger read lock poisoned")
            .clone()
            .normalize()
    }

    pub fn audit_usage_ledger_status(&self) -> AuditUsageLedgerStatus {
        let snapshot = self.audit_usage_ledger_snapshot();
        let persistence_path = self
            .audit_usage_ledger_path
            .read()
            .expect("event bus audit/usage ledger path read lock poisoned")
            .clone();
        let last_persist_error = self
            .audit_usage_ledger_last_error
            .read()
            .expect("event bus audit/usage ledger error read lock poisoned")
            .clone();
        snapshot.status(persistence_path.as_deref(), last_persist_error)
    }

    pub fn export_audit_usage_ledger_json(&self) -> Result<String, AuditUsageLedgerError> {
        self.audit_usage_ledger_snapshot().export_json()
    }

    pub fn import_audit_usage_ledger_json(&self, value: &str) -> Result<(), AuditUsageLedgerError> {
        let snapshot = AuditUsageLedgerSnapshot::import_json(value)?;
        let mut ledger = self
            .audit_usage_ledger
            .write()
            .expect("event bus audit/usage ledger write lock poisoned");
        *ledger = snapshot;
        self.clear_audit_usage_ledger_error();
        self.mark_audit_usage_ledger_clean(None);
        Ok(())
    }

    pub fn import_audit_usage_ledger_snapshot(&self, snapshot: AuditUsageLedgerSnapshot) {
        let mut ledger = self
            .audit_usage_ledger
            .write()
            .expect("event bus audit/usage ledger write lock poisoned");
        *ledger = snapshot.normalize();
        self.clear_audit_usage_ledger_error();
        self.mark_audit_usage_ledger_clean(None);
    }

    pub fn reset_audit_usage_ledger(&self) {
        {
            let mut ledger = self
                .audit_usage_ledger
                .write()
                .expect("event bus audit/usage ledger write lock poisoned");
            *ledger = AuditUsageLedgerSnapshot::default();
        }
        self.clear_audit_usage_ledger_error();
        self.mark_audit_usage_ledger_dirty();
        self.refresh_audit_usage_ledger_persistence_if_configured();
    }

    pub fn set_audit_usage_ledger_persistence(&self, path: impl Into<PathBuf>) {
        let mut target = self
            .audit_usage_ledger_path
            .write()
            .expect("event bus audit/usage ledger path write lock poisoned");
        *target = Some(path.into());
    }

    pub fn refresh_audit_usage_ledger_persistence(&self) -> Result<(), AuditUsageLedgerError> {
        match self.persist_audit_usage_ledger_if_configured() {
            Ok(()) => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn persist_audit_usage_ledger(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<(), AuditUsageLedgerError> {
        self.audit_usage_ledger_snapshot().persist_to_path(path)
    }

    pub fn restore_audit_usage_ledger(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<(), AuditUsageLedgerError> {
        let snapshot = AuditUsageLedgerSnapshot::load_from_path(path)?;
        let mut ledger = self
            .audit_usage_ledger
            .write()
            .expect("event bus audit/usage ledger write lock poisoned");
        *ledger = snapshot;
        self.clear_audit_usage_ledger_error();
        Ok(())
    }

    fn persist_audit_usage_ledger_if_configured(&self) -> Result<(), AuditUsageLedgerError> {
        let Some(path) = self
            .audit_usage_ledger_path
            .read()
            .expect("event bus audit/usage ledger path read lock poisoned")
            .clone()
        else {
            return Ok(());
        };

        let snapshot = self.audit_usage_ledger_snapshot();
        match snapshot.persist_to_path(&path) {
            Ok(()) => {
                self.clear_audit_usage_ledger_error();
                self.mark_audit_usage_ledger_clean(Some(UtcMillis::now()));
                Ok(())
            }
            Err(error) => {
                self.record_audit_usage_ledger_error(error.to_string());
                self.mark_audit_usage_ledger_dirty();
                Err(error)
            }
        }
    }

    fn refresh_audit_usage_ledger_persistence_if_configured(&self) {
        let _ = self.persist_audit_usage_ledger_if_configured();
    }

    fn clear_audit_usage_ledger_error(&self) {
        let mut error = self
            .audit_usage_ledger_last_error
            .write()
            .expect("event bus audit/usage ledger error write lock poisoned");
        *error = None;
    }

    fn mark_audit_usage_ledger_dirty(&self) {
        let mut pending_flush = self
            .audit_usage_ledger_pending_flush
            .write()
            .expect("event bus audit/usage ledger pending flush write lock poisoned");
        *pending_flush = true;
    }

    fn mark_audit_usage_ledger_clean(&self, last_persisted_at: Option<UtcMillis>) {
        let mut pending_flush = self
            .audit_usage_ledger_pending_flush
            .write()
            .expect("event bus audit/usage ledger pending flush write lock poisoned");
        *pending_flush = false;

        let mut persisted_at = self
            .audit_usage_ledger_last_persisted_at
            .write()
            .expect("event bus audit/usage ledger last persisted at write lock poisoned");
        *persisted_at = last_persisted_at;
    }

    fn record_audit_usage_ledger_error(&self, error: String) {
        let mut last_error = self
            .audit_usage_ledger_last_error
            .write()
            .expect("event bus audit/usage ledger error write lock poisoned");
        *last_error = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EventCategory;
    use magi_core::EventId;
    use serde_json::json;
    use std::{fs, thread, time::Duration};

    fn event(category: EventCategory, event_type: &str, sequence: u64) -> EventEnvelope {
        let mut event = match category {
            EventCategory::Domain => EventEnvelope::domain(
                EventId::new(format!("e-{sequence}")),
                event_type,
                json!({"sequence": sequence}),
            ),
            EventCategory::Audit => EventEnvelope::audit(
                EventId::new(format!("e-{sequence}")),
                event_type,
                json!({"sequence": sequence}),
            ),
            EventCategory::Usage => EventEnvelope::usage(
                EventId::new(format!("e-{sequence}")),
                event_type,
                json!({"sequence": sequence}),
            ),
            EventCategory::Projection => EventEnvelope::projection(
                EventId::new(format!("e-{sequence}")),
                event_type,
                json!({"sequence": sequence}),
            ),
            EventCategory::System => EventEnvelope::system(
                EventId::new(format!("e-{sequence}")),
                event_type,
                json!({"sequence": sequence}),
            ),
        };
        event.sequence = sequence;
        event
    }

    #[test]
    fn no_live_subscriber_does_not_block_event_publish() {
        let bus = InMemoryEventBus::new(8);

        let sequence = bus
            .publish(event(EventCategory::Domain, "system.test.write", 1))
            .expect("publish should succeed without subscribers");

        assert_eq!(sequence, 1);
        assert_eq!(bus.snapshot().recent_events.len(), 1);
    }

    #[test]
    fn snapshot_and_subscribe_delivers_events_after_snapshot() {
        let bus = InMemoryEventBus::new(8);
        bus.publish(event(EventCategory::Domain, "event.before", 1))
            .expect("publish before snapshot");

        let (snapshot, mut receiver) = bus.snapshot_and_subscribe();
        bus.publish(event(EventCategory::Domain, "event.after", 2))
            .expect("publish after snapshot");

        assert_eq!(snapshot.recent_events.len(), 1);
        assert_eq!(snapshot.recent_events[0].event_type, "event.before");
        let live_event = receiver.try_recv().expect("live event should arrive");
        assert_eq!(live_event.event_type, "event.after");
    }

    #[test]
    fn snapshot_and_subscribe_receives_publish_blocked_by_snapshot_reader() {
        let bus = InMemoryEventBus::new(8);
        bus.publish(event(EventCategory::Domain, "event.before", 1))
            .expect("publish before snapshot");
        let read_guard = bus
            .recent_events
            .read()
            .expect("event bus recent events read lock should be available");
        let (snapshot, mut receiver) = bus.snapshot_and_subscribe();
        let publish_bus = bus.clone();
        let publish_thread = thread::spawn(move || {
            publish_bus
                .publish(event(EventCategory::Domain, "event.inflight", 2))
                .expect("blocked publish should complete after snapshot subscribes")
        });
        thread::sleep(Duration::from_millis(20));

        assert_eq!(snapshot.next_sequence, 2);
        assert_eq!(snapshot.recent_events.len(), 1);
        assert_eq!(snapshot.recent_events[0].event_type, "event.before");
        drop(read_guard);
        let sequence = publish_thread
            .join()
            .expect("publish thread should not panic");
        assert_eq!(sequence, 2);
        let live_event = receiver
            .try_recv()
            .expect("in-flight event must be delivered to subscriber");
        assert_eq!(live_event.event_type, "event.inflight");
        assert_eq!(live_event.sequence, 2);
    }

    #[test]
    fn 发布审计用量事件时会自动刷新账本落盘() {
        let bus = InMemoryEventBus::new(8);
        let _receiver = bus.subscribe();
        let base = std::env::temp_dir().join(format!(
            "magi-event-bus-ledger-refresh-{}-{}",
            std::process::id(),
            magi_core::UtcMillis::now().0
        ));
        fs::create_dir_all(&base).expect("create temp dir");
        let path = base.join("audit-usage-ledger.json");

        bus.set_audit_usage_ledger_persistence(path.clone());
        bus.publish(event(EventCategory::Audit, "ledger.audit.recorded", 1))
            .expect("publish audit event");
        bus.publish(event(EventCategory::Usage, "ledger.usage.recorded", 2))
            .expect("publish usage event");

        let restored = AuditUsageLedgerSnapshot::load_from_path(&path).expect("restore ledger");
        let status = bus.audit_usage_ledger_status();

        assert_eq!(restored.audit_count(), 1);
        assert_eq!(restored.usage_count(), 1);
        assert_eq!(status.audit_count, 1);
        assert_eq!(status.usage_count, 1);
        assert_eq!(status.next_sequence, 3);
        assert_eq!(status.last_persist_error, None);

        let runtime_ledger = bus.runtime_ledger_summary();
        let read_model = bus.runtime_read_model_input();

        assert!(runtime_ledger.is_persist_healthy);
        assert!(!runtime_ledger.pending_flush);
        assert!(runtime_ledger.last_persisted_at.is_some());
        assert!(runtime_ledger.readiness.is_ready);
        assert_eq!(runtime_ledger.readiness.blocking_issue_count, 0);
        assert!(runtime_ledger.cutover_readiness.is_ready);
        assert_eq!(runtime_ledger.cutover_readiness.blocking_issue_count, 0);
        assert_eq!(
            read_model.meta.ledger.is_persist_healthy,
            runtime_ledger.is_persist_healthy
        );
        assert_eq!(
            read_model.meta.ledger.pending_flush,
            runtime_ledger.pending_flush
        );
        assert_eq!(
            read_model.meta.ledger.last_persisted_at,
            runtime_ledger.last_persisted_at
        );
        assert_eq!(
            read_model.meta.ledger.readiness.is_ready,
            runtime_ledger.readiness.is_ready
        );
    }

    #[test]
    fn 账本持久化失败不应阻断事件发布但应记录错误() {
        let bus = InMemoryEventBus::new(8);
        let _receiver = bus.subscribe();
        let base = std::env::temp_dir().join(format!(
            "magi-event-bus-ledger-fail-{}-{}",
            std::process::id(),
            magi_core::UtcMillis::now().0
        ));
        fs::create_dir_all(&base).expect("create temp dir");
        let blocker = base.join("blocker");
        fs::write(&blocker, b"blocker").expect("create blocker file");
        let path = blocker.join("audit-usage-ledger.json");

        bus.set_audit_usage_ledger_persistence(path);
        bus.publish(event(EventCategory::Audit, "ledger.audit.recorded", 1))
            .expect("publish audit event");

        let status = bus.audit_usage_ledger_status();
        let runtime_ledger = bus.runtime_ledger_summary();
        let read_model = bus.runtime_read_model_input();

        assert_eq!(status.audit_count, 1);
        assert!(status.last_persist_error.is_some());
        assert!(!runtime_ledger.is_persist_healthy);
        assert!(runtime_ledger.pending_flush);
        assert!(runtime_ledger.last_persisted_at.is_none());
        assert!(!runtime_ledger.readiness.is_ready);
        assert!(
            runtime_ledger
                .readiness
                .blocking_issues
                .contains(&"ledger persistence is unhealthy".to_string())
        );
        assert!(!runtime_ledger.cutover_readiness.is_ready);
        assert!(
            runtime_ledger
                .cutover_readiness
                .blocking_issues
                .contains(&"ledger has pending flush".to_string())
        );
        assert!(
            runtime_ledger
                .cutover_readiness
                .blocking_issues
                .contains(&"ledger has not been persisted yet".to_string())
        );
        assert_eq!(
            read_model.meta.ledger.is_persist_healthy,
            runtime_ledger.is_persist_healthy
        );
        assert_eq!(
            read_model.meta.ledger.pending_flush,
            runtime_ledger.pending_flush
        );
        assert_eq!(
            read_model.meta.ledger.last_persisted_at,
            runtime_ledger.last_persisted_at
        );
        assert_eq!(
            read_model.meta.ledger.readiness.blocking_issue_count,
            runtime_ledger.readiness.blocking_issue_count
        );
        assert_eq!(
            read_model.meta.ledger.cutover_readiness.is_ready,
            runtime_ledger.cutover_readiness.is_ready
        );
        assert_eq!(
            read_model
                .meta
                .ledger
                .cutover_readiness
                .blocking_issue_count,
            runtime_ledger.cutover_readiness.blocking_issue_count
        );
    }

    #[test]
    fn runtime_read_model应反映账本持久化失败状态() {
        let bus = InMemoryEventBus::new(8);
        let _receiver = bus.subscribe();
        let base = std::env::temp_dir().join(format!(
            "magi-event-bus-runtime-ledger-fail-{}-{}",
            std::process::id(),
            magi_core::UtcMillis::now().0
        ));
        fs::create_dir_all(&base).expect("create temp dir");
        let blocker = base.join("blocker");
        fs::write(&blocker, b"blocker").expect("create blocker file");
        let path = blocker.join("audit-usage-ledger.json");

        bus.set_audit_usage_ledger_persistence(path);
        bus.publish(event(EventCategory::Usage, "ledger.usage.recorded", 1))
            .expect("publish usage event");

        let status = bus.audit_usage_ledger_status();
        let runtime_ledger = bus.runtime_ledger_summary();
        let read_model = bus.runtime_read_model_input();

        assert!(status.last_persist_error.is_some());
        assert_eq!(runtime_ledger.schema_version, status.schema_version);
        assert_eq!(runtime_ledger.audit_count, status.audit_count);
        assert_eq!(runtime_ledger.usage_count, status.usage_count);
        assert_eq!(runtime_ledger.next_sequence, status.next_sequence);
        assert_eq!(
            runtime_ledger.persistence_path,
            status
                .persistence_path
                .as_ref()
                .map(|path| path.display().to_string())
        );
        assert_eq!(runtime_ledger.last_persist_error, status.last_persist_error);
        assert_eq!(
            runtime_ledger.is_persist_healthy,
            status.last_persist_error.is_none()
        );
        assert_eq!(
            read_model.meta.ledger.schema_version,
            runtime_ledger.schema_version
        );
        assert_eq!(
            read_model.meta.ledger.audit_count,
            runtime_ledger.audit_count
        );
        assert_eq!(
            read_model.meta.ledger.usage_count,
            runtime_ledger.usage_count
        );
        assert_eq!(
            read_model.meta.ledger.next_sequence,
            runtime_ledger.next_sequence
        );
        assert_eq!(
            read_model.meta.ledger.persistence_path,
            runtime_ledger.persistence_path
        );
        assert_eq!(
            read_model.meta.ledger.last_persist_error,
            runtime_ledger.last_persist_error
        );
        assert_eq!(
            read_model.meta.ledger.is_persist_healthy,
            runtime_ledger.is_persist_healthy
        );
        assert_eq!(
            read_model.meta.ledger.pending_flush,
            runtime_ledger.pending_flush
        );
        assert_eq!(
            read_model.meta.ledger.last_persisted_at,
            runtime_ledger.last_persisted_at
        );
        assert_eq!(
            read_model.meta.ledger.readiness.is_ready,
            runtime_ledger.readiness.is_ready
        );
        assert_eq!(
            read_model.meta.ledger.readiness.blocking_issue_count,
            runtime_ledger.readiness.blocking_issue_count
        );
        assert_eq!(
            read_model.meta.ledger.cutover_readiness.is_ready,
            runtime_ledger.cutover_readiness.is_ready
        );
        assert_eq!(
            read_model
                .meta
                .ledger
                .cutover_readiness
                .blocking_issue_count,
            runtime_ledger.cutover_readiness.blocking_issue_count
        );
    }

    #[test]
    fn runtime_read_model应反映maintenance状态事件() {
        let bus = InMemoryEventBus::new(8);
        let _receiver = bus.subscribe();
        bus.publish(EventEnvelope::system(
            magi_core::EventId::new("maintenance-status-1"),
            "system.runtime.maintenance.status",
            serde_json::json!({
                "maintenance_mode": "cutover-prep",
                "policy_profile": "cutover-prep",
                "mode_reason": "cutover readiness probe",
                "last_tick_at": 123456,
                "last_sidecar_outcome": "due-and-flushed",
                "last_ledger_outcome": "due-and-refreshed",
                "tick_interval_millis": 100,
                "sidecar_flush_enabled": true,
                "ledger_refresh_enabled": true,
                "eager_flush_dirty_sidecars": true,
                "refresh_ledger_when_unhealthy": true,
                "refresh_ledger_when_never_persisted": true
            }),
        ))
        .expect("publish maintenance event");

        let read_model = bus.runtime_read_model_input();
        assert_eq!(
            read_model.meta.maintenance.maintenance_mode.as_deref(),
            Some("cutover-prep")
        );
        assert_eq!(
            read_model.meta.maintenance.policy_profile.as_deref(),
            Some("cutover-prep")
        );
        assert_eq!(
            read_model.meta.maintenance.mode_reason.as_deref(),
            Some("cutover readiness probe")
        );
        assert_eq!(
            read_model.meta.maintenance.last_tick_at,
            Some(magi_core::UtcMillis(123456))
        );
        assert_eq!(
            read_model.meta.maintenance.last_sidecar_outcome.as_deref(),
            Some("due-and-flushed")
        );
        assert_eq!(
            read_model.meta.maintenance.last_ledger_outcome.as_deref(),
            Some("due-and-refreshed")
        );
        assert_eq!(read_model.meta.maintenance.tick_interval_millis, Some(100));
        assert!(read_model.meta.maintenance.sidecar_flush_enabled);
        assert!(read_model.meta.maintenance.ledger_refresh_enabled);
        assert!(read_model.meta.maintenance.eager_flush_dirty_sidecars);
    }

    #[test]
    fn runtime_read_model应反映executor观测事件() {
        let bus = InMemoryEventBus::new(8);
        let _receiver = bus.subscribe();
        bus.publish(EventEnvelope::audit(
            magi_core::EventId::new("executor-observed-1"),
            "worker.executor.observed",
            serde_json::json!({
                "worker_id": "worker-test-1",
                "task_id": "task-test-1",
                "requested_stage": "execute",
                "request_id": "executor-request-test-1",
                "request_source": "dispatch",
                "requested_reuse_policy": "preferred",
                "requested_binding_scope": "session",
                "requested_lease_state": "requested",
                "requested_binding_lifecycle": "requested",
                "requested_process_lifecycle": "persistent",
                "requested_process_model": "persistent-process",
                "requested_parallelism": 1,
                "requested_step_kinds": ["builtin-tool-invocation", "skill-dispatch", "final-report"],
                "executor_kind": "LocalProcess",
                "observation_status": "ready",
                "executor_id": "local-process-worker-executor",
                "executor_version": "worker-local-process-executor-v2",
                "executor_instance_id": "local-process-worker-executor-instance-1",
                "executor_lease_id": "local-process-worker-executor-lease-1",
                "execution_mode": "local-process",
                "protocol_version": "worker-local-process-v1",
                "process_model": "persistent-process",
                "lease_state": "active",
                "binding_lifecycle": "bound",
                "process_lifecycle": "persistent",
                "reuse_scope": "session",
                "parallelism_scope": "executor",
                "max_parallelism": 1,
                "strict_session_affinity": false,
                "strict_workspace_affinity": false,
                "supported_step_kinds": ["builtin-tool-invocation", "skill-dispatch", "final-report"],
                "health_status": "Healthy",
                "health_detail": "loopback ready",
                "observed_at": 777777
            }),
        ))
        .expect("publish executor observation event");

        let read_model = bus.runtime_read_model_input();
        assert_eq!(read_model.overview.activity.executor_event_count, 1);
        assert_eq!(
            read_model.meta.executor.executor_kind.as_deref(),
            Some("LocalProcess")
        );
        assert_eq!(
            read_model.meta.executor.observation_status.as_deref(),
            Some("ready")
        );
        assert_eq!(
            read_model.meta.executor.execution_mode.as_deref(),
            Some("local-process")
        );
        assert_eq!(
            read_model.meta.executor.process_model.as_deref(),
            Some("persistent-process")
        );
        assert_eq!(
            read_model.meta.executor.health_status.as_deref(),
            Some("healthy")
        );
        assert!(read_model.meta.executor.is_ready);
        assert_eq!(read_model.meta.executor.blocking_issue_count, 0);
        assert!(read_model.meta.executor.is_cutover_candidate);
        assert_eq!(
            read_model.meta.executor.executor_instance_id.as_deref(),
            Some("local-process-worker-executor-instance-1")
        );
        assert_eq!(
            read_model.meta.executor.reuse_scope.as_deref(),
            Some("session")
        );
        assert_eq!(
            read_model.meta.executor.request_id.as_deref(),
            Some("executor-request-test-1")
        );
        assert_eq!(
            read_model.meta.executor.requested_binding_scope.as_deref(),
            Some("session")
        );
        assert_eq!(
            read_model.meta.executor.requested_lease_state.as_deref(),
            Some("requested")
        );
        assert_eq!(
            read_model
                .meta
                .executor
                .requested_binding_lifecycle
                .as_deref(),
            Some("requested")
        );
        assert_eq!(
            read_model
                .meta
                .executor
                .requested_process_lifecycle
                .as_deref(),
            Some("persistent")
        );
        assert_eq!(
            read_model.meta.executor.lease_state.as_deref(),
            Some("active")
        );
        assert_eq!(
            read_model.meta.executor.binding_lifecycle.as_deref(),
            Some("bound")
        );
        assert_eq!(
            read_model.meta.executor.process_lifecycle.as_deref(),
            Some("persistent")
        );
        assert_eq!(
            read_model.details.workers[0]
                .executor_requested_lease_state
                .as_deref(),
            Some("requested")
        );
        assert_eq!(
            read_model.details.workers[0]
                .executor_requested_binding_lifecycle
                .as_deref(),
            Some("requested")
        );
        assert_eq!(
            read_model.details.workers[0]
                .executor_requested_process_lifecycle
                .as_deref(),
            Some("persistent")
        );
        assert_eq!(
            read_model.details.workers[0]
                .executor_lease_state
                .as_deref(),
            Some("active")
        );
        assert_eq!(
            read_model.details.workers[0]
                .executor_binding_lifecycle
                .as_deref(),
            Some("bound")
        );
        assert_eq!(
            read_model.details.workers[0]
                .executor_process_lifecycle
                .as_deref(),
            Some("persistent")
        );
        assert_eq!(
            read_model.details.workers[0].executor_supported_step_kinds,
            vec![
                "builtin-tool-invocation".to_string(),
                "final-report".to_string(),
                "skill-dispatch".to_string(),
            ]
        );
    }

    #[test]
    fn runtime_read_model应汇总degraded与unavailable_executor信号() {
        let bus = InMemoryEventBus::new(8);
        let _receiver = bus.subscribe();
        bus.publish(EventEnvelope::audit(
            magi_core::EventId::new("executor-observed-degraded"),
            "worker.executor.observed",
            serde_json::json!({
                "worker_id": "worker-test-1",
                "task_id": "task-test-1",
                "requested_stage": "execute",
                "request_id": "executor-request-test-2",
                "request_source": "dispatch",
                "executor_kind": "LocalProcess",
                "observation_status": "degraded",
                "executor_id": "local-process-worker-executor",
                "executor_version": "worker-local-process-executor-v2",
                "executor_instance_id": "local-process-worker-executor-instance-2",
                "execution_mode": "local-process",
                "protocol_version": "worker-local-process-v1",
                "process_model": "one-shot-subprocess",
                "reuse_scope": "none",
                "parallelism_scope": "executor",
                "max_parallelism": 1,
                "strict_session_affinity": false,
                "strict_workspace_affinity": false,
                "supported_step_kinds": ["final-report"],
                "health_status": "Degraded",
                "health_detail": "executor is warming up",
                "observed_at": 777777
            }),
        ))
        .expect("publish degraded executor observation");
        bus.publish(EventEnvelope::audit(
            magi_core::EventId::new("executor-observed-unavailable"),
            "worker.executor.observed",
            serde_json::json!({
                "worker_id": "worker-test-2",
                "task_id": "task-test-2",
                "requested_stage": "execute",
                "executor_kind": "LocalProcess",
                "observation_status": "unavailable",
                "failure_layer": "transport",
                "failure_message": "spawn failed",
                "observed_at": 888888
            }),
        ))
        .expect("publish unavailable executor observation");

        let read_model = bus.runtime_read_model_input();
        assert_eq!(read_model.overview.diagnostics.degraded_executor_count, 1);
        assert_eq!(
            read_model.overview.diagnostics.unavailable_executor_count,
            1
        );
        assert_eq!(
            read_model.operations.attention.degraded_executor_worker_ids,
            vec!["worker-test-1".to_string()]
        );
        assert_eq!(
            read_model
                .operations
                .attention
                .unavailable_executor_worker_ids,
            vec!["worker-test-2".to_string()]
        );
        assert!(!read_model.meta.executor.is_ready);
        assert!(read_model.meta.executor.blocking_issue_count > 0);
    }

    #[test]
    fn one_shot_executor_ready不应自动成为cutover_candidate() {
        let bus = InMemoryEventBus::new(8);
        let _receiver = bus.subscribe();
        bus.publish(EventEnvelope::audit(
            magi_core::EventId::new("executor-observed-one-shot"),
            "worker.executor.observed",
            serde_json::json!({
                "worker_id": "worker-test-3",
                "task_id": "task-test-3",
                "requested_stage": "execute",
                "request_id": "executor-request-test-3",
                "request_source": "dispatch",
                "requested_reuse_policy": "not-required",
                "requested_binding_scope": "none",
                "requested_lease_state": "none",
                "requested_binding_lifecycle": "none",
                "requested_process_lifecycle": "one-shot",
                "requested_process_model": "one-shot-subprocess",
                "requested_parallelism": 1,
                "requested_step_kinds": ["final-report"],
                "executor_kind": "LocalProcess",
                "observation_status": "ready",
                "executor_id": "local-process-worker-executor",
                "executor_version": "worker-local-process-executor-v2",
                "execution_mode": "local-process",
                "protocol_version": "worker-local-process-v1",
                "process_model": "one-shot-subprocess",
                "lease_state": "none",
                "binding_lifecycle": "none",
                "process_lifecycle": "one-shot",
                "reuse_scope": "none",
                "parallelism_scope": "executor",
                "max_parallelism": 1,
                "strict_session_affinity": false,
                "strict_workspace_affinity": false,
                "supported_step_kinds": ["final-report"],
                "health_status": "Healthy",
                "health_detail": "ready for one-shot execution",
                "observed_at": 999999
            }),
        ))
        .expect("publish one-shot executor observation");

        let read_model = bus.runtime_read_model_input();
        assert!(read_model.meta.executor.is_ready);
        assert!(!read_model.meta.executor.is_cutover_candidate);
        assert_eq!(
            read_model
                .meta
                .executor
                .requested_process_lifecycle
                .as_deref(),
            Some("one-shot")
        );
        assert_eq!(
            read_model.meta.executor.process_lifecycle.as_deref(),
            Some("one-shot")
        );
    }

    #[test]
    fn persistent_executor_without_active_lease_and_bound_binding不应成为cutover_candidate() {
        let bus = InMemoryEventBus::new(8);
        let _receiver = bus.subscribe();
        bus.publish(EventEnvelope::audit(
            magi_core::EventId::new("executor-observed-persistent-unbound"),
            "worker.executor.observed",
            serde_json::json!({
                "worker_id": "worker-test-4",
                "task_id": "task-test-4",
                "requested_stage": "execute",
                "request_id": "executor-request-test-4",
                "request_source": "dispatch",
                "requested_reuse_policy": "required",
                "requested_binding_scope": "session",
                "requested_lease_state": "requested",
                "requested_binding_lifecycle": "requested",
                "requested_process_lifecycle": "persistent",
                "requested_process_model": "persistent-process",
                "requested_parallelism": 1,
                "requested_step_kinds": ["final-report"],
                "executor_kind": "LocalProcess",
                "observation_status": "ready",
                "executor_id": "local-process-worker-executor",
                "executor_version": "worker-local-process-executor-v2",
                "executor_instance_id": "local-process-worker-executor-instance-1",
                "executor_lease_id": "local-process-worker-executor-session-test-4-lease",
                "execution_mode": "local-process",
                "protocol_version": "worker-local-process-v1",
                "process_model": "persistent-process",
                "lease_state": "released",
                "binding_lifecycle": "released",
                "process_lifecycle": "persistent",
                "reuse_scope": "session",
                "parallelism_scope": "executor",
                "max_parallelism": 1,
                "strict_session_affinity": false,
                "strict_workspace_affinity": false,
                "supported_step_kinds": ["final-report"],
                "health_status": "Healthy",
                "health_detail": "persistent executor is releasing lease",
                "observed_at": 1111111
            }),
        ))
        .expect("publish persistent executor observation");

        let read_model = bus.runtime_read_model_input();
        assert!(!read_model.meta.executor.is_ready);
        assert!(!read_model.meta.executor.is_cutover_candidate);
        assert!(
            read_model
                .meta
                .executor
                .blocking_issues
                .iter()
                .any(|issue| issue.contains("lease is not active"))
        );
        assert!(
            read_model
                .meta
                .executor
                .blocking_issues
                .iter()
                .any(|issue| issue.contains("binding is not bound"))
        );
    }
}
