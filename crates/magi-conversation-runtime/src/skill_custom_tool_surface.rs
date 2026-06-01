use magi_bridge_client::{ChatToolCall, ChatToolDefinition, ChatToolFunctionDefinition};
use magi_core::{AccessProfile, ApprovalRequirement, ExecutionResultStatus, RiskLevel, ToolCallId};
use magi_skill_runtime::{
    CustomToolBinding, SkillDispatchInput, SkillDispatchResult, SkillDispatchRuntime,
    SkillDispatchStatus, SkillRuntime, SkillSelection, SkillToolRuntimePlan,
    bridge_binding_allowed_in_access_profile,
};
use magi_tool_runtime::{ToolExecutionContext, ToolExecutionPolicy};
use serde_json::Value;

const SKILL_CUSTOM_TOOL_PREFIX: &str = "skill";
const SKILL_TOOL_UNAVAILABLE_PUBLIC_ERROR: &str = "Skill 工具暂不可用，请稍后重试";
const SKILL_TOOL_CONFIG_PUBLIC_ERROR: &str = "Skill 工具配置不可用，请重新加载该 Skill";
const SKILL_TOOL_SCOPE_PUBLIC_ERROR: &str = "该 Skill 工具不属于当前激活 Skill";
const SKILL_TOOL_DISPATCH_PUBLIC_ERROR: &str = "Skill 工具执行失败，请稍后重试";

pub fn build_skill_custom_tool_definitions(
    skill_name: &str,
    plan: &SkillToolRuntimePlan,
    access_profile: AccessProfile,
) -> Vec<ChatToolDefinition> {
    plan.custom_tool_bindings
        .iter()
        .filter(|binding| {
            bridge_binding_allowed_in_access_profile(binding.bridge_kind, access_profile)
        })
        .map(|binding| build_skill_custom_tool_definition(skill_name, binding))
        .collect()
}

pub fn parse_skill_custom_tool_name(tool_name: &str) -> Option<(String, String)> {
    let mut parts = tool_name.split("__");
    let prefix = parts.next()?;
    if prefix != SKILL_CUSTOM_TOOL_PREFIX {
        return None;
    }
    let skill_name = parts.next()?.trim();
    let binding_id = parts.next()?.trim();
    if skill_name.is_empty() || binding_id.is_empty() || parts.next().is_some() {
        return None;
    }
    Some((skill_name.to_string(), binding_id.to_string()))
}

pub fn extract_skill_custom_tool_payload(arguments: &str) -> String {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|value| {
            value
                .get("payload")
                .or_else(|| value.get("input"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| arguments.to_string())
}

pub fn active_skill_tool_execution_policy(
    access_profile: AccessProfile,
    skill_runtime: Option<&SkillRuntime>,
    skill_name: Option<&str>,
) -> ToolExecutionPolicy {
    let mut policy = ToolExecutionPolicy {
        access_profile,
        ..ToolExecutionPolicy::default()
    };
    let (Some(skill_runtime), Some(skill_name)) = (
        skill_runtime,
        skill_name.map(str::trim).filter(|value| !value.is_empty()),
    ) else {
        return policy;
    };

    let plan = skill_runtime.build_tool_runtime_plan(SkillSelection {
        skill_ids: vec![skill_name.to_string()],
        requested_tools: Vec::new(),
    });
    policy.source_skill_ids = plan.tool_policy.source_skill_ids;
    policy.allowed_tool_names = plan.tool_policy.allowed_tool_names;
    policy.denied_tool_names = plan.tool_policy.denied_tool_names;
    policy
}

fn build_skill_custom_tool_definition(
    skill_name: &str,
    binding: &CustomToolBinding,
) -> ChatToolDefinition {
    ChatToolDefinition {
        kind: "function".to_string(),
        function: ChatToolFunctionDefinition {
            name: format!("skill__{skill_name}__{}", binding.binding_id),
            description: format!(
                "{}（{} → {}，{}）",
                binding.description, binding.tool_name, binding.bridge_target, binding.binding_id
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "payload": {
                        "type": "string",
                        "description": "传给该自定义桥接工具的原始输入字符串"
                    }
                },
                "required": ["payload"]
            }),
        },
    }
}

fn custom_tool_public_failure(
    tool_name: &str,
    error_code: &'static str,
    public_error: &'static str,
    status: ExecutionResultStatus,
) -> (String, ExecutionResultStatus) {
    (
        serde_json::json!({
            "tool": tool_name,
            "status": match status {
                ExecutionResultStatus::Rejected => "rejected",
                ExecutionResultStatus::NeedsApproval => "needs_approval",
                _ => "failed",
            },
            "error_code": error_code,
            "error": public_error,
        })
        .to_string(),
        status,
    )
}

/// 执行 `skill__<skill>__<binding>` 形态的 skill 自定义桥接工具调用。
///
/// task 派发路径（tool_batch）与 session turn 写回路径（session_writeback）共用此实现，
/// 唯一差异是调用方传入的 [`ToolExecutionContext`]（worker_id / task_id 取值不同），
/// 由参数承载，不在两处各写一份执行分支。
#[allow(clippy::too_many_arguments)]
pub fn execute_skill_custom_tool(
    tool_call: &ChatToolCall,
    tool_skill_name: &str,
    binding_id: &str,
    active_skill_name: Option<&str>,
    access_profile: AccessProfile,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    skill_runtime: Option<&SkillRuntime>,
    skill_dispatch_runtime: Option<&SkillDispatchRuntime>,
    context: ToolExecutionContext,
    working_directory: Option<String>,
) -> (String, ExecutionResultStatus) {
    let tool_name = tool_call.function.name.as_str();

    if active_skill_name.is_some_and(|active_skill| active_skill != tool_skill_name) {
        tracing::warn!(
            tool_name,
            requested_skill = %tool_skill_name,
            active_skill = active_skill_name.unwrap_or_default(),
            "skill custom tool called outside active skill"
        );
        return custom_tool_public_failure(
            tool_name,
            "skill_tool_scope_mismatch",
            SKILL_TOOL_SCOPE_PUBLIC_ERROR,
            ExecutionResultStatus::Rejected,
        );
    }

    let Some(skill_runtime) = skill_runtime else {
        tracing::warn!(tool_name, "skill runtime unavailable for custom tool");
        return custom_tool_public_failure(
            tool_name,
            "skill_runtime_unavailable",
            SKILL_TOOL_UNAVAILABLE_PUBLIC_ERROR,
            ExecutionResultStatus::Failed,
        );
    };
    let Some(skill_dispatch_runtime) = skill_dispatch_runtime else {
        tracing::warn!(
            tool_name,
            "skill dispatch runtime unavailable for custom tool"
        );
        return custom_tool_public_failure(
            tool_name,
            "skill_dispatch_unavailable",
            SKILL_TOOL_UNAVAILABLE_PUBLIC_ERROR,
            ExecutionResultStatus::Failed,
        );
    };

    let mut plan = skill_runtime.build_tool_runtime_plan(SkillSelection {
        skill_ids: vec![tool_skill_name.to_string()],
        requested_tools: Vec::new(),
    });
    plan.tool_policy.access_profile = access_profile;
    let Some(binding) = plan
        .custom_tool_bindings
        .iter()
        .find(|binding| binding.binding_id == binding_id)
        .cloned()
    else {
        tracing::warn!(
            tool_name,
            skill_name = %tool_skill_name,
            binding_id,
            "skill custom tool binding not found"
        );
        return custom_tool_public_failure(
            tool_name,
            "skill_tool_binding_missing",
            SKILL_TOOL_CONFIG_PUBLIC_ERROR,
            ExecutionResultStatus::Failed,
        );
    };

    let payload = extract_skill_custom_tool_payload(&tool_call.function.arguments);
    if let Some(preflight) = custom_tool_safety_decision(
        safety_gate,
        access_profile,
        tool_name,
        tool_skill_name,
        &binding,
        &payload,
    ) {
        return preflight;
    }
    let outcome = skill_dispatch_runtime.dispatch_observed(
        &plan,
        SkillDispatchInput {
            tool_call_id: ToolCallId::new(&tool_call.id),
            tool_name: binding.tool_name.clone(),
            binding_id: Some(binding.binding_id.clone()),
            payload,
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
            context,
            working_directory,
        },
    );
    let observation = outcome.observation;
    match outcome.result {
        Ok(SkillDispatchResult::Builtin { output }) => (output.payload, output.status),
        Ok(SkillDispatchResult::Preflight { output }) => (output.payload, output.status),
        Ok(SkillDispatchResult::Bridge { output }) => (
            output.response.payload,
            if output.response.ok {
                ExecutionResultStatus::Succeeded
            } else {
                ExecutionResultStatus::Failed
            },
        ),
        Err(_error) => {
            custom_tool_dispatch_failure_payload(tool_name, tool_skill_name, observation)
        }
    }
}

fn custom_tool_dispatch_failure_payload(
    tool_name: &str,
    skill_name: &str,
    observation: magi_skill_runtime::SkillDispatchObservation,
) -> (String, ExecutionResultStatus) {
    tracing::warn!(
        tool_name,
        skill_name,
        binding_id = observation.binding_id.as_deref().unwrap_or_default(),
        error_kind = ?observation.error_kind,
        bridge_error_layer = ?observation.bridge_error_layer,
        bridge_error_message = observation.bridge_error_message.as_deref().unwrap_or_default(),
        detail = %observation.detail,
        "skill custom tool dispatch failed"
    );
    let (status_label, status, error_code, public_error) = match observation.status {
        SkillDispatchStatus::NeedsApproval => (
            "needs_approval",
            ExecutionResultStatus::NeedsApproval,
            "skill_tool_needs_approval",
            "Skill 工具需要批准后执行",
        ),
        SkillDispatchStatus::Rejected => (
            "rejected",
            ExecutionResultStatus::Rejected,
            "skill_tool_config_unavailable",
            SKILL_TOOL_CONFIG_PUBLIC_ERROR,
        ),
        _ => (
            "failed",
            ExecutionResultStatus::Failed,
            "skill_tool_dispatch_failed",
            SKILL_TOOL_DISPATCH_PUBLIC_ERROR,
        ),
    };
    (
        serde_json::json!({
            "tool": tool_name,
            "status": status_label,
            "binding_id": observation.binding_id,
            "skill_name": skill_name,
            "error_code": error_code,
            "error": public_error,
        })
        .to_string(),
        status,
    )
}

fn custom_tool_safety_decision(
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    access_profile: AccessProfile,
    tool_name: &str,
    skill_name: &str,
    binding: &CustomToolBinding,
    payload: &str,
) -> Option<(String, ExecutionResultStatus)> {
    if binding.bridge_kind != magi_bridge_client::BridgeBindingKind::Mcp {
        return None;
    }
    let decision = safety_gate?.evaluate_text(payload);
    match decision {
        magi_safety_gate::SafetyDecision::Allow
        | magi_safety_gate::SafetyDecision::AuditOnly { .. } => None,
        magi_safety_gate::SafetyDecision::HardBlock {
            category,
            pattern,
            reason,
        } => Some(custom_tool_safety_payload(
            tool_name,
            skill_name,
            binding,
            ExecutionResultStatus::Rejected,
            category,
            magi_safety_gate::SafetyAction::HardBlock,
            pattern,
            reason,
        )),
        magi_safety_gate::SafetyDecision::RequireApprovalInRestricted {
            category,
            pattern,
            reason,
        } => match access_profile {
            AccessProfile::FullAccess => None,
            AccessProfile::Restricted => Some(custom_tool_safety_payload(
                tool_name,
                skill_name,
                binding,
                ExecutionResultStatus::NeedsApproval,
                category,
                magi_safety_gate::SafetyAction::RequireApprovalInRestricted,
                pattern,
                reason,
            )),
            AccessProfile::ReadOnly => Some(custom_tool_safety_payload(
                tool_name,
                skill_name,
                binding,
                ExecutionResultStatus::Rejected,
                category,
                magi_safety_gate::SafetyAction::RequireApprovalInRestricted,
                pattern,
                format!("{reason}；只读分析模式不支持通过审批升级执行"),
            )),
        },
    }
}

fn custom_tool_safety_payload(
    tool_name: &str,
    skill_name: &str,
    binding: &CustomToolBinding,
    status: ExecutionResultStatus,
    category: magi_safety_gate::SafetyCategory,
    action: magi_safety_gate::SafetyAction,
    pattern: String,
    reason: String,
) -> (String, ExecutionResultStatus) {
    (
        serde_json::json!({
            "tool": tool_name,
            "status": match status {
                ExecutionResultStatus::Succeeded => "succeeded",
                ExecutionResultStatus::Failed => "failed",
                ExecutionResultStatus::Rejected => "rejected",
                ExecutionResultStatus::NeedsApproval => "needs_approval",
                ExecutionResultStatus::Cancelled => "cancelled",
            },
            "binding_id": &binding.binding_id,
            "skill_name": skill_name,
            "error": reason,
            "safety_gate": {
                "category": category.as_str(),
                "pattern": pattern,
                "action": action.as_str(),
            },
        })
        .to_string(),
        status,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::{BridgeDispatchRuntime, ChatToolFunction};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_skill_runtime::SkillRegistry;
    use magi_tool_runtime::ToolRegistry;
    use std::sync::Arc;

    fn tool_call(name: &str) -> ChatToolCall {
        ChatToolCall {
            id: "skill-tool-call".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: name.to_string(),
                arguments: serde_json::json!({ "payload": "hello" }).to_string(),
            },
        }
    }

    fn dispatch_runtime() -> SkillDispatchRuntime {
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        SkillDispatchRuntime::new(tool_registry, BridgeDispatchRuntime::new())
    }

    #[test]
    fn custom_skill_tool_missing_runtime_uses_public_error() {
        let (payload, status) = execute_skill_custom_tool(
            &tool_call("skill__code-review__review"),
            "code-review",
            "review",
            Some("code-review"),
            AccessProfile::Restricted,
            None,
            None,
            None,
            ToolExecutionContext::default(),
            None,
        );

        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: Value = serde_json::from_str(&payload).expect("payload should be json");
        assert_eq!(parsed["error_code"], "skill_runtime_unavailable");
        assert_eq!(parsed["error"], SKILL_TOOL_UNAVAILABLE_PUBLIC_ERROR);
        assert!(!payload.contains("runtime not available"));
    }

    #[test]
    fn custom_skill_tool_scope_mismatch_uses_public_error() {
        let (payload, status) = execute_skill_custom_tool(
            &tool_call("skill__other__review"),
            "other",
            "review",
            Some("active"),
            AccessProfile::Restricted,
            None,
            None,
            None,
            ToolExecutionContext::default(),
            None,
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        let parsed: Value = serde_json::from_str(&payload).expect("payload should be json");
        assert_eq!(parsed["error_code"], "skill_tool_scope_mismatch");
        assert_eq!(parsed["error"], SKILL_TOOL_SCOPE_PUBLIC_ERROR);
        assert!(!payload.contains("custom skill tool"));
    }

    #[test]
    fn custom_skill_tool_missing_binding_uses_public_error() {
        let skill_runtime = SkillRuntime::new(SkillRegistry::new());
        let dispatch_runtime = dispatch_runtime();

        let (payload, status) = execute_skill_custom_tool(
            &tool_call("skill__code-review__missing"),
            "code-review",
            "missing",
            Some("code-review"),
            AccessProfile::Restricted,
            None,
            Some(&skill_runtime),
            Some(&dispatch_runtime),
            ToolExecutionContext::default(),
            None,
        );

        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: Value = serde_json::from_str(&payload).expect("payload should be json");
        assert_eq!(parsed["error_code"], "skill_tool_binding_missing");
        assert_eq!(parsed["error"], SKILL_TOOL_CONFIG_PUBLIC_ERROR);
        assert!(!payload.contains("custom skill binding"));
        assert!(!payload.contains("code-review / missing"));
    }
}
