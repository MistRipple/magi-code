//! 外部（MCP）工具调用入口。
//!
//! 外部客户端没有模型 turn，也没有 Task，但必须走与 Magi 自己 agent 相同的执行前判定
//! （访问档、路径与命令策略、SafetyGate）和同一个 `ToolRegistry` 执行链。本模块只做两件事：
//! 1. 用合成的 `TaskPolicy` 复用 `tool_batch` 里的判定，不复制第二份规则；
//! 2. 把“是否已获人工授权”交给调用方决定：`approval_granted=false` 时遇到需要授权的
//!    操作返回 `NeedsApproval`，由调用方走自己的审批循环后带 `approval_granted=true` 重入。
//!
//! 授权只能放行 `NeedsApproval`；`Rejected`（HardBlock、路径越界、访问档禁止）永远拒绝。

use std::path::Path;

use magi_core::{
    AccessProfile, CollaborationMode, ExecutionResultStatus, SessionId, TaskPolicy, TaskTier,
    ToolCallId, WorkspaceId,
};
use magi_event_bus::InMemoryEventBus;
use magi_tool_runtime::{
    BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolRegistry,
    canonical_builtin_tool_name,
};

use crate::builtin_tool_schema::internal_builtin_tool_rejection_payload;
use crate::tool_batch::{
    SafetyEvaluationAuditContext, policy_tool_preflight_decision, publish_safety_evaluation_audit,
};
use crate::tool_execution_policy_scope;
use crate::tool_result_utils::{approval_resume_contract_failure, approval_resume_is_safe};

/// 外部工具会话对工作区之外永远不可见的目录名（相对工作区根）。
const EXTERNAL_DENIED_RELATIVE_PATHS: &[&str] = &[".magi"];

/// 一次外部工具调用。`tool_name` 必须是内部内置工具名（公开名到内部名的映射在 MCP 层）。
pub struct ExternalToolCall<'a> {
    pub call_id: &'a str,
    pub tool_name: &'a str,
    pub arguments_json: &'a str,
    pub session_id: &'a SessionId,
    pub workspace_id: Option<&'a WorkspaceId>,
    pub workspace_root: &'a Path,
    pub access_profile: AccessProfile,
}

/// 为外部调用合成策略快照：只允许访问工作区根，`.magi` 永远拒绝。
///
/// 只读档同时把命令模式收紧为 `read_only`，与 Magi 只读任务一致。
pub fn external_tool_policy(access_profile: AccessProfile, workspace_root: &Path) -> TaskPolicy {
    let denied_paths = EXTERNAL_DENIED_RELATIVE_PATHS
        .iter()
        .map(|relative| workspace_root.join(relative).to_string_lossy().into_owned())
        .collect();
    TaskPolicy {
        autonomy_level: "Supervised".to_string(),
        access_profile,
        collaboration_mode: CollaborationMode::Disabled,
        allowed_tools: Vec::new(),
        denied_tools: Vec::new(),
        allowed_paths: vec![workspace_root.to_string_lossy().into_owned()],
        denied_paths,
        read_only_paths: Vec::new(),
        network_mode: "full".to_string(),
        command_mode: if access_profile == AccessProfile::ReadOnly {
            "read_only".to_string()
        } else {
            "full".to_string()
        },
        retry_limit: 0,
        validation_profile: None,
        checkpoint_mode: "turn".to_string(),
        task_tier: TaskTier::ExecutionChain,
        background_allowed: false,
        escalation_conditions: Vec::new(),
    }
}

fn rejected(tool_name: &str, error: &str) -> (String, ExecutionResultStatus) {
    (
        serde_json::json!({
            "tool": tool_name,
            "status": "rejected",
            "error_code": "external_tool_rejected",
            "error": error,
        })
        .to_string(),
        ExecutionResultStatus::Rejected,
    )
}

/// 执行一次外部工具调用，返回 `(payload, status)`。
///
/// `NeedsApproval` 表示尚未产生任何副作用，调用方获得人工授权后以
/// `approval_granted=true` 重入同一调用。
pub fn execute_external_tool_call(
    event_bus: &InMemoryEventBus,
    registry: &ToolRegistry,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    call: &ExternalToolCall<'_>,
    approval_granted: bool,
) -> (String, ExecutionResultStatus) {
    let Some(canonical) = canonical_builtin_tool_name(call.tool_name)
        .and_then(|name| BuiltinToolName::from_name(&name))
        .filter(|tool| tool.is_public_tool_surface())
    else {
        return rejected(call.tool_name, "该工具不对外部调用开放");
    };
    // 协调器、目标、计划、记忆、上下文与技能工具依赖 Task / 会话上下文，外部调用没有这些上下文。
    if matches!(
        canonical,
        BuiltinToolName::AgentSpawn
            | BuiltinToolName::AgentSend
            | BuiltinToolName::AgentWait
            | BuiltinToolName::GetGoal
            | BuiltinToolName::CreateGoal
            | BuiltinToolName::UpdateGoal
            | BuiltinToolName::ContextSearch
            | BuiltinToolName::ContextRead
            | BuiltinToolName::ContextRequest
            | BuiltinToolName::UpdatePlan
            | BuiltinToolName::MemoryWrite
    ) || internal_builtin_tool_rejection_payload(canonical.as_str()).is_some()
    {
        return rejected(canonical.as_str(), "该工具不对外部调用开放");
    }

    let policy = external_tool_policy(call.access_profile, call.workspace_root);
    let workspace_root = call.workspace_root.to_path_buf();

    if let Some(gate) = safety_gate {
        publish_safety_evaluation_audit(
            event_bus,
            gate,
            canonical.as_str(),
            call.arguments_json,
            call.call_id,
            SafetyEvaluationAuditContext {
                access_profile: policy.effective_access_profile(),
                session_id: Some(call.session_id),
                workspace_id: call.workspace_id,
                mission_id: None,
                task_id: None,
            },
        );
    }

    if let Some(decision) = policy_tool_preflight_decision(
        Some(&policy),
        safety_gate,
        canonical.as_str(),
        call.arguments_json,
        Some(&workspace_root),
    ) {
        match decision.status {
            ExecutionResultStatus::NeedsApproval if approval_granted => {}
            _ => return (decision.payload, decision.status),
        }
    }

    let run = |effective: AccessProfile| {
        let mut tool_policy = tool_execution_policy_scope(
            effective,
            policy.command_mode.clone(),
            &policy.allowed_paths,
            &policy.denied_paths,
        );
        tool_policy.access_profile = effective;
        let input = ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new(call.call_id),
            canonical.as_str(),
            call.arguments_json.to_string(),
        );
        let context = ToolExecutionContext {
            worker_id: None,
            task_id: None,
            session_id: Some(call.session_id.clone()),
            workspace_id: call.workspace_id.cloned(),
            access_profile: effective,
            working_directory: Some(workspace_root.clone()),
            browser_capability_snapshot: None,
            browser_execution_id: None,
        };
        let output = registry.execute_with_policy(input, context, &tool_policy);
        (output.payload, output.status)
    };

    // 与 Task 执行一致：获得授权后以 FullAccess 重入，路径与命令范围仍由策略限定。
    let effective = if approval_granted {
        AccessProfile::FullAccess
    } else {
        policy.effective_access_profile()
    };
    let result = run(effective);
    if result.1 == ExecutionResultStatus::NeedsApproval {
        if approval_granted || !approval_resume_is_safe(&result.0) {
            return approval_resume_contract_failure(canonical.as_str());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call<'a>(
        tool: &'a str,
        args: &'a str,
        root: &'a Path,
        profile: AccessProfile,
        session: &'a SessionId,
    ) -> ExternalToolCall<'a> {
        ExternalToolCall {
            call_id: "call-1",
            tool_name: tool,
            arguments_json: args,
            session_id: session,
            workspace_id: None,
            workspace_root: root,
            access_profile: profile,
        }
    }

    fn registry() -> (InMemoryEventBus, ToolRegistry) {
        (
            InMemoryEventBus::new(8),
            ToolRegistry::new(
                std::sync::Arc::new(magi_governance::GovernanceService::default()),
                std::sync::Arc::new(InMemoryEventBus::new(8)),
            ),
        )
    }

    #[test]
    fn policy_confines_to_workspace_and_denies_magi_dir() {
        let root = Path::new("/work/space");
        let policy = external_tool_policy(AccessProfile::Restricted, root);
        assert_eq!(policy.allowed_paths, vec!["/work/space".to_string()]);
        assert_eq!(policy.denied_paths, vec!["/work/space/.magi".to_string()]);
        assert_eq!(policy.command_mode, "full");
        assert_eq!(
            external_tool_policy(AccessProfile::ReadOnly, root).command_mode,
            "read_only"
        );
    }

    #[test]
    fn unknown_and_non_public_tools_are_rejected() {
        let (bus, reg) = registry();
        let session = SessionId::new("s");
        let root = Path::new("/work/space");
        for tool in ["definitely_not_a_tool", "agent_spawn", "update_plan"] {
            let (_, status) = execute_external_tool_call(
                &bus,
                &reg,
                None,
                &call(tool, "{}", root, AccessProfile::Restricted, &session),
                true,
            );
            assert_eq!(status, ExecutionResultStatus::Rejected, "{tool}");
        }
    }

    #[test]
    fn read_only_profile_rejects_writes_even_when_approval_is_granted() {
        let (bus, reg) = registry();
        let session = SessionId::new("s");
        let root = Path::new("/work/space");
        let args = r#"{"path":"/work/space/a.txt","content":"x"}"#;
        for granted in [false, true] {
            let (_, status) = execute_external_tool_call(
                &bus,
                &reg,
                None,
                &call("file_write", args, root, AccessProfile::ReadOnly, &session),
                granted,
            );
            assert_eq!(status, ExecutionResultStatus::Rejected, "granted={granted}");
        }
    }

    #[test]
    fn paths_outside_workspace_and_magi_dir_are_rejected_even_when_granted() {
        let (bus, reg) = registry();
        let session = SessionId::new("s");
        let root = Path::new("/work/space");
        for path in ["/etc/passwd", "/work/space/.magi/state.json"] {
            let args = format!(r#"{{"path":"{path}"}}"#);
            let (_, status) = execute_external_tool_call(
                &bus,
                &reg,
                None,
                &call(
                    "file_read",
                    &args,
                    root,
                    AccessProfile::Restricted,
                    &session,
                ),
                true,
            );
            assert_eq!(status, ExecutionResultStatus::Rejected, "{path}");
        }
    }
}
