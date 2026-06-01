use crate::{
    SkillDispatchError, SkillDispatchExecutionOutcome, SkillDispatchInput, SkillDispatchResult,
    SkillDispatchRoute, SkillDispatchRuntime, SkillToolRuntimePlan,
    bridge_binding_allowed_in_access_profile,
    observation::build_dispatch_observation,
    routing::{resolve_bridge_binding_id, resolve_observation_binding, resolve_route},
};
use magi_bridge_client::{
    BridgeBindingKind, BridgeBindingReference, BridgeClientError, BridgeDispatchInput,
    BridgeDispatchResult,
};
use magi_core::{ApprovalRequirement, ExecutionResultStatus, RiskLevel};
use magi_governance::{DecisionPhase, GovernanceDecision, ToolExecutionRequest, ToolKind};
use magi_tool_runtime::{ToolExecutionContext, ToolExecutionInput, ToolExecutionOutput};
use std::path::PathBuf;

pub(crate) fn execute_dispatch(
    runtime: &SkillDispatchRuntime,
    plan: &SkillToolRuntimePlan,
    input: SkillDispatchInput,
) -> Result<SkillDispatchResult, SkillDispatchError> {
    match resolve_route(plan, &input)? {
        SkillDispatchRoute::Builtin => execute_builtin_dispatch(runtime, plan, input),
        SkillDispatchRoute::Bridge => execute_bridge_dispatch(runtime, plan, input),
    }
}

pub(crate) fn dispatch_observed(
    runtime: &SkillDispatchRuntime,
    plan: &SkillToolRuntimePlan,
    input: SkillDispatchInput,
) -> SkillDispatchExecutionOutcome {
    let route = resolve_route(plan, &input).ok();
    let result = execute_dispatch(runtime, plan, input.clone());
    let binding = resolve_observation_binding(plan, &input);
    let observation = build_dispatch_observation(&input, binding, route, result.as_ref());
    SkillDispatchExecutionOutcome {
        observation,
        result,
    }
}

fn execute_builtin_dispatch(
    runtime: &SkillDispatchRuntime,
    plan: &SkillToolRuntimePlan,
    input: SkillDispatchInput,
) -> Result<SkillDispatchResult, SkillDispatchError> {
    let context = builtin_context_for_dispatch(input.context, input.working_directory.as_deref());
    Ok(SkillDispatchResult::Builtin {
        output: runtime.tool_registry.execute_with_policy(
            ToolExecutionInput::for_builtin_invocation(
                input.tool_call_id,
                input.tool_name,
                input.payload,
            ),
            context,
            &plan.tool_policy,
        ),
    })
}

fn builtin_context_for_dispatch(
    mut context: ToolExecutionContext,
    working_directory: Option<&str>,
) -> ToolExecutionContext {
    if let Some(path) = working_directory
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        context.working_directory = Some(path);
    }
    context
}

fn execute_bridge_dispatch(
    runtime: &SkillDispatchRuntime,
    plan: &SkillToolRuntimePlan,
    input: SkillDispatchInput,
) -> Result<SkillDispatchResult, SkillDispatchError> {
    let binding_id = resolve_bridge_binding_id(plan, &input)?;
    let binding = plan
        .bridge_dispatch_plan
        .bindings
        .iter()
        .find(|binding| binding.binding_id == binding_id)
        .ok_or_else(|| SkillDispatchError::MissingBridgeBinding {
            tool_name: input.tool_name.clone(),
            binding_id: binding_id.clone(),
        })?;
    let external_input = external_tool_execution_input(&input, binding);
    if let Some(output) = bridge_preflight_output(runtime, plan, &external_input, binding) {
        runtime
            .tool_registry
            .record_external_invocation(&external_input, &input.context, &output);
        return Ok(SkillDispatchResult::Preflight { output });
    }

    let output = runtime
        .bridge_runtime
        .dispatch(
            &plan.bridge_dispatch_plan,
            BridgeDispatchInput {
                binding_id,
                payload: input.payload,
                working_directory: input.working_directory,
            },
        )
        .map_err(|error| {
            let output = bridge_error_output(&external_input, binding, &error);
            runtime.tool_registry.record_external_invocation(
                &external_input,
                &input.context,
                &output,
            );
            SkillDispatchError::Bridge(error)
        })?;
    runtime.tool_registry.record_external_invocation(
        &external_input,
        &input.context,
        &bridge_success_output(&external_input, &output),
    );
    Ok(SkillDispatchResult::Bridge { output })
}

fn external_tool_execution_input(
    input: &SkillDispatchInput,
    binding: &BridgeBindingReference,
) -> ToolExecutionInput {
    let (default_risk_level, default_approval_requirement) =
        external_tool_invocation_policy(binding);
    ToolExecutionInput {
        tool_call_id: input.tool_call_id.clone(),
        tool_name: binding.tool_name.clone(),
        tool_kind: external_tool_kind(binding),
        input: input.payload.clone(),
        approval_requirement: stricter_approval_requirement(
            input.approval_requirement,
            default_approval_requirement,
        ),
        risk_level: stricter_risk_level(input.risk_level, default_risk_level),
    }
}

fn external_tool_kind(binding: &BridgeBindingReference) -> ToolKind {
    match binding.bridge_kind {
        BridgeBindingKind::Mcp => ToolKind::Mcp,
        BridgeBindingKind::Model => ToolKind::SkillBound,
    }
}

fn external_tool_invocation_policy(
    binding: &BridgeBindingReference,
) -> (RiskLevel, ApprovalRequirement) {
    match binding.bridge_kind {
        BridgeBindingKind::Mcp => (RiskLevel::High, ApprovalRequirement::Required),
        BridgeBindingKind::Model => (RiskLevel::Low, ApprovalRequirement::None),
    }
}

fn stricter_approval_requirement(
    left: ApprovalRequirement,
    right: ApprovalRequirement,
) -> ApprovalRequirement {
    if left == ApprovalRequirement::Required || right == ApprovalRequirement::Required {
        ApprovalRequirement::Required
    } else {
        ApprovalRequirement::None
    }
}

fn stricter_risk_level(left: RiskLevel, right: RiskLevel) -> RiskLevel {
    match (left, right) {
        (RiskLevel::High, _) | (_, RiskLevel::High) => RiskLevel::High,
        (RiskLevel::Medium, _) | (_, RiskLevel::Medium) => RiskLevel::Medium,
        _ => RiskLevel::Low,
    }
}

fn bridge_preflight_output(
    runtime: &SkillDispatchRuntime,
    plan: &SkillToolRuntimePlan,
    input: &ToolExecutionInput,
    binding: &BridgeBindingReference,
) -> Option<ToolExecutionOutput> {
    if !bridge_binding_allowed_in_access_profile(
        binding.bridge_kind,
        plan.tool_policy.access_profile,
    ) {
        let reason = format!("只读访问模式不允许调用 MCP 外接工具: {}", binding.tool_name);
        return Some(ToolExecutionOutput {
            tool_call_id: input.tool_call_id.clone(),
            status: ExecutionResultStatus::Rejected,
            payload: bridge_policy_payload(
                input,
                binding,
                ExecutionResultStatus::Rejected,
                reason.clone(),
            ),
            governance: GovernanceDecision::rejected(
                DecisionPhase::ToolPolicy,
                input.risk_level,
                Some(reason),
            ),
        });
    }

    let governance = runtime.tool_registry.governance_decision_for_tool_request(
        &ToolExecutionRequest {
            tool_name: input.tool_name.clone(),
            tool_kind: input.tool_kind.clone(),
            risk_level: input.risk_level,
            approval_requirement: input.approval_requirement,
        },
        plan.tool_policy.access_profile,
    );
    if governance.allowed {
        return None;
    }

    let status = if governance.requires_approval {
        ExecutionResultStatus::NeedsApproval
    } else {
        ExecutionResultStatus::Rejected
    };
    Some(ToolExecutionOutput {
        tool_call_id: input.tool_call_id.clone(),
        status,
        payload: bridge_policy_payload(
            input,
            binding,
            status,
            governance
                .reason
                .clone()
                .unwrap_or_else(|| "外接工具调用被治理策略阻断".to_string()),
        ),
        governance,
    })
}

fn bridge_success_output(
    input: &ToolExecutionInput,
    output: &BridgeDispatchResult,
) -> ToolExecutionOutput {
    let status = if output.response.ok {
        ExecutionResultStatus::Succeeded
    } else {
        ExecutionResultStatus::Failed
    };
    ToolExecutionOutput {
        tool_call_id: input.tool_call_id.clone(),
        status,
        payload: output.response.payload.clone(),
        governance: GovernanceDecision::allowed(
            DecisionPhase::ToolPolicy,
            input.risk_level,
            Some("外接工具调用已通过治理策略".to_string()),
        ),
    }
}

fn bridge_error_output(
    input: &ToolExecutionInput,
    binding: &BridgeBindingReference,
    error: &BridgeClientError,
) -> ToolExecutionOutput {
    let status = match error {
        BridgeClientError::InvalidBindingTarget { .. }
        | BridgeClientError::IncompatibleBindingAction { .. }
        | BridgeClientError::MissingClient { .. }
        | BridgeClientError::MissingBinding { .. }
        | BridgeClientError::MissingWorkingDirectory { .. } => ExecutionResultStatus::Rejected,
        BridgeClientError::CallFailed { .. } => ExecutionResultStatus::Failed,
    };
    ToolExecutionOutput {
        tool_call_id: input.tool_call_id.clone(),
        status,
        payload: bridge_policy_payload(input, binding, status, error.to_string()),
        governance: GovernanceDecision::allowed(
            DecisionPhase::ToolPolicy,
            input.risk_level,
            Some("外接工具调用已通过治理策略".to_string()),
        ),
    }
}

fn bridge_policy_payload(
    input: &ToolExecutionInput,
    binding: &BridgeBindingReference,
    status: ExecutionResultStatus,
    message: String,
) -> String {
    serde_json::json!({
        "tool": &input.tool_name,
        "status": match status {
            ExecutionResultStatus::Succeeded => "succeeded",
            ExecutionResultStatus::Failed => "failed",
            ExecutionResultStatus::Rejected => "rejected",
            ExecutionResultStatus::NeedsApproval => "needs_approval",
            ExecutionResultStatus::Cancelled => "cancelled",
        },
        "error": message,
        "bridge_kind": format!("{:?}", binding.bridge_kind),
        "dispatch_action": format!("{:?}", binding.dispatch_action),
        "binding_id": &binding.binding_id,
        "bridge_target": &binding.bridge_target,
        "risk_level": format!("{:?}", input.risk_level),
        "approval_requirement": format!("{:?}", input.approval_requirement),
    })
    .to_string()
}
