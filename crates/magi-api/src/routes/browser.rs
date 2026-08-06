use std::sync::atomic::{AtomicU64, Ordering};

use axum::{
    Json, Router,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use futures_util::{SinkExt, StreamExt};
use magi_browser_runtime::{
    BrowserAnnotation, BrowserAnnotationAnchor, BrowserAnnotationAuthor, BrowserAnnotationKind,
    BrowserAnnotationStatus, BrowserElementAnnotationAnchor, BrowserHostCommand,
    BrowserHostCommandOutcome, BrowserHostCommandResult, BrowserHostControl,
    BrowserHostControlMode, BrowserHostEvent, BrowserHostHitTest, BrowserHostRect,
    BrowserNavigation, BrowserNormalizedRect, BrowserProfileControlMode,
    BrowserProfileControlSnapshot, BrowserRegionAnnotationAnchor, BrowserRuntimeComponentAction,
    BrowserRuntimeUpdateLevel, BrowserScreencastFormat, BrowserSession, BrowserSessionLifecycle,
    BrowserTab, BrowserTabLifecycle, BrowserUserInputEvent, BrowserViewport, BrowserViewportMode,
    CreateBrowserSession, CreateBrowserTab, validate_browser_navigation_url,
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
    state::{ApiState, BrowserScreencastOptions},
};

static BROWSER_SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static BROWSER_TAB_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static BROWSER_ANNOTATION_ARTIFACT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const MAX_BROWSER_CLIPBOARD_TEXT_BYTES: usize = 1024 * 1024;
const BROWSER_SCREENCAST_JPEG_QUALITY: u8 = 90;
const BROWSER_SCREENCAST_MAX_WIDTH: u32 = 7_680;
const BROWSER_SCREENCAST_MAX_HEIGHT: u32 = 4_320;

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/browser/capabilities", get(capabilities))
        .route("/browser/settings", post(update_browser_settings))
        .route(
            "/browser/clipboard/text",
            get(read_browser_clipboard_text).post(write_browser_clipboard_text),
        )
        .route(
            "/browser/runtime/check-updates",
            post(check_browser_runtime_updates),
        )
        .route("/browser/runtime/install", post(install_browser_runtime))
        .route(
            "/browser/runtime/uninstall",
            post(uninstall_browser_runtime),
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
        .route("/browser/tabs/{tab_id}/viewport", post(set_viewport))
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
        .route("/browser/tabs/{tab_id}/channel", get(browser_channel))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BrowserClipboardTextRequest {
    text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserClipboardTextResponse {
    text: String,
}

async fn read_browser_clipboard_text(
    headers: HeaderMap,
) -> Result<Json<BrowserClipboardTextResponse>, ApiError> {
    ensure_local_browser_clipboard_access(&headers)?;
    let text = tokio::task::spawn_blocking(|| {
        let mut clipboard = arboard::Clipboard::new()?;
        clipboard.get_text()
    })
    .await
    .map_err(|error| ApiError::internal_assembly("读取系统剪贴板任务失败", error))?
    .map_err(|error| {
        tracing::warn!(error = %error, "读取系统剪贴板失败");
        ApiError::Conflict("系统剪贴板当前没有可读取的文本".to_string())
    })?;
    if text.len() > MAX_BROWSER_CLIPBOARD_TEXT_BYTES {
        return Err(ApiError::Conflict(
            "系统剪贴板文本超过 1 MiB 限制".to_string(),
        ));
    }
    Ok(Json(BrowserClipboardTextResponse { text }))
}

async fn write_browser_clipboard_text(
    headers: HeaderMap,
    Json(request): Json<BrowserClipboardTextRequest>,
) -> Result<StatusCode, ApiError> {
    ensure_local_browser_clipboard_access(&headers)?;
    if request.text.len() > MAX_BROWSER_CLIPBOARD_TEXT_BYTES {
        return Err(ApiError::InvalidInput(
            "浏览器复制文本不能超过 1 MiB".to_string(),
        ));
    }
    tokio::task::spawn_blocking(move || {
        let mut clipboard = arboard::Clipboard::new()?;
        clipboard.set_text(request.text)
    })
    .await
    .map_err(|error| ApiError::internal_assembly("写入系统剪贴板任务失败", error))?
    .map_err(|error| {
        tracing::warn!(error = %error, "写入系统剪贴板失败");
        ApiError::Conflict("系统剪贴板当前不可写入".to_string())
    })?;
    Ok(StatusCode::NO_CONTENT)
}

fn ensure_local_browser_clipboard_access(headers: &HeaderMap) -> Result<(), ApiError> {
    if super::is_public_tunnel_request(headers) {
        return Err(ApiError::Conflict(
            "远程公网访问不能读写本机剪贴板".to_string(),
        ));
    }
    Ok(())
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
        if annotation.status != BrowserAnnotationStatus::Active {
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
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserCapabilitiesResponse {
    revision: u64,
    in_app_browser_enabled: bool,
    browser_use_enabled: bool,
    runtime_status: magi_browser_runtime::BrowserRuntimeComponentStatus,
    host_protocol_compatible: bool,
    access_profile: magi_core::AccessProfile,
    runtime_mode: String,
    host_status: String,
    runtime_version: Option<String>,
    host_version: Option<String>,
    playwright_version: Option<String>,
    chromium_version: Option<String>,
    available_runtime_version: Option<String>,
    update_level: Option<BrowserRuntimeUpdateLevel>,
    component_management_available: bool,
    last_error_code: Option<String>,
}

async fn capabilities(
    State(state): State<ApiState>,
    Query(query): Query<BrowserCapabilitiesQuery>,
) -> Json<BrowserCapabilitiesResponse> {
    let session_id = query.session_id.as_deref().map(SessionId::new);
    Json(browser_capabilities_response(&state, session_id.as_ref()))
}

fn browser_capabilities_response(
    state: &ApiState,
    session_id: Option<&SessionId>,
) -> BrowserCapabilitiesResponse {
    let capability = state.browser_capability_snapshot(session_id);
    let runtime = state.browser_runtime_status();
    BrowserCapabilitiesResponse {
        revision: capability.revision,
        in_app_browser_enabled: capability.in_app_browser_enabled,
        browser_use_enabled: capability.browser_use_enabled,
        runtime_status: capability.runtime_status,
        host_protocol_compatible: capability.host_protocol_compatible,
        access_profile: capability.access_profile,
        runtime_mode: runtime.runtime_mode,
        host_status: runtime.host_status,
        runtime_version: runtime.runtime_version,
        host_version: runtime.host_version,
        playwright_version: runtime.playwright_version,
        chromium_version: runtime.chromium_version,
        available_runtime_version: runtime.available_runtime_version,
        update_level: runtime.update_level,
        component_management_available: runtime.component_management_available,
        last_error_code: runtime.last_error_code,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateBrowserSettingsRequest {
    in_app_browser_enabled: bool,
    browser_use_enabled: bool,
}

async fn update_browser_settings(
    State(state): State<ApiState>,
    Json(request): Json<UpdateBrowserSettingsRequest>,
) -> Result<Json<BrowserCapabilitiesResponse>, ApiError> {
    state.update_browser_capability_settings(
        request.in_app_browser_enabled,
        request.browser_use_enabled,
    )?;
    let response = browser_capabilities_response(&state, None);
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

async fn check_browser_runtime_updates(
    State(state): State<ApiState>,
) -> Result<Json<BrowserCapabilitiesResponse>, ApiError> {
    run_browser_runtime_action(&state, BrowserRuntimeComponentAction::CheckForUpdates).await
}

async fn install_browser_runtime(
    State(state): State<ApiState>,
) -> Result<Json<BrowserCapabilitiesResponse>, ApiError> {
    run_browser_runtime_action(&state, BrowserRuntimeComponentAction::Install).await
}

async fn uninstall_browser_runtime(
    State(state): State<ApiState>,
) -> Result<Json<BrowserCapabilitiesResponse>, ApiError> {
    run_browser_runtime_action(&state, BrowserRuntimeComponentAction::Uninstall).await
}

async fn run_browser_runtime_action(
    state: &ApiState,
    action: BrowserRuntimeComponentAction,
) -> Result<Json<BrowserCapabilitiesResponse>, ApiError> {
    let control = state
        .browser_runtime_control()
        .ok_or_else(|| ApiError::Conflict("浏览器运行组件管理不可用".to_string()))?;
    control.request(action).await.map_err(ApiError::Conflict)?;
    Ok(Json(browser_capabilities_response(state, None)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateBrowserSessionRequest {
    workspace_id: String,
    session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CurrentBrowserSessionQuery {
    workspace_id: String,
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
    workspace_id: WorkspaceId,
    session_id: SessionId,
    profile_id: BrowserProfileId,
    lifecycle: BrowserSessionLifecycle,
    active_tab_id: Option<BrowserTabId>,
    tabs: Vec<BrowserTabResponse>,
    runtime_epoch: u64,
    revision: u64,
    control_mode: BrowserProfileControlMode,
    control_fence: u64,
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
    viewport: BrowserViewportResponse,
    viewport_mode: BrowserViewportMode,
    navigation_revision: u64,
    snapshot_revision: u64,
    frame_sequence: u64,
    created_at: UtcMillis,
    updated_at: UtcMillis,
    annotations: Vec<BrowserAnnotationResponse>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserViewportResponse {
    width: u32,
    height: u32,
    device_scale_factor_millis: u32,
    device_type: magi_browser_runtime::BrowserDeviceType,
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
            viewport: tab.viewport.into(),
            viewport_mode: tab.viewport_mode,
            navigation_revision: tab.navigation_revision,
            snapshot_revision: tab.snapshot_revision,
            frame_sequence: tab.frame_sequence,
            created_at: tab.created_at,
            updated_at: tab.updated_at,
            annotations: Vec::new(),
        }
    }
}

async fn create_session(
    State(state): State<ApiState>,
    Json(request): Json<CreateBrowserSessionRequest>,
) -> Result<(StatusCode, Json<BrowserSessionResponse>), ApiError> {
    let (workspace_id, session_id) = validate_session_scope(
        &state,
        request.workspace_id.trim(),
        request.session_id.trim(),
    )?;
    ensure_browser_ui_ready(&state, &session_id)?;
    let existing = {
        let authority = state
            .browser_authority
            .lock()
            .expect("browser authority lock poisoned");
        authority.session_for_magi_session(&session_id).cloned()
    };
    if let Some(existing) = existing {
        return Ok((
            StatusCode::OK,
            Json(browser_session_response(&state, existing)?),
        ));
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
        &workspace_id,
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
    let (workspace_id, session_id) =
        validate_session_scope(&state, query.workspace_id.trim(), query.session_id.trim())?;
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
        &closed.workspace_id,
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
    #[serde(default = "default_browser_url")]
    initial_url: String,
    #[serde(default)]
    viewport: BrowserViewport,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum SetBrowserViewportRequest {
    Sync {
        width: u32,
        height: u32,
        #[serde(default)]
        controller_id: Option<String>,
        #[serde(default)]
        claim: bool,
    },
    Set {
        mode: BrowserViewportMode,
        width: u32,
        height: u32,
        #[serde(default)]
        device_type: Option<magi_browser_runtime::BrowserDeviceType>,
        #[serde(default)]
        controller_id: Option<String>,
    },
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

fn default_browser_url() -> String {
    "about:blank".to_string()
}

async fn create_tab(
    State(state): State<ApiState>,
    Path(browser_session_id): Path<String>,
    Json(request): Json<CreateBrowserTabRequest>,
) -> Result<(StatusCode, Json<BrowserTabResponse>), ApiError> {
    let browser_session_id = BrowserSessionId::new(browser_session_id);
    let session = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .session(&browser_session_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("浏览器会话不存在", browser_session_id.as_str()))?;
    ensure_browser_ui_ready(&state, &session.session_id)?;
    validate_navigation_url(&request.initial_url)?;
    let client = state
        .browser_host_client()
        .ok_or_else(|| ApiError::Conflict("浏览器运行组件尚未启动".to_string()))?;
    let now = UtcMillis::now();
    let tab_id = BrowserTabId::new(format!(
        "browser-tab-{}-{}",
        now.0,
        BROWSER_TAB_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    state.mutate_browser_authority(|authority| {
        authority.create_tab(CreateBrowserTab {
            tab_id: tab_id.clone(),
            browser_session_id: browser_session_id.clone(),
            url: request.initial_url.clone(),
            viewport: request.viewport,
            now,
        })
    })?;
    let host_reply = client
        .request(BrowserHostCommand::CreatePage {
            tab_id: tab_id.clone(),
            initial_url: request.initial_url,
            viewport: magi_browser_runtime::HostViewport {
                width: request.viewport.width,
                height: request.viewport.height,
                device_scale_factor_millis: request.viewport.device_scale_factor_millis,
                device_type: request.viewport.device_type,
            },
            navigation_revision: 0,
            snapshot_revision: 0,
        })
        .await;
    let page_state = match host_reply {
        Ok(reply) => match reply.response.outcome {
            BrowserHostCommandOutcome::Succeeded(result) => match *result {
                BrowserHostCommandResult::PageState(state) => state,
                result => {
                    mark_tab_crashed(&state, &tab_id);
                    return Err(ApiError::Conflict(format!(
                        "浏览器 Host 创建页面失败: {:?}",
                        BrowserHostCommandOutcome::Succeeded(Box::new(result))
                    )));
                }
            },
            outcome => {
                mark_tab_crashed(&state, &tab_id);
                return Err(ApiError::Conflict(format!(
                    "浏览器 Host 创建页面失败: {outcome:?}"
                )));
            }
        },
        Err(error) => {
            mark_tab_crashed(&state, &tab_id);
            return Err(ApiError::model_invocation_failed(
                "浏览器 Host 创建页面失败",
                error,
            ));
        }
    };
    let tab = state.mutate_browser_authority(|authority| {
        authority.transition_tab(&tab_id, BrowserTabLifecycle::Ready, UtcMillis::now())?;
        authority.apply_host_page_state(
            &tab_id,
            page_state.navigation_revision,
            page_state.url.clone(),
            page_state.origin.clone(),
            page_state.title.clone(),
            UtcMillis::now(),
        )
    })?;
    publish_browser_event(
        &state,
        "browser.tab.created",
        &session.workspace_id,
        &session.session_id,
        serde_json::json!({
            "browser_session_id": browser_session_id,
            "tab_id": tab_id,
            "url": tab.url,
        }),
    );
    Ok((StatusCode::CREATED, Json(tab.into())))
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
    let (tab, session) = browser_tab_scope(&state, &tab_id)?;
    let comment = request.comment.trim().to_string();
    if comment.is_empty() || comment.chars().count() > 4_000 {
        return Err(ApiError::InvalidInput(
            "标记内容不能为空且不能超过 4000 个字符".to_string(),
        ));
    }
    let _control_guard = state.browser_control_lock.lock().await;
    ensure_user_control_for_ui_locked(&state, &session).await?;
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
                x * f64::from(tab.viewport.width),
                y * f64::from(tab.viewport.height),
                None,
            )
        }
        BrowserAnnotationSelectionRequest::Region {
            navigation_revision,
            rect,
        } => {
            validate_annotation_rect(rect)?;
            let x = (rect.x + rect.width / 2.0) * f64::from(tab.viewport.width);
            let y = (rect.y + rect.height / 2.0) * f64::from(tab.viewport.height);
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
    let anchor = match region {
        Some(rect) => BrowserAnnotationAnchor::Region(BrowserRegionAnnotationAnchor {
            url: tab.url.clone(),
            origin: tab.origin.clone(),
            viewport: tab.viewport,
            scroll_x: hit.scroll_x,
            scroll_y: hit.scroll_y,
            rect,
            snapshot_revision: tab.snapshot_revision,
        }),
        None => BrowserAnnotationAnchor::Element(Box::new(BrowserElementAnnotationAnchor {
            url: tab.url.clone(),
            origin: tab.origin.clone(),
            frame_path: Vec::new(),
            viewport: tab.viewport,
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
            bounding_box: normalize_hit_bounds(&tab, hit.bounds)?,
            snapshot_revision: tab.snapshot_revision,
        })),
    };
    let screenshot_artifact_id =
        persist_browser_annotation_screenshot(&state, &session.session_id, &tab.tab_id).await?;
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
        &session.workspace_id,
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
) -> Result<String, ApiError> {
    let reply = require_browser_host(state)?
        .request(BrowserHostCommand::Screenshot {
            tab_id: tab_id.clone(),
            target: None,
            full_page: false,
            format: magi_browser_runtime::BrowserScreenshotFormat::Png,
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
    tab: &BrowserTab,
    bounds: BrowserHostRect,
) -> Result<BrowserNormalizedRect, ApiError> {
    let width = f64::from(tab.viewport.width);
    let height = f64::from(tab.viewport.height);
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
    ensure_user_control_for_ui_locked(&state, &session).await?;
    let updated = state.mutate_browser_authority(|authority| {
        authority.update_annotation_status(&annotation_id, request.status, UtcMillis::now())
    })?;
    publish_browser_event(
        &state,
        "browser.annotation.status_changed",
        &session.workspace_id,
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
    ensure_user_control_for_ui_locked(&state, &session).await?;
    let updated = state.mutate_browser_authority(|authority| {
        authority.update_annotation_comment(&annotation_id, comment, UtcMillis::now())
    })?;
    publish_browser_event(
        &state,
        "browser.annotation.updated",
        &session.workspace_id,
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
) -> Result<Response, ApiError> {
    let annotation_id = BrowserAnnotationId::new(annotation_id);
    let artifact_id = {
        let authority = state
            .browser_authority
            .lock()
            .expect("browser authority lock poisoned");
        let annotation = authority
            .annotation(&annotation_id)
            .ok_or_else(|| ApiError::not_found("浏览器标记不存在", annotation_id.as_str()))?;
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
    let client = require_browser_host(&state)?;
    require_host_success(
        client
            .request(BrowserHostCommand::ActivatePage {
                tab_id: tab_id.clone(),
            })
            .await,
        "激活浏览器 Tab 失败",
    )?;
    let session = state.mutate_browser_authority(|authority| {
        authority.set_active_tab(&tab.browser_session_id, &tab_id, UtcMillis::now())
    })?;
    publish_browser_event(
        &state,
        "browser.tab.activated",
        &session.workspace_id,
        &session.session_id,
        serde_json::json!({
            "browser_session_id": session.browser_session_id,
            "tab_id": tab_id,
            "revision": session.revision,
        }),
    );
    Ok(Json(browser_session_response(&state, session)?))
}

async fn set_viewport(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
    Json(request): Json<SetBrowserViewportRequest>,
) -> Result<Json<BrowserTabResponse>, ApiError> {
    let (width, height, requested_mode, requested_device_type, panel_sync, controller_id, claim) =
        match request {
            SetBrowserViewportRequest::Sync {
                width,
                height,
                controller_id,
                claim,
            } => (
                width,
                height,
                BrowserViewportMode::Auto,
                None,
                true,
                controller_id,
                claim,
            ),
            SetBrowserViewportRequest::Set {
                mode,
                width,
                height,
                device_type,
                controller_id,
            } => (
                width,
                height,
                mode,
                device_type,
                false,
                controller_id,
                false,
            ),
        };
    validate_viewport_dimensions(width, height)?;
    let device_type = magi_browser_runtime::BrowserDeviceType::for_dimensions(width);
    if requested_device_type.is_some_and(|requested| requested != device_type) {
        return Err(ApiError::InvalidInput(
            "浏览器设备类型与视口宽度不一致：320-600 为 mobile，601 以上为 desktop".to_string(),
        ));
    }
    let tab_id = BrowserTabId::new(tab_id);
    let _control_guard = state.browser_control_lock.lock().await;
    let (tab, session) = browser_tab_scope(&state, &tab_id)?;
    ensure_browser_ui_ready(&state, &session.session_id)?;
    if panel_sync && tab.viewport_mode != BrowserViewportMode::Auto {
        return Ok(Json(tab.into()));
    }
    if panel_sync {
        let Some(controller_id) = normalize_viewport_controller_id(controller_id.as_deref())?
        else {
            // 旧页面没有控制器身份，不能继续覆盖当前权威视口。
            return Ok(Json(tab.into()));
        };
        if !state.accept_browser_viewport_controller(&tab_id, controller_id, claim) {
            return Ok(Json(tab.into()));
        }
    } else if requested_mode == BrowserViewportMode::Auto {
        if let Some(controller_id) = normalize_viewport_controller_id(controller_id.as_deref())? {
            state.accept_browser_viewport_controller(&tab_id, controller_id, true);
        }
    } else {
        state.clear_browser_viewport_controller(&tab_id);
    }
    let viewport = BrowserViewport {
        width,
        height,
        device_scale_factor_millis: tab.viewport.device_scale_factor_millis,
        device_type,
    };
    if tab.viewport == viewport && tab.viewport_mode == requested_mode {
        return Ok(Json(tab.into()));
    }
    if tab.viewport != viewport {
        require_host_success(
            require_browser_host(&state)?
                .request(BrowserHostCommand::SetViewport {
                    tab_id: tab_id.clone(),
                    viewport: magi_browser_runtime::HostViewport {
                        width: viewport.width,
                        height: viewport.height,
                        device_scale_factor_millis: viewport.device_scale_factor_millis,
                        device_type: viewport.device_type,
                    },
                })
                .await,
            "调整浏览器页面视口失败",
        )?;
    }
    let updated = state.mutate_browser_authority(|authority| {
        authority.set_tab_viewport(&tab_id, viewport, requested_mode, UtcMillis::now())
    })?;
    publish_browser_event(
        &state,
        "browser.tab.viewport_changed",
        &session.workspace_id,
        &session.session_id,
        serde_json::json!({
            "browser_session_id": session.browser_session_id,
            "tab_id": updated.tab_id,
            "viewport": updated.viewport,
            "viewport_mode": updated.viewport_mode,
            "snapshot_revision": updated.snapshot_revision,
        }),
    );
    Ok(Json(updated.into()))
}

fn normalize_viewport_controller_id(value: Option<&str>) -> Result<Option<&str>, ApiError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.len() > 128
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        return Err(ApiError::InvalidInput(
            "浏览器视口控制器标识无效".to_string(),
        ));
    }
    Ok(Some(value))
}

async fn close_tab(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let tab_id = BrowserTabId::new(tab_id);
    let (tab, session) = browser_tab_scope(&state, &tab_id)?;
    let _control_guard = state.browser_control_lock.lock().await;
    let _ = ensure_user_control_for_ui_locked(&state, &session).await?;
    let client = require_browser_host(&state)?;
    require_host_success(
        client
            .request(BrowserHostCommand::ClosePage {
                tab_id: tab_id.clone(),
            })
            .await,
        "关闭浏览器 Tab 失败",
    )?;
    state.mutate_browser_authority(|authority| {
        authority.transition_tab(&tab_id, BrowserTabLifecycle::Closed, UtcMillis::now())
    })?;
    state.clear_browser_viewport_controller(&tab_id);
    publish_browser_event(
        &state,
        "browser.tab.closed",
        &session.workspace_id,
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
}

async fn navigate_tab(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
    Json(request): Json<NavigateBrowserTabRequest>,
) -> Result<Json<BrowserTabResponse>, ApiError> {
    let tab_id = BrowserTabId::new(tab_id);
    let (_tab, session) = browser_tab_scope(&state, &tab_id)?;
    let _control_guard = state.browser_control_lock.lock().await;
    let fence = ensure_user_control_for_ui_locked(&state, &session)
        .await?
        .fence;
    let navigation = match request.action.trim() {
        "url" => {
            let url = request
                .url
                .as_deref()
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .ok_or_else(|| ApiError::InvalidInput("url 导航必须提供 URL".to_string()))?
                .to_string();
            validate_navigation_url(&url)?;
            BrowserNavigation::Url { url }
        }
        "back" => BrowserNavigation::Back,
        "forward" => BrowserNavigation::Forward,
        "reload" => BrowserNavigation::Reload,
        _ => {
            return Err(ApiError::InvalidInput(
                "action 必须是 url、back、forward 或 reload".to_string(),
            ));
        }
    };
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
        &session.workspace_id,
        &session.session_id,
        serde_json::json!({
            "browser_session_id": session.browser_session_id,
            "tab_id": tab.tab_id,
            "url": tab.url,
            "navigation_revision": tab.navigation_revision,
        }),
    );
    Ok(Json(tab.into()))
}

fn validate_navigation_url(url: &str) -> Result<(), ApiError> {
    validate_browser_navigation_url(url)
        .map_err(|error| ApiError::InvalidInput(format!("浏览器导航 URL 不合法: {error}")))
}

fn validate_viewport_dimensions(width: u32, height: u32) -> Result<(), ApiError> {
    if !(320..=7_680).contains(&width) || !(240..=4_320).contains(&height) {
        return Err(ApiError::InvalidInput(
            "浏览器页面视口尺寸超出支持范围".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScreenshotBrowserTabRequest {
    #[serde(default)]
    full_page: bool,
}

async fn screenshot_tab(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
    Json(request): Json<ScreenshotBrowserTabRequest>,
) -> Result<Response, ApiError> {
    let tab_id = BrowserTabId::new(tab_id);
    let (_tab, session) = browser_tab_scope(&state, &tab_id)?;
    ensure_browser_ui_ready(&state, &session.session_id)?;
    let reply = require_browser_host(&state)?
        .request(BrowserHostCommand::Screenshot {
            tab_id,
            target: None,
            full_page: request.full_page,
            format: magi_browser_runtime::BrowserScreenshotFormat::Png,
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

async fn browser_channel(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    if super::is_public_tunnel_request(&headers) {
        return Err(ApiError::Conflict(
            "远程公网访问不能订阅本地浏览器画面".to_string(),
        ));
    }
    let tab_id = BrowserTabId::new(tab_id);
    let (tab, session) = browser_tab_scope(&state, &tab_id)?;
    ensure_browser_ui_ready(&state, &session.session_id)?;
    if tab.lifecycle != BrowserTabLifecycle::Ready {
        return Err(ApiError::Conflict(
            "浏览器 Tab 尚未就绪，不能订阅页面画面".to_string(),
        ));
    }
    Ok(ws.on_upgrade(move |socket| run_browser_channel(socket, state, tab_id)))
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum BrowserChannelClientMessage {
    UserInput { event: BrowserUserInputEvent },
}

async fn run_browser_channel(socket: WebSocket, state: ApiState, tab_id: BrowserTabId) {
    let Some((client, host_generation)) = state.browser_host_client_with_generation() else {
        return;
    };
    let mut events = client.subscribe();
    let subscription = state
        .browser_screencasts
        .subscribe(
            &client,
            host_generation,
            &tab_id,
            BrowserScreencastOptions {
                format: BrowserScreencastFormat::Jpeg,
                quality: BROWSER_SCREENCAST_JPEG_QUALITY,
                max_width: BROWSER_SCREENCAST_MAX_WIDTH,
                max_height: BROWSER_SCREENCAST_MAX_HEIGHT,
            },
        )
        .await;
    let Ok(subscription) = subscription else {
        tracing::warn!(tab_id = %tab_id, ?subscription, "建立浏览器画面订阅失败");
        return;
    };
    let (mut sink, mut source) = socket.split();
    loop {
        tokio::select! {
            incoming = source.next() => {
                let Some(Ok(message)) = incoming else { break; };
                match message {
                    Message::Text(text) => {
                        let Ok(message) = serde_json::from_str::<BrowserChannelClientMessage>(&text) else {
                            continue;
                        };
                        let BrowserChannelClientMessage::UserInput { event } = message;
                        let session = {
                            let authority = state.browser_authority.lock().expect("browser authority lock poisoned");
                            let Some(tab) = authority.tab(&tab_id) else { break; };
                            let Some(session) = authority.session(&tab.browser_session_id) else { break; };
                            session.clone()
                        };
                        let _control_guard = state.browser_control_lock.lock().await;
                        if !matches!(&event, BrowserUserInputEvent::MouseMove { .. })
                            && let Err(error) = ensure_user_control_for_ui_locked(&state, &session).await
                        {
                                let payload = serde_json::json!({
                                    "type": "error",
                                    "code": "browser_user_input_rejected",
                                    "message": error.message(),
                                });
                                if sink.send(Message::Text(payload.to_string().into())).await.is_err() {
                                    break;
                                }
                                continue;
                        }
                        let control = {
                            let authority = state.browser_authority.lock().expect("browser authority lock poisoned");
                            let Ok(control) = authority.profile_control_snapshot(&session.profile_id) else { break; };
                            control
                        };
                        if control.mode != BrowserProfileControlMode::User {
                            continue;
                        }
                        let input_result = client.request(BrowserHostCommand::UserInput {
                            tab_id: tab_id.clone(),
                            control: BrowserHostControl::User { fence: control.fence },
                            event,
                        }).await;
                        let input_error = match input_result {
                            Ok(reply) => match reply.response.outcome {
                                BrowserHostCommandOutcome::Succeeded(result) => match *result {
                                    BrowserHostCommandResult::ClipboardText(clipboard) => {
                                        let payload = serde_json::json!({
                                            "type": "clipboard_text",
                                            "operation": clipboard.operation,
                                            "text": clipboard.text,
                                        });
                                        if sink.send(Message::Text(payload.to_string().into())).await.is_err() {
                                            break;
                                        }
                                        None
                                    }
                                    _ => None,
                                },
                                outcome => Some(host_outcome_error(
                                    "浏览器用户输入失败",
                                    outcome,
                                ).message().to_string()),
                            },
                            Err(error) => Some(format!("浏览器用户输入失败: {error}")),
                        };
                        if let Some(message) = input_error {
                            let payload = serde_json::json!({
                                "type": "error",
                                "code": "browser_user_input_failed",
                                "message": message,
                            });
                            if sink.send(Message::Text(payload.to_string().into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Message::Close(_) => break,
                    Message::Ping(bytes) => {
                        if sink.send(Message::Pong(bytes)).await.is_err() { break; }
                    }
                    Message::Binary(_) | Message::Pong(_) => {}
                }
            }
            event = events.recv() => {
                let Ok(event) = event else { break; };
                let BrowserHostEvent::ScreencastFrame(frame) = &event.envelope.event else {
                    continue;
                };
                if frame.tab_id != tab_id {
                    continue;
                }
                let Some(binary) = event.binary else { continue; };
                let metadata = serde_json::json!({
                    "type": "frame",
                    "frameSequence": frame.frame_sequence,
                    "navigationRevision": frame.navigation_revision,
                    "mimeType": frame.mime_type,
                    "width": frame.width,
                    "height": frame.height,
                    "deviceScaleFactorMillis": frame.device_scale_factor_millis,
                });
                if sink.send(Message::Text(metadata.to_string().into())).await.is_err()
                    || sink.send(Message::Binary(binary.into())).await.is_err()
                {
                    break;
                }
            }
        }
    }
    state
        .browser_screencasts
        .unsubscribe(&client, subscription)
        .await;
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
) -> Result<magi_browser_runtime::BrowserHostClient, ApiError> {
    state
        .browser_host_client()
        .ok_or_else(|| ApiError::Conflict("浏览器运行组件尚未启动".to_string()))
}

async fn ensure_user_control_for_ui_locked(
    state: &ApiState,
    session: &BrowserSession,
) -> Result<BrowserProfileControlSnapshot, ApiError> {
    ensure_browser_ui_ready(state, &session.session_id)?;
    let (control, changed) = state.mutate_browser_authority(|authority| {
        let current = authority.profile_control_snapshot(&session.profile_id)?;
        if current.mode == BrowserProfileControlMode::User {
            return Ok((current, false));
        }
        authority.take_user_control(&session.profile_id, UtcMillis::now())?;
        Ok((
            authority.profile_control_snapshot(&session.profile_id)?,
            true,
        ))
    })?;
    if changed {
        if let Err(error) = super::sessions::interrupt_session_turn_for_browser_takeover(
            state,
            &session.session_id,
            &session.workspace_id,
        )
        .await
        {
            tracing::warn!(
                session_id = %session.session_id,
                ?error,
                "浏览器用户操作后收口当前 Turn 失败，控制 fence 仍保持生效"
            );
        }
        require_host_success(
            require_browser_host(state)?
                .request(BrowserHostCommand::UpdateControl {
                    fence: control.fence,
                    mode: BrowserHostControlMode::User,
                })
                .await,
            "同步浏览器用户控制权失败",
        )?;
        publish_browser_event(
            state,
            "browser.control.changed",
            &session.workspace_id,
            &session.session_id,
            serde_json::json!({
                "browser_session_id": session.browser_session_id,
                "profile_id": session.profile_id,
                "mode": control.mode,
                "fence": control.fence,
            }),
        );
    }
    Ok(control)
}

fn require_host_success(
    result: Result<
        magi_browser_runtime::BrowserHostCommandReply,
        magi_browser_runtime::BrowserHostClientError,
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

fn mark_tab_crashed(state: &ApiState, tab_id: &BrowserTabId) {
    if let Err(error) = state.mutate_browser_authority(|authority| {
        authority.transition_tab(tab_id, BrowserTabLifecycle::Crashed, UtcMillis::now())
    }) {
        tracing::error!(tab_id = %tab_id, ?error, "标记浏览器 Tab 崩溃失败");
    }
}

fn validate_session_scope(
    state: &ApiState,
    workspace_id: &str,
    session_id: &str,
) -> Result<(WorkspaceId, SessionId), ApiError> {
    if workspace_id.is_empty() || session_id.is_empty() {
        return Err(ApiError::InvalidInput(
            "workspaceId 和 sessionId 不能为空".to_string(),
        ));
    }
    let session_id = SessionId::new(session_id);
    let session = state
        .session_store
        .session(&session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    if session.status != SessionLifecycleStatus::Active {
        return Err(ApiError::Conflict("会话已关闭，不能打开浏览器".to_string()));
    }
    if session.workspace_id.as_deref() != Some(workspace_id) {
        return Err(ApiError::Conflict(
            "浏览器会话与工作区绑定不匹配".to_string(),
        ));
    }
    Ok((WorkspaceId::new(workspace_id), session_id))
}

fn ensure_browser_ui_ready(state: &ApiState, session_id: &SessionId) -> Result<(), ApiError> {
    let capability = state.browser_capability_snapshot(Some(session_id));
    if !capability.in_app_browser_enabled {
        return Err(ApiError::Conflict("内置浏览器功能未启用".to_string()));
    }
    if !capability.runtime_status.is_usable() {
        return Err(ApiError::Conflict(format!(
            "浏览器运行组件不可用: {:?}",
            capability.runtime_status
        )));
    }
    if !capability.host_protocol_compatible || state.browser_host_client().is_none() {
        return Err(ApiError::Conflict("浏览器运行组件尚未就绪".to_string()));
    }
    Ok(())
}

fn browser_session_response(
    state: &ApiState,
    session: BrowserSession,
) -> Result<BrowserSessionResponse, ApiError> {
    let authority = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned");
    let tabs = session
        .tab_ids
        .iter()
        .filter_map(|tab_id| authority.tab(tab_id).cloned())
        .map(|tab| {
            let mut response = BrowserTabResponse::from(tab.clone());
            response.annotations = authority
                .annotations_for_tab(&tab.tab_id)
                .into_iter()
                .map(BrowserAnnotationResponse::from)
                .collect();
            response
        })
        .collect();
    let control = authority
        .profile_control_snapshot(&session.profile_id)
        .map_err(|error| ApiError::InternalAssemblyError(error.to_string()))?;
    Ok(BrowserSessionResponse {
        browser_session_id: session.browser_session_id,
        workspace_id: session.workspace_id,
        session_id: session.session_id,
        profile_id: session.profile_id,
        lifecycle: session.lifecycle,
        active_tab_id: session.active_tab_id,
        tabs,
        runtime_epoch: session.runtime_epoch,
        revision: session.revision,
        control_mode: control.mode,
        control_fence: control.fence,
        created_at: session.created_at,
        updated_at: session.updated_at,
    })
}

fn host_command_succeeded(outcome: &BrowserHostCommandOutcome) -> bool {
    matches!(outcome, BrowserHostCommandOutcome::Succeeded(_))
}

fn publish_browser_event(
    state: &ApiState,
    event_type: &str,
    workspace_id: &WorkspaceId,
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
            workspace_id: Some(workspace_id.clone()),
            session_id: Some(session_id.clone()),
            ..EventContext::default()
        }),
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use magi_browser_runtime::{
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
        BrowserAnnotationAnchorResponse, BrowserElementAnnotationAnchorResponse,
        BrowserRegionAnnotationAnchorResponse, resolve_browser_annotation_context,
    };
    use crate::{errors::ApiError, state::ApiState};

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
                    workspace_id: WorkspaceId::new("workspace-browser-annotation-current"),
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
                    viewport: BrowserViewport::default(),
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
                    author: BrowserAnnotationAuthor::User,
                    kind: BrowserAnnotationKind::Region,
                    anchor: BrowserAnnotationAnchor::Region(BrowserRegionAnnotationAnchor {
                        url: "https://example.com/settings".to_string(),
                        origin: Some("https://example.com".to_string()),
                        viewport: BrowserViewport::default(),
                        scroll_x: 0.0,
                        scroll_y: 120.0,
                        rect: magi_browser_runtime::BrowserNormalizedRect {
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

    #[test]
    fn annotation_context_is_authoritative_session_scoped_and_active_only() {
        let (state, session_id, other_session_id, annotation_id) = annotation_fixture();
        let resolved =
            resolve_browser_annotation_context(&state, &session_id, &[annotation_id.to_string()])
                .expect("active annotation should resolve");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0]["annotationId"], annotation_id.to_string());
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
                    BrowserAnnotationStatus::Resolved,
                    UtcMillis(3),
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
                rect: magi_browser_runtime::BrowserNormalizedRect {
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
                bounding_box: magi_browser_runtime::BrowserNormalizedRect {
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
}
