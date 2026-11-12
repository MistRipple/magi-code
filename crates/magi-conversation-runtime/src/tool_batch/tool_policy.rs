use super::*;

pub(super) const TOOL_VISIBILITY_REJECTED_PUBLIC_ERROR: &str = "该工具在当前任务角色或阶段下不可用";

pub(super) const TOOL_POLICY_CONTEXT_REJECTED_PUBLIC_ERROR: &str = "该工具受当前任务角色或策略限制";

pub(super) const TOOL_POLICY_NEEDS_APPROVAL_PUBLIC_ERROR: &str =
    "该操作需要你的确认，授权后将继续当前调用";

pub(crate) struct ToolPreflightDecision {
    pub(crate) payload: String,
    pub(crate) status: ExecutionResultStatus,
}

pub(super) fn project_directory_rejection(
    registry: &TaskExecutionRegistry,
    task: &magi_core::Task,
    workspace_id: &Option<WorkspaceId>,
    tool: &str,
) -> Option<(String, ExecutionResultStatus)> {
    let error = registry
        .validate_project_directories(task, workspace_id)
        .err()?;
    Some((
        serde_json::json!({
            "tool": tool, "status": "rejected", "error_code": "project_directory_invalid",
            "execution_started": false, "error": format!("原任务的源文件夹授权已失效：{error}"),
        })
        .to_string(),
        ExecutionResultStatus::Rejected,
    ))
}

pub(super) fn tool_approval_policy_snapshot(
    task: &magi_core::Task,
    directories: Option<&magi_core::ProjectDirectoryExecutionMap>,
) -> serde_json::Value {
    let mut policy = serde_json::json!({"policy":task.policy_snapshot,"workspaceScope":task.workspace_scope,"writeScope":task.write_scope});
    if let Some(binding) = task
        .executor_binding
        .as_ref()
        .and_then(|executor| executor.project_directories.as_ref())
    {
        policy["projectDirectories"] = serde_json::json!(binding);
        policy["projectExecutionMap"] = serde_json::json!(directories);
    }
    policy
}

pub(super) fn task_tool_execution_policy_scope_with_access_profile(
    task: &magi_core::Task,
    access_profile: AccessProfile,
) -> ToolExecutionPolicy {
    let Some(policy) = task.policy_snapshot.as_ref() else {
        return tool_execution_policy_scope(access_profile, "", &[], &[]);
    };
    let mut tool_policy = tool_execution_policy_scope(
        access_profile,
        policy.command_mode.clone(),
        &policy.allowed_paths,
        &policy.denied_paths,
    );
    tool_policy.read_only_paths = policy.read_only_paths.clone();
    tool_policy
}

#[cfg(test)]
pub(super) fn task_policy_tool_decision(
    task: &magi_core::Task,
    requested_tool_name: &str,
    arguments: &str,
) -> Option<ToolPreflightDecision> {
    policy_tool_decision(
        task.policy_snapshot.as_ref()?,
        requested_tool_name,
        arguments,
        None,
    )
}

#[cfg(test)]
pub(super) fn task_tool_preflight_decision(
    task: &magi_core::Task,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    requested_tool_name: &str,
    arguments: &str,
    workspace_root_path: Option<&PathBuf>,
) -> Option<ToolPreflightDecision> {
    policy_tool_preflight_decision(
        task.policy_snapshot.as_ref(),
        safety_gate,
        requested_tool_name,
        arguments,
        workspace_root_path,
    )
}

#[cfg(test)]
pub(crate) fn policy_tool_preflight_decision(
    policy_snapshot: Option<&TaskPolicy>,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    requested_tool_name: &str,
    arguments: &str,
    workspace_root_path: Option<&PathBuf>,
) -> Option<ToolPreflightDecision> {
    policy_tool_preflight_decision_in_context(
        policy_snapshot,
        safety_gate,
        requested_tool_name,
        arguments,
        workspace_root_path,
        None,
    )
}

pub(crate) fn policy_tool_preflight_decision_in_context(
    policy_snapshot: Option<&TaskPolicy>,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    requested_tool_name: &str,
    arguments: &str,
    workspace_root_path: Option<&PathBuf>,
    context: Option<&ToolExecutionContext>,
) -> Option<ToolPreflightDecision> {
    let task_policy_decision = policy_snapshot.and_then(|policy| {
        policy_tool_decision_in_context(
            policy,
            requested_tool_name,
            arguments,
            workspace_root_path,
            context,
        )
    });
    // S8：SafetyGate 语义判定。它和 TaskPolicy 都属于执行前判定：
    // HardBlock 必须压过常规风险拦截，TaskPolicy 的 Rejected 也不能被 SafetyGate
    // 的 NeedsApproval 降级。
    let access_profile = policy_snapshot
        .map(magi_core::TaskPolicy::effective_access_profile)
        .unwrap_or_default();
    let safety_gate_decision = safety_gate.and_then(|gate| {
        safety_gate_tool_decision(gate, access_profile, requested_tool_name, arguments)
    });
    select_preflight_decision(task_policy_decision, safety_gate_decision)
}

#[cfg(test)]
pub(super) fn task_policy_tool_decision_with_workspace_root(
    task: &magi_core::Task,
    requested_tool_name: &str,
    arguments: &str,
    workspace_root_path: Option<&PathBuf>,
) -> Option<ToolPreflightDecision> {
    policy_tool_decision(
        task.policy_snapshot.as_ref()?,
        requested_tool_name,
        arguments,
        workspace_root_path,
    )
}

/// 按策略快照做访问档判定。Task 执行与外部（MCP）调用共用这一份判定，
/// 外部调用使用合成的 `TaskPolicy`，不需要构造 Task。
pub(crate) fn policy_tool_decision(
    policy_snapshot: &TaskPolicy,
    requested_tool_name: &str,
    arguments: &str,
    workspace_root_path: Option<&PathBuf>,
) -> Option<ToolPreflightDecision> {
    policy_tool_decision_in_context(
        policy_snapshot,
        requested_tool_name,
        arguments,
        workspace_root_path,
        None,
    )
}

pub(super) fn policy_tool_decision_in_context(
    policy_snapshot: &TaskPolicy,
    requested_tool_name: &str,
    arguments: &str,
    workspace_root_path: Option<&PathBuf>,
    context: Option<&ToolExecutionContext>,
) -> Option<ToolPreflightDecision> {
    let canonical_tool_name = canonical_builtin_tool_name(requested_tool_name)
        .unwrap_or_else(|| requested_tool_name.trim().to_string());
    // no_tools 是 PermissionEngine 三维之外的全局开关，本层先单独拦截。
    if policy_snapshot
        .command_mode
        .eq_ignore_ascii_case("no_tools")
    {
        return Some(task_policy_decision_payload(
            &canonical_tool_name,
            ExecutionResultStatus::Rejected,
            format!("当前任务阶段不允许调用工具: {canonical_tool_name}"),
            Some(policy_snapshot.access_profile),
        ));
    }

    access_profile_tool_decision_in_context(
        AccessProfileToolDecisionInput {
            access_profile: policy_snapshot.access_profile,
            command_mode: &policy_snapshot.command_mode,
            allowed_tools: &policy_snapshot.allowed_tools,
            denied_tools: &policy_snapshot.denied_tools,
            allowed_paths: &policy_snapshot.allowed_paths,
            denied_paths: &policy_snapshot.denied_paths,
            read_only_paths: &policy_snapshot.read_only_paths,
            requested_tool_name,
            arguments,
            workspace_root_path,
        },
        context,
    )
}

pub(crate) struct AccessProfileToolDecisionInput<'a> {
    pub access_profile: magi_core::AccessProfile,
    pub command_mode: &'a str,
    pub allowed_tools: &'a [String],
    pub denied_tools: &'a [String],
    pub allowed_paths: &'a [String],
    pub denied_paths: &'a [String],
    pub read_only_paths: &'a [String],
    pub requested_tool_name: &'a str,
    pub arguments: &'a str,
    pub workspace_root_path: Option<&'a PathBuf>,
}

#[cfg(test)]
pub(crate) fn access_profile_tool_decision(
    input: AccessProfileToolDecisionInput<'_>,
) -> Option<ToolPreflightDecision> {
    access_profile_tool_decision_in_context(input, None)
}

pub(super) fn access_profile_tool_decision_in_context(
    input: AccessProfileToolDecisionInput<'_>,
    context: Option<&ToolExecutionContext>,
) -> Option<ToolPreflightDecision> {
    let AccessProfileToolDecisionInput {
        access_profile,
        command_mode,
        allowed_tools,
        denied_tools,
        allowed_paths,
        denied_paths,
        read_only_paths,
        requested_tool_name,
        arguments,
        workspace_root_path,
    } = input;
    let effective_access_profile = access_profile.constrained_by_command_mode(command_mode);
    let canonical_tool_name = canonical_builtin_tool_name(requested_tool_name)
        .unwrap_or_else(|| requested_tool_name.trim().to_string());
    // PermissionEngine 比对工具名是按字面比对，因此把 policy 中的别名先 canonical 化。
    let mut canonical_policy = magi_permissions::PermissionPolicy {
        allowed_tools: allowed_tools
            .iter()
            .map(|tool| {
                canonical_builtin_tool_name(tool).unwrap_or_else(|| tool.trim().to_string())
            })
            .collect(),
        denied_tools: denied_tools
            .iter()
            .map(|tool| {
                canonical_builtin_tool_name(tool).unwrap_or_else(|| tool.trim().to_string())
            })
            .collect(),
        allowed_paths: effective_tool_policy_allowed_paths(
            effective_access_profile,
            allowed_paths,
            workspace_root_path.map(|path| path.as_path()),
        ),
        denied_paths: normalize_tool_policy_paths(
            denied_paths,
            workspace_root_path.map(|path| path.as_path()),
        ),
        command_mode: command_mode.to_string(),
    };
    if let Some(context) = context {
        canonical_policy = magi_tool_runtime::execution_permission_policy(
            &ToolExecutionPolicy {
                access_profile,
                command_mode: command_mode.into(),
                allowed_tool_names: (!allowed_tools.is_empty()).then(|| allowed_tools.to_vec()),
                denied_tool_names: denied_tools.to_vec(),
                allowed_paths: allowed_paths.to_vec(),
                denied_paths: denied_paths.to_vec(),
                read_only_paths: read_only_paths.to_vec(),
                ..Default::default()
            },
            context,
        );
    }
    let engine = builtin_permission_engine();
    let is_write_tool = BuiltinToolName::from_name(canonical_tool_name.as_str())
        .is_some_and(|tool| tool.is_access_profile_write_operation());
    let mut pending_decision = None;

    let tool_request = magi_permissions::PermissionRequest::ToolInvocation {
        tool_name: canonical_tool_name.as_str(),
        is_write_tool,
    };
    if let Some(decision) = select_access_profile_axis_decision(
        &mut pending_decision,
        permission_decision_payload(
            &canonical_tool_name,
            engine.decide(&tool_request, &canonical_policy, effective_access_profile),
            effective_access_profile,
        ),
    ) {
        return Some(decision);
    }
    // shell_exec 在只读任务下需要 access_mode=read_only —— 走 ShellCommand 轴判定。
    if canonical_tool_name == BuiltinToolName::ShellExec.as_str() {
        let shell_request = magi_permissions::PermissionRequest::ShellCommand {
            arguments_json: arguments,
        };
        if let Some(decision) = select_access_profile_axis_decision(
            &mut pending_decision,
            permission_decision_payload(
                &canonical_tool_name,
                engine.decide(&shell_request, &canonical_policy, effective_access_profile),
                effective_access_profile,
            ),
        ) {
            return Some(decision);
        }
    }
    // 浏览器工具对网页的写动作按访问档位判定：只读拒绝，受限下执行脚本 / 改站点存储需要确认。
    if let Some(class) = magi_tool_runtime::browser_action_class(&canonical_tool_name, arguments) {
        let browser_request = magi_permissions::PermissionRequest::BrowserAction { class };
        if let Some(decision) = select_access_profile_axis_decision(
            &mut pending_decision,
            permission_decision_payload(
                &canonical_tool_name,
                engine.decide(
                    &browser_request,
                    &canonical_policy,
                    effective_access_profile,
                ),
                effective_access_profile,
            ),
        ) {
            return Some(decision);
        }
    }
    let normalized_read_only_paths = normalize_tool_policy_paths(
        read_only_paths,
        workspace_root_path.map(|path| path.as_path()),
    );
    for path_request in tool_path_access_requests(
        &canonical_tool_name,
        arguments,
        workspace_root_path.map(|path| path.as_path()),
        effective_access_profile,
    ) {
        if context.is_some_and(|context| {
            context
                .references_retired_project_root(&path_request.absolute_path)
                .unwrap_or(true)
        }) {
            return Some(task_policy_decision_payload(
                &canonical_tool_name,
                ExecutionResultStatus::Rejected,
                "路径指向已隔离的源目录，工具未执行".into(),
                Some(effective_access_profile),
            ));
        }
        if path_request.kind == magi_permissions::PathAccessKind::Write
            && normalized_read_only_paths
                .iter()
                .any(|root| path_request.absolute_path.starts_with(root))
        {
            return Some(task_policy_decision_payload(
                &canonical_tool_name,
                ExecutionResultStatus::Rejected,
                "上下文引用只允许读取".to_string(),
                Some(effective_access_profile),
            ));
        }
        if let Some(decision) = select_access_profile_axis_decision(
            &mut pending_decision,
            permission_decision_payload(
                &canonical_tool_name,
                magi_tool_runtime::decide_tool_path_access(
                    &engine,
                    &canonical_policy,
                    canonical_tool_name.as_str(),
                    &path_request,
                    allowed_paths,
                    effective_access_profile,
                ),
                effective_access_profile,
            ),
        ) {
            return Some(decision);
        }
    }
    pending_decision
}

pub(super) fn select_access_profile_axis_decision(
    pending_decision: &mut Option<ToolPreflightDecision>,
    decision: Option<ToolPreflightDecision>,
) -> Option<ToolPreflightDecision> {
    match decision {
        Some(decision) if decision.status == ExecutionResultStatus::Rejected => Some(decision),
        Some(decision) => {
            if pending_decision.is_none() {
                *pending_decision = Some(decision);
            }
            None
        }
        None => None,
    }
}

pub(crate) fn select_preflight_decision(
    task_policy_decision: Option<ToolPreflightDecision>,
    safety_gate_decision: Option<ToolPreflightDecision>,
) -> Option<ToolPreflightDecision> {
    match (task_policy_decision, safety_gate_decision) {
        (Some(policy), Some(safety)) => match (policy.status, safety.status) {
            (_, ExecutionResultStatus::Rejected) => Some(safety),
            (ExecutionResultStatus::Rejected, _) => Some(policy),
            (_, ExecutionResultStatus::NeedsApproval) => Some(safety),
            (ExecutionResultStatus::NeedsApproval, _) => Some(policy),
            _ => Some(policy),
        },
        (Some(policy), None) => Some(policy),
        (None, Some(safety)) => Some(safety),
        (None, None) => None,
    }
}

pub(super) fn permission_decision_payload(
    tool_name: &str,
    decision: magi_permissions::Decision,
    access_profile: magi_core::AccessProfile,
) -> Option<ToolPreflightDecision> {
    match decision {
        magi_permissions::Decision::Allow => None,
        magi_permissions::Decision::Deny { reason } => Some(task_policy_decision_payload(
            tool_name,
            ExecutionResultStatus::Rejected,
            reason,
            Some(access_profile),
        )),
        magi_permissions::Decision::NeedsApproval { reason } => Some(task_policy_decision_payload(
            tool_name,
            ExecutionResultStatus::NeedsApproval,
            reason,
            Some(access_profile),
        )),
    }
}

pub(super) fn task_policy_decision_payload(
    tool_name: &str,
    status: ExecutionResultStatus,
    reason: String,
    access_profile: Option<magi_core::AccessProfile>,
) -> ToolPreflightDecision {
    let restriction_kind = policy_restriction_kind(status, &reason);
    let constrained_by_skill = restriction_kind == "skill_tool_scope";
    let constrained_by_access_profile = reason.starts_with("只读任务")
        || reason.starts_with("受限执行")
        || reason.starts_with("只读访问");
    let (error_code, public_error, required_access_profile, instruction) = match status {
        ExecutionResultStatus::NeedsApproval => (
            "tool_policy_needs_approval",
            TOOL_POLICY_NEEDS_APPROVAL_PUBLIC_ERROR,
            Some("full_access"),
            "当前调用已暂停并等待用户授权，不要重复调用。若用户拒绝，再改用专用读取、搜索工具或缩小操作范围。",
        ),
        ExecutionResultStatus::Rejected => (
            "tool_policy_rejected",
            if constrained_by_skill {
                "当前激活 Skill 不允许调用该工具"
            } else if !constrained_by_access_profile
                || access_profile == Some(magi_core::AccessProfile::FullAccess)
            {
                TOOL_POLICY_CONTEXT_REJECTED_PUBLIC_ERROR
            } else {
                "该工具在当前访问模式下不可用"
            },
            constrained_by_access_profile.then_some("full_access"),
            if constrained_by_skill {
                "不要重复相同调用。请使用当前 Skill 明确允许的工具；Skill 被移除时需要重新选择有效 Skill。"
            } else if constrained_by_access_profile {
                "不要重复相同调用。只读目的必须改用专用读取、搜索工具；需要写入时应等待当前授权结果。"
            } else {
                "不要重复相同调用。该工具受当前任务角色、路径范围或显式策略限制，请使用当前任务允许的工具和路径。"
            },
        ),
        _ => (
            "tool_policy_failed",
            "该工具暂不可用",
            None,
            "不要重复相同调用，请改用当前可用工具继续。",
        ),
    };
    tracing::warn!(
        tool_name,
        status = %status.wire_label(),
        access_profile = access_profile.map(|profile| profile.as_str()).unwrap_or_default(),
        reason = %reason,
        "tool preflight policy decision"
    );
    ToolPreflightDecision {
        payload: serde_json::json!({
            "tool": tool_name,
            "status": status.wire_label(),
            "error_code": error_code,
            "error": public_error,
            "access_profile": access_profile.map(|profile| profile.as_str()),
            "required_access_profile": required_access_profile,
            "restriction_kind": restriction_kind,
            "retryable_with_same_arguments": false,
            "instruction": instruction,
        })
        .to_string(),
        status,
    }
}

pub(super) fn policy_restriction_kind(status: ExecutionResultStatus, reason: &str) -> &'static str {
    if status == ExecutionResultStatus::NeedsApproval {
        return "approval_required";
    }
    if reason.starts_with("tool_not_in_allowed_scope:")
        || reason.starts_with("tool_explicitly_denied:")
    {
        return "skill_tool_scope";
    }
    if reason.starts_with("只读任务") || reason.starts_with("只读访问") {
        return "read_only";
    }
    if reason.starts_with("策略拒绝访问路径") || reason.starts_with("策略未授权访问路径")
    {
        return "path_scope";
    }
    if reason.starts_with("任务策略拒绝工具") || reason.starts_with("任务策略未授权工具")
    {
        return "tool_scope";
    }
    "policy"
}

pub(super) fn task_tool_visibility_decision_payload(
    tool_name: &str,
    task: &magi_core::Task,
) -> ToolPreflightDecision {
    let task_tier = task
        .policy_snapshot
        .as_ref()
        .map(|policy| format!("{:?}", policy.task_tier))
        .unwrap_or_else(|| "unknown".to_string());
    tracing::warn!(
        tool_name,
        task_id = %task.task_id,
        task_tier = %task_tier,
        "tool preflight visibility decision"
    );
    ToolPreflightDecision {
        payload: serde_json::json!({
            "tool": tool_name,
            "status": ExecutionResultStatus::Rejected.wire_label(),
            "error_code": "tool_policy_rejected",
            "error": TOOL_VISIBILITY_REJECTED_PUBLIC_ERROR,
            "reason_code": "model_call_scope_mismatch",
            "model_call_scope": magi_tool_runtime::BuiltinToolName::from_name(tool_name)
                .map(|tool| tool.model_call_scope().as_str()),
            "task_id": task.task_id,
            "retryable_with_same_arguments": false,
            "instruction": "不要重复相同调用，请改用当前可用工具或直接返回文本结果。",
        })
        .to_string(),
        status: ExecutionResultStatus::Rejected,
    }
}

/// S8：SafetyGate 的 HardBlock / RequireApprovalInRestricted 都是执行前判定。
/// HardBlock 代表禁止执行；RequireApprovalInRestricted 代表受限模式拦截、完全访问审计放行。
pub(crate) fn safety_gate_tool_decision(
    gate: &magi_safety_gate::SafetyGate,
    access_profile: magi_core::AccessProfile,
    tool_name: &str,
    arguments: &str,
) -> Option<ToolPreflightDecision> {
    let canonical_tool_name =
        canonical_builtin_tool_name(tool_name).unwrap_or_else(|| tool_name.trim().to_string());
    match gate.evaluate(&canonical_tool_name, arguments) {
        magi_safety_gate::SafetyDecision::Allow => None,
        magi_safety_gate::SafetyDecision::AuditOnly { .. } => None,
        magi_safety_gate::SafetyDecision::HardBlock {
            category,
            pattern,
            reason,
        } => Some(safety_gate_decision_payload(
            &canonical_tool_name,
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
            magi_core::AccessProfile::FullAccess => None,
            magi_core::AccessProfile::Restricted => Some(safety_gate_decision_payload(
                &canonical_tool_name,
                ExecutionResultStatus::NeedsApproval,
                category,
                magi_safety_gate::SafetyAction::RequireApprovalInRestricted,
                pattern,
                reason,
            )),
            magi_core::AccessProfile::ReadOnly => Some(safety_gate_decision_payload(
                &canonical_tool_name,
                ExecutionResultStatus::Rejected,
                category,
                magi_safety_gate::SafetyAction::RequireApprovalInRestricted,
                pattern,
                format!("{reason}；只读分析模式不支持升级访问模式执行"),
            )),
        },
    }
}

pub(crate) struct SafetyEvaluationAuditContext<'a> {
    pub access_profile: magi_core::AccessProfile,
    pub session_id: Option<&'a SessionId>,
    pub workspace_id: Option<&'a WorkspaceId>,
    pub mission_id: Option<&'a magi_core::MissionId>,
    pub task_id: Option<&'a TaskId>,
}

pub(crate) fn publish_safety_evaluation_audit(
    event_bus: &InMemoryEventBus,
    gate: &magi_safety_gate::SafetyGate,
    tool_name: &str,
    arguments: &str,
    tool_call_id: &str,
    context: SafetyEvaluationAuditContext<'_>,
) {
    let canonical_tool_name =
        canonical_builtin_tool_name(tool_name).unwrap_or_else(|| tool_name.trim().to_string());
    let evaluation = gate.evaluate_with_evidence(&canonical_tool_name, arguments);
    if evaluation.matches.is_empty() {
        return;
    }

    let decision = match &evaluation.decision {
        magi_safety_gate::SafetyDecision::Allow => "allow",
        magi_safety_gate::SafetyDecision::HardBlock { .. } => "hard_block",
        magi_safety_gate::SafetyDecision::RequireApprovalInRestricted { .. } => {
            "require_approval_in_restricted"
        }
        magi_safety_gate::SafetyDecision::AuditOnly { .. } => "audit_only",
    };
    let _ = event_bus.publish(
        EventEnvelope::audit(
            EventId::unique(format!("event-safety-evaluation-{}", tool_call_id)),
            "security.safety.evaluated",
            serde_json::json!({
                "tool_name": canonical_tool_name,
                "tool_call_id": tool_call_id,
                "access_profile": format!("{:?}", context.access_profile),
                "decision": decision,
                "matched_rules": evaluation.matches.iter().map(|matched| serde_json::json!({
                    "category": matched.category.as_str(),
                    "pattern": matched.pattern,
                    "action": matched.action.as_str(),
                })).collect::<Vec<_>>(),
            }),
        )
        .with_context(EventContext {
            workspace_id: context.workspace_id.cloned(),
            session_id: context.session_id.cloned(),
            mission_id: context.mission_id.cloned(),
            task_id: context.task_id.cloned(),
            ..EventContext::default()
        }),
    );
}

pub(super) fn safety_gate_decision_payload(
    tool_name: &str,
    status: ExecutionResultStatus,
    category: magi_safety_gate::SafetyCategory,
    action: magi_safety_gate::SafetyAction,
    pattern: String,
    reason: String,
) -> ToolPreflightDecision {
    let public_error = safety_gate_public_error(status);
    tracing::warn!(
        tool_name,
        status = %status.wire_label(),
        category = category.as_str(),
        action = action.as_str(),
        pattern = %pattern,
        reason = %reason,
        "tool preflight safety gate decision"
    );
    ToolPreflightDecision {
        payload: serde_json::json!({
            "tool": tool_name,
            "status": status.wire_label(),
            "error_code": public_error.error_code,
            "error": public_error.error,
            "retryable_with_same_arguments": status == ExecutionResultStatus::Rejected,
            "instruction": if status == ExecutionResultStatus::Rejected {
                "该安全策略拒绝不可重试的相同调用，请改用安全替代方案。"
            } else {
                "当前调用已暂停并等待用户授权，不要重复调用。"
            },
        })
        .to_string(),
        status,
    }
}
