use super::*;

impl ApiState {
    /// Browser Host 连接断开时的窄域清理。连接断开只代表浏览器自动化暂时
    /// 不可用，不能把它升级为整个会话执行被取消。
    pub fn revoke_browser_execution_resources(
        &self,
        session_id: Option<&SessionId>,
        workspace_id: Option<&WorkspaceId>,
        task_id: Option<&TaskId>,
        reason: BrowserLeaseEndReason,
    ) -> usize {
        self.execution_resources.revoke_browser_leases(
            &ToolExecutionContextQuery {
                session_id: session_id.cloned(),
                workspace_id: workspace_id.cloned(),
                task_id: task_id.cloned(),
                worker_id: None,
                taskless_only: false,
            },
            reason,
            UtcMillis::now(),
        )
    }

    pub fn cancel_browser_surface_control(
        &self,
        tab_id: &BrowserTabId,
        surface_id: &str,
        reason: BrowserLeaseEndReason,
    ) -> Vec<BrowserControlLease> {
        self.execution_resources
            .cancel_surface(tab_id, surface_id, reason, UtcMillis::now())
    }

    pub fn browser_host_status(&self) -> BrowserHostStatusSnapshot {
        self.browser_host_status
            .read()
            .expect("browser host status lock poisoned")
            .clone()
    }

    /// 返回 BrowserAuthority 当前记录的唯一主 Surface。
    ///
    /// Surface 属于桌面运行态，不进入持久化状态；所有 API 和 Host 事件
    /// 都必须从 Authority 读取，不能在各层维护自己的 Surface 缓存。
    pub fn browser_primary_surface_id(&self, tab_id: &BrowserTabId) -> Option<String> {
        self.browser_authority
            .lock()
            .expect("browser authority lock poisoned")
            .primary_surface(tab_id)
            .map(|surface| surface.surface_id.clone())
    }

    pub fn browser_host_connection_config(&self) -> Option<BrowserHostConnectionConfig> {
        self.browser_host_connection.borrow().clone()
    }

    pub fn set_browser_host_connection_config(&self, config: Option<BrowserHostConnectionConfig>) {
        if let Some(config) = config.as_ref() {
            let candidate_owner = BrowserHostOwner::from(config);
            let mut owner = self
                .browser_host_owner
                .lock()
                .expect("browser Host owner lock poisoned");
            match owner.as_ref() {
                Some(current) => assert_eq!(
                    current, &candidate_owner,
                    "同一 daemon 生命周期不能切换 Electron Desktop 所有者"
                ),
                None => *owner = Some(candidate_owner),
            }
            self.browser_host_connection_generation
                .fetch_max(config.generation, Ordering::AcqRel);
        } else {
            // 清除连接同时释放 Desktop 所有权。开发 daemon 会被多个 Electron
            // 生命周期复用；如果正常退出只清了 config 却保留 owner，下一次
            // Desktop 永远会收到 409，浏览器能力会永久停在 stopped。
            self.browser_host_owner
                .lock()
                .expect("browser Host owner lock poisoned")
                .take();
        }
        self.browser_host_connection.send_replace(config);
    }

    pub fn browser_host_connection_receiver(
        &self,
    ) -> watch::Receiver<Option<BrowserHostConnectionConfig>> {
        self.browser_host_connection.subscribe()
    }

    pub fn browser_host_connection_generation(&self) -> u64 {
        self.browser_host_connection_generation
            .load(Ordering::Acquire)
    }

    pub fn register_browser_host_connection(
        &self,
        candidate: BrowserHostConnectionConfig,
        expected_generation: u64,
    ) -> Result<(BrowserHostConnectionConfig, bool), BrowserHostConnectionRegistrationError> {
        let candidate_owner = BrowserHostOwner::from(&candidate);
        let mut owner = self
            .browser_host_owner
            .lock()
            .expect("browser Host owner lock poisoned");
        if owner
            .as_ref()
            .is_some_and(|current| current != &candidate_owner)
        {
            return Err(BrowserHostConnectionRegistrationError::OwnerConflict {
                current_generation: self.browser_host_connection_generation(),
            });
        }
        let mut outcome = Ok((candidate.clone(), false));
        self.browser_host_connection.send_if_modified(|current| {
            let next_generation = || {
                self.browser_host_connection_generation
                    .fetch_add(1, Ordering::AcqRel)
                    .saturating_add(1)
            };
            match current {
                Some(existing)
                    if existing.desktop_epoch == candidate.desktop_epoch
                        && existing.parent_pid == candidate.parent_pid
                        && existing.socket_path == candidate.socket_path
                        && existing.auth_token == candidate.auth_token
                        && expected_generation == existing.generation
                        && existing.generation != 0 =>
                {
                    outcome = Ok((existing.clone(), false));
                    false
                }
                Some(existing) if expected_generation == existing.generation => {
                    let mut accepted = candidate.clone();
                    accepted.generation = next_generation();
                    outcome = Ok((accepted.clone(), true));
                    *current = Some(accepted);
                    true
                }
                Some(existing) => {
                    outcome = Err(BrowserHostConnectionRegistrationError::Conflict {
                        current_generation: existing.generation,
                    });
                    false
                }
                None if expected_generation == self.browser_host_connection_generation() => {
                    let mut accepted = candidate.clone();
                    accepted.generation = next_generation();
                    outcome = Ok((accepted.clone(), true));
                    *current = Some(accepted);
                    true
                }
                None => {
                    outcome = Err(BrowserHostConnectionRegistrationError::Conflict {
                        current_generation: self.browser_host_connection_generation(),
                    });
                    false
                }
            }
        });
        if outcome.is_ok() && owner.is_none() {
            *owner = Some(candidate_owner);
        }
        outcome
    }

    pub fn set_browser_host_status(&self, mut status: BrowserHostStatusSnapshot) {
        let mut current = self
            .browser_host_status
            .write()
            .expect("browser host status lock poisoned");
        status.revision = current.revision.saturating_add(1);
        status.settings = current.settings;
        *current = status;
    }

    pub fn update_browser_capability_settings(
        &self,
        settings: BrowserCapabilitySettings,
    ) -> Result<(), ApiError> {
        self.settings_store
            .set_section("browser", settings.to_section())
            .map_err(crate::errors::settings_persistence_error)?;
        let mut current = self
            .browser_host_status
            .write()
            .expect("browser host status lock poisoned");
        current.settings = settings;
        current.revision = current.revision.saturating_add(1);
        Ok(())
    }

    pub fn set_browser_host_client(&self, client: Option<BrowserHostClient>) -> u64 {
        let mut current = self
            .browser_host_client
            .write()
            .expect("browser Host client lock poisoned");
        *current = client;
        self.browser_host_generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn browser_host_client(&self) -> Option<BrowserHostClient> {
        self.browser_host_client
            .read()
            .expect("browser Host client lock poisoned")
            .clone()
    }

    /// 清理孤儿 Browser Session。
    ///
    /// 异常退出等情况下，Browser Authority 的持久化状态可能早于对应 Magi 会话被删除或归档，
    /// 这些状态不能继续参与恢复和全局 Tab 容量计算。只保留当前 SessionStore
    /// 中仍为 Active 的 Magi 会话，所有其他打开状态一次性转为 Closed。
    pub fn reconcile_browser_sessions_with_session_store(&self) -> Result<usize, ApiError> {
        let active_session_ids = self
            .session_store
            .sessions()
            .into_iter()
            .filter(|session| session.status == SessionLifecycleStatus::Active)
            .map(|session| session.session_id)
            .collect::<HashSet<_>>();
        let orphan_browser_session_ids = self
            .browser_authority
            .lock()
            .expect("browser authority lock poisoned")
            .snapshot()
            .sessions
            .into_iter()
            // App 级会话不属于任何 Magi 会话，只按 owner 的 Session 分支参与孤儿判定
            // （设计基线 §5.3）。
            .filter(|session| {
                session.lifecycle.is_open()
                    && session
                        .magi_session_id()
                        .is_some_and(|session_id| !active_session_ids.contains(session_id))
            })
            .map(|session| session.browser_session_id)
            .collect::<Vec<_>>();
        if orphan_browser_session_ids.is_empty() {
            return Ok(0);
        }

        let now = UtcMillis::now();
        self.mutate_browser_authority(|authority| {
            for browser_session_id in &orphan_browser_session_ids {
                authority.transition_session(
                    browser_session_id,
                    BrowserSessionLifecycle::Closed,
                    now,
                )?;
            }
            Ok(orphan_browser_session_ids.len())
        })
    }

    pub fn browser_host_generation(&self) -> u64 {
        self.browser_host_generation.load(Ordering::Acquire)
    }

    pub fn browser_downloads(&self) -> &Arc<crate::browser_downloads::BrowserDownloadRegistry> {
        &self.browser_downloads
    }

    pub fn browser_tool_runtime_dependencies(&self) -> crate::BrowserToolRuntimeDependencies {
        crate::BrowserToolRuntimeDependencies {
            authority: Arc::clone(&self.browser_authority),
            write_lock: Arc::clone(&self.browser_write_lock),
            control_locks: self.browser_control_locks.clone(),
            downloads: Arc::clone(&self.browser_downloads),
            state_writable: Arc::clone(&self.browser_state_writable),
            host_status: Arc::clone(&self.browser_host_status),
            host_client: Arc::clone(&self.browser_host_client),
            event_bus: Arc::clone(&self.event_bus),
            session_store: Arc::clone(&self.session_store),
            persistence: self.runtime_persistence.clone(),
        }
    }

    pub fn browser_capability_snapshot(&self) -> BrowserCapabilitySnapshot {
        let host = self.browser_host_status();
        BrowserCapabilitySnapshot {
            revision: host.revision,
            in_app_browser_enabled: host.settings.in_app_browser_enabled,
            browser_use_enabled: host.settings.browser_use_enabled,
            devtools_enabled: host.settings.devtools_enabled,
            lan_access_enabled: host.settings.lan_access_enabled,
            host_status: host.status,
            host_protocol_compatible: host.protocol_compatible,
        }
    }

    pub fn persist_browser_durable_state(&self) -> Result<(), ApiError> {
        if !self.browser_state_writable.load(Ordering::Acquire) {
            return Err(ApiError::InternalAssemblyError(
                "浏览器持久状态不可写，原状态文件需要修复".to_string(),
            ));
        }
        let Some(persistence) = &self.runtime_persistence else {
            return Ok(());
        };
        let Some(state_root) = persistence.state_root() else {
            return Ok(());
        };
        let durable = browser_durable_state_for_persistence(
            &self
                .browser_authority
                .lock()
                .expect("browser authority lock poisoned"),
        );
        persistence.save_json(&state_root.join("browser/state.json"), &durable)
    }

    /// 会话删除时一并删除它产生的浏览器截图等 artifact。
    pub(super) fn remove_browser_artifacts_for_session(&self, session_id: &SessionId) {
        let Some(state_root) = self
            .runtime_persistence
            .as_ref()
            .and_then(|persistence| persistence.state_root())
        else {
            return;
        };
        let directory = state_root
            .join(crate::browser_tool_runtime::BROWSER_ARTIFACT_DIR)
            .join(session_id.as_str());
        match std::fs::remove_dir_all(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                %session_id,
                path = %directory.display(),
                %error,
                "删除会话浏览器 artifact 失败"
            ),
        }
    }

    pub fn persist_browser_durable_state_for_api(&self) -> Result<(), ApiError> {
        self.persist_browser_durable_state().map_err(|error| {
            public_runtime_persistence_error("browser", BROWSER_PERSISTENCE_PUBLIC_ERROR, error)
        })
    }

    pub fn mutate_browser_authority<T>(
        &self,
        mutation: impl FnOnce(&mut BrowserAuthority) -> Result<T, BrowserAuthorityError>,
    ) -> Result<T, ApiError> {
        commit_browser_authority_mutation(
            &self.browser_authority,
            &self.browser_write_lock,
            &self.browser_state_writable,
            self.runtime_persistence.as_deref(),
            mutation,
        )
        .map_err(|error| match error {
            BrowserAuthorityCommitError::NotWritable => {
                ApiError::Conflict("浏览器状态文件无效，修复前不能修改浏览器状态".to_string())
            }
            BrowserAuthorityCommitError::Rejected(error) => browser_authority_api_error(error),
            BrowserAuthorityCommitError::Persist(error) => error,
        })
    }
}
