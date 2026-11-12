use super::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct DesktopConnectionRequest {
    pub(super) socket_path: String,
    pub(super) auth_token: String,
    pub(super) desktop_epoch: String,
    pub(super) parent_pid: u32,
    pub(super) expected_generation: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct DesktopConnectionClearRequest {
    pub(super) desktop_epoch: String,
    pub(super) parent_pid: u32,
    pub(super) generation: u64,
}

pub(super) async fn get_desktop_connection(
    State(state): State<ApiState>,
) -> Json<BrowserCapabilitiesResponse> {
    Json(browser_capabilities_response(
        &state,
        BrowserClientPlatform::Desktop,
    ))
}

pub(super) async fn register_desktop_connection(
    State(state): State<ApiState>,
    Json(request): Json<DesktopConnectionRequest>,
) -> Result<Json<BrowserCapabilitiesResponse>, ApiError> {
    let socket_path = bounded_connection_value(request.socket_path, "socketPath")?;
    let auth_token = bounded_connection_value(request.auth_token, "authToken")?;
    let desktop_epoch = bounded_connection_value(request.desktop_epoch, "desktopEpoch")?;
    if request.parent_pid == 0 {
        return Err(ApiError::InvalidInput("parentPid 必须是正整数".to_string()));
    }
    let candidate = BrowserHostConnectionConfig {
        socket_path,
        auth_token,
        desktop_epoch,
        parent_pid: request.parent_pid,
        generation: 0,
    };
    let (_, changed) = state
        .register_browser_host_connection(candidate, request.expected_generation)
        .map_err(|error| match error {
            crate::state::BrowserHostConnectionRegistrationError::Conflict {
                current_generation,
            } => ApiError::Conflict(format!(
                "桌面浏览器连接所有权冲突，当前代次为 {current_generation}"
            )),
            crate::state::BrowserHostConnectionRegistrationError::OwnerConflict {
                current_generation,
            } => ApiError::Conflict(format!(
                "当前 daemon 已绑定另一 Desktop 实例，当前代次为 {current_generation}"
            )),
        })?;
    if changed {
        let previous = state.browser_host_status();
        let status = BrowserHostStatusSnapshot {
            revision: previous.revision,
            settings: previous.settings,
            status: BrowserHostStatus::Starting,
            protocol_compatible: false,
            last_error_code: None,
        };
        state.set_browser_host_status(status.clone());
        publish_browser_host_status(&state, &status);
    }
    Ok(Json(browser_capabilities_response(
        &state,
        BrowserClientPlatform::Desktop,
    )))
}

pub(super) async fn clear_desktop_connection(
    State(state): State<ApiState>,
    Json(request): Json<DesktopConnectionClearRequest>,
) -> Result<Json<BrowserCapabilitiesResponse>, ApiError> {
    let desktop_epoch = bounded_connection_value(request.desktop_epoch, "desktopEpoch")?;
    if request.parent_pid == 0 {
        return Err(ApiError::InvalidInput("parentPid 必须是正整数".to_string()));
    }
    let current = state.browser_host_connection_config();
    if current.as_ref().is_some_and(|config| {
        config.desktop_epoch == desktop_epoch
            && config.parent_pid == request.parent_pid
            && config.generation == request.generation
    }) {
        state.set_browser_host_connection_config(None);
        let previous = state.browser_host_status();
        let status = BrowserHostStatusSnapshot {
            revision: previous.revision,
            settings: previous.settings,
            status: BrowserHostStatus::Stopped,
            protocol_compatible: false,
            last_error_code: None,
        };
        state.set_browser_host_status(status.clone());
        publish_browser_host_status(&state, &status);
    }
    Ok(Json(browser_capabilities_response(
        &state,
        BrowserClientPlatform::Desktop,
    )))
}

pub(super) fn bounded_connection_value(value: String, field: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 4096 {
        return Err(ApiError::InvalidInput(format!("{field} 无效")));
    }
    Ok(value.to_string())
}

pub(super) fn publish_browser_host_status(state: &ApiState, status: &BrowserHostStatusSnapshot) {
    state.event_bus.publish(EventEnvelope::system(
        EventId::unique(format!("event-browser-host-status-{:?}", status.status)),
        "browser.host.status_changed",
        serde_json::json!({
            "host_status": status.status,
            "protocol_compatible": status.protocol_compatible,
            "error_code": status.last_error_code,
            "revision": status.revision,
        }),
    ));
}
