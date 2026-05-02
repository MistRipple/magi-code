use crate::{
    BuiltinToolAccessMode, ToolExecutionContext, ToolExecutionInput, ToolExecutionOutput,
    ToolExecutionPolicy, ToolRegistry, WriteProtectionScope,
    builtin::{field_string, parse_json_object, resolve_path},
};
use magi_core::{ToolCallId, UtcMillis};
use magi_governance::{DecisionPhase, GovernanceDecision, GovernanceOutcome};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    sync::{Arc, RwLock},
};

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

    pub(crate) fn build_policy_rejection(
        &self,
        input: &ToolExecutionInput,
        policy: &ToolExecutionPolicy,
        reason: String,
    ) -> ToolExecutionOutput {
        ToolExecutionOutput {
            tool_call_id: input.tool_call_id.clone(),
            status: magi_core::ExecutionResultStatus::Rejected,
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

    pub(crate) fn resolve_access_mode(&self, input: &ToolExecutionInput) -> BuiltinToolAccessMode {
        let Some(tool_name) = crate::BuiltinToolName::from_str(input.tool_name.trim()) else {
            return BuiltinToolAccessMode::ReadOnly;
        };
        if tool_name == crate::BuiltinToolName::ShellExec
            || tool_name == crate::BuiltinToolName::ProcessLaunch
        {
            return self
                .parse_requested_access_mode(&input.input)
                .unwrap_or(BuiltinToolAccessMode::MaybeWrite);
        }
        if tool_name.is_write_operation() {
            return BuiltinToolAccessMode::ExplicitWrite;
        }
        BuiltinToolAccessMode::ReadOnly
    }

    pub(crate) fn parse_requested_access_mode(&self, input: &str) -> Option<BuiltinToolAccessMode> {
        parse_json_object(input).and_then(|object| {
            field_string(&object, &["access_mode", "write_mode", "intent"])
                .and_then(|value| BuiltinToolAccessMode::from_str(&value))
        })
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

        if let Some(object) = &request {
            for key in [
                "path",
                "file_path",
                "target_path",
                "cwd",
                "working_directory",
                "workdir",
                "root",
            ] {
                if let Some(value) = field_string(object, &[key]) {
                    if let Ok(path) = resolve_path(&value) {
                        paths.push(normalize_path_for_lock(&path));
                    }
                }
            }

            for key in ["paths", "file_paths", "target_paths"] {
                if let Some(values) = object.get(key).and_then(Value::as_array) {
                    for value in values {
                        if let Some(path_value) = value.as_str() {
                            if let Ok(path) = resolve_path(path_value) {
                                paths.push(normalize_path_for_lock(&path));
                            }
                        }
                    }
                }
            }
        }

        paths.sort();
        paths.dedup();

        let working_directory = if input.tool_name == crate::BuiltinToolName::ShellExec.as_str() {
            request
                .as_ref()
                .and_then(|object| field_string(object, &["cwd", "working_directory", "workdir"]))
                .map(|value| resolve_path(&value).map(|path| normalize_path_for_lock(&path)))
                .unwrap_or_else(|| {
                    std::env::current_dir()
                        .map(|path| normalize_path_for_lock(&path))
                        .map_err(|error| error.to_string())
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
            "检测到并发写冲突: tool={} workspace={:?} session={:?} task={:?} cwd={:?} paths={}",
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
                .join(",")
        );
        ToolExecutionOutput {
            tool_call_id: input.tool_call_id.clone(),
            status: magi_core::ExecutionResultStatus::Rejected,
            payload: serde_json::json!({
                "tool": input.tool_name,
                "status": "rejected",
                "error": reason.clone(),
                "access_mode": access_mode.as_str(),
                "write_scope": {
                    "workspace_id": scope.workspace_id.as_ref().map(ToString::to_string),
                    "session_id": scope.session_id.as_ref().map(ToString::to_string),
                    "task_id": scope.task_id.as_ref().map(ToString::to_string),
                    "working_directory": scope.working_directory.as_ref().map(|path| path.display().to_string()),
                    "paths": scope.paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
                },
                "conflicting_claim": {
                    "tool_call_id": conflict.tool_call_id.to_string(),
                    "access_mode": conflict.access_mode.as_str(),
                    "workspace_id": conflict.scope.workspace_id.as_ref().map(ToString::to_string),
                    "session_id": conflict.scope.session_id.as_ref().map(ToString::to_string),
                    "task_id": conflict.scope.task_id.as_ref().map(ToString::to_string),
                    "working_directory": conflict.scope.working_directory.as_ref().map(|path| path.display().to_string()),
                    "paths": conflict.scope.paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
                }
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
