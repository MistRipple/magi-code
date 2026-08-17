use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header::USER_AGENT},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use magi_browser_authority::{
    BrowserAnnotation, BrowserAnnotationAnchor, BrowserAnnotationAuthor, BrowserAnnotationKind,
    BrowserAnnotationStatus, BrowserAuthority, BrowserElementAnnotationAnchor, BrowserHostCommand,
    BrowserHostCommandOutcome, BrowserHostCommandResult, BrowserHostControl,
    BrowserHostControlUpdate, BrowserHostHitTest, BrowserHostRect, BrowserHostStatus,
    BrowserNavigation, BrowserNormalizedRect, BrowserRegionAnnotationAnchor, BrowserSession,
    BrowserSessionLifecycle, BrowserSurfaceControlSnapshot, BrowserTab, BrowserTabLifecycle,
    BrowserViewport, CreateBrowserSession, CreateBrowserTab, validate_browser_navigation_url,
};
use magi_core::{
    BrowserAnnotationId, BrowserProfileId, BrowserSessionId, BrowserTabId, EventId, SessionId,
    SessionLifecycleStatus, UtcMillis, WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    errors::ApiError,
    routes::session_scope::{self, SessionScope},
    state::{ApiState, BrowserHostConnectionConfig, BrowserHostStatusSnapshot},
};

static BROWSER_SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static BROWSER_TAB_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static BROWSER_ANNOTATION_ARTIFACT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const BROWSER_READY_WAIT_TIMEOUT: Duration = Duration::from_secs(65);
const BROWSER_READY_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/browser/capabilities", get(capabilities))
        .route("/browser/settings", post(update_browser_settings))
        .route(
            "/browser/desktop/connection",
            get(get_desktop_connection)
                .post(register_desktop_connection)
                .delete(clear_desktop_connection),
        )
        .route("/browser/sessions", post(create_session))
        .route("/browser/sessions/current", get(get_current_session))
        .route("/browser/sessions/{browser_session_id}", get(get_session))
        .route(
            "/browser/sessions/{browser_session_id}",
            delete(close_session),
        )
        .route(
            "/browser/sessions/{browser_session_id}/tabs",
            post(create_tab),
        )
        .route("/browser/tabs/{tab_id}", delete(close_tab))
        .route("/browser/tabs/{tab_id}/activate", post(activate_tab))
        .route("/browser/tabs/{tab_id}/navigation", post(navigate_tab))
        .route("/browser/tabs/{tab_id}/screenshot", post(screenshot_tab))
        .route(
            "/browser/tabs/{tab_id}/annotations",
            get(list_annotations).post(create_annotation),
        )
        .route(
            "/browser/annotations/{annotation_id}/status",
            post(update_annotation_status),
        )
        .route(
            "/browser/annotations/{annotation_id}",
            post(update_annotation_comment),
        )
        .route(
            "/browser/annotations/{annotation_id}/artifact",
            get(annotation_artifact),
        )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DesktopConnectionRequest {
    socket_path: String,
    auth_token: String,
    desktop_epoch: String,
    parent_pid: u32,
    expected_generation: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DesktopConnectionClearRequest {
    desktop_epoch: String,
    parent_pid: u32,
    generation: u64,
}

async fn get_desktop_connection(
    State(state): State<ApiState>,
) -> Json<BrowserCapabilitiesResponse> {
    Json(browser_capabilities_response(
        &state,
        None,
        BrowserClientPlatform::Desktop,
    ))
}

async fn register_desktop_connection(
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
        })?;
    if changed {
        let previous = state.browser_host_status();
        state.set_browser_host_status(BrowserHostStatusSnapshot {
            revision: previous.revision,
            in_app_browser_enabled: previous.in_app_browser_enabled,
            browser_use_enabled: previous.browser_use_enabled,
            status: BrowserHostStatus::Starting,
            protocol_compatible: false,
            last_error_code: None,
        });
    }
    Ok(Json(browser_capabilities_response(
        &state,
        None,
        BrowserClientPlatform::Desktop,
    )))
}

async fn clear_desktop_connection(
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
        state.set_browser_host_status(BrowserHostStatusSnapshot {
            revision: previous.revision,
            in_app_browser_enabled: previous.in_app_browser_enabled,
            browser_use_enabled: previous.browser_use_enabled,
            status: BrowserHostStatus::Stopped,
            protocol_compatible: false,
            last_error_code: None,
        });
    }
    Ok(Json(browser_capabilities_response(
        &state,
        None,
        BrowserClientPlatform::Desktop,
    )))
}

fn bounded_connection_value(value: String, field: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 4096 {
        return Err(ApiError::InvalidInput(format!("{field} 无效")));
    }
    Ok(value.to_string())
}

fn browser_annotation_artifact_path(
    state: &ApiState,
    artifact_id: &str,
) -> Result<Option<std::path::PathBuf>, ApiError> {
    let relative = std::path::Path::new(artifact_id);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(ApiError::Conflict("浏览器 artifact 引用无效".to_string()));
    }
    let Some(state_root) = state
        .runtime_persistence()
        .and_then(|persistence| persistence.state_root())
    else {
        return Ok(None);
    };
    let artifact_root = state_root.join("browser/artifacts");
    let path = artifact_root.join(relative);
    let canonical_root = std::fs::canonicalize(&artifact_root)
        .map_err(|error| ApiError::internal_assembly("读取浏览器 artifact 根目录失败", error))?;
    let canonical_path = std::fs::canonicalize(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ApiError::NotFound("浏览器标记截图 artifact 不存在".to_string())
        } else {
            ApiError::internal_assembly("解析浏览器标记截图 artifact 失败", error)
        }
    })?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(ApiError::Conflict("浏览器 artifact 引用越界".to_string()));
    }
    let metadata = std::fs::metadata(&canonical_path)
        .map_err(|error| ApiError::internal_assembly("读取浏览器标记截图元数据失败", error))?;
    if !metadata.is_file() {
        return Err(ApiError::NotFound(
            "浏览器标记截图 artifact 不是文件".to_string(),
        ));
    }
    Ok(Some(canonical_path))
}

/// 将用户提交的浏览器标记 ID 解析为当前 session 范围内的权威上下文。
///
/// 前端只提交 ID，页面 URL、锚点、评论和截图 artifact 均从 BrowserAuthority
/// 读取，避免把客户端坐标或伪造的页面状态写入模型上下文。
pub(crate) fn resolve_browser_annotation_context(
    state: &ApiState,
    session_id: &SessionId,
    requested_ids: &[String],
) -> Result<Vec<Value>, ApiError> {
    const MAX_BROWSER_ANNOTATIONS: usize = 20;
    let ids = requested_ids
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if ids.len() > MAX_BROWSER_ANNOTATIONS {
        return Err(ApiError::InvalidInput(format!(
            "单轮最多添加 {MAX_BROWSER_ANNOTATIONS} 个浏览器标记"
        )));
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let authority = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned");
    let browser_session = authority
        .session_for_magi_session(session_id)
        .ok_or_else(|| ApiError::Conflict("当前会话没有可用的浏览器上下文".to_string()))?;
    let mut resolved = Vec::with_capacity(ids.len());
    for id in ids {
        let annotation_id = BrowserAnnotationId::new(id);
        let annotation = authority
            .annotation(&annotation_id)
            .ok_or_else(|| ApiError::NotFound(format!("浏览器标记不存在: {id}")))?;
        if annotation.browser_session_id != browser_session.browser_session_id
            || !browser_session
                .tab_ids
                .iter()
                .any(|tab_id| tab_id == &annotation.tab_id)
        {
            return Err(ApiError::Conflict(format!(
                "浏览器标记不属于当前会话: {id}"
            )));
        }
        if matches!(
            annotation.status,
            BrowserAnnotationStatus::Resolved | BrowserAnnotationStatus::Deleted
        ) {
            return Err(ApiError::Conflict(format!(
                "浏览器标记已失效，不能作为本轮上下文: {id}"
            )));
        }
        let anchor = serde_json::to_value(&annotation.anchor)
            .map_err(|error| ApiError::internal_assembly("序列化浏览器标记锚点失败", error))?;
        let screenshot_path = annotation
            .screenshot_artifact_id
            .as_deref()
            .map(|artifact_id| browser_annotation_artifact_path(state, artifact_id))
            .transpose()?
            .flatten()
            .map(|path| path.to_string_lossy().into_owned());
        resolved.push(serde_json::json!({
            "sequence": annotation.sequence,
            "annotationId": annotation.annotation_id,
            "browserSessionId": annotation.browser_session_id,
            "tabId": annotation.tab_id,
            "kind": annotation.kind,
            "comment": annotation.comment,
            "anchor": anchor,
            "screenshotArtifactId": annotation.screenshot_artifact_id,
            "screenshotPath": screenshot_path,
            "status": annotation.status,
            "resolvedAtRevision": authority.revision(),
        }));
    }
    Ok(resolved)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BrowserCapabilitiesQuery {
    session_id: Option<String>,
    client_platform: Option<BrowserClientPlatform>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum BrowserClientPlatform {
    Desktop,
    #[default]
    Web,
    #[serde(rename = "mobile-web")]
    MobileWeb,
}

impl BrowserClientPlatform {
    fn label(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Web => "web",
            Self::MobileWeb => "mobile-web",
        }
    }

    fn is_desktop(self) -> bool {
        matches!(self, Self::Desktop)
    }
}

fn request_client_platform(
    headers: &HeaderMap,
    declared: Option<BrowserClientPlatform>,
) -> BrowserClientPlatform {
    if let Some(platform) = declared {
        return platform;
    }
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if user_agent.contains("Electron/") {
        BrowserClientPlatform::Desktop
    } else if user_agent
        .as_bytes()
        .windows(6)
        .any(|window| window.eq_ignore_ascii_case(b"mobile"))
    {
        BrowserClientPlatform::MobileWeb
    } else {
        BrowserClientPlatform::Web
    }
}

fn require_desktop_browser_capability(
    headers: &HeaderMap,
    declared: Option<BrowserClientPlatform>,
) -> Result<(), ApiError> {
    let platform = request_client_platform(headers, declared);
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
struct BrowserPlatformCapabilities {
    desktop_browser_surface: bool,
    browser_records: bool,
    browser_annotations: bool,
    browser_remote_surface: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserCapabilitiesResponse {
    revision: u64,
    in_app_browser_enabled: bool,
    browser_use_enabled: bool,
    host_status: magi_browser_authority::BrowserHostStatus,
    host_protocol_compatible: bool,
    access_profile: magi_core::AccessProfile,
    host_state: String,
    last_error_code: Option<String>,
    desktop_connection_generation: u64,
    platform_capabilities: BrowserPlatformCapabilities,
}

async fn capabilities(
    State(state): State<ApiState>,
    Query(query): Query<BrowserCapabilitiesQuery>,
) -> Json<BrowserCapabilitiesResponse> {
    let session_id = query.session_id.as_deref().map(SessionId::new);
    Json(browser_capabilities_response(
        &state,
        session_id.as_ref(),
        query.client_platform.unwrap_or_default(),
    ))
}

fn browser_capabilities_response(
    state: &ApiState,
    session_id: Option<&SessionId>,
    client_platform: BrowserClientPlatform,
) -> BrowserCapabilitiesResponse {
    let host = state.browser_host_status();
    let capability = state.browser_capability_snapshot(session_id);
    BrowserCapabilitiesResponse {
        revision: host.revision,
        in_app_browser_enabled: capability.in_app_browser_enabled,
        browser_use_enabled: capability.browser_use_enabled,
        host_status: host.status,
        host_protocol_compatible: host.protocol_compatible,
        access_profile: capability.access_profile,
        host_state: serde_json::to_value(host.status)
            .ok()
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "stopped".to_string()),
        last_error_code: host.last_error_code,
        desktop_connection_generation: state.browser_host_connection_generation(),
        platform_capabilities: browser_platform_capabilities(client_platform),
    }
}

fn browser_platform_capabilities(
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
struct UpdateBrowserSettingsRequest {
    in_app_browser_enabled: bool,
    browser_use_enabled: bool,
    client_platform: Option<BrowserClientPlatform>,
}

async fn update_browser_settings(
    State(state): State<ApiState>,
    Json(request): Json<UpdateBrowserSettingsRequest>,
) -> Result<Json<BrowserCapabilitiesResponse>, ApiError> {
    state.update_browser_capability_settings(
        request.in_app_browser_enabled,
        request.browser_use_enabled,
    )?;
    let response =
        browser_capabilities_response(&state, None, request.client_platform.unwrap_or_default());
    state.event_bus.publish(EventEnvelope::system(
        EventId::new(format!(
            "event-browser-settings-updated-{}",
            UtcMillis::now().0
        )),
        "browser.settings.updated",
        serde_json::json!({
            "in_app_browser_enabled": response.in_app_browser_enabled,
            "browser_use_enabled": response.browser_use_enabled,
            "revision": response.revision,
        }),
    ));
    Ok(Json(response))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateBrowserSessionRequest {
    scope: crate::dto::SessionScopeKindDto,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    session_id: String,
    #[serde(default)]
    client_platform: Option<BrowserClientPlatform>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CurrentBrowserSessionQuery {
    scope: crate::dto::SessionScopeKindDto,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AnnotationArtifactQuery {
    session_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CurrentBrowserSessionResponse {
    session: Option<BrowserSessionResponse>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserSessionResponse {
    browser_session_id: BrowserSessionId,
    workspace_id: Option<WorkspaceId>,
    session_id: SessionId,
    profile_id: BrowserProfileId,
    lifecycle: BrowserSessionLifecycle,
    tabs: Vec<BrowserTabResponse>,
    runtime_epoch: u64,
    revision: u64,
    control_mode: String,
    control_fence: u64,
    agent_occupied: bool,
    created_at: UtcMillis,
    updated_at: UtcMillis,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserTabResponse {
    tab_id: BrowserTabId,
    browser_session_id: BrowserSessionId,
    lifecycle: BrowserTabLifecycle,
    url: String,
    origin: Option<String>,
    title: String,
    navigation_revision: u64,
    snapshot_revision: u64,
    created_at: UtcMillis,
    updated_at: UtcMillis,
    annotations: Vec<BrowserAnnotationResponse>,
    #[serde(default)]
    surface_id: Option<String>,
    #[serde(default)]
    agent_occupied: bool,
    #[serde(default)]
    control_fence: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserViewportResponse {
    width: u32,
    height: u32,
    device_scale_factor_millis: u32,
    device_type: magi_browser_authority::BrowserDeviceType,
}

impl From<BrowserViewport> for BrowserViewportResponse {
    fn from(viewport: BrowserViewport) -> Self {
        Self {
            width: viewport.width,
            height: viewport.height,
            device_scale_factor_millis: viewport.device_scale_factor_millis,
            device_type: viewport.device_type,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserAnnotationResponse {
    annotation_id: BrowserAnnotationId,
    browser_session_id: BrowserSessionId,
    tab_id: BrowserTabId,
    sequence: u64,
    author: BrowserAnnotationAuthor,
    kind: BrowserAnnotationKind,
    anchor: BrowserAnnotationAnchorResponse,
    comment: String,
    status: BrowserAnnotationStatus,
    screenshot_artifact_id: Option<String>,
    created_at: UtcMillis,
    updated_at: UtcMillis,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum BrowserAnnotationAnchorResponse {
    Element(Box<BrowserElementAnnotationAnchorResponse>),
    Region(BrowserRegionAnnotationAnchorResponse),
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserElementAnnotationAnchorResponse {
    url: String,
    origin: Option<String>,
    frame_path: Vec<String>,
    viewport: BrowserViewportResponse,
    scroll_x: f64,
    scroll_y: f64,
    test_id: Option<String>,
    stable_id: Option<String>,
    aria_role: Option<String>,
    aria_name: Option<String>,
    tag_name: String,
    text_excerpt: Option<String>,
    css_path: String,
    ancestor_fingerprint: String,
    dom_fingerprint: String,
    bounding_box: BrowserNormalizedRect,
    snapshot_revision: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserRegionAnnotationAnchorResponse {
    url: String,
    origin: Option<String>,
    viewport: BrowserViewportResponse,
    scroll_x: f64,
    scroll_y: f64,
    rect: BrowserNormalizedRect,
    snapshot_revision: u64,
}

impl From<BrowserAnnotationAnchor> for BrowserAnnotationAnchorResponse {
    fn from(anchor: BrowserAnnotationAnchor) -> Self {
        match anchor {
            BrowserAnnotationAnchor::Element(anchor) => {
                let BrowserElementAnnotationAnchor {
                    url,
                    origin,
                    frame_path,
                    viewport,
                    scroll_x,
                    scroll_y,
                    test_id,
                    stable_id,
                    aria_role,
                    aria_name,
                    tag_name,
                    text_excerpt,
                    css_path,
                    ancestor_fingerprint,
                    dom_fingerprint,
                    bounding_box,
                    snapshot_revision,
                } = *anchor;
                Self::Element(Box::new(BrowserElementAnnotationAnchorResponse {
                    url,
                    origin,
                    frame_path,
                    viewport: viewport.into(),
                    scroll_x,
                    scroll_y,
                    test_id,
                    stable_id,
                    aria_role,
                    aria_name,
                    tag_name,
                    text_excerpt,
                    css_path,
                    ancestor_fingerprint,
                    dom_fingerprint,
                    bounding_box,
                    snapshot_revision,
                }))
            }
            BrowserAnnotationAnchor::Region(anchor) => {
                let BrowserRegionAnnotationAnchor {
                    url,
                    origin,
                    viewport,
                    scroll_x,
                    scroll_y,
                    rect,
                    snapshot_revision,
                } = anchor;
                Self::Region(BrowserRegionAnnotationAnchorResponse {
                    url,
                    origin,
                    viewport: viewport.into(),
                    scroll_x,
                    scroll_y,
                    rect,
                    snapshot_revision,
                })
            }
        }
    }
}

impl From<BrowserAnnotation> for BrowserAnnotationResponse {
    fn from(annotation: BrowserAnnotation) -> Self {
        Self {
            annotation_id: annotation.annotation_id,
            browser_session_id: annotation.browser_session_id,
            tab_id: annotation.tab_id,
            sequence: annotation.sequence,
            author: annotation.author,
            kind: annotation.kind,
            anchor: annotation.anchor.into(),
            comment: annotation.comment,
            status: annotation.status,
            screenshot_artifact_id: annotation.screenshot_artifact_id,
            created_at: annotation.created_at,
            updated_at: annotation.updated_at,
        }
    }
}

impl From<BrowserTab> for BrowserTabResponse {
    fn from(tab: BrowserTab) -> Self {
        Self {
            tab_id: tab.tab_id,
            browser_session_id: tab.browser_session_id,
            lifecycle: tab.lifecycle,
            url: tab.url,
            origin: tab.origin,
            title: tab.title,
            navigation_revision: tab.navigation_revision,
            snapshot_revision: tab.snapshot_revision,
            created_at: tab.created_at,
            updated_at: tab.updated_at,
            annotations: Vec::new(),
            surface_id: None,
            agent_occupied: false,
            control_fence: 0,
        }
    }
}

async fn create_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateBrowserSessionRequest>,
) -> Result<(StatusCode, Json<BrowserSessionResponse>), ApiError> {
    require_desktop_browser_capability(&headers, request.client_platform)?;
    let (scope, session_id) = validate_session_scope(
        &state,
        request.scope,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        request.session_id.trim(),
    )?;
    let workspace_id = scope.workspace_id();
    ensure_browser_ui_ready(&state, &session_id)?;
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
            closed.workspace_id.as_ref(),
            &closed.session_id,
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
            workspace_id: workspace_id.clone(),
            session_id: session_id.clone(),
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

async fn get_session(
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

async fn get_current_session(
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
        .filter(|session| session.workspace_id == workspace_id)
        .cloned();
    let session = session
        .map(|session| browser_session_response(&state, session))
        .transpose()?;
    Ok(Json(CurrentBrowserSessionResponse { session }))
}

async fn close_session(
    State(state): State<ApiState>,
    Path(browser_session_id): Path<String>,
) -> Result<StatusCode, ApiError> {
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
        closed.workspace_id.as_ref(),
        &closed.session_id,
        serde_json::json!({
            "browser_session_id": closed.browser_session_id,
            "lifecycle": closed.lifecycle,
            "revision": closed.revision,
        }),
    );
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateBrowserTabRequest {
    initial_url: String,
    #[serde(default)]
    client_platform: Option<BrowserClientPlatform>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateBrowserAnnotationRequest {
    selection: BrowserAnnotationSelectionRequest,
    comment: String,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum BrowserAnnotationSelectionRequest {
    Element {
        navigation_revision: u64,
        x: f64,
        y: f64,
    },
    Region {
        navigation_revision: u64,
        rect: BrowserNormalizedRect,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateBrowserAnnotationStatusRequest {
    status: BrowserAnnotationStatus,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateBrowserAnnotationCommentRequest {
    comment: String,
}

async fn create_tab(
    State(state): State<ApiState>,
    Path(browser_session_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CreateBrowserTabRequest>,
) -> Result<(StatusCode, Json<BrowserTabResponse>), ApiError> {
    require_desktop_browser_capability(&headers, request.client_platform)?;
    let browser_session_id = BrowserSessionId::new(browser_session_id);
    let session = wait_for_browser_session_ready(&state, &browser_session_id).await?;
    validate_navigation_url(&request.initial_url)?;
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
    // 浏览器页面由 Electron Main 持有的 WebContentsView 承载，并且只绑定
    // 当前桌面右栏 Browser Tab 的内容槽。先发布逻辑 Tab，再要求 Host 物化
    // 真实 Chromium 页面，使统一 App Renderer 可以立即渲染加载状态。
    publish_browser_event(
        &state,
        "browser.tab.created",
        session.workspace_id.as_ref(),
        &session.session_id,
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
    let workspace_id = session.workspace_id.clone();
    let session_id = session.session_id.clone();
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

fn finish_browser_tab_creation(
    state: &ApiState,
    workspace_id: &Option<WorkspaceId>,
    session_id: &SessionId,
    browser_session_id: &BrowserSessionId,
    tab_id: &BrowserTabId,
    reason: &str,
) {
    if let Err(error) = state.mutate_browser_authority(|authority| {
        authority.transition_tab(tab_id, BrowserTabLifecycle::Crashed, UtcMillis::now())
    }) {
        tracing::error!(%tab_id, ?error, "浏览器逻辑 Tab 创建失败后无法收敛为 crashed 状态");
    }
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

async fn list_annotations(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
) -> Result<Json<Vec<BrowserAnnotationResponse>>, ApiError> {
    let tab_id = BrowserTabId::new(tab_id);
    let (_tab, session) = browser_tab_scope(&state, &tab_id)?;
    ensure_browser_ui_ready(&state, &session.session_id)?;
    let annotations = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .annotations_for_tab(&tab_id)
        .into_iter()
        .map(BrowserAnnotationResponse::from)
        .collect();
    Ok(Json(annotations))
}

async fn create_annotation(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
    Json(request): Json<CreateBrowserAnnotationRequest>,
) -> Result<(StatusCode, Json<BrowserAnnotationResponse>), ApiError> {
    let tab_id = BrowserTabId::new(tab_id);
    let (_tab_before_lock, session) = browser_tab_scope(&state, &tab_id)?;
    let comment = request.comment.trim().to_string();
    if comment.is_empty() || comment.chars().count() > 4_000 {
        return Err(ApiError::InvalidInput(
            "标记内容不能为空且不能超过 4000 个字符".to_string(),
        ));
    }
    let _control_guard = state.browser_control_lock.lock().await;
    ensure_user_control_for_ui_locked(&state, &session, &tab_id).await?;
    // 获得控制锁后重新读取权威 Tab；请求等待期间可能已完成面板调整或导航。
    let (tab, session) = browser_tab_scope(&state, &tab_id)?;
    let (kind, navigation_revision, hit_x, hit_y, region) = match request.selection {
        BrowserAnnotationSelectionRequest::Element {
            navigation_revision,
            x,
            y,
        } => {
            validate_annotation_point(x, y)?;
            (
                BrowserAnnotationKind::Element,
                navigation_revision,
                x,
                y,
                None,
            )
        }
        BrowserAnnotationSelectionRequest::Region {
            navigation_revision,
            rect,
        } => {
            validate_annotation_rect(rect)?;
            let x = rect.x + rect.width / 2.0;
            let y = rect.y + rect.height / 2.0;
            (
                BrowserAnnotationKind::Region,
                navigation_revision,
                x,
                y,
                Some(rect),
            )
        }
    };
    state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .validate_navigation_ref(&session.browser_session_id, &tab_id, navigation_revision)
        .map_err(|error| ApiError::Conflict(error.to_string()))?;
    let hit = browser_hit_test(&state, &tab_id, navigation_revision, hit_x, hit_y).await?;
    let hit_viewport = BrowserViewport {
        width: hit.viewport_width,
        height: hit.viewport_height,
        device_scale_factor_millis: 1_000,
        device_type: magi_browser_authority::BrowserDeviceType::for_dimensions(hit.viewport_width),
    };
    let anchor = match region {
        Some(rect) => BrowserAnnotationAnchor::Region(BrowserRegionAnnotationAnchor {
            url: tab.url.clone(),
            origin: tab.origin.clone(),
            viewport: hit_viewport,
            scroll_x: hit.scroll_x,
            scroll_y: hit.scroll_y,
            rect,
            snapshot_revision: tab.snapshot_revision,
        }),
        None => BrowserAnnotationAnchor::Element(Box::new(BrowserElementAnnotationAnchor {
            url: tab.url.clone(),
            origin: tab.origin.clone(),
            frame_path: Vec::new(),
            viewport: hit_viewport,
            scroll_x: hit.scroll_x,
            scroll_y: hit.scroll_y,
            test_id: hit.test_id,
            stable_id: hit.stable_id,
            aria_role: hit.aria_role,
            aria_name: hit.aria_name,
            tag_name: hit.tag_name,
            text_excerpt: hit.text_excerpt,
            css_path: hit.css_path,
            ancestor_fingerprint: hit.ancestor_fingerprint,
            dom_fingerprint: hit.dom_fingerprint,
            bounding_box: normalize_hit_bounds(
                hit.viewport_width,
                hit.viewport_height,
                hit.bounds,
            )?,
            snapshot_revision: tab.snapshot_revision,
        })),
    };
    let screenshot_clip = match &anchor {
        BrowserAnnotationAnchor::Element(anchor) => anchor.bounding_box,
        BrowserAnnotationAnchor::Region(anchor) => anchor.rect,
    };
    let screenshot_artifact_id = persist_browser_annotation_screenshot(
        &state,
        &session.session_id,
        &tab.tab_id,
        screenshot_clip,
        &tab_id,
    )
    .await?;
    let now = UtcMillis::now();
    let annotation = state.mutate_browser_authority(|authority| {
        authority.validate_navigation_ref(
            &session.browser_session_id,
            &tab_id,
            navigation_revision,
        )?;
        authority.create_annotation(BrowserAnnotation {
            annotation_id: BrowserAnnotationId::new(format!(
                "browser-annotation-{}-{}",
                now.0,
                BROWSER_TAB_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            )),
            browser_session_id: session.browser_session_id.clone(),
            tab_id: tab.tab_id.clone(),
            sequence: 0,
            author: BrowserAnnotationAuthor::User,
            kind,
            anchor,
            comment,
            status: BrowserAnnotationStatus::Active,
            screenshot_artifact_id: Some(screenshot_artifact_id),
            created_at: now,
            updated_at: now,
        })
    })?;
    publish_browser_event(
        &state,
        "browser.annotation.created",
        session.workspace_id.as_ref(),
        &session.session_id,
        serde_json::json!({
            "annotation_id": annotation.annotation_id,
            "tab_id": annotation.tab_id,
            "status": annotation.status,
        }),
    );
    Ok((
        StatusCode::CREATED,
        Json(BrowserAnnotationResponse::from(annotation)),
    ))
}

async fn persist_browser_annotation_screenshot(
    state: &ApiState,
    session_id: &SessionId,
    tab_id: &BrowserTabId,
    clip: BrowserNormalizedRect,
    host_tab_id: &BrowserTabId,
) -> Result<String, ApiError> {
    let reply = require_browser_host(state)?
        .request(BrowserHostCommand::Screenshot {
            tab_id: host_tab_id.clone(),
            target: None,
            clip: Some(clip),
            full_page: false,
            format: magi_browser_authority::BrowserScreenshotFormat::Png,
            quality: None,
        })
        .await
        .map_err(|error| ApiError::model_invocation_failed("保存浏览器标记截图失败", error))?;
    let metadata = match reply.response.outcome {
        BrowserHostCommandOutcome::Succeeded(result) => match *result {
            BrowserHostCommandResult::BinaryPayload(metadata) => metadata,
            result => {
                return Err(host_outcome_error(
                    "保存浏览器标记截图失败",
                    BrowserHostCommandOutcome::Succeeded(Box::new(result)),
                ));
            }
        },
        outcome => return Err(host_outcome_error("保存浏览器标记截图失败", outcome)),
    };
    let bytes = reply.binary.ok_or_else(|| {
        ApiError::InternalAssemblyError("浏览器标记截图缺少二进制内容".to_string())
    })?;
    let Some(persistence) = state.runtime_persistence() else {
        return Err(ApiError::Conflict("浏览器标记截图存储不可用".to_string()));
    };
    let Some(state_root) = persistence.state_root() else {
        return Err(ApiError::Conflict(
            "浏览器标记截图存储根目录不可用".to_string(),
        ));
    };
    let filename = format!(
        "annotation-shot-{}-{}-{}.png",
        UtcMillis::now().0,
        tab_id,
        BROWSER_ANNOTATION_ARTIFACT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let relative_id = format!("{}/{}", session_id, filename);
    let path = state_root.join("browser/artifacts").join(&relative_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| ApiError::internal_assembly("创建浏览器标记截图目录失败", error))?;
    }
    magi_core::fs_atomic::write_atomic(&path, bytes)
        .map_err(|error| ApiError::internal_assembly("写入浏览器标记截图失败", error))?;
    tracing::debug!(
        session_id = %session_id,
        tab_id = %tab_id,
        mime = %metadata.mime_type,
        artifact_id = %relative_id,
        "已持久化浏览器标记截图 artifact"
    );
    Ok(relative_id)
}

async fn browser_hit_test(
    state: &ApiState,
    tab_id: &BrowserTabId,
    navigation_revision: u64,
    x: f64,
    y: f64,
) -> Result<BrowserHostHitTest, ApiError> {
    let result = require_host_success(
        require_browser_host(state)?
            .request(BrowserHostCommand::HitTest {
                tab_id: tab_id.clone(),
                navigation_revision,
                x,
                y,
            })
            .await,
        "页面标记命中检测失败",
    )?;
    match result {
        BrowserHostCommandResult::HitTest(hit) => Ok(hit),
        _ => Err(ApiError::InternalAssemblyError(
            "浏览器 Host 命中检测结果无效".to_string(),
        )),
    }
}

fn validate_annotation_point(x: f64, y: f64) -> Result<(), ApiError> {
    if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 || x > 1.0 || y > 1.0 {
        return Err(ApiError::InvalidInput(
            "页面标记坐标超出当前浏览器视口".to_string(),
        ));
    }
    Ok(())
}

fn validate_annotation_rect(rect: BrowserNormalizedRect) -> Result<(), ApiError> {
    let values = [rect.x, rect.y, rect.width, rect.height];
    if values.iter().any(|value| !value.is_finite())
        || rect.x < 0.0
        || rect.y < 0.0
        || rect.width <= 0.0
        || rect.height <= 0.0
        || rect.x + rect.width > 1.0
        || rect.y + rect.height > 1.0
    {
        return Err(ApiError::InvalidInput(
            "页面区域标记必须位于当前浏览器视口内".to_string(),
        ));
    }
    Ok(())
}

fn normalize_hit_bounds(
    viewport_width: u32,
    viewport_height: u32,
    bounds: BrowserHostRect,
) -> Result<BrowserNormalizedRect, ApiError> {
    let width = f64::from(viewport_width);
    let height = f64::from(viewport_height);
    if width <= 0.0
        || height <= 0.0
        || !bounds.x.is_finite()
        || !bounds.y.is_finite()
        || !bounds.width.is_finite()
        || !bounds.height.is_finite()
    {
        return Err(ApiError::InvalidInput("页面元素标记边界无效".to_string()));
    }
    let left = (bounds.x / width).clamp(0.0, 1.0);
    let top = (bounds.y / height).clamp(0.0, 1.0);
    let right = ((bounds.x + bounds.width) / width).clamp(0.0, 1.0);
    let bottom = ((bounds.y + bounds.height) / height).clamp(0.0, 1.0);
    if right <= left || bottom <= top {
        return Err(ApiError::InvalidInput(
            "页面元素当前不在可见视口中".to_string(),
        ));
    }
    Ok(BrowserNormalizedRect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

async fn update_annotation_status(
    State(state): State<ApiState>,
    Path(annotation_id): Path<String>,
    Json(request): Json<UpdateBrowserAnnotationStatusRequest>,
) -> Result<Json<BrowserAnnotationResponse>, ApiError> {
    let annotation_id = BrowserAnnotationId::new(annotation_id);
    let annotation = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .annotation(&annotation_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("浏览器标记不存在", annotation_id.as_str()))?;
    let (_tab, session) = browser_tab_scope(&state, &annotation.tab_id)?;
    let _control_guard = state.browser_control_lock.lock().await;
    ensure_user_control_for_ui_locked(&state, &session, &annotation.tab_id).await?;
    let updated = state.mutate_browser_authority(|authority| {
        authority.update_annotation_status(&annotation_id, request.status, UtcMillis::now())
    })?;
    publish_browser_event(
        &state,
        "browser.annotation.status_changed",
        session.workspace_id.as_ref(),
        &session.session_id,
        serde_json::json!({
            "annotation_id": updated.annotation_id,
            "tab_id": updated.tab_id,
            "status": updated.status,
        }),
    );
    Ok(Json(BrowserAnnotationResponse::from(updated)))
}

async fn update_annotation_comment(
    State(state): State<ApiState>,
    Path(annotation_id): Path<String>,
    Json(request): Json<UpdateBrowserAnnotationCommentRequest>,
) -> Result<Json<BrowserAnnotationResponse>, ApiError> {
    let annotation_id = BrowserAnnotationId::new(annotation_id);
    let comment = request.comment.trim().to_string();
    if comment.is_empty() || comment.chars().count() > 4_000 {
        return Err(ApiError::InvalidInput(
            "标记内容不能为空且不能超过 4000 个字符".to_string(),
        ));
    }
    let annotation = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .annotation(&annotation_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("浏览器标记不存在", annotation_id.as_str()))?;
    let (_tab, session) = browser_tab_scope(&state, &annotation.tab_id)?;
    let _control_guard = state.browser_control_lock.lock().await;
    ensure_user_control_for_ui_locked(&state, &session, &annotation.tab_id).await?;
    let updated = state.mutate_browser_authority(|authority| {
        authority.update_annotation_comment(&annotation_id, comment, UtcMillis::now())
    })?;
    publish_browser_event(
        &state,
        "browser.annotation.updated",
        session.workspace_id.as_ref(),
        &session.session_id,
        serde_json::json!({
            "annotation_id": updated.annotation_id,
            "tab_id": updated.tab_id,
        }),
    );
    Ok(Json(BrowserAnnotationResponse::from(updated)))
}

async fn annotation_artifact(
    State(state): State<ApiState>,
    Path(annotation_id): Path<String>,
    Query(query): Query<AnnotationArtifactQuery>,
) -> Result<Response, ApiError> {
    let annotation_id = BrowserAnnotationId::new(annotation_id);
    let session_id = SessionId::new(query.session_id.trim());
    let artifact_id = {
        let authority = state
            .browser_authority
            .lock()
            .expect("browser authority lock poisoned");
        let annotation = authority
            .annotation(&annotation_id)
            .ok_or_else(|| ApiError::not_found("浏览器标记不存在", annotation_id.as_str()))?;
        let browser_session = authority
            .session_for_magi_session(&session_id)
            .ok_or_else(|| ApiError::not_found("浏览器会话不存在", session_id.as_str()))?;
        if annotation.browser_session_id != browser_session.browser_session_id
            || !browser_session
                .tab_ids
                .iter()
                .any(|tab_id| tab_id == &annotation.tab_id)
        {
            return Err(ApiError::NotFound("浏览器标记不存在".to_string()));
        }
        annotation
            .screenshot_artifact_id
            .clone()
            .ok_or_else(|| ApiError::NotFound("浏览器标记没有截图 artifact".to_string()))?
    };
    let path = browser_annotation_artifact_path(&state, &artifact_id)?
        .ok_or_else(|| ApiError::Conflict("浏览器 artifact 存储不可用".to_string()))?;
    let bytes = std::fs::read(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ApiError::NotFound("浏览器标记截图 artifact 不存在".to_string())
        } else {
            ApiError::internal_assembly("读取浏览器标记截图 artifact 失败", error)
        }
    })?;
    Ok((
        StatusCode::OK,
        [
            ("content-type", "image/png"),
            ("cache-control", "private, max-age=60"),
        ],
        bytes,
    )
        .into_response())
}

async fn activate_tab(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
) -> Result<Json<BrowserSessionResponse>, ApiError> {
    let tab_id = BrowserTabId::new(tab_id);
    let (tab, session) = browser_tab_scope(&state, &tab_id)?;
    ensure_browser_ui_ready(&state, &session.session_id)?;
    if !matches!(
        tab.lifecycle,
        BrowserTabLifecycle::Creating
            | BrowserTabLifecycle::Ready
            | BrowserTabLifecycle::Suspended
            | BrowserTabLifecycle::Crashed
    ) {
        return Err(ApiError::Conflict("浏览器 Tab 当前不可激活".to_string()));
    }
    let tab = if tab.lifecycle == BrowserTabLifecycle::Crashed {
        state.mutate_browser_authority(|authority| {
            authority.transition_tab(&tab_id, BrowserTabLifecycle::Suspended, UtcMillis::now())
        })?
    } else {
        tab
    };
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
    let session = state.mutate_browser_authority(|authority| {
        if authority.tab(&tab_id).is_some_and(|tab| {
            matches!(
                tab.lifecycle,
                BrowserTabLifecycle::Creating
                    | BrowserTabLifecycle::Suspended
                    | BrowserTabLifecycle::Crashed
            )
        }) {
            authority.transition_tab(&tab_id, BrowserTabLifecycle::Ready, UtcMillis::now())?;
        }
        authority.apply_host_page_state(
            &tab_id,
            page_state.navigation_revision,
            page_state.url,
            page_state.origin,
            page_state.title,
            UtcMillis::now(),
        )?;
        authority
            .session(&tab.browser_session_id)
            .cloned()
            .ok_or_else(|| {
                magi_browser_authority::BrowserAuthorityError::UnknownSession(
                    tab.browser_session_id.clone(),
                )
            })
    })?;
    publish_browser_event(
        &state,
        "browser.tab.activated",
        session.workspace_id.as_ref(),
        &session.session_id,
        serde_json::json!({
            "browser_session_id": session.browser_session_id,
            "tab_id": tab_id,
            "revision": session.revision,
        }),
    );
    Ok(Json(browser_session_response(&state, session)?))
}

async fn close_tab(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let tab_id = BrowserTabId::new(tab_id);
    let (tab, session) = browser_tab_scope(&state, &tab_id)?;
    let _control_guard = state.browser_control_lock.lock().await;
    if state.browser_host_client().is_some()
        && let Err(error) = ensure_user_control_for_ui_locked(&state, &session, &tab_id).await
    {
        tracing::warn!(tab_id = %tab_id, ?error, "关闭浏览器 Tab 时同步用户控制权失败，继续收口逻辑状态");
    }
    state.mutate_browser_authority(|authority| {
        authority.transition_tab(&tab_id, BrowserTabLifecycle::Closed, UtcMillis::now())
    })?;
    if let Some(client) = state.browser_host_client() {
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
                "关闭浏览器 Tab 时 Host 返回失败，逻辑状态已经关闭"
            ),
            Err(error) => tracing::warn!(
                tab_id = %tab_id,
                ?error,
                "关闭浏览器 Tab 时 Host 不可用，逻辑状态已经关闭"
            ),
        }
    }
    publish_browser_event(
        &state,
        "browser.tab.closed",
        session.workspace_id.as_ref(),
        &session.session_id,
        serde_json::json!({
            "browser_session_id": tab.browser_session_id,
            "tab_id": tab_id,
        }),
    );
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NavigateBrowserTabRequest {
    action: String,
    url: Option<String>,
    #[serde(default)]
    client_platform: Option<BrowserClientPlatform>,
}

async fn navigate_tab(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<NavigateBrowserTabRequest>,
) -> Result<Json<BrowserTabResponse>, ApiError> {
    require_desktop_browser_capability(&headers, request.client_platform)?;
    let tab_id = BrowserTabId::new(tab_id);
    let (_, session) = browser_tab_scope(&state, &tab_id)?;
    let _control_guard = state.browser_control_lock.lock().await;
    ensure_browser_ui_ready(&state, &session.session_id)?;
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
            validate_navigation_url(&url)?;
            BrowserNavigation::Url {
                url,
                handle_before_unload: None,
                init_script: None,
                timeout_ms: None,
            }
        }
        "back" => BrowserNavigation::Back { timeout_ms: None },
        "forward" => BrowserNavigation::Forward { timeout_ms: None },
        "reload" => BrowserNavigation::Reload {
            ignore_cache: false,
            handle_before_unload: None,
            timeout_ms: None,
        },
        _ => {
            return Err(ApiError::InvalidInput(
                "action 必须是 url、back、forward 或 reload".to_string(),
            ));
        }
    };
    let fence = ensure_user_control_for_ui_locked(&state, &session, &tab_id)
        .await?
        .fence;
    let reply = require_host_success(
        require_browser_host(&state)?
            .request(BrowserHostCommand::Navigate {
                tab_id: tab_id.clone(),
                control: BrowserHostControl::User { fence },
                navigation,
            })
            .await,
        "浏览器导航失败",
    )?;
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
    publish_browser_event(
        &state,
        "browser.tab.updated",
        session.workspace_id.as_ref(),
        &session.session_id,
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

fn validate_navigation_url(url: &str) -> Result<(), ApiError> {
    validate_browser_navigation_url(url)
        .map_err(|error| ApiError::InvalidInput(format!("浏览器导航 URL 不合法: {error}")))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScreenshotBrowserTabRequest {
    #[serde(default)]
    full_page: bool,
    #[serde(default)]
    client_platform: Option<BrowserClientPlatform>,
}

async fn screenshot_tab(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ScreenshotBrowserTabRequest>,
) -> Result<Response, ApiError> {
    require_desktop_browser_capability(&headers, request.client_platform)?;
    let tab_id = BrowserTabId::new(tab_id);
    let (_tab, session) = browser_tab_scope(&state, &tab_id)?;
    ensure_browser_ui_ready(&state, &session.session_id)?;
    let reply = require_browser_host(&state)?
        .request(BrowserHostCommand::Screenshot {
            tab_id,
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

fn browser_tab_scope(
    state: &ApiState,
    tab_id: &BrowserTabId,
) -> Result<(BrowserTab, BrowserSession), ApiError> {
    let authority = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned");
    let tab = authority
        .tab(tab_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("浏览器 Tab 不存在", tab_id.as_str()))?;
    let session = authority
        .session(&tab.browser_session_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("浏览器会话不存在", tab.browser_session_id.as_str()))?;
    Ok((tab, session))
}

fn require_browser_host(
    state: &ApiState,
) -> Result<magi_browser_authority::BrowserHostClient, ApiError> {
    state
        .browser_host_client()
        .ok_or_else(|| ApiError::Conflict("桌面浏览器控制通道尚未启动".to_string()))
}

async fn ensure_user_control_for_ui_locked(
    state: &ApiState,
    session: &BrowserSession,
    tab_id: &BrowserTabId,
) -> Result<BrowserSurfaceControlSnapshot, ApiError> {
    ensure_browser_ui_ready(state, &session.session_id)?;
    let (control, revoked) = state.mutate_browser_authority(|authority| {
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
    })?;
    if !revoked.is_empty() {
        if let Err(error) = super::sessions::interrupt_session_turn_for_browser_takeover(
            state,
            &session.session_id,
            session.workspace_id.as_ref(),
        )
        .await
        {
            tracing::warn!(
                session_id = %session.session_id,
                ?error,
                "浏览器用户操作后收口当前 Turn 失败，控制 fence 仍保持生效"
            );
        }
        if let Some(client) = state.browser_host_client() {
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
            session.workspace_id.as_ref(),
            &session.session_id,
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

fn require_host_success(
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

fn host_outcome_error(context: &str, outcome: BrowserHostCommandOutcome) -> ApiError {
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

fn validate_session_scope(
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

fn ensure_browser_ui_ready(state: &ApiState, session_id: &SessionId) -> Result<(), ApiError> {
    let capability = state.browser_capability_snapshot(Some(session_id));
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

async fn wait_for_magi_browser_session(
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

async fn wait_for_browser_session_ready(
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

fn browser_session_response(
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
    Ok(BrowserSessionResponse {
        browser_session_id: session.browser_session_id,
        workspace_id: session.workspace_id,
        session_id: session.session_id,
        profile_id: session.profile_id,
        lifecycle: session.lifecycle,
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

fn browser_tab_response_from_authority(
    authority: &BrowserAuthority,
    tab: BrowserTab,
) -> BrowserTabResponse {
    let mut response = BrowserTabResponse::from(tab.clone());
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

fn host_command_succeeded(outcome: &BrowserHostCommandOutcome) -> bool {
    matches!(outcome, BrowserHostCommandOutcome::Succeeded(_))
}

fn publish_browser_event(
    state: &ApiState,
    event_type: &str,
    workspace_id: Option<&WorkspaceId>,
    session_id: &SessionId,
    payload: serde_json::Value,
) {
    let now = UtcMillis::now();
    state.event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-{}-{}", event_type.replace('.', "-"), now.0)),
            event_type,
            payload,
        )
        .with_context(EventContext {
            workspace_id: workspace_id.cloned(),
            session_id: Some(session_id.clone()),
            ..EventContext::default()
        }),
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::{HeaderMap, header::USER_AGENT};
    use magi_browser_authority::{
        BrowserAnnotation, BrowserAnnotationAnchor, BrowserAnnotationAuthor, BrowserAnnotationKind,
        BrowserAnnotationStatus, BrowserProfile, BrowserProfileKind, BrowserRegionAnnotationAnchor,
        BrowserSessionLifecycle, BrowserTabLifecycle, BrowserViewport, CreateBrowserSession,
        CreateBrowserTab,
    };
    use magi_core::{
        BrowserAnnotationId, BrowserProfileId, BrowserSessionId, BrowserTabId, SessionId,
        UtcMillis, WorkspaceId,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;

    use super::{
        BrowserAnnotationAnchorResponse, BrowserClientPlatform,
        BrowserElementAnnotationAnchorResponse, BrowserRegionAnnotationAnchorResponse,
        DesktopConnectionClearRequest, DesktopConnectionRequest, browser_platform_capabilities,
        browser_session_response, clear_desktop_connection, finish_browser_tab_creation,
        register_desktop_connection, require_desktop_browser_capability,
        resolve_browser_annotation_context,
    };
    use crate::{
        errors::ApiError,
        state::{ApiState, BrowserHostConnectionConfig},
    };

    fn annotation_fixture() -> (ApiState, SessionId, SessionId, BrowserAnnotationId) {
        let state = ApiState::new(
            "browser-annotation-context-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::new()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let current_session_id = SessionId::new("session-browser-annotation-current");
        let other_session_id = SessionId::new("session-browser-annotation-other");
        let browser_session_id = BrowserSessionId::new("browser-session-annotation-current");
        let tab_id = BrowserTabId::new("browser-tab-annotation-current");
        let annotation_id = BrowserAnnotationId::new("browser-annotation-current");
        let profile_id = BrowserProfileId::new("browser-profile-annotation-context");
        state
            .mutate_browser_authority(|authority| {
                authority.register_profile(BrowserProfile {
                    profile_id: profile_id.clone(),
                    kind: BrowserProfileKind::ManagedDefault,
                    data_path: tempfile::tempdir()
                        .expect("annotation profile fixture should create")
                        .keep(),
                    created_at: UtcMillis(1),
                    updated_at: UtcMillis(1),
                })?;
                authority.create_session(CreateBrowserSession {
                    browser_session_id: browser_session_id.clone(),
                    workspace_id: Some(WorkspaceId::new("workspace-browser-annotation-current")),
                    session_id: current_session_id.clone(),
                    profile_id,
                    now: UtcMillis(1),
                })?;
                authority.transition_session(
                    &browser_session_id,
                    BrowserSessionLifecycle::Ready,
                    UtcMillis(1),
                )?;
                authority.create_tab(CreateBrowserTab {
                    tab_id: tab_id.clone(),
                    browser_session_id: browser_session_id.clone(),
                    url: "https://example.com/settings".to_string(),
                    now: UtcMillis(1),
                })?;
                authority.transition_tab(&tab_id, BrowserTabLifecycle::Ready, UtcMillis(1))?;
                authority.apply_host_page_state(
                    &tab_id,
                    3,
                    "https://example.com/settings".to_string(),
                    Some("https://example.com".to_string()),
                    "Settings".to_string(),
                    UtcMillis(2),
                )?;
                authority.create_annotation(BrowserAnnotation {
                    annotation_id: annotation_id.clone(),
                    browser_session_id,
                    tab_id,
                    sequence: 0,
                    author: BrowserAnnotationAuthor::User,
                    kind: BrowserAnnotationKind::Region,
                    anchor: BrowserAnnotationAnchor::Region(BrowserRegionAnnotationAnchor {
                        url: "https://example.com/settings".to_string(),
                        origin: Some("https://example.com".to_string()),
                        viewport: BrowserViewport::default(),
                        scroll_x: 0.0,
                        scroll_y: 120.0,
                        rect: magi_browser_authority::BrowserNormalizedRect {
                            x: 0.1,
                            y: 0.2,
                            width: 0.3,
                            height: 0.15,
                        },
                        snapshot_revision: 1,
                    }),
                    comment: "检查保存按钮".to_string(),
                    status: BrowserAnnotationStatus::Active,
                    screenshot_artifact_id: Some(
                        "session-browser-annotation-current/annotation.png".to_string(),
                    ),
                    created_at: UtcMillis(2),
                    updated_at: UtcMillis(2),
                })?;
                Ok(())
            })
            .expect("annotation authority fixture should create");
        (state, current_session_id, other_session_id, annotation_id)
    }

    #[tokio::test]
    async fn stale_desktop_process_cannot_clear_the_current_connection() {
        let state = ApiState::new(
            "browser-desktop-connection-owner-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::new()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        state.set_browser_host_connection_config(Some(BrowserHostConnectionConfig {
            socket_path: "/tmp/magi-current.sock".to_string(),
            auth_token: "current-token".to_string(),
            desktop_epoch: "desktop-current".to_string(),
            parent_pid: 200,
            generation: 3,
        }));

        let _ = clear_desktop_connection(
            axum::extract::State(state.clone()),
            axum::Json(DesktopConnectionClearRequest {
                desktop_epoch: "desktop-stale".to_string(),
                parent_pid: 100,
                generation: 3,
            }),
        )
        .await
        .expect("stale unregister should be an idempotent no-op");
        let current = state
            .browser_host_connection_config()
            .expect("current desktop connection should remain registered");
        assert_eq!(current.desktop_epoch, "desktop-current");
        assert_eq!(current.parent_pid, 200);

        let _ = clear_desktop_connection(
            axum::extract::State(state.clone()),
            axum::Json(DesktopConnectionClearRequest {
                desktop_epoch: "desktop-current".to_string(),
                parent_pid: 200,
                generation: 3,
            }),
        )
        .await
        .expect("current desktop should unregister itself");
        assert!(state.browser_host_connection_config().is_none());
    }

    #[tokio::test]
    async fn desktop_connection_registration_requires_generation_cas() {
        let state = ApiState::new(
            "browser-desktop-connection-cas-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::new()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );

        let first = register_desktop_connection(
            axum::extract::State(state.clone()),
            axum::Json(DesktopConnectionRequest {
                socket_path: "/tmp/magi-first.sock".to_string(),
                auth_token: "first-token".to_string(),
                desktop_epoch: "desktop-first".to_string(),
                parent_pid: 101,
                expected_generation: 0,
            }),
        )
        .await
        .expect("first desktop registration should succeed");
        assert_eq!(first.0.desktop_connection_generation, 1);

        let stale = register_desktop_connection(
            axum::extract::State(state.clone()),
            axum::Json(DesktopConnectionRequest {
                socket_path: "/tmp/magi-stale.sock".to_string(),
                auth_token: "stale-token".to_string(),
                desktop_epoch: "desktop-stale".to_string(),
                parent_pid: 102,
                expected_generation: 0,
            }),
        )
        .await;
        assert!(matches!(stale, Err(ApiError::Conflict(_))));

        let idempotent = register_desktop_connection(
            axum::extract::State(state.clone()),
            axum::Json(DesktopConnectionRequest {
                socket_path: "/tmp/magi-first.sock".to_string(),
                auth_token: "first-token".to_string(),
                desktop_epoch: "desktop-first".to_string(),
                parent_pid: 101,
                expected_generation: 1,
            }),
        )
        .await
        .expect("same owner registration should be idempotent");
        assert_eq!(idempotent.0.desktop_connection_generation, 1);

        let replacement = register_desktop_connection(
            axum::extract::State(state.clone()),
            axum::Json(DesktopConnectionRequest {
                socket_path: "/tmp/magi-second.sock".to_string(),
                auth_token: "second-token".to_string(),
                desktop_epoch: "desktop-second".to_string(),
                parent_pid: 103,
                expected_generation: 1,
            }),
        )
        .await
        .expect("CAS replacement should succeed");
        assert_eq!(replacement.0.desktop_connection_generation, 2);

        let _ = clear_desktop_connection(
            axum::extract::State(state.clone()),
            axum::Json(DesktopConnectionClearRequest {
                desktop_epoch: "desktop-first".to_string(),
                parent_pid: 101,
                generation: 1,
            }),
        )
        .await
        .expect("stale clear should be an idempotent no-op");
        let current = state
            .browser_host_connection_config()
            .expect("replacement owner should remain registered");
        assert_eq!(current.desktop_epoch, "desktop-second");
        assert_eq!(current.generation, 2);
    }

    #[test]
    fn annotation_context_is_session_scoped_and_keeps_stale_artifact_context() {
        let (state, session_id, other_session_id, annotation_id) = annotation_fixture();
        let resolved =
            resolve_browser_annotation_context(&state, &session_id, &[annotation_id.to_string()])
                .expect("active annotation should resolve");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0]["annotationId"], annotation_id.to_string());
        assert_eq!(resolved[0]["sequence"], 1);
        assert_eq!(resolved[0]["comment"], "检查保存按钮");
        assert_eq!(resolved[0]["anchor"]["url"], "https://example.com/settings");
        assert_eq!(
            resolved[0]["screenshotArtifactId"],
            "session-browser-annotation-current/annotation.png"
        );

        assert!(matches!(
            resolve_browser_annotation_context(
                &state,
                &other_session_id,
                &[annotation_id.to_string()]
            ),
            Err(ApiError::Conflict(_))
        ));

        state
            .mutate_browser_authority(|authority| {
                authority.update_annotation_status(
                    &annotation_id,
                    BrowserAnnotationStatus::Stale,
                    UtcMillis(3),
                )
            })
            .expect("annotation should become stale");
        let stale =
            resolve_browser_annotation_context(&state, &session_id, &[annotation_id.to_string()])
                .expect("stale annotation artifact should remain usable as historical context");
        assert_eq!(stale[0]["status"], "stale");

        state
            .mutate_browser_authority(|authority| {
                authority.update_annotation_status(
                    &annotation_id,
                    BrowserAnnotationStatus::Resolved,
                    UtcMillis(4),
                )
            })
            .expect("annotation should resolve");
        assert!(matches!(
            resolve_browser_annotation_context(&state, &session_id, &[annotation_id.to_string()]),
            Err(ApiError::Conflict(_))
        ));
    }

    #[test]
    fn annotation_anchor_response_serializes_variant_fields_as_camel_case() {
        let value = serde_json::to_value(BrowserAnnotationAnchorResponse::Region(
            BrowserRegionAnnotationAnchorResponse {
                url: "https://example.com/settings".to_string(),
                origin: Some("https://example.com".to_string()),
                viewport: BrowserViewport::default().into(),
                scroll_x: 12.0,
                scroll_y: 24.0,
                rect: magi_browser_authority::BrowserNormalizedRect {
                    x: 0.1,
                    y: 0.2,
                    width: 0.3,
                    height: 0.4,
                },
                snapshot_revision: 7,
            },
        ))
        .expect("annotation anchor response should serialize");

        assert_eq!(value["kind"], "region");
        assert_eq!(value["scrollX"], 12.0);
        assert_eq!(value["scrollY"], 24.0);
        assert_eq!(value["snapshotRevision"], 7);
        assert!(value.get("rect").is_some());
        assert!(value.get("scroll_x").is_none());
        assert!(value.get("scroll_y").is_none());
        assert!(value.get("snapshot_revision").is_none());

        let element = serde_json::to_value(BrowserAnnotationAnchorResponse::Element(Box::new(
            BrowserElementAnnotationAnchorResponse {
                url: "https://example.com/settings".to_string(),
                origin: Some("https://example.com".to_string()),
                frame_path: Vec::new(),
                viewport: BrowserViewport::default().into(),
                scroll_x: 0.0,
                scroll_y: 0.0,
                test_id: None,
                stable_id: None,
                aria_role: Some("button".to_string()),
                aria_name: Some("Save".to_string()),
                tag_name: "button".to_string(),
                text_excerpt: Some("Save".to_string()),
                css_path: "button.save".to_string(),
                ancestor_fingerprint: "ancestor".to_string(),
                dom_fingerprint: "dom".to_string(),
                bounding_box: magi_browser_authority::BrowserNormalizedRect {
                    x: 0.1,
                    y: 0.2,
                    width: 0.3,
                    height: 0.4,
                },
                snapshot_revision: 8,
            },
        )))
        .expect("element annotation anchor response should serialize");

        assert!(element.get("boundingBox").is_some());
        assert_eq!(element["snapshotRevision"], 8);
        assert!(element.get("bounding_box").is_none());
        assert!(element.get("snapshot_revision").is_none());
    }

    #[test]
    fn browser_tab_response_preserves_primary_surface_after_navigation() {
        let state = ApiState::new(
            "browser-tab-response-surface-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::new()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let profile_id = BrowserProfileId::new("browser-profile-response-surface");
        let browser_session_id = BrowserSessionId::new("browser-session-response-surface");
        let tab_id = BrowserTabId::new("browser-tab-response-surface");
        let session_id = SessionId::new("session-response-surface");

        state
            .mutate_browser_authority(|authority| {
                authority.register_profile(BrowserProfile {
                    profile_id: profile_id.clone(),
                    kind: BrowserProfileKind::ManagedDefault,
                    data_path: tempfile::tempdir()
                        .expect("surface response profile should create")
                        .keep(),
                    created_at: UtcMillis(1),
                    updated_at: UtcMillis(1),
                })?;
                authority.create_session(CreateBrowserSession {
                    browser_session_id: browser_session_id.clone(),
                    workspace_id: None,
                    session_id,
                    profile_id,
                    now: UtcMillis(1),
                })?;
                authority.transition_session(
                    &browser_session_id,
                    BrowserSessionLifecycle::Ready,
                    UtcMillis(2),
                )?;
                authority.create_tab(CreateBrowserTab {
                    tab_id: tab_id.clone(),
                    browser_session_id: browser_session_id.clone(),
                    url: "https://example.com/old".to_string(),
                    now: UtcMillis(2),
                })?;
                authority.transition_tab(&tab_id, BrowserTabLifecycle::Ready, UtcMillis(2))?;
                authority.set_primary_surface(
                    magi_browser_authority::BrowserSurfaceBinding {
                        desktop_epoch: "desktop-response".to_string(),
                        window_id: "window-response".to_string(),
                        surface_id: "surface-response".to_string(),
                        surface_revision: 3,
                        tab_id: tab_id.clone(),
                        web_contents_id: 23,
                        target_id: "target-response".to_string(),
                        browser_context_id: "context-response".to_string(),
                        navigation_revision: 0,
                    },
                    UtcMillis(3),
                )?;
                authority.apply_host_page_state(
                    &tab_id,
                    1,
                    "https://example.com/new".to_string(),
                    Some("https://example.com".to_string()),
                    "New page".to_string(),
                    UtcMillis(4),
                )?;
                Ok(())
            })
            .expect("surface response fixture should create");

        let session = state
            .browser_authority
            .lock()
            .expect("browser authority lock should hold")
            .session(&browser_session_id)
            .cloned()
            .expect("browser session should exist");
        let response = browser_session_response(&state, session)
            .expect("browser session response should assemble");
        assert_eq!(response.tabs.len(), 1);
        assert_eq!(
            response.tabs[0].surface_id.as_deref(),
            Some("surface-response")
        );
        assert_eq!(response.tabs[0].url, "https://example.com/new");
    }

    #[test]
    fn platform_capabilities_are_explicit_for_each_client() {
        let desktop = serde_json::to_value(browser_platform_capabilities(
            BrowserClientPlatform::Desktop,
        ))
        .expect("desktop capabilities should serialize");
        assert_eq!(desktop["desktopBrowserSurface"], true);
        assert_eq!(desktop["browserRecords"], true);
        assert_eq!(desktop["browserAnnotations"], true);
        assert_eq!(desktop["browserRemoteSurface"], false);

        let mobile = serde_json::to_value(browser_platform_capabilities(
            BrowserClientPlatform::MobileWeb,
        ))
        .expect("mobile web capabilities should serialize");
        assert_eq!(mobile["desktopBrowserSurface"], false);
        assert_eq!(mobile["browserRecords"], true);
        assert_eq!(mobile["browserAnnotations"], true);
        assert_eq!(mobile["browserRemoteSurface"], false);
    }

    #[test]
    fn real_browser_operations_are_unavailable_to_web_clients() {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, "Mozilla/5.0".parse().unwrap());
        let error = require_desktop_browser_capability(&headers, None)
            .expect_err("web clients must not operate the Desktop browser host");
        assert!(matches!(error, ApiError::CapabilityUnavailable { .. }));

        headers.insert(USER_AGENT, "Mozilla/5.0 Electron/40.0".parse().unwrap());
        require_desktop_browser_capability(&headers, None)
            .expect("Electron clients should be able to operate the Desktop browser host");
    }

    #[test]
    fn explicit_non_desktop_platform_cannot_be_overridden_by_a_desktop_user_agent() {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, "Mozilla/5.0 Electron/40.0".parse().unwrap());
        let error =
            require_desktop_browser_capability(&headers, Some(BrowserClientPlatform::MobileWeb))
                .expect_err("explicit mobile platform must remain record-only");
        assert!(matches!(error, ApiError::CapabilityUnavailable { .. }));
    }

    #[test]
    fn failed_host_materialization_keeps_logical_tab_recoverable() {
        let state = ApiState::new(
            "browser-tab-materialization-failure-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::new()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let session_id = SessionId::new("session-browser-tab-materialization-failure");
        let browser_session_id = BrowserSessionId::new("browser-session-materialization-failure");
        let tab_id = BrowserTabId::new("browser-tab-materialization-failure");
        let profile_id = BrowserProfileId::new("browser-profile-materialization-failure");

        state
            .mutate_browser_authority(|authority| {
                authority.register_profile(BrowserProfile {
                    profile_id: profile_id.clone(),
                    kind: BrowserProfileKind::ManagedDefault,
                    data_path: tempfile::tempdir()
                        .expect("materialization failure profile should create")
                        .keep(),
                    created_at: UtcMillis(1),
                    updated_at: UtcMillis(1),
                })?;
                authority.create_session(CreateBrowserSession {
                    browser_session_id: browser_session_id.clone(),
                    workspace_id: None,
                    session_id: session_id.clone(),
                    profile_id,
                    now: UtcMillis(1),
                })?;
                authority.transition_session(
                    &browser_session_id,
                    BrowserSessionLifecycle::Ready,
                    UtcMillis(2),
                )?;
                authority.create_tab(CreateBrowserTab {
                    tab_id: tab_id.clone(),
                    browser_session_id: browser_session_id.clone(),
                    url: "https://example.com".to_string(),
                    now: UtcMillis(2),
                })?;
                Ok(())
            })
            .expect("creating browser tab fixture should succeed");

        finish_browser_tab_creation(
            &state,
            &None,
            &session_id,
            &browser_session_id,
            &tab_id,
            "surface unavailable",
        );

        let tab = state
            .browser_authority
            .lock()
            .expect("browser authority lock should hold")
            .tab(&tab_id)
            .cloned()
            .expect("failed tab should remain in authority");
        assert_eq!(tab.lifecycle, BrowserTabLifecycle::Crashed);
    }
}
