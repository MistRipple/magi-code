use super::*;

pub(super) async fn activate_tab(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
) -> Result<Json<BrowserSessionResponse>, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    let tab_id = BrowserTabId::new(tab_id);
    let (_tab, session) = browser_tab_scope(&state, &tab_id)?;
    require_session_scope(&session)?;
    ensure_browser_ui_ready(&state)?;
    let mut tab = state
        .mutate_browser_authority(|authority| prepare_browser_tab_activation(authority, &tab_id))?;
    // Host 的真实 Surface 只有在右栏 Renderer 已经切换到目标 Browser Tab
    // 后才能取得内容槽。先发出激活意图，再等待 Renderer 注册真实
    // <webview>；RestorePage 必须排在内容槽绑定之后，否则会把页面恢复请求
    // 送进没有 WebContents 的逻辑 Surface，形成“双方互等”的激活死锁。
    publish_browser_event(
        &state,
        "browser.tab.activation_requested",
        session.owner_workspace_id(),
        require_session_scope(&session)?,
        serde_json::json!({
            "browser_session_id": session.browser_session_id,
            "tab_id": tab_id,
        }),
    );
    wait_for_browser_primary_surface(&state, &tab_id).await?;
    let session = loop {
        let reply = require_host_success(
            require_browser_host(&state)?
                .request(BrowserHostCommand::RestorePage {
                    tab_id: tab_id.clone(),
                    browser_session_id: tab.browser_session_id.clone(),
                    initial_url: tab.url.clone(),
                    logical_viewport: magi_browser_authority::BrowserLogicalViewport::Auto,
                    navigation_revision: tab.navigation_revision,
                    snapshot_revision: tab.snapshot_revision,
                    allow_page_eviction: true,
                })
                .await,
            "激活浏览器页面失败",
        )?;
        let BrowserHostCommandResult::PageState(page_state) = reply else {
            return Err(ApiError::InternalAssemblyError(
                "浏览器 Host 激活页面结果缺少页面状态".to_string(),
            ));
        };
        match state.mutate_browser_authority(|authority| {
            apply_browser_tab_activation(authority, &tab_id, page_state)
        })? {
            BrowserTabActivation::Retry(next_tab) => tab = next_tab,
            BrowserTabActivation::Completed(session) => break session,
        }
    };
    sync_browser_annotations_to_host(&state, &tab_id).await?;
    publish_browser_event(
        &state,
        "browser.tab.activated",
        session.owner_workspace_id(),
        require_session_scope(&session)?,
        serde_json::json!({
            "browser_session_id": session.browser_session_id,
            "tab_id": tab_id,
            "revision": session.revision,
        }),
    );
    Ok(Json(browser_session_response(&state, session)?))
}

/// 用户在浏览器面板点击“交还控制”：解除用户接管，代理的下一次浏览器操作才能重新取得控制权。
pub(super) async fn release_user_control(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
) -> Result<Json<BrowserSessionResponse>, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    let tab_id = BrowserTabId::new(tab_id);
    let (_tab, session) = browser_tab_scope(&state, &tab_id)?;
    let session_id = require_session_scope(&session)?.clone();
    let released =
        state.mutate_browser_authority(|authority| authority.release_user_control(&tab_id))?;
    let session = browser_tab_scope(&state, &tab_id)?.1;
    if released {
        publish_browser_event(
            &state,
            "browser.control.released_to_agent",
            session.owner_workspace_id(),
            &session_id,
            serde_json::json!({
                "browser_session_id": session.browser_session_id,
                "tab_id": tab_id,
                "revision": session.revision,
            }),
        );
    }
    Ok(Json(browser_session_response(&state, session)?))
}

pub(super) async fn close_tab(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    let tab_id = BrowserTabId::new(tab_id);
    let (_tab, session) = browser_tab_scope(&state, &tab_id)?;
    if state.browser_host_client().is_some()
        && let Err(error) = ensure_user_control_for_ui(&state, &session, &tab_id).await
    {
        tracing::warn!(tab_id = %tab_id, ?error, "关闭浏览器 Tab 时同步用户控制权失败，继续收口逻辑状态");
    }
    close_browser_tab(&state, &tab_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct NavigateBrowserTabRequest {
    pub(super) action: String,
    pub(super) url: Option<String>,
    #[serde(default)]
    pub(super) client_platform: Option<BrowserClientPlatform>,
}

pub(super) async fn navigate_tab(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<NavigateBrowserTabRequest>,
) -> Result<Json<BrowserTabResponse>, ApiError> {
    require_desktop_browser_capability(&state, &headers, request.client_platform)?;
    let tab_id = BrowserTabId::new(tab_id);
    let (_, session) = browser_tab_scope(&state, &tab_id)?;
    require_session_scope(&session)?;
    ensure_browser_ui_ready(&state)?;
    let action = request.action.trim();
    let navigation = match action {
        "url" => {
            let url = request
                .url
                .as_deref()
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .ok_or_else(|| ApiError::InvalidInput("url 导航必须提供 URL".to_string()))?
                .to_string();
            validate_navigation_url(&state, &url)?;
            BrowserNavigation::Url {
                url,
                handle_before_unload: None,
                init_script: None,
                timeout_ms: None,
            }
        }
        "back" => BrowserNavigation::Back { timeout_ms: None },
        "forward" => BrowserNavigation::Forward { timeout_ms: None },
        "stop" => BrowserNavigation::Stop,
        "reload" => BrowserNavigation::Reload {
            ignore_cache: false,
            handle_before_unload: None,
            timeout_ms: None,
        },
        _ => {
            return Err(ApiError::InvalidInput(
                "action 必须是 url、back、forward、stop 或 reload".to_string(),
            ));
        }
    };
    let reply = match navigation {
        BrowserNavigation::Stop => require_host_success(
            require_browser_host(&state)?
                .request(BrowserHostCommand::StopNavigation {
                    tab_id: tab_id.clone(),
                })
                .await,
            "停止浏览器导航失败",
        )?,
        navigation => {
            let fence = ensure_user_control_for_ui(&state, &session, &tab_id)
                .await?
                .fence;
            let _control_guard = state.browser_control_locks.lock_tab(&tab_id).await;
            require_host_success(
                require_browser_host(&state)?
                    .request(BrowserHostCommand::Navigate {
                        tab_id: tab_id.clone(),
                        control: BrowserHostControl::User { fence },
                        navigation,
                    })
                    .await,
                "浏览器导航失败",
            )?
        }
    };
    let BrowserHostCommandResult::PageState(page_state) = reply else {
        return Err(ApiError::InternalAssemblyError(
            "浏览器 Host 导航结果缺少页面状态".to_string(),
        ));
    };
    let tab = state.mutate_browser_authority(|authority| {
        authority.apply_host_page_state(
            &tab_id,
            page_state.navigation_revision,
            page_state.url,
            page_state.origin,
            page_state.title,
            UtcMillis::now(),
        )
    })?;
    // 导航请求已经在 Desktop Host 的 Tab 资源队列中占用当前资源。
    // 此处不能重新发送同一 Tab 的标记投影并等待它完成，否则 Host 必须
    // 先结算 navigate 才会执行 set_annotations，而当前请求又在等待后者，
    // 形成跨进程的资源队列死锁。标记事实已经由 Authority 收敛；导航完成
    // 后由 Host 的 page_updated/content-ready 事件按最新 Surface 重放。
    publish_browser_event(
        &state,
        "browser.tab.updated",
        session.owner_workspace_id(),
        require_session_scope(&session)?,
        serde_json::json!({
            "browser_session_id": session.browser_session_id,
            "tab_id": tab.tab_id,
            "url": tab.url,
            "navigation_revision": tab.navigation_revision,
            "surface_id": state.browser_primary_surface_id(&tab.tab_id),
        }),
    );
    let response = {
        let authority = state
            .browser_authority
            .lock()
            .expect("browser authority lock poisoned");
        browser_tab_response_from_authority(&authority, tab)
    };
    Ok(Json(response))
}

pub(super) fn validate_navigation_url(state: &ApiState, url: &str) -> Result<(), ApiError> {
    validate_browser_navigation_url_with(
        url,
        state.browser_capability_snapshot().lan_access_enabled,
    )
    .map_err(|error| ApiError::InvalidInput(format!("浏览器导航 URL 不合法: {error}")))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ScreenshotBrowserTabRequest {
    #[serde(default)]
    pub(super) full_page: bool,
    #[serde(default)]
    pub(super) client_platform: Option<BrowserClientPlatform>,
}

pub(super) async fn screenshot_tab(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ScreenshotBrowserTabRequest>,
) -> Result<Response, ApiError> {
    require_desktop_browser_capability(&state, &headers, request.client_platform)?;
    let tab_id = BrowserTabId::new(tab_id);
    let (tab, session) = browser_tab_scope(&state, &tab_id)?;
    require_session_scope(&session)?;
    ensure_browser_ui_ready(&state)?;
    let reply = require_browser_host(&state)?
        .request(BrowserHostCommand::Screenshot {
            tab_id,
            navigation_revision: tab.navigation_revision,
            target: None,
            clip: None,
            full_page: request.full_page,
            format: magi_browser_authority::BrowserScreenshotFormat::Png,
            quality: None,
        })
        .await
        .map_err(|error| ApiError::model_invocation_failed("浏览器截图失败", error))?;
    match reply.response.outcome {
        BrowserHostCommandOutcome::Succeeded(result) => match *result {
            BrowserHostCommandResult::BinaryPayload(metadata) => {
                let bytes = reply.binary.ok_or_else(|| {
                    ApiError::InternalAssemblyError("浏览器截图缺少二进制内容".to_string())
                })?;
                Ok((
                    [(axum::http::header::CONTENT_TYPE, metadata.mime_type)],
                    bytes,
                )
                    .into_response())
            }
            result => Err(host_outcome_error(
                "浏览器截图失败",
                BrowserHostCommandOutcome::Succeeded(Box::new(result)),
            )),
        },
        outcome => Err(host_outcome_error("浏览器截图失败", outcome)),
    }
}

pub(super) enum BrowserTabActivation {
    Retry(BrowserTab),
    Completed(BrowserSession),
}

pub(super) fn prepare_browser_tab_activation(
    authority: &mut BrowserAuthority,
    tab_id: &BrowserTabId,
) -> Result<BrowserTab, magi_browser_authority::BrowserAuthorityError> {
    let tab = authority
        .tab(tab_id)
        .cloned()
        .ok_or_else(|| magi_browser_authority::BrowserAuthorityError::UnknownTab(tab_id.clone()))?;
    match tab.lifecycle {
        BrowserTabLifecycle::Creating
        | BrowserTabLifecycle::Ready
        | BrowserTabLifecycle::Suspended => Ok(tab),
        BrowserTabLifecycle::Crashed => {
            authority.transition_tab(tab_id, BrowserTabLifecycle::Suspended, UtcMillis::now())
        }
        BrowserTabLifecycle::Closed => {
            Err(magi_browser_authority::BrowserAuthorityError::TabNotReady {
                tab_id: tab_id.clone(),
                lifecycle: tab.lifecycle,
            })
        }
    }
}

pub(super) fn apply_browser_tab_activation(
    authority: &mut BrowserAuthority,
    tab_id: &BrowserTabId,
    page_state: magi_browser_authority::BrowserHostPageState,
) -> Result<BrowserTabActivation, magi_browser_authority::BrowserAuthorityError> {
    let current = authority
        .tab(tab_id)
        .cloned()
        .ok_or_else(|| magi_browser_authority::BrowserAuthorityError::UnknownTab(tab_id.clone()))?;
    if current.lifecycle == BrowserTabLifecycle::Closed {
        return Err(magi_browser_authority::BrowserAuthorityError::TabNotReady {
            tab_id: tab_id.clone(),
            lifecycle: current.lifecycle,
        });
    }
    if current.lifecycle == BrowserTabLifecycle::Crashed {
        return Ok(BrowserTabActivation::Retry(authority.transition_tab(
            tab_id,
            BrowserTabLifecycle::Suspended,
            UtcMillis::now(),
        )?));
    }
    let ready = match current.lifecycle {
        BrowserTabLifecycle::Creating | BrowserTabLifecycle::Suspended => {
            authority.transition_tab(tab_id, BrowserTabLifecycle::Ready, UtcMillis::now())?
        }
        BrowserTabLifecycle::Ready => current,
        BrowserTabLifecycle::Crashed | BrowserTabLifecycle::Closed => unreachable!(),
    };
    authority.apply_host_page_state(
        tab_id,
        page_state.navigation_revision,
        page_state.url,
        page_state.origin,
        page_state.title,
        UtcMillis::now(),
    )?;
    let session = authority
        .session(&ready.browser_session_id)
        .cloned()
        .ok_or_else(|| {
            magi_browser_authority::BrowserAuthorityError::UnknownSession(
                ready.browser_session_id.clone(),
            )
        })?;
    Ok(BrowserTabActivation::Completed(session))
}
