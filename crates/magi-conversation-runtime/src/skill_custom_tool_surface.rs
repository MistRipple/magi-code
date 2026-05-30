use magi_bridge_client::{ChatToolCall, ChatToolDefinition, ChatToolFunctionDefinition};
use magi_core::{AccessProfile, ApprovalRequirement, ExecutionResultStatus, RiskLevel, ToolCallId};
use magi_skill_runtime::{
    CustomToolBinding, SkillDispatchInput, SkillDispatchResult, SkillDispatchRuntime,
    SkillDispatchStatus, SkillRuntime, SkillSelection, SkillToolRuntimePlan,
};
use magi_tool_runtime::{ToolExecutionContext, ToolExecutionPolicy};
use serde_json::Value;

const SKILL_CUSTOM_TOOL_PREFIX: &str = "skill";

pub fn build_skill_custom_tool_definitions(
    skill_name: &str,
    plan: &SkillToolRuntimePlan,
) -> Vec<ChatToolDefinition> {
    plan.custom_tool_bindings
        .iter()
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

fn custom_tool_failure(
    tool_name: &str,
    error: impl Into<String>,
    status: ExecutionResultStatus,
) -> (String, ExecutionResultStatus) {
    (
        serde_json::json!({
            "tool": tool_name,
            "status": match status {
                ExecutionResultStatus::Rejected => "rejected",
                _ => "failed",
            },
            "error": error.into(),
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
        return custom_tool_failure(
            tool_name,
            format!(
                "custom skill tool {} 不属于当前激活 skill {}",
                tool_name,
                active_skill_name.unwrap_or_default()
            ),
            ExecutionResultStatus::Rejected,
        );
    }

    let Some(skill_runtime) = skill_runtime else {
        return custom_tool_failure(
            tool_name,
            "skill runtime not available",
            ExecutionResultStatus::Failed,
        );
    };
    let Some(skill_dispatch_runtime) = skill_dispatch_runtime else {
        return custom_tool_failure(
            tool_name,
            "skill dispatch runtime not available",
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
        return custom_tool_failure(
            tool_name,
            format!("未找到 custom skill binding: {tool_skill_name} / {binding_id}"),
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
        Err(_error) => (
            serde_json::json!({
                "tool": tool_name,
                "status": "failed",
                "binding_id": observation.binding_id,
                "skill_name": tool_skill_name,
                "error": observation.detail,
                "bridge_error_layer": observation.bridge_error_layer,
                "bridge_error_message": observation.bridge_error_message,
            })
            .to_string(),
            match observation.status {
                SkillDispatchStatus::NeedsApproval => ExecutionResultStatus::NeedsApproval,
                SkillDispatchStatus::Rejected => ExecutionResultStatus::Rejected,
                _ => ExecutionResultStatus::Failed,
            },
        ),
    }
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
