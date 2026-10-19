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
    pub task_store: Option<Arc<magi_orchestrator::task_store::TaskStore>>,
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
        Err(message) => return rejected(tool, "git_invalid_input", message),
    };
    if execution_context.caller_scope != magi_tool_runtime::ToolCallerScope::SessionMainline {
        if tool == "agent_apply"
            && execution_context.caller_scope == magi_tool_runtime::ToolCallerScope::DelegatingTask
        {
            let binding = match resolve_delegating_binding(deps, execution_context) {
                Ok(binding) => binding,
                Err(result) => return result,
            };
            return execute_agent_apply(deps, tool, &arguments, binding, execution_context);
        }
        return execute_delegated_git_query(deps, tool, &arguments, execution_context);
    }
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
                Err(message) => return rejected(tool, "git_invalid_input", message),
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
        "agent_apply" => execute_agent_apply(deps, tool, &arguments, binding, execution_context),
        _ => rejected(tool, "git_unknown_tool", "未知的结构化 Git 工具"),
    }
}

/// 把已结束的直接子代理改动应用到调用者工作树。
///
/// 只应用代理快照基线到代理分支 HEAD 的差异；任一文件无法干净应用时整体拒绝，
/// 调用者工作树保持原样。成功后标记已应用并删除代理分支。
fn execute_agent_apply(
    deps: &GitToolRuntimeDependencies,
    tool: &str,
    arguments: &Map<String, Value>,
    binding: GitToolBinding,
    execution_context: &ToolExecutionContext,
) -> (String, ExecutionResultStatus) {
    let task_id = match required_string(arguments, "task_id") {
        Ok(value) => value,
        Err(message) => return rejected(tool, "git_invalid_input", message),
    };
    {
        let (Some(store), Some(caller_id)) =
            (deps.task_store.as_ref(), execution_context.task_id.as_ref())
        else {
            return rejected(
                tool,
                "agent_apply_missing_task_scope",
                "采用代理产出需要真实调用任务与 TaskStore 归属",
            );
        };
        let child = store.get_task(&magi_core::TaskId::new(&task_id));
        let caller = store.get_task(caller_id);
        if child.as_ref().is_none_or(|child| {
            child.status != magi_core::TaskStatus::Completed
                || caller.as_ref().is_none_or(|caller| {
                    caller.status != magi_core::TaskStatus::Running
                        || caller.mission_id != child.mission_id
                        || caller.workspace_scope != child.workspace_scope
                        || caller.write_scope != child.write_scope
                        || if caller.parent_task_id.is_none() {
                            // 主线可采用本会话历史主线直接派发的产出；不能绕过活跃中间代理。
                            child.parent_task_id.as_ref() != Some(&child.root_task_id)
                        } else {
                            child.parent_task_id.as_ref() != Some(caller_id)
                                || caller.root_task_id != child.root_task_id
                        }
                })
        }) {
            return rejected(
                tool,
                "agent_output_not_direct_child",
                "只能采用当前任务直接派发且已完成的子代理产出；主线可采用本会话历史主线的直接产出",
            );
        }
    }
    let Some(worktree) = binding
        .context
        .agent_worktrees
        .iter()
        .filter(|worktree| worktree.task_id == task_id)
        .find(|worktree| worktree.result_head.is_some())
        .cloned()
    else {
        return rejected(
            tool,
            "agent_output_missing",
            "该代理没有可应用的改动：它可能没有修改文件、仍在运行，或不是当前会话派发的可写代理",
        );
    };
    if worktree.active {
        return rejected(
            tool,
            "agent_still_running",
            "代理仍在运行；请先 agent_wait 等它结束",
        );
    }
    let result_head = worktree.result_head.clone().unwrap_or_default();
    if worktree.applied {
        return rejected(
            tool,
            "agent_output_already_applied",
            "该代理的改动已经应用到主线工作树",
        );
    }
    let applied_paths = match block_on(deps.git_service.apply_revision_diff(
        &binding.path,
        &worktree.base_head,
        &result_head,
    )) {
        Ok(paths) => paths,
        Err(GitError::ApplyConflict {
            conflicted_paths,
            stderr,
        }) => {
            return (
                json!({
                    "tool": tool,
                    "status": "rejected",
                    "ok": false,
                    "error_code": "git_apply_conflict",
                    "error": "代理改动与当前任务工作树冲突，未做任何修改",
                    "conflicted_paths": conflicted_paths,
                    "base": worktree.base_head,
                    "head": result_head,
                    "branch": worktree.branch,
                    "stderr": stderr,
                    "instruction": "当前任务工作树保持原样。可用 shell_exec 执行 `git diff <base> <head> -- <path>` 查看代理在冲突文件上的改动，再用文件工具手动合并。",
                })
                .to_string(),
                ExecutionResultStatus::Rejected,
            );
        }
        Err(error) => return git_error(tool, error),
    };
    if let Err(error) = deps.session_code_contexts.mark_agent_worktree_applied(
        &binding.session_id,
        &task_id,
        &result_head,
    ) {
        return failed(tool, "agent_apply_record_failed", error.to_string());
    }
    if let Err(message) = persist_contexts(deps) {
        return failed(tool, "git_context_persist_failed", message);
    }
    // 改动已经进入调用者工作树，代理分支不再需要；删除失败只影响分支整洁度。
    let branch_deleted = worktree.branch.as_ref().is_some_and(|branch| {
        block_on(deps.git_service.branch_delete_after_revision_apply(
            &binding.path,
            magi_git::BranchDeleteOptions {
                branch: branch.clone(),
                remote: None,
                force: true,
                confirm_force: true,
                confirm_remote: false,
                precondition: binding.precondition.clone(),
            },
        ))
        .is_ok()
    });
    if execution_context.caller_scope == magi_tool_runtime::ToolCallerScope::SessionMainline {
        schedule_code_index_refresh(deps, &binding.workspace_id, &binding.path);
    }
    succeeded(
        tool,
        json!({
            "task_id": task_id,
            "applied_paths": applied_paths,
            "branch": worktree.branch,
            "branch_deleted": branch_deleted,
        }),
    )
}

/// 子任务查询自己的执行目录，不刷新或修改主对话的 Git context/CAS/基线。
fn execute_delegated_git_query(
    deps: &GitToolRuntimeDependencies,
    tool: &str,
    arguments: &Map<String, Value>,
    context: &ToolExecutionContext,
) -> (String, ExecutionResultStatus) {
    if !matches!(tool, "git_status" | "git_branch_list" | "git_worktree_list") {
        return rejected(
            tool,
            "agent_git_mutation_forbidden",
            "子代理只能查询自己获授的 Git 资源；主工作区操作需由主代理执行",
        );
    }
    let (Some(session_id), Some(workspace_id), Some(task_id), Some(path)) = (
        context.session_id.as_ref(),
        context.workspace_id.as_ref(),
        context.task_id.as_ref(),
        context.working_directory.as_ref(),
    ) else {
        return rejected(
            tool,
            "git_missing_task_scope",
            "Git 查询缺少任务与执行目录归属",
        );
    };
    let Some(session) = deps.session_code_contexts.get(session_id.as_str()) else {
        return rejected(tool, "git_missing_context", "当前会话尚未建立 Git context");
    };
    if session.workspace_id != workspace_id.as_str() {
        return rejected(
            tool,
            "git_workspace_mismatch",
            "任务与 Git 资源不属于同一工作区",
        );
    }
    let assigned = session
        .agent_worktrees
        .iter()
        .find(|assigned| assigned.task_id == task_id.as_str());
    let permitted = match assigned {
        Some(assigned) => {
            assigned.active
                && same_path(path, &assigned.path)
                && context
                    .worker_id
                    .as_ref()
                    .is_none_or(|worker| worker.as_str() == assigned.worker_id)
        }
        None => same_path(path, &session.execution_root),
    };
    if !permitted {
        return rejected(
            tool,
            "git_task_resource_mismatch",
            "任务执行目录不属于当前获授的 Git 资源",
        );
    }
    let result = match tool {
        "git_status" => block_on(deps.git_service.observe(path))
            .map(|observation| json!({"observation": observation})),
        "git_branch_list" => block_on(
            deps.git_service
                .branch_list(path, bool_arg(arguments, "includeRemote", false)),
        )
        .map(|list| json!({"observation": list.observation, "branches": list.branches})),
        "git_worktree_list" => block_on(deps.git_service.worktree_list(path))
            .map(|worktrees| json!({"worktrees": worktrees})),
        _ => unreachable!(),
    };
    match result {
        Ok(mut result) => {
            result["resource"] = json!({"ownerTaskId": task_id, "workingDirectory": path});
            succeeded(tool, result)
        }
        Err(error) => git_error(tool, error),
    }
}

/// 受委派代理仅能在自己的活动副本中采用直接子代理改动，不更新主线 Git 基线。
fn resolve_delegating_binding(
    deps: &GitToolRuntimeDependencies,
    context: &ToolExecutionContext,
) -> Result<GitToolBinding, (String, ExecutionResultStatus)> {
    let reject = || {
        rejected(
            "agent_apply",
            "agent_apply_scope_mismatch",
            "代理委派权限、任务租约或执行目录已失效",
        )
    };
    let (Some(store), Some(session_id), Some(workspace_id), Some(task_id), Some(path)) = (
        deps.task_store.as_ref(),
        context.session_id.as_ref(),
        context.workspace_id.as_ref(),
        context.task_id.as_ref(),
        context.working_directory.as_ref(),
    ) else {
        return Err(reject());
    };
    let caller = store.get_task(task_id).ok_or_else(reject)?;
    if caller.status != magi_core::TaskStatus::Running || !caller.delegation_allowed() {
        return Err(reject());
    }
    store
        .validate_agent_delegation(&caller)
        .map_err(|_| reject())?;
    let session = deps
        .session_code_contexts
        .get(session_id.as_str())
        .ok_or_else(reject)?;
    let assigned = session
        .agent_worktrees
        .iter()
        .find(|assigned| assigned.task_id == task_id.as_str())
        .ok_or_else(reject)?;
    if session.workspace_id != workspace_id.as_str()
        || !assigned.active
        || !same_path(path, &assigned.path)
        || assigned.mode != magi_git::AgentWorktreeMode::Writable
        || store
            .get_active_lease(task_id)
            .is_none_or(|lease| assigned.lease_id.as_deref() != Some(lease.lease_id.as_str()))
        || context
            .worker_id
            .as_ref()
            .is_some_and(|id| id.as_str() != assigned.worker_id)
    {
        return Err(reject());
    }
    let observation = block_on(deps.git_service.observe(path))
        .map_err(|error| git_error("agent_apply", error))?;
    let precondition = GitPrecondition {
        expected_head: observation.head.clone(),
        expected_branch: observation.branch.clone(),
        expected_worktree_path: Some(path.clone()),
    };
    Ok(GitToolBinding {
        session_id: session_id.to_string(),
        workspace_id: workspace_id.to_string(),
        path: path.clone(),
        context: session,
        observation,
        precondition,
    })
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
                "git_missing_session",
                "Git 工具必须在主对话 session 中调用",
            )
        })?;
    let workspace_id = execution_context
        .workspace_id
        .as_ref()
        .map(ToString::to_string)
        .ok_or_else(|| {
            rejected(
                "git",
                "git_missing_workspace",
                "当前 session 未绑定 workspace",
            )
        })?;
    let existing = deps.session_code_contexts.get(&session_id).ok_or_else(|| {
        rejected(
            "git",
            "git_missing_context",
            "当前 session 尚未建立 Git context",
        )
    })?;
    if existing.workspace_id != workspace_id {
        return Err(rejected(
            "git",
            "git_workspace_mismatch",
            "工具上下文与 session Git context 不属于同一 workspace",
        ));
    }
    let working_directory = execution_context
        .working_directory
        .as_ref()
        .ok_or_else(|| {
            rejected(
                "git",
                "git_missing_working_directory",
                "Git 工具缺少执行目录",
            )
        })?;
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
        return Err(rejected(
            "git",
            "git_stale_context_revision",
            error.to_string(),
        ));
    }
    let observation = block_on(deps.git_service.observe(&existing.execution_root))
        .map_err(|error| git_error("git_status", error))?;
    let runtime_workspace_roots = existing.runtime_workspace_roots.clone();
    let mut context = deps.session_code_contexts.observe(
        &session_id,
        &workspace_id,
        runtime_workspace_roots.clone(),
        &observation,
    );
    if context.has_external_drift() {
        let is_fast_forward = match block_on(
            deps.git_service
                .is_session_context_fast_forward(&existing, &observation),
        ) {
            Ok(is_fast_forward) => is_fast_forward,
            Err(error) => {
                tracing::warn!(
                    session_id,
                    previous_head = ?existing.git.base_head,
                    current_head = ?observation.head,
                    ?error,
                    "结构化 Git 工具判断 session 快进失败，保持显式确认"
                );
                false
            }
        };
        if !is_fast_forward {
            if let Err(message) = persist_contexts(deps) {
                return Err(failed("git", "git_context_persist_failed", message));
            }
            return Err(rejected(
                "git",
                "git_stale_context",
                format!(
                    "Git context 发生高风险变化：期望 branch={:?} HEAD={:?}，实际 branch={:?} HEAD={:?}",
                    context.git.desired_ref,
                    context.git.base_head,
                    context.git.observed_branch,
                    context.git.observed_head
                ),
            ));
        }
        if let Err(error) = block_on(
            deps.snapshot_manager
                .rebase_session(session_id.clone(), observation.worktree_path.clone()),
        ) {
            return Err(failed(
                "git",
                "git_snapshot_baseline_rebuild_failed",
                format!("Git 快进后重建变更基线失败: {error}"),
            ));
        }
        context = deps.session_code_contexts.accept(
            &session_id,
            &workspace_id,
            runtime_workspace_roots,
            &observation,
        );
        if let Err(message) = persist_contexts(deps) {
            return Err(failed("git", "git_context_persist_failed", message));
        }
        publish_context_changed(deps, &session_id, &workspace_id, &context, &observation);
        schedule_code_index_refresh(deps, &workspace_id, &observation.worktree_path);
    } else if let Err(message) = persist_contexts(deps) {
        return Err(failed("git", "git_context_persist_failed", message));
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
                Err(message) => return rejected(tool, "git_invalid_input", message),
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
                Err(message) => return rejected(tool, "git_invalid_input", message),
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
                    "git_confirmation_required",
                    "必须先展示 git_merge_preview，并在用户明确确认后传 confirm=true",
                );
            }
            let target = match required_string(arguments, "target") {
                Ok(value) => value,
                Err(message) => return rejected(tool, "git_invalid_input", message),
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
                Err(message) => return rejected(tool, "git_invalid_input", message),
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
                    return rejected(
                        tool,
                        "git_invalid_input",
                        "mode 只能是 read_only 或 writable",
                    );
                }
                Err(message) => return rejected(tool, "git_invalid_input", message),
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
                return failed(tool, "git_worktree_directory_failed", error.to_string());
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
                Err(message) => return rejected(tool, "git_invalid_input", message),
            };
            let managed_root = deps.managed_worktree_root.join(&binding.workspace_id);
            if !path_is_within(&path, &managed_root) {
                return rejected(
                    tool,
                    "git_unmanaged_worktree",
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
        _ => return rejected(tool, "git_unknown_tool", "未知的 Git mutation"),
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
    let workspace_root = workspace_root.to_path_buf();
    std::thread::spawn(move || {
        knowledge_store.build_workspace_index(&workspace_id, &workspace_root);
        knowledge_store.finish_workspace_index_build(&workspace_id);
    });
}

fn publish_context_changed(
    deps: &GitToolRuntimeDependencies,
    session_id: &str,
    workspace_id: &str,
    context: &SessionCodeContext,
    observation: &GitObservation,
) {
    deps.event_bus.publish(
        EventEnvelope::domain(
            EventId::unique(format!("workspace-git-context-changed-{workspace_id}")),
            "workspace.git.context.changed",
            json!({
                "workspace_id": workspace_id,
                "session_id": session_id,
                "repository_root": observation.repository_root,
                "worktree_path": observation.worktree_path,
                "branch": observation.branch,
                "head": observation.head,
                "context_revision": context.context_revision,
                "refresh_scopes": ["changes", "file_tree", "code_index", "knowledge", "context_cache"]
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

/// 失败码对应的下一步指引。只给确定知道该怎么做的类别；其余由错误文本自己说明。
fn git_instruction(code: &str) -> Option<&'static str> {
    Some(match code {
        "git_network_unreachable" => {
            "网络或远端暂时不可达：不要用相同参数立刻重试；告知用户检查网络或代理，稍后再试。"
        }
        "git_authentication_failed" => {
            "凭据缺失或无效：不要重试；告知用户为该远端配置凭据（SSH key 或令牌）。"
        }
        "git_push_rejected" => {
            "远端有本地没有的新提交：先用 git_pull 合并并解决冲突，再重新 git_push；不要强制推送。"
        }
        "git_remote_unavailable" => {
            "远端仓库不存在或没有访问权限：核对 remote 配置，告知用户确认地址。"
        }
        "git_dirty_workspace" => "工作区有未提交改动：先让用户提交或暂存，不要丢弃他们的改动。",
        "git_stale_context" => {
            "Git 上下文已变化（分支、HEAD 或 worktree 被外部改动）：先调用 git_status 刷新，再重新发起。"
        }
        "git_merge_conflict" | "git_apply_conflict" => {
            "存在冲突：按 conflicted_paths 逐个文件解决冲突，不要重复同一次合并。"
        }
        "git_confirmation_required" => {
            "该操作风险较高：按错误说明显式确认后再调用，或先向用户确认。"
        }
        "git_current_branch" => "不能删除当前分支：先切换到其他分支。",
        "git_branch_in_use" => "该分支正被其他 worktree 使用：先移除对应 worktree，或换一个分支。",
        "git_not_repository" => "当前目录不在 Git 仓库中；不要重试，改用不依赖 Git 的方式。",
        _ => return None,
    })
}

fn git_error(tool: &str, error: GitError) -> (String, ExecutionResultStatus) {
    let code = error.code();
    let details = match &error {
        GitError::DirtyWorkspace { dirty, .. } => json!({ "dirty": dirty }),
        GitError::StaleContext {
            actual_branch,
            actual_head,
            actual_worktree_path,
            ..
        } => json!({
            "actual_branch": actual_branch,
            "actual_head": actual_head,
            "actual_worktree_path": actual_worktree_path,
        }),
        GitError::BranchInUse { worktree_paths, .. } => json!({ "worktree_paths": worktree_paths }),
        GitError::MergeConflict {
            conflicted_paths, ..
        } => json!({ "conflicted_paths": conflicted_paths }),
        GitError::ApplyConflict {
            conflicted_paths,
            stderr,
        } => json!({ "conflicted_paths": conflicted_paths, "stderr": stderr }),
        _ => Value::Null,
    };
    // 命令确实执行并失败是 failed；前置条件不满足、被规则拒绝是 rejected。
    let (status, label) = if error.is_execution_failure() {
        (ExecutionResultStatus::Failed, "failed")
    } else {
        (ExecutionResultStatus::Rejected, "rejected")
    };
    let mut payload = json!({
        "tool": tool,
        "status": label,
        "ok": false,
        "error_code": code,
        "error": error.to_string(),
    });
    if !details.is_null() {
        payload["details"] = details;
    }
    if let Some(instruction) = git_instruction(code) {
        payload["instruction"] = Value::String(instruction.to_string());
    }
    (payload.to_string(), status)
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

    fn apply_task(
        id: &str,
        parent: Option<&str>,
        status: magi_core::TaskStatus,
    ) -> magi_core::Task {
        magi_core::Task {
            task_id: magi_core::TaskId::new(id),
            mission_id: magi_core::MissionId::new("apply-mission"),
            root_task_id: magi_core::TaskId::new("apply-root"),
            parent_task_id: parent.map(magi_core::TaskId::new),
            kind: magi_core::TaskKind::LocalAgent,
            title: id.into(),
            goal: id.into(),
            status,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            completion_contract: Default::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: Default::default(),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        }
    }

    #[test]
    fn agent_apply_brings_finished_agent_changes_into_main_worktree_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repository = temp.path().join("repo");
        init_repository(&repository);
        let git_service = Arc::new(magi_git::GitService::new());
        let contexts = magi_git::SessionCodeContextRegistry::default();
        let observation = block_on(git_service.observe(&repository)).expect("observe");
        contexts.accept(
            "session-agent-apply",
            "workspace-agent-apply",
            vec![repository.clone()],
            &observation,
        );
        // 代理从主线快照出发，在自己的分支上新增文件，由运行时提交。
        let head = git(&repository, &["rev-parse", "HEAD"]);
        let base = block_on(git_service.snapshot_worktree(&repository, &head)).expect("snapshot");
        let agent_path = temp.path().join("agent");
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "magi/agent/apply",
                agent_path.to_str().expect("utf8 path"),
                &base,
            ],
        );
        fs::write(agent_path.join("agent.txt"), "from agent\n").expect("agent write");
        let result_head = block_on(git_service.commit_all(&agent_path, "magi agent: apply"))
            .expect("commit")
            .expect("has changes");
        git(
            &repository,
            &[
                "worktree",
                "remove",
                agent_path.to_str().expect("utf8 path"),
            ],
        );
        contexts
            .register_agent_worktree(
                "session-agent-apply",
                magi_git::AgentWorktreeContext {
                    task_id: "task-agent-apply".to_string(),
                    lease_id: Some("lease-agent-apply".to_string()),
                    worker_id: "worker-agent-apply".to_string(),
                    path: agent_path.clone(),
                    mode: magi_git::AgentWorktreeMode::Writable,
                    base_head: base,
                    branch: Some("magi/agent/apply".to_string()),
                    active: false,
                    result_head: Some(result_head),
                    changed_paths: vec!["agent.txt".to_string()],
                    applied: false,
                },
            )
            .expect("register agent worktree");
        let persistence = Arc::new(RuntimeStatePersistence::new(
            temp.path().join("state"),
            temp.path().join("state/workspaces.json"),
            temp.path().join("state/knowledge.json"),
        ));
        let store = Arc::new(magi_orchestrator::task_store::TaskStore::new());
        store
            .insert_task(apply_task(
                "apply-root",
                None,
                magi_core::TaskStatus::Running,
            ))
            .unwrap();
        store
            .insert_task(apply_task(
                "task-agent-apply",
                Some("apply-root"),
                magi_core::TaskStatus::Completed,
            ))
            .unwrap();
        store
            .insert_task(apply_task(
                "grandchild",
                Some("task-agent-apply"),
                magi_core::TaskStatus::Completed,
            ))
            .unwrap();
        let executor = build_git_tool_executor(GitToolRuntimeDependencies {
            task_store: Some(store),
            git_service: git_service.clone(),
            session_code_contexts: contexts.clone(),
            workspace_git_coordinator: magi_git::WorkspaceGitOperationCoordinator::default(),
            event_bus: Arc::new(InMemoryEventBus::new(32)),
            knowledge_store: Arc::new(magi_knowledge_store::KnowledgeStore::new()),
            snapshot_manager: Arc::new(magi_snapshot::SnapshotManager::new()),
            runtime_persistence: persistence,
            managed_worktree_root: temp.path().join("worktrees"),
        });
        let context = ToolExecutionContext {
            task_id: Some(magi_core::TaskId::new("apply-root")),
            session_id: Some(SessionId::new("session-agent-apply")),
            workspace_id: Some(WorkspaceId::new("workspace-agent-apply")),
            working_directory: Some(repository.clone()),
            access_profile: AccessProfile::Restricted,
            ..ToolExecutionContext::default()
        };

        let (payload, status) = executor("agent_apply", r#"{"task_id":"grandchild"}"#, &context);
        assert_eq!(status, ExecutionResultStatus::Rejected, "{payload}");
        assert!(payload.contains("agent_output_not_direct_child"));
        let mut anonymous = context.clone();
        anonymous.task_id = None;
        let (payload, status) = executor(
            "agent_apply",
            r#"{"task_id":"task-agent-apply"}"#,
            &anonymous,
        );
        assert_eq!(status, ExecutionResultStatus::Rejected, "{payload}");
        assert!(payload.contains("agent_apply_missing_task_scope"));

        let (payload, status) =
            executor("agent_apply", r#"{"task_id":"task-agent-apply"}"#, &context);
        assert_eq!(status, ExecutionResultStatus::Succeeded, "{payload}");
        assert_eq!(
            fs::read_to_string(repository.join("agent.txt")).expect("applied file"),
            "from agent\n"
        );
        assert_eq!(
            git(&repository, &["rev-parse", "HEAD"]),
            head,
            "应用不产生提交"
        );
        assert!(
            contexts
                .get("session-agent-apply")
                .expect("context")
                .agent_worktrees[0]
                .applied
        );
        assert!(
            git(&repository, &["branch", "--list", "magi/agent/apply"]).is_empty(),
            "应用后删除代理分支"
        );

        let (payload, status) =
            executor("agent_apply", r#"{"task_id":"task-agent-apply"}"#, &context);
        assert_eq!(status, ExecutionResultStatus::Rejected);
        assert!(payload.contains("agent_output_already_applied"));
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
            temp.path().join("state"),
            temp.path().join("state/workspaces.json"),
            temp.path().join("state/knowledge.json"),
        ));
        let snapshot_manager = Arc::new(magi_snapshot::SnapshotManager::new());
        let executor = build_git_tool_executor(GitToolRuntimeDependencies {
            task_store: None,
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

        fs::write(repository.join("external.txt"), "external fast-forward\n")
            .expect("external fixture");
        git(&repository, &["add", "external.txt"]);
        git(&repository, &["commit", "-m", "external fast-forward"]);
        let advanced_head = git(&repository, &["rev-parse", "HEAD"]);
        let (advanced_payload, advanced_status) = executor("git_status", "{}", &context);
        assert_eq!(
            advanced_status,
            ExecutionResultStatus::Succeeded,
            "{advanced_payload}"
        );
        let advanced_payload =
            serde_json::from_str::<Value>(&advanced_payload).expect("advanced status json");
        assert_eq!(
            advanced_payload["result"]["sessionContext"]["git"]["baseHead"],
            advanced_head
        );
        assert!(
            snapshot_manager
                .get_session("session-git-tool")
                .expect("fast-forward snapshot session")
                .pending_changes()
                .expect("pending changes")
                .is_empty(),
            "shell 提交后调用结构化 Git 工具必须同步推进变更基线"
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
        contexts
            .register_agent_worktree(
                "session-git-tool",
                magi_git::AgentWorktreeContext {
                    task_id: "query-child".into(),
                    lease_id: Some("query-lease".into()),
                    worker_id: "query-worker".into(),
                    path: PathBuf::from(&worktree_path),
                    mode: magi_git::AgentWorktreeMode::ReadOnly,
                    base_head: git(&repository, &["rev-parse", "HEAD"]),
                    branch: None,
                    active: true,
                    result_head: None,
                    changed_paths: vec![],
                    applied: false,
                },
            )
            .unwrap();
        let child_context = ToolExecutionContext {
            caller_scope: magi_tool_runtime::ToolCallerScope::DelegatedTask,
            task_id: Some(magi_core::TaskId::new("query-child")),
            worker_id: Some(WorkerId::new("query-worker")),
            working_directory: Some(PathBuf::from(&worktree_path)),
            ..context.clone()
        };
        let main_before = contexts.get("session-git-tool").unwrap();
        let (child_status, status) = executor("git_status", "{}", &child_context);
        assert_eq!(status, ExecutionResultStatus::Succeeded, "{child_status}");
        let child_status: Value = serde_json::from_str(&child_status).unwrap();
        assert_eq!(
            child_status["result"]["resource"]["workingDirectory"],
            worktree_path
        );
        for query in ["git_branch_list", "git_worktree_list"] {
            let (payload, status) = executor(query, "{}", &child_context);
            assert_eq!(status, ExecutionResultStatus::Succeeded, "{payload}");
        }
        assert_eq!(contexts.get("session-git-tool").unwrap(), main_before);
        contexts
            .release_agent_worktree("session-git-tool", "query-child")
            .unwrap();
        let (_, released_status) = executor("git_status", "{}", &child_context);
        assert_eq!(
            released_status,
            ExecutionResultStatus::Rejected,
            "已释放资源不能被旧任务继续调用"
        );

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
                    lease_id: None,
                    worker_id: "worker-running-agent".to_string(),
                    path: temp.path().join("running-agent-worktree"),
                    mode: magi_git::AgentWorktreeMode::Writable,
                    base_head: git(&repository, &["rev-parse", "HEAD"]),
                    branch: Some("magi/agent/running".to_string()),
                    active: true,
                    result_head: None,
                    changed_paths: Vec::new(),
                    applied: false,
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
            caller_scope: magi_tool_runtime::ToolCallerScope::DelegatedTask,
            task_id: Some(magi_core::TaskId::new("task-side-agent")),
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

        let before_query = contexts.get("session-git-tool").unwrap();
        let (query, query_status) = executor("git_status", "{}", &side_agent_context);
        assert_eq!(query_status, ExecutionResultStatus::Succeeded, "{query}");
        assert_eq!(
            contexts.get("session-git-tool").unwrap(),
            before_query,
            "子代理查询不能改写主会话上下文"
        );
        let wrong_scope = ToolExecutionContext {
            working_directory: Some(temp.path().to_path_buf()),
            ..side_agent_context.clone()
        };
        let (_, wrong_status) = executor("git_status", "{}", &wrong_scope);
        assert_eq!(wrong_status, ExecutionResultStatus::Rejected);
        let (_, merge_status) = executor("git_merge", r#"{"target":"main"}"#, &context);
        assert_eq!(merge_status, ExecutionResultStatus::Rejected);
        assert!(
            temp.path()
                .join("state/session-git-contexts.json")
                .is_file()
        );
    }
}
