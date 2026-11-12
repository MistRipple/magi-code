use super::*;

impl ApiState {
    /// 安装 SessionLifecycleObserver，把 session 创建/归档/删除事件桥接到 SnapshotManager。
    ///
    /// 历史 session 不在启动阶段批量建立快照。快照的整树初始化属于变更视图的
    /// 按需资源，启动时批量恢复会把所有 workspace 扫描排进同一条生命周期队列，
    /// 既浪费 IO，也会让用户第一次导航等待与当前会话无关的历史任务。实际访问
    /// 变更视图或执行需要快照的操作时，由 ensure_snapshot_session 懒加载。
    pub fn install_snapshot_lifecycle_observer(&self) {
        let observer = Arc::new(crate::snapshot_lifecycle::SnapshotLifecycleObserver::new(
            self.snapshot_manager.clone(),
        ));
        self.session_store.set_lifecycle_observer(observer);
    }

    /// 同步取 session + workspace 对应的 SnapshotSession。按需资源尚未装载时返回 None；
    /// 需要变更账本的操作必须先经 ensure 入口准备。
    pub fn snapshot_session(
        &self,
        session_id: &SessionId,
        workspace_root: &Path,
    ) -> Option<Arc<SnapshotSession>> {
        let workspace_root = self.snapshot_root_for_session(session_id, workspace_root);
        self.snapshot_manager
            .get_session_for_workspace(session_id.as_str(), &workspace_root)
    }

    /// 隔离会话的变更账本建立在隔离副本上；调用方按习惯传入主工作区路径时在这里换成副本路径，
    /// 这样变更面板、MCP、Git 面板等所有按「会话 + 工作区」取账本的入口都自然指向副本。
    pub(crate) fn snapshot_root_for_session(
        &self,
        session_id: &SessionId,
        workspace_root: &Path,
    ) -> PathBuf {
        let Some(isolation) = self
            .session_isolations
            .get_for_source(session_id.as_str(), workspace_root)
        else {
            return workspace_root.to_path_buf();
        };
        let canonical = std::fs::canonicalize(workspace_root).ok();
        if workspace_root == isolation.source_root
            || canonical.as_deref() == Some(isolation.source_root.as_path())
        {
            isolation.root
        } else {
            workspace_root.to_path_buf()
        }
    }

    pub(crate) async fn ensure_snapshot_session(
        &self,
        session_id: &SessionId,
        workspace_root: &Path,
    ) -> Result<Arc<SnapshotSession>, ApiError> {
        let _ = self.session_execution_root_for_source(session_id, workspace_root)?;
        if let Some(session) = self.snapshot_session(session_id, workspace_root) {
            return Ok(session);
        }
        let workspace_root = self.snapshot_root_for_session(session_id, workspace_root);
        self.snapshot_manager
            .start_session(session_id.as_str().to_string(), workspace_root)
            .await
            .map_err(|error| ApiError::internal_assembly("启动会话快照账本失败", error))
    }

    /// 同步 session 变更面板依赖的磁盘状态与 Git baseline。
    ///
    /// `force_reconcile` 用于页面首次打开、窗口重新聚焦及执行变更操作前的强一致读取；
    /// 常规轮询依赖 watcher 增量，只做轻量 Git ref 指纹检查。同分支快进只应用
    /// old HEAD→new HEAD 的 tree patch，绝不把整个 dirty worktree 直接提升为 baseline。
    pub(crate) async fn synchronize_session_changes(
        &self,
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
        workspace_root: &Path,
        force_reconcile: bool,
    ) -> Result<Arc<SnapshotSession>, ApiError> {
        let _sync_guard = self.lock_session_change_sync(session_id).await;
        let snapshot = self
            .ensure_snapshot_session(session_id, workspace_root)
            .await?;
        tracing::info!(%session_id, stage = "changes_snapshot_ready", "workspace execution preparation");
        if force_reconcile {
            snapshot
                .reconcile_async()
                .await
                .map_err(|error| ApiError::internal_assembly("刷新磁盘变更状态失败", error))?;
        }
        tracing::info!(%session_id, stage = "changes_reconciled", "workspace execution preparation");
        // 隔离副本有自己独立的 Git 状态，不与主工作区的 branch/HEAD 对齐。
        if self
            .session_isolations
            .get_for_root(session_id.as_str(), workspace_root)
            .is_some()
        {
            return Ok(snapshot);
        }

        let Some(existing_context) = self
            .session_code_contexts
            .get_for_path(session_id.as_str(), workspace_root)
        else {
            match self.git_service.observe(workspace_root).await {
                Ok(observation) => {
                    tracing::info!(%session_id, stage = "changes_git_observed", "workspace execution preparation");
                    self.align_snapshot_baseline_to_observation_head(
                        workspace_root,
                        &observation,
                        &snapshot,
                    )
                    .await?;
                    tracing::info!(%session_id, stage = "changes_baseline_aligned", "workspace execution preparation");
                    self.session_code_contexts.accept(
                        session_id.as_str(),
                        workspace_id.as_str(),
                        vec![workspace_root.to_path_buf()],
                        &observation,
                    );
                    self.persist_session_git_contexts()?;
                }
                Err(magi_git::GitError::NotRepository { .. }) => {}
                Err(error) => {
                    return Err(ApiError::internal_assembly(
                        "初始化变更面板 Git context 失败",
                        error,
                    ));
                }
            }
            return Ok(snapshot);
        };

        let ref_observation = match self.git_service.observe_ref(workspace_root).await {
            Ok(observation) => observation,
            Err(magi_git::GitError::NotRepository { .. }) => return Ok(snapshot),
            Err(error) => {
                return Err(ApiError::internal_assembly(
                    "刷新变更面板 Git ref 失败",
                    error,
                ));
            }
        };
        let observed_ref_changed = existing_context.git.observed_branch != ref_observation.branch
            || existing_context.git.observed_head != ref_observation.head;
        if !existing_context.has_external_drift() && !observed_ref_changed {
            return Ok(snapshot);
        }

        let observation = self
            .git_service
            .observe(workspace_root)
            .await
            .map_err(|error| ApiError::internal_assembly("刷新变更面板 Git context 失败", error))?;
        let observed_context = self.session_code_contexts.observe(
            session_id.as_str(),
            workspace_id.as_str(),
            existing_context.runtime_workspace_roots.clone(),
            &observation,
        );
        if !observed_context.has_external_drift() {
            self.persist_session_git_contexts()?;
            return Ok(snapshot);
        }

        let safe_fast_forward = self
            .git_service
            .is_session_context_fast_forward(&existing_context, &observation)
            .await
            .map_err(|error| {
                ApiError::internal_assembly("判断变更面板 Git context 是否安全快进失败", error)
            })?;
        if !safe_fast_forward
            || self
                .workspace_git_coordinator
                .session_holds_execution(session_id.as_str(), &existing_context.git.git_common_dir)
        {
            self.persist_session_git_contexts()?;
            if observed_ref_changed {
                self.publish_session_git_drift(
                    session_id,
                    workspace_id,
                    &observed_context,
                    existing_context.git.observed_head.as_deref(),
                    if safe_fast_forward {
                        "fast_forward_pending"
                    } else {
                        "external_drift_detected"
                    },
                );
            }
            return Ok(snapshot);
        }

        self.advance_snapshot_baseline_for_fast_forward(
            session_id,
            workspace_root,
            &existing_context,
            &observation,
            &snapshot,
        )
        .await?;
        let context = self.session_code_contexts.accept(
            session_id.as_str(),
            workspace_id.as_str(),
            existing_context.runtime_workspace_roots.clone(),
            &observation,
        );
        self.persist_session_git_contexts()?;
        if let Some(previous_head) = existing_context.git.base_head.as_deref() {
            self.publish_session_git_fast_forward(
                session_id,
                workspace_id,
                &context,
                previous_head,
            );
        }
        self.schedule_workspace_code_index(workspace_id.clone(), workspace_root.to_path_buf());
        Ok(snapshot)
    }

    pub(super) async fn advance_snapshot_baseline_for_fast_forward(
        &self,
        session_id: &SessionId,
        workspace_root: &Path,
        existing_context: &magi_git::SessionCodeContext,
        observation: &magi_git::GitObservation,
        snapshot: &Arc<SnapshotSession>,
    ) -> Result<(), ApiError> {
        let previous_head = existing_context.git.base_head.as_deref().ok_or_else(|| {
            ApiError::InternalAssemblyError("原 Git baseline HEAD 缺失".to_string())
        })?;
        let current_head = observation.head.as_deref().ok_or_else(|| {
            ApiError::InternalAssemblyError("新 Git baseline HEAD 缺失".to_string())
        })?;
        snapshot
            .reconcile_async()
            .await
            .map_err(|error| ApiError::internal_assembly("Git 快进前刷新磁盘状态失败", error))?;
        let git_patch = self
            .git_service
            .tree_baseline_patch(workspace_root, previous_head, current_head)
            .await
            .map_err(|error| {
                ApiError::internal_assembly("读取 Git 快进 baseline 补丁失败", error)
            })?;
        let snapshot_patch = snapshot_baseline_patch(git_patch);
        snapshot
            .apply_baseline_patch(&snapshot_patch)
            .map_err(|error| {
                tracing::error!(session_id = %session_id, ?error, "推进 session Git baseline 失败");
                ApiError::internal_assembly("推进 session Git baseline 失败", error)
            })?;
        Ok(())
    }

    pub(super) async fn align_snapshot_baseline_to_observation_head(
        &self,
        workspace_root: &Path,
        observation: &magi_git::GitObservation,
        snapshot: &Arc<SnapshotSession>,
    ) -> Result<(), ApiError> {
        let Some(head) = observation.head.as_deref() else {
            return Ok(());
        };
        snapshot.reconcile_async().await.map_err(|error| {
            ApiError::internal_assembly("Git baseline 校准前刷新磁盘失败", error)
        })?;
        let mut paths = snapshot
            .pending_changes()
            .map_err(|error| ApiError::internal_assembly("读取待校准变更失败", error))?
            .into_iter()
            .flat_map(|change| {
                change
                    .old_path
                    .into_iter()
                    .chain(std::iter::once(change.path))
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        if paths.is_empty() {
            return Ok(());
        }
        let git_entries = self
            .git_service
            .tree_baseline_entries_for_paths(workspace_root, head, &paths)
            .await
            .map_err(|error| ApiError::internal_assembly("读取 Git HEAD baseline 失败", error))?;
        snapshot
            .apply_baseline_patch(&snapshot_baseline_patch(git_entries))
            .map_err(|error| {
                ApiError::internal_assembly("校准 snapshot Git baseline 失败", error)
            })?;
        Ok(())
    }

    pub fn with_snapshot_manager(mut self, snapshot_manager: Arc<SnapshotManager>) -> Self {
        self.snapshot_manager = snapshot_manager;
        self
    }
}
