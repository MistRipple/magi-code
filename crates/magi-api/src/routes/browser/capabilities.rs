use super::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct BrowserCapabilitiesQuery {
    pub(super) client_platform: Option<BrowserClientPlatform>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum BrowserClientPlatform {
    Desktop,
    #[default]
    Web,
    #[serde(rename = "mobile-web")]
    MobileWeb,
}

impl BrowserClientPlatform {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Web => "web",
            Self::MobileWeb => "mobile-web",
        }
    }

    pub(super) fn is_desktop(self) -> bool {
        matches!(self, Self::Desktop)
    }
}

pub(super) fn request_client_platform(
    state: &ApiState,
    headers: &HeaderMap,
    declared: Option<BrowserClientPlatform>,
) -> BrowserClientPlatform {
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let observed = if super::super::is_trusted_desktop_renderer_request(state, headers) {
        BrowserClientPlatform::Desktop
    } else if user_agent
        .as_bytes()
        .windows(6)
        .any(|window| window.eq_ignore_ascii_case(b"mobile"))
    {
        BrowserClientPlatform::MobileWeb
    } else {
        BrowserClientPlatform::Web
    };
    match (observed, declared) {
        // 客户端可以主动降级能力，但不能把服务端观测到的 Web 提升为 Desktop。
        (BrowserClientPlatform::Desktop, Some(declared)) => declared,
        (_, Some(BrowserClientPlatform::Desktop)) => observed,
        (_, Some(declared)) => declared,
        (_, None) => observed,
    }
}

pub(super) fn require_desktop_browser_capability(
    state: &ApiState,
    headers: &HeaderMap,
    declared: Option<BrowserClientPlatform>,
) -> Result<(), ApiError> {
    let platform = request_client_platform(state, headers, declared);
    if platform.is_desktop() {
        return Ok(());
    }
    Err(ApiError::CapabilityUnavailable {
        capability: "desktop_browser_surface".to_string(),
        platform: platform.label().to_string(),
        message: "真实内置浏览器仅在 Magi Desktop 中可用，Web 和移动 Web 端仅支持浏览器记录"
            .to_string(),
    })
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BrowserPlatformCapabilities {
    pub(super) desktop_browser_surface: bool,
    pub(super) browser_records: bool,
    pub(super) browser_annotations: bool,
    pub(super) browser_remote_surface: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BrowserCapabilitiesResponse {
    pub(super) revision: u64,
    pub(super) in_app_browser_enabled: bool,
    pub(super) browser_use_enabled: bool,
    pub(super) devtools_enabled: bool,
    pub(super) lan_access_enabled: bool,
    pub(super) host_status: magi_browser_authority::BrowserHostStatus,
    pub(super) host_protocol_compatible: bool,
    pub(super) host_state: String,
    pub(super) last_error_code: Option<String>,
    pub(super) desktop_connection_generation: u64,
    pub(super) platform_capabilities: BrowserPlatformCapabilities,
}

pub(super) async fn capabilities(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<BrowserCapabilitiesQuery>,
) -> Json<BrowserCapabilitiesResponse> {
    Json(browser_capabilities_response(
        &state,
        request_client_platform(&state, &headers, query.client_platform),
    ))
}

pub(super) fn browser_capabilities_response(
    state: &ApiState,
    client_platform: BrowserClientPlatform,
) -> BrowserCapabilitiesResponse {
    let host = state.browser_host_status();
    let capability = state.browser_capability_snapshot();
    BrowserCapabilitiesResponse {
        revision: host.revision,
        in_app_browser_enabled: capability.in_app_browser_enabled,
        browser_use_enabled: capability.browser_use_enabled,
        devtools_enabled: capability.devtools_enabled,
        lan_access_enabled: capability.lan_access_enabled,
        host_status: host.status,
        host_protocol_compatible: host.protocol_compatible,
        host_state: serde_json::to_value(host.status)
            .ok()
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "stopped".to_string()),
        last_error_code: host.last_error_code,
        desktop_connection_generation: state.browser_host_connection_generation(),
        platform_capabilities: browser_platform_capabilities(client_platform),
    }
}

pub(super) fn browser_platform_capabilities(
    client_platform: BrowserClientPlatform,
) -> BrowserPlatformCapabilities {
    BrowserPlatformCapabilities {
        desktop_browser_surface: matches!(client_platform, BrowserClientPlatform::Desktop),
        browser_records: true,
        browser_annotations: true,
        browser_remote_surface: false,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct UpdateBrowserSettingsRequest {
    pub(super) in_app_browser_enabled: bool,
    pub(super) browser_use_enabled: bool,
    pub(super) devtools_enabled: bool,
    pub(super) lan_access_enabled: bool,
    pub(super) client_platform: Option<BrowserClientPlatform>,
}

pub(super) async fn update_browser_settings(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<UpdateBrowserSettingsRequest>,
) -> Result<Json<BrowserCapabilitiesResponse>, ApiError> {
    require_desktop_browser_capability(&state, &headers, request.client_platform)?;
    // 先让已连接的 Desktop 应用新的网络策略，成功后才落盘；未连接时在下次建立连接时推送。
    if let Some(client) = state.browser_host_client() {
        crate::browser_network_policy::push_network_policy(&client, request.lan_access_enabled)
            .await
            .map_err(|error| {
                ApiError::Conflict(format!("设置未能同步到桌面浏览器，未保存: {error}"))
            })?;
    }
    state.update_browser_capability_settings(crate::state::BrowserCapabilitySettings {
        in_app_browser_enabled: request.in_app_browser_enabled,
        browser_use_enabled: request.browser_use_enabled,
        devtools_enabled: request.devtools_enabled,
        lan_access_enabled: request.lan_access_enabled,
    })?;
    let response = browser_capabilities_response(
        &state,
        request_client_platform(&state, &headers, request.client_platform),
    );
    state.event_bus.publish(EventEnvelope::system(
        EventId::unique("event-browser-settings-updated"),
        "browser.settings.updated",
        serde_json::json!({
            "in_app_browser_enabled": response.in_app_browser_enabled,
            "browser_use_enabled": response.browser_use_enabled,
            "devtools_enabled": response.devtools_enabled,
            "lan_access_enabled": response.lan_access_enabled,
            "revision": response.revision,
        }),
    ));
    Ok(Json(response))
}

/// 返回 daemon 全局共享的逻辑 Browser Tab 资源，而不是当前右侧面板中的页面。
///
/// 页面在 daemon 重启后可能处于 Suspended：没有物理 Chromium Page，但仍然
/// 代表可恢复的历史页面并占用逻辑容量。资源管理入口正是为了让用户看见并
/// 显式回收这类资源，避免“当前没有 Tab 但页面已达上限”的不可解释状态。
pub(super) async fn browser_resources(
    State(state): State<ApiState>,
) -> Result<Json<BrowserResourcesResponse>, ApiError> {
    let reconciled = state.reconcile_browser_sessions_with_session_store()?;
    if reconciled > 0 {
        state.persist_browser_durable_state_for_api()?;
    }
    Ok(Json(browser_resources_response(&state)))
}

pub(super) async fn reclaim_browser_resources(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<ReclaimBrowserResourcesRequest>,
) -> Result<Json<ReclaimBrowserResourcesResponse>, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    let requested_ids = request
        .tab_ids
        .into_iter()
        .map(|tab_id| tab_id.trim().to_string())
        .filter(|tab_id| !tab_id.is_empty())
        .collect::<std::collections::BTreeSet<_>>();
    if requested_ids.is_empty() {
        return Err(ApiError::InvalidInput(
            "至少选择一个要回收的浏览器页面".to_string(),
        ));
    }
    if requested_ids.len() > MAX_BROWSER_TABS_TOTAL {
        return Err(ApiError::InvalidInput(format!(
            "单次最多回收 {MAX_BROWSER_TABS_TOTAL} 个浏览器页面"
        )));
    }

    // 回收属于跨 Host/Authority 的资源操作。先串行化它与页面关闭、激活、
    // 导航等物理控制，避免用户在确认后又把同一 Page 重新物化。
    let requested_ids = requested_ids.into_iter().collect::<Vec<_>>();
    let _control_guards = state
        .browser_control_locks
        .lock_tabs(
            &requested_ids
                .iter()
                .map(|tab_id| BrowserTabId::new(tab_id.as_str()))
                .collect::<Vec<_>>(),
        )
        .await;
    let current_resources = browser_resources_response(&state);
    let mut targets = Vec::new();
    let mut skipped = Vec::new();
    for tab_id in &requested_ids {
        let Some(resource) = current_resources
            .tabs
            .iter()
            .find(|resource| resource.tab_id.as_str() == tab_id)
        else {
            skipped.push(BrowserResourceReclaimSkip {
                tab_id: tab_id.clone(),
                reason: "not_found".to_string(),
            });
            continue;
        };
        if !resource.can_reclaim {
            skipped.push(BrowserResourceReclaimSkip {
                tab_id: tab_id.clone(),
                reason: resource
                    .reclaim_reason
                    .clone()
                    .unwrap_or_else(|| "not_reclaimable".to_string()),
            });
            continue;
        }
        let Some(session_id) = resource.session_id.clone() else {
            continue;
        };
        targets.push(BrowserReclaimTarget {
            tab_id: resource.tab_id.clone(),
            browser_session_id: resource.browser_session_id.clone(),
            session_id,
            workspace_id: resource.workspace_id.clone(),
        });
    }

    // ClosePage 对 Suspended 是幂等清理，对 Crashed 则尽量释放仍被 Host
    // 持有的物理页。即使 Host 已断开，也继续收口逻辑状态，确保容量真正
    // 释放，而不是把用户再次留在 64/64 的死循环里。
    if let Some(client) = state.browser_host_client() {
        for target in &targets {
            match client
                .request(BrowserHostCommand::ClosePage {
                    tab_id: target.tab_id.clone(),
                })
                .await
            {
                Ok(reply) if host_command_succeeded(&reply.response.outcome) => {}
                Ok(reply) => tracing::warn!(
                    tab_id = %target.tab_id,
                    outcome = ?reply.response.outcome,
                    "回收浏览器资源时 Host 未能关闭物理 Page，继续收口逻辑状态"
                ),
                Err(error) => tracing::warn!(
                    tab_id = %target.tab_id,
                    ?error,
                    "回收浏览器资源时 Browser Host 不可用，继续收口逻辑状态"
                ),
            }
        }
    }

    let now = UtcMillis::now();
    let mut reclaimed = Vec::new();
    let mut state_changed = Vec::new();
    state.mutate_browser_authority(|authority| {
        for target in &targets {
            if !authority.is_reclaimable_tab(&target.tab_id, now) {
                state_changed.push(target.tab_id.clone());
                continue;
            }
            authority.transition_tab(&target.tab_id, BrowserTabLifecycle::Closed, now)?;
            reclaimed.push(target.clone());
        }
        Ok(())
    })?;
    for tab_id in state_changed {
        skipped.push(BrowserResourceReclaimSkip {
            tab_id: tab_id.to_string(),
            reason: "state_changed".to_string(),
        });
    }

    for target in &reclaimed {
        publish_browser_event(
            &state,
            "browser.tab.closed",
            target.workspace_id.as_ref(),
            &target.session_id,
            serde_json::json!({
                "browser_session_id": target.browser_session_id,
                "tab_id": target.tab_id,
                "reason": "user_reclaimed_inactive_resource",
            }),
        );
    }

    Ok(Json(ReclaimBrowserResourcesResponse {
        reclaimed_count: reclaimed.len(),
        reclaimed_tab_ids: reclaimed.into_iter().map(|target| target.tab_id).collect(),
        skipped,
        resources: browser_resources_response(&state),
    }))
}

pub(super) fn browser_resources_response(state: &ApiState) -> BrowserResourcesResponse {
    let now = UtcMillis::now();
    let current_session_id = state
        .session_store
        .current_session()
        .map(|session| session.session_id);
    let session_records = state
        .session_store
        .sessions()
        .into_iter()
        .map(|session| (session.session_id.clone(), session))
        .collect::<std::collections::HashMap<_, _>>();
    let authority = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned");
    let live_tabs = authority.live_tab_count();
    let mut tabs = Vec::new();

    for session in authority.snapshot().sessions {
        let session_magi_id = session.magi_session_id().cloned();
        let session_running = session_magi_id
            .as_ref()
            .and_then(|session_id| session_records.get(session_id))
            .and_then(|record| state.session_store.runtime_sidecar(&record.session_id))
            .as_ref()
            .map(|sidecar| session_running_task_count(Some(sidecar)))
            .unwrap_or_default()
            > 0;
        let is_current_session = session_magi_id
            .as_ref()
            .is_some_and(|session_id| current_session_id.as_ref() == Some(session_id));
        let active_tab_id = authority.active_tab(&session.browser_session_id);
        for tab_id in session.tab_ids.clone() {
            let Some(tab) = authority.tab(&tab_id) else {
                continue;
            };
            let is_current_tab = active_tab_id == Some(&tab.tab_id) && is_current_session;
            let lifecycle_reclaimable = matches!(
                tab.lifecycle,
                BrowserTabLifecycle::Suspended | BrowserTabLifecycle::Crashed
            );
            let active_lease = authority.active_lease_for_tab(&tab.tab_id, now).is_some();
            let can_reclaim =
                lifecycle_reclaimable && !active_lease && !session_running && !is_current_tab;
            let reclaim_reason = if can_reclaim {
                None
            } else if !lifecycle_reclaimable {
                Some("active".to_string())
            } else if active_lease {
                Some("agent_occupied".to_string())
            } else if session_running {
                Some("session_running".to_string())
            } else if is_current_tab {
                Some("current_tab".to_string())
            } else {
                Some("not_reclaimable".to_string())
            };
            let session_title = session_magi_id
                .as_ref()
                .and_then(|session_id| session_records.get(session_id))
                .map(|record| record.title.clone());
            tabs.push(BrowserResourceTabResponse {
                tab_id: tab.tab_id.clone(),
                browser_session_id: tab.browser_session_id.clone(),
                session_id: session_magi_id.clone(),
                workspace_id: session.owner_workspace_id().cloned(),
                scope: session.owner_kind().to_string(),
                session_title,
                lifecycle: tab.lifecycle,
                url: tab.url.clone(),
                title: tab.title.clone(),
                created_at: tab.created_at,
                updated_at: tab.updated_at,
                is_current_session,
                is_current_tab,
                session_running,
                can_reclaim,
                reclaim_reason,
            });
        }
    }
    tabs.sort_by(|left, right| {
        right
            .can_reclaim
            .cmp(&left.can_reclaim)
            .then_with(|| left.updated_at.cmp(&right.updated_at))
            .then_with(|| left.tab_id.as_str().cmp(right.tab_id.as_str()))
    });

    BrowserResourcesResponse {
        max_tabs: MAX_BROWSER_TABS_TOTAL,
        live_tabs,
        available_tabs: MAX_BROWSER_TABS_TOTAL.saturating_sub(live_tabs),
        reclaimable_tabs: tabs.iter().filter(|tab| tab.can_reclaim).count(),
        tabs,
    }
}
