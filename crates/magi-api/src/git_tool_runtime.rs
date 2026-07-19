//! 主模型可见 `git_*` 工具的应用层执行器。
//!
//! HTTP 与模型工具都复用 `magi_git::GitService` 作为唯一 Git 命令实现；本模块只把
//! ToolExecutionContext 绑定到 SessionCodeContext，并负责会话基线、持久化和刷新事件。

use crate::RuntimeStatePersistence;
use magi_core::{EventId, ExecutionResultStatus, SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_git::{
    BranchCreateOptions, BranchDeleteOptions, BranchSwitchOptions, GitError, GitObservation,
    GitPrecondition, MergeOptions, PullOptions, PushOptions, SessionCodeContext,
    WorktreeCreateOptions, WorktreeRemoveOptions,
};
use magi_tool_runtime::{GitToolExecutor, ToolExecutionContext};
use serde_json::{Map, Value, json};
use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
pub struct GitToolRuntimeDependencies {
    pub git_service: Arc<magi_git::GitService>,
    pub session_code_contexts: magi_git::SessionCodeContextRegistry,
    pub workspace_git_coordinator: magi_git::WorkspaceGitOperationCoordinator,
    pub event_bus: Arc<InMemoryEventBus>,
    pub knowledge_store: Arc<magi_knowledge_store::KnowledgeStore>,
    pub snapshot_manager: Arc<magi_snapshot::SnapshotManager>,
    pub runtime_persistence: Arc<RuntimeStatePersistence>,
    pub managed_worktree_root: PathBuf,
}

pub fn build_git_tool_executor(deps: GitToolRuntimeDependencies) -> GitToolExecutor {
    Arc::new(move |tool, input, execution_context| {
        execute_git_tool(&deps, tool, input, execution_context)
    })
}

fn execute_git_tool(
    deps: &GitToolRuntimeDependencies,
    tool: &str,
    input: &str,
    execution_context: &ToolExecutionContext,
) -> (String, ExecutionResultStatus) {
    let arguments = match parse_arguments(input) {
        Ok(arguments) => arguments,
        Err(message) => return rejected(tool, "invalid_input", message),
    };
    let binding = match resolve_binding(deps, execution_context, &arguments) {
        Ok(binding) => binding,
        Err(result) => return result,
    };

    match tool {
        "git_status" => succeeded(
            tool,
            json!({
                "observation": binding.observation,
                "sessionContext": binding.context,
                "contextDrift": false,
            }),
        ),
        "git_branch_list" => {
            let include_remote = bool_arg(&arguments, "includeRemote", false);
            match block_on(deps.git_service.branch_list(&binding.path, include_remote)) {
                Ok(list) => succeeded(
                    tool,
                    json!({
                        "observation": list.observation,
                        "branches": list.branches,
                        "sessionContext": binding.context,
                    }),
                ),
                Err(error) => git_error(tool, error),
            }
        }
        "git_merge_preview" => {
            let target = match required_string(&arguments, "target") {
                Ok(value) => value,
                Err(message) => return rejected(tool, "invalid_input", message),
            };
            match block_on(deps.git_service.merge_preview(
                &binding.path,
                &target,
                &binding.precondition,
            )) {
                Ok(preview) => succeeded(tool, json!({ "preview": preview })),
                Err(error) => git_error(tool, error),
            }
        }
        "git_worktree_list" => match block_on(deps.git_service.worktree_list(&binding.path)) {
            Ok(worktrees) => succeeded(tool, json!({ "worktrees": worktrees })),
            Err(error) => git_error(tool, error),
        },
        "git_branch_create"
        | "git_branch_switch"
        | "git_pull"
        | "git_push"
        | "git_merge"
        | "git_branch_delete"
        | "git_worktree_create"
        | "git_worktree_remove" => {
            execute_mutation(deps, tool, &arguments, binding, execution_context)
        }
        _ => rejected(tool, "unknown_git_tool", "未知的结构化 Git 工具"),
    }
}

struct GitToolBinding {
    session_id: String,
    workspace_id: String,
    path: PathBuf,
    context: SessionCodeContext,
    observation: GitObservation,
    precondition: GitPrecondition,
}

fn resolve_binding(
    deps: &GitToolRuntimeDependencies,
    execution_context: &ToolExecutionContext,
    arguments: &Map<String, Value>,
) -> Result<GitToolBinding, (String, ExecutionResultStatus)> {
    let session_id = execution_context
        .session_id
        .as_ref()
        .map(ToString::to_string)
        .ok_or_else(|| {
            rejected(
                "git",
                "missing_session",
                "Git 工具必须在主对话 session 中调用",
            )
        })?;
    let workspace_id = execution_context
        .workspace_id
        .as_ref()
        .map(ToString::to_string)
        .ok_or_else(|| rejected("git", "missing_workspace", "当前 session 未绑定 workspace"))?;
    let existing = deps.session_code_contexts.get(&session_id).ok_or_else(|| {
        rejected(
            "git",
            "missing_git_context",
            "当前 session 尚未建立 Git context",
        )
    })?;
    if existing.workspace_id != workspace_id {
        return Err(rejected(
            "git",
            "workspace_mismatch",
            "工具上下文与 session Git context 不属于同一 workspace",
        ));
    }
    let working_directory = execution_context
        .working_directory
        .as_ref()
        .ok_or_else(|| rejected("git", "missing_working_directory", "Git 工具缺少执行目录"))?;
    if !same_path(working_directory, &existing.execution_root) {
        return Err(rejected(
            "git",
            "agent_git_mutation_forbidden",
            "子代理或非主 worktree 不能管理主对话 Git context",
        ));
    }
    if let Err(error) = deps.session_code_contexts.validate_revision(
        &session_id,
        arguments
            .get("expectedContextRevision")
            .and_then(Value::as_u64),
    ) {
        return Err(rejected("git", "stale_context_revision", error.to_string()));
    }
    let observation = block_on(deps.git_service.observe(&existing.execution_root))
        .map_err(|error| git_error("git_status", error))?;
    let context = deps.session_code_contexts.observe(
        &session_id,
        &workspace_id,
        existing.runtime_workspace_roots,
        &observation,
    );
    if let Err(message) = persist_contexts(deps) {
        return Err(failed("git", "git_context_persist_failed", message));
    }
    if context.has_external_drift() {
        return Err(rejected(
            "git",
            "stale_git_context",
            format!(
                "Git context 已漂移：期望 branch={:?} HEAD={:?}，实际 branch={:?} HEAD={:?}",
                context.git.desired_ref,
                context.git.base_head,
                context.git.observed_branch,
                context.git.observed_head
            ),
        ));
    }
    let precondition = explicit_precondition(arguments).unwrap_or_else(|| context.precondition());
    Ok(GitToolBinding {
        session_id,
        workspace_id,
        path: context.execution_root.clone(),
        context,
        observation,
        precondition,
    })
}

fn execute_mutation(
    deps: &GitToolRuntimeDependencies,
    tool: &str,
    arguments: &Map<String, Value>,
    binding: GitToolBinding,
    execution_context: &ToolExecutionContext,
) -> (String, ExecutionResultStatus) {
    if execution_context.worker_id.is_some() {
        return rejected(
            tool,
            "agent_git_mutation_forbidden",
            "子代理不能改变主对话的 branch、HEAD 或 worktree",
        );
    }
    if !deps
        .workspace_git_coordinator
        .session_holds_execution(&binding.session_id, &binding.context.git.git_common_dir)
    {
        return rejected(
            tool,
            "git_execution_lease_missing",
            "主对话未持有当前 repository 的 execution lease",
        );
    }
    if binding
        .context
        .agent_worktrees
        .iter()
        .any(|worktree| worktree.active)
    {
        return rejected(
            tool,
            "agent_execution_active",
            "仍有子代理在继承当前 Git 基线执行；必须先等待 worker 结束，才能改变 branch、HEAD 或 worktree",
        );
    }

    let operation = match tool {
        "git_branch_create" => {
            let branch = match required_string(arguments, "branch") {
                Ok(value) => value,
                Err(message) => return rejected(tool, "invalid_input", message),
            };
            MutationResult::Observation(block_on(deps.git_service.branch_create(
                &binding.path,
                BranchCreateOptions {
                    branch,
                    start_point: optional_string(arguments, "startPoint"),
                    switch: bool_arg(arguments, "switch", true),
                    precondition: binding.precondition.clone(),
                },
            )))
        }
        "git_branch_switch" => {
            let branch = match required_string(arguments, "branch") {
                Ok(value) => value,
                Err(message) => return rejected(tool, "invalid_input", message),
            };
            MutationResult::Observation(block_on(deps.git_service.branch_switch(
                &binding.path,
                BranchSwitchOptions {
                    branch,
                    precondition: binding.precondition.clone(),
                },
            )))
        }
        "git_pull" => MutationResult::Observation(block_on(deps.git_service.pull(
            &binding.path,
            PullOptions {
                remote: optional_string(arguments, "remote"),
                branch: optional_string(arguments, "branch"),
                ff_only: bool_arg(arguments, "ffOnly", true),
                precondition: binding.precondition.clone(),
            },
        ))),
        "git_push" => MutationResult::Observation(block_on(deps.git_service.push(
            &binding.path,
            PushOptions {
                remote: optional_string(arguments, "remote"),
                branch: optional_string(arguments, "branch"),
                set_upstream: bool_arg(arguments, "setUpstream", false),
                force_with_lease: bool_arg(arguments, "forceWithLease", false),
                confirm_force: bool_arg(arguments, "confirmForce", false),
                precondition: binding.precondition.clone(),
            },
        ))),
        "git_merge" => {
            if !bool_arg(arguments, "confirm", false) {
                return rejected(
                    tool,
                    "confirmation_required",
                    "必须先展示 git_merge_preview，并在用户明确确认后传 confirm=true",
                );
            }
            let target = match required_string(arguments, "target") {
                Ok(value) => value,
                Err(message) => return rejected(tool, "invalid_input", message),
            };
            MutationResult::Observation(block_on(deps.git_service.merge(
                &binding.path,
                MergeOptions {
                    target,
                    ff_only: bool_arg(arguments, "ffOnly", false),
                    precondition: binding.precondition.clone(),
                },
            )))
        }
        "git_branch_delete" => {
            let branch = match required_string(arguments, "branch") {
                Ok(value) => value,
                Err(message) => return rejected(tool, "invalid_input", message),
            };
            MutationResult::Observation(block_on(deps.git_service.branch_delete(
                &binding.path,
                BranchDeleteOptions {
                    branch,
                    remote: optional_string(arguments, "remote"),
                    force: bool_arg(arguments, "force", false),
                    confirm_force: bool_arg(arguments, "confirmForce", false),
                    confirm_remote: bool_arg(arguments, "confirmRemote", false),
                    precondition: binding.precondition.clone(),
                },
            )))
        }
        "git_worktree_create" => {
            let mode = match required_string(arguments, "mode") {
                Ok(value) if matches!(value.as_str(), "read_only" | "writable") => value,
                Ok(_) => {
                    return rejected(tool, "invalid_input", "mode 只能是 read_only 或 writable");
                }
                Err(message) => return rejected(tool, "invalid_input", message),
            };
            let allocation_key = sanitize_component(
                optional_string(arguments, "allocationKey")
                    .as_deref()
                    .unwrap_or(&binding.session_id),
            );
            let path = deps
                .managed_worktree_root
                .join(&binding.workspace_id)
                .join(format!("{}-{}", allocation_key, UtcMillis::now().0));
            if let Some(parent) = path.parent()
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                return failed(tool, "worktree_directory_failed", error.to_string());
            }
            let detached = mode == "read_only";
            let branch = if detached {
                None
            } else {
                optional_string(arguments, "branch").or_else(|| {
                    Some(format!(
                        "magi/agent/{}-{}",
                        allocation_key,
                        UtcMillis::now().0
                    ))
                })
            };
            let base = optional_string(arguments, "base")
                .or_else(|| binding.context.git.base_head.clone())
                .unwrap_or_else(|| "HEAD".to_string());
            MutationResult::Worktree(block_on(deps.git_service.worktree_create(
                &binding.path,
                WorktreeCreateOptions {
                    path,
                    base,
                    branch: branch.clone(),
                    create_branch: branch.is_some(),
                    detached,
                    precondition: binding.precondition.clone(),
                },
            )))
        }
        "git_worktree_remove" => {
            let path = match required_string(arguments, "path") {
                Ok(value) => PathBuf::from(value),
                Err(message) => return rejected(tool, "invalid_input", message),
            };
            let managed_root = deps.managed_worktree_root.join(&binding.workspace_id);
            if !path_is_within(&path, &managed_root) {
                return rejected(
                    tool,
                    "unmanaged_worktree",
                    "只能移除 Magi 管理目录中的 worktree",
                );
            }
            MutationResult::WorktreeList(block_on(deps.git_service.worktree_remove(
                &binding.path,
                WorktreeRemoveOptions {
                    path,
                    force: bool_arg(arguments, "force", false),
                    confirm_force: bool_arg(arguments, "confirmForce", false),
                    precondition: binding.precondition.clone(),
                },
            )))
        }
        _ => return rejected(tool, "unknown_git_tool", "未知的 Git mutation"),
    };

    finish_mutation(deps, tool, binding, operation)
}

enum MutationResult {
    Observation(Result<GitObservation, GitError>),
    Worktree(Result<magi_git::GitWorktree, GitError>),
    WorktreeList(Result<Vec<magi_git::GitWorktree>, GitError>),
}

fn finish_mutation(
    deps: &GitToolRuntimeDependencies,
    tool: &str,
    binding: GitToolBinding,
    operation: MutationResult,
) -> (String, ExecutionResultStatus) {
    let data = match operation {
        MutationResult::Observation(Ok(observation)) => json!({ "observation": observation }),
        MutationResult::Worktree(Ok(worktree)) => json!({ "worktree": worktree }),
        MutationResult::WorktreeList(Ok(worktrees)) => json!({ "worktrees": worktrees }),
        MutationResult::Observation(Err(error))
        | MutationResult::Worktree(Err(error))
        | MutationResult::WorktreeList(Err(error)) => {
            if matches!(error, GitError::MergeConflict { .. })
                && let Ok(observation) = block_on(deps.git_service.observe(&binding.path))
            {
                let context = deps.session_code_contexts.observe(
                    &binding.session_id,
                    &binding.workspace_id,
                    binding.context.runtime_workspace_roots.clone(),
                    &observation,
                );
                let _ = persist_contexts(deps);
                publish_context_changed(
                    deps,
                    &binding.session_id,
                    &binding.workspace_id,
                    &context,
                    &observation,
                );
            }
            return git_error(tool, error);
        }
    };
    let observation = match block_on(deps.git_service.observe(&binding.path)) {
        Ok(observation) => observation,
        Err(error) => return git_error(tool, error),
    };
    let git_tree_changed = git_tree_changed(&binding.context.precondition(), &observation);
    let context = deps.session_code_contexts.accept(
        &binding.session_id,
        &binding.workspace_id,
        binding.context.runtime_workspace_roots,
        &observation,
    );
    if let Err(message) = persist_contexts(deps) {
        return failed(tool, "git_context_persist_failed", message);
    }
    let snapshot_baseline_status = if git_tree_changed {
        match block_on(
            deps.snapshot_manager
                .rebase_session(binding.session_id.clone(), binding.path.clone()),
        ) {
            Ok(_) => "refreshed",
            Err(error) => {
                tracing::warn!(
                    session_id = %binding.session_id,
                    workspace_id = %binding.workspace_id,
                    ?error,
                    "Git context 变化后重建 snapshot baseline 失败"
                );
                "failed"
            }
        }
    } else {
        "not_required"
    };
    publish_context_changed(
        deps,
        &binding.session_id,
        &binding.workspace_id,
        &context,
        &observation,
    );
    schedule_code_index_refresh(deps, &binding.workspace_id, &binding.path);
    succeeded(
        tool,
        json!({
            "data": data,
            "observation": observation,
            "sessionContext": context,
            "snapshotBaselineStatus": snapshot_baseline_status,
            "refreshScopes": ["file_tree", "code_index", "knowledge", "context_cache"],
        }),
    )
}

fn git_tree_changed(precondition: &GitPrecondition, observation: &GitObservation) -> bool {
    precondition.expected_branch != observation.branch
        || precondition.expected_head != observation.head
        || precondition
            .expected_worktree_path
            .as_deref()
            .is_some_and(|path| !same_path(path, &observation.worktree_path))
}

fn schedule_code_index_refresh(
    deps: &GitToolRuntimeDependencies,
    workspace_id: &str,
    workspace_root: &Path,
) {
    let workspace_id = WorkspaceId::new(workspace_id.to_string());
    if !deps
        .knowledge_store
        .begin_workspace_index_build(&workspace_id)
    {
        return;
    }
    let knowledge_store = deps.knowledge_store.clone();
    let persistence = deps.runtime_persistence.clone();
    let workspace_root = workspace_root.to_path_buf();
    std::thread::spawn(move || {
        knowledge_store.build_workspace_index(&workspace_id, &workspace_root);
        knowledge_store.finish_workspace_index_build(&workspace_id);
        if let Err(error) = persistence.save_knowledge_store(&knowledge_store) {
            tracing::warn!(workspace_id = %workspace_id, ?error, "Git context 变化后持久化代码索引失败");
        }
    });
}

fn publish_context_changed(
    deps: &GitToolRuntimeDependencies,
    session_id: &str,
    workspace_id: &str,
    context: &SessionCodeContext,
    observation: &GitObservation,
) {
    let now = UtcMillis::now();
    deps.event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!(
                "workspace-git-context-changed-{workspace_id}-{}",
                now.0
            )),
            "workspace.git.context.changed",
            json!({
                "workspace_id": workspace_id,
                "session_id": session_id,
                "repository_root": observation.repository_root,
                "worktree_path": observation.worktree_path,
                "branch": observation.branch,
                "head": observation.head,
                "context_revision": context.context_revision,
                "refresh_scopes": ["file_tree", "code_index", "knowledge", "context_cache"]
            }),
        )
        .with_context(EventContext {
            session_id: Some(SessionId::new(session_id.to_string())),
            workspace_id: Some(WorkspaceId::new(workspace_id.to_string())),
            ..EventContext::default()
        }),
    );
}

fn persist_contexts(deps: &GitToolRuntimeDependencies) -> Result<(), String> {
    let Some(root) = deps.runtime_persistence.state_root() else {
        return Ok(());
    };
    deps.runtime_persistence
        .save_json(
            &root.join("session-git-contexts.json"),
            &deps.session_code_contexts.all(),
        )
        .map_err(|error| {
            tracing::warn!(?error, "结构化 Git 工具持久化 session context 失败");
            "session Git context 暂时无法保存".to_string()
        })
}

fn parse_arguments(input: &str) -> Result<Map<String, Value>, String> {
    if input.trim().is_empty() {
        return Ok(Map::new());
    }
    serde_json::from_str::<Value>(input)
        .map_err(|_| "输入必须是 JSON object".to_string())?
        .as_object()
        .cloned()
        .ok_or_else(|| "输入必须是 JSON object".to_string())
}

fn required_string(arguments: &Map<String, Value>, key: &str) -> Result<String, String> {
    optional_string(arguments, key).ok_or_else(|| format!("缺少非空字符串字段 {key}"))
}

fn optional_string(arguments: &Map<String, Value>, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn bool_arg(arguments: &Map<String, Value>, key: &str, default: bool) -> bool {
    arguments
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

fn explicit_precondition(arguments: &Map<String, Value>) -> Option<GitPrecondition> {
    let expected_branch = optional_string(arguments, "expectedBranch");
    let expected_head = optional_string(arguments, "expectedHead");
    let expected_worktree_path =
        optional_string(arguments, "expectedWorktreePath").map(PathBuf::from);
    (expected_branch.is_some() || expected_head.is_some() || expected_worktree_path.is_some())
        .then_some(GitPrecondition {
            expected_branch,
            expected_head,
            expected_worktree_path,
        })
}

fn succeeded(tool: &str, data: Value) -> (String, ExecutionResultStatus) {
    (
        json!({ "tool": tool, "status": "succeeded", "ok": true, "result": data }).to_string(),
        ExecutionResultStatus::Succeeded,
    )
}

fn rejected(
    tool: &str,
    error_code: &str,
    message: impl Into<String>,
) -> (String, ExecutionResultStatus) {
    (
        json!({
            "tool": tool,
            "status": "rejected",
            "ok": false,
            "error_code": error_code,
            "error": message.into(),
        })
        .to_string(),
        ExecutionResultStatus::Rejected,
    )
}

fn failed(
    tool: &str,
    error_code: &str,
    message: impl Into<String>,
) -> (String, ExecutionResultStatus) {
    (
        json!({
            "tool": tool,
            "status": "failed",
            "ok": false,
            "error_code": error_code,
            "error": message.into(),
        })
        .to_string(),
        ExecutionResultStatus::Failed,
    )
}

fn git_error(tool: &str, error: GitError) -> (String, ExecutionResultStatus) {
    let code = match &error {
        GitError::NotRepository { .. } => "not_repository",
        GitError::InvalidInput { .. } => "invalid_input",
        GitError::DirtyWorkspace { .. } => "dirty_workspace",
        GitError::StaleContext { .. } => "stale_git_context",
        GitError::CurrentBranch { .. } => "current_branch",
        GitError::BranchInUse { .. } => "branch_in_use",
        GitError::ConfirmationRequired { .. } => "confirmation_required",
        GitError::MergeConflict { .. } => "merge_conflict",
        GitError::CommandFailed { .. } => "git_command_failed",
        GitError::Io(_) => "git_io_error",
    };
    let details = match &error {
        GitError::DirtyWorkspace { dirty, .. } => json!({ "dirty": dirty }),
        GitError::StaleContext {
            actual_branch,
            actual_head,
            actual_worktree_path,
            ..
        } => json!({
            "actualBranch": actual_branch,
            "actualHead": actual_head,
            "actualWorktreePath": actual_worktree_path,
        }),
        GitError::BranchInUse { worktree_paths, .. } => json!({ "worktreePaths": worktree_paths }),
        GitError::MergeConflict {
            conflicted_paths, ..
        } => json!({ "conflictedPaths": conflicted_paths }),
        _ => Value::Null,
    };
    (
        json!({
            "tool": tool,
            "status": "rejected",
            "ok": false,
            "error_code": code,
            "error": error.to_string(),
            "details": details,
        })
        .to_string(),
        ExecutionResultStatus::Rejected,
    )
}

fn sanitize_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    let sanitized = sanitized.trim_matches('-');
    if sanitized.is_empty() {
        "allocation".to_string()
    } else {
        sanitized.chars().take(80).collect()
    }
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    let candidate = path
        .canonicalize()
        .or_else(|_| {
            path.parent()
                .ok_or_else(|| std::io::Error::other("path has no parent"))?
                .canonicalize()
                .map(|parent| parent.join(path.file_name().unwrap_or_default()))
        })
        .unwrap_or_else(|_| path.to_path_buf());
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    candidate.starts_with(root)
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.canonicalize().unwrap_or_else(|_| left.to_path_buf())
        == right.canonicalize().unwrap_or_else(|_| right.to_path_buf())
}

fn block_on<F: Future>(future: F) -> F::Output {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Git tool runtime should create a Tokio runtime")
            .block_on(future)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{AccessProfile, WorkerId};
    use std::fs;

    fn git(repo: &Path, args: &[&str]) -> String {
        let output = magi_process::std_command("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("git should execute");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn init_repository(root: &Path) {
        fs::create_dir_all(root).expect("repo directory");
        git(root, &["init", "-b", "main"]);
        git(root, &["config", "user.name", "Magi Test"]);
        git(root, &["config", "user.email", "magi@example.test"]);
        fs::write(root.join("README.md"), "base\n").expect("seed file");
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-m", "base"]);
    }

    #[test]
    fn model_git_tools_use_session_context_and_isolated_worktrees() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repository = temp.path().join("repo");
        init_repository(&repository);
        let git_service = Arc::new(magi_git::GitService::new());
        let contexts = magi_git::SessionCodeContextRegistry::default();
        let coordinator = magi_git::WorkspaceGitOperationCoordinator::default();
        let observation = block_on(git_service.observe(&repository)).expect("observe");
        contexts.accept(
            "session-git-tool",
            "workspace-git-tool",
            vec![repository.clone()],
            &observation,
        );
        coordinator
            .begin_execution("session-git-tool", &observation.git_common_dir)
            .expect("execution lease");
        let persistence = Arc::new(RuntimeStatePersistence::new(
            temp.path().join("state/sessions.json"),
            temp.path().join("state/workspaces.json"),
            temp.path().join("state/knowledge.json"),
        ));
        let snapshot_manager = Arc::new(magi_snapshot::SnapshotManager::new());
        let executor = build_git_tool_executor(GitToolRuntimeDependencies {
            git_service: git_service.clone(),
            session_code_contexts: contexts.clone(),
            workspace_git_coordinator: coordinator,
            event_bus: Arc::new(InMemoryEventBus::new(32)),
            knowledge_store: Arc::new(magi_knowledge_store::KnowledgeStore::new()),
            snapshot_manager: snapshot_manager.clone(),
            runtime_persistence: persistence,
            managed_worktree_root: temp.path().join("worktrees"),
        });
        let context = ToolExecutionContext {
            session_id: Some(SessionId::new("session-git-tool")),
            workspace_id: Some(WorkspaceId::new("workspace-git-tool")),
            working_directory: Some(repository.clone()),
            access_profile: AccessProfile::FullAccess,
            ..ToolExecutionContext::default()
        };

        let (status_payload, status) = executor("git_status", "{}", &context);
        assert_eq!(status, ExecutionResultStatus::Succeeded);
        assert_eq!(
            serde_json::from_str::<Value>(&status_payload).expect("status json")["result"]["observation"]
                ["branch"],
            "main"
        );

        let (create_payload, create_status) = executor(
            "git_branch_create",
            r#"{"branch":"feature/model-tool"}"#,
            &context,
        );
        assert_eq!(create_status, ExecutionResultStatus::Succeeded);
        assert_eq!(
            serde_json::from_str::<Value>(&create_payload).expect("create json")["result"]["snapshotBaselineStatus"],
            "refreshed"
        );
        assert!(
            snapshot_manager
                .get_session("session-git-tool")
                .expect("snapshot session")
                .pending_changes()
                .expect("pending changes")
                .is_empty()
        );
        assert_eq!(
            git(&repository, &["branch", "--show-current"]),
            "feature/model-tool"
        );

        let remote = temp.path().join("remote.git");
        fs::create_dir_all(&remote).expect("remote directory");
        git(&remote, &["init", "--bare"]);
        let remote_path = remote.to_string_lossy().to_string();
        git(
            &repository,
            &["remote", "add", "origin", remote_path.as_str()],
        );
        let (push_payload, push_status) = executor(
            "git_push",
            r#"{"remote":"origin","setUpstream":true}"#,
            &context,
        );
        assert_eq!(
            push_status,
            ExecutionResultStatus::Succeeded,
            "{push_payload}"
        );
        let (pull_payload, pull_status) = executor("git_pull", "{}", &context);
        assert_eq!(
            pull_status,
            ExecutionResultStatus::Succeeded,
            "{pull_payload}; status={}",
            git(&repository, &["status", "--short"])
        );

        let (worktree_payload, worktree_status) = executor(
            "git_worktree_create",
            r#"{"mode":"read_only","allocationKey":"probe"}"#,
            &context,
        );
        assert_eq!(worktree_status, ExecutionResultStatus::Succeeded);
        let worktree_path = serde_json::from_str::<Value>(&worktree_payload)
            .expect("worktree json")["result"]["data"]["worktree"]["path"]
            .as_str()
            .expect("worktree path")
            .to_string();
        assert!(Path::new(&worktree_path).is_dir());

        let (_, remove_status) = executor(
            "git_worktree_remove",
            &json!({ "path": worktree_path }).to_string(),
            &context,
        );
        assert_eq!(remove_status, ExecutionResultStatus::Succeeded);

        contexts
            .register_agent_worktree(
                "session-git-tool",
                magi_git::AgentWorktreeContext {
                    task_id: "task-running-agent".to_string(),
                    worker_id: "worker-running-agent".to_string(),
                    path: temp.path().join("running-agent-worktree"),
                    mode: magi_git::AgentWorktreeMode::Writable,
                    base_head: git(&repository, &["rev-parse", "HEAD"]),
                    branch: Some("magi/agent/running".to_string()),
                    active: true,
                },
            )
            .expect("register running agent");
        let (active_payload, active_status) =
            executor("git_branch_switch", r#"{"branch":"main"}"#, &context);
        assert_eq!(active_status, ExecutionResultStatus::Rejected);
        assert_eq!(
            serde_json::from_str::<Value>(&active_payload).expect("active agent rejection")["error_code"],
            "agent_execution_active"
        );
        contexts
            .release_agent_worktree("session-git-tool", "task-running-agent")
            .expect("release running agent");

        let side_agent_context = ToolExecutionContext {
            worker_id: Some(WorkerId::new("worker-side-agent")),
            ..context.clone()
        };
        let (rejected_payload, rejected_status) = executor(
            "git_branch_switch",
            r#"{"branch":"main"}"#,
            &side_agent_context,
        );
        assert_eq!(rejected_status, ExecutionResultStatus::Rejected);
        assert_eq!(
            serde_json::from_str::<Value>(&rejected_payload).expect("rejection json")["error_code"],
            "agent_git_mutation_forbidden"
        );

        let (_, merge_status) = executor("git_merge", r#"{"target":"main"}"#, &context);
        assert_eq!(merge_status, ExecutionResultStatus::Rejected);
        assert!(
            temp.path()
                .join("state/session-git-contexts.json")
                .is_file()
        );
    }
}
