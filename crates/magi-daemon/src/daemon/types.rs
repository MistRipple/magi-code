use magi_core::UtcMillis;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DaemonMaintenancePolicyProfile {
    #[default]
    ShadowDefault,
    AggressiveFlush,
    PreCutoverDrain,
}

impl DaemonMaintenancePolicyProfile {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::ShadowDefault => "shadow-default",
            Self::AggressiveFlush => "aggressive-flush",
            Self::PreCutoverDrain => "cutover-prep",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DaemonMaintenanceMode {
    #[default]
    Active,
    AggressiveFlush,
    CutoverPrep,
    ShutdownRequested,
    ShutdownComplete,
}

impl DaemonMaintenanceMode {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::AggressiveFlush => "aggressive-flush",
            Self::CutoverPrep => "cutover-prep",
            Self::ShutdownRequested => "shutdown-requested",
            Self::ShutdownComplete => "shutdown-complete",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DaemonRuntimeStatus {
    pub maintenance_mode: DaemonMaintenanceMode,
    pub policy_profile: DaemonMaintenancePolicyProfile,
    pub mode_reason: Option<String>,
    pub shutdown_requested_at: Option<UtcMillis>,
    pub shutdown_completed_at: Option<UtcMillis>,
    pub last_tick_at: Option<UtcMillis>,
    pub last_sidecar_outcome: Option<String>,
    pub last_ledger_outcome: Option<String>,
    pub tick_interval_millis: u64,
    pub sidecar_flush_enabled: bool,
    pub ledger_refresh_enabled: bool,
    pub eager_flush_dirty_sidecars: bool,
    pub refresh_ledger_when_unhealthy: bool,
    pub refresh_ledger_when_never_persisted: bool,
}
