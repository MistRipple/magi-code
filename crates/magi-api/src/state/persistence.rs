use super::*;

impl ApiState {
    pub fn runtime_persistence(&self) -> Option<&RuntimeStatePersistence> {
        self.runtime_persistence.as_deref()
    }

    pub fn with_runtime_persistence(mut self, persistence: Arc<RuntimeStatePersistence>) -> Self {
        if let Some(state_root) = persistence.state_root() {
            self.restore_session_isolations_from(&persistence);
            let path = state_root.join("session-git-contexts.json");
            match fs::read(&path) {
                Ok(bytes) => {
                    match serde_json::from_slice::<Vec<magi_git::SessionCodeContext>>(&bytes) {
                        Ok(contexts) => {
                            let released =
                                self.session_code_contexts.restore_after_restart(contexts);
                            if released > 0 {
                                tracing::info!(
                                    released,
                                    "已释放上次运行遗留的子代理 worktree 占用标记"
                                );
                            }
                        }
                        Err(error) => tracing::warn!(
                            path = %path.display(),
                            error = %error,
                            "忽略无法解析的 session Git context 持久化状态"
                        ),
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    "忽略无法读取的 session Git context 持久化状态"
                ),
            }
        }
        let browser_load = persistence
            .state_root()
            .map(load_browser_authority)
            .transpose();
        if let Some(state_root) = persistence.state_root() {
            self.plugin_jobs = Arc::new(crate::plugin_jobs::PluginJobs::open(
                state_root.join("plugin-jobs.json"),
            ));
            self.mcp_service = Arc::new(crate::mcp_runtime::McpServiceRuntime::load(
                state_root.join("mcp-server.json"),
            ));
            if let Err(error) = self
                .mcp_service
                .migrate_legacy_directory_bindings(&self.workspace_registry)
            {
                tracing::error!(%error, "MCP 旧令牌目录授权迁移失败，保留原令牌与文件；缺少持久目录授权的调用拒绝执行");
            }
        }
        self.runtime_persistence = Some(persistence);
        if let Some(persistence) = self.runtime_persistence.clone() {
            let store = self.knowledge_store.clone();
            store.set_index_persistence_callback(move |store| {
                persistence.schedule_knowledge_store_save(store.clone());
            });
        }
        match browser_load {
            Ok(Some((authority, is_new))) => {
                *self
                    .browser_authority
                    .lock()
                    .expect("browser authority lock poisoned") = authority;
                if is_new && let Err(error) = self.persist_browser_durable_state() {
                    self.browser_state_writable.store(false, Ordering::Release);
                    self.set_browser_host_status(BrowserHostStatusSnapshot {
                        status: BrowserHostStatus::Failed,
                        last_error_code: Some("browser_state_persist_failed".to_string()),
                        ..self.browser_host_status()
                    });
                    tracing::error!(?error, "初始化浏览器持久状态失败");
                }
            }
            Ok(None) => {}
            Err(error) => {
                self.browser_state_writable.store(false, Ordering::Release);
                self.set_browser_host_status(BrowserHostStatusSnapshot {
                    status: BrowserHostStatus::Failed,
                    protocol_compatible: false,
                    last_error_code: Some("browser_state_invalid".to_string()),
                    revision: 0,
                    ..BrowserHostStatusSnapshot::default()
                });
                tracing::error!(error = %error, "浏览器持久状态无效，禁止覆盖原文件");
            }
        }
        self
    }

    pub fn with_canonical_event_next_sequence_provider(
        mut self,
        provider: CanonicalEventNextSequenceProvider,
    ) -> Self {
        self.canonical_event_next_sequence_provider = Some(provider);
        self
    }

    pub fn with_session_state_checkpoint_persist(
        mut self,
        persist: SessionStateCheckpointPersist,
    ) -> Self {
        self.session_state_checkpoint_persist = Some(persist);
        self
    }

    pub fn with_session_projection_persist(mut self, persist: SessionProjectionPersist) -> Self {
        self.session_projection_persist = Some(persist);
        self
    }

    pub fn with_unavailable_session_discard(mut self, discard: UnavailableSessionDiscard) -> Self {
        self.unavailable_session_discard = Some(discard);
        self
    }

    pub fn with_workspace_history_restore(mut self, restore: WorkspaceHistoryRestore) -> Self {
        self.workspace_history_restore = Some(restore);
        self
    }

    pub(crate) fn restore_workspace_history_for_api(
        &self,
        project: &magi_workspace::WorkspaceRecord,
    ) -> Result<(), ApiError> {
        if let Some(restore) = &self.workspace_history_restore {
            restore(project)?;
        }
        Ok(())
    }

    pub fn persist_session_state_checkpoint(&self, checkpoint: &str) -> Result<(), ApiError> {
        let durable_persisted = if let Some(persist) = &self.session_state_checkpoint_persist {
            persist(checkpoint)?
        } else {
            false
        };
        if !durable_persisted {
            self.persist_session_projection()?;
        }
        self.persist_session_git_contexts()
    }

    pub fn persist_session_projection(&self) -> Result<(), ApiError> {
        let Some(persist) = &self.session_projection_persist else {
            return Ok(());
        };
        self.session_store
            .persist_projection_with(|durable, sidecars| {
                persist(durable, sidecars, SessionProjectionPersistMode::Full)
            })
    }

    /// 只提交指定会话的 projection（以及 current 指针等全局小文件）。
    ///
    /// `persist_session_projection` 每次都要重新序列化**全部**会话的历史来比对摘要，会话越多越慢
    /// （本仓库实测新建一个会话要 14 秒），而且是在会话存储的锁内执行，会让整个 daemon 卡住。
    /// 已知只有哪几个会话变了时，都应该用这个。
    pub fn persist_session_projection_for_sessions(
        &self,
        session_ids: &[SessionId],
    ) -> Result<(), ApiError> {
        let Some(persist) = &self.session_projection_persist else {
            return Ok(());
        };
        self.session_store
            .persist_sessions_projection_with(session_ids, |durable, sidecars| {
                persist(
                    durable,
                    sidecars,
                    SessionProjectionPersistMode::Sessions {
                        session_ids: session_ids.to_vec(),
                    },
                )
            })
    }

    pub fn persist_session_projection_for_sessions_for_api(
        &self,
        session_ids: &[SessionId],
    ) -> Result<(), ApiError> {
        self.persist_session_projection_for_sessions(session_ids)
            .map_err(|error| {
                public_runtime_persistence_error("session", SESSION_PERSISTENCE_PUBLIC_ERROR, error)
            })
    }

    /// 在这个通知上下文里可见的通知，分别属于哪些会话。
    ///
    /// 通知操作（已读 / 清除 / 删除 / 解决）只会改动「在该上下文可见」的通知；会话级通知存放在
    /// 所属会话的 projection 里，应用级和项目级通知存放在各自的 meta 文件里。所以操作前取一次
    /// 这个集合，就知道要重新提交哪些会话，其余会话不用动。
    pub(crate) fn sessions_with_notifications_in_context(
        &self,
        context: &magi_session_store::NotificationContext,
    ) -> Vec<SessionId> {
        let mut session_ids = Vec::new();
        for notification in self.session_store.notifications_for_context(context) {
            if let Some(session_id) = notification.session_id
                && !session_ids.contains(&session_id)
            {
                session_ids.push(session_id);
            }
        }
        session_ids
    }

    pub(crate) fn rename_session_with_persistence_for_api(
        &self,
        session_id: &SessionId,
        title: impl Into<String>,
    ) -> Result<SessionRecord, ApiError> {
        let persist = self.session_projection_persist.clone();
        self.session_store
            .rename_session_with_persistence(session_id, title, |durable, sidecars| {
                if let Some(persist) = &persist {
                    persist(
                        durable,
                        sidecars,
                        SessionProjectionPersistMode::Sessions {
                            session_ids: vec![session_id.clone()],
                        },
                    )
                } else {
                    Ok(())
                }
            })
            .map_err(|error| match error {
                magi_session_store::SessionMutationTransactionError::Domain(
                    magi_core::DomainError::Validation { message },
                ) => ApiError::InvalidInput(message),
                magi_session_store::SessionMutationTransactionError::Domain(error) => {
                    ApiError::internal_assembly("重命名会话失败", error)
                }
                magi_session_store::SessionMutationTransactionError::Persistence(error) => error,
            })
    }

    /// 在 WorkspaceStore 的管理事务中调用，快照由持有写锁的原拥有方提供，
    /// 不重新读取 registry，也不先发布尚未落盘的内存事实。
    pub(crate) fn persist_workspace_snapshot(
        &self,
        snapshot: &magi_workspace::WorkspaceDurableState,
    ) -> magi_core::DomainResult<()> {
        let Some(persistence) = &self.runtime_persistence else {
            return Ok(());
        };
        persistence
            .save_json(&persistence.workspace_path, snapshot)
            .map_err(|error| magi_core::DomainError::Persistence {
                message: format!("{error:?}"),
            })
    }

    pub fn persist_workspace_durable_state(&self) -> Result<(), ApiError> {
        let Some(persistence) = &self.runtime_persistence else {
            return Ok(());
        };
        persistence.save_workspace_store(&self.workspace_registry)
    }

    pub fn persist_workspace_durable_state_for_api(&self) -> Result<(), ApiError> {
        self.persist_workspace_durable_state().map_err(|error| {
            public_runtime_persistence_error("workspace", WORKSPACE_PERSISTENCE_PUBLIC_ERROR, error)
        })
    }

    pub fn persist_knowledge_state(&self) -> Result<(), ApiError> {
        let Some(persistence) = &self.runtime_persistence else {
            return Ok(());
        };
        persistence.save_knowledge_store(&self.knowledge_store)
    }

    pub(crate) fn schedule_workspace_code_index(
        &self,
        workspace_id: WorkspaceId,
        workspace_root: PathBuf,
    ) -> bool {
        let Some(project) = self.workspace_registry.project(&workspace_id) else {
            return false;
        };
        let root = canonicalize_path_for_workspace_match(&workspace_root);
        let source = self
            .session_isolations
            .all()
            .into_iter()
            .find(|isolation| {
                isolation.workspace_id == workspace_id.as_str()
                    && canonicalize_path_for_workspace_match(&isolation.root) == root
            })
            .map(|isolation| isolation.source_root)
            .unwrap_or_else(|| root.clone());
        let Some(folder) = project
            .source_folders
            .iter()
            .find(|folder| canonicalize_path_for_workspace_match(&folder.native_path()) == source)
        else {
            // 不能将无法归属的副本索引写入项目的旧单根槽位。
            return false;
        };
        let isolated = source != root;
        let index_store = if project.source_folders.len() == 1 && !isolated {
            self.knowledge_store.as_ref().clone()
        } else {
            let Ok(index_store) = self
                .knowledge_store
                .for_source_folder(&folder.source_folder_id, &root)
            else {
                return false;
            };
            index_store
        };
        self.schedule_code_index(workspace_id, root, index_store)
    }

    pub(crate) fn schedule_code_index(
        &self,
        workspace_id: WorkspaceId,
        workspace_root: PathBuf,
        index_store: KnowledgeStore,
    ) -> bool {
        if !index_store.begin_workspace_index_build(&workspace_id) {
            return false;
        }
        let state = self.clone();
        tokio::spawn(async move {
            let build_store = index_store.clone();
            let build_workspace_id = workspace_id.clone();
            let result = tokio::task::spawn_blocking(move || {
                build_store.build_workspace_index(&build_workspace_id, &workspace_root)
            })
            .await;
            if let Err(error) = result {
                tracing::warn!(workspace_id = %workspace_id, scope = ?index_store.code_index_scope(), error = %error, "后台代码索引构建失败");
            }
            if index_store.finish_workspace_index_build(&workspace_id)
                && let Err(error) = state.persist_knowledge_state()
            {
                tracing::warn!(workspace_id = %workspace_id, error = ?error, "已取消的代码索引清理结果持久化失败");
            }
        });
        true
    }

    pub fn persist_knowledge_state_for_api(&self) -> Result<(), ApiError> {
        self.persist_knowledge_state().map_err(|error| {
            public_runtime_persistence_error("knowledge", KNOWLEDGE_PERSISTENCE_PUBLIC_ERROR, error)
        })
    }

    /// 运行态整体持久化：先 workspace 注册事实，再引用它的 session projection（只提交指定会话），
    /// 最后 knowledge。保持 workspace 先于引用它的 session projection 落盘。
    pub fn persist_runtime_durable_state_for_sessions_for_api(
        &self,
        session_ids: &[SessionId],
    ) -> Result<(), ApiError> {
        self.persist_workspace_durable_state_for_api()?;
        self.persist_session_projection_for_sessions_for_api(session_ids)?;
        self.persist_knowledge_state_for_api()?;
        Ok(())
    }

    /// 会话导航只改变当前会话指针和工作区选择。
    ///
    /// 导航处于全局串行锁内，不能把无关的 knowledge 快照同步写盘，否则
    /// 本地知识库增长后，普通切换会话会被拖成一次长耗时请求。知识状态有
    /// 独立的生命周期和持久化入口，不属于导航提交事务。
    pub fn persist_session_navigation_state_for_api(
        &self,
        target_session_id: Option<SessionId>,
    ) -> Result<(), ApiError> {
        let Some(persist) = &self.session_projection_persist else {
            self.persist_workspace_durable_state_for_api()?;
            return Ok(());
        };
        // 导航只需要当前指针与目标会话的切片，不能走会整体克隆全部会话历史的通用入口。
        self.session_store
            .persist_navigation_projection_with(target_session_id.as_ref(), |durable, sidecars| {
                persist(
                    durable,
                    sidecars,
                    SessionProjectionPersistMode::Navigation {
                        target_session_id: target_session_id.clone(),
                    },
                )
            })
            .map_err(|error| {
                public_runtime_persistence_error("session", SESSION_PERSISTENCE_PUBLIC_ERROR, error)
            })?;
        self.persist_workspace_durable_state_for_api()?;
        Ok(())
    }
}
