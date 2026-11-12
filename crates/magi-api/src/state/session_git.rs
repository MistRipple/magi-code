use super::*;

impl ApiState {
    pub fn persist_session_git_contexts(&self) -> Result<(), ApiError> {
        let Some(persistence) = &self.runtime_persistence else {
            return Ok(());
        };
        let Some(state_root) = persistence.state_root() else {
            return Ok(());
        };
        persistence.save_json(
            &state_root.join("session-git-contexts.json"),
            &self.session_code_contexts.all(),
        )
    }

    /// 本轮首次工作区执行前重新观测 session 绑定的 Git branch/HEAD/worktree。
    ///
    /// 同仓库、同 worktree、同分支的线性快进会自动成为新基线，保证提交、拉取或其他
    /// 正常协作操作不会中断后续对话。分支切换、HEAD 回退或历史改写仍需用户明确接受。
    pub async fn ensure_session_code_context(
        &self,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
    ) -> Result<Option<magi_git::SessionCodeContext>, ApiError> {
        let Some(workspace_id) = workspace_id else {
            return Ok(None);
        };
        // 旧入口无法表达目录身份，多根时必须由调用方指定源目录再执行。
        let (_, workspace_root) = self
            .workspace_registry
            .resolve_source_folder(workspace_id, None)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
        self.ensure_session_code_context_for_root(session_id, workspace_id, &workspace_root)
            .await
    }

    pub(crate) async fn ensure_session_code_context_for_root(
        &self,
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
        workspace_root: &Path,
    ) -> Result<Option<magi_git::SessionCodeContext>, ApiError> {
        let workspace_root =
            std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
        if let Some(isolation) = self
            .session_isolations
            .get_for_source(session_id.as_str(), &workspace_root)
        {
            if isolation.source_root != workspace_root || !isolation.root.is_dir() {
                return Err(ApiError::Conflict(
                    "隔离副本与原目录绑定不匹配或已丢失".into(),
                ));
            }
            return Ok(None);
        } else if self.session_isolations.contains(session_id.as_str()) {
            return Err(ApiError::Conflict(
                "源文件夹缺少会话隔离映射，不能回落原目录".into(),
            ));
        }
        let _change_sync_guard = self.lock_session_change_sync(session_id).await;
        tracing::info!(%session_id, stage = "change_sync_acquired", "workspace execution preparation");
        let session_key = session_id.as_str();
        let existing_context = self
            .session_code_contexts
            .get_for_path(session_key, &workspace_root);
        if existing_context.as_ref().is_some_and(|context| {
            std::fs::canonicalize(&context.execution_root)
                .unwrap_or_else(|_| context.execution_root.clone())
                != workspace_root
                && !context.runtime_workspace_roots.iter().any(|path| {
                    std::fs::canonicalize(path).unwrap_or_else(|_| path.clone()) == workspace_root
                })
        }) {
            return Err(ApiError::Conflict(
                "会话 Git 上下文属于另一个源文件夹，不能重用目录绑定".into(),
            ));
        }
        if let Some(existing_context) = existing_context.as_ref() {
            self.begin_session_git_execution(session_id, &existing_context.git.git_common_dir)
                .await?;
        }
        let mut observation = match self.git_service.observe(&workspace_root).await {
            Ok(observation) => observation,
            Err(magi_git::GitError::NotRepository { .. }) => {
                // 普通源目录没有 Git 资源；同会话其它获授仓库的租约仍在使用。
                return Ok(None);
            }
            Err(error) => {
                self.release_session_git_execution_lease(session_id);
                return Err(ApiError::InternalAssemblyError(format!(
                    "每轮执行前校验 Git context 失败: {error}"
                )));
            }
        };
        tracing::info!(%session_id, stage = "git_observed", "workspace execution preparation");
        if existing_context.is_none() {
            // 首次观测只用于确定需要哪把 repository lease。等到执行权后重新观测，
            // 再建立快照基线，不能记录另一个会话尚未完成写入的中间状态。
            self.begin_session_git_execution(session_id, &observation.git_common_dir)
                .await?;
            tracing::info!(%session_id, stage = "git_lease_acquired", "workspace execution preparation");
            let leased_common_dir = observation.git_common_dir.clone();
            observation = match self.git_service.observe(&workspace_root).await {
                Ok(observation) if observation.git_common_dir == leased_common_dir => observation,
                Ok(_) => {
                    self.release_session_git_execution_lease(session_id);
                    return Err(ApiError::Conflict(
                        "等待执行期间工作区 Git repository 已变化，请重新核对".into(),
                    ));
                }
                Err(error) => {
                    self.release_session_git_execution_lease(session_id);
                    return Err(ApiError::internal_assembly(
                        "取得执行租约后观测 Git 失败",
                        error,
                    ));
                }
            };
            let snapshot = match self
                .ensure_snapshot_session(session_id, &observation.worktree_path)
                .await
            {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    self.release_session_git_execution_lease(session_id);
                    return Err(error);
                }
            };
            tracing::info!(%session_id, stage = "git_snapshot_loaded", "workspace execution preparation");
            if let Err(error) = self
                .align_snapshot_baseline_to_observation_head(
                    &observation.worktree_path,
                    &observation,
                    &snapshot,
                )
                .await
            {
                self.release_session_git_execution_lease(session_id);
                return Err(error);
            }
        }
        let mut context = if existing_context.is_some() {
            self.session_code_contexts.observe(
                session_key,
                workspace_id.as_str(),
                vec![workspace_root.clone()],
                &observation,
            )
        } else {
            self.session_code_contexts.accept(
                session_key,
                workspace_id.as_str(),
                vec![workspace_root.clone()],
                &observation,
            )
        };
        let mut adopted_fast_forward_from = None;
        if context.has_external_drift()
            && let Some(existing_context) = existing_context.as_ref()
            && match self
                .git_service
                .is_session_context_fast_forward(existing_context, &observation)
                .await
            {
                Ok(is_fast_forward) => is_fast_forward,
                Err(error) => {
                    tracing::warn!(
                        session_id = %existing_context.session_id,
                        previous_head = ?existing_context.git.base_head,
                        current_head = ?observation.head,
                        ?error,
                        "判断 session Git context 是否安全快进失败，保持显式确认"
                    );
                    false
                }
            }
        {
            let snapshot = match self
                .ensure_snapshot_session(session_id, &observation.worktree_path)
                .await
            {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    self.release_session_git_execution_lease(session_id);
                    return Err(error);
                }
            };
            if let Err(error) = self
                .advance_snapshot_baseline_for_fast_forward(
                    session_id,
                    &observation.worktree_path,
                    existing_context,
                    &observation,
                    &snapshot,
                )
                .await
            {
                self.release_session_git_execution_lease(session_id);
                return Err(error);
            }
            context = self.session_code_contexts.accept(
                session_key,
                workspace_id.as_str(),
                context.runtime_workspace_roots.clone(),
                &observation,
            );
            adopted_fast_forward_from = existing_context.git.base_head.clone();
        }
        if context.has_external_drift() {
            self.release_session_git_execution_lease(session_id);
            self.persist_session_git_contexts()?;
            return Err(ApiError::Conflict(format!(
                "Git context 发生高风险变化：期望 branch={:?} HEAD={:?}，实际 branch={:?} HEAD={:?}。分支切换、HEAD 回退或历史改写需要明确接受新基线，或切回原基线后重试",
                context.git.desired_ref,
                context.git.base_head,
                context.git.observed_branch,
                context.git.observed_head
            )));
        }
        if !context.git.dirty.conflicted_paths.is_empty() {
            self.release_session_git_execution_lease(session_id);
            self.persist_session_git_contexts()?;
            return Err(ApiError::Conflict(format!(
                "当前 workspace 存在未解决的 Git merge conflict，必须先解决或 abort 后再执行新一轮：{}",
                context.git.dirty.conflicted_paths.join(", ")
            )));
        }
        if let Err(error) = self.persist_session_git_contexts() {
            self.release_session_git_execution_lease(session_id);
            return Err(error);
        }
        if let Some(previous_head) = adopted_fast_forward_from {
            self.publish_session_git_fast_forward(
                session_id,
                workspace_id,
                &context,
                &previous_head,
            );
            self.schedule_workspace_code_index(
                workspace_id.clone(),
                observation.worktree_path.clone(),
            );
        }
        Ok(Some(context))
    }

    /// 占用 repository 的独占执行租约；仓库正被别的会话执行时排队等待，而不是让这一轮失败。
    ///
    /// 同一 repository 的主 session 共享 live worktree，必须串行执行。这一轮已经被接受，
    /// 若在这里直接报冲突，用户的消息就变成一条失败的 Turn，目标续跑则会被记成运行时
    /// 错误而受阻。只有这个会话自己确有一轮正在启动（current turn 未终态）时才排队等待；
    /// 其它场景（例如用户手动触发的 Git 操作）保持立即失败。等待按先后顺序交接，
    /// 轮次被取消（终态）后放弃等待。
    pub(super) async fn begin_session_git_execution(
        &self,
        session_id: &SessionId,
        git_common_dir: &std::path::Path,
    ) -> Result<(), ApiError> {
        self.begin_session_git_execution_set(session_id, &[git_common_dir.to_path_buf()], None)
            .await
    }

    pub(crate) async fn begin_session_git_execution_set(
        &self,
        session_id: &SessionId,
        git_common_dirs: &[PathBuf],
        expected_turn_id: Option<&str>,
    ) -> Result<(), ApiError> {
        let mut announced = false;
        let result = loop {
            if let Some(turn_id) = expected_turn_id
                && !self
                    .session_store
                    .runtime_sidecar(session_id)
                    .and_then(|sidecar| sidecar.current_turn)
                    .is_some_and(|turn| {
                        turn.turn_id == turn_id
                            && self
                                .session_store
                                .canonical_turn_for_session_turn_id(session_id, turn_id)
                                .is_some_and(|turn| !turn.status.is_terminal())
                    })
            {
                self.workspace_git_coordinator
                    .cancel_waiter(session_id.as_str());
                break Err(ApiError::Conflict(
                    "工作区准备目标 Turn 已关闭或变化".into(),
                ));
            }
            match self
                .workspace_git_coordinator
                .begin_execution_set(session_id.as_str(), git_common_dirs)
            {
                Ok(()) => break Ok(()),
                Err(
                    error @ (magi_git::GitCoordinationError::ExecutionActive { .. }
                    | magi_git::GitCoordinationError::MutationActive { .. }),
                ) => {
                    let turn_is_starting = matches!(
                        self.session_store
                            .ensure_current_turn_acceptance_available(session_id),
                        Err(DomainError::CurrentTurnConflict { .. })
                    );
                    if !turn_is_starting {
                        self.workspace_git_coordinator
                            .cancel_waiter(session_id.as_str());
                        break Err(ApiError::Conflict(error.to_string()));
                    }
                    for root in git_common_dirs {
                        self.workspace_git_coordinator
                            .register_waiter(session_id.as_str(), root);
                    }
                    if !announced {
                        announced = true;
                        let blocking: Vec<String> = match &error {
                            magi_git::GitCoordinationError::ExecutionActive {
                                active_session_ids,
                                ..
                            } => active_session_ids.clone(),
                            _ => Vec::new(),
                        };
                        tracing::info!(
                            session_id = %session_id,
                            reason = %error,
                            "repository 正被其它 session 执行，本轮排队等待"
                        );
                        self.publish_session_workspace_wait(session_id, true, &blocking);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                }
                Err(error) => {
                    self.workspace_git_coordinator
                        .cancel_waiter(session_id.as_str());
                    break Err(ApiError::Conflict(error.to_string()));
                }
            }
        };
        if announced {
            self.publish_session_workspace_wait(session_id, false, &[]);
        }
        result
    }

    /// 会话的这一轮开始 / 结束等待别的会话释放工作区。前端据此在运行指示里说明「正在等谁」，
    /// 而不是让用户对着一个没有输出的「运行中」干等。
    pub(super) fn publish_session_workspace_wait(
        &self,
        session_id: &SessionId,
        waiting: bool,
        blocking_session_ids: &[String],
    ) {
        let event_name = if waiting {
            "session.workspace.waiting"
        } else {
            "session.workspace.ready"
        };
        {
            let mut waits = self
                .session_workspace_waits
                .lock()
                .expect("session workspace waits lock poisoned");
            if waiting {
                waits.insert(
                    session_id.as_str().to_string(),
                    blocking_session_ids.to_vec(),
                );
            } else {
                waits.remove(session_id.as_str());
            }
        }
        let workspace_id = self
            .session_store
            .session(session_id)
            .and_then(|session| session.workspace_id)
            .map(WorkspaceId::new);
        let _ = self.event_bus.publish(
            EventEnvelope::domain(
                magi_core::EventId::unique(format!("{event_name}-{session_id}")),
                event_name,
                serde_json::json!({
                    "session_id": session_id,
                    "workspace_id": workspace_id,
                    "blocking_session_ids": blocking_session_ids,
                }),
            )
            .with_context(EventContext {
                session_id: Some(session_id.clone()),
                workspace_id,
                ..EventContext::default()
            }),
        );
    }

    pub(super) fn publish_session_git_fast_forward(
        &self,
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
        context: &magi_git::SessionCodeContext,
        previous_head: &str,
    ) {
        self.event_bus.publish(
            EventEnvelope::domain(
                magi_core::EventId::unique(format!("workspace-git-context-changed-{workspace_id}")),
                "workspace.git.context.changed",
                serde_json::json!({
                    "workspace_id": workspace_id,
                    "session_id": session_id,
                    "repository_root": context.git.repository_root,
                    "worktree_path": context.git.worktree_path,
                    "branch": context.git.observed_branch,
                    "head": context.git.observed_head,
                    "previous_head": previous_head,
                    "context_revision": context.context_revision,
                    "change_kind": "fast_forward_adopted",
                    "refresh_scopes": ["changes", "file_tree", "code_index", "knowledge", "context_cache"]
                }),
            )
            .with_context(EventContext {
                session_id: Some(session_id.clone()),
                workspace_id: Some(workspace_id.clone()),
                ..EventContext::default()
            }),
        );
    }

    pub(super) fn publish_session_git_drift(
        &self,
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
        context: &magi_git::SessionCodeContext,
        previous_head: Option<&str>,
        change_kind: &str,
    ) {
        self.event_bus.publish(
            EventEnvelope::domain(
                magi_core::EventId::unique(format!("workspace-git-context-changed-{workspace_id}")),
                "workspace.git.context.changed",
                serde_json::json!({
                    "workspace_id": workspace_id,
                    "session_id": session_id,
                    "repository_root": context.git.repository_root,
                    "worktree_path": context.git.worktree_path,
                    "branch": context.git.observed_branch,
                    "head": context.git.observed_head,
                    "previous_head": previous_head,
                    "context_revision": context.context_revision,
                    "change_kind": change_kind,
                    "refresh_scopes": ["changes", "file_tree", "code_index", "knowledge", "context_cache"]
                }),
            )
            .with_context(EventContext {
                session_id: Some(session_id.clone()),
                workspace_id: Some(workspace_id.clone()),
                ..EventContext::default()
            }),
        );
    }

    pub fn release_session_git_execution_lease(&self, session_id: &SessionId) {
        self.workspace_git_coordinator
            .end_execution(session_id.as_str());
    }

    pub fn release_session_git_execution_lease_for_repository(
        &self,
        session_id: &SessionId,
        git_common_dir: &std::path::Path,
    ) {
        self.workspace_git_coordinator
            .end_execution_for_repository(session_id.as_str(), git_common_dir);
    }

    /// 删除 session 时只回收干净的 agent worktree；任何未提交改动都保留目录与 context，
    /// 禁止用 `--force` 静默丢失代理产物。
    pub(super) async fn cleanup_session_git_resources(&self, session_id: &SessionId) -> bool {
        self.release_session_git_execution_lease(session_id);
        let personal = self
            .session_code_contexts
            .all()
            .into_iter()
            .filter(|context| {
                context.session_id == session_id.as_str() && context.workspace_id.is_empty()
            })
            .collect::<Vec<_>>();
        if !personal.is_empty() {
            let Some(state_root) = self
                .runtime_persistence()
                .and_then(RuntimeStatePersistence::state_root)
            else {
                return false;
            };
            let managed_root = state_root
                .join("worktrees/personal")
                .join(session_id.as_str());
            let managed_root = managed_root.canonicalize().unwrap_or(managed_root);
            for context in personal {
                let Ok(_lease) = self
                    .workspace_git_coordinator
                    .begin_mutation(&context.git.git_common_dir)
                else {
                    tracing::warn!(%session_id, "个人会话仓库正在使用，保留 Git 资源与执行目录");
                    return false;
                };
                let worktrees = match self
                    .git_service
                    .worktree_list(&context.execution_root)
                    .await
                {
                    Ok(worktrees) => worktrees,
                    Err(error) => {
                        tracing::warn!(%session_id, %error, "无法核对个人会话 Git worktree，保留目录");
                        return false;
                    }
                };
                for worktree in worktrees
                    .into_iter()
                    .filter(|tree| tree.path.starts_with(&managed_root))
                {
                    if let Err(error) = self
                        .git_service
                        .worktree_remove(
                            &context.execution_root,
                            magi_git::WorktreeRemoveOptions {
                                path: worktree.path,
                                force: false,
                                confirm_force: false,
                                precondition: magi_git::GitPrecondition::default(),
                            },
                        )
                        .await
                    {
                        tracing::warn!(%session_id, %error, "个人会话删除时保留无法安全回收的 worktree 与仓库目录");
                        return false;
                    }
                }
            }
            self.session_code_contexts.remove(session_id.as_str());
            if let Err(error) = self.persist_session_git_contexts() {
                tracing::warn!(%session_id, ?error, "持久化个人 Git 资源清理失败");
            }
            return true;
        }
        let mut cleanup_failed = false;
        for context in self.session_code_contexts.for_session(session_id.as_str()) {
            for agent in &context.agent_worktrees {
                if !agent.path.exists() {
                    continue;
                }
                if let Err(error) = self
                    .git_service
                    .worktree_remove(
                        &context.git.worktree_path,
                        magi_git::WorktreeRemoveOptions {
                            path: agent.path.clone(),
                            force: false,
                            confirm_force: false,
                            precondition: context.precondition(),
                        },
                    )
                    .await
                {
                    cleanup_failed = true;
                    tracing::warn!(
                        session_id = %session_id,
                        task_id = %agent.task_id,
                        worktree_path = %agent.path.display(),
                        %error,
                        "session 删除时保留无法安全回收的 agent worktree"
                    );
                }
            }
        }
        if !cleanup_failed {
            self.session_code_contexts.remove(session_id.as_str());
        }
        if let Err(error) = self.persist_session_git_contexts() {
            tracing::warn!(session_id = %session_id, ?error, "持久化 session Git 资源清理失败");
        }
        !cleanup_failed
    }
}
