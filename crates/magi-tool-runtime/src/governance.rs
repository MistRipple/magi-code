use crate::builtin::infer_execution_status;
use crate::json_support::{field_string, parse_json_object};
use crate::{
    BuiltinToolAccessMode, BuiltinToolName, ToolExecutionInput, ToolExecutionOutput,
    ToolExecutionPolicy, ToolRegistry,
};
use magi_core::ExecutionResultStatus;
use magi_governance::{DecisionPhase, GovernanceDecision, GovernanceOutcome};

impl ToolRegistry {
    pub(crate) fn enforce_execution_policy(
        &self,
        input: &ToolExecutionInput,
        policy: &ToolExecutionPolicy,
    ) -> Option<ToolExecutionOutput> {
        let policy = normalize_execution_policy(policy);
        if policy.source_skill_ids.is_empty()
            && policy.allowed_tool_names.is_empty()
            && policy.denied_tool_names.is_empty()
        {
            return None;
        }
        if !policy.denied_tool_names.is_empty()
            && policy
                .denied_tool_names
                .iter()
                .any(|tool_name| tool_name == &input.tool_name)
        {
            return Some(self.build_policy_rejection(
                input,
                &policy,
                format!("skill runtime 已显式拒绝工具: {}", input.tool_name),
            ));
        }

        if !policy.allowed_tool_names.is_empty()
            && !policy
                .allowed_tool_names
                .iter()
                .any(|tool_name| tool_name == &input.tool_name)
        {
            return Some(self.build_policy_rejection(
                input,
                &policy,
                format!("skill runtime 未授权工具: {}", input.tool_name),
            ));
        }

        if policy.allowed_tool_names.is_empty() {
            return Some(self.build_policy_rejection(
                input,
                &policy,
                format!("skill runtime 未授权工具: {}", input.tool_name),
            ));
        }

        None
    }

    pub(crate) fn resolve_access_mode(&self, input: &ToolExecutionInput) -> BuiltinToolAccessMode {
        if input.tool_name == BuiltinToolName::ShellExec.as_str() {
            self.parse_requested_access_mode(&input.input)
                .unwrap_or(BuiltinToolAccessMode::MaybeWrite)
        } else {
            BuiltinToolAccessMode::ReadOnly
        }
    }

    fn build_policy_rejection(
        &self,
        input: &ToolExecutionInput,
        policy: &ToolExecutionPolicy,
        reason: String,
    ) -> ToolExecutionOutput {
        ToolExecutionOutput {
            tool_call_id: input.tool_call_id.clone(),
            status: ExecutionResultStatus::Rejected,
            payload: if policy.source_skill_ids.is_empty() {
                reason.clone()
            } else {
                format!("{} (skills={})", reason, policy.source_skill_ids.join(","))
            },
            governance: GovernanceDecision {
                outcome: GovernanceOutcome::Rejected,
                allowed: false,
                requires_approval: false,
                phase: DecisionPhase::ToolPolicy,
                threshold: input.risk_level,
                reason: Some(reason),
            },
        }
    }

    fn parse_requested_access_mode(&self, input: &str) -> Option<BuiltinToolAccessMode> {
        parse_json_object(input).and_then(|object| {
            field_string(&object, &["access_mode", "write_mode", "intent"])
                .and_then(|value| BuiltinToolAccessMode::from_str(&value))
        })
    }
}

fn normalize_execution_policy(policy: &ToolExecutionPolicy) -> ToolExecutionPolicy {
    let mut normalized = policy.clone();
    normalized.source_skill_ids.sort();
    normalized.source_skill_ids.dedup();
    normalized.allowed_tool_names.sort();
    normalized.allowed_tool_names.dedup();
    normalized.denied_tool_names.sort();
    normalized.denied_tool_names.dedup();
    normalized
}

pub(crate) fn status_from_payload(payload: &str) -> ExecutionResultStatus {
    infer_execution_status(payload)
}
