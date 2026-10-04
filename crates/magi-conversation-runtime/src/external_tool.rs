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

use magi_bridge_client::{ChatToolCall, ChatToolFunction};
use magi_core::{
    AccessProfile, CollaborationMode, ExecutionResultStatus, SessionId, TaskPolicy, TaskTier,
    ToolCallId, WorkspaceId,
};
use magi_event_bus::InMemoryEventBus;
use magi_snapshot::{ToolHook, ToolHookCtx};
use magi_tool_runtime::{
    BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolRegistry,
    canonical_builtin_tool_name,
};

use crate::builtin_tool_schema::internal_builtin_tool_rejection_payload;
use crate::tool_batch::{
    SafetyEvaluationAuditContext, policy_tool_preflight_decision, publish_safety_evaluation_audit,
};
use crate::tool_declared_paths::{append_result_declared_paths, derive_declared_paths};
use crate::tool_result_utils::{approval_resume_contract_failure, approval_resume_is_safe};
use crate::{execute_skill_custom_tool, parse_skill_custom_tool_name, tool_execution_policy_scope};

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
    /// 该外部会话的变更账本钩子。写入类工具执行前后由它对改写路径拍 hash，
    /// 使外部改动进入待处理变更并可批准 / 回退；只读调用传 `None` 亦可。
    pub snapshot: Option<&'a dyn ToolHook>,
    /// Skill handler 的运行时（执行 `skill__*` 工具时需要）。
    pub skill_runtime: Option<&'a magi_skill_runtime::SkillRuntime>,
    pub skill_dispatch_runtime: Option<&'a magi_skill_runtime::SkillDispatchRuntime>,
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

/// 外部调用可以到达的工具种类。
enum ExternalTarget {
    Builtin(BuiltinToolName),
    /// 下游 MCP 工具（Magi 已连接的 MCP 服务器提供）。
    DownstreamMcp,
    /// Skill 的 handler 工具（`skill__<skill>__<binding>`）。
    Skill {
        skill: String,
        binding: String,
    },
}

/// 内置工具能否对外部调用开放。
///
/// 需要 Task / 会话上下文才能执行的（协调器、目标、计划、记忆、上下文、浏览器、图片、
/// 内部执行能力）不开放；其余公共内置工具都是“项目允许的内置工具”，由权限档与审批决定能否调用。
pub fn builtin_exposed_externally(tool: BuiltinToolName) -> bool {
    tool.is_public_tool_surface()
        && tool.browser_tool_kind().is_none()
        && internal_builtin_tool_rejection_payload(tool.as_str()).is_none()
        && !matches!(
            tool,
            BuiltinToolName::AgentSpawn
                | BuiltinToolName::AgentSend
                | BuiltinToolName::AgentCancel
                | BuiltinToolName::AgentWait
                | BuiltinToolName::GetGoal
                | BuiltinToolName::CreateGoal
                | BuiltinToolName::UpdateGoal
                | BuiltinToolName::ContextSearch
                | BuiltinToolName::ContextRead
                | BuiltinToolName::ContextRequest
                | BuiltinToolName::UpdatePlan
                | BuiltinToolName::MemoryWrite
                | BuiltinToolName::ViewImage
                | BuiltinToolName::ImageGenerate
        )
}

fn resolve_target(registry: &ToolRegistry, name: &str) -> Option<ExternalTarget> {
    let name = name.trim();
    if let Some(tool) =
        canonical_builtin_tool_name(name).and_then(|n| BuiltinToolName::from_name(&n))
    {
        return builtin_exposed_externally(tool).then_some(ExternalTarget::Builtin(tool));
    }
    let catalog = registry.external_tool_catalog_snapshot();
    if catalog
        .mcp_tools
        .iter()
        .any(|tool| tool.model_tool_name == name)
    {
        return Some(ExternalTarget::DownstreamMcp);
    }
    let (skill, binding) = parse_skill_custom_tool_name(name)?;
    catalog
        .skill_tools
        .iter()
        .any(|tool| tool.name == name && tool.status == "available")
        .then_some(ExternalTarget::Skill { skill, binding })
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
    let Some(target) = resolve_target(registry, call.tool_name) else {
        return rejected(call.tool_name, "该工具不对外部调用开放");
    };
    // 判定使用的工具名：内置工具用规范名，其余（下游 MCP、Skill handler）用调用名。
    let decision_name = match &target {
        ExternalTarget::Builtin(tool) => tool.as_str().to_string(),
        _ => call.tool_name.trim().to_string(),
    };

    let policy = external_tool_policy(call.access_profile, call.workspace_root);
    let workspace_root = call.workspace_root.to_path_buf();

    if let Some(gate) = safety_gate {
        publish_safety_evaluation_audit(
            event_bus,
            gate,
            &decision_name,
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
        &decision_name,
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
        let mut hook_ctx = ToolHookCtx {
            tool_call_id: call.call_id.to_string(),
            worker_id: None,
            execution_group_id: None,
            declared_paths: derive_declared_paths(&ChatToolCall {
                id: call.call_id.to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: decision_name.clone(),
                    arguments: call.arguments_json.to_string(),
                },
            }),
        };
        if let Some(hook) = call.snapshot {
            hook.before_tool(&hook_ctx);
        }
        let (payload, status) = match &target {
            ExternalTarget::Builtin(tool) => {
                let input = ToolExecutionInput::for_builtin_invocation(
                    ToolCallId::new(call.call_id),
                    tool.as_str(),
                    call.arguments_json.to_string(),
                );
                let output = registry.execute_with_policy(input, context, &tool_policy);
                (output.payload, output.status)
            }
            ExternalTarget::DownstreamMcp => registry
                .execute_external_mcp_tool(call.tool_name.trim(), call.arguments_json, effective)
                .unwrap_or_else(|| rejected(call.tool_name, "下游 MCP 工具当前不可用")),
            ExternalTarget::Skill { skill, binding } => execute_skill_custom_tool(
                &ChatToolCall {
                    id: call.call_id.to_string(),
                    kind: "function".to_string(),
                    function: ChatToolFunction {
                        name: call.tool_name.trim().to_string(),
                        arguments: call.arguments_json.to_string(),
                    },
                },
                skill,
                binding,
                None,
                tool_policy.clone(),
                safety_gate,
                call.skill_runtime,
                call.skill_dispatch_runtime,
                context,
                Some(workspace_root.display().to_string()),
            ),
        };
        append_result_declared_paths(&mut hook_ctx.declared_paths, &payload);
        if let Some(hook) = call.snapshot {
            hook.after_tool(&hook_ctx);
        }
        (payload, status)
    };

    // 与 Task 执行一致：获得授权后以 FullAccess 重入，路径与命令范围仍由策略限定。
    let effective = if approval_granted {
        AccessProfile::FullAccess
    } else {
        policy.effective_access_profile()
    };
    let result = run(effective);
    if result.1 == ExecutionResultStatus::NeedsApproval
        && (approval_granted || !approval_resume_is_safe(&result.0))
    {
        return approval_resume_contract_failure(&decision_name);
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
            snapshot: None,
            skill_runtime: None,
            skill_dispatch_runtime: None,
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

    struct StubWriteTool;

    impl magi_tool_runtime::BuiltinTool for StubWriteTool {
        fn name(&self) -> &'static str {
            "file_write"
        }

        fn execute(
            &self,
            _tool_call_id: &ToolCallId,
            _input: &str,
            _context: &ToolExecutionContext,
            _resources: &magi_tool_runtime::ToolRuntimeResources,
        ) -> String {
            serde_json::json!({ "tool": "file_write", "status": "succeeded" }).to_string()
        }

        fn spec(&self) -> magi_tool_runtime::BuiltinToolSpec {
            magi_tool_runtime::BuiltinToolSpec {
                name: "file_write".to_string(),
                risk_level: magi_core::RiskLevel::Low,
                approval_requirement: magi_core::ApprovalRequirement::None,
            }
        }
    }

    #[derive(Default)]
    struct RecordingHook {
        events: std::sync::Mutex<Vec<(String, Vec<std::path::PathBuf>)>>,
    }

    impl ToolHook for RecordingHook {
        fn before_tool(&self, ctx: &ToolHookCtx) {
            self.events
                .lock()
                .unwrap()
                .push(("before".into(), ctx.declared_paths.clone()));
        }

        fn after_tool(&self, ctx: &ToolHookCtx) {
            self.events
                .lock()
                .unwrap()
                .push(("after".into(), ctx.declared_paths.clone()));
        }
    }

    #[test]
    fn write_runs_inside_the_ledger_hook_and_rejected_write_does_not() {
        let (bus, mut reg) = registry();
        reg.register_builtin(std::sync::Arc::new(StubWriteTool));
        let session = SessionId::new("s");
        let root = Path::new("/work/space");
        let workspace = WorkspaceId::new("ws-1");
        let hook = RecordingHook::default();
        let args = r#"{"path":"/work/space/a.txt","content":"x"}"#;
        let mut write_call = call(
            "file_write",
            args,
            root,
            AccessProfile::Restricted,
            &session,
        );
        write_call.snapshot = Some(&hook);
        write_call.workspace_id = Some(&workspace);

        // Magi 的 Restricted 档本身允许工作区内写入，“是否需要人工确认”由 MCP 层的
        // 权限档决策负责，本入口不重复询问。
        let (_, status) = execute_external_tool_call(&bus, &reg, None, &write_call, true);
        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let declared = vec![std::path::PathBuf::from("/work/space/a.txt")];
        assert_eq!(
            *hook.events.lock().unwrap(),
            vec![
                ("before".to_string(), declared.clone()),
                ("after".to_string(), declared)
            ]
        );

        let outside = r#"{"path":"/etc/evil","content":"x"}"#;
        let mut rejected_call = call(
            "file_write",
            outside,
            root,
            AccessProfile::Restricted,
            &session,
        );
        let rejected_hook = RecordingHook::default();
        rejected_call.snapshot = Some(&rejected_hook);
        rejected_call.workspace_id = Some(&workspace);
        let (_, status) = execute_external_tool_call(&bus, &reg, None, &rejected_call, true);
        assert_eq!(status, ExecutionResultStatus::Rejected);
        assert!(
            rejected_hook.events.lock().unwrap().is_empty(),
            "被拒绝的调用不得触碰账本"
        );
    }
}
