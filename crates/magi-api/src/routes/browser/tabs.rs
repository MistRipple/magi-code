use super::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CreateBrowserTabRequest {
    pub(super) initial_url: String,
    #[serde(default)]
    pub(super) client_platform: Option<BrowserClientPlatform>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct SetActiveBrowserTabRequest {
    pub(super) tab_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CreateBrowserAnnotationRequest {
    pub(super) selection: BrowserAnnotationSelectionRequest,
    pub(super) comment: String,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(super) enum BrowserAnnotationSelectionRequest {
    Element {
        navigation_revision: u64,
        normalized_x: f64,
        normalized_y: f64,
    },
    Region {
        navigation_revision: u64,
        rect: BrowserNormalizedRect,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct UpdateBrowserAnnotationCommentRequest {
    pub(super) comment: String,
}

pub(super) async fn create_tab(
    State(state): State<ApiState>,
    Path(browser_session_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CreateBrowserTabRequest>,
) -> Result<(StatusCode, Json<BrowserTabResponse>), ApiError> {
    require_desktop_browser_capability(&state, &headers, request.client_platform)?;
    let browser_session_id = BrowserSessionId::new(browser_session_id);
    let session = wait_for_browser_session_ready(&state, &browser_session_id).await?;
    validate_navigation_url(&state, &request.initial_url)?;
    let now = UtcMillis::now();
    let tab_id = BrowserTabId::new(format!(
        "browser-tab-{}-{}",
        now.0,
        BROWSER_TAB_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let created = state.mutate_browser_authority(|authority| {
        authority.create_tab(CreateBrowserTab {
            tab_id: tab_id.clone(),
            browser_session_id: browser_session_id.clone(),
            url: request.initial_url.clone(),
            now,
        })
    })?;
    // 浏览器页面由 Electron Main 管理真实 Chromium guest，并绑定到当前
    // 桌面右栏 Browser Tab 的内容槽。先发布逻辑 Tab，再要求 Host 物化
    // 页面，使统一 App Renderer 可以立即渲染加载状态。
    publish_browser_event(
        &state,
        "browser.tab.created",
        session.owner_workspace_id(),
        require_session_scope(&session)?,
        serde_json::json!({
            "browser_session_id": browser_session_id,
            "tab_id": tab_id,
            "url": created.url.clone(),
            "lifecycle": created.lifecycle.clone(),
        }),
    );
    // HTTP 创建只负责提交逻辑 Tab，不能等待 Host 的首帧。
    // Host 通过私有控制通道异步完成 Chromium Page 物化；这样逻辑 Tab、
    // 右栏选择器和主窗口布局不会被网络导航或 Renderer 首次布局阻塞。
    let state_for_host = state.clone();
    let created_response = created.clone();
    let workspace_id = session.owner_workspace_id().cloned();
    let session_id = require_session_scope(&session)?.clone();
    let browser_session_id_for_host = browser_session_id.clone();
    let tab_id_for_host = tab_id.clone();
    let initial_url = request.initial_url;
    let client = state.browser_host_client();
    tokio::spawn(async move {
        let Some(client) = client else {
            finish_browser_tab_creation(
                &state_for_host,
                &workspace_id,
                &session_id,
                &browser_session_id_for_host,
                &tab_id_for_host,
                "桌面浏览器控制通道尚未启动",
            );
            return;
        };
        let result = client
            .request(BrowserHostCommand::CreatePage {
                tab_id: tab_id_for_host.clone(),
                browser_session_id: browser_session_id_for_host.clone(),
                initial_url,
                logical_viewport: magi_browser_authority::BrowserLogicalViewport::Auto,
                navigation_revision: created.navigation_revision,
                snapshot_revision: created.snapshot_revision,
                allow_page_eviction: true,
            })
            .await;
        let page_state = match result {
            Ok(reply) => match reply.response.outcome {
                BrowserHostCommandOutcome::Succeeded(result) => match *result {
                    BrowserHostCommandResult::PageState(page_state) => page_state,
                    _ => {
                        finish_browser_tab_creation(
                            &state_for_host,
                            &workspace_id,
                            &session_id,
                            &browser_session_id_for_host,
                            &tab_id_for_host,
                            "浏览器 Host 创建页面结果缺少页面状态",
                        );
                        return;
                    }
                },
                BrowserHostCommandOutcome::Failed(error)
                | BrowserHostCommandOutcome::Indeterminate(error) => {
                    finish_browser_tab_creation(
                        &state_for_host,
                        &workspace_id,
                        &session_id,
                        &browser_session_id_for_host,
                        &tab_id_for_host,
                        &format!("{} ({})", error.message, error.code),
                    );
                    return;
                }
                BrowserHostCommandOutcome::Cancelled => {
                    finish_browser_tab_creation(
                        &state_for_host,
                        &workspace_id,
                        &session_id,
                        &browser_session_id_for_host,
                        &tab_id_for_host,
                        "浏览器 Host 创建页面已取消",
                    );
                    return;
                }
            },
            Err(error) => {
                finish_browser_tab_creation(
                    &state_for_host,
                    &workspace_id,
                    &session_id,
                    &browser_session_id_for_host,
                    &tab_id_for_host,
                    &error.to_string(),
                );
                return;
            }
        };
        let tab = state_for_host.mutate_browser_authority(|authority| {
            authority.transition_tab(
                &tab_id_for_host,
                BrowserTabLifecycle::Ready,
                UtcMillis::now(),
            )?;
            authority.apply_host_page_state(
                &tab_id_for_host,
                page_state.navigation_revision,
                page_state.url,
                page_state.origin,
                page_state.title,
                UtcMillis::now(),
            )
        });
        match tab {
            Ok(tab) => {
                let surface_id = state_for_host.browser_primary_surface_id(&tab_id_for_host);
                publish_browser_event(
                    &state_for_host,
                    "browser.tab.updated",
                    workspace_id.as_ref(),
                    &session_id,
                    serde_json::json!({
                        "browser_session_id": browser_session_id_for_host,
                        "tab_id": tab_id_for_host,
                        "url": tab.url,
                        "title": tab.title,
                        "lifecycle": tab.lifecycle,
                        "surface_id": surface_id,
                    }),
                );
            }
            Err(error) => finish_browser_tab_creation(
                &state_for_host,
                &workspace_id,
                &session_id,
                &browser_session_id_for_host,
                &tab_id_for_host,
                &format!("浏览器逻辑状态收敛失败: {error:?}"),
            ),
        }
    });
    Ok((StatusCode::CREATED, Json(created_response.into())))
}

/// 记录右侧面板当前选中的 Browser Tab。
///
/// 这是 UI 焦点同步，不会恢复或创建 Chromium Page，也不会改变控制 Lease。
/// 浏览器工具在没有显式 tab_id 时使用该运行态焦点作为默认目标。
pub(super) async fn set_active_tab(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(browser_session_id): Path<String>,
    Json(request): Json<SetActiveBrowserTabRequest>,
) -> Result<Json<BrowserSessionResponse>, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    let browser_session_id = BrowserSessionId::new(browser_session_id);
    wait_for_browser_session_ready(&state, &browser_session_id).await?;
    let tab_id = BrowserTabId::new(request.tab_id);
    let session = state.mutate_browser_authority(|authority| {
        authority.set_active_tab(&browser_session_id, &tab_id)?;
        authority
            .session(&browser_session_id)
            .cloned()
            .ok_or_else(|| {
                magi_browser_authority::BrowserAuthorityError::UnknownSession(
                    browser_session_id.clone(),
                )
            })
    })?;
    publish_browser_event(
        &state,
        "browser.tab.focused",
        session.owner_workspace_id(),
        require_session_scope(&session)?,
        serde_json::json!({
            "browser_session_id": browser_session_id,
            "tab_id": tab_id,
            "revision": session.revision,
        }),
    );
    Ok(Json(browser_session_response(&state, session)?))
}

pub(super) fn finish_browser_tab_creation(
    state: &ApiState,
    workspace_id: &Option<WorkspaceId>,
    session_id: &SessionId,
    browser_session_id: &BrowserSessionId,
    tab_id: &BrowserTabId,
    reason: &str,
) {
    let outcome = state.mutate_browser_authority(|authority| {
        let Some(current) = authority.tab(tab_id) else {
            // 创建结果晚于用户关闭/删除 Tab 到达时，逻辑资源已经完成收口。
            return Ok(None);
        };
        if current.lifecycle != BrowserTabLifecycle::Creating {
            // 创建结果可能晚于激活成功到达。此时 Ready/Suspended/Closed 是
            // 更新的权威状态，迟到的失败不能覆盖它。
            return Ok(None);
        }
        authority
            .transition_tab(tab_id, BrowserTabLifecycle::Crashed, UtcMillis::now())
            .map(Some)
    });
    match outcome {
        Ok(Some(_)) => {
            tracing::warn!(%tab_id, %reason, "浏览器逻辑 Tab 创建失败，保留为 crashed 状态");
            publish_browser_event(
                state,
                "browser.tab.updated",
                workspace_id.as_ref(),
                session_id,
                serde_json::json!({
                    "browser_session_id": browser_session_id,
                    "tab_id": tab_id,
                    "lifecycle": "crashed",
                    "surface_id": null,
                    "reason": reason,
                }),
            );
        }
        Ok(None) => {
            tracing::debug!(%tab_id, %reason, "忽略已收敛的浏览器逻辑 Tab 创建失败结果");
        }
        Err(error) => {
            tracing::error!(%tab_id, ?error, "浏览器逻辑 Tab 创建失败后无法收敛为 crashed 状态");
        }
    }
}
