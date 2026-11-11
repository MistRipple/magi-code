use magi_core::{ApprovalRequirement, RiskLevel};

use crate::{GovernanceOutcome, GovernanceService, ToolExecutionRequest, ToolKind};

#[test]
fn tool_risk_requires_approval_before_execution() {
    let service = GovernanceService::default();

    let tool_allowed = service.evaluate_tool_request(&ToolExecutionRequest {
        tool_name: "file_read".to_string(),
        tool_kind: ToolKind::Builtin,
        risk_level: RiskLevel::Low,
        approval_requirement: ApprovalRequirement::None,
    });
    assert_eq!(tool_allowed.outcome, GovernanceOutcome::Allowed);
    assert!(tool_allowed.allowed);

    let tool_needs_approval = service.evaluate_tool_request(&ToolExecutionRequest {
        tool_name: "shell_exec".to_string(),
        tool_kind: ToolKind::Builtin,
        risk_level: RiskLevel::High,
        approval_requirement: ApprovalRequirement::None,
    });
    assert_eq!(
        tool_needs_approval.outcome,
        GovernanceOutcome::NeedsApproval
    );
    assert!(tool_needs_approval.requires_approval);
}
