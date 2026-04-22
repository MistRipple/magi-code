use serde::{Deserialize, Serialize};

use super::entry_router::PlanMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRequirement {
    None,
    Recommended,
    Required,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeGovernanceSummary {
    pub mode: PlanMode,
    pub phase: String,
    pub dispatch_allowed: bool,
    pub phase_reason: String,
    pub review_state: String,
    pub verification_summary: String,
    pub wait_state: String,
    pub replan_state: String,
    pub approval_requirement: ApprovalRequirement,
    pub blocked_assignments: usize,
    pub awaiting_approval_assignments: usize,
    pub review_required_assignments: usize,
}

pub struct GovernanceInput {
    pub mission_phase: Option<String>,
    pub plan_mode: Option<PlanMode>,
    pub plan_review_state: Option<String>,
    pub plan_acceptance_summary: Option<String>,
    pub plan_wait_state: Option<String>,
    pub plan_replan_state: Option<String>,
    pub assignments_blocked: usize,
    pub assignments_awaiting_approval: usize,
    pub assignments_review_required: usize,
    pub risk_score: f64,
}

pub struct RuntimeGovernanceControlPlane;

impl RuntimeGovernanceControlPlane {
    pub fn new() -> Self {
        Self
    }

    pub fn build_summary(&self, input: &GovernanceInput) -> RuntimeGovernanceSummary {
        let phase = input
            .mission_phase
            .clone()
            .unwrap_or_else(|| "idle".to_string());

        let (dispatch_allowed, phase_reason) = resolve_dispatch_permission(&phase);

        let approval_requirement = resolve_approval_requirement(input.risk_score);

        RuntimeGovernanceSummary {
            mode: input.plan_mode.unwrap_or(PlanMode::Standard),
            phase,
            dispatch_allowed,
            phase_reason,
            review_state: input
                .plan_review_state
                .clone()
                .unwrap_or_else(|| "idle".to_string()),
            verification_summary: input
                .plan_acceptance_summary
                .clone()
                .unwrap_or_else(|| "pending".to_string()),
            wait_state: input
                .plan_wait_state
                .clone()
                .unwrap_or_else(|| "none".to_string()),
            replan_state: input
                .plan_replan_state
                .clone()
                .unwrap_or_else(|| "none".to_string()),
            approval_requirement,
            blocked_assignments: input.assignments_blocked,
            awaiting_approval_assignments: input.assignments_awaiting_approval,
            review_required_assignments: input.assignments_review_required,
        }
    }
}

impl Default for RuntimeGovernanceControlPlane {
    fn default() -> Self {
        Self::new()
    }
}

fn resolve_dispatch_permission(phase: &str) -> (bool, String) {
    match phase {
        "planning" | "dispatching" | "executing" => (
            true,
            format!("阶段 {phase} 允许继续派发"),
        ),
        "reviewing" | "verifying" => (
            false,
            format!("阶段 {phase} 暂不允许新派发"),
        ),
        "completed" | "failed" | "cancelled" => (
            false,
            format!("Mission 已处于终态 {phase}"),
        ),
        "idle" => (
            false,
            "当前没有活跃 Mission，控制面不允许继续派发。".to_string(),
        ),
        _ => (
            false,
            format!("未知阶段 {phase}，默认禁止派发"),
        ),
    }
}

fn resolve_approval_requirement(risk_score: f64) -> ApprovalRequirement {
    if risk_score >= 0.8 {
        ApprovalRequirement::Required
    } else if risk_score >= 0.4 {
        ApprovalRequirement::Recommended
    } else {
        ApprovalRequirement::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_input() -> GovernanceInput {
        GovernanceInput {
            mission_phase: Some("executing".to_string()),
            plan_mode: Some(PlanMode::Standard),
            plan_review_state: Some("idle".to_string()),
            plan_acceptance_summary: Some("pending".to_string()),
            plan_wait_state: None,
            plan_replan_state: None,
            assignments_blocked: 0,
            assignments_awaiting_approval: 0,
            assignments_review_required: 0,
            risk_score: 0.2,
        }
    }

    #[test]
    fn executing_phase_allows_dispatch() {
        let plane = RuntimeGovernanceControlPlane::new();
        let summary = plane.build_summary(&make_input());
        assert!(summary.dispatch_allowed);
        assert_eq!(summary.approval_requirement, ApprovalRequirement::None);
    }

    #[test]
    fn idle_phase_blocks_dispatch() {
        let plane = RuntimeGovernanceControlPlane::new();
        let mut input = make_input();
        input.mission_phase = Some("idle".to_string());
        let summary = plane.build_summary(&input);
        assert!(!summary.dispatch_allowed);
    }

    #[test]
    fn completed_phase_blocks_dispatch() {
        let plane = RuntimeGovernanceControlPlane::new();
        let mut input = make_input();
        input.mission_phase = Some("completed".to_string());
        let summary = plane.build_summary(&input);
        assert!(!summary.dispatch_allowed);
    }

    #[test]
    fn high_risk_requires_approval() {
        let plane = RuntimeGovernanceControlPlane::new();
        let mut input = make_input();
        input.risk_score = 0.9;
        let summary = plane.build_summary(&input);
        assert_eq!(summary.approval_requirement, ApprovalRequirement::Required);
    }

    #[test]
    fn medium_risk_recommends_approval() {
        let plane = RuntimeGovernanceControlPlane::new();
        let mut input = make_input();
        input.risk_score = 0.5;
        let summary = plane.build_summary(&input);
        assert_eq!(
            summary.approval_requirement,
            ApprovalRequirement::Recommended
        );
    }

    #[test]
    fn defaults_applied_when_no_plan() {
        let plane = RuntimeGovernanceControlPlane::new();
        let input = GovernanceInput {
            mission_phase: None,
            plan_mode: None,
            plan_review_state: None,
            plan_acceptance_summary: None,
            plan_wait_state: None,
            plan_replan_state: None,
            assignments_blocked: 0,
            assignments_awaiting_approval: 0,
            assignments_review_required: 0,
            risk_score: 0.0,
        };
        let summary = plane.build_summary(&input);
        assert_eq!(summary.phase, "idle");
        assert_eq!(summary.mode, PlanMode::Standard);
        assert_eq!(summary.review_state, "idle");
    }
}
