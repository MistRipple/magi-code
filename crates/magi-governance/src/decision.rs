use magi_core::RiskLevel;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecisionPhase {
    ToolPolicy,
    ApprovalPolicy,
    SandboxPolicy,
    WorkerControl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovernanceOutcome {
    Allowed,
    NeedsApproval,
    Rejected,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceDecision {
    pub outcome: GovernanceOutcome,
    pub allowed: bool,
    pub requires_approval: bool,
    pub phase: DecisionPhase,
    pub threshold: RiskLevel,
    pub reason: Option<String>,
}

impl GovernanceDecision {
    pub fn allowed(
        phase: DecisionPhase,
        threshold: RiskLevel,
        reason: Option<String>,
    ) -> Self {
        Self {
            outcome: GovernanceOutcome::Allowed,
            allowed: true,
            requires_approval: false,
            phase,
            threshold,
            reason,
        }
    }

    pub fn needs_approval(
        phase: DecisionPhase,
        threshold: RiskLevel,
        reason: Option<String>,
    ) -> Self {
        Self {
            outcome: GovernanceOutcome::NeedsApproval,
            allowed: false,
            requires_approval: true,
            phase,
            threshold,
            reason,
        }
    }

    pub fn rejected(phase: DecisionPhase, threshold: RiskLevel, reason: Option<String>) -> Self {
        Self {
            outcome: GovernanceOutcome::Rejected,
            allowed: false,
            requires_approval: false,
            phase,
            threshold,
            reason,
        }
    }

    pub fn blocked(phase: DecisionPhase, threshold: RiskLevel, reason: Option<String>) -> Self {
        Self {
            outcome: GovernanceOutcome::Blocked,
            allowed: false,
            requires_approval: false,
            phase,
            threshold,
            reason,
        }
    }

    pub fn action(&self) -> GovernanceAction {
        match self.outcome {
            GovernanceOutcome::Allowed => GovernanceAction::AutoAllowed,
            GovernanceOutcome::NeedsApproval => GovernanceAction::RequiresManualApproval,
            GovernanceOutcome::Rejected => GovernanceAction::Rejected,
            GovernanceOutcome::Blocked => GovernanceAction::Blocked,
        }
    }

    pub fn outcome_label(&self) -> &'static str {
        match self.outcome {
            GovernanceOutcome::Allowed => "allowed",
            GovernanceOutcome::NeedsApproval => "needs_approval",
            GovernanceOutcome::Rejected => "rejected",
            GovernanceOutcome::Blocked => "blocked",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceThresholds {
    pub auto_allow_max_risk: RiskLevel,
    pub manual_approval_risk: RiskLevel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalAction {
    AutoAllowed,
    RequiresManualApproval,
    Rejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovernanceAction {
    AutoAllowed,
    RequiresManualApproval,
    Rejected,
    Blocked,
}
