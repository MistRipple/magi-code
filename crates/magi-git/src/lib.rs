//! 主对话与代理共享的结构化 Git 服务。
//!
//! 本 crate 只描述 Git branch / repository / worktree，不承载对话历史分叉或任务执行树。
//! 所有会改变 refs、HEAD、index 或 worktree 的操作都在 repository mutex 内重新观测状态，
//! 并校验调用方携带的预期 branch/HEAD/worktree，防止外部终端和其他会话造成竞态。

use magi_process::tokio_command;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitPrecondition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_worktree_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitDirtySummary {
    pub has_uncommitted: bool,
    pub staged: u64,
    pub unstaged: u64,
    pub untracked: u64,
    pub conflicted: u64,
    pub renamed: u64,
    pub deleted: u64,
    pub additions: u64,
    pub deletions: u64,
    #[serde(default)]
    pub conflicted_paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitObservation {
    pub repository_root: PathBuf,
    pub git_common_dir: PathBuf,
    pub worktree_path: PathBuf,
    pub worktree_git_dir: PathBuf,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub origin_url: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    pub dirty: GitDirtySummary,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitBranch {
    pub name: String,
    pub full_ref: String,
    pub is_remote: bool,
    pub is_current: bool,
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub worktree_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchList {
    pub observation: GitObservation,
    pub branches: Vec<GitBranch>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitMergePreview {
    pub observation: GitObservation,
    pub target: String,
    pub target_head: String,
    pub merge_base: Option<String>,
    pub fast_forward: bool,
    pub already_up_to_date: bool,
    pub incoming_commit_count: u64,
    pub changed_paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktree {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: bool,
    pub prunable: bool,
}

/// 主对话绑定的代码执行上下文。它与 conversation/thread fork、任务执行树完全正交。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionCodeContext {
    pub session_id: String,
    pub workspace_id: String,
    pub execution_root: PathBuf,
    #[serde(default)]
    pub runtime_workspace_roots: Vec<PathBuf>,
    pub context_revision: u64,
    pub git: SessionGitContext,
    #[serde(default)]
    pub agent_worktrees: Vec<AgentWorktreeContext>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionGitContext {
    pub repository_root: PathBuf,
    pub git_common_dir: PathBuf,
    pub worktree_path: PathBuf,
    pub worktree_git_dir: PathBuf,
    pub desired_ref: Option<String>,
    pub base_head: Option<String>,
    pub observed_branch: Option<String>,
    pub observed_head: Option<String>,
    pub upstream: Option<String>,
    pub dirty: GitDirtySummary,
    pub lease_generation: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentWorktreeMode {
    ReadOnly,
    Writable,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorktreeContext {
    pub task_id: String,
    pub worker_id: String,
    pub path: PathBuf,
    pub mode: AgentWorktreeMode,
    pub base_head: String,
    pub branch: Option<String>,
    pub active: bool,
}

impl SessionCodeContext {
    pub fn has_external_drift(&self) -> bool {
        self.git.desired_ref != self.git.observed_branch
            || self.git.base_head != self.git.observed_head
            || !same_path(&self.execution_root, &self.git.worktree_path)
    }

    pub fn precondition(&self) -> GitPrecondition {
        GitPrecondition {
            expected_branch: self.git.desired_ref.clone(),
            expected_head: self.git.base_head.clone(),
            expected_worktree_path: Some(self.git.worktree_path.clone()),
        }
    }
}

/// 进程内权威 session → code context 注册表；持久化由宿主负责。
#[derive(Clone, Default)]
pub struct SessionCodeContextRegistry {
    contexts: Arc<RwLock<HashMap<String, SessionCodeContext>>>,
}

impl SessionCodeContextRegistry {
    pub fn get(&self, session_id: &str) -> Option<SessionCodeContext> {
        self.contexts
            .read()
            .expect("session code context read lock poisoned")
            .get(session_id)
            .cloned()
    }

    pub fn all(&self) -> Vec<SessionCodeContext> {
        let mut contexts = self
            .contexts
            .read()
            .expect("session code context read lock poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        contexts.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        contexts
    }

    pub fn replace_all(&self, contexts: Vec<SessionCodeContext>) {
        let mut target = self
            .contexts
            .write()
            .expect("session code context write lock poisoned");
        target.clear();
        target.extend(
            contexts
                .into_iter()
                .map(|context| (context.session_id.clone(), context)),
        );
    }

    pub fn remove(&self, session_id: &str) -> Option<SessionCodeContext> {
        self.contexts
            .write()
            .expect("session code context write lock poisoned")
            .remove(session_id)
    }

    /// 只刷新观测值，不接受外部 branch/HEAD 变化为新的期望基线。
    pub fn observe(
        &self,
        session_id: &str,
        workspace_id: &str,
        runtime_workspace_roots: Vec<PathBuf>,
        observation: &GitObservation,
    ) -> SessionCodeContext {
        self.upsert(
            session_id,
            workspace_id,
            runtime_workspace_roots,
            observation,
            false,
        )
    }

    /// 接受一次由结构化 Git 操作产生的新 branch/HEAD，推进 session CAS revision。
    pub fn accept(
        &self,
        session_id: &str,
        workspace_id: &str,
        runtime_workspace_roots: Vec<PathBuf>,
        observation: &GitObservation,
    ) -> SessionCodeContext {
        self.upsert(
            session_id,
            workspace_id,
            runtime_workspace_roots,
            observation,
            true,
        )
    }

    pub fn validate_revision(
        &self,
        session_id: &str,
        expected_revision: Option<u64>,
    ) -> Result<(), SessionContextError> {
        let Some(expected_revision) = expected_revision else {
            return Ok(());
        };
        let context = self
            .get(session_id)
            .ok_or_else(|| SessionContextError::Missing {
                session_id: session_id.to_string(),
            })?;
        if context.context_revision == expected_revision {
            Ok(())
        } else {
            Err(SessionContextError::StaleRevision {
                expected: expected_revision,
                actual: context.context_revision,
            })
        }
    }

    pub fn register_agent_worktree(
        &self,
        session_id: &str,
        agent_worktree: AgentWorktreeContext,
    ) -> Result<SessionCodeContext, SessionContextError> {
        let mut contexts = self
            .contexts
            .write()
            .expect("session code context write lock poisoned");
        let context = contexts
            .get_mut(session_id)
            .ok_or_else(|| SessionContextError::Missing {
                session_id: session_id.to_string(),
            })?;
        context
            .agent_worktrees
            .retain(|existing| existing.task_id != agent_worktree.task_id);
        context
            .runtime_workspace_roots
            .push(agent_worktree.path.clone());
        context.runtime_workspace_roots.sort();
        context.runtime_workspace_roots.dedup();
        context.agent_worktrees.push(agent_worktree);
        context
            .agent_worktrees
            .sort_by(|left, right| left.task_id.cmp(&right.task_id));
        context.context_revision = context.context_revision.saturating_add(1);
        Ok(context.clone())
    }

    pub fn release_agent_worktree(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<SessionCodeContext, SessionContextError> {
        let mut contexts = self
            .contexts
            .write()
            .expect("session code context write lock poisoned");
        let context = contexts
            .get_mut(session_id)
            .ok_or_else(|| SessionContextError::Missing {
                session_id: session_id.to_string(),
            })?;
        let agent_worktree = context
            .agent_worktrees
            .iter_mut()
            .find(|worktree| worktree.task_id == task_id)
            .ok_or_else(|| SessionContextError::MissingAgentWorktree {
                task_id: task_id.to_string(),
            })?;
        agent_worktree.active = false;
        context
            .runtime_workspace_roots
            .retain(|root| !same_path(root, &agent_worktree.path));
        context.context_revision = context.context_revision.saturating_add(1);
        Ok(context.clone())
    }

    fn upsert(
        &self,
        session_id: &str,
        workspace_id: &str,
        mut runtime_workspace_roots: Vec<PathBuf>,
        observation: &GitObservation,
        accept_as_baseline: bool,
    ) -> SessionCodeContext {
        runtime_workspace_roots.push(observation.worktree_path.clone());
        runtime_workspace_roots.sort();
        runtime_workspace_roots.dedup();
        let mut contexts = self
            .contexts
            .write()
            .expect("session code context write lock poisoned");
        let context =
            contexts
                .entry(session_id.to_string())
                .or_insert_with(|| SessionCodeContext {
                    session_id: session_id.to_string(),
                    workspace_id: workspace_id.to_string(),
                    execution_root: observation.worktree_path.clone(),
                    runtime_workspace_roots: runtime_workspace_roots.clone(),
                    context_revision: 0,
                    git: SessionGitContext {
                        repository_root: observation.repository_root.clone(),
                        git_common_dir: observation.git_common_dir.clone(),
                        worktree_path: observation.worktree_path.clone(),
                        worktree_git_dir: observation.worktree_git_dir.clone(),
                        desired_ref: observation.branch.clone(),
                        base_head: observation.head.clone(),
                        observed_branch: observation.branch.clone(),
                        observed_head: observation.head.clone(),
                        upstream: observation.upstream.clone(),
                        dirty: observation.dirty.clone(),
                        lease_generation: 0,
                    },
                    agent_worktrees: Vec::new(),
                });
        let old_observed_branch = context.git.observed_branch.clone();
        let old_observed_head = context.git.observed_head.clone();
        let old_worktree = context.git.worktree_path.clone();
        context.workspace_id = workspace_id.to_string();
        context.execution_root = observation.worktree_path.clone();
        context.runtime_workspace_roots = runtime_workspace_roots;
        context.git.repository_root = observation.repository_root.clone();
        context.git.git_common_dir = observation.git_common_dir.clone();
        context.git.worktree_path = observation.worktree_path.clone();
        context.git.worktree_git_dir = observation.worktree_git_dir.clone();
        context.git.observed_branch = observation.branch.clone();
        context.git.observed_head = observation.head.clone();
        context.git.upstream = observation.upstream.clone();
        context.git.dirty = observation.dirty.clone();
        if accept_as_baseline {
            context.git.desired_ref = observation.branch.clone();
            context.git.base_head = observation.head.clone();
            context.git.lease_generation = context.git.lease_generation.saturating_add(1);
        }
        if context.context_revision == 0
            || accept_as_baseline
            || old_observed_branch != observation.branch
            || old_observed_head != observation.head
            || !same_path(&old_worktree, &observation.worktree_path)
        {
            context.context_revision = context.context_revision.saturating_add(1);
        }
        context.clone()
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SessionContextError {
    #[error("session {session_id} 尚未绑定代码上下文")]
    Missing { session_id: String },
    #[error("session Git context revision 已变化: expected={expected}, actual={actual}")]
    StaleRevision { expected: u64, actual: u64 },
    #[error("task {task_id} 尚未绑定 agent worktree")]
    MissingAgentWorktree { task_id: String },
}

#[derive(Clone, Default)]
pub struct WorkspaceGitOperationCoordinator {
    state: Arc<Mutex<WorkspaceGitOperationState>>,
}

#[derive(Default)]
struct WorkspaceGitOperationState {
    active_executions: HashMap<String, PathBuf>,
    active_mutations: HashSet<PathBuf>,
}

pub struct GitMutationLease {
    repository_key: PathBuf,
    coordinator: WorkspaceGitOperationCoordinator,
}

impl Drop for GitMutationLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.coordinator.state.lock() {
            state.active_mutations.remove(&self.repository_key);
        }
    }
}

impl WorkspaceGitOperationCoordinator {
    pub fn session_holds_execution(&self, session_id: &str, git_common_dir: &Path) -> bool {
        let repository_key = canonical_or_original(git_common_dir);
        self.state
            .lock()
            .ok()
            .and_then(|state| state.active_executions.get(session_id).cloned())
            .is_some_and(|active_repository| same_path(&active_repository, &repository_key))
    }

    pub fn begin_execution(
        &self,
        session_id: &str,
        git_common_dir: &Path,
    ) -> Result<(), GitCoordinationError> {
        let repository_key = canonical_or_original(git_common_dir);
        let mut state = self
            .state
            .lock()
            .map_err(|error| GitCoordinationError::Internal(error.to_string()))?;
        if state.active_mutations.contains(&repository_key) {
            return Err(GitCoordinationError::MutationActive { repository_key });
        }
        if let Some(existing) = state.active_executions.get(session_id) {
            if same_path(existing, &repository_key) {
                return Ok(());
            }
            return Err(GitCoordinationError::SessionAlreadyBound {
                session_id: session_id.to_string(),
                repository_key: existing.clone(),
            });
        }
        let active_session_ids = state
            .active_executions
            .iter()
            .filter(|(_, active_repository)| same_path(active_repository, &repository_key))
            .map(|(active_session_id, _)| active_session_id.clone())
            .collect::<Vec<_>>();
        if !active_session_ids.is_empty() {
            return Err(GitCoordinationError::ExecutionActive {
                repository_key,
                active_session_ids,
            });
        }
        state
            .active_executions
            .insert(session_id.to_string(), repository_key);
        Ok(())
    }

    pub fn end_execution(&self, session_id: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.active_executions.remove(session_id);
        }
    }

    pub fn begin_mutation(
        &self,
        git_common_dir: &Path,
    ) -> Result<GitMutationLease, GitCoordinationError> {
        let repository_key = canonical_or_original(git_common_dir);
        let mut state = self
            .state
            .lock()
            .map_err(|error| GitCoordinationError::Internal(error.to_string()))?;
        let active_session_ids = state
            .active_executions
            .iter()
            .filter(|(_, active_repository)| same_path(active_repository, &repository_key))
            .map(|(session_id, _)| session_id.clone())
            .collect::<Vec<_>>();
        if !active_session_ids.is_empty() {
            return Err(GitCoordinationError::ExecutionActive {
                repository_key,
                active_session_ids,
            });
        }
        if !state.active_mutations.insert(repository_key.clone()) {
            return Err(GitCoordinationError::MutationActive { repository_key });
        }
        Ok(GitMutationLease {
            repository_key,
            coordinator: self.clone(),
        })
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GitCoordinationError {
    #[error("repository 正在执行 Git mutation: {}", repository_key.display())]
    MutationActive { repository_key: PathBuf },
    #[error("repository 有运行中的 session/worker，不能执行 Git mutation")]
    ExecutionActive {
        repository_key: PathBuf,
        active_session_ids: Vec<String>,
    },
    #[error("session {session_id} 已绑定到另一个运行中 repository")]
    SessionAlreadyBound {
        session_id: String,
        repository_key: PathBuf,
    },
    #[error("Git coordination state 不可用: {0}")]
    Internal(String),
}

#[derive(Clone, Debug)]
pub struct BranchCreateOptions {
    pub branch: String,
    pub start_point: Option<String>,
    pub switch: bool,
    pub precondition: GitPrecondition,
}

#[derive(Clone, Debug)]
pub struct BranchSwitchOptions {
    pub branch: String,
    pub precondition: GitPrecondition,
}

#[derive(Clone, Debug)]
pub struct MergeOptions {
    pub target: String,
    pub ff_only: bool,
    pub precondition: GitPrecondition,
}

#[derive(Clone, Debug)]
pub struct PullOptions {
    pub remote: Option<String>,
    pub branch: Option<String>,
    pub ff_only: bool,
    pub precondition: GitPrecondition,
}

#[derive(Clone, Debug)]
pub struct PushOptions {
    pub remote: Option<String>,
    pub branch: Option<String>,
    pub set_upstream: bool,
    pub force_with_lease: bool,
    pub confirm_force: bool,
    pub precondition: GitPrecondition,
}

#[derive(Clone, Debug)]
pub struct BranchDeleteOptions {
    pub branch: String,
    pub remote: Option<String>,
    pub force: bool,
    pub confirm_force: bool,
    pub confirm_remote: bool,
    pub precondition: GitPrecondition,
}

#[derive(Clone, Debug)]
pub struct WorktreeCreateOptions {
    pub path: PathBuf,
    pub base: String,
    pub branch: Option<String>,
    pub create_branch: bool,
    pub detached: bool,
    pub precondition: GitPrecondition,
}

#[derive(Clone, Debug)]
pub struct WorktreeRemoveOptions {
    pub path: PathBuf,
    pub force: bool,
    pub confirm_force: bool,
    pub precondition: GitPrecondition,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GitError {
    #[error("路径不在 Git worktree 中: {path}")]
    NotRepository { path: PathBuf },
    #[error("Git 输入无效: {message}")]
    InvalidInput { message: String },
    #[error("工作区存在未提交改动，已拒绝 {operation}")]
    DirtyWorkspace {
        operation: String,
        dirty: GitDirtySummary,
    },
    #[error("Git 上下文已变化，调用方必须刷新后重试")]
    StaleContext {
        expected: Box<GitPrecondition>,
        actual_branch: Option<String>,
        actual_head: Option<String>,
        actual_worktree_path: PathBuf,
    },
    #[error("不能删除当前分支 {branch}")]
    CurrentBranch { branch: String },
    #[error("分支 {branch} 正被其他 worktree 使用")]
    BranchInUse {
        branch: String,
        worktree_paths: Vec<PathBuf>,
    },
    #[error("高风险操作需要显式二次确认: {operation}")]
    ConfirmationRequired { operation: String },
    #[error("合并产生冲突")]
    MergeConflict {
        target: String,
        conflicted_paths: Vec<String>,
        stdout: String,
        stderr: String,
    },
    #[error("Git 操作失败: {operation}: {stderr}")]
    CommandFailed {
        operation: String,
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    #[error("无法执行 Git: {0}")]
    Io(String),
}

#[derive(Clone, Default)]
pub struct GitService {
    repository_mutexes: Arc<Mutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>>,
}

#[derive(Debug)]
struct CommandOutput {
    success: bool,
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl GitService {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn observe(&self, path: &Path) -> Result<GitObservation, GitError> {
        observe_unlocked(path).await
    }

    pub async fn branch_list(
        &self,
        path: &Path,
        include_remote: bool,
    ) -> Result<GitBranchList, GitError> {
        let observation = observe_unlocked(path).await?;
        let worktrees = worktree_list_unlocked(&observation.repository_root).await?;
        let mut args = vec![
            "for-each-ref",
            "--format=%(refname)%00%(refname:short)%00%(objectname)%00%(upstream:short)",
            "refs/heads",
        ];
        if include_remote {
            args.push("refs/remotes");
        }
        let output = run_git(&observation.worktree_path, &args).await?;
        ensure_success("branch_list", output).map(|stdout| {
            let mut branches = stdout
                .lines()
                .filter_map(|line| {
                    let mut fields = line.split('\0');
                    let full_ref = fields.next()?.trim().to_string();
                    let name = fields.next()?.trim().to_string();
                    if name.is_empty() || name.ends_with("/HEAD") {
                        return None;
                    }
                    let head = non_empty(fields.next().unwrap_or_default());
                    let upstream = non_empty(fields.next().unwrap_or_default());
                    let is_remote = full_ref.starts_with("refs/remotes/");
                    let worktree_path = worktrees.iter().find_map(|worktree| {
                        (worktree.branch.as_deref() == Some(name.as_str()))
                            .then(|| worktree.path.clone())
                    });
                    Some(GitBranch {
                        is_current: !is_remote
                            && observation.branch.as_deref() == Some(name.as_str()),
                        name,
                        full_ref,
                        is_remote,
                        head,
                        upstream,
                        worktree_path,
                    })
                })
                .collect::<Vec<_>>();
            branches.sort_by(|left, right| {
                left.is_remote
                    .cmp(&right.is_remote)
                    .then_with(|| left.name.cmp(&right.name))
            });
            GitBranchList {
                observation,
                branches,
            }
        })
    }

    pub async fn branch_create(
        &self,
        path: &Path,
        options: BranchCreateOptions,
    ) -> Result<GitObservation, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, &options.precondition)?;
        require_clean(&observation, "创建分支")?;
        validate_branch_name(&observation.worktree_path, &options.branch).await?;
        let start_point = resolve_revision(
            &observation.worktree_path,
            options.start_point.as_deref().unwrap_or("HEAD"),
        )
        .await?;
        let args = if options.switch {
            vec![
                "switch",
                "-c",
                options.branch.as_str(),
                start_point.as_str(),
            ]
        } else {
            vec!["branch", options.branch.as_str(), start_point.as_str()]
        };
        let output = run_git(&observation.worktree_path, &args).await?;
        ensure_success("branch_create", output)?;
        observe_unlocked(&observation.worktree_path).await
    }

    pub async fn branch_switch(
        &self,
        path: &Path,
        options: BranchSwitchOptions,
    ) -> Result<GitObservation, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, &options.precondition)?;
        require_clean(&observation, "切换分支")?;
        validate_local_branch_exists(&observation.worktree_path, &options.branch).await?;
        let output = run_git(
            &observation.worktree_path,
            &["switch", "--", options.branch.as_str()],
        )
        .await?;
        ensure_success("branch_switch", output)?;
        observe_unlocked(&observation.worktree_path).await
    }

    pub async fn merge_preview(
        &self,
        path: &Path,
        target: &str,
        precondition: &GitPrecondition,
    ) -> Result<GitMergePreview, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, precondition)?;
        require_clean(&observation, "预览合并")?;
        let head = observation
            .head
            .as_deref()
            .ok_or_else(|| GitError::InvalidInput {
                message: "当前仓库没有可合并的 HEAD".to_string(),
            })?;
        let target_head = resolve_revision(&observation.worktree_path, target).await?;
        let merge_base_output = run_git(
            &observation.worktree_path,
            &["merge-base", head, target_head.as_str()],
        )
        .await?;
        let merge_base = merge_base_output
            .success
            .then(|| non_empty(&merge_base_output.stdout))
            .flatten();
        let fast_forward = is_ancestor(&observation.worktree_path, head, &target_head).await?;
        let already_up_to_date =
            is_ancestor(&observation.worktree_path, &target_head, head).await?;
        let incoming_commit_count = count_revisions(
            &observation.worktree_path,
            &format!("{head}..{target_head}"),
        )
        .await?;
        let diff_output = run_git(
            &observation.worktree_path,
            &["diff", "--name-only", "-z", head, target_head.as_str()],
        )
        .await?;
        let changed_paths = ensure_success("merge_preview_diff", diff_output)?
            .split('\0')
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        Ok(GitMergePreview {
            observation,
            target: target.to_string(),
            target_head,
            merge_base,
            fast_forward,
            already_up_to_date,
            incoming_commit_count,
            changed_paths,
        })
    }

    pub async fn merge(
        &self,
        path: &Path,
        options: MergeOptions,
    ) -> Result<GitObservation, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, &options.precondition)?;
        require_clean(&observation, "合并分支")?;
        let target_head = resolve_revision(&observation.worktree_path, &options.target).await?;
        let mut args = vec!["merge", "--no-edit"];
        if options.ff_only {
            args.push("--ff-only");
        }
        args.push(target_head.as_str());
        let output = run_git(&observation.worktree_path, &args).await?;
        if !output.success {
            let after = observe_unlocked(&observation.worktree_path).await?;
            if !after.dirty.conflicted_paths.is_empty() {
                return Err(GitError::MergeConflict {
                    target: options.target,
                    conflicted_paths: after.dirty.conflicted_paths,
                    stdout: output.stdout,
                    stderr: output.stderr,
                });
            }
            return Err(command_failed("merge", output));
        }
        observe_unlocked(&observation.worktree_path).await
    }

    pub async fn pull(
        &self,
        path: &Path,
        options: PullOptions,
    ) -> Result<GitObservation, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, &options.precondition)?;
        require_clean(&observation, "拉取远程更新")?;
        let current_branch =
            observation
                .branch
                .as_deref()
                .ok_or_else(|| GitError::InvalidInput {
                    message: "detached HEAD 不能执行 pull".to_string(),
                })?;
        let configured = configured_upstream(&observation.worktree_path, current_branch).await?;
        let remote = options
            .remote
            .or_else(|| configured.as_ref().map(|(remote, _)| remote.clone()))
            .ok_or_else(|| GitError::InvalidInput {
                message: "当前分支没有 upstream，必须提供 remote".to_string(),
            })?;
        let branch = options
            .branch
            .or_else(|| configured.map(|(_, branch)| branch))
            .unwrap_or_else(|| current_branch.to_string());
        validate_remote_name(&observation.worktree_path, &remote).await?;
        validate_branch_name(&observation.worktree_path, &branch).await?;

        let mut args = vec!["pull"];
        if options.ff_only {
            args.push("--ff-only");
        } else {
            args.extend(["--no-rebase", "--no-edit"]);
        }
        args.extend([remote.as_str(), branch.as_str()]);
        let output = run_git(&observation.worktree_path, &args).await?;
        if !output.success {
            let after = observe_unlocked(&observation.worktree_path).await?;
            if !after.dirty.conflicted_paths.is_empty() {
                return Err(GitError::MergeConflict {
                    target: format!("{remote}/{branch}"),
                    conflicted_paths: after.dirty.conflicted_paths,
                    stdout: output.stdout,
                    stderr: output.stderr,
                });
            }
            return Err(command_failed("pull", output));
        }
        observe_unlocked(&observation.worktree_path).await
    }

    pub async fn push(
        &self,
        path: &Path,
        options: PushOptions,
    ) -> Result<GitObservation, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, &options.precondition)?;
        if options.force_with_lease && !options.confirm_force {
            return Err(GitError::ConfirmationRequired {
                operation: "使用 --force-with-lease 推送分支".to_string(),
            });
        }
        let current_branch =
            observation
                .branch
                .as_deref()
                .ok_or_else(|| GitError::InvalidInput {
                    message: "detached HEAD 不能执行 push".to_string(),
                })?;
        let branch = options.branch.unwrap_or_else(|| current_branch.to_string());
        validate_local_branch_exists(&observation.worktree_path, &branch).await?;
        let configured = configured_upstream(&observation.worktree_path, &branch).await?;
        let remote = options
            .remote
            .or_else(|| configured.map(|(remote, _)| remote))
            .ok_or_else(|| GitError::InvalidInput {
                message: "当前分支没有 upstream，必须提供 remote".to_string(),
            })?;
        validate_remote_name(&observation.worktree_path, &remote).await?;

        let mut args = vec!["push"];
        if options.set_upstream {
            args.push("--set-upstream");
        }
        if options.force_with_lease {
            args.push("--force-with-lease");
        }
        args.extend([remote.as_str(), branch.as_str()]);
        let output = run_git(&observation.worktree_path, &args).await?;
        ensure_success("push", output)?;
        observe_unlocked(&observation.worktree_path).await
    }

    pub async fn branch_delete(
        &self,
        path: &Path,
        options: BranchDeleteOptions,
    ) -> Result<GitObservation, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, &options.precondition)?;
        require_clean(&observation, "删除分支")?;
        validate_branch_name(&observation.worktree_path, &options.branch).await?;
        if let Some(remote) = options.remote.as_deref() {
            if !options.confirm_remote {
                return Err(GitError::ConfirmationRequired {
                    operation: format!("删除远程分支 {remote}/{}", options.branch),
                });
            }
            validate_remote_name(&observation.worktree_path, remote).await?;
            let output = run_git(
                &observation.worktree_path,
                &["push", remote, "--delete", options.branch.as_str()],
            )
            .await?;
            ensure_success("branch_delete_remote", output)?;
            return observe_unlocked(&observation.worktree_path).await;
        }
        if observation.branch.as_deref() == Some(options.branch.as_str()) {
            return Err(GitError::CurrentBranch {
                branch: options.branch,
            });
        }
        if options.force && !options.confirm_force {
            return Err(GitError::ConfirmationRequired {
                operation: format!("强制删除本地分支 {}", options.branch),
            });
        }
        let worktree_paths = worktree_list_unlocked(&observation.repository_root)
            .await?
            .into_iter()
            .filter_map(|worktree| {
                (worktree.branch.as_deref() == Some(options.branch.as_str()))
                    .then_some(worktree.path)
            })
            .collect::<Vec<_>>();
        if !worktree_paths.is_empty() {
            return Err(GitError::BranchInUse {
                branch: options.branch,
                worktree_paths,
            });
        }
        let delete_flag = if options.force { "-D" } else { "-d" };
        let output = run_git(
            &observation.worktree_path,
            &["branch", delete_flag, options.branch.as_str()],
        )
        .await?;
        ensure_success("branch_delete_local", output)?;
        observe_unlocked(&observation.worktree_path).await
    }

    pub async fn worktree_list(&self, path: &Path) -> Result<Vec<GitWorktree>, GitError> {
        let observation = observe_unlocked(path).await?;
        worktree_list_unlocked(&observation.repository_root).await
    }

    pub async fn worktree_create(
        &self,
        path: &Path,
        options: WorktreeCreateOptions,
    ) -> Result<GitWorktree, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, &options.precondition)?;
        if !options.path.is_absolute() {
            return Err(GitError::InvalidInput {
                message: "worktree path 必须是绝对路径".to_string(),
            });
        }
        if options.path.exists() {
            return Err(GitError::InvalidInput {
                message: format!("worktree path 已存在: {}", options.path.display()),
            });
        }
        if options.detached && (options.branch.is_some() || options.create_branch) {
            return Err(GitError::InvalidInput {
                message: "detached worktree 不能同时绑定或创建分支".to_string(),
            });
        }
        if options.create_branch && options.branch.is_none() {
            return Err(GitError::InvalidInput {
                message: "createBranch=true 时必须提供 branch".to_string(),
            });
        }
        if let Some(branch) = options.branch.as_deref() {
            validate_branch_name(&observation.worktree_path, branch).await?;
        }
        let base = resolve_revision(&observation.worktree_path, &options.base).await?;
        let destination = options.path.to_string_lossy().to_string();
        let mut args = vec!["worktree", "add"];
        if options.detached {
            args.push("--detach");
        } else if options.create_branch {
            args.push("-b");
            args.push(options.branch.as_deref().unwrap_or_default());
        }
        args.push(destination.as_str());
        if options.detached || options.create_branch {
            args.push(base.as_str());
        } else if let Some(branch) = options.branch.as_deref() {
            validate_local_branch_exists(&observation.worktree_path, branch).await?;
            args.push(branch);
        } else {
            args.push(base.as_str());
        }
        let output = run_git(&observation.worktree_path, &args).await?;
        ensure_success("worktree_create", output)?;
        worktree_list_unlocked(&observation.repository_root)
            .await?
            .into_iter()
            .find(|worktree| same_path(&worktree.path, &options.path))
            .ok_or_else(|| GitError::Io("Git 已创建 worktree，但无法重新定位它".to_string()))
    }

    pub async fn worktree_remove(
        &self,
        path: &Path,
        options: WorktreeRemoveOptions,
    ) -> Result<Vec<GitWorktree>, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, &options.precondition)?;
        if same_path(&observation.worktree_path, &options.path) {
            return Err(GitError::InvalidInput {
                message: "不能从当前 worktree 内删除自身".to_string(),
            });
        }
        let worktrees = worktree_list_unlocked(&observation.repository_root).await?;
        if !worktrees
            .iter()
            .any(|worktree| same_path(&worktree.path, &options.path))
        {
            return Err(GitError::InvalidInput {
                message: "目标路径不是当前 repository 管理的 worktree".to_string(),
            });
        }
        if options.force && !options.confirm_force {
            return Err(GitError::ConfirmationRequired {
                operation: format!("强制移除 worktree {}", options.path.display()),
            });
        }
        let destination = options.path.to_string_lossy().to_string();
        let mut args = vec!["worktree", "remove"];
        if options.force {
            args.push("--force");
        }
        args.push(destination.as_str());
        let output = run_git(&observation.worktree_path, &args).await?;
        ensure_success("worktree_remove", output)?;
        worktree_list_unlocked(&observation.repository_root).await
    }

    async fn lock_and_observe(
        &self,
        path: &Path,
    ) -> Result<(OwnedMutexGuard<()>, GitObservation), GitError> {
        let initial = observe_unlocked(path).await?;
        let mutex = {
            let mut mutexes = self
                .repository_mutexes
                .lock()
                .map_err(|error| GitError::Io(format!("Git mutex poisoned: {error}")))?;
            mutexes
                .entry(initial.git_common_dir.clone())
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone()
        };
        let guard = mutex.lock_owned().await;
        let actual = observe_unlocked(path).await?;
        Ok((guard, actual))
    }
}

async fn observe_unlocked(path: &Path) -> Result<GitObservation, GitError> {
    let repository_root = required_path(path, &["rev-parse", "--show-toplevel"])
        .await
        .map_err(|error| match error {
            GitError::CommandFailed { .. } => GitError::NotRepository {
                path: path.to_path_buf(),
            },
            other => other,
        })?;
    let worktree_path = canonical_or_original(&repository_root);
    let git_common_dir = resolve_git_path(
        &worktree_path,
        &required_text(&worktree_path, &["rev-parse", "--git-common-dir"]).await?,
    );
    let worktree_git_dir = resolve_git_path(
        &worktree_path,
        &required_text(&worktree_path, &["rev-parse", "--git-dir"]).await?,
    );
    let branch = optional_text(
        &worktree_path,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    )
    .await?;
    let head = optional_text(&worktree_path, &["rev-parse", "--verify", "HEAD"]).await?;
    let upstream = optional_text(
        &worktree_path,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .await?;
    let origin_url = optional_text(&worktree_path, &["remote", "get-url", "origin"]).await?;
    let (ahead, behind) = if upstream.is_some() {
        ahead_behind(&worktree_path).await?
    } else {
        (0, 0)
    };
    let dirty = dirty_summary(&worktree_path, head.is_some()).await?;
    Ok(GitObservation {
        repository_root: worktree_path.clone(),
        git_common_dir: canonical_or_original(&git_common_dir),
        worktree_path,
        worktree_git_dir: canonical_or_original(&worktree_git_dir),
        branch,
        head,
        upstream,
        origin_url,
        ahead,
        behind,
        dirty,
    })
}

async fn dirty_summary(path: &Path, has_head: bool) -> Result<GitDirtySummary, GitError> {
    let status_output = run_git(path, &["status", "--porcelain=v1", "-z"]).await?;
    let status_text = ensure_success("git_status", status_output)?;
    let mut summary = GitDirtySummary::default();
    let entries = status_text.split('\0').collect::<Vec<_>>();
    let mut index = 0usize;
    while index < entries.len() {
        let entry = entries[index];
        index += 1;
        if entry.len() < 3 {
            continue;
        }
        let bytes = entry.as_bytes();
        let x = bytes[0] as char;
        let y = bytes[1] as char;
        if (x, y) == ('?', '?') {
            summary.untracked += 1;
            continue;
        }
        if (x, y) == ('!', '!') {
            continue;
        }
        if x != ' ' {
            summary.staged += 1;
        }
        if y != ' ' {
            summary.unstaged += 1;
        }
        if is_conflicted_status(x, y) {
            summary.conflicted += 1;
            summary.conflicted_paths.push(entry[3..].to_string());
        }
        if x == 'R' || y == 'R' {
            summary.renamed += 1;
            index = index.saturating_add(1);
        }
        if x == 'D' || y == 'D' {
            summary.deleted += 1;
        }
    }
    if has_head {
        let diff_output = run_git(path, &["diff", "HEAD", "--numstat"]).await?;
        let diff_text = ensure_success("git_diff_stat", diff_output)?;
        for line in diff_text.lines() {
            let mut fields = line.split('\t');
            summary.additions += fields
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            summary.deletions += fields
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
        }
    }
    summary.conflicted_paths.sort();
    summary.conflicted_paths.dedup();
    summary.has_uncommitted = summary.staged > 0
        || summary.unstaged > 0
        || summary.untracked > 0
        || summary.conflicted > 0;
    Ok(summary)
}

async fn worktree_list_unlocked(path: &Path) -> Result<Vec<GitWorktree>, GitError> {
    let output = run_git(path, &["worktree", "list", "--porcelain", "-z"]).await?;
    let text = ensure_success("worktree_list", output)?;
    let mut result = Vec::new();
    let mut current: Option<GitWorktree> = None;
    for field in text.split('\0') {
        if field.is_empty() {
            continue;
        }
        if let Some(value) = field.strip_prefix("worktree ") {
            if let Some(worktree) = current.take() {
                result.push(worktree);
            }
            current = Some(GitWorktree {
                path: PathBuf::from(value),
                head: None,
                branch: None,
                bare: false,
                detached: false,
                locked: false,
                prunable: false,
            });
        } else if let Some(worktree) = current.as_mut() {
            if let Some(value) = field.strip_prefix("HEAD ") {
                worktree.head = non_empty(value);
            } else if let Some(value) = field.strip_prefix("branch refs/heads/") {
                worktree.branch = non_empty(value);
            } else if field == "bare" {
                worktree.bare = true;
            } else if field == "detached" {
                worktree.detached = true;
            } else if field.starts_with("locked") {
                worktree.locked = true;
            } else if field.starts_with("prunable") {
                worktree.prunable = true;
            }
        }
    }
    if let Some(worktree) = current {
        result.push(worktree);
    }
    result.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(result)
}

async fn validate_branch_name(path: &Path, branch: &str) -> Result<(), GitError> {
    if branch.trim().is_empty() || branch != branch.trim() {
        return Err(GitError::InvalidInput {
            message: "分支名不能为空或包含首尾空白".to_string(),
        });
    }
    let output = run_git(path, &["check-ref-format", "--branch", branch]).await?;
    if output.success {
        Ok(())
    } else {
        Err(GitError::InvalidInput {
            message: format!("非法分支名: {branch}"),
        })
    }
}

async fn validate_local_branch_exists(path: &Path, branch: &str) -> Result<(), GitError> {
    validate_branch_name(path, branch).await?;
    let full_ref = format!("refs/heads/{branch}");
    let output = run_git(
        path,
        &["show-ref", "--verify", "--quiet", full_ref.as_str()],
    )
    .await?;
    if output.success {
        Ok(())
    } else {
        Err(GitError::InvalidInput {
            message: format!("本地分支不存在: {branch}"),
        })
    }
}

async fn validate_remote_name(path: &Path, remote: &str) -> Result<(), GitError> {
    if remote.trim().is_empty() || remote != remote.trim() || remote.starts_with('-') {
        return Err(GitError::InvalidInput {
            message: "remote 名不能为空、包含首尾空白或以 '-' 开头".to_string(),
        });
    }
    let remotes = ensure_success("remote_list", run_git(path, &["remote"]).await?)?;
    if remotes.lines().any(|candidate| candidate == remote) {
        Ok(())
    } else {
        Err(GitError::InvalidInput {
            message: format!("remote 不存在: {remote}"),
        })
    }
}

async fn configured_upstream(
    path: &Path,
    branch: &str,
) -> Result<Option<(String, String)>, GitError> {
    let remote_key = format!("branch.{branch}.remote");
    let merge_key = format!("branch.{branch}.merge");
    let remote = optional_text(path, &["config", "--get", remote_key.as_str()]).await?;
    let merge_ref = optional_text(path, &["config", "--get", merge_key.as_str()]).await?;
    match (remote, merge_ref) {
        (Some(remote), Some(merge_ref)) => {
            let branch =
                merge_ref
                    .strip_prefix("refs/heads/")
                    .ok_or_else(|| GitError::InvalidInput {
                        message: format!("upstream merge ref 不是分支: {merge_ref}"),
                    })?;
            Ok(Some((remote, branch.to_string())))
        }
        (None, None) => Ok(None),
        _ => Err(GitError::InvalidInput {
            message: format!("分支 {branch} 的 upstream 配置不完整"),
        }),
    }
}

async fn resolve_revision(path: &Path, revision: &str) -> Result<String, GitError> {
    if revision.trim().is_empty() || revision != revision.trim() || revision.starts_with('-') {
        return Err(GitError::InvalidInput {
            message: "revision 不能为空、包含首尾空白或以 '-' 开头".to_string(),
        });
    }
    rev_parse(path, revision)
        .await
        .map_err(|_| GitError::InvalidInput {
            message: format!("revision 不存在或不是 commit: {revision}"),
        })
}

fn enforce_precondition(
    actual: &GitObservation,
    expected: &GitPrecondition,
) -> Result<(), GitError> {
    let branch_matches = expected
        .expected_branch
        .as_deref()
        .is_none_or(|branch| actual.branch.as_deref() == Some(branch));
    let head_matches = expected
        .expected_head
        .as_deref()
        .is_none_or(|head| actual.head.as_deref() == Some(head));
    let worktree_matches = expected
        .expected_worktree_path
        .as_deref()
        .is_none_or(|path| same_path(path, &actual.worktree_path));
    if branch_matches && head_matches && worktree_matches {
        return Ok(());
    }
    Err(GitError::StaleContext {
        expected: Box::new(expected.clone()),
        actual_branch: actual.branch.clone(),
        actual_head: actual.head.clone(),
        actual_worktree_path: actual.worktree_path.clone(),
    })
}

fn require_clean(observation: &GitObservation, operation: &str) -> Result<(), GitError> {
    if observation.dirty.has_uncommitted {
        return Err(GitError::DirtyWorkspace {
            operation: operation.to_string(),
            dirty: observation.dirty.clone(),
        });
    }
    Ok(())
}

async fn ahead_behind(path: &Path) -> Result<(u64, u64), GitError> {
    let output = run_git(
        path,
        &["rev-list", "--left-right", "--count", "@{u}...HEAD"],
    )
    .await?;
    if !output.success {
        return Ok((0, 0));
    }
    let mut fields = output.stdout.split_whitespace();
    let behind = fields
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let ahead = fields
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    Ok((ahead, behind))
}

async fn is_ancestor(path: &Path, ancestor: &str, descendant: &str) -> Result<bool, GitError> {
    let output = run_git(path, &["merge-base", "--is-ancestor", ancestor, descendant]).await?;
    match output.code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(command_failed("merge_base_is_ancestor", output)),
    }
}

async fn count_revisions(path: &Path, range: &str) -> Result<u64, GitError> {
    let output = run_git(path, &["rev-list", "--count", range]).await?;
    let stdout = ensure_success("rev_list_count", output)?;
    stdout
        .parse::<u64>()
        .map_err(|error| GitError::Io(format!("无法解析 Git revision count {stdout:?}: {error}")))
}

async fn rev_parse(path: &Path, revision: &str) -> Result<String, GitError> {
    required_text(
        path,
        &["rev-parse", "--verify", &format!("{revision}^{{commit}}")],
    )
    .await
}

async fn required_path(path: &Path, args: &[&str]) -> Result<PathBuf, GitError> {
    required_text(path, args).await.map(PathBuf::from)
}

async fn required_text(path: &Path, args: &[&str]) -> Result<String, GitError> {
    let output = run_git(path, args).await?;
    ensure_success(&args.join(" "), output)
}

async fn optional_text(path: &Path, args: &[&str]) -> Result<Option<String>, GitError> {
    let output = run_git(path, args).await?;
    if output.success {
        Ok(non_empty(&output.stdout))
    } else {
        Ok(None)
    }
}

async fn run_git(path: &Path, args: &[&str]) -> Result<CommandOutput, GitError> {
    let output = tokio_command("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .await
        .map_err(|error| GitError::Io(error.to_string()))?;
    Ok(CommandOutput {
        success: output.status.success(),
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout)
            .trim_end_matches(['\r', '\n'])
            .to_string(),
        stderr: String::from_utf8_lossy(&output.stderr)
            .trim_end_matches(['\r', '\n'])
            .to_string(),
    })
}

fn ensure_success(operation: &str, output: CommandOutput) -> Result<String, GitError> {
    if output.success {
        Ok(output.stdout)
    } else {
        Err(command_failed(operation, output))
    }
}

fn command_failed(operation: &str, output: CommandOutput) -> GitError {
    GitError::CommandFailed {
        operation: operation.to_string(),
        exit_code: output.code,
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn resolve_git_path(worktree_path: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        worktree_path.join(path)
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    canonical_or_original(left) == canonical_or_original(right)
}

fn is_conflicted_status(index_status: char, worktree_status: char) -> bool {
    matches!(
        (index_status, worktree_status),
        ('D', 'D') | ('A', 'U') | ('U', 'D') | ('U', 'A') | ('D', 'U') | ('A', 'A') | ('U', 'U')
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeSet, fs};
    use tempfile::TempDir;

    fn git(path: &Path, args: &[&str]) -> String {
        let output = magi_process::std_command("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .expect("git command should start");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn repository() -> TempDir {
        let temp = tempfile::tempdir().expect("temp repo");
        git(temp.path(), &["init", "-b", "main"]);
        git(temp.path(), &["config", "user.name", "Magi Test"]);
        git(temp.path(), &["config", "user.email", "magi@example.test"]);
        fs::write(temp.path().join("README.md"), "initial\n").expect("write fixture");
        git(temp.path(), &["add", "README.md"]);
        git(temp.path(), &["commit", "-m", "initial"]);
        temp
    }

    fn precondition(observation: &GitObservation) -> GitPrecondition {
        GitPrecondition {
            expected_branch: observation.branch.clone(),
            expected_head: observation.head.clone(),
            expected_worktree_path: Some(observation.worktree_path.clone()),
        }
    }

    #[tokio::test]
    async fn create_switch_and_delete_branch_with_cas() {
        let repo = repository();
        let service = GitService::new();
        let initial = service.observe(repo.path()).await.expect("observe");
        let created = service
            .branch_create(
                repo.path(),
                BranchCreateOptions {
                    branch: "feature/test".to_string(),
                    start_point: None,
                    switch: true,
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect("create branch");
        assert_eq!(created.branch.as_deref(), Some("feature/test"));

        let switched = service
            .branch_switch(
                repo.path(),
                BranchSwitchOptions {
                    branch: "main".to_string(),
                    precondition: precondition(&created),
                },
            )
            .await
            .expect("switch branch");
        service
            .branch_delete(
                repo.path(),
                BranchDeleteOptions {
                    branch: "feature/test".to_string(),
                    remote: None,
                    force: false,
                    confirm_force: false,
                    confirm_remote: false,
                    precondition: precondition(&switched),
                },
            )
            .await
            .expect("delete branch");
        let list = service
            .branch_list(repo.path(), false)
            .await
            .expect("list branches");
        assert_eq!(list.branches.len(), 1);
        assert_eq!(list.branches[0].name, "main");
    }

    #[tokio::test]
    async fn rejects_dirty_switch_and_stale_head() {
        let repo = repository();
        let service = GitService::new();
        let initial = service.observe(repo.path()).await.expect("observe");
        service
            .branch_create(
                repo.path(),
                BranchCreateOptions {
                    branch: "other".to_string(),
                    start_point: None,
                    switch: false,
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect("create branch");
        fs::write(repo.path().join("README.md"), "dirty\n").expect("dirty fixture");
        let dirty = service
            .branch_switch(
                repo.path(),
                BranchSwitchOptions {
                    branch: "other".to_string(),
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect_err("dirty switch must fail");
        assert!(matches!(dirty, GitError::DirtyWorkspace { .. }));

        git(repo.path(), &["restore", "README.md"]);
        fs::write(repo.path().join("next.txt"), "next\n").expect("next fixture");
        git(repo.path(), &["add", "next.txt"]);
        git(repo.path(), &["commit", "-m", "external commit"]);
        let stale = service
            .branch_switch(
                repo.path(),
                BranchSwitchOptions {
                    branch: "other".to_string(),
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect_err("stale head must fail");
        assert!(matches!(stale, GitError::StaleContext { .. }));
    }

    #[tokio::test]
    async fn concurrent_branch_mutations_allow_exactly_one_cas_winner() {
        let repo = repository();
        let service = GitService::new();
        let initial = service.observe(repo.path()).await.expect("observe");
        let first_service = service.clone();
        let second_service = service.clone();
        let first_precondition = precondition(&initial);
        let second_precondition = first_precondition.clone();
        let (first, second) = tokio::join!(
            first_service.branch_create(
                repo.path(),
                BranchCreateOptions {
                    branch: "concurrent/first".to_string(),
                    start_point: None,
                    switch: true,
                    precondition: first_precondition,
                }
            ),
            second_service.branch_create(
                repo.path(),
                BranchCreateOptions {
                    branch: "concurrent/second".to_string(),
                    start_point: None,
                    switch: true,
                    precondition: second_precondition,
                }
            )
        );
        let results = [first, second];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(GitError::StaleContext { .. })))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn creates_detached_and_writable_worktrees() {
        let repo = repository();
        let parent = repo.path().parent().expect("parent");
        let detached_path = parent.join(format!(
            "{}-detached",
            repo.path().file_name().unwrap().to_string_lossy()
        ));
        let writable_path = parent.join(format!(
            "{}-writable",
            repo.path().file_name().unwrap().to_string_lossy()
        ));
        let service = GitService::new();
        let initial = service.observe(repo.path()).await.expect("observe");
        let detached = service
            .worktree_create(
                repo.path(),
                WorktreeCreateOptions {
                    path: detached_path.clone(),
                    base: initial.head.clone().expect("head"),
                    branch: None,
                    create_branch: false,
                    detached: true,
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect("detached worktree");
        assert!(detached.detached);
        let writable = service
            .worktree_create(
                repo.path(),
                WorktreeCreateOptions {
                    path: writable_path.clone(),
                    base: initial.head.clone().expect("head"),
                    branch: Some("magi/worker-test".to_string()),
                    create_branch: true,
                    detached: false,
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect("writable worktree");
        assert_eq!(writable.branch.as_deref(), Some("magi/worker-test"));
        service
            .worktree_remove(
                repo.path(),
                WorktreeRemoveOptions {
                    path: detached_path,
                    force: false,
                    confirm_force: false,
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect("remove detached");
        service
            .worktree_remove(
                repo.path(),
                WorktreeRemoveOptions {
                    path: writable_path,
                    force: false,
                    confirm_force: false,
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect("remove writable");
    }

    #[tokio::test]
    async fn merge_preview_and_merge_report_revision_changes() {
        let repo = repository();
        let service = GitService::new();
        let initial = service.observe(repo.path()).await.expect("observe");
        let feature = service
            .branch_create(
                repo.path(),
                BranchCreateOptions {
                    branch: "feature".to_string(),
                    start_point: None,
                    switch: true,
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect("feature");
        fs::write(repo.path().join("feature.txt"), "feature\n").expect("feature fixture");
        git(repo.path(), &["add", "feature.txt"]);
        git(repo.path(), &["commit", "-m", "feature"]);
        let feature_head = service.observe(repo.path()).await.expect("feature head");
        let main = service
            .branch_switch(
                repo.path(),
                BranchSwitchOptions {
                    branch: "main".to_string(),
                    precondition: precondition(&feature_head),
                },
            )
            .await
            .expect("main");
        let preview = service
            .merge_preview(repo.path(), "feature", &precondition(&main))
            .await
            .expect("preview");
        assert!(preview.fast_forward);
        assert_eq!(preview.incoming_commit_count, 1);
        assert_eq!(preview.changed_paths, vec!["feature.txt"]);
        let merged = service
            .merge(
                repo.path(),
                MergeOptions {
                    target: "feature".to_string(),
                    ff_only: false,
                    precondition: precondition(&main),
                },
            )
            .await
            .expect("merge");
        assert_eq!(merged.head, feature_head.head);
        assert_ne!(feature.head, merged.head);
    }

    #[tokio::test]
    async fn merge_conflict_returns_paths_and_keeps_git_conflict_state() {
        let repo = repository();
        let service = GitService::new();
        let initial = service.observe(repo.path()).await.expect("observe");
        let feature = service
            .branch_create(
                repo.path(),
                BranchCreateOptions {
                    branch: "conflicting".to_string(),
                    start_point: None,
                    switch: true,
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect("feature");
        fs::write(repo.path().join("README.md"), "feature\n").expect("feature content");
        git(repo.path(), &["add", "README.md"]);
        git(repo.path(), &["commit", "-m", "feature conflict"]);
        let feature_head = service.observe(repo.path()).await.expect("feature head");
        let _main = service
            .branch_switch(
                repo.path(),
                BranchSwitchOptions {
                    branch: "main".to_string(),
                    precondition: precondition(&feature_head),
                },
            )
            .await
            .expect("main");
        fs::write(repo.path().join("README.md"), "main\n").expect("main content");
        git(repo.path(), &["add", "README.md"]);
        git(repo.path(), &["commit", "-m", "main conflict"]);
        let main_head = service.observe(repo.path()).await.expect("main head");
        let conflict = service
            .merge(
                repo.path(),
                MergeOptions {
                    target: "conflicting".to_string(),
                    ff_only: false,
                    precondition: precondition(&main_head),
                },
            )
            .await
            .expect_err("merge should conflict");
        match conflict {
            GitError::MergeConflict {
                conflicted_paths, ..
            } => assert_eq!(conflicted_paths, vec!["README.md"]),
            other => panic!("unexpected error: {other}"),
        }
        let conflicted = service
            .observe(repo.path())
            .await
            .expect("conflicted status");
        assert_eq!(conflicted.dirty.conflicted_paths, vec!["README.md"]);
        assert_eq!(feature.branch.as_deref(), Some("conflicting"));
    }

    #[tokio::test]
    async fn branch_delete_blocks_current_remote_and_checked_out_branch() {
        let repo = repository();
        let service = GitService::new();
        let initial = service.observe(repo.path()).await.expect("observe");
        let current = service
            .branch_delete(
                repo.path(),
                BranchDeleteOptions {
                    branch: "main".to_string(),
                    remote: None,
                    force: false,
                    confirm_force: false,
                    confirm_remote: false,
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect_err("current branch deletion must fail");
        assert!(matches!(current, GitError::CurrentBranch { .. }));

        let remote = service
            .branch_delete(
                repo.path(),
                BranchDeleteOptions {
                    branch: "main".to_string(),
                    remote: Some("origin".to_string()),
                    force: false,
                    confirm_force: false,
                    confirm_remote: false,
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect_err("remote delete needs confirmation");
        assert!(matches!(remote, GitError::ConfirmationRequired { .. }));

        let worktree_path = repo.path().parent().unwrap().join(format!(
            "{}-checked-out",
            repo.path().file_name().unwrap().to_string_lossy()
        ));
        service
            .worktree_create(
                repo.path(),
                WorktreeCreateOptions {
                    path: worktree_path.clone(),
                    base: initial.head.clone().expect("head"),
                    branch: Some("checked-out".to_string()),
                    create_branch: true,
                    detached: false,
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect("worktree");
        let in_use = service
            .branch_delete(
                repo.path(),
                BranchDeleteOptions {
                    branch: "checked-out".to_string(),
                    remote: None,
                    force: true,
                    confirm_force: true,
                    confirm_remote: false,
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect_err("checked out branch deletion must fail");
        assert!(matches!(in_use, GitError::BranchInUse { .. }));
        service
            .worktree_remove(
                repo.path(),
                WorktreeRemoveOptions {
                    path: worktree_path,
                    force: false,
                    confirm_force: false,
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect("cleanup worktree");
    }

    #[tokio::test]
    async fn pull_and_push_use_configured_upstream() {
        let repo = repository();
        let remote = tempfile::tempdir().expect("bare remote");
        git(remote.path(), &["init", "--bare"]);
        let remote_path = remote.path().to_string_lossy().to_string();
        git(
            repo.path(),
            &["remote", "add", "origin", remote_path.as_str()],
        );
        git(repo.path(), &["push", "--set-upstream", "origin", "main"]);
        git(remote.path(), &["symbolic-ref", "HEAD", "refs/heads/main"]);

        let peer_parent = tempfile::tempdir().expect("peer parent");
        let peer = peer_parent.path().join("peer");
        let peer_path = peer.to_string_lossy().to_string();
        git(
            peer_parent.path(),
            &[
                "clone",
                "--branch",
                "main",
                remote_path.as_str(),
                &peer_path,
            ],
        );
        git(&peer, &["config", "user.name", "Magi Peer"]);
        git(&peer, &["config", "user.email", "peer@example.test"]);
        fs::write(peer.join("remote.txt"), "from remote\n").expect("remote fixture");
        git(&peer, &["add", "remote.txt"]);
        git(&peer, &["commit", "-m", "remote update"]);
        git(&peer, &["push", "origin", "main"]);

        let service = GitService::new();
        let initial = service.observe(repo.path()).await.expect("observe");
        let pulled = service
            .pull(
                repo.path(),
                PullOptions {
                    remote: None,
                    branch: None,
                    ff_only: true,
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect("pull configured upstream");
        assert_eq!(
            fs::read_to_string(repo.path().join("remote.txt")).unwrap(),
            "from remote\n"
        );

        fs::write(repo.path().join("local.txt"), "from local\n").expect("local fixture");
        git(repo.path(), &["add", "local.txt"]);
        git(repo.path(), &["commit", "-m", "local update"]);
        let local = service.observe(repo.path()).await.expect("local head");
        let confirmation = service
            .push(
                repo.path(),
                PushOptions {
                    remote: None,
                    branch: None,
                    set_upstream: false,
                    force_with_lease: true,
                    confirm_force: false,
                    precondition: precondition(&local),
                },
            )
            .await
            .expect_err("force-with-lease needs confirmation");
        assert!(matches!(
            confirmation,
            GitError::ConfirmationRequired { .. }
        ));

        let pushed = service
            .push(
                repo.path(),
                PushOptions {
                    remote: None,
                    branch: None,
                    set_upstream: false,
                    force_with_lease: false,
                    confirm_force: false,
                    precondition: precondition(&local),
                },
            )
            .await
            .expect("push configured upstream");
        assert_eq!(
            git(remote.path(), &["rev-parse", "refs/heads/main"]),
            pushed.head.expect("pushed head")
        );
        assert_eq!(pulled.branch.as_deref(), Some("main"));
    }

    #[tokio::test]
    async fn confirmed_remote_delete_uses_exact_configured_remote() {
        let repo = repository();
        let remote = tempfile::tempdir().expect("bare remote");
        git(remote.path(), &["init", "--bare"]);
        let remote_path = remote.path().to_string_lossy().to_string();
        git(
            repo.path(),
            &["remote", "add", "fixture", remote_path.as_str()],
        );
        git(
            repo.path(),
            &["push", "fixture", "main:refs/heads/delete-me"],
        );
        let service = GitService::new();
        let initial = service.observe(repo.path()).await.expect("observe");
        service
            .branch_delete(
                repo.path(),
                BranchDeleteOptions {
                    branch: "delete-me".to_string(),
                    remote: Some("fixture".to_string()),
                    force: false,
                    confirm_force: false,
                    confirm_remote: true,
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect("confirmed remote delete");
        let deleted = run_git(
            remote.path(),
            &["show-ref", "--verify", "--quiet", "refs/heads/delete-me"],
        )
        .await
        .expect("inspect remote");
        assert!(!deleted.success, "remote branch must be deleted");
    }

    #[tokio::test]
    async fn structured_operations_reject_option_like_refs_and_remotes() {
        let repo = repository();
        let service = GitService::new();
        let initial = service.observe(repo.path()).await.expect("observe");

        let switch = service
            .branch_switch(
                repo.path(),
                BranchSwitchOptions {
                    branch: "--detach".to_string(),
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect_err("option-like branch must be rejected");
        assert!(matches!(switch, GitError::InvalidInput { .. }));

        let merge = service
            .merge(
                repo.path(),
                MergeOptions {
                    target: "--abort".to_string(),
                    ff_only: false,
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect_err("option-like revision must be rejected");
        assert!(matches!(merge, GitError::InvalidInput { .. }));

        let remote = service
            .branch_delete(
                repo.path(),
                BranchDeleteOptions {
                    branch: "topic".to_string(),
                    remote: Some("--force".to_string()),
                    force: false,
                    confirm_force: false,
                    confirm_remote: true,
                    precondition: precondition(&initial),
                },
            )
            .await
            .expect_err("option-like remote must be rejected");
        assert!(matches!(remote, GitError::InvalidInput { .. }));
    }

    #[test]
    fn conflict_status_matrix_is_explicit() {
        let statuses = BTreeSet::from([
            ('D', 'D'),
            ('A', 'U'),
            ('U', 'D'),
            ('U', 'A'),
            ('D', 'U'),
            ('A', 'A'),
            ('U', 'U'),
        ]);
        for x in [' ', 'A', 'D', 'M', 'R', 'U'] {
            for y in [' ', 'A', 'D', 'M', 'R', 'U'] {
                assert_eq!(is_conflicted_status(x, y), statuses.contains(&(x, y)));
            }
        }
    }

    #[tokio::test]
    async fn session_context_reports_external_branch_or_head_drift() {
        let repo = repository();
        let service = GitService::new();
        let registry = SessionCodeContextRegistry::default();
        let initial = service.observe(repo.path()).await.expect("observe");
        let bound = registry.accept("session-1", "workspace-1", vec![], &initial);
        assert!(!bound.has_external_drift());

        fs::write(repo.path().join("external.txt"), "external\n").expect("external fixture");
        git(repo.path(), &["add", "external.txt"]);
        git(repo.path(), &["commit", "-m", "external"]);
        let external = service
            .observe(repo.path())
            .await
            .expect("external observe");
        let drifted = registry.observe("session-1", "workspace-1", vec![], &external);
        assert!(drifted.has_external_drift());
        assert_eq!(drifted.git.base_head, initial.head);
        assert_eq!(drifted.git.observed_head, external.head);

        let accepted = registry.accept("session-1", "workspace-1", vec![], &external);
        assert!(!accepted.has_external_drift());
        assert!(accepted.context_revision > bound.context_revision);
    }

    #[test]
    fn workspace_coordinator_closes_turn_mutation_race() {
        let coordinator = WorkspaceGitOperationCoordinator::default();
        let repository = PathBuf::from("/tmp/magi-git-coordinator-repo/.git");
        coordinator
            .begin_execution("session-1", &repository)
            .expect("execution lease");
        let parallel_session = coordinator
            .begin_execution("session-parallel", &repository)
            .expect_err("sessions sharing one worktree must execute serially");
        assert!(matches!(
            parallel_session,
            GitCoordinationError::ExecutionActive { .. }
        ));
        let blocked = match coordinator.begin_mutation(&repository) {
            Ok(_) => panic!("mutation must not overlap execution"),
            Err(error) => error,
        };
        assert!(matches!(
            blocked,
            GitCoordinationError::ExecutionActive { .. }
        ));

        coordinator.end_execution("session-1");
        let mutation = coordinator
            .begin_mutation(&repository)
            .expect("mutation lease");
        let blocked_execution = coordinator
            .begin_execution("session-2", &repository)
            .expect_err("execution must not overlap mutation");
        assert!(matches!(
            blocked_execution,
            GitCoordinationError::MutationActive { .. }
        ));
        drop(mutation);
        coordinator
            .begin_execution("session-2", &repository)
            .expect("execution after mutation");
    }
}
