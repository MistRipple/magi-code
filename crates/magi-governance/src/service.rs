use magi_core::{ApprovalRequirement, RiskLevel};

use crate::{
    ApprovalAction, DecisionPhase, GovernanceDecision, GovernanceDecisionTrace, GovernanceTarget,
    GovernanceThresholds, PathAccessRequest, SandboxRequest, ToolExecutionRequest,
    WorkerControlKind, WorkerControlRequest,
};

#[derive(Clone, Debug)]
pub struct GovernanceService {
    thresholds: GovernanceThresholds,
}

impl Default for GovernanceService {
    fn default() -> Self {
        Self {
            thresholds: GovernanceThresholds {
                auto_allow_max_risk: RiskLevel::Medium,
                manual_approval_risk: RiskLevel::High,
            },
        }
    }
}

impl GovernanceService {
    pub fn with_thresholds(thresholds: GovernanceThresholds) -> Self {
        Self { thresholds }
    }

    pub fn thresholds(&self) -> &GovernanceThresholds {
        &self.thresholds
    }

    pub fn decide_approval_action(&self, request: &ToolExecutionRequest) -> ApprovalAction {
        match (request.risk_level, request.approval_requirement) {
            (RiskLevel::High, _) => ApprovalAction::RequiresManualApproval,
            (_, ApprovalRequirement::Required) => ApprovalAction::RequiresManualApproval,
            _ => ApprovalAction::AutoAllowed,
        }
    }

    pub fn evaluate_tool_request(&self, request: &ToolExecutionRequest) -> GovernanceDecision {
        match self.decide_approval_action(request) {
            ApprovalAction::RequiresManualApproval => GovernanceDecision::needs_approval(
                DecisionPhase::ApprovalPolicy,
                self.thresholds.manual_approval_risk,
                Some(format!(
                    "高风险工具已被当前风险策略拦截: {}",
                    request.tool_name
                )),
            ),
            ApprovalAction::AutoAllowed => GovernanceDecision::allowed(
                DecisionPhase::ToolPolicy,
                self.thresholds.auto_allow_max_risk,
                None,
            ),
            ApprovalAction::Rejected => GovernanceDecision::rejected(
                DecisionPhase::ToolPolicy,
                self.thresholds.manual_approval_risk,
                Some(format!("工具请求已被拒绝: {}", request.tool_name)),
            ),
        }
    }

    pub fn trace_tool_request(&self, request: &ToolExecutionRequest) -> GovernanceDecisionTrace {
        GovernanceDecisionTrace::new(
            GovernanceTarget::Tool {
                tool_name: request.tool_name.clone(),
                tool_kind: request.tool_kind.clone(),
            },
            self.evaluate_tool_request(request),
        )
    }

    pub fn evaluate_sandbox(&self, request: &SandboxRequest) -> GovernanceDecision {
        if request.working_directory.is_empty() {
            return GovernanceDecision::blocked(
                DecisionPhase::SandboxPolicy,
                self.thresholds.auto_allow_max_risk,
                Some("工作目录不能为空".to_string()),
            );
        }

        GovernanceDecision::allowed(
            DecisionPhase::SandboxPolicy,
            self.thresholds.auto_allow_max_risk,
            Some(format!("允许在受控目录执行命令: {}", request.command)),
        )
    }

    pub fn trace_sandbox(&self, request: &SandboxRequest) -> GovernanceDecisionTrace {
        GovernanceDecisionTrace::new(
            GovernanceTarget::Sandbox {
                command: request.command.clone(),
                working_directory: request.working_directory.clone(),
            },
            self.evaluate_sandbox(request),
        )
    }

    pub fn evaluate_path_access(&self, request: &PathAccessRequest) -> GovernanceDecision {
        if !request.absolute_path.starts_with('/') {
            return GovernanceDecision::blocked(
                DecisionPhase::ToolPolicy,
                self.thresholds.auto_allow_max_risk,
                Some("仅允许绝对路径".to_string()),
            );
        }

        GovernanceDecision::allowed(
            DecisionPhase::ToolPolicy,
            self.thresholds.auto_allow_max_risk,
            None,
        )
    }

    pub fn trace_path_access(&self, request: &PathAccessRequest) -> GovernanceDecisionTrace {
        GovernanceDecisionTrace::new(
            GovernanceTarget::PathAccess {
                absolute_path: request.absolute_path.clone(),
            },
            self.evaluate_path_access(request),
        )
    }

    pub fn evaluate_worker_control_request(
        &self,
        request: &WorkerControlRequest,
    ) -> GovernanceDecision {
        if request.blocked {
            return GovernanceDecision::blocked(
                DecisionPhase::WorkerControl,
                self.thresholds.auto_allow_max_risk,
                Some(
                    request
                        .reason
                        .clone()
                        .unwrap_or_else(|| "worker control 请求被治理阻断".to_string()),
                ),
            );
        }

        if matches!(request.action, WorkerControlKind::RepairRetry) && request.retry_count == 0 {
            return GovernanceDecision::rejected(
                DecisionPhase::WorkerControl,
                self.thresholds.auto_allow_max_risk,
                Some(
                    request
                        .reason
                        .clone()
                        .unwrap_or_else(|| "repair retry 需要至少一次前置尝试".to_string()),
                ),
            );
        }

        if request.risk_level == RiskLevel::High
            || request.approval_requirement == ApprovalRequirement::Required
        {
            return GovernanceDecision::needs_approval(
                DecisionPhase::ApprovalPolicy,
                self.thresholds.manual_approval_risk,
                Some(
                    request
                        .reason
                        .clone()
                        .unwrap_or_else(|| "worker 控制动作已被当前风险策略拦截".to_string()),
                ),
            );
        }

        GovernanceDecision::allowed(
            DecisionPhase::WorkerControl,
            self.thresholds.auto_allow_max_risk,
            request.reason.clone(),
        )
    }

    pub fn trace_worker_control_request(
        &self,
        request: &WorkerControlRequest,
    ) -> GovernanceDecisionTrace {
        GovernanceDecisionTrace::new(
            GovernanceTarget::WorkerControl {
                action: request.action.clone(),
                worker_id: request.worker_id.clone(),
                mission_id: request.mission_id.clone(),
                assignment_id: request.assignment_id.clone(),
                task_id: request.task_id.clone(),
            },
            self.evaluate_worker_control_request(request),
        )
    }
}
