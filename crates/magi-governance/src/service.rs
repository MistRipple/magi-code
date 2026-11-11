use magi_core::{ApprovalRequirement, RiskLevel};

use crate::{DecisionPhase, GovernanceDecision, ToolExecutionRequest};

/// 工具风险与显式审批要求的公共判定。目录、命令与代理准入由各自正式入口负责。
#[derive(Clone, Debug, Default)]
pub struct GovernanceService {}

impl GovernanceService {
    pub fn evaluate_tool_request(&self, request: &ToolExecutionRequest) -> GovernanceDecision {
        if request.risk_level == RiskLevel::High
            || request.approval_requirement == ApprovalRequirement::Required
        {
            GovernanceDecision::needs_approval(
                DecisionPhase::ApprovalPolicy,
                RiskLevel::High,
                Some(format!(
                    "高风险工具已被当前风险策略拦截: {}",
                    request.tool_name
                )),
            )
        } else {
            GovernanceDecision::allowed(DecisionPhase::ToolPolicy, RiskLevel::Medium, None)
        }
    }
}
