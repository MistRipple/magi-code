use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

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
    state::{ApiState, BrowserScreencastOptions, BrowserViewBinding},
};

static BROWSER_SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static BROWSER_TAB_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static BROWSER_VIEW_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static BROWSER_ANNOTATION_ARTIFACT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const MAX_BROWSER_CLIPBOARD_TEXT_BYTES: usize = 1024 * 1024;
const BROWSER_SCREENCAST_JPEG_QUALITY: u8 = 90;
const BROWSER_SCREENCAST_MAX_WIDTH: u32 = 7_680;
const BROWSER_SCREENCAST_MAX_HEIGHT: u32 = 4_320;
const BROWSER_READY_WAIT_TIMEOUT: Duration = Duration::from_secs(65);
const BROWSER_READY_POLL_INTERVAL: Duration = Duration::from_millis(50);

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
        .route(
            "/browser/tabs/{tab_id}/viewport-controller",
            delete(release_viewport_controller),
        )
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
    required_magi_version: Option<String>,
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
        required_magi_version: runtime.required_magi_version,
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
    wait_for_browser_ui_ready(&state, &session_id).await?;
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
            &closed.workspace_id,
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
    let view_bindings = state.browser_views.remove_for_session(&session.tab_ids);
    if let Some(client) = state.browser_host_client() {
        for binding in view_bindings {
            retire_browser_view(&state, &client, binding).await;
        }
    }
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
    initial_url: String,
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
        surface_width: u32,
        surface_height: u32,
        controller_id: String,
    },
    Set {
        mode: BrowserViewportMode,
        width: u32,
        height: u32,
        surface_width: u32,
        surface_height: u32,
        device_type: magi_browser_runtime::BrowserDeviceType,
        #[serde(default)]
        controller_id: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleaseBrowserViewportControllerRequest {
    controller_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateBrowserAnnotationRequest {
    selection: BrowserAnnotationSelectionRequest,
    comment: String,
    #[serde(default)]
    view_id: Option<String>,
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
    wait_for_browser_ui_ready(&state, &session.session_id).await?;
    let session = wait_for_browser_session_ready(&state, &browser_session_id).await?;
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
                surface_width: request.viewport.width,
                surface_height: request.viewport.height,
                device_scale_factor_millis: request.viewport.device_scale_factor_millis,
                device_type: request.viewport.device_type,
            },
            navigation_revision: 0,
            snapshot_revision: 0,
            allow_streaming_eviction: true,
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
    let (_tab_before_lock, session) = browser_tab_scope(&state, &tab_id)?;
    let comment = request.comment.trim().to_string();
    if comment.is_empty() || comment.chars().count() > 4_000 {
        return Err(ApiError::InvalidInput(
            "标记内容不能为空且不能超过 4000 个字符".to_string(),
        ));
    }
    let _control_guard = state.browser_control_lock.lock().await;
    ensure_user_control_for_ui_locked(&state, &session).await?;
    // Re-read the authoritative tab after acquiring the control lock. A panel
    // resize or navigation may have completed while the request was waiting.
    let (tab, session) = browser_tab_scope(&state, &tab_id)?;
    let view_use = normalize_browser_view_id(request.view_id.as_deref())?
        .map(|view_id| {
            state
                .browser_views
                .acquire(&tab_id, view_id, state.browser_host_generation())
                .ok_or_else(|| ApiError::Conflict("浏览器面板 View 尚未就绪".to_string()))
        })
        .transpose()?;
    let host_tab_id = view_use
        .as_ref()
        .map(|view| view.host_tab_id().clone())
        .unwrap_or_else(|| tab_id.clone());
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
    let hit = browser_hit_test(&state, &host_tab_id, navigation_revision, hit_x, hit_y).await?;
    let hit_viewport = BrowserViewport {
        width: hit.viewport_width,
        height: hit.viewport_height,
        device_scale_factor_millis: tab.viewport.device_scale_factor_millis,
        device_type: magi_browser_runtime::BrowserDeviceType::for_dimensions(hit.viewport_width),
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
        &host_tab_id,
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
    clip: BrowserNormalizedRect,
    host_tab_id: &BrowserTabId,
) -> Result<String, ApiError> {
    let reply = require_browser_host(state)?
        .request(BrowserHostCommand::Screenshot {
            tab_id: host_tab_id.clone(),
            target: None,
            clip: Some(clip),
            full_page: false,
            format: magi_browser_runtime::BrowserScreenshotFormat::Png,
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
    let client = require_browser_host(&state)?;
    if !matches!(
        tab.lifecycle,
        BrowserTabLifecycle::Ready | BrowserTabLifecycle::Suspended | BrowserTabLifecycle::Crashed
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
    let page_state = match require_host_success(
        client
            .request(BrowserHostCommand::RestorePage {
                tab_id: tab_id.clone(),
                initial_url: tab.url.clone(),
                viewport: magi_browser_runtime::HostViewport {
                    width: tab.viewport.width,
                    height: tab.viewport.height,
                    surface_width: tab.viewport.width,
                    surface_height: tab.viewport.height,
                    device_scale_factor_millis: tab.viewport.device_scale_factor_millis,
                    device_type: tab.viewport.device_type,
                },
                navigation_revision: tab.navigation_revision,
                snapshot_revision: tab.snapshot_revision,
                allow_streaming_eviction: true,
            })
            .await,
        "恢复浏览器 Tab 失败",
    ) {
        Ok(page_state) => page_state,
        Err(error) => {
            mark_tab_crashed(&state, &tab_id);
            return Err(error);
        }
    };
    let BrowserHostCommandResult::PageState(page_state) = page_state else {
        return Err(ApiError::InternalAssemblyError(
            "激活浏览器 Tab 缺少页面状态".to_string(),
        ));
    };
    let session = state.mutate_browser_authority(|authority| {
        if authority.tab(&tab_id).is_some_and(|tab| {
            matches!(
                tab.lifecycle,
                BrowserTabLifecycle::Suspended | BrowserTabLifecycle::Crashed
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
    let (
        width,
        height,
        surface_width,
        surface_height,
        requested_mode,
        requested_device_type,
        controller_id,
    ) = match request {
        SetBrowserViewportRequest::Sync {
            width,
            height,
            surface_width,
            surface_height,
            controller_id,
        } => (
            width,
            height,
            surface_width,
            surface_height,
            BrowserViewportMode::Auto,
            None,
            Some(controller_id),
        ),
        SetBrowserViewportRequest::Set {
            mode,
            width,
            height,
            surface_width,
            surface_height,
            device_type,
            controller_id,
        } => (
            width,
            height,
            surface_width,
            surface_height,
            mode,
            Some(device_type),
            controller_id,
        ),
    };
    validate_viewport_dimensions(width, height)?;
    validate_viewport_dimensions(surface_width, surface_height)?;
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
    let view_id = normalize_browser_view_id(controller_id.as_deref())?;
    let view_use = view_id
        .map(|view_id| {
            state
                .browser_views
                .acquire(&tab_id, view_id, state.browser_host_generation())
                .ok_or_else(|| ApiError::Conflict("浏览器面板 View 尚未就绪".to_string()))
        })
        .transpose()?;
    let host_tab_id = view_use
        .as_ref()
        .map(|view| view.host_tab_id().clone())
        .unwrap_or_else(|| tab_id.clone());
    let viewport = BrowserViewport {
        width,
        height,
        device_scale_factor_millis: tab.viewport.device_scale_factor_millis,
        device_type,
    };
    require_host_success(
        require_browser_host(&state)?
            .request(BrowserHostCommand::SetViewport {
                tab_id: host_tab_id,
                viewport: magi_browser_runtime::HostViewport {
                    width: viewport.width,
                    height: viewport.height,
                    surface_width,
                    surface_height,
                    device_scale_factor_millis: viewport.device_scale_factor_millis,
                    device_type: viewport.device_type,
                },
            })
            .await,
        "调整浏览器页面视口失败",
    )?;
    if view_id.is_some() {
        let mut response = tab.clone();
        response.viewport = viewport;
        response.viewport_mode = requested_mode;
        return Ok(Json(response.into()));
    }
    if tab.viewport == viewport && tab.viewport_mode == requested_mode {
        return Ok(Json(tab.into()));
    }
    let updated = state.mutate_browser_authority_transient(|authority| {
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

async fn release_viewport_controller(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
    Json(request): Json<ReleaseBrowserViewportControllerRequest>,
) -> Result<StatusCode, ApiError> {
    let tab_id = BrowserTabId::new(tab_id);
    let (_tab, session) = browser_tab_scope(&state, &tab_id)?;
    ensure_browser_ui_ready(&state, &session.session_id)?;
    let view_id = normalize_browser_view_id(Some(&request.controller_id))?
        .ok_or_else(|| ApiError::InvalidInput("浏览器视口控制器标识不能为空".to_string()))?;
    if let Some(binding) =
        state
            .browser_views
            .resolve(&tab_id, view_id, state.browser_host_generation())
    {
        // View 的 WebSocket 通道拥有物理 Page 的生命周期。释放接口只撤销
        // 绑定，避免在通道仍处于 RestorePage/StartScreencast 初始化阶段时
        // 直接关闭 Page，造成刷新竞态下的 browser_tab_unknown。
        state.browser_views.release(&binding);
    }
    Ok(StatusCode::NO_CONTENT)
}

fn normalize_browser_view_id(value: Option<&str>) -> Result<Option<&str>, ApiError> {
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
    if state.browser_host_client().is_some()
        && let Err(error) = ensure_user_control_for_ui_locked(&state, &session).await
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
        for binding in state.browser_views.remove_for_tab(&tab_id) {
            retire_browser_view(&state, &client, binding).await;
        }
    } else {
        state.browser_views.remove_for_tab(&tab_id);
    }
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
    #[serde(default)]
    view_id: Option<String>,
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
    let view_use = normalize_browser_view_id(request.view_id.as_deref())?
        .map(|view_id| {
            state
                .browser_views
                .acquire(&tab_id, view_id, state.browser_host_generation())
                .ok_or_else(|| ApiError::Conflict("浏览器面板 View 尚未就绪".to_string()))
        })
        .transpose()?;
    let host_tab_id = view_use
        .as_ref()
        .map(|view| view.host_tab_id().clone())
        .unwrap_or_else(|| tab_id.clone());
    let reply = require_host_success(
        require_browser_host(&state)?
            .request(BrowserHostCommand::Navigate {
                tab_id: host_tab_id,
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

fn validate_viewport_dimension(width: u32) -> Result<(), ApiError> {
    if !(320..=7_680).contains(&width) {
        return Err(ApiError::InvalidInput(
            "浏览器面板宽度超出支持范围".to_string(),
        ));
    }
    Ok(())
}

fn validate_viewport_height(height: u32) -> Result<(), ApiError> {
    if !(240..=4_320).contains(&height) {
        return Err(ApiError::InvalidInput(
            "浏览器面板高度超出支持范围".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScreenshotBrowserTabRequest {
    #[serde(default)]
    full_page: bool,
    #[serde(default)]
    view_id: Option<String>,
}

async fn screenshot_tab(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
    Json(request): Json<ScreenshotBrowserTabRequest>,
) -> Result<Response, ApiError> {
    let tab_id = BrowserTabId::new(tab_id);
    let (_tab, session) = browser_tab_scope(&state, &tab_id)?;
    ensure_browser_ui_ready(&state, &session.session_id)?;
    let view_use = normalize_browser_view_id(request.view_id.as_deref())?
        .map(|view_id| {
            state
                .browser_views
                .acquire(&tab_id, view_id, state.browser_host_generation())
                .ok_or_else(|| ApiError::Conflict("浏览器面板 View 尚未就绪".to_string()))
        })
        .transpose()?;
    let host_tab_id = view_use
        .as_ref()
        .map(|view| view.host_tab_id().clone())
        .unwrap_or_else(|| tab_id.clone());
    let reply = require_browser_host(&state)?
        .request(BrowserHostCommand::Screenshot {
            tab_id: host_tab_id,
            target: None,
            clip: None,
            full_page: request.full_page,
            format: magi_browser_runtime::BrowserScreenshotFormat::Png,
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

async fn browser_channel(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<BrowserChannelQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    if super::is_public_tunnel_request(&headers) {
        return Err(ApiError::Conflict(
            "远程公网访问不能订阅本地浏览器画面".to_string(),
        ));
    }
    let tab_id = BrowserTabId::new(tab_id);
    let view_id = normalize_browser_view_id(Some(query.view_id.trim()))?
        .ok_or_else(|| ApiError::InvalidInput("浏览器面板 View 标识不能为空".to_string()))?
        .to_string();
    validate_viewport_dimension(query.width)?;
    validate_viewport_height(query.height)?;
    validate_viewport_dimension(query.surface_width)?;
    validate_viewport_height(query.surface_height)?;
    let (tab, session) = browser_tab_scope(&state, &tab_id)?;
    ensure_browser_ui_ready(&state, &session.session_id)?;
    if tab.lifecycle != BrowserTabLifecycle::Ready {
        return Err(ApiError::Conflict(
            "浏览器 Tab 尚未就绪，不能订阅页面画面".to_string(),
        ));
    }
    let initial_viewport = magi_browser_runtime::HostViewport {
        width: query.width,
        height: query.height,
        surface_width: query.surface_width,
        surface_height: query.surface_height,
        device_scale_factor_millis: tab.viewport.device_scale_factor_millis,
        device_type: magi_browser_runtime::BrowserDeviceType::for_dimensions(query.width),
    };
    Ok(ws.on_upgrade(move |socket| {
        run_browser_channel(socket, state, tab_id, view_id, initial_viewport)
    }))
}

fn agent_cursor_targets_view(
    cursor_tab_id: &BrowserTabId,
    logical_tab_id: &BrowserTabId,
    host_view_tab_id: &BrowserTabId,
) -> bool {
    cursor_tab_id == logical_tab_id || cursor_tab_id == host_view_tab_id
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum BrowserChannelClientMessage {
    UserInput { event: BrowserUserInputEvent },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BrowserChannelQuery {
    view_id: String,
    width: u32,
    height: u32,
    surface_width: u32,
    surface_height: u32,
}

async fn run_browser_channel(
    socket: WebSocket,
    state: ApiState,
    tab_id: BrowserTabId,
    view_id: String,
    initial_viewport: magi_browser_runtime::HostViewport,
) {
    let Some((client, host_generation)) = state.browser_host_client_with_generation() else {
        return;
    };
    let Ok((tab, session)) = browser_tab_scope(&state, &tab_id) else {
        return;
    };
    let host_tab_id = BrowserTabId::new(format!(
        "browser-view-{}-{}",
        UtcMillis::now().0,
        BROWSER_VIEW_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let restored = client
        .request(BrowserHostCommand::RestorePage {
            tab_id: host_tab_id.clone(),
            initial_url: tab.url.clone(),
            viewport: initial_viewport,
            navigation_revision: tab.navigation_revision,
            snapshot_revision: tab.snapshot_revision,
            allow_streaming_eviction: true,
        })
        .await;
    if !matches!(
        restored,
        Ok(ref reply)
            if matches!(
                reply.response.outcome,
                BrowserHostCommandOutcome::Succeeded(ref result)
                    if matches!(result.as_ref(), BrowserHostCommandResult::PageState(_))
            )
    ) {
        tracing::warn!(tab_id = %tab_id, view_id, ?restored, "恢复浏览器面板 View 失败");
        let _ = client
            .request(BrowserHostCommand::ClosePage {
                tab_id: host_tab_id,
            })
            .await;
        return;
    }
    // 只有物理 Page 完成恢复后才发布 View 绑定。Worker 始终解析到一个已经
    // 可执行的前台页面，不会在 WebSocket 重连窗口命中尚未物化的新页面。
    let (binding, previous) = state.browser_views.bind(
        tab_id.clone(),
        view_id.clone(),
        host_tab_id.clone(),
        host_generation,
    );
    if let Some(previous) = previous {
        retire_browser_view(&state, &client, previous).await;
    }
    let (mut sink, mut source) = socket.split();
    let mut events = client.subscribe();
    let subscription = state
        .browser_screencasts
        .subscribe(
            &client,
            host_generation,
            &host_tab_id,
            BrowserScreencastOptions {
                format: BrowserScreencastFormat::Jpeg,
                quality: BROWSER_SCREENCAST_JPEG_QUALITY,
                max_width: BROWSER_SCREENCAST_MAX_WIDTH,
                max_height: BROWSER_SCREENCAST_MAX_HEIGHT,
            },
        )
        .await;
    let Ok(subscription) = subscription else {
        tracing::warn!(tab_id = %tab_id, view_id, ?subscription, "建立浏览器画面订阅失败");
        let payload = serde_json::json!({
            "type": "error",
            "message": "浏览器画面初始化失败，请重新打开。",
        });
        let _ = sink.send(Message::Text(payload.to_string().into())).await;
        retire_browser_view(&state, &client, binding).await;
        return;
    };
    let ready = serde_json::json!({ "type": "ready" });
    if sink
        .send(Message::Text(ready.to_string().into()))
        .await
        .is_err()
    {
        state
            .browser_screencasts
            .unsubscribe(&client, subscription)
            .await;
        retire_browser_view(&state, &client, binding).await;
        return;
    }
    let agent_controls_browser = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .profile_control_snapshot(&session.profile_id)
        .is_ok_and(|control| control.mode == BrowserProfileControlMode::Agent);
    if agent_controls_browser {
        let cursor = serde_json::json!({
            "type": "agent_cursor",
            "visible": true,
            "x": 0.5,
            "y": 0.5,
            "action": "move",
        });
        if sink
            .send(Message::Text(cursor.to_string().into()))
            .await
            .is_err()
        {
            state
                .browser_screencasts
                .unsubscribe(&client, subscription)
                .await;
            retire_browser_view(&state, &client, binding).await;
            return;
        }
    }
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
                            tab_id: host_tab_id.clone(),
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
                match &event.envelope.event {
                    BrowserHostEvent::PageUpdated(page) if page.tab_id == host_tab_id => {
                        let _ = state.mutate_browser_authority(|authority| {
                            let Some(current) = authority.tab(&tab_id).cloned() else {
                                return Ok(());
                            };
                            let navigation_revision = if current.url != page.url
                                || current.origin != page.origin
                                || current.title != page.title
                            {
                                current.navigation_revision.saturating_add(1)
                            } else {
                                current.navigation_revision
                            };
                            authority.apply_host_page_state(
                                &tab_id,
                                navigation_revision,
                                page.url.clone(),
                                page.origin.clone(),
                                page.title.clone(),
                                UtcMillis::now(),
                            )?;
                            Ok(())
                        });
                    }
                    BrowserHostEvent::AgentCursor(cursor)
                        if agent_cursor_targets_view(&cursor.tab_id, &tab_id, &host_tab_id) =>
                    {
                        let payload = serde_json::json!({
                            "type": "agent_cursor",
                            "visible": cursor.visible,
                            "x": cursor.x,
                            "y": cursor.y,
                            "action": cursor.action,
                        });
                        if sink.send(Message::Text(payload.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    BrowserHostEvent::ScreencastFrame(frame) if frame.tab_id == host_tab_id => {
                        let Some(binary) = event.binary else { continue; };
                        let metadata = serde_json::json!({
                            "type": "frame",
                            "frameSequence": frame.frame_sequence,
                            "navigationRevision": frame.navigation_revision,
                            "mimeType": frame.mime_type,
                            "width": frame.width,
                            "height": frame.height,
                            "surfaceWidth": frame.surface_width,
                            "surfaceHeight": frame.surface_height,
                            "deviceScaleFactorMillis": frame.device_scale_factor_millis,
                        });
                        if sink.send(Message::Text(metadata.to_string().into())).await.is_err()
                            || sink.send(Message::Binary(binary.into())).await.is_err()
                        {
                            break;
                        }
                    }
                    BrowserHostEvent::PageSuspended(page) if page.tab_id == host_tab_id => {
                        let payload = serde_json::json!({ "type": "page_suspended" });
                        let _ = sink.send(Message::Text(payload.to_string().into())).await;
                        break;
                    }
                    BrowserHostEvent::PageCrashed { tab_id: crashed_tab_id, .. }
                        if *crashed_tab_id == host_tab_id =>
                    {
                        let payload = serde_json::json!({
                            "type": "error",
                            "message": "浏览器面板 View 已关闭，请重新打开。",
                        });
                        let _ = sink.send(Message::Text(payload.to_string().into())).await;
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    state
        .browser_screencasts
        .unsubscribe(&client, subscription)
        .await;
    retire_browser_view(&state, &client, binding).await;
}

async fn retire_browser_view(
    state: &ApiState,
    client: &magi_browser_runtime::BrowserHostClient,
    binding: BrowserViewBinding,
) {
    state.browser_views.release(&binding);
    binding.wait_until_idle().await;
    let _ = client
        .request(BrowserHostCommand::ClosePage {
            tab_id: binding.host_tab_id,
        })
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

async fn wait_for_browser_ui_ready(
    state: &ApiState,
    session_id: &SessionId,
) -> Result<(), ApiError> {
    let deadline = Instant::now() + BROWSER_READY_WAIT_TIMEOUT;
    loop {
        match ensure_browser_ui_ready(state, session_id) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let runtime = state.browser_runtime_status();
                // 未安装、无效或明确禁用的运行组件不会因等待而就绪，立即返回权威错误；
                // 只有已安装且 Host 正在启动或恢复页面时才继续等待。
                if !runtime.component_status.is_usable()
                    || runtime.host_status == "failed"
                    || !runtime.in_app_browser_enabled
                {
                    return Err(error);
                }
                if Instant::now() >= deadline {
                    return Err(error);
                }
                tokio::time::sleep(BROWSER_READY_POLL_INTERVAL).await;
            }
        }
    }
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
    let agent_occupied =
        browser_session_agent_occupied(&authority, &session, &control, UtcMillis::now());
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
        agent_occupied,
        created_at: session.created_at,
        updated_at: session.updated_at,
    })
}

fn browser_session_agent_occupied(
    authority: &magi_browser_runtime::BrowserAuthority,
    session: &BrowserSession,
    control: &BrowserProfileControlSnapshot,
    now: UtcMillis,
) -> bool {
    control.mode == BrowserProfileControlMode::Agent
        && authority
            .active_lease_for_profile(&session.profile_id)
            .is_some_and(|lease| {
                lease.browser_session_id == session.browser_session_id
                    && lease.owner.session_id.as_ref() == Some(&session.session_id)
                    && now < lease.expires_at
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
        AcquireBrowserLease, BrowserAnnotation, BrowserAnnotationAnchor, BrowserAnnotationAuthor,
        BrowserAnnotationKind, BrowserAnnotationStatus, BrowserAuthority, BrowserProfile,
        BrowserProfileKind, BrowserRegionAnnotationAnchor, BrowserSession, BrowserSessionLifecycle,
        BrowserTabLifecycle, BrowserViewport, CreateBrowserSession, CreateBrowserTab,
    };
    use magi_core::{
        BrowserAnnotationId, BrowserLeaseId, BrowserProfileId, BrowserSessionId, BrowserTabId,
        ExecutionOwnership, SessionId, UtcMillis, WorkspaceId,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;

    use super::{
        BrowserAnnotationAnchorResponse, BrowserElementAnnotationAnchorResponse,
        BrowserRegionAnnotationAnchorResponse, agent_cursor_targets_view,
        browser_session_agent_occupied, resolve_browser_annotation_context,
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
                    sequence: 0,
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

    #[test]
    fn agent_cursor_targets_logical_tab_and_its_panel_view_only() {
        let logical_tab_id = BrowserTabId::new("browser-tab-logical");
        let panel_view_id = BrowserTabId::new("browser-view-panel");
        let other_tab_id = BrowserTabId::new("browser-tab-other");

        assert!(agent_cursor_targets_view(
            &logical_tab_id,
            &logical_tab_id,
            &panel_view_id,
        ));
        assert!(agent_cursor_targets_view(
            &panel_view_id,
            &logical_tab_id,
            &panel_view_id,
        ));
        assert!(!agent_cursor_targets_view(
            &other_tab_id,
            &logical_tab_id,
            &panel_view_id,
        ));
    }

    #[test]
    fn browser_session_occupancy_requires_a_live_lease_owned_by_that_session() {
        let profile_id = BrowserProfileId::new("browser-profile-occupancy");
        let browser_session_id = BrowserSessionId::new("browser-session-occupancy");
        let session_id = SessionId::new("session-occupancy");
        let workspace_id = WorkspaceId::new("workspace-occupancy");
        let mut authority = BrowserAuthority::new();
        authority
            .register_profile(BrowserProfile {
                profile_id: profile_id.clone(),
                kind: BrowserProfileKind::ManagedDefault,
                data_path: tempfile::tempdir()
                    .expect("occupancy profile fixture should create")
                    .keep(),
                created_at: UtcMillis(1),
                updated_at: UtcMillis(1),
            })
            .expect("occupancy profile should register");
        let session = authority
            .create_session(CreateBrowserSession {
                browser_session_id: browser_session_id.clone(),
                workspace_id: workspace_id.clone(),
                session_id: session_id.clone(),
                profile_id: profile_id.clone(),
                now: UtcMillis(1),
            })
            .expect("occupancy session should create");
        authority
            .transition_session(
                &browser_session_id,
                BrowserSessionLifecycle::Ready,
                UtcMillis(2),
            )
            .expect("occupancy session should become ready");
        let control = authority
            .profile_control_snapshot(&profile_id)
            .expect("occupancy control should exist");
        assert!(!browser_session_agent_occupied(
            &authority,
            &session,
            &control,
            UtcMillis(3),
        ));

        authority
            .acquire_lease(AcquireBrowserLease {
                lease_id: BrowserLeaseId::new("browser-lease-occupancy"),
                profile_id: profile_id.clone(),
                browser_session_id: browser_session_id.clone(),
                owner: ExecutionOwnership {
                    session_id: Some(session_id),
                    workspace_id: Some(workspace_id),
                    ..ExecutionOwnership::default()
                },
                turn_id: "turn-occupancy".to_string(),
                goal_binding: None,
                acquired_at: UtcMillis(4),
                expires_at: UtcMillis(10),
            })
            .expect("occupancy lease should acquire");
        let control = authority
            .profile_control_snapshot(&profile_id)
            .expect("occupancy control should remain available");
        assert!(browser_session_agent_occupied(
            &authority,
            &session,
            &control,
            UtcMillis(9),
        ));
        assert!(!browser_session_agent_occupied(
            &authority,
            &session,
            &control,
            UtcMillis(10),
        ));

        let other_session = BrowserSession {
            browser_session_id: BrowserSessionId::new("browser-session-other"),
            session_id: SessionId::new("session-other"),
            ..session
        };
        assert!(!browser_session_agent_occupied(
            &authority,
            &other_session,
            &control,
            UtcMillis(9),
        ));
    }
}
