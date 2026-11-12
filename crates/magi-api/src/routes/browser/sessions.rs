use super::*;

pub(super) async fn create_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateBrowserSessionRequest>,
) -> Result<(StatusCode, Json<BrowserSessionResponse>), ApiError> {
    require_desktop_browser_capability(&state, &headers, request.client_platform)?;
    let (scope, session_id) = validate_session_scope(
        &state,
        request.scope,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        request.session_id.trim(),
    )?;
    let workspace_id = scope.workspace_id();
    ensure_browser_ui_ready(&state)?;

    // Browser Session 与 Magi Session 同生命周期。桌面端长期运行时，异常退出
    // 可能留下不再属于 SessionStore 的逻辑 Tab；在新建浏览器会话前
    // 先执行一次权威收口，避免这些孤儿资源继续占用全局页面容量。
    let reconciled_browser_sessions = state.reconcile_browser_sessions_with_session_store()?;
    if reconciled_browser_sessions > 0 {
        state.persist_browser_durable_state_for_api()?;
        tracing::info!(
            count = reconciled_browser_sessions,
            "新建浏览器会话前已清理孤儿 Browser Session"
        );
    }
    let existing = wait_for_magi_browser_session(&state, &session_id).await?;
    if let Some(existing) = existing {
        if existing.lifecycle != BrowserSessionLifecycle::Failed {
            return Ok((
                StatusCode::OK,
                Json(browser_session_response(&state, existing)?),
            ));
        }

        // 失败会话不能复用：其 Tab 已经没有对应的 Browser Host 页面。
        // 先关闭失效的权威边界，再为用户下一次浏览器操作创建新会话。
        let closed = state.mutate_browser_authority(|authority| {
            authority.transition_session(
                &existing.browser_session_id,
                BrowserSessionLifecycle::Closed,
                UtcMillis::now(),
            )
        })?;
        publish_browser_event(
            &state,
            "browser.session.closed",
            closed.owner_workspace_id(),
            require_session_scope(&closed)?,
            serde_json::json!({
                "browser_session_id": closed.browser_session_id,
                "lifecycle": closed.lifecycle,
                "reason": "failed_session_replaced",
                "revision": closed.revision,
            }),
        );
    }
    let now = UtcMillis::now();
    let browser_session_id = BrowserSessionId::new(format!(
        "browser-session-{}-{}",
        now.0,
        BROWSER_SESSION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let session = state.mutate_browser_authority(|authority| {
        authority.create_session(CreateBrowserSession {
            browser_session_id: browser_session_id.clone(),
            owner: BrowserSessionOwner::Session {
                session_id: session_id.clone(),
                workspace_id: workspace_id.clone(),
            },
            profile_id: BrowserProfileId::new("browser-profile-default"),
            now,
        })?;
        authority.transition_session(&browser_session_id, BrowserSessionLifecycle::Ready, now)
    })?;
    publish_browser_event(
        &state,
        "browser.session.created",
        workspace_id.as_ref(),
        &session_id,
        serde_json::json!({
            "browser_session_id": browser_session_id,
            "lifecycle": session.lifecycle,
            "revision": session.revision,
        }),
    );
    Ok((
        StatusCode::CREATED,
        Json(browser_session_response(&state, session)?),
    ))
}

pub(super) async fn get_session(
    State(state): State<ApiState>,
    Path(browser_session_id): Path<String>,
) -> Result<Json<BrowserSessionResponse>, ApiError> {
    let browser_session_id = BrowserSessionId::new(browser_session_id);
    let session = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .session(&browser_session_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("浏览器会话不存在", browser_session_id.as_str()))?;
    Ok(Json(browser_session_response(&state, session)?))
}

pub(super) async fn get_current_session(
    State(state): State<ApiState>,
    Query(query): Query<CurrentBrowserSessionQuery>,
) -> Result<Json<CurrentBrowserSessionResponse>, ApiError> {
    let (scope, session_id) = validate_session_scope(
        &state,
        query.scope,
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
        query.session_id.trim(),
    )?;
    let workspace_id = scope.workspace_id();
    let session = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .session_for_magi_session(&session_id)
        .filter(|session| session.owner_workspace_id() == workspace_id.as_ref())
        .cloned();
    let session = session
        .map(|session| browser_session_response(&state, session))
        .transpose()?;
    Ok(Json(CurrentBrowserSessionResponse { session }))
}

pub(super) async fn close_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(browser_session_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    let browser_session_id = BrowserSessionId::new(browser_session_id);
    let session = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .session(&browser_session_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("浏览器会话不存在", browser_session_id.as_str()))?;
    if let Some(client) = state.browser_host_client() {
        for tab_id in &session.tab_ids {
            match client
                .request(BrowserHostCommand::ClosePage {
                    tab_id: tab_id.clone(),
                })
                .await
            {
                Ok(reply) if host_command_succeeded(&reply.response.outcome) => {}
                Ok(reply) => tracing::warn!(
                    tab_id = %tab_id,
                    outcome = ?reply.response.outcome,
                    "关闭浏览器 Tab 时 Host 返回失败，继续收口权威状态"
                ),
                Err(error) => tracing::warn!(
                    tab_id = %tab_id,
                    ?error,
                    "关闭浏览器 Tab 时 Host 不可用，继续收口权威状态"
                ),
            }
        }
    }
    let closed = state.mutate_browser_authority(|authority| {
        authority.transition_session(
            &browser_session_id,
            BrowserSessionLifecycle::Closed,
            UtcMillis::now(),
        )
    })?;
    publish_browser_event(
        &state,
        "browser.session.status_changed",
        closed.owner_workspace_id(),
        require_session_scope(&closed)?,
        serde_json::json!({
            "browser_session_id": closed.browser_session_id,
            "lifecycle": closed.lifecycle,
            "revision": closed.revision,
        }),
    );
    Ok(StatusCode::NO_CONTENT)
}
