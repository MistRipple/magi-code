use super::*;

pub(super) fn require_browser_host(
    state: &ApiState,
) -> Result<magi_browser_authority::BrowserHostClient, ApiError> {
    state
        .browser_host_client()
        .ok_or_else(|| ApiError::Conflict("桌面浏览器控制通道尚未启动".to_string()))
}

pub(super) async fn wait_for_browser_primary_surface(
    state: &ApiState,
    tab_id: &BrowserTabId,
) -> Result<String, ApiError> {
    let deadline = Instant::now() + BROWSER_SURFACE_WAIT_TIMEOUT;
    loop {
        if let Some(surface_id) = state.browser_primary_surface_id(tab_id) {
            return Ok(surface_id);
        }
        if Instant::now() >= deadline {
            return Err(ApiError::Conflict(
                "浏览器页面已恢复，但真实 Surface 尚未完成绑定，请稍后重试".to_string(),
            ));
        }
        tokio::time::sleep(BROWSER_SURFACE_POLL_INTERVAL).await;
    }
}

pub(super) async fn sync_browser_annotations_to_host(
    state: &ApiState,
    tab_id: &BrowserTabId,
) -> Result<(), ApiError> {
    let Some(client) = state.browser_host_client() else {
        return Ok(());
    };
    let annotations = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .annotations_for_tab(tab_id)
        .into_iter()
        .map(|annotation| {
            serde_json::to_value(annotation)
                .map_err(|error| ApiError::internal_assembly("序列化浏览器标记失败", error))
        })
        .collect::<Result<Vec<_>, _>>()?;
    require_host_success(
        client
            .request(BrowserHostCommand::SetAnnotations {
                tab_id: tab_id.clone(),
                annotations,
            })
            .await,
        "同步浏览器标记到页面失败",
    )?;
    Ok(())
}

pub(super) async fn ensure_user_control_for_ui(
    state: &ApiState,
    session: &BrowserSession,
    tab_id: &BrowserTabId,
) -> Result<BrowserSurfaceControlSnapshot, ApiError> {
    require_session_scope(session)?;
    ensure_browser_ui_ready(state)?;
    // Authority 的短临界区与 Session/Host 的异步等待分开。这里不能由调用方
    // 先持有 Tab 控制锁再进入 interrupt_session_turn，否则关闭
    // 会话的固定顺序（session turn -> browser control）会与用户接管互锁。
    let (control, revoked) = {
        let _control_guard = state.browser_control_locks.lock_tab(tab_id).await;
        state.mutate_browser_authority(|authority| {
            let surface_id = authority
                .primary_surface(tab_id)
                .map(|surface| surface.surface_id.clone())
                .ok_or_else(|| {
                    magi_browser_authority::BrowserAuthorityError::PrimarySurfaceUnavailable(
                        tab_id.clone(),
                    )
                })?;
            let (control, revoked) =
                authority.take_user_control(tab_id, &surface_id, UtcMillis::now())?;
            Ok((control, revoked))
        })?
    };
    if !revoked.is_empty() {
        if let Err(error) = crate::turn_service::interrupt_session_turn(
            state,
            require_session_scope(session)?,
            session.owner_workspace_id().map(|id| id.as_str()),
            None,
        )
        .await
        {
            tracing::warn!(
                session_id = %session.magi_session_id().map(|id| id.as_str()).unwrap_or("app"),
                ?error,
                "浏览器用户操作后收口当前 Turn 失败，控制 fence 仍保持生效"
            );
        }
        if let Some(client) = state.browser_host_client() {
            let _control_guard = state.browser_control_locks.lock_tab(tab_id).await;
            require_host_success(
                client
                    .request(BrowserHostCommand::UpdateControl {
                        tab_id: control.tab_id.clone(),
                        surface_id: control.surface_id.clone(),
                        control: BrowserHostControlUpdate::Released {
                            fence: control.fence,
                        },
                    })
                    .await,
                "同步浏览器用户控制权失败",
            )?;
        }
        publish_browser_event(
            state,
            "browser.control.changed",
            session.owner_workspace_id(),
            require_session_scope(session)?,
            serde_json::json!({
                "browser_session_id": session.browser_session_id,
                "tab_id": control.tab_id,
                "surface_id": control.surface_id,
                "mode": "user",
                "fence": control.fence,
            }),
        );
    }
    Ok(control)
}

pub(super) fn require_host_success(
    result: Result<
        magi_browser_authority::BrowserHostCommandReply,
        magi_browser_authority::BrowserHostClientError,
    >,
    context: &str,
) -> Result<BrowserHostCommandResult, ApiError> {
    let reply = result.map_err(|error| ApiError::model_invocation_failed(context, error))?;
    match reply.response.outcome {
        BrowserHostCommandOutcome::Succeeded(result) => Ok(*result),
        outcome => Err(host_outcome_error(context, outcome)),
    }
}

pub(super) fn host_outcome_error(context: &str, outcome: BrowserHostCommandOutcome) -> ApiError {
    match outcome {
        BrowserHostCommandOutcome::Failed(error)
        | BrowserHostCommandOutcome::Indeterminate(error) => {
            ApiError::Conflict(format!("{context}: {} ({})", error.message, error.code))
        }
        BrowserHostCommandOutcome::Cancelled => {
            ApiError::Conflict(format!("{context}: 操作已取消"))
        }
        BrowserHostCommandOutcome::Succeeded(_) => {
            ApiError::InternalAssemblyError(format!("{context}: 无效的 Host 结果"))
        }
    }
}

pub(super) fn validate_session_scope(
    state: &ApiState,
    requested_scope: crate::dto::SessionScopeKindDto,
    requested_workspace_id: Option<&str>,
    requested_workspace_path: Option<&str>,
    session_id: &str,
) -> Result<(SessionScope, SessionId), ApiError> {
    if session_id.is_empty() {
        return Err(ApiError::InvalidInput("sessionId 不能为空".to_string()));
    }
    let session_id = SessionId::new(session_id);
    let request_scope = session_scope::require_session_request_scope(
        state,
        Some(session_id.as_str()),
        requested_scope,
        requested_workspace_id,
        requested_workspace_path,
    )?;
    let session = state
        .session_store
        .session(&session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    if session.status != SessionLifecycleStatus::Active {
        return Err(ApiError::Conflict("会话已关闭，不能打开浏览器".to_string()));
    }
    Ok((request_scope.scope, session_id))
}

pub(super) fn ensure_browser_ui_ready(state: &ApiState) -> Result<(), ApiError> {
    let capability = state.browser_capability_snapshot();
    if !capability.in_app_browser_enabled {
        return Err(ApiError::Conflict("内置浏览器功能未启用".to_string()));
    }
    if !capability.host_status.is_usable() {
        return Err(ApiError::Conflict(format!(
            "桌面浏览器控制通道不可用: {:?}",
            capability.host_status
        )));
    }
    if !capability.host_protocol_compatible || state.browser_host_client().is_none() {
        return Err(ApiError::Conflict("桌面浏览器控制通道尚未就绪".to_string()));
    }
    Ok(())
}

pub(super) async fn wait_for_magi_browser_session(
    state: &ApiState,
    session_id: &SessionId,
) -> Result<Option<BrowserSession>, ApiError> {
    let deadline = Instant::now() + BROWSER_READY_WAIT_TIMEOUT;
    loop {
        let existing = {
            let authority = state
                .browser_authority
                .lock()
                .expect("browser authority lock poisoned");
            authority.session_for_magi_session(session_id).cloned()
        };
        match existing {
            Some(session)
                if matches!(
                    session.lifecycle,
                    BrowserSessionLifecycle::Creating | BrowserSessionLifecycle::Recovering
                ) =>
            {
                if Instant::now() >= deadline {
                    return Err(ApiError::Conflict(
                        "浏览器会话恢复超时，请稍后重试".to_string(),
                    ));
                }
                tokio::time::sleep(BROWSER_READY_POLL_INTERVAL).await;
            }
            other => return Ok(other),
        }
    }
}

pub(super) async fn wait_for_browser_session_ready(
    state: &ApiState,
    browser_session_id: &BrowserSessionId,
) -> Result<BrowserSession, ApiError> {
    let deadline = Instant::now() + BROWSER_READY_WAIT_TIMEOUT;
    loop {
        let session = {
            let authority = state
                .browser_authority
                .lock()
                .expect("browser authority lock poisoned");
            authority
                .session(browser_session_id)
                .cloned()
                .ok_or_else(|| {
                    ApiError::not_found("浏览器会话不存在", browser_session_id.as_str())
                })?
        };
        match session.lifecycle {
            BrowserSessionLifecycle::Ready | BrowserSessionLifecycle::Failed => {
                return Ok(session);
            }
            BrowserSessionLifecycle::Interrupted => {
                return Err(ApiError::Conflict(
                    "浏览器会话因运行组件中断，等待 Browser Host 恢复后重试".to_string(),
                ));
            }
            BrowserSessionLifecycle::Closed => {
                return Err(ApiError::Conflict("浏览器会话已关闭".to_string()));
            }
            BrowserSessionLifecycle::Creating | BrowserSessionLifecycle::Recovering => {
                if Instant::now() >= deadline {
                    return Err(ApiError::Conflict(
                        "浏览器会话恢复超时，请稍后重试".to_string(),
                    ));
                }
                tokio::time::sleep(BROWSER_READY_POLL_INTERVAL).await;
            }
        }
    }
}

pub(super) fn browser_session_response(
    state: &ApiState,
    session: BrowserSession,
) -> Result<BrowserSessionResponse, ApiError> {
    let authority = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned");
    let mut agent_occupied = false;
    let mut control_fence = 0;
    let tabs = session
        .tab_ids
        .iter()
        .filter_map(|tab_id| authority.tab(tab_id).cloned())
        .map(|tab| {
            let response = browser_tab_response_from_authority(&authority, tab);
            if response.agent_occupied {
                agent_occupied = true;
                control_fence = control_fence.max(response.control_fence);
            }
            response
        })
        .collect();
    let active_tab_id = authority.active_tab(&session.browser_session_id).cloned();
    let owner = session.owner_kind().to_string();
    let owner_workspace_id = session.owner_workspace_id().cloned();
    let magi_session_id = session.magi_session_id().cloned();
    Ok(BrowserSessionResponse {
        browser_session_id: session.browser_session_id,
        owner,
        workspace_id: owner_workspace_id,
        session_id: magi_session_id,
        profile_id: session.profile_id,
        lifecycle: session.lifecycle,
        active_tab_id,
        tabs,
        runtime_epoch: session.runtime_epoch,
        revision: session.revision,
        control_mode: if agent_occupied { "agent" } else { "user" }.to_string(),
        control_fence,
        agent_occupied,
        created_at: session.created_at,
        updated_at: session.updated_at,
    })
}

pub(super) fn browser_tab_response_from_authority(
    authority: &BrowserAuthority,
    tab: BrowserTab,
) -> BrowserTabResponse {
    let mut response = BrowserTabResponse::from(tab.clone());
    response.user_control_held = authority.user_control_held(&tab.tab_id);
    response.annotations = authority
        .annotations_for_tab(&tab.tab_id)
        .into_iter()
        .map(BrowserAnnotationResponse::from)
        .collect();
    if let Some(surface) = authority.primary_surface(&tab.tab_id) {
        response.surface_id = Some(surface.surface_id.clone());
        if let Some(lease) = authority.active_lease_for_surface(&tab.tab_id, &surface.surface_id)
            && lease.lifecycle == magi_browser_authority::BrowserLeaseLifecycle::Held
            && lease.expires_at > UtcMillis::now()
        {
            response.agent_occupied = true;
            response.control_fence = lease.fence;
        }
    }
    response
}

pub(super) fn host_command_succeeded(outcome: &BrowserHostCommandOutcome) -> bool {
    matches!(outcome, BrowserHostCommandOutcome::Succeeded(_))
}
