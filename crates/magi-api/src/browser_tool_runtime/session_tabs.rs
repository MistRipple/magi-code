use super::*;

impl BrowserToolRuntimeDependencies {
    pub(super) fn ensure_session(
        &self,
        session_id: &SessionId,
        workspace_id: Option<&WorkspaceId>,
    ) -> Result<magi_browser_authority::BrowserSession, BrowserToolError> {
        let current = self
            .authority
            .lock()
            .expect("browser authority lock poisoned")
            .session_for_magi_session(session_id)
            .cloned();
        if let Some(session) = current {
            if session.owner_workspace_id() != workspace_id {
                return Err(BrowserToolError::new(
                    "browser_workspace_scope_mismatch",
                    "浏览器会话与当前工作区不匹配",
                ));
            }
            return Ok(session);
        }

        let browser_session_id =
            BrowserSessionId::new(format!("browser-tool-session-{session_id}"));
        let session = self.mutate(|authority| {
            authority.create_session(CreateBrowserSession {
                browser_session_id: browser_session_id.clone(),
                owner: magi_browser_authority::BrowserSessionOwner::Session {
                    session_id: session_id.clone(),
                    workspace_id: workspace_id.cloned(),
                },
                profile_id: BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID),
                now: UtcMillis::now(),
            })?;
            authority.transition_session(
                &browser_session_id,
                magi_browser_authority::BrowserSessionLifecycle::Ready,
                UtcMillis::now(),
            )
        })?;
        self.publish_browser_event(
            "browser.session.created",
            &session,
            json!({
                "browser_session_id": session.browser_session_id,
                "lifecycle": session.lifecycle,
                "revision": session.revision,
            }),
        );
        Ok(session)
    }

    pub(super) async fn ensure_tab(
        &self,
        session: &magi_browser_authority::BrowserSession,
        arguments: &Map<String, Value>,
        client: &BrowserHostClient,
        executor: &str,
        context: &magi_tool_runtime::ToolExecutionContext,
    ) -> Result<magi_browser_authority::BrowserTab, BrowserToolError> {
        let requested_tab_id = optional_string(arguments, "tab_id").map(BrowserTabId::new);
        let initial_url = "about:blank".to_string();
        validate_browser_navigation_url(&initial_url).map_err(|error| {
            BrowserToolError::new(
                "browser_navigation_url_rejected",
                format!("浏览器导航 URL 不合法: {error}"),
            )
        })?;
        let tab = {
            let authority = self
                .authority
                .lock()
                .expect("browser authority lock poisoned");
            if let Some(id) = requested_tab_id.as_ref() {
                let tab = authority
                    .tab(id)
                    .ok_or_else(|| browser_tab_not_found_error(&authority, session, id))?;
                if tab.browser_session_id != session.browser_session_id {
                    return Err(BrowserToolError::new(
                        "browser_tab_scope_mismatch",
                        "指定的浏览器 Tab 不属于当前浏览器会话",
                    ));
                }
                if !matches!(
                    tab.lifecycle,
                    magi_browser_authority::BrowserTabLifecycle::Ready
                        | magi_browser_authority::BrowserTabLifecycle::Suspended
                        | magi_browser_authority::BrowserTabLifecycle::Crashed
                ) {
                    return Err(BrowserToolError::new(
                        "browser_tab_not_ready",
                        "指定的浏览器 Tab 当前不可用",
                    ));
                }
                Some(tab.clone())
            } else {
                let usable = |tab: &magi_browser_authority::BrowserTab| {
                    matches!(
                        tab.lifecycle,
                        magi_browser_authority::BrowserTabLifecycle::Ready
                            | magi_browser_authority::BrowserTabLifecycle::Suspended
                            | magi_browser_authority::BrowserTabLifecycle::Crashed
                    )
                };
                match authority
                    .agent_tab(&session.browser_session_id, executor)
                    .cloned()
                {
                    // 执行者自己的 Tab 是唯一默认目标。
                    Some(AgentTabTarget::Tab(id)) => {
                        authority.tab(&id).filter(|tab| usable(tab)).cloned()
                    }
                    // 执行者使用的 Tab 被关闭：明确告知，绝不悄悄换到用户的其他 Tab。
                    Some(AgentTabTarget::Closed(_)) => {
                        drop(authority);
                        self.mutate(|authority| {
                            authority.set_agent_tab(
                                &session.browser_session_id,
                                executor,
                                AgentTabTarget::NewTabRequired,
                            );
                            Ok(())
                        })?;
                        return Err(BrowserToolError::new(
                            "browser_agent_tab_closed",
                            "你之前使用的浏览器标签页已被用户关闭。不要操作用户的其他标签页；如仍需浏览器，再次调用浏览器工具会为你新开一个标签页。",
                        ));
                    }
                    Some(AgentTabTarget::NewTabRequired) => None,
                    // 首次使用：沿用用户当前选中的 Tab；若它正被其他代理控制，则为自己新开 Tab。
                    None => authority
                        .active_tab(&session.browser_session_id)
                        .into_iter()
                        .chain(session.tab_ids.iter())
                        .find_map(|id| authority.tab(id).cloned())
                        .filter(|tab| usable(tab))
                        .filter(|tab| {
                            !authority.tab_controlled_by_other_executor(
                                &tab.tab_id,
                                context.task_id.as_ref(),
                                UtcMillis::now(),
                            )
                        }),
                }
            }
        };
        if let Some(tab) = tab {
            self.mutate(|authority| {
                authority.set_agent_tab(
                    &session.browser_session_id,
                    executor,
                    AgentTabTarget::Tab(tab.tab_id.clone()),
                );
                authority.set_active_tab(&session.browser_session_id, &tab.tab_id)
            })?;
            return self.materialize_tab(tab, client).await;
        }
        let tab_id = BrowserTabId::new(format!(
            "browser-tool-tab-{}-{}",
            UtcMillis::now().0,
            BROWSER_TAB_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let created = self.mutate(|authority| {
            let created = authority.create_tab(CreateBrowserTab {
                tab_id: tab_id.clone(),
                browser_session_id: session.browser_session_id.clone(),
                url: initial_url.clone(),
                now: UtcMillis::now(),
            })?;
            authority.set_active_tab(&session.browser_session_id, &tab_id)?;
            authority.set_agent_tab(
                &session.browser_session_id,
                executor,
                AgentTabTarget::Tab(tab_id.clone()),
            );
            authority.record_agent_created_tab(executor, tab_id.clone());
            Ok(created)
        })?;
        self.publish_tab_event("browser.tab.created", &created);
        let reply = match client
            .request(BrowserHostCommand::CreatePage {
                tab_id: tab_id.clone(),
                browser_session_id: session.browser_session_id.clone(),
                initial_url,
                logical_viewport: magi_browser_authority::BrowserLogicalViewport::Fixed {
                    width: 1280,
                    height: 800,
                    device_scale_factor_millis: 1000,
                    device_type: BrowserDeviceType::Desktop,
                },
                navigation_revision: 0,
                snapshot_revision: 0,
                allow_page_eviction: false,
            })
            .await
        {
            Ok(reply) => reply,
            Err(error) => {
                self.mark_tab_crashed(&tab_id);
                return Err(BrowserToolError::new(
                    "browser_host_disconnected",
                    error.to_string(),
                ));
            }
        };
        let page = match page_state(reply.response.outcome, "创建浏览器页面失败") {
            Ok(page) => page,
            Err(error) => {
                self.mark_tab_crashed(&tab_id);
                return Err(error);
            }
        };
        let tab = self.apply_page_state(&tab_id, page)?;
        self.publish_tab_event("browser.tab.created", &tab);
        Ok(tab)
    }

    pub(super) async fn materialize_tab(
        &self,
        tab: magi_browser_authority::BrowserTab,
        client: &BrowserHostClient,
    ) -> Result<magi_browser_authority::BrowserTab, BrowserToolError> {
        if !matches!(
            tab.lifecycle,
            magi_browser_authority::BrowserTabLifecycle::Ready
                | magi_browser_authority::BrowserTabLifecycle::Suspended
                | magi_browser_authority::BrowserTabLifecycle::Crashed
        ) {
            return Err(BrowserToolError::new(
                "browser_tab_not_ready",
                "指定的浏览器 Tab 当前不可用",
            ));
        }
        let tab = if tab.lifecycle == magi_browser_authority::BrowserTabLifecycle::Crashed {
            self.mutate(|authority| {
                authority.transition_tab(
                    &tab.tab_id,
                    magi_browser_authority::BrowserTabLifecycle::Suspended,
                    UtcMillis::now(),
                )
            })?
        } else {
            tab
        };
        let reply = client
            .request(BrowserHostCommand::RestorePage {
                tab_id: tab.tab_id.clone(),
                browser_session_id: tab.browser_session_id.clone(),
                initial_url: tab.url.clone(),
                logical_viewport: magi_browser_authority::BrowserLogicalViewport::Auto,
                navigation_revision: tab.navigation_revision,
                snapshot_revision: tab.snapshot_revision,
                allow_page_eviction: false,
            })
            .await
            .map_err(browser_host_client_error)?;
        let page = page_state(reply.response.outcome, "恢复浏览器 Tab 失败")?;
        self.apply_page_state(&tab.tab_id, page)
    }

    /// 向 Host 取真实 Surface 并让 Authority 接受。
    ///
    /// 新建 Tab 后，Renderer 注册 guest 并完成初始加载会让 Host 的导航代次先走到 1，而 Authority
    /// 里的逻辑 Tab 仍是 0，直到 `page_updated` 事件被应用才追平。这个窗口里
    /// `accept_primary_surface` 会因代次不相等而返回 false。此时 Surface 已经是真实的，页面上
    /// 还没有执行任何动作，所以对「Host 领先于 Authority」这一种情形做**有限次**重新确认；
    /// 其他原因（代次落后、Surface 被替换）仍然立即失败，不做盲目重试。
    pub(super) async fn ensure_surface(
        &self,
        client: &BrowserHostClient,
        tab_id: &BrowserTabId,
    ) -> Result<BrowserSurfaceBinding, BrowserToolError> {
        for attempt in 1..=SURFACE_SYNC_ATTEMPTS {
            let reply = client
                .request(BrowserHostCommand::EnsureSurface {
                    tab_id: tab_id.clone(),
                })
                .await
                .map_err(browser_host_client_error)?;
            let BrowserHostCommandResult::SurfaceBinding(binding) =
                succeeded_result(reply.response.outcome, "等待浏览器真实 Surface 失败")?
            else {
                return Err(BrowserToolError::new(
                    "browser_result_invalid",
                    "浏览器真实 Surface 返回结果无效",
                ));
            };
            if binding.tab_id != *tab_id || binding.web_contents_id == 0 {
                return Err(BrowserToolError::new(
                    "browser_surface_invalid",
                    "浏览器 Host 返回了无效的真实 Surface 身份",
                ));
            }
            let accepted = self.mutate(|authority| {
                let (accepted, _, _) =
                    authority.accept_primary_surface(binding.clone(), UtcMillis::now())?;
                Ok(accepted)
            })?;
            if accepted {
                return Ok(binding);
            }
            let authority_revision = self
                .authority
                .lock()
                .expect("browser authority lock poisoned")
                .tab(tab_id)
                .map(|tab| tab.navigation_revision);
            if !should_resync_surface(authority_revision, binding.navigation_revision, attempt) {
                break;
            }
            tokio::time::sleep(SURFACE_SYNC_DELAY).await;
        }
        Err(BrowserToolError::new(
            "browser_surface_stale",
            "浏览器真实 Surface 在确认前已经变更",
        ))
    }

    pub(super) async fn prepare_agent_write(
        &self,
        client: &BrowserHostClient,
        session: &magi_browser_authority::BrowserSession,
        tab: &magi_browser_authority::BrowserTab,
        scope: BrowserToolCallScope<'_>,
    ) -> Result<BrowserHostControl, BrowserToolError> {
        let _control_guard = self.control_locks.lock_tab(&tab.tab_id).await;
        // Control is scoped to one physical Surface. There is no session-wide
        // control mode to reclaim; acquiring the target Surface lease below is
        // the only authority transition required for an Agent write.
        let (lease, identity, lease_acquired) = self.acquire_or_reuse_lease(
            session,
            tab,
            scope.context,
            scope.session_id,
            scope.workspace_id,
        )?;
        let validated = self
            .authority
            .lock()
            .expect("browser authority lock poisoned")
            .validate_write(ValidateBrowserWrite {
                lease_id: &lease.lease_id,
                fence: lease.fence,
                tab_id: &tab.tab_id,
                surface_id: &lease.surface_id,
                owner: &identity.owner,
                turn_id: &identity.turn_id,
                goal_binding: identity.goal_binding.as_ref(),
                now: UtcMillis::now(),
            })
            .map_err(|error| {
                BrowserToolError::new("browser_control_lease_invalid", error.to_string())
            })?;
        let reply = client
            .request(BrowserHostCommand::UpdateControl {
                tab_id: tab.tab_id.clone(),
                surface_id: lease.surface_id.clone(),
                control: BrowserHostControlUpdate::Agent {
                    lease_id: lease.lease_id.clone(),
                    fence: validated.fence,
                },
            })
            .await
            .map_err(browser_host_client_error)?;
        succeeded_result(reply.response.outcome, "同步浏览器控制权失败")?;
        if lease_acquired {
            self.publish_browser_event(
                "browser.lease.acquired",
                session,
                json!({
                    "lease_id": lease.lease_id,
                    "tab_id": tab.tab_id,
                    "surface_id": lease.surface_id,
                    "turn_id": identity.turn_id,
                    "expires_at": lease.expires_at,
                }),
            );
        }
        Ok(BrowserHostControl::Agent {
            lease_id: lease.lease_id,
            fence: validated.fence,
        })
    }

    pub(super) fn acquire_or_reuse_lease(
        &self,
        _session: &magi_browser_authority::BrowserSession,
        tab: &magi_browser_authority::BrowserTab,
        context: &magi_tool_runtime::ToolExecutionContext,
        session_id: &SessionId,
        workspace_id: Option<&WorkspaceId>,
    ) -> Result<
        (
            magi_browser_authority::BrowserControlLease,
            BrowserExecutionIdentity,
            bool,
        ),
        BrowserToolError,
    > {
        let mut ownership = self
            .session_store
            .execution_ownership(session_id)
            .unwrap_or(ExecutionOwnership {
                session_id: Some(session_id.clone()),
                workspace_id: workspace_id.cloned(),
                mission_id: None,
                task_id: context.task_id.clone(),
                worker_id: context.worker_id.clone(),
                execution_chain_ref: None,
            });
        ownership.session_id = Some(session_id.clone());
        ownership.workspace_id = workspace_id.cloned();
        if context.task_id.is_some() {
            ownership.task_id = context.task_id.clone();
        }
        if context.worker_id.is_some() {
            ownership.worker_id = context.worker_id.clone();
        }
        let turn_id = self.browser_execution_id(context, session_id);
        let goal_binding =
            self.session_store
                .active_goal(session_id)
                .map(|goal| GoalControlBinding {
                    goal_id: goal.goal_id,
                    control_revision: goal.control_revision,
                });
        let identity = BrowserExecutionIdentity {
            owner: ownership.clone(),
            turn_id: turn_id.clone(),
            goal_binding: goal_binding.clone(),
        };
        let now = UtcMillis::now();
        let _write_guard = self.write_lock.lock().expect("browser write lock poisoned");
        let mut authority = self
            .authority
            .lock()
            .expect("browser authority lock poisoned");
        let surface_id = authority
            .primary_surface(&tab.tab_id)
            .map(|surface| surface.surface_id.clone())
            .ok_or_else(|| {
                BrowserToolError::new(
                    "browser_surface_unavailable",
                    "浏览器 Tab 尚未绑定可控制的真实 Browser Surface",
                )
            })?;
        if let Some(lease) = authority
            .active_lease_for_surface(&tab.tab_id, &surface_id)
            .cloned()
        {
            if lease.owner.session_id == ownership.session_id
                && lease.owner.task_id == ownership.task_id
                && lease.owner.worker_id == ownership.worker_id
                && lease.turn_id == turn_id
                && lease.goal_binding == goal_binding
                && now < lease.expires_at
            {
                return Ok((lease, identity, false));
            }
            if lease.owner.session_id == ownership.session_id
                && lease.owner.task_id == ownership.task_id
                && lease.owner.worker_id == ownership.worker_id
            {
                authority
                    .revoke_lease(&lease.lease_id, BrowserLeaseEndReason::TurnStopped, now)
                    .map_err(|error| {
                        BrowserToolError::new("browser_control_lease_failed", error.to_string())
                    })?;
            } else {
                return Err(BrowserToolError::new(
                    "browser_control_lease_conflict",
                    "这个浏览器标签页正由另一个代理控制。不要等待或重试同一标签页：省略 tab_id 让系统为你新开标签页，或指定其他 tab_id。",
                ));
            }
        }
        let lease = authority
            .acquire_lease(AcquireBrowserLease {
                lease_id: BrowserLeaseId::new(format!(
                    "browser-lease-{}-{}",
                    now.0,
                    BROWSER_LEASE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
                )),
                tab_id: tab.tab_id.clone(),
                surface_id,
                owner: ownership,
                turn_id,
                goal_binding,
                acquired_at: now,
                expires_at: UtcMillis(now.0.saturating_add(LEASE_TTL.as_millis() as u64)),
            })
            .map_err(|error| match error {
                magi_browser_authority::BrowserAuthorityError::UserControlHeld(_) => {
                    let mut tool_error = BrowserToolError::new(
                        "browser_user_in_control",
                        "用户正在直接操作这个浏览器页面，代理的浏览器操作已暂停。不要重试浏览器操作：等待用户在浏览器面板点击“交还控制”，或先继续不依赖该页面的工作并告知用户。",
                    );
                    tool_error.requires_user_action = true;
                    tool_error
                }
                other => BrowserToolError::new("browser_control_lease_failed", other.to_string()),
            })?;
        Ok((lease, identity, true))
    }

    pub(super) fn browser_execution_id(
        &self,
        context: &magi_tool_runtime::ToolExecutionContext,
        session_id: &SessionId,
    ) -> String {
        context
            .browser_execution_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                context
                    .task_id
                    .as_ref()
                    .map(|task_id| format!("task:{task_id}"))
            })
            .or_else(|| {
                self.session_store
                    .runtime_sidecar(session_id)
                    .and_then(|sidecar| sidecar.current_turn.map(|turn| turn.turn_id))
            })
            .unwrap_or_else(|| format!("session:{session_id}"))
    }

    pub(super) fn mutate<T>(
        &self,
        mutation: impl FnOnce(
            &mut magi_browser_authority::BrowserAuthority,
        ) -> Result<T, magi_browser_authority::BrowserAuthorityError>,
    ) -> Result<T, BrowserToolError> {
        crate::state::commit_browser_authority_mutation(
            &self.authority,
            &self.write_lock,
            &self.state_writable,
            self.persistence.as_deref(),
            mutation,
        )
        .map_err(|error| match error {
            crate::state::BrowserAuthorityCommitError::NotWritable => {
                BrowserToolError::new("browser_state_not_writable", "浏览器状态当前不可持久化")
            }
            crate::state::BrowserAuthorityCommitError::Rejected(error) => {
                browser_authority_tool_error(error)
            }
            crate::state::BrowserAuthorityCommitError::Persist(error) => BrowserToolError::new(
                "browser_state_persist_failed",
                format!("浏览器状态持久化失败: {error:?}"),
            ),
        })
    }

    pub(super) fn apply_page_state(
        &self,
        tab_id: &BrowserTabId,
        page: magi_browser_authority::BrowserHostPageState,
    ) -> Result<magi_browser_authority::BrowserTab, BrowserToolError> {
        let current = self
            .authority
            .lock()
            .expect("browser authority lock poisoned")
            .tab(tab_id)
            .cloned();
        if let Some(current) = current
            && current.lifecycle == magi_browser_authority::BrowserTabLifecycle::Ready
            && current.navigation_revision == page.navigation_revision
            && current.url == page.url
            && current.origin == page.origin
            && current.title == page.title
        {
            return Ok(current);
        }
        let tab = self.mutate(|authority| {
            authority.transition_tab(
                tab_id,
                magi_browser_authority::BrowserTabLifecycle::Ready,
                UtcMillis::now(),
            )?;
            authority.apply_host_page_state(
                tab_id,
                page.navigation_revision,
                page.url,
                page.origin,
                page.title,
                UtcMillis::now(),
            )
        })?;
        self.publish_tab_event("browser.tab.updated", &tab);
        Ok(tab)
    }

    pub(super) fn mark_tab_crashed(&self, tab_id: &BrowserTabId) {
        let crashed = self.mutate(|authority| {
            authority.transition_tab(
                tab_id,
                magi_browser_authority::BrowserTabLifecycle::Crashed,
                UtcMillis::now(),
            )
        });
        if let Ok(tab) = crashed {
            self.publish_tab_event("browser.tab.crashed", &tab);
        }
    }
}
