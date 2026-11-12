use super::*;

#[derive(Clone, Default)]
pub struct WorkspaceGitOperationCoordinator {
    pub(super) state: Arc<Mutex<WorkspaceGitOperationState>>,
}

#[derive(Default)]
pub(super) struct WorkspaceGitOperationState {
    pub(super) active_executions: HashMap<(String, PathBuf), ()>,
    pub(super) active_mutations: HashSet<PathBuf>,
    pub(super) suspended_executions: HashMap<String, Vec<PathBuf>>,
    /// 因仓库被占用而排队的 session，按先后顺序。只有队首能在仓库空闲时拿到租约，
    /// 否则一个连续续跑的目标会在每轮之间抢先重新占用，其它会话永远等不到。
    pub(super) waiters: Vec<ExecutionWaiter>,
}

pub(super) struct ExecutionWaiter {
    pub(super) session_id: String,
    pub(super) repository_keys: Vec<PathBuf>,
    pub(super) refreshed_at: std::time::Instant,
}

/// 排队者每次重试都会刷新自己；超过这个时间没刷新的视为已经放弃（对应的 future 被丢弃、
/// 进程内任务崩溃等），不再阻挡后面的 session。
pub(super) const EXECUTION_WAITER_STALE_AFTER: std::time::Duration =
    std::time::Duration::from_secs(5);

impl WorkspaceGitOperationState {
    pub(super) fn prune_stale_waiters(&mut self) {
        self.waiters
            .retain(|waiter| waiter.refreshed_at.elapsed() < EXECUTION_WAITER_STALE_AFTER);
    }
}

pub struct GitMutationLease {
    pub(super) repository_key: PathBuf,
    pub(super) coordinator: WorkspaceGitOperationCoordinator,
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
            .and_then(|state| {
                state
                    .active_executions
                    .keys()
                    .any(|(active_session, active_repository)| {
                        active_session == session_id
                            && same_path(active_repository, &repository_key)
                    })
                    .then_some(())
            })
            .is_some()
    }

    pub fn begin_execution(
        &self,
        session_id: &str,
        git_common_dir: &Path,
    ) -> Result<(), GitCoordinationError> {
        // 运行中的子代理也用此入口复核占用；失败不能释放同会话其它任务正在用的租约。
        self.acquire_execution_set(session_id, &[git_common_dir.to_path_buf()], false)
    }

    /// 在同一把协调锁里获取规范化、排序、去重后的整组仓库。无法全部取得时
    /// 释放该会话已持有的整组资源，再由调用者等待/重试，避免交叉持有死锁。
    pub fn begin_execution_set(
        &self,
        session_id: &str,
        git_common_dirs: &[PathBuf],
    ) -> Result<(), GitCoordinationError> {
        self.acquire_execution_set(session_id, git_common_dirs, true)
    }

    pub(super) fn acquire_execution_set(
        &self,
        session_id: &str,
        git_common_dirs: &[PathBuf],
        release_on_failure: bool,
    ) -> Result<(), GitCoordinationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| GitCoordinationError::Internal(error.to_string()))?;
        let mut desired = git_common_dirs
            .iter()
            .map(|root| canonical_or_original(root))
            .chain(
                state
                    .active_executions
                    .keys()
                    .filter(|(id, _)| id == session_id)
                    .map(|(_, path)| path.clone()),
            )
            .collect::<Vec<_>>();
        desired.sort();
        desired.dedup();
        state.prune_stale_waiters();
        let check = desired.iter().try_for_each(|repository_key| {
            if state
                .active_executions
                .contains_key(&(session_id.to_string(), repository_key.clone()))
            {
                return Ok(());
            }
            if state.active_mutations.contains(repository_key) {
                return Err(GitCoordinationError::MutationActive {
                    repository_key: repository_key.clone(),
                });
            }
            let active_session_ids = state
                .active_executions
                .keys()
                .filter(|(id, repository)| {
                    id != session_id && same_path(repository, repository_key)
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            if !active_session_ids.is_empty() {
                return Err(GitCoordinationError::ExecutionActive {
                    repository_key: repository_key.clone(),
                    active_session_ids,
                });
            }
            if let Some(head) = state
                .waiters
                .iter()
                .find(|waiter| waiter.repository_keys.contains(repository_key))
                && head.session_id != session_id
            {
                return Err(GitCoordinationError::ExecutionActive {
                    repository_key: repository_key.clone(),
                    active_session_ids: vec![head.session_id.clone()],
                });
            }
            Ok(())
        });
        if let Err(error) = check {
            if release_on_failure {
                state
                    .active_executions
                    .retain(|(id, _), _| id != session_id);
            }
            return Err(error);
        }
        state
            .waiters
            .retain(|waiter| waiter.session_id != session_id);
        for root in desired {
            state
                .active_executions
                .insert((session_id.to_string(), root), ());
        }
        Ok(())
    }

    pub fn execution_roots(&self, session_id: &str) -> Vec<PathBuf> {
        let mut roots = self
            .state
            .lock()
            .map(|state| {
                state
                    .active_executions
                    .keys()
                    .filter(|(id, _)| id == session_id)
                    .map(|(_, path)| path.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        roots.sort();
        roots
    }

    /// 等人时保留原租约集合，恢复不能从当前项目成员或历史 context 扩张范围。
    pub fn suspend_execution(&self, session_id: &str) {
        if let Ok(mut state) = self.state.lock() {
            let roots = state
                .active_executions
                .keys()
                .filter(|(id, _)| id == session_id)
                .map(|(_, path)| path.clone())
                .collect::<Vec<_>>();
            if !roots.is_empty() {
                state.suspended_executions.insert(session_id.into(), roots);
            }
            state
                .active_executions
                .retain(|(id, _), _| id != session_id);
        }
    }

    pub fn try_resume_execution(&self, session_id: &str) -> Result<(), GitCoordinationError> {
        let roots = self
            .state
            .lock()
            .map_err(|error| GitCoordinationError::Internal(error.to_string()))?
            .suspended_executions
            .get(session_id)
            .cloned()
            .unwrap_or_default();
        let result = self.begin_execution_set(session_id, &roots);
        match &result {
            Ok(()) => {
                if let Ok(mut state) = self.state.lock() {
                    state.suspended_executions.remove(session_id);
                }
            }
            Err(_) => {
                for root in &roots {
                    self.register_waiter(session_id, root);
                }
            }
        }
        result
    }

    /// 登记（或刷新）一个因仓库被占用而等待的 session。等待者每次重试 `begin_execution`
    /// 失败后调用；拿到租约、`end_execution` 或超过一段时间没刷新都会自动退出队列。
    pub fn register_waiter(&self, session_id: &str, git_common_dir: &Path) {
        let repository_key = canonical_or_original(git_common_dir);
        if let Ok(mut state) = self.state.lock() {
            state.prune_stale_waiters();
            // 每个会话占据一个全局队列位置，全部仓库共享该次序。逐仓库排队会
            // 因 A→B 与 B→A 的登记顺序形成两个互相等待的队首。
            if let Some(waiter) = state
                .waiters
                .iter_mut()
                .find(|waiter| waiter.session_id == session_id)
            {
                waiter.refreshed_at = std::time::Instant::now();
                if !waiter.repository_keys.contains(&repository_key) {
                    waiter.repository_keys.push(repository_key);
                    waiter.repository_keys.sort();
                }
                return;
            }
            state.waiters.push(ExecutionWaiter {
                session_id: session_id.to_string(),
                repository_keys: vec![repository_key],
                refreshed_at: std::time::Instant::now(),
            });
        }
    }

    /// 放弃等待（轮次被取消等）。
    pub fn cancel_waiter(&self, session_id: &str) {
        if let Ok(mut state) = self.state.lock() {
            state
                .waiters
                .retain(|waiter| waiter.session_id != session_id);
        }
    }

    pub fn end_execution(&self, session_id: &str) {
        if let Ok(mut state) = self.state.lock() {
            state
                .active_executions
                .retain(|(id, _), _| id != session_id);
            state.suspended_executions.remove(session_id);
            state
                .waiters
                .retain(|waiter| waiter.session_id != session_id);
        }
    }

    pub fn end_execution_for_repository(&self, session_id: &str, git_common_dir: &Path) {
        let repository_key = canonical_or_original(git_common_dir);
        if let Ok(mut state) = self.state.lock() {
            state.active_executions.retain(|(id, repository), _| {
                !(id == session_id && same_path(repository, &repository_key))
            });
            if let Some(roots) = state.suspended_executions.get_mut(session_id) {
                roots.retain(|root| !same_path(root, &repository_key));
                if roots.is_empty() {
                    state.suspended_executions.remove(session_id);
                }
            }
            for waiter in state
                .waiters
                .iter_mut()
                .filter(|waiter| waiter.session_id == session_id)
            {
                waiter
                    .repository_keys
                    .retain(|root| !same_path(root, &repository_key));
            }
            state
                .waiters
                .retain(|waiter| !waiter.repository_keys.is_empty());
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
            .filter(|((_, active_repository), _)| same_path(active_repository, &repository_key))
            .map(|((session_id, _), _)| session_id.clone())
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
    #[error("Git coordination state 不可用: {0}")]
    Internal(String),
}
