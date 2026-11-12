use super::*;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRefObservation {
    pub branch: Option<String>,
    pub head: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitTreeBaselineEntry {
    Deleted { path: String },
    RegularFile { path: String, content: Vec<u8> },
    Symlink { path: String, target: Vec<u8> },
    Gitlink { path: String, object_id: String },
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
    /// 既有持久合同：空值表示个人会话，按 session + 仓库根区分基线。
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
    /// 创建该 worktree 时持有的执行租约。租约是一次执行代际的唯一标识，
    /// 用于防止旧 dispatch 在任务恢复后误清理新一轮 worktree。
    #[serde(default)]
    pub lease_id: Option<String>,
    pub worker_id: String,
    pub path: PathBuf,
    pub mode: AgentWorktreeMode,
    pub base_head: String,
    pub branch: Option<String>,
    pub active: bool,
    /// 代理结束时运行时提交其改动后的分支 HEAD；没有改动或未提交时为空。
    #[serde(default)]
    pub result_head: Option<String>,
    /// `base_head..result_head` 之间改动的路径。
    #[serde(default)]
    pub changed_paths: Vec<String>,
    /// 改动是否已经由主线通过 agent_apply 应用到主工作树。
    #[serde(default)]
    pub applied: bool,
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
    pub(super) contexts: Arc<RwLock<HashMap<SessionContextKey, SessionCodeContext>>>,
}

impl SessionCodeContextRegistry {
    /// 仅兼容唯一项目仓库的旧读入口；多仓库时不猜测默认仓库。
    pub fn get(&self, session_id: &str) -> Option<SessionCodeContext> {
        let mut contexts = self
            .for_session(session_id)
            .into_iter()
            .filter(|context| !context.workspace_id.is_empty());
        let only = contexts.next()?;
        contexts.next().is_none().then_some(only)
    }

    pub fn for_session(&self, session_id: &str) -> Vec<SessionCodeContext> {
        self.all()
            .into_iter()
            .filter(|context| context.session_id == session_id)
            .collect()
    }

    /// 项目和个人会话都按实际 worktree 保存独立 Git 基线。
    pub fn get_repository(&self, session_id: &str, root: &Path) -> Option<SessionCodeContext> {
        self.contexts
            .read()
            .expect("context lock")
            .get(&Self::context_key(session_id, "", root))
            .cloned()
    }

    /// 寻找实际执行路径所属仓库（包括子代理 worktree）。不跨到另一个任意仓库。
    pub fn get_for_path(&self, session_id: &str, path: &Path) -> Option<SessionCodeContext> {
        let path = canonical_or_original(path);
        let contexts = self.for_session(session_id);
        let mut candidates = contexts
            .into_iter()
            .filter_map(|context| {
                let longest = std::iter::once(&context.execution_root)
                    .chain(context.runtime_workspace_roots.iter())
                    .filter_map(|root| {
                        let root = canonical_or_original(root);
                        path.starts_with(&root).then_some(root.components().count())
                    })
                    .max()?;
                Some((longest, context))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
        if candidates.len() > 1 && candidates[0].0 == candidates[1].0 {
            return None;
        }
        candidates.into_iter().next().map(|(_, context)| context)
    }

    pub(super) fn context_key(
        session_id: &str,
        _workspace_id: &str,
        root: &Path,
    ) -> SessionContextKey {
        SessionContextKey::Repository {
            session_id: session_id.to_string(),
            root: canonical_or_original(root),
        }
    }

    pub(super) fn unambiguous_root(
        &self,
        session_id: &str,
    ) -> Result<PathBuf, SessionContextError> {
        self.get(session_id)
            .map(|context| context.execution_root)
            .ok_or_else(|| SessionContextError::Missing {
                session_id: session_id.to_string(),
            })
    }

    pub fn all(&self) -> Vec<SessionCodeContext> {
        let mut contexts = self
            .contexts
            .read()
            .expect("session code context read lock poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        contexts.sort_by(|left, right| {
            left.session_id
                .cmp(&right.session_id)
                .then_with(|| left.execution_root.cmp(&right.execution_root))
        });
        contexts
    }

    /// daemon 启动时载入持久化的上下文。上一个进程里的子代理不可能仍在运行，
    /// 因此所有 worktree 的“占用中”标记都已失效：统一改为非占用并移出运行期工作区根，
    /// 否则主线的 Git 写操作会以“仍有子代理在执行”为由被永久拒绝。目录与产出记录保留。
    pub fn restore_after_restart(&self, contexts: Vec<SessionCodeContext>) -> usize {
        let mut released = 0;
        let mut target = self
            .contexts
            .write()
            .expect("session code context write lock poisoned");
        target.clear();
        for mut context in contexts {
            for worktree in context
                .agent_worktrees
                .iter_mut()
                .filter(|worktree| worktree.active)
            {
                worktree.active = false;
                let path = worktree.path.clone();
                context
                    .runtime_workspace_roots
                    .retain(|root| !same_path(root, &path));
                released += 1;
            }
            target.insert(
                Self::context_key(
                    &context.session_id,
                    &context.workspace_id,
                    &context.execution_root,
                ),
                context,
            );
        }
        released
    }

    pub fn remove(&self, session_id: &str) -> Option<SessionCodeContext> {
        let mut contexts = self
            .contexts
            .write()
            .expect("session code context write lock poisoned");
        let only = contexts
            .values()
            .filter(|context| context.session_id == session_id)
            .cloned()
            .collect::<Vec<_>>();
        contexts.retain(|_, context| context.session_id != session_id);
        (only.len() == 1).then(|| only[0].clone())
    }

    pub fn remove_for_root(&self, session_id: &str, root: &Path) -> Option<SessionCodeContext> {
        self.contexts
            .write()
            .expect("session code context write lock poisoned")
            .remove(&Self::context_key(session_id, "", root))
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

    pub fn validate_revision_for_root(
        &self,
        session_id: &str,
        root: &Path,
        expected_revision: Option<u64>,
    ) -> Result<(), SessionContextError> {
        let Some(expected_revision) = expected_revision else {
            return Ok(());
        };
        let context =
            self.get_repository(session_id, root)
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
        let root = self.unambiguous_root(session_id)?;
        self.register_agent_worktree_for_root(session_id, &root, agent_worktree)
    }

    pub fn register_agent_worktree_for_root(
        &self,
        session_id: &str,
        root: &Path,
        agent_worktree: AgentWorktreeContext,
    ) -> Result<SessionCodeContext, SessionContextError> {
        let mut contexts = self
            .contexts
            .write()
            .expect("session code context write lock poisoned");
        let context = contexts
            .get_mut(&Self::context_key(session_id, "", root))
            .ok_or_else(|| SessionContextError::Missing {
                session_id: session_id.to_string(),
            })?;
        context.agent_worktrees.retain(|existing| {
            !(existing.task_id == agent_worktree.task_id
                && existing.lease_id == agent_worktree.lease_id)
        });
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
        let root = self.unambiguous_root(session_id)?;
        self.release_agent_worktree_for_root(session_id, &root, task_id)
    }

    pub fn release_agent_worktree_for_root(
        &self,
        session_id: &str,
        root: &Path,
        task_id: &str,
    ) -> Result<SessionCodeContext, SessionContextError> {
        let mut contexts = self
            .contexts
            .write()
            .expect("session code context write lock poisoned");
        let context = contexts
            .get_mut(&Self::context_key(session_id, "", root))
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

    pub fn record_agent_worktree_result_for_root(
        &self,
        session_id: &str,
        root: &Path,
        task_id: &str,
        lease_id: &str,
        result_head: String,
        changed_paths: Vec<String>,
    ) -> Result<SessionCodeContext, SessionContextError> {
        let mut contexts = self
            .contexts
            .write()
            .expect("session code context write lock poisoned");
        let context = contexts
            .get_mut(&Self::context_key(session_id, "", root))
            .ok_or_else(|| SessionContextError::Missing {
                session_id: session_id.to_string(),
            })?;
        let agent_worktree = context
            .agent_worktrees
            .iter_mut()
            .find(|worktree| {
                worktree.task_id == task_id && worktree.lease_id.as_deref() == Some(lease_id)
            })
            .ok_or_else(|| SessionContextError::MissingAgentWorktree {
                task_id: format!("{task_id} (lease {lease_id})"),
            })?;
        agent_worktree.result_head = Some(result_head);
        agent_worktree.changed_paths = changed_paths;
        context.context_revision = context.context_revision.saturating_add(1);
        Ok(context.clone())
    }

    pub fn mark_agent_worktree_applied_for_root(
        &self,
        session_id: &str,
        root: &Path,
        task_id: &str,
        result_head: &str,
    ) -> Result<SessionCodeContext, SessionContextError> {
        let mut contexts = self
            .contexts
            .write()
            .expect("session code context write lock poisoned");
        let context = contexts
            .get_mut(&Self::context_key(session_id, "", root))
            .ok_or_else(|| SessionContextError::Missing {
                session_id: session_id.to_string(),
            })?;
        let agent_worktree = context
            .agent_worktrees
            .iter_mut()
            .find(|worktree| {
                worktree.task_id == task_id && worktree.result_head.as_deref() == Some(result_head)
            })
            .ok_or_else(|| SessionContextError::MissingAgentWorktree {
                task_id: task_id.to_string(),
            })?;
        agent_worktree.applied = true;
        context.context_revision = context.context_revision.saturating_add(1);
        Ok(context.clone())
    }

    /// 只释放指定执行租约对应的 worktree。旧 dispatch 到达时如果任务已经
    /// 获得新租约，必须保持新一轮 worktree 的 active 状态。
    pub fn release_agent_worktree_for_lease(
        &self,
        session_id: &str,
        task_id: &str,
        lease_id: &str,
    ) -> Result<SessionCodeContext, SessionContextError> {
        let root = self.unambiguous_root(session_id)?;
        self.release_agent_worktree_for_lease_for_root(session_id, &root, task_id, lease_id)
    }

    pub fn release_agent_worktree_for_lease_for_root(
        &self,
        session_id: &str,
        root: &Path,
        task_id: &str,
        lease_id: &str,
    ) -> Result<SessionCodeContext, SessionContextError> {
        let mut contexts = self
            .contexts
            .write()
            .expect("session code context write lock poisoned");
        let context = contexts
            .get_mut(&Self::context_key(session_id, "", root))
            .ok_or_else(|| SessionContextError::Missing {
                session_id: session_id.to_string(),
            })?;
        let agent_worktree = context
            .agent_worktrees
            .iter_mut()
            .find(|worktree| {
                worktree.task_id == task_id
                    && worktree.active
                    && worktree.lease_id.as_deref() == Some(lease_id)
            })
            .ok_or_else(|| SessionContextError::MissingAgentWorktree {
                task_id: format!("{task_id} (lease {lease_id})"),
            })?;
        agent_worktree.active = false;
        context
            .runtime_workspace_roots
            .retain(|root| !same_path(root, &agent_worktree.path));
        context.context_revision = context.context_revision.saturating_add(1);
        Ok(context.clone())
    }

    /// 恢复沿原 Task 的分配记录切换租约。文件和 index 保留，旧 dispatch 的
    /// lease-scoped release 不得释放新执行；Git 来源检查由调用方在提交前完成。
    pub fn reattach_agent_worktree(
        &self,
        session_id: &str,
        expected: &AgentWorktreeContext,
        lease_id: &str,
        worker_id: &str,
    ) -> Result<SessionCodeContext, SessionContextError> {
        let root = self.unambiguous_root(session_id)?;
        self.reattach_agent_worktree_for_root(session_id, &root, expected, lease_id, worker_id)
    }

    pub fn reattach_agent_worktree_for_root(
        &self,
        session_id: &str,
        root: &Path,
        expected: &AgentWorktreeContext,
        lease_id: &str,
        worker_id: &str,
    ) -> Result<SessionCodeContext, SessionContextError> {
        let mut contexts = self
            .contexts
            .write()
            .expect("session code context write lock poisoned");
        let context = contexts
            .get_mut(&Self::context_key(session_id, "", root))
            .ok_or_else(|| SessionContextError::Missing {
                session_id: session_id.to_string(),
            })?;
        if lease_id.trim().is_empty()
            || worker_id.trim().is_empty()
            || expected.active
            || expected.applied
            || expected.lease_id.as_deref() == Some(lease_id)
            || !expected.path.is_dir()
            || context.agent_worktrees.iter().any(|worktree| {
                worktree.active
                    && (worktree.task_id == expected.task_id
                        || same_path(&worktree.path, &expected.path))
            })
        {
            return Err(SessionContextError::MissingAgentWorktree {
                task_id: format!("{} (inactive recovery source required)", expected.task_id),
            });
        }
        let allocation = context
            .agent_worktrees
            .iter_mut()
            .find(|worktree| *worktree == expected)
            .ok_or_else(|| SessionContextError::MissingAgentWorktree {
                task_id: format!("{} (recovery source changed)", expected.task_id),
            })?;
        allocation.lease_id = Some(lease_id.to_string());
        allocation.worker_id = worker_id.to_string();
        allocation.active = true;
        context.runtime_workspace_roots.push(expected.path.clone());
        context.runtime_workspace_roots.sort();
        context.runtime_workspace_roots.dedup();
        context.context_revision = context.context_revision.saturating_add(1);
        Ok(context.clone())
    }

    pub(super) fn upsert(
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
        let context = contexts
            .entry(Self::context_key(
                session_id,
                workspace_id,
                &observation.worktree_path,
            ))
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
        // 刷新主线观测不能撤销仍在执行的子代理目录归属。
        runtime_workspace_roots.extend(
            context
                .agent_worktrees
                .iter()
                .filter(|agent| agent.active)
                .map(|agent| agent.path.clone()),
        );
        runtime_workspace_roots.sort();
        runtime_workspace_roots.dedup();
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
