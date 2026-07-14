use crate::{
    BuiltinToolAccessMode, ToolExecutionContext, ToolExecutionInput, ToolExecutionOutput,
    ToolExecutionPolicy, ToolRegistry, WriteProtectionScope,
    apply_patch::apply_patch_declared_paths_from_input,
    builtin::{field_string, parse_json_object, resolve_path_with_context},
    tool_policy_decision_payload,
};
use magi_core::{AccessProfile, ExecutionResultStatus, ToolCallId, UtcMillis};
use magi_governance::{DecisionPhase, GovernanceDecision, GovernanceOutcome};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    sync::{Arc, RwLock},
};

const WRITE_CONFLICT_PUBLIC_ERROR: &str = "检测到并发写冲突，请稍后重试";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WriteProtectionClaim {
    pub(crate) tool_call_id: ToolCallId,
    pub(crate) scope: WriteProtectionScope,
    pub(crate) access_mode: BuiltinToolAccessMode,
    pub(crate) acquired_at: UtcMillis,
}

#[derive(Debug)]
pub(crate) struct WriteProtectionGuard {
    pub(crate) active_claims: Arc<RwLock<HashMap<ToolCallId, WriteProtectionClaim>>>,
    pub(crate) tool_call_id: ToolCallId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolPathAccessRequest {
    pub absolute_path: PathBuf,
    pub kind: magi_permissions::PathAccessKind,
}

impl Drop for WriteProtectionGuard {
    fn drop(&mut self) {
        self.active_claims
            .write()
            .expect("write protection write lock poisoned")
            .remove(&self.tool_call_id);
    }
}

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
                format!("skill runtime 已显式拒绝工具: {}", input.tool_name),
                policy.effective_access_profile(),
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
                format!("skill runtime 未授权工具: {}", input.tool_name),
                policy.effective_access_profile(),
            ));
        }

        if policy.allowed_tool_names.is_empty() {
            return Some(self.build_policy_rejection(
                input,
                format!("skill runtime 未授权工具: {}", input.tool_name),
                policy.effective_access_profile(),
            ));
        }

        None
    }

    pub(crate) fn build_policy_rejection(
        &self,
        input: &ToolExecutionInput,
        reason: String,
        access_profile: AccessProfile,
    ) -> ToolExecutionOutput {
        ToolExecutionOutput {
            tool_call_id: input.tool_call_id.clone(),
            status: ExecutionResultStatus::Rejected,
            payload: tool_policy_decision_payload(
                &input.tool_name,
                ExecutionResultStatus::Rejected,
                &reason,
                access_profile,
            ),
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

    pub(crate) fn resolve_access_mode(&self, input: &ToolExecutionInput) -> BuiltinToolAccessMode {
        let Some(tool_name) = crate::BuiltinToolName::from_name(input.tool_name.trim()) else {
            return BuiltinToolAccessMode::ReadOnly;
        };
        let default_access_mode = tool_name.default_access_mode();
        if default_access_mode == BuiltinToolAccessMode::MaybeWrite {
            return self
                .parse_requested_access_mode(&input.input)
                .unwrap_or(default_access_mode);
        }
        default_access_mode
    }

    pub(crate) fn parse_requested_access_mode(&self, input: &str) -> Option<BuiltinToolAccessMode> {
        parse_json_object(input).and_then(|object| {
            field_string(&object, &["access_mode"])
                .and_then(|value| BuiltinToolAccessMode::from_str(&value))
        })
    }

    pub(crate) fn enforce_access_profile_policy(
        &self,
        input: &ToolExecutionInput,
        context: &ToolExecutionContext,
        policy: &ToolExecutionPolicy,
        access_mode: BuiltinToolAccessMode,
    ) -> Option<ToolExecutionOutput> {
        let policy = normalize_execution_policy(policy);
        let effective_access_profile = policy.effective_access_profile();
        let workspace_root_path = context.working_directory.as_deref();
        let engine = crate::builtin_permission_engine();
        let permission_policy = magi_permissions::PermissionPolicy {
            allowed_tools: policy
                .allowed_tool_names
                .iter()
                .map(|tool_name| {
                    crate::canonical_builtin_tool_name(tool_name)
                        .unwrap_or_else(|| tool_name.trim().to_string())
                })
                .collect(),
            denied_tools: policy
                .denied_tool_names
                .iter()
                .map(|tool_name| {
                    crate::canonical_builtin_tool_name(tool_name)
                        .unwrap_or_else(|| tool_name.trim().to_string())
                })
                .collect(),
            allowed_paths: effective_tool_policy_allowed_paths(
                effective_access_profile,
                &policy.allowed_paths,
                workspace_root_path,
            ),
            denied_paths: normalize_tool_policy_paths(&policy.denied_paths, workspace_root_path),
            command_mode: policy.command_mode.clone(),
        };
        let mut pending_output = None;

        let tool_is_writeful = if input.tool_name == crate::BuiltinToolName::ShellExec.as_str() {
            false
        } else {
            access_mode.is_writeful()
        };
        let tool_decision = engine.decide(
            &magi_permissions::PermissionRequest::ToolInvocation {
                tool_name: &input.tool_name,
                is_write_tool: tool_is_writeful,
            },
            &permission_policy,
            effective_access_profile,
        );
        if let Some(output) = select_permission_axis_output(
            &mut pending_output,
            self.permission_decision_output(input, tool_decision, effective_access_profile),
        ) {
            return Some(output);
        }

        if input.tool_name == crate::BuiltinToolName::ShellExec.as_str() {
            let shell_decision = engine.decide(
                &magi_permissions::PermissionRequest::ShellCommand {
                    arguments_json: &input.input,
                },
                &permission_policy,
                effective_access_profile,
            );
            if let Some(output) = select_permission_axis_output(
                &mut pending_output,
                self.permission_decision_output(input, shell_decision, effective_access_profile),
            ) {
                return Some(output);
            }
        }

        let read_only_paths =
            normalize_tool_policy_paths(&policy.read_only_paths, workspace_root_path);
        for path_request in tool_path_access_requests(
            &input.tool_name,
            &input.input,
            workspace_root_path,
            effective_access_profile,
        ) {
            if path_request.kind == magi_permissions::PathAccessKind::Write
                && read_only_paths
                    .iter()
                    .any(|root| path_request.absolute_path.starts_with(root))
            {
                return Some(self.build_policy_rejection(
                    input,
                    "上下文引用只允许读取".to_string(),
                    effective_access_profile,
                ));
            }
            let path_decision = engine.decide(
                &magi_permissions::PermissionRequest::PathAccess {
                    absolute_path: path_request.absolute_path.as_path(),
                    kind: path_request.kind,
                },
                &permission_policy,
                effective_access_profile,
            );
            if let Some(output) = select_permission_axis_output(
                &mut pending_output,
                self.permission_decision_output(input, path_decision, effective_access_profile),
            ) {
                return Some(output);
            }
        }

        pending_output
    }

    fn permission_decision_output(
        &self,
        input: &ToolExecutionInput,
        decision: magi_permissions::Decision,
        access_profile: AccessProfile,
    ) -> Option<ToolExecutionOutput> {
        match decision {
            magi_permissions::Decision::Allow => None,
            magi_permissions::Decision::Deny { reason } => Some(ToolExecutionOutput {
                tool_call_id: input.tool_call_id.clone(),
                status: ExecutionResultStatus::Rejected,
                payload: tool_policy_decision_payload(
                    &input.tool_name,
                    ExecutionResultStatus::Rejected,
                    &reason,
                    access_profile,
                ),
                governance: GovernanceDecision::rejected(
                    DecisionPhase::ToolPolicy,
                    input.risk_level,
                    Some(reason),
                ),
            }),
            magi_permissions::Decision::NeedsApproval { reason } => Some(ToolExecutionOutput {
                tool_call_id: input.tool_call_id.clone(),
                status: ExecutionResultStatus::NeedsApproval,
                payload: tool_policy_decision_payload(
                    &input.tool_name,
                    ExecutionResultStatus::NeedsApproval,
                    &reason,
                    access_profile,
                ),
                governance: GovernanceDecision::needs_approval(
                    DecisionPhase::ApprovalPolicy,
                    input.risk_level,
                    Some(reason),
                ),
            }),
        }
    }

    pub(crate) fn acquire_write_guard(
        &self,
        input: &ToolExecutionInput,
        context: &ToolExecutionContext,
        access_mode: BuiltinToolAccessMode,
    ) -> Result<Option<WriteProtectionGuard>, ToolExecutionOutput> {
        if !access_mode.is_writeful() {
            return Ok(None);
        }

        let scope = match self.build_write_scope(input, context, access_mode) {
            Some(scope) => scope,
            None => return Ok(None),
        };

        let claim = WriteProtectionClaim {
            tool_call_id: input.tool_call_id.clone(),
            scope: scope.clone(),
            access_mode,
            acquired_at: UtcMillis::now(),
        };

        let mut active_claims = self
            .active_write_claims
            .write()
            .expect("write protection write lock poisoned");
        if let Some(conflict) = active_claims
            .values()
            .find(|existing| existing.conflicts_with(&scope))
            .cloned()
        {
            return Err(self.build_write_conflict_rejection(input, access_mode, scope, conflict));
        }

        active_claims.insert(input.tool_call_id.clone(), claim);
        Ok(Some(WriteProtectionGuard {
            active_claims: Arc::clone(&self.active_write_claims),
            tool_call_id: input.tool_call_id.clone(),
        }))
    }

    pub(crate) fn build_write_scope(
        &self,
        input: &ToolExecutionInput,
        context: &ToolExecutionContext,
        access_mode: BuiltinToolAccessMode,
    ) -> Option<WriteProtectionScope> {
        if !access_mode.is_writeful() {
            return None;
        }

        let request = parse_json_object(&input.input);
        let mut paths = Vec::new();

        if crate::BuiltinToolName::from_name(input.tool_name.trim())
            == Some(crate::BuiltinToolName::ApplyPatch)
        {
            for path_value in apply_patch_declared_paths_from_input(&input.input) {
                if let Ok(path) = resolve_path_with_context(&path_value.to_string_lossy(), context)
                {
                    paths.push(normalize_path_for_lock(&path));
                }
            }
        }

        if let Some(object) = &request {
            for key in ["path", "source", "destination", "cwd", "root"] {
                if let Some(value) = field_string(object, &[key])
                    && let Ok(path) = resolve_path_with_context(&value, context)
                {
                    paths.push(normalize_path_for_lock(&path));
                }
            }
        }

        paths.sort();
        paths.dedup();

        let working_directory = if input.tool_name == crate::BuiltinToolName::ShellExec.as_str() {
            request
                .as_ref()
                .and_then(|object| field_string(object, &["cwd"]))
                .map(|value| {
                    resolve_path_with_context(&value, context)
                        .map(|path| normalize_path_for_lock(&path))
                })
                .unwrap_or_else(|| {
                    context
                        .working_directory
                        .clone()
                        .or_else(|| std::env::current_dir().ok())
                        .map(|path| normalize_path_for_lock(&path))
                        .ok_or_else(|| "无法解析当前工作目录".to_string())
                })
                .ok()
        } else {
            None
        };

        if context.workspace_id.is_none()
            && context.task_id.is_none()
            && working_directory.is_none()
            && paths.is_empty()
        {
            return None;
        }

        Some(WriteProtectionScope {
            workspace_id: context.workspace_id.clone(),
            session_id: context.session_id.clone(),
            task_id: context.task_id.clone(),
            working_directory,
            paths,
        })
    }

    pub(crate) fn build_write_conflict_rejection(
        &self,
        input: &ToolExecutionInput,
        access_mode: BuiltinToolAccessMode,
        scope: WriteProtectionScope,
        conflict: WriteProtectionClaim,
    ) -> ToolExecutionOutput {
        let reason = format!(
            "检测到并发写冲突: tool={} workspace={:?} session={:?} task={:?} cwd={:?} paths={} conflict_tool_call={} conflict_workspace={:?} conflict_session={:?} conflict_task={:?} conflict_cwd={:?} conflict_paths={}",
            input.tool_name,
            scope.workspace_id.as_ref().map(ToString::to_string),
            scope.session_id.as_ref().map(ToString::to_string),
            scope.task_id.as_ref().map(ToString::to_string),
            scope
                .working_directory
                .as_ref()
                .map(|path| path.display().to_string()),
            scope
                .paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(","),
            conflict.tool_call_id,
            conflict
                .scope
                .workspace_id
                .as_ref()
                .map(ToString::to_string),
            conflict.scope.session_id.as_ref().map(ToString::to_string),
            conflict.scope.task_id.as_ref().map(ToString::to_string),
            conflict
                .scope
                .working_directory
                .as_ref()
                .map(|path| path.display().to_string()),
            conflict
                .scope
                .paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        ToolExecutionOutput {
            tool_call_id: input.tool_call_id.clone(),
            status: magi_core::ExecutionResultStatus::Rejected,
            payload: serde_json::json!({
                "tool": input.tool_name,
                "status": "rejected",
                "error_code": "write_conflict",
                "error": WRITE_CONFLICT_PUBLIC_ERROR,
                "access_mode": access_mode.as_str(),
            })
            .to_string(),
            governance: GovernanceDecision {
                outcome: GovernanceOutcome::Blocked,
                allowed: false,
                requires_approval: false,
                phase: DecisionPhase::SandboxPolicy,
                threshold: input.risk_level,
                reason: Some(reason),
            },
        }
    }
}

fn select_permission_axis_output(
    pending_output: &mut Option<ToolExecutionOutput>,
    output: Option<ToolExecutionOutput>,
) -> Option<ToolExecutionOutput> {
    match output {
        Some(output) if output.status == ExecutionResultStatus::Rejected => Some(output),
        Some(output) => {
            if pending_output.is_none() {
                *pending_output = Some(output);
            }
            None
        }
        None => None,
    }
}

pub fn effective_tool_policy_allowed_paths(
    access_profile: AccessProfile,
    allowed_paths: &[String],
    workspace_root_path: Option<&Path>,
) -> Vec<PathBuf> {
    let normalized = normalize_tool_policy_paths(allowed_paths, workspace_root_path);
    if !normalized.is_empty() || access_profile == AccessProfile::FullAccess {
        return normalized;
    }
    workspace_root_path
        .map(|root| vec![canonicalize_tool_permission_path(root)])
        .unwrap_or_default()
}

pub fn normalize_tool_policy_paths(
    paths: &[String],
    workspace_root_path: Option<&Path>,
) -> Vec<PathBuf> {
    paths
        .iter()
        .filter_map(|path| {
            let trimmed = path.trim();
            if trimmed.is_empty() {
                return None;
            }
            Some(resolve_tool_policy_path(trimmed, workspace_root_path))
        })
        .collect()
}

fn resolve_tool_policy_path(path: &str, workspace_root_path: Option<&Path>) -> PathBuf {
    let path = PathBuf::from(path);
    let resolved = if path.is_absolute() {
        path
    } else if let Some(root) = workspace_root_path {
        root.join(path)
    } else {
        path
    };
    canonicalize_tool_permission_path(resolved.as_path())
}

fn resolve_tool_path(path: &str, workspace_root_path: Option<&Path>) -> PathBuf {
    resolve_tool_policy_path(path, workspace_root_path)
}

fn normalize_permission_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let can_pop = normalized
                    .components()
                    .next_back()
                    .is_some_and(|last| matches!(last, Component::Normal(_)));
                if can_pop {
                    normalized.pop();
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    normalized
}

pub fn canonicalize_tool_permission_path(path: &Path) -> PathBuf {
    let lexical_path = normalize_permission_path(path.to_path_buf());
    if let Ok(canonical_path) = lexical_path.canonicalize() {
        return normalize_permission_path(canonical_path);
    }
    canonicalize_existing_permission_ancestor(&lexical_path).unwrap_or(lexical_path)
}

fn canonicalize_existing_permission_ancestor(path: &Path) -> Option<PathBuf> {
    let mut ancestor = path.to_path_buf();
    let mut missing_components = Vec::new();
    loop {
        if let Ok(canonical_ancestor) = ancestor.canonicalize() {
            let mut resolved = normalize_permission_path(canonical_ancestor);
            for component in missing_components.iter().rev() {
                resolved.push(component);
            }
            return Some(normalize_permission_path(resolved));
        }
        let component = ancestor.file_name()?.to_os_string();
        missing_components.push(component);
        if !ancestor.pop() {
            return None;
        }
    }
}

pub fn tool_path_access_requests(
    canonical_tool_name: &str,
    arguments: &str,
    workspace_root_path: Option<&Path>,
    access_profile: AccessProfile,
) -> Vec<ToolPathAccessRequest> {
    let Some(tool) = crate::BuiltinToolName::from_name(canonical_tool_name) else {
        return Vec::new();
    };
    let write = magi_permissions::PathAccessKind::Write;
    let read = magi_permissions::PathAccessKind::Read;
    let mut paths = Vec::new();

    if tool == crate::BuiltinToolName::ApplyPatch {
        for path in apply_patch_declared_paths_from_input(arguments) {
            paths.push(ToolPathAccessRequest {
                absolute_path: resolve_tool_path(&path.to_string_lossy(), workspace_root_path),
                kind: write,
            });
        }
        return dedup_path_accesses(paths);
    }

    let arguments_value = serde_json::from_str::<Value>(arguments).ok();
    let object = arguments_value.as_ref().and_then(Value::as_object);

    match tool {
        crate::BuiltinToolName::FileRead | crate::BuiltinToolName::ViewImage => {
            push_tool_path_fields(&mut paths, object, &["path"], read, workspace_root_path);
        }
        crate::BuiltinToolName::FileWrite
        | crate::BuiltinToolName::FilePatch
        | crate::BuiltinToolName::FileRemove => {
            push_tool_path_fields(&mut paths, object, &["path"], write, workspace_root_path);
        }
        crate::BuiltinToolName::ImageGenerate => {
            push_tool_path_fields(
                &mut paths,
                object,
                &["output_path"],
                write,
                workspace_root_path,
            );
            if paths.is_empty()
                && let Some(root) = workspace_root_path
            {
                paths.push(ToolPathAccessRequest {
                    absolute_path: canonicalize_tool_permission_path(
                        &root.join("generated-images"),
                    ),
                    kind: write,
                });
            }
        }
        crate::BuiltinToolName::FileMkdir => {
            push_tool_path_fields(&mut paths, object, &["path"], write, workspace_root_path);
        }
        crate::BuiltinToolName::FileCopy => {
            push_tool_path_fields(&mut paths, object, &["source"], read, workspace_root_path);
            push_tool_path_fields(
                &mut paths,
                object,
                &["destination"],
                write,
                workspace_root_path,
            );
        }
        crate::BuiltinToolName::FileMove => {
            push_tool_path_fields(&mut paths, object, &["source"], write, workspace_root_path);
            push_tool_path_fields(
                &mut paths,
                object,
                &["destination"],
                write,
                workspace_root_path,
            );
        }
        crate::BuiltinToolName::DiffPreview => {
            push_tool_path_fields(
                &mut paths,
                object,
                &["before_path"],
                read,
                workspace_root_path,
            );
            push_tool_path_fields(
                &mut paths,
                object,
                &["after_path"],
                read,
                workspace_root_path,
            );
        }
        crate::BuiltinToolName::SearchText => {
            push_tool_path_fields(&mut paths, object, &["root"], read, workspace_root_path);
        }
        crate::BuiltinToolName::CodeSymbols => {
            push_tool_path_fields(&mut paths, object, &["path"], read, workspace_root_path);
        }
        crate::BuiltinToolName::ShellExec => {
            let shell_kind =
                if magi_permissions::PermissionEngine::shell_arguments_request_read_only(arguments)
                {
                    read
                } else {
                    write
                };
            push_tool_path_fields(
                &mut paths,
                object,
                &["cwd"],
                shell_kind,
                workspace_root_path,
            );
            if paths.is_empty()
                && let Some(root) = workspace_root_path
                && shell_exec_uses_working_directory(arguments, object)
            {
                paths.push(ToolPathAccessRequest {
                    absolute_path: canonicalize_tool_permission_path(root),
                    kind: shell_kind,
                });
            }
            paths.extend(shell_exec_command_path_accesses(
                object,
                workspace_root_path,
                shell_kind,
            ));
            if access_profile == AccessProfile::ReadOnly {
                for path in &mut paths {
                    path.kind = read;
                }
            }
        }
        crate::BuiltinToolName::ProcessLaunch => {
            push_tool_path_fields(&mut paths, object, &["cwd"], write, workspace_root_path);
            if paths.is_empty()
                && let Some(root) = workspace_root_path
            {
                paths.push(ToolPathAccessRequest {
                    absolute_path: canonicalize_tool_permission_path(root),
                    kind: write,
                });
            }
        }
        _ => {}
    }

    dedup_path_accesses(paths)
}

fn shell_exec_uses_working_directory(
    arguments: &str,
    object: Option<&serde_json::Map<String, Value>>,
) -> bool {
    let Some(object) = object else {
        return !arguments.trim().is_empty();
    };
    object_has_non_empty_string(object, &["command"])
}

fn shell_exec_command_path_accesses(
    object: Option<&serde_json::Map<String, Value>>,
    workspace_root_path: Option<&Path>,
    shell_kind: magi_permissions::PathAccessKind,
) -> Vec<ToolPathAccessRequest> {
    let Some(command) = object.and_then(|object| field_string(object, &["command"])) else {
        return Vec::new();
    };
    let tokens = shell_command_tokens(&command);
    if tokens.is_empty() {
        return Vec::new();
    }

    let mut paths = shell_redirection_write_paths(&tokens, workspace_root_path);
    for segment in shell_command_segments(&tokens) {
        paths.extend(shell_segment_path_accesses(
            segment,
            workspace_root_path,
            shell_kind,
        ));
    }
    paths
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ShellToken {
    Word(String),
    Operator(String),
}

fn shell_command_tokens(command: &str) -> Vec<ShellToken> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut chars = command.chars().peekable();
    let mut quote = None;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if escaped {
            word.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            } else {
                word.push(ch);
            }
            continue;
        }
        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
            continue;
        }
        if ch.is_whitespace() {
            push_shell_word(&mut tokens, &mut word);
            continue;
        }
        if matches!(ch, '>' | '<' | '|' | '&' | ';') {
            push_shell_word(&mut tokens, &mut word);
            let mut operator = ch.to_string();
            if let Some(next) = chars.peek().copied()
                && ((ch == '>' && matches!(next, '>' | '|'))
                    || (ch == '<' && next == '<')
                    || (ch == '|' && next == '|')
                    || (ch == '&' && matches!(next, '&' | '>')))
            {
                operator.push(next);
                chars.next();
                if operator == "&>" && chars.peek() == Some(&'>') {
                    operator.push('>');
                    chars.next();
                }
            }
            tokens.push(ShellToken::Operator(operator));
            continue;
        }
        word.push(ch);
    }
    if escaped {
        word.push('\\');
    }
    push_shell_word(&mut tokens, &mut word);
    tokens
}

fn push_shell_word(tokens: &mut Vec<ShellToken>, word: &mut String) {
    if !word.is_empty() {
        tokens.push(ShellToken::Word(std::mem::take(word)));
    }
}

fn shell_redirection_write_paths(
    tokens: &[ShellToken],
    workspace_root_path: Option<&Path>,
) -> Vec<ToolPathAccessRequest> {
    let mut paths = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        let ShellToken::Operator(operator) = token else {
            continue;
        };
        if !matches!(operator.as_str(), ">" | ">>" | ">|" | "&>" | "&>>") {
            continue;
        }
        let Some(ShellToken::Word(target)) = tokens.get(index + 1) else {
            continue;
        };
        if target == "/dev/null" || target.starts_with('&') || target == "-" {
            continue;
        }
        push_shell_path(
            &mut paths,
            target,
            magi_permissions::PathAccessKind::Write,
            workspace_root_path,
        );
    }
    paths
}

fn shell_command_segments(tokens: &[ShellToken]) -> Vec<&[ShellToken]> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        if matches!(
            token,
            ShellToken::Operator(operator)
                if matches!(operator.as_str(), "|" | "||" | "&&" | ";")
        ) {
            if start < index {
                segments.push(&tokens[start..index]);
            }
            start = index + 1;
        }
    }
    if start < tokens.len() {
        segments.push(&tokens[start..]);
    }
    segments
}

fn shell_segment_path_accesses(
    segment: &[ShellToken],
    workspace_root_path: Option<&Path>,
    shell_kind: magi_permissions::PathAccessKind,
) -> Vec<ToolPathAccessRequest> {
    let words = segment
        .iter()
        .filter_map(|token| match token {
            ShellToken::Word(word) => Some(word.as_str()),
            ShellToken::Operator(_) => None,
        })
        .collect::<Vec<_>>();
    let Some(command_index) = shell_command_word_index(&words) else {
        return Vec::new();
    };
    let command = shell_command_basename(words[command_index]);
    let arguments = &words[command_index + 1..];
    let mut paths = Vec::new();

    match command {
        "cp" | "mv" | "install" => {
            let operands = shell_non_option_operands(arguments);
            for source in operands.iter().take(operands.len().saturating_sub(1)) {
                push_shell_path(
                    &mut paths,
                    source,
                    magi_permissions::PathAccessKind::Read,
                    workspace_root_path,
                );
            }
            if let Some(destination) = operands.last() {
                push_shell_path(
                    &mut paths,
                    destination,
                    magi_permissions::PathAccessKind::Write,
                    workspace_root_path,
                );
            }
        }
        "tee" => {
            for path in shell_non_option_operands(arguments) {
                push_shell_path(
                    &mut paths,
                    path,
                    magi_permissions::PathAccessKind::Write,
                    workspace_root_path,
                );
            }
        }
        "touch" | "mkdir" | "rmdir" | "rm" | "unlink" | "truncate" => {
            for path in shell_non_option_operands(arguments) {
                push_shell_path(
                    &mut paths,
                    path,
                    magi_permissions::PathAccessKind::Write,
                    workspace_root_path,
                );
            }
        }
        _ => {
            for path in arguments
                .iter()
                .copied()
                .filter(|value| shell_word_looks_like_path(value))
            {
                push_shell_path(&mut paths, path, shell_kind, workspace_root_path);
            }
        }
    }
    paths
}

fn shell_command_word_index(words: &[&str]) -> Option<usize> {
    let mut index = 0usize;
    while index < words.len() {
        let word = words[index];
        if matches!(word, "env" | "command" | "builtin" | "exec" | "sudo") {
            index += 1;
            continue;
        }
        if word.contains('=') && !word.starts_with('/') && !word.starts_with("./") {
            index += 1;
            continue;
        }
        return Some(index);
    }
    None
}

fn shell_command_basename(command: &str) -> &str {
    command.rsplit('/').next().unwrap_or(command)
}

fn shell_non_option_operands<'a>(arguments: &'a [&'a str]) -> Vec<&'a str> {
    arguments
        .iter()
        .copied()
        .filter(|value| !value.starts_with('-') && *value != "--")
        .collect()
}

fn shell_word_looks_like_path(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with("./")
        || value.starts_with("../")
        || value.contains('/')
}

fn push_shell_path(
    paths: &mut Vec<ToolPathAccessRequest>,
    value: &str,
    kind: magi_permissions::PathAccessKind,
    workspace_root_path: Option<&Path>,
) {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed == "-"
        || trimmed.contains(['*', '?', '$', '`'])
        || trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
    {
        return;
    }
    paths.push(ToolPathAccessRequest {
        absolute_path: resolve_tool_path(trimmed, workspace_root_path),
        kind,
    });
}

fn object_has_non_empty_string(object: &serde_json::Map<String, Value>, keys: &[&str]) -> bool {
    keys.iter().any(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    })
}

fn push_tool_path_fields(
    paths: &mut Vec<ToolPathAccessRequest>,
    object: Option<&serde_json::Map<String, Value>>,
    keys: &[&str],
    kind: magi_permissions::PathAccessKind,
    workspace_root_path: Option<&Path>,
) {
    let Some(object) = object else {
        return;
    };
    for key in keys {
        if let Some(path) = object
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
        {
            paths.push(ToolPathAccessRequest {
                absolute_path: resolve_tool_path(path, workspace_root_path),
                kind,
            });
        }
    }
}

fn dedup_path_accesses(paths: Vec<ToolPathAccessRequest>) -> Vec<ToolPathAccessRequest> {
    let mut deduped = Vec::new();
    for item in paths {
        if !deduped.iter().any(|existing| existing == &item) {
            deduped.push(item);
        }
    }
    deduped
}

fn normalize_execution_policy(policy: &ToolExecutionPolicy) -> ToolExecutionPolicy {
    let mut normalized = policy.clone();
    normalized.source_skill_ids.sort();
    normalized.source_skill_ids.dedup();
    normalized.allowed_tool_names.sort();
    normalized.allowed_tool_names.dedup();
    normalized.denied_tool_names.sort();
    normalized.denied_tool_names.dedup();
    normalized.allowed_paths.sort();
    normalized.allowed_paths.dedup();
    normalized.denied_paths.sort();
    normalized.denied_paths.dedup();
    normalized.read_only_paths.sort();
    normalized.read_only_paths.dedup();
    normalized
}

impl WriteProtectionClaim {
    fn conflicts_with(&self, other: &WriteProtectionScope) -> bool {
        if self.scope.workspace_id.is_some()
            && other.workspace_id.is_some()
            && self.scope.workspace_id != other.workspace_id
        {
            return false;
        }
        if self.scope.session_id.is_some()
            && other.session_id.is_some()
            && self.scope.session_id != other.session_id
        {
            return false;
        }
        if self.scope.task_id.is_some()
            && other.task_id.is_some()
            && self.scope.task_id == other.task_id
        {
            return true;
        }
        if self.scope.working_directory.is_some()
            && other.working_directory.is_some()
            && self.scope.working_directory == other.working_directory
        {
            return true;
        }
        self.scope
            .paths
            .iter()
            .any(|left| other.paths.iter().any(|right| left == right))
    }
}

fn normalize_path_for_lock(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => {
                if normalized.as_os_str().is_empty() {
                    normalized.push(std::path::MAIN_SEPARATOR.to_string());
                }
            }
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    if normalized.as_os_str().is_empty() {
        path.to_path_buf()
    } else {
        normalized
    }
}
