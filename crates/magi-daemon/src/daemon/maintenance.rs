use super::{
    config::DaemonError,
    events::{publish_ledger_status_event, runtime_status_payload},
    persistence::RuntimeSidecarPersistence,
    types::{DaemonMaintenanceMode, DaemonMaintenancePolicyProfile, DaemonRuntimeStatus},
};
use magi_core::{EventId, UtcMillis};
use magi_event_bus::{EventEnvelope, InMemoryEventBus};
use magi_session_store::{SessionSidecarFlushMetadata, SessionStore};
use magi_workspace::{WorkspaceRecoveryFlushMetadata, WorkspaceStore};
use std::sync::{Arc, Mutex};
use tokio::time::{self, Duration};
use tracing::{info, warn};

const RUNTIME_MAINTENANCE_INTERVAL_MILLIS: u64 = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeMaintenancePolicy {
    pub(crate) profile: DaemonMaintenancePolicyProfile,
    pub(crate) tick_interval: Duration,
    pub(crate) sidecar_flush_enabled: bool,
    pub(crate) ledger_refresh_enabled: bool,
    pub(crate) eager_flush_dirty_sidecars: bool,
    pub(crate) refresh_ledger_when_unhealthy: bool,
    pub(crate) refresh_ledger_when_never_persisted: bool,
    pub(crate) force_flush_on_mode_transition: bool,
    pub(crate) force_ledger_refresh_on_shutdown: bool,
}

impl RuntimeMaintenancePolicy {
    pub(crate) fn from_profile(profile: DaemonMaintenancePolicyProfile) -> Self {
        match profile {
            DaemonMaintenancePolicyProfile::Standard => Self {
                profile,
                tick_interval: Duration::from_millis(RUNTIME_MAINTENANCE_INTERVAL_MILLIS),
                sidecar_flush_enabled: true,
                ledger_refresh_enabled: true,
                eager_flush_dirty_sidecars: false,
                refresh_ledger_when_unhealthy: false,
                refresh_ledger_when_never_persisted: false,
                force_flush_on_mode_transition: true,
                force_ledger_refresh_on_shutdown: true,
            },
            DaemonMaintenancePolicyProfile::AggressiveFlush => Self {
                profile,
                tick_interval: Duration::from_millis(150),
                sidecar_flush_enabled: true,
                ledger_refresh_enabled: true,
                eager_flush_dirty_sidecars: true,
                refresh_ledger_when_unhealthy: true,
                refresh_ledger_when_never_persisted: true,
                force_flush_on_mode_transition: true,
                force_ledger_refresh_on_shutdown: true,
            },
            DaemonMaintenancePolicyProfile::PreCutoverDrain => Self {
                profile,
                tick_interval: Duration::from_millis(100),
                sidecar_flush_enabled: true,
                ledger_refresh_enabled: true,
                eager_flush_dirty_sidecars: true,
                refresh_ledger_when_unhealthy: true,
                refresh_ledger_when_never_persisted: true,
                force_flush_on_mode_transition: true,
                force_ledger_refresh_on_shutdown: true,
            },
        }
    }

    fn runtime_mode(&self) -> DaemonMaintenanceMode {
        match self.profile {
            DaemonMaintenancePolicyProfile::Standard => DaemonMaintenanceMode::Active,
            DaemonMaintenancePolicyProfile::AggressiveFlush => {
                DaemonMaintenanceMode::AggressiveFlush
            }
            DaemonMaintenancePolicyProfile::PreCutoverDrain => DaemonMaintenanceMode::CutoverPrep,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimeMaintenanceConfig {
    pub(crate) policy: RuntimeMaintenancePolicy,
}

impl Default for RuntimeMaintenanceConfig {
    fn default() -> Self {
        let profile = DaemonMaintenancePolicyProfile::default();
        Self {
            policy: RuntimeMaintenancePolicy::from_profile(profile),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum RuntimeMaintenanceStepKind {
    #[default]
    SidecarFlush,
    LedgerRefresh,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum RuntimeMaintenanceStepOutcome {
    #[default]
    Skipped,
    DueAndFlushed,
    DueAndRefreshed,
    Failed,
}

impl RuntimeMaintenanceStepOutcome {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::DueAndFlushed => "due-and-flushed",
            Self::DueAndRefreshed => "due-and-refreshed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RuntimeMaintenanceStateSnapshot {
    mode: DaemonMaintenanceMode,
    mode_reason: Option<String>,
    shutdown_requested_at: Option<UtcMillis>,
    shutdown_completed_at: Option<UtcMillis>,
    last_tick_at: Option<UtcMillis>,
    last_sidecar_outcome: RuntimeMaintenanceStepOutcome,
    last_ledger_outcome: RuntimeMaintenanceStepOutcome,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RuntimeMaintenanceStepReport {
    kind: RuntimeMaintenanceStepKind,
    pub(crate) outcome: RuntimeMaintenanceStepOutcome,
    pub(crate) detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeMaintenanceReport {
    pub(crate) tick_at: UtcMillis,
    policy: RuntimeMaintenancePolicy,
    pub(crate) runtime_status: DaemonRuntimeStatus,
    state_before: RuntimeMaintenanceStateSnapshot,
    state_after: RuntimeMaintenanceStateSnapshot,
    pub(crate) sidecar_report: RuntimeMaintenanceStepReport,
    pub(crate) ledger_report: RuntimeMaintenanceStepReport,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RuntimeMaintenanceState {
    mode: DaemonMaintenanceMode,
    mode_reason: Option<String>,
    shutdown_requested_at: Option<UtcMillis>,
    shutdown_completed_at: Option<UtcMillis>,
    last_tick_at: Option<UtcMillis>,
    last_report: Option<RuntimeMaintenanceReport>,
}

impl RuntimeMaintenanceState {
    fn snapshot(&self) -> RuntimeMaintenanceStateSnapshot {
        let mut snapshot = RuntimeMaintenanceStateSnapshot {
            mode: self.mode.clone(),
            mode_reason: self.mode_reason.clone(),
            shutdown_requested_at: self.shutdown_requested_at,
            shutdown_completed_at: self.shutdown_completed_at,
            last_tick_at: self.last_tick_at,
            ..RuntimeMaintenanceStateSnapshot::default()
        };
        if let Some(report) = &self.last_report {
            snapshot.last_sidecar_outcome = report.sidecar_report.outcome.clone();
            snapshot.last_ledger_outcome = report.ledger_report.outcome.clone();
            snapshot.last_tick_at = Some(report.tick_at);
        }
        snapshot
    }
}

#[derive(Clone)]
pub(crate) struct RuntimeMaintenance {
    config: RuntimeMaintenanceConfig,
    event_bus: Arc<InMemoryEventBus>,
    sidecar_persistence: RuntimeSidecarPersistence,
    session_store: Arc<SessionStore>,
    workspace_store: Arc<WorkspaceStore>,
    state: Arc<Mutex<RuntimeMaintenanceState>>,
}

impl RuntimeMaintenance {
    pub(crate) fn new(
        config: RuntimeMaintenanceConfig,
        event_bus: Arc<InMemoryEventBus>,
        sidecar_persistence: RuntimeSidecarPersistence,
        session_store: Arc<SessionStore>,
        workspace_store: Arc<WorkspaceStore>,
    ) -> Self {
        let initial_mode = config.policy.runtime_mode();
        Self {
            config,
            event_bus,
            sidecar_persistence,
            session_store,
            workspace_store,
            state: Arc::new(Mutex::new(RuntimeMaintenanceState {
                mode: initial_mode,
                ..RuntimeMaintenanceState::default()
            })),
        }
    }

    pub(crate) async fn run_loop(self) {
        let mut interval = time::interval(self.config.policy.tick_interval);
        loop {
            interval.tick().await;
            match self.run_once() {
                Ok(report) => {
                    if report.runtime_status.maintenance_mode
                        == DaemonMaintenanceMode::ShutdownComplete
                    {
                        info!("影子运行时维护已完成优雅关闭前置刷新");
                        break;
                    }
                }
                Err(error) => {
                    warn!(error = %error, "影子运行时维护 tick 执行失败");
                }
            }
        }
    }

    pub(crate) fn run_once(&self) -> Result<RuntimeMaintenanceReport, DaemonError> {
        let now = UtcMillis::now();
        let state_before = self.state_snapshot();
        let force_sidecar_flush = self.force_sidecar_flush(&state_before);
        let force_ledger_refresh = self.force_ledger_refresh(&state_before);
        let sidecar_report = self.flush_sidecars_if_due(now, force_sidecar_flush)?;
        let ledger_report = self.refresh_ledger_if_needed(force_ledger_refresh)?;
        let state_after =
            self.next_state_after_tick(now, &state_before, &sidecar_report, &ledger_report);
        let runtime_status = self.runtime_status_from_snapshot(&state_after);
        let report = RuntimeMaintenanceReport {
            tick_at: now,
            policy: self.config.policy.clone(),
            runtime_status,
            state_before,
            state_after,
            sidecar_report,
            ledger_report,
        };
        self.record_state(report.clone());
        self.publish_runtime_status_event(&format!(
            "system-runtime-maintenance-status-{}",
            report.tick_at.0
        ));
        Ok(report)
    }

    pub(crate) fn publish_runtime_status_event(&self, event_id: &str) {
        let status = self.runtime_status();
        let _ = self.event_bus.publish(EventEnvelope::system(
            EventId::new(event_id),
            "system.runtime.maintenance.status",
            runtime_status_payload(&status),
        ));
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn enter_maintenance_mode(&self, reason: impl Into<String>) {
        let mut state = self
            .state
            .lock()
            .expect("runtime maintenance state mutex should not be poisoned");
        if state.mode == DaemonMaintenanceMode::ShutdownComplete {
            return;
        }
        state.mode = self.config.policy.runtime_mode();
        state.mode_reason = Some(reason.into());
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn request_graceful_shutdown(&self, reason: impl Into<String>) {
        let mut state = self
            .state
            .lock()
            .expect("runtime maintenance state mutex should not be poisoned");
        if state.mode == DaemonMaintenanceMode::ShutdownComplete {
            return;
        }
        if state.shutdown_requested_at.is_none() {
            state.shutdown_requested_at = Some(UtcMillis::now());
        }
        state.mode = DaemonMaintenanceMode::ShutdownRequested;
        state.mode_reason = Some(reason.into());
    }

    pub(crate) fn runtime_status(&self) -> DaemonRuntimeStatus {
        let snapshot = self.state_snapshot();
        self.runtime_status_from_snapshot(&snapshot)
    }

    fn sidecar_flush_due(&self, now: UtcMillis) -> bool {
        if !self.config.policy.sidecar_flush_enabled {
            return false;
        }
        let worker_runtime_snapshot_dirty =
            self.sidecar_persistence.worker_runtime_snapshot_dirty();
        if self.config.policy.eager_flush_dirty_sidecars {
            let session_metadata = self.session_store.execution_sidecar_flush_metadata();
            let workspace_metadata = self.workspace_store.recovery_sidecar_flush_metadata();
            return session_metadata.current_version != session_metadata.flushed_version
                || workspace_metadata.current_version != workspace_metadata.flushed_version
                || worker_runtime_snapshot_dirty;
        }
        if worker_runtime_snapshot_dirty {
            return true;
        }
        session_sidecar_flush_due(&self.session_store.execution_sidecar_flush_metadata(), now)
            || workspace_sidecar_flush_due(
                &self.workspace_store.recovery_sidecar_flush_metadata(),
                now,
            )
    }

    fn flush_sidecars_if_due(
        &self,
        now: UtcMillis,
        force_flush: bool,
    ) -> Result<RuntimeMaintenanceStepReport, DaemonError> {
        if !self.config.policy.sidecar_flush_enabled {
            return Ok(RuntimeMaintenanceStepReport {
                kind: RuntimeMaintenanceStepKind::SidecarFlush,
                outcome: RuntimeMaintenanceStepOutcome::Skipped,
                detail: Some("policy disabled".to_string()),
            });
        }
        if !force_flush && !self.sidecar_flush_due(now) {
            return Ok(RuntimeMaintenanceStepReport {
                kind: RuntimeMaintenanceStepKind::SidecarFlush,
                outcome: RuntimeMaintenanceStepOutcome::Skipped,
                detail: Some("not due".to_string()),
            });
        }
        match self.sidecar_persistence.flush_runtime_sidecars() {
            Ok(flushed) => Ok(RuntimeMaintenanceStepReport {
                kind: RuntimeMaintenanceStepKind::SidecarFlush,
                outcome: RuntimeMaintenanceStepOutcome::DueAndFlushed,
                detail: Some(format!(
                    "force_flush={force_flush};session_sidecars_flushed={};workspace_recovery_sidecars_flushed={};worker_runtime_snapshot_flushed={}",
                    flushed.session_sidecars_flushed,
                    flushed.workspace_recovery_sidecars_flushed,
                    flushed.worker_runtime_snapshot_flushed
                )),
            }),
            Err(error) => {
                warn!(error = %error, "运行时维护阶段刷新 sidecar 失败");
                Ok(RuntimeMaintenanceStepReport {
                    kind: RuntimeMaintenanceStepKind::SidecarFlush,
                    outcome: RuntimeMaintenanceStepOutcome::Failed,
                    detail: Some(error.to_string()),
                })
            }
        }
    }

    fn refresh_ledger_if_needed(
        &self,
        force_refresh: bool,
    ) -> Result<RuntimeMaintenanceStepReport, DaemonError> {
        if !self.config.policy.ledger_refresh_enabled {
            return Ok(RuntimeMaintenanceStepReport {
                kind: RuntimeMaintenanceStepKind::LedgerRefresh,
                outcome: RuntimeMaintenanceStepOutcome::Skipped,
                detail: Some("policy disabled".to_string()),
            });
        }
        let runtime_ledger = self.event_bus.runtime_ledger_summary();
        let should_refresh = runtime_ledger.pending_flush
            || (self.config.policy.refresh_ledger_when_unhealthy
                && (!runtime_ledger.is_persist_healthy
                    || runtime_ledger.last_persist_error.is_some()))
            || (self.config.policy.refresh_ledger_when_never_persisted
                && runtime_ledger.last_persisted_at.is_none())
            || (force_refresh
                && (!runtime_ledger.is_persist_healthy
                    || runtime_ledger.last_persist_error.is_some()
                    || runtime_ledger.pending_flush));
        if !should_refresh {
            return Ok(RuntimeMaintenanceStepReport {
                kind: RuntimeMaintenanceStepKind::LedgerRefresh,
                outcome: RuntimeMaintenanceStepOutcome::Skipped,
                detail: Some("not due".to_string()),
            });
        }
        if let Err(error) = self.event_bus.refresh_audit_usage_ledger_persistence() {
            warn!(error = %error, "运行时维护阶段刷新审计/用量账本失败");
            publish_ledger_status_event(
                &self.event_bus,
                "system-ledger-refresh-failed",
                "system.ledger.ready",
            );
            return Ok(RuntimeMaintenanceStepReport {
                kind: RuntimeMaintenanceStepKind::LedgerRefresh,
                outcome: RuntimeMaintenanceStepOutcome::Failed,
                detail: Some(error.to_string()),
            });
        }
        publish_ledger_status_event(
            &self.event_bus,
            "system-ledger-refreshed",
            "system.ledger.ready",
        );
        Ok(RuntimeMaintenanceStepReport {
            kind: RuntimeMaintenanceStepKind::LedgerRefresh,
            outcome: RuntimeMaintenanceStepOutcome::DueAndRefreshed,
            detail: Some(format!("force_refresh={force_refresh};ledger refreshed")),
        })
    }

    fn state_snapshot(&self) -> RuntimeMaintenanceStateSnapshot {
        self.state
            .lock()
            .expect("runtime maintenance state mutex should not be poisoned")
            .snapshot()
    }

    fn runtime_status_from_snapshot(
        &self,
        snapshot: &RuntimeMaintenanceStateSnapshot,
    ) -> DaemonRuntimeStatus {
        DaemonRuntimeStatus {
            maintenance_mode: snapshot.mode.clone(),
            policy_profile: self.config.policy.profile.clone(),
            mode_reason: snapshot.mode_reason.clone(),
            shutdown_requested_at: snapshot.shutdown_requested_at,
            shutdown_completed_at: snapshot.shutdown_completed_at,
            last_tick_at: snapshot.last_tick_at,
            last_sidecar_outcome: Some(snapshot.last_sidecar_outcome.as_str().to_string()),
            last_ledger_outcome: Some(snapshot.last_ledger_outcome.as_str().to_string()),
            tick_interval_millis: self.config.policy.tick_interval.as_millis() as u64,
            sidecar_flush_enabled: self.config.policy.sidecar_flush_enabled,
            ledger_refresh_enabled: self.config.policy.ledger_refresh_enabled,
            eager_flush_dirty_sidecars: self.config.policy.eager_flush_dirty_sidecars,
            refresh_ledger_when_unhealthy: self.config.policy.refresh_ledger_when_unhealthy,
            refresh_ledger_when_never_persisted: self
                .config
                .policy
                .refresh_ledger_when_never_persisted,
        }
    }

    fn force_sidecar_flush(&self, state: &RuntimeMaintenanceStateSnapshot) -> bool {
        self.config.policy.force_flush_on_mode_transition
            && matches!(
                state.mode,
                DaemonMaintenanceMode::AggressiveFlush
                    | DaemonMaintenanceMode::CutoverPrep
                    | DaemonMaintenanceMode::ShutdownRequested
            )
    }

    fn force_ledger_refresh(&self, state: &RuntimeMaintenanceStateSnapshot) -> bool {
        self.config.policy.force_ledger_refresh_on_shutdown
            && matches!(state.mode, DaemonMaintenanceMode::ShutdownRequested)
    }

    fn next_state_after_tick(
        &self,
        now: UtcMillis,
        state_before: &RuntimeMaintenanceStateSnapshot,
        sidecar_report: &RuntimeMaintenanceStepReport,
        ledger_report: &RuntimeMaintenanceStepReport,
    ) -> RuntimeMaintenanceStateSnapshot {
        let mode = if state_before.mode == DaemonMaintenanceMode::ShutdownRequested {
            DaemonMaintenanceMode::ShutdownComplete
        } else {
            state_before.mode.clone()
        };
        RuntimeMaintenanceStateSnapshot {
            mode,
            mode_reason: state_before.mode_reason.clone(),
            shutdown_requested_at: state_before.shutdown_requested_at,
            shutdown_completed_at: if state_before.mode == DaemonMaintenanceMode::ShutdownRequested
            {
                Some(now)
            } else {
                state_before.shutdown_completed_at
            },
            last_tick_at: Some(now),
            last_sidecar_outcome: sidecar_report.outcome.clone(),
            last_ledger_outcome: ledger_report.outcome.clone(),
        }
    }

    fn record_state(&self, report: RuntimeMaintenanceReport) {
        let mut state = self
            .state
            .lock()
            .expect("runtime maintenance state mutex should not be poisoned");
        state.mode = report.state_after.mode.clone();
        state.mode_reason = report.state_after.mode_reason.clone();
        state.shutdown_requested_at = report.state_after.shutdown_requested_at;
        state.shutdown_completed_at = report.state_after.shutdown_completed_at;
        state.last_tick_at = Some(report.tick_at);
        state.last_report = Some(report);
    }
}

pub(crate) fn session_sidecar_flush_due(
    metadata: &SessionSidecarFlushMetadata,
    now: UtcMillis,
) -> bool {
    if metadata.current_version == metadata.flushed_version {
        return false;
    }
    metadata.next_flush_hint.unwrap_or(now) <= now
}

pub(crate) fn workspace_sidecar_flush_due(
    metadata: &WorkspaceRecoveryFlushMetadata,
    now: UtcMillis,
) -> bool {
    if metadata.current_version == metadata.flushed_version {
        return false;
    }
    metadata.next_flush_hint.unwrap_or(now) <= now
}
