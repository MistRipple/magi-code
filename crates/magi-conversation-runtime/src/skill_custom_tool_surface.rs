use magi_bridge_client::{ChatToolCall, ChatToolDefinition, ChatToolFunctionDefinition};
use magi_core::{AccessProfile, ApprovalRequirement, ExecutionResultStatus, RiskLevel, ToolCallId};
use magi_skill_runtime::{
    CustomToolBinding, SkillDispatchInput, SkillDispatchResult, SkillDispatchRuntime,
    SkillDispatchStatus, SkillRuntime, SkillSelection, SkillToolRuntimePlan,
    bridge_binding_allowed_in_access_profile,
};
use magi_tool_runtime::{ToolExecutionContext, ToolExecutionPolicy};
use serde_json::Value;

use crate::tool_result_utils::{safety_gate_public_error, tool_execution_status_label};

const SKILL_CUSTOM_TOOL_PREFIX: &str = "skill";
const SKILL_TOOL_UNAVAILABLE_PUBLIC_ERROR: &str = "Skill 工具暂不可用，请稍后重试";
const SKILL_TOOL_CONFIG_PUBLIC_ERROR: &str = "Skill 工具配置不可用，请重新加载该 Skill";
const SKILL_TOOL_SCOPE_PUBLIC_ERROR: &str = "该 Skill 工具不属于当前激活 Skill";
const SKILL_TOOL_DISPATCH_PUBLIC_ERROR: &str = "Skill 工具执行失败，请稍后重试";
const SKILL_TOOL_POLICY_PUBLIC_ERROR: &str = "该 Skill 工具在当前访问模式下不可用";
const SKILL_TOOL_REMOTE_PUBLIC_ERROR: &str = "Skill 工具返回失败，请检查输入或外接工具状态";
const SKILL_CUSTOM_TOOL_SKILL_SEGMENT_MAX_LEN: usize = 20;
const SKILL_CUSTOM_TOOL_BINDING_SEGMENT_MAX_LEN: usize = 35;

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

pub fn tool_execution_policy_scope(
    access_profile: AccessProfile,
    command_mode: impl Into<String>,
    allowed_paths: &[String],
    denied_paths: &[String],
) -> ToolExecutionPolicy {
    ToolExecutionPolicy {
        access_profile,
        allowed_paths: allowed_paths.to_vec(),
        denied_paths: denied_paths.to_vec(),
        command_mode: command_mode.into(),
        ..ToolExecutionPolicy::default()
    }
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
            name: skill_custom_tool_surface_name(skill_name, binding),
            description: format!(
                "{}（{}，{}）",
                binding.description,
                binding.tool_name,
                skill_custom_tool_bridge_label(binding.bridge_kind)
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

fn skill_custom_tool_bridge_label(
    bridge_kind: magi_bridge_client::BridgeBindingKind,
) -> &'static str {
    match bridge_kind {
        magi_bridge_client::BridgeBindingKind::Model => "模型工具",
        magi_bridge_client::BridgeBindingKind::Mcp => "外接工具",
    }
}

fn skill_custom_tool_surface_name(skill_name: &str, binding: &CustomToolBinding) -> String {
    format!(
        "{SKILL_CUSTOM_TOOL_PREFIX}__{}__{}",
        skill_custom_tool_skill_segment(skill_name),
        skill_custom_tool_binding_segment(&binding.binding_id)
    )
}

fn skill_custom_tool_skill_segment(skill_name: &str) -> String {
    model_tool_name_segment(skill_name, "skill", SKILL_CUSTOM_TOOL_SKILL_SEGMENT_MAX_LEN)
}

fn skill_custom_tool_binding_segment(binding_id: &str) -> String {
    model_tool_name_segment(
        binding_id,
        "binding",
        SKILL_CUSTOM_TOOL_BINDING_SEGMENT_MAX_LEN,
    )
}

fn model_tool_name_segment(value: &str, fallback: &str, max_len: usize) -> String {
    let trimmed = value.trim();
    let mut segment = String::new();
    let mut last_was_separator = false;
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            segment.push(ch);
            last_was_separator = false;
        } else if ch == '_' {
            if !last_was_separator {
                segment.push('_');
                last_was_separator = true;
            }
        } else if !last_was_separator {
            segment.push('_');
            last_was_separator = true;
        }
    }
    let segment = segment
        .trim_matches(|ch| ch == '_' || ch == '-')
        .to_string();
    let mut segment = if segment.is_empty() {
        fallback.to_string()
    } else {
        segment
    };
    let needs_hash = segment != trimmed || segment.len() > max_len;
    if !needs_hash {
        return segment;
    }

    let suffix = format!("-{}", stable_tool_name_hash8(trimmed));
    let prefix_len = max_len.saturating_sub(suffix.len()).max(1);
    if segment.len() > prefix_len {
        segment.truncate(prefix_len);
        segment = segment
            .trim_matches(|ch| ch == '_' || ch == '-')
            .to_string();
        if segment.is_empty() {
            segment = fallback.to_string();
        }
    }
    format!("{segment}{suffix}")
}

fn stable_tool_name_hash8(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", hash & 0xffff_ffff)
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
    execution_policy_scope: ToolExecutionPolicy,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    skill_runtime: Option<&SkillRuntime>,
    skill_dispatch_runtime: Option<&SkillDispatchRuntime>,
    context: ToolExecutionContext,
    working_directory: Option<String>,
) -> (String, ExecutionResultStatus) {
    let tool_name = tool_call.function.name.as_str();
    let active_skill_name = active_skill_name
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if active_skill_name.is_some_and(|active_skill| {
        skill_custom_tool_skill_segment(active_skill) != tool_skill_name
    }) {
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

    let access_profile = execution_policy_scope.access_profile;
    let effective_access_profile = execution_policy_scope.effective_access_profile();
    let resolved_skill_name = active_skill_name.unwrap_or(tool_skill_name);
    let mut plan = skill_runtime.build_tool_runtime_plan(SkillSelection {
        skill_ids: vec![resolved_skill_name.to_string()],
        requested_tools: Vec::new(),
    });
    plan.tool_policy.access_profile = access_profile;
    plan.tool_policy.allowed_paths = execution_policy_scope.allowed_paths;
    plan.tool_policy.denied_paths = execution_policy_scope.denied_paths;
    plan.tool_policy.command_mode = execution_policy_scope.command_mode;
    let Some(binding) = plan
        .custom_tool_bindings
        .iter()
        .find(|binding| skill_custom_tool_binding_segment(&binding.binding_id) == binding_id)
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
        effective_access_profile,
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
        Ok(SkillDispatchResult::Preflight { .. }) => {
            custom_tool_preflight_payload(tool_name, tool_skill_name, observation)
        }
        Ok(SkillDispatchResult::Bridge { output }) => {
            if output.response.ok {
                (output.response.payload, ExecutionResultStatus::Succeeded)
            } else {
                custom_tool_remote_failure_payload(tool_name, tool_skill_name, observation)
            }
        }
        Err(_error) => {
            custom_tool_dispatch_failure_payload(tool_name, tool_skill_name, observation)
        }
    }
}

fn custom_tool_preflight_payload(
    tool_name: &str,
    skill_name: &str,
    observation: magi_skill_runtime::SkillDispatchObservation,
) -> (String, ExecutionResultStatus) {
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
            "skill_tool_policy_rejected",
            SKILL_TOOL_POLICY_PUBLIC_ERROR,
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
            "skill_name": skill_name,
            "error_code": error_code,
            "error": public_error,
        })
        .to_string(),
        status,
    )
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
            "skill_name": skill_name,
            "error_code": error_code,
            "error": public_error,
        })
        .to_string(),
        status,
    )
}

fn custom_tool_remote_failure_payload(
    tool_name: &str,
    skill_name: &str,
    observation: magi_skill_runtime::SkillDispatchObservation,
) -> (String, ExecutionResultStatus) {
    tracing::warn!(
        tool_name,
        skill_name,
        binding_id = observation.binding_id.as_deref().unwrap_or_default(),
        detail = %observation.detail,
        "skill custom tool returned failed bridge response"
    );
    (
        serde_json::json!({
            "tool": tool_name,
            "status": "failed",
            "skill_name": skill_name,
            "error_code": "skill_tool_remote_failed",
            "error": SKILL_TOOL_REMOTE_PUBLIC_ERROR,
        })
        .to_string(),
        ExecutionResultStatus::Failed,
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
                ExecutionResultStatus::NeedsApproval,
                category,
                magi_safety_gate::SafetyAction::RequireApprovalInRestricted,
                pattern,
                reason,
            )),
            AccessProfile::ReadOnly => Some(custom_tool_safety_payload(
                tool_name,
                skill_name,
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
    status: ExecutionResultStatus,
    category: magi_safety_gate::SafetyCategory,
    action: magi_safety_gate::SafetyAction,
    pattern: String,
    reason: String,
) -> (String, ExecutionResultStatus) {
    let status_label = tool_execution_status_label(status);
    let public_error = safety_gate_public_error(status);
    tracing::warn!(
        tool_name,
        skill_name,
        status = status_label,
        category = category.as_str(),
        action = action.as_str(),
        pattern = %pattern,
        reason = %reason,
        "skill custom tool safety gate decision"
    );
    (
        serde_json::json!({
            "tool": tool_name,
            "status": status_label,
            "skill_name": skill_name,
            "error_code": public_error.error_code,
            "error": public_error.error,
        })
        .to_string(),
        status,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::{
        BridgeBindingKind, BridgeDispatchAction, BridgeDispatchRuntime, ChatToolFunction,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_skill_runtime::{CustomToolBinding, SkillDefinition, SkillMetadata, SkillRegistry};
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
    fn custom_skill_tool_definitions_use_model_safe_surface_names() {
        let registry = SkillRegistry::new();
        let skill_id = "code/review v1";
        registry.register(SkillDefinition {
            skill_id: skill_id.to_string(),
            title: "Code Review".to_string(),
            instruction: "审查代码。".to_string(),
            metadata: SkillMetadata {
                category: "review".to_string(),
                tags: vec![],
            },
            allowed_tools: vec![],
            custom_tool_bindings: vec![CustomToolBinding {
                binding_id: "mcp.review:loopback/server".to_string(),
                tool_name: "mcp.review".to_string(),
                description: "调用审查工具".to_string(),
                bridge_kind: BridgeBindingKind::Mcp,
                dispatch_action: BridgeDispatchAction::McpToolCall,
                bridge_target: "loopback/server".to_string(),
            }],
            prompt_priority: 50,
        });
        let skill_runtime = SkillRuntime::new(registry);
        let plan = skill_runtime.build_tool_runtime_plan(SkillSelection {
            skill_ids: vec![skill_id.to_string()],
            requested_tools: Vec::new(),
        });

        let definitions =
            build_skill_custom_tool_definitions(skill_id, &plan, AccessProfile::Restricted);

        assert_eq!(definitions.len(), 1);
        let tool_name = definitions[0].function.name.as_str();
        assert!(
            tool_name.len() <= 64,
            "模型工具名必须满足函数名长度限制，实际: {tool_name}"
        );
        assert!(
            tool_name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'),
            "模型工具名只能包含安全字符，实际: {tool_name}"
        );
        assert!(!tool_name.chars().any(|ch| matches!(ch, ':' | '/' | ' ')));
        let description = definitions[0].function.description.as_str();
        assert!(description.contains("调用审查工具"));
        assert!(description.contains("mcp.review"));
        assert!(description.contains("外接工具"));
        assert!(!description.contains("mcp.review:loopback/server"));
        assert!(!description.contains("loopback/server"));
        assert!(
            parse_skill_custom_tool_name(tool_name).is_some(),
            "规范化后的工具名仍应能被 runtime 解析"
        );
    }

    #[test]
    fn custom_skill_tool_surface_names_do_not_conflict_with_parser_separator() {
        let registry = SkillRegistry::new();
        let skill_id = "code__review";
        registry.register(SkillDefinition {
            skill_id: skill_id.to_string(),
            title: "Code Review".to_string(),
            instruction: "审查代码。".to_string(),
            metadata: SkillMetadata {
                category: "review".to_string(),
                tags: vec![],
            },
            allowed_tools: vec![],
            custom_tool_bindings: vec![CustomToolBinding {
                binding_id: "mcp__review".to_string(),
                tool_name: "mcp.review".to_string(),
                description: "调用审查工具".to_string(),
                bridge_kind: BridgeBindingKind::Mcp,
                dispatch_action: BridgeDispatchAction::McpToolCall,
                bridge_target: "loopback".to_string(),
            }],
            prompt_priority: 50,
        });
        let skill_runtime = SkillRuntime::new(registry);
        let plan = skill_runtime.build_tool_runtime_plan(SkillSelection {
            skill_ids: vec![skill_id.to_string()],
            requested_tools: Vec::new(),
        });

        let definitions =
            build_skill_custom_tool_definitions(skill_id, &plan, AccessProfile::Restricted);

        let tool_name = definitions[0].function.name.as_str();
        assert_eq!(
            tool_name.matches("__").count(),
            2,
            "模型工具名内部 segment 不能再包含解析分隔符，实际: {tool_name}"
        );
        assert!(
            parse_skill_custom_tool_name(tool_name).is_some(),
            "带连续下划线的原始 Skill / binding 也必须生成可解析工具名"
        );
    }

    #[test]
    fn custom_skill_tool_surface_name_dispatches_to_original_binding() {
        let registry = SkillRegistry::new();
        let skill_id = "model/skill";
        registry.register(SkillDefinition {
            skill_id: skill_id.to_string(),
            title: "Model Skill".to_string(),
            instruction: "调用模型桥接工具。".to_string(),
            metadata: SkillMetadata {
                category: "integration".to_string(),
                tags: vec!["model".to_string()],
            },
            allowed_tools: vec![],
            custom_tool_bindings: vec![CustomToolBinding {
                binding_id: "ask.model:openai".to_string(),
                tool_name: "model.prompt".to_string(),
                description: "询问模型".to_string(),
                bridge_kind: BridgeBindingKind::Model,
                dispatch_action: BridgeDispatchAction::ModelPrompt,
                bridge_target: "openai".to_string(),
            }],
            prompt_priority: 50,
        });
        let skill_runtime = SkillRuntime::new(registry);
        let plan = skill_runtime.build_tool_runtime_plan(SkillSelection {
            skill_ids: vec![skill_id.to_string()],
            requested_tools: Vec::new(),
        });
        let definitions =
            build_skill_custom_tool_definitions(skill_id, &plan, AccessProfile::Restricted);
        let tool_name = definitions[0].function.name.clone();
        let (tool_skill_name, binding_id) =
            parse_skill_custom_tool_name(&tool_name).expect("surface name should parse");
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let dispatch_runtime = SkillDispatchRuntime::new(
            tool_registry,
            BridgeDispatchRuntime::new().with_model_client(Arc::new(BusinessFailureModelClient)),
        );

        let (payload, status) = execute_skill_custom_tool(
            &tool_call(&tool_name),
            &tool_skill_name,
            &binding_id,
            Some(skill_id),
            tool_execution_policy_scope(AccessProfile::Restricted, "", &[], &[]),
            None,
            Some(&skill_runtime),
            Some(&dispatch_runtime),
            ToolExecutionContext::default(),
            None,
        );

        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: Value = serde_json::from_str(&payload).expect("payload should be json");
        assert_eq!(parsed["error_code"], "skill_tool_remote_failed");
        assert_ne!(parsed["error_code"], "skill_tool_binding_missing");
        assert!(!payload.contains("ask.model:openai"));
        assert!(!payload.contains("secret-token"));
    }

    #[derive(Clone, Debug, Default)]
    struct BusinessFailureModelClient;

    impl magi_bridge_client::ModelBridgeClient for BusinessFailureModelClient {
        fn invoke(
            &self,
            _request: magi_bridge_client::ModelInvocationRequest,
        ) -> Result<magi_bridge_client::BridgeResponse, magi_bridge_client::BridgeClientError>
        {
            Ok(magi_bridge_client::BridgeResponse {
                ok: false,
                payload: "remote business detail: secret-token".to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: magi_bridge_client::ModelInvocationRequest,
            _on_delta: &dyn Fn(&magi_bridge_client::ModelStreamingDelta),
        ) -> Result<magi_bridge_client::BridgeResponse, magi_bridge_client::BridgeClientError>
        {
            self.invoke(request)
        }
    }

    #[test]
    fn custom_skill_tool_missing_runtime_uses_public_error() {
        let (payload, status) = execute_skill_custom_tool(
            &tool_call("skill__code-review__review"),
            "code-review",
            "review",
            Some("code-review"),
            tool_execution_policy_scope(AccessProfile::Restricted, "", &[], &[]),
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
            tool_execution_policy_scope(AccessProfile::Restricted, "", &[], &[]),
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
            tool_execution_policy_scope(AccessProfile::Restricted, "", &[], &[]),
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

    #[test]
    fn custom_skill_tool_read_only_mcp_preflight_uses_public_error() {
        let registry = SkillRegistry::new();
        registry.register(SkillDefinition {
            skill_id: "mcp-skill".to_string(),
            title: "MCP Skill".to_string(),
            instruction: "调用外接能力。".to_string(),
            metadata: SkillMetadata {
                category: "integration".to_string(),
                tags: vec!["mcp".to_string()],
            },
            allowed_tools: vec![],
            custom_tool_bindings: vec![CustomToolBinding {
                binding_id: "inspect".to_string(),
                tool_name: "echo.inspect".to_string(),
                description: "检查输入".to_string(),
                bridge_kind: BridgeBindingKind::Mcp,
                dispatch_action: BridgeDispatchAction::McpToolCall,
                bridge_target: "loopback-mcp".to_string(),
            }],
            prompt_priority: 50,
        });
        let skill_runtime = SkillRuntime::new(registry);
        let dispatch_runtime = dispatch_runtime();

        let (payload, status) = execute_skill_custom_tool(
            &ChatToolCall {
                id: "skill-tool-call-read-only".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: "skill__mcp-skill__inspect".to_string(),
                    arguments: serde_json::json!({ "payload": "inspect" }).to_string(),
                },
            },
            "mcp-skill",
            "inspect",
            Some("mcp-skill"),
            tool_execution_policy_scope(AccessProfile::ReadOnly, "", &[], &[]),
            None,
            Some(&skill_runtime),
            Some(&dispatch_runtime),
            ToolExecutionContext::default(),
            None,
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        let parsed: Value = serde_json::from_str(&payload).expect("payload should be json");
        assert_eq!(parsed["status"], "rejected");
        assert_eq!(parsed["tool"], "skill__mcp-skill__inspect");
        assert_eq!(parsed["error_code"], "skill_tool_policy_rejected");
        assert_eq!(parsed["error"], SKILL_TOOL_POLICY_PUBLIC_ERROR);
        assert_eq!(parsed.get("bridge_target"), None);
        assert_eq!(parsed.get("bridge_kind"), None);
    }

    #[test]
    fn custom_skill_tool_read_only_command_mode_mcp_preflight_uses_public_error() {
        let registry = SkillRegistry::new();
        registry.register(SkillDefinition {
            skill_id: "mcp-skill".to_string(),
            title: "MCP Skill".to_string(),
            instruction: "调用外接能力。".to_string(),
            metadata: SkillMetadata {
                category: "integration".to_string(),
                tags: vec!["mcp".to_string()],
            },
            allowed_tools: vec![],
            custom_tool_bindings: vec![CustomToolBinding {
                binding_id: "inspect".to_string(),
                tool_name: "echo.inspect".to_string(),
                description: "检查输入".to_string(),
                bridge_kind: BridgeBindingKind::Mcp,
                dispatch_action: BridgeDispatchAction::McpToolCall,
                bridge_target: "loopback-mcp".to_string(),
            }],
            prompt_priority: 50,
        });
        let skill_runtime = SkillRuntime::new(registry);
        let dispatch_runtime = dispatch_runtime();

        let (payload, status) = execute_skill_custom_tool(
            &ChatToolCall {
                id: "skill-tool-call-effective-read-only".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: "skill__mcp-skill__inspect".to_string(),
                    arguments: serde_json::json!({ "payload": "inspect" }).to_string(),
                },
            },
            "mcp-skill",
            "inspect",
            Some("mcp-skill"),
            tool_execution_policy_scope(AccessProfile::FullAccess, "read_only", &[], &[]),
            None,
            Some(&skill_runtime),
            Some(&dispatch_runtime),
            ToolExecutionContext {
                access_profile: AccessProfile::FullAccess,
                ..ToolExecutionContext::default()
            },
            None,
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        let parsed: Value = serde_json::from_str(&payload).expect("payload should be json");
        assert_eq!(parsed["status"], "rejected");
        assert_eq!(parsed["tool"], "skill__mcp-skill__inspect");
        assert_eq!(parsed["error_code"], "skill_tool_policy_rejected");
        assert_eq!(parsed["error"], SKILL_TOOL_POLICY_PUBLIC_ERROR);
        assert_eq!(parsed.get("bridge_target"), None);
        assert_eq!(parsed.get("bridge_kind"), None);
    }

    #[test]
    fn custom_skill_tool_safety_gate_uses_public_error() {
        let registry = SkillRegistry::new();
        registry.register(SkillDefinition {
            skill_id: "mcp-skill".to_string(),
            title: "MCP Skill".to_string(),
            instruction: "调用外接能力。".to_string(),
            metadata: SkillMetadata {
                category: "integration".to_string(),
                tags: vec!["mcp".to_string()],
            },
            allowed_tools: vec![],
            custom_tool_bindings: vec![CustomToolBinding {
                binding_id: "inspect".to_string(),
                tool_name: "echo.inspect".to_string(),
                description: "检查输入".to_string(),
                bridge_kind: BridgeBindingKind::Mcp,
                dispatch_action: BridgeDispatchAction::McpToolCall,
                bridge_target: "loopback-mcp".to_string(),
            }],
            prompt_priority: 50,
        });
        let skill_runtime = SkillRuntime::new(registry);
        let dispatch_runtime = dispatch_runtime();
        let safety_gate =
            magi_safety_gate::SafetyGate::new(vec![magi_safety_gate::SafetyRule::with_action(
                "rm -rf",
                magi_safety_gate::SafetyCategory::BulkDelete,
                magi_safety_gate::SafetyAction::HardBlock,
            )]);

        let (payload, status) = execute_skill_custom_tool(
            &ChatToolCall {
                id: "skill-tool-call-safety".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: "skill__mcp-skill__inspect".to_string(),
                    arguments:
                        serde_json::json!({ "payload": r#"{"command":"rm -rf /tmp/demo"}"# })
                            .to_string(),
                },
            },
            "mcp-skill",
            "inspect",
            Some("mcp-skill"),
            tool_execution_policy_scope(AccessProfile::FullAccess, "", &[], &[]),
            Some(&safety_gate),
            Some(&skill_runtime),
            Some(&dispatch_runtime),
            ToolExecutionContext::default(),
            None,
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        let parsed: Value = serde_json::from_str(&payload).expect("payload should be json");
        assert_eq!(parsed["status"], "rejected");
        assert_eq!(parsed["tool"], "skill__mcp-skill__inspect");
        assert_eq!(parsed["error_code"], "tool_safety_rejected");
        assert_eq!(parsed["error"], "该操作已被安全防护阻止");
        assert_eq!(parsed.get("safety_gate"), None);
        assert!(!payload.contains("rm -rf"));
        assert!(!payload.contains("bulk_delete"));
        assert!(!payload.contains("hard_block"));
        assert!(!payload.contains("loopback-mcp"));
    }

    #[test]
    fn custom_skill_tool_safety_gate_uses_effective_read_only_profile() {
        let registry = SkillRegistry::new();
        registry.register(SkillDefinition {
            skill_id: "mcp-skill".to_string(),
            title: "MCP Skill".to_string(),
            instruction: "调用外接能力。".to_string(),
            metadata: SkillMetadata {
                category: "integration".to_string(),
                tags: vec!["mcp".to_string()],
            },
            allowed_tools: vec![],
            custom_tool_bindings: vec![CustomToolBinding {
                binding_id: "inspect".to_string(),
                tool_name: "echo.inspect".to_string(),
                description: "检查输入".to_string(),
                bridge_kind: BridgeBindingKind::Mcp,
                dispatch_action: BridgeDispatchAction::McpToolCall,
                bridge_target: "loopback-mcp".to_string(),
            }],
            prompt_priority: 50,
        });
        let skill_runtime = SkillRuntime::new(registry);
        let dispatch_runtime = dispatch_runtime();
        let safety_gate =
            magi_safety_gate::SafetyGate::new(vec![magi_safety_gate::SafetyRule::with_action(
                "git push --force",
                magi_safety_gate::SafetyCategory::GitHistory,
                magi_safety_gate::SafetyAction::RequireApprovalInRestricted,
            )]);

        let (payload, status) = execute_skill_custom_tool(
            &ChatToolCall {
                id: "skill-tool-call-effective-safety".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: "skill__mcp-skill__inspect".to_string(),
                    arguments:
                        serde_json::json!({ "payload": r#"{"command":"git push --force"}"# })
                            .to_string(),
                },
            },
            "mcp-skill",
            "inspect",
            Some("mcp-skill"),
            tool_execution_policy_scope(AccessProfile::FullAccess, "read_only", &[], &[]),
            Some(&safety_gate),
            Some(&skill_runtime),
            Some(&dispatch_runtime),
            ToolExecutionContext {
                access_profile: AccessProfile::FullAccess,
                ..ToolExecutionContext::default()
            },
            None,
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        let parsed: Value = serde_json::from_str(&payload).expect("payload should be json");
        assert_eq!(parsed["status"], "rejected");
        assert_eq!(parsed["tool"], "skill__mcp-skill__inspect");
        assert_eq!(parsed["error_code"], "tool_safety_rejected");
        assert_eq!(parsed["error"], "该操作已被安全防护阻止");
        assert_eq!(parsed.get("safety_gate"), None);
        assert!(!payload.contains("git push --force"));
        assert!(!payload.contains("loopback-mcp"));
    }

    #[test]
    fn custom_skill_tool_ok_false_bridge_response_uses_public_error() {
        let registry = SkillRegistry::new();
        registry.register(SkillDefinition {
            skill_id: "model-skill".to_string(),
            title: "Model Skill".to_string(),
            instruction: "调用外接模型能力。".to_string(),
            metadata: SkillMetadata {
                category: "integration".to_string(),
                tags: vec!["model".to_string()],
            },
            allowed_tools: vec![],
            custom_tool_bindings: vec![CustomToolBinding {
                binding_id: "ask".to_string(),
                tool_name: "model.prompt".to_string(),
                description: "询问模型".to_string(),
                bridge_kind: BridgeBindingKind::Model,
                dispatch_action: BridgeDispatchAction::ModelPrompt,
                bridge_target: "openai".to_string(),
            }],
            prompt_priority: 50,
        });
        let skill_runtime = SkillRuntime::new(registry);
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let dispatch_runtime = SkillDispatchRuntime::new(
            tool_registry,
            BridgeDispatchRuntime::new().with_model_client(Arc::new(BusinessFailureModelClient)),
        );

        let (payload, status) = execute_skill_custom_tool(
            &ChatToolCall {
                id: "skill-tool-call-ok-false".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: "skill__model-skill__ask".to_string(),
                    arguments: serde_json::json!({ "payload": "ask" }).to_string(),
                },
            },
            "model-skill",
            "ask",
            Some("model-skill"),
            tool_execution_policy_scope(AccessProfile::Restricted, "", &[], &[]),
            None,
            Some(&skill_runtime),
            Some(&dispatch_runtime),
            ToolExecutionContext::default(),
            None,
        );

        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: Value = serde_json::from_str(&payload).expect("payload should be json");
        assert_eq!(parsed["status"], "failed");
        assert_eq!(parsed["tool"], "skill__model-skill__ask");
        assert_eq!(parsed["error_code"], "skill_tool_remote_failed");
        assert_eq!(parsed["error"], SKILL_TOOL_REMOTE_PUBLIC_ERROR);
        assert_eq!(parsed.get("bridge_target"), None);
        assert_eq!(parsed.get("bridge_kind"), None);
        assert!(!payload.contains("secret-token"));
        assert!(!payload.contains("remote business detail"));
    }
}
