use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorTerminationReason {
    Completed,
    Failed,
    Stalled,
    BudgetExceeded,
    MaxRoundsReached,
    UpstreamModelError,
    ExternalWaitTimeout,
    GovernancePause,
    UserCancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryDecisionAction {
    None,
    AutoRepair,
    AutoRepairStalledNotice,
    AutoGovernanceResume,
    Pause,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplanSource {
    DeliveryFailed,
    AskFollowupPending,
    BudgetPressure,
    ScopeExpansion,
    AcceptanceFailure,
    BlockerPressure,
    ProgressStalled,
}

#[derive(Clone, Debug, Default)]
pub struct RecoveryRuntimeSnapshot {
    pub review_accepted: usize,
    pub review_total: usize,
    pub open_blockers: usize,
    pub external_wait_open: usize,
    pub max_external_wait_age_ms: u64,
    pub token_limit: Option<u64>,
    pub usage_ratio: Option<f64>,
    pub warning_level: Option<String>,
    pub error_rate: Option<f64>,
    pub required_total: usize,
    pub failed_required: usize,
    pub running_or_pending_required: usize,
}

#[derive(Clone, Debug)]
pub struct RecoveryAuditIssue {
    pub dimension: String,
    pub detail: String,
    pub level: String,
}

#[derive(Clone, Debug)]
pub struct ReplanGateSignals {
    pub budget_pressure: bool,
    pub scope_expansion: bool,
    pub scope_issues: Vec<String>,
    pub acceptance_failure: bool,
    pub blocker_pressure: bool,
    pub progress_stalled: bool,
    pub pending_required_tasks: usize,
    pub failed_required_tasks: usize,
    pub unresolved_blockers: usize,
    pub external_wait_open: usize,
}

#[derive(Clone, Debug)]
pub struct RecoveryDecisionResult {
    pub action: RecoveryDecisionAction,
    pub replan_source: Option<ReplanSource>,
    pub rationale: Vec<String>,
}

pub fn is_governance_auto_recover_reason(reason: OrchestratorTerminationReason) -> bool {
    matches!(
        reason,
        OrchestratorTerminationReason::UpstreamModelError
            | OrchestratorTerminationReason::ExternalWaitTimeout
            | OrchestratorTerminationReason::GovernancePause
    )
}

pub fn derive_replan_gate_signals(
    runtime_reason: Option<OrchestratorTerminationReason>,
    snapshot: &RecoveryRuntimeSnapshot,
    audit_issues: &[RecoveryAuditIssue],
) -> ReplanGateSignals {
    let budget_pressure_by_reason =
        runtime_reason == Some(OrchestratorTerminationReason::BudgetExceeded);
    let budget_pressure_by_usage = snapshot.usage_ratio.map_or(false, |r| r >= 0.95);
    let budget_pressure_by_level = snapshot.warning_level.as_deref() == Some("danger");
    let budget_pressure_by_error_rate = snapshot.error_rate.map_or(false, |r| r >= 0.5);
    let budget_pressure = budget_pressure_by_reason
        || budget_pressure_by_usage
        || budget_pressure_by_level
        || budget_pressure_by_error_rate;

    let scope_issues: Vec<String> = audit_issues
        .iter()
        .filter(|i| i.dimension == "scope")
        .map(|i| i.detail.trim().to_string())
        .filter(|d| !d.is_empty())
        .collect();

    let acceptance_failure = snapshot.failed_required > 0
        || (snapshot.review_total > 0
            && snapshot.review_accepted < snapshot.review_total
            && runtime_reason == Some(OrchestratorTerminationReason::Failed));

    let blocker_pressure = snapshot.open_blockers > 0
        || snapshot.external_wait_open > 0
        || runtime_reason == Some(OrchestratorTerminationReason::ExternalWaitTimeout);

    let progress_stalled = runtime_reason == Some(OrchestratorTerminationReason::Stalled)
        || (snapshot.running_or_pending_required > 0 && snapshot.open_blockers > 0);

    ReplanGateSignals {
        budget_pressure,
        scope_expansion: !scope_issues.is_empty(),
        scope_issues,
        acceptance_failure,
        blocker_pressure,
        progress_stalled,
        pending_required_tasks: snapshot.running_or_pending_required,
        failed_required_tasks: snapshot.failed_required,
        unresolved_blockers: snapshot.open_blockers,
        external_wait_open: snapshot.external_wait_open,
    }
}

pub struct DeliveryRecoveryInput {
    pub allow_auto_governance_resume: bool,
    pub is_governance_paused: bool,
    pub governance_reason: Option<OrchestratorTerminationReason>,
    pub governance_recovery_attempt: usize,
    pub governance_recovery_max_rounds: usize,
    pub delivery_failed: bool,
    pub continuation_policy_auto: bool,
    pub can_auto_repair_by_rounds: bool,
    pub auto_repair_stalled: bool,
}

pub fn decide_delivery_recovery(input: &DeliveryRecoveryInput) -> RecoveryDecisionResult {
    decide_recovery_action(
        input.delivery_failed,
        input.continuation_policy_auto,
        input.can_auto_repair_by_rounds,
        input.auto_repair_stalled,
        input.allow_auto_governance_resume,
        input.is_governance_paused,
        input.governance_reason,
        input.governance_recovery_attempt,
        input.governance_recovery_max_rounds,
    )
}

pub struct GovernanceRecoveryInput {
    pub allow_auto_governance_resume: bool,
    pub is_governance_paused: bool,
    pub governance_reason: Option<OrchestratorTerminationReason>,
    pub governance_recovery_attempt: usize,
    pub governance_recovery_max_rounds: usize,
}

pub fn decide_governance_recovery(input: &GovernanceRecoveryInput) -> RecoveryDecisionResult {
    decide_recovery_action(
        false,
        false,
        false,
        false,
        input.allow_auto_governance_resume,
        input.is_governance_paused,
        input.governance_reason,
        input.governance_recovery_attempt,
        input.governance_recovery_max_rounds,
    )
}

fn decide_recovery_action(
    delivery_failed: bool,
    continuation_policy_auto: bool,
    can_auto_repair_by_rounds: bool,
    auto_repair_stalled: bool,
    allow_auto_governance_resume: bool,
    is_governance_paused: bool,
    governance_reason: Option<OrchestratorTerminationReason>,
    governance_recovery_attempt: usize,
    governance_recovery_max_rounds: usize,
) -> RecoveryDecisionResult {
    let mut rationale = Vec::new();

    if delivery_failed
        && continuation_policy_auto
        && can_auto_repair_by_rounds
        && !auto_repair_stalled
        && !is_governance_paused
    {
        rationale.push("delivery_failed:auto_repair".to_string());
        return RecoveryDecisionResult {
            action: RecoveryDecisionAction::AutoRepair,
            replan_source: None,
            rationale,
        };
    }

    if delivery_failed && continuation_policy_auto && auto_repair_stalled {
        rationale.push("delivery_failed:auto_repair_stalled".to_string());
        return RecoveryDecisionResult {
            action: RecoveryDecisionAction::AutoRepairStalledNotice,
            replan_source: None,
            rationale,
        };
    }

    if is_governance_paused
        && allow_auto_governance_resume
        && governance_reason.map_or(false, is_governance_auto_recover_reason)
        && governance_recovery_attempt < governance_recovery_max_rounds
    {
        rationale.push("governance:auto_resume".to_string());
        return RecoveryDecisionResult {
            action: RecoveryDecisionAction::AutoGovernanceResume,
            replan_source: None,
            rationale,
        };
    }

    if is_governance_paused {
        rationale.push("governance:pause".to_string());
        return RecoveryDecisionResult {
            action: RecoveryDecisionAction::Pause,
            replan_source: None,
            rationale,
        };
    }

    RecoveryDecisionResult {
        action: RecoveryDecisionAction::None,
        replan_source: None,
        rationale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_failed_auto_repair() {
        let result = decide_delivery_recovery(&DeliveryRecoveryInput {
            allow_auto_governance_resume: false,
            is_governance_paused: false,
            governance_reason: None,
            governance_recovery_attempt: 0,
            governance_recovery_max_rounds: 3,
            delivery_failed: true,
            continuation_policy_auto: true,
            can_auto_repair_by_rounds: true,
            auto_repair_stalled: false,
        });
        assert_eq!(result.action, RecoveryDecisionAction::AutoRepair);
    }

    #[test]
    fn delivery_failed_stalled_notice() {
        let result = decide_delivery_recovery(&DeliveryRecoveryInput {
            allow_auto_governance_resume: false,
            is_governance_paused: false,
            governance_reason: None,
            governance_recovery_attempt: 0,
            governance_recovery_max_rounds: 3,
            delivery_failed: true,
            continuation_policy_auto: true,
            can_auto_repair_by_rounds: true,
            auto_repair_stalled: true,
        });
        assert_eq!(result.action, RecoveryDecisionAction::AutoRepairStalledNotice);
    }

    #[test]
    fn governance_auto_resume() {
        let result = decide_governance_recovery(&GovernanceRecoveryInput {
            allow_auto_governance_resume: true,
            is_governance_paused: true,
            governance_reason: Some(OrchestratorTerminationReason::UpstreamModelError),
            governance_recovery_attempt: 0,
            governance_recovery_max_rounds: 3,
        });
        assert_eq!(result.action, RecoveryDecisionAction::AutoGovernanceResume);
    }

    #[test]
    fn governance_pause_when_no_auto_resume() {
        let result = decide_governance_recovery(&GovernanceRecoveryInput {
            allow_auto_governance_resume: false,
            is_governance_paused: true,
            governance_reason: Some(OrchestratorTerminationReason::UpstreamModelError),
            governance_recovery_attempt: 0,
            governance_recovery_max_rounds: 3,
        });
        assert_eq!(result.action, RecoveryDecisionAction::Pause);
    }

    #[test]
    fn governance_pause_when_max_rounds_reached() {
        let result = decide_governance_recovery(&GovernanceRecoveryInput {
            allow_auto_governance_resume: true,
            is_governance_paused: true,
            governance_reason: Some(OrchestratorTerminationReason::UpstreamModelError),
            governance_recovery_attempt: 3,
            governance_recovery_max_rounds: 3,
        });
        assert_eq!(result.action, RecoveryDecisionAction::Pause);
    }

    #[test]
    fn no_action_when_nothing_wrong() {
        let result = decide_delivery_recovery(&DeliveryRecoveryInput {
            allow_auto_governance_resume: false,
            is_governance_paused: false,
            governance_reason: None,
            governance_recovery_attempt: 0,
            governance_recovery_max_rounds: 3,
            delivery_failed: false,
            continuation_policy_auto: false,
            can_auto_repair_by_rounds: false,
            auto_repair_stalled: false,
        });
        assert_eq!(result.action, RecoveryDecisionAction::None);
    }

    #[test]
    fn replan_gate_signals_budget_pressure() {
        let snapshot = RecoveryRuntimeSnapshot {
            usage_ratio: Some(0.96),
            ..Default::default()
        };
        let signals = derive_replan_gate_signals(None, &snapshot, &[]);
        assert!(signals.budget_pressure);
    }

    #[test]
    fn replan_gate_signals_scope_expansion() {
        let snapshot = RecoveryRuntimeSnapshot::default();
        let issues = vec![RecoveryAuditIssue {
            dimension: "scope".to_string(),
            detail: "超出范围".to_string(),
            level: "warning".to_string(),
        }];
        let signals = derive_replan_gate_signals(None, &snapshot, &issues);
        assert!(signals.scope_expansion);
        assert_eq!(signals.scope_issues.len(), 1);
    }

    #[test]
    fn replan_gate_signals_blocker_pressure() {
        let snapshot = RecoveryRuntimeSnapshot {
            open_blockers: 2,
            ..Default::default()
        };
        let signals = derive_replan_gate_signals(None, &snapshot, &[]);
        assert!(signals.blocker_pressure);
    }

    #[test]
    fn is_governance_auto_recover_reasons() {
        assert!(is_governance_auto_recover_reason(
            OrchestratorTerminationReason::UpstreamModelError
        ));
        assert!(is_governance_auto_recover_reason(
            OrchestratorTerminationReason::ExternalWaitTimeout
        ));
        assert!(is_governance_auto_recover_reason(
            OrchestratorTerminationReason::GovernancePause
        ));
        assert!(!is_governance_auto_recover_reason(
            OrchestratorTerminationReason::Completed
        ));
        assert!(!is_governance_auto_recover_reason(
            OrchestratorTerminationReason::Stalled
        ));
    }
}
