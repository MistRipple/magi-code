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
    BrowserSessionLifecycle, BrowserSessionOwner, BrowserSurfaceControlSnapshot, BrowserTab,
    BrowserTabLifecycle, BrowserViewport, CreateBrowserSession, CreateBrowserTab,
    MAX_BROWSER_TABS_TOTAL, validate_browser_navigation_url,
};
use magi_core::{
    BrowserAnnotationId, BrowserProfileId, BrowserSessionId, BrowserTabId, EventId, SessionId,
    SessionLifecycleStatus, UtcMillis, WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    browser_image::crop_browser_screenshot,
    errors::ApiError,
    routes::session_scope::{self, SessionScope},
    session_activity::session_running_task_count,
    state::{ApiState, BrowserHostConnectionConfig, BrowserHostStatusSnapshot},
};

static BROWSER_SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static BROWSER_TAB_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static BROWSER_ANNOTATION_ARTIFACT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const BROWSER_READY_WAIT_TIMEOUT: Duration = Duration::from_secs(65);
const BROWSER_READY_POLL_INTERVAL: Duration = Duration::from_millis(50);
const BROWSER_SURFACE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const BROWSER_SURFACE_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// `restore_page`/`ensure_surface` 只保证物理 guest 已物化，不保证 Chromium
/// 已经完成这一代文档的导航。GPT Web 探测是只读操作，可以在这段交接窗口
/// 内安全重试；写入和提交路径不能复用这条重试策略（避免重放副作用）。
const WEB_MODEL_PROBE_RETRIES: usize = 8;
const WEB_MODEL_PROBE_RETRY_DELAY: Duration = Duration::from_millis(250);
const WEB_MODEL_PROBE_PAGE_LOAD_RETRIES: usize = 40;
const WEB_MODEL_PROBE_PAGE_LOAD_DELAY: Duration = Duration::from_millis(500);

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/browser/capabilities", get(capabilities))
        .route("/browser/settings", post(update_browser_settings))
        .route("/browser/resources", get(browser_resources))
        .route(
            "/browser/resources/reclaim",
            post(reclaim_browser_resources),
        )
        .route(
            "/browser/desktop/connection",
            get(get_desktop_connection)
                .post(register_desktop_connection)
                .delete(clear_desktop_connection),
        )
        .route("/browser/sessions", post(create_session))
        .route(
            "/browser/sessions/app",
            get(get_app_session).post(create_app_session),
        )
        .route("/browser/web-models/probe", post(probe_web_model))
        .route(
            "/browser/web-models/ensure-home",
            post(ensure_web_model_home_surface),
        )
        .route(
            "/browser/web-models/tunnel",
            get(get_web_model_tunnel).post(configure_web_model_tunnel),
        )
        .route("/browser/web-models/runtime", get(get_web_model_runtime))
        .route("/browser/web-models/reset", post(reset_web_models))
        .route("/browser/web-models/stop", post(stop_web_model))
        .route(
            "/browser/web-models/navigate",
            post(navigate_web_model_page),
        )
        .route("/browser/web-models/reload", post(reload_web_model_page))
        .route(
            "/browser/web-models/tunnel/api-key",
            get(reveal_web_model_api_key),
        )
        .route(
            "/browser/web-models/tunnel/policy",
            post(set_web_model_tool_policy),
        )
        .route(
            "/browser/web-models/connector",
            get(get_web_connector).post(configure_web_connector),
        )
        .route(
            "/browser/web-models/saved",
            get(list_web_saved_conversations),
        )
        .route(
            "/browser/web-models/saved/bind",
            post(bind_web_saved_conversation),
        )
        .route(
            "/browser/web-models/saved/sync",
            post(sync_web_saved_conversation),
        )
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
        .route(
            "/browser/sessions/{browser_session_id}/active-tab",
            post(set_active_tab),
        )
        .route("/browser/tabs/{tab_id}", delete(close_tab))
        .route("/browser/tabs/{tab_id}/activate", post(activate_tab))
        .route(
            "/browser/tabs/{tab_id}/control/release",
            post(release_user_control),
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
        .route(
            "/browser/artifacts/{session_id}/{file_name}",
            get(session_browser_artifact),
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
            in_app_browser_enabled: previous.in_app_browser_enabled,
            browser_use_enabled: previous.browser_use_enabled,
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
        let status = BrowserHostStatusSnapshot {
            revision: previous.revision,
            in_app_browser_enabled: previous.in_app_browser_enabled,
            browser_use_enabled: previous.browser_use_enabled,
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

fn bounded_connection_value(value: String, field: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 4096 {
        return Err(ApiError::InvalidInput(format!("{field} 无效")));
    }
    Ok(value.to_string())
}

fn publish_browser_host_status(state: &ApiState, status: &BrowserHostStatusSnapshot) {
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

fn browser_artifact_path(
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
    let artifact_root = state_root.join(crate::browser_tool_runtime::BROWSER_ARTIFACT_DIR);
    let path = artifact_root.join(relative);
    let canonical_root = std::fs::canonicalize(&artifact_root)
        .map_err(|error| ApiError::internal_assembly("读取浏览器 artifact 根目录失败", error))?;
    let canonical_path = std::fs::canonicalize(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ApiError::NotFound("浏览器 artifact 不存在".to_string())
        } else {
            ApiError::internal_assembly("解析浏览器 artifact 失败", error)
        }
    })?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(ApiError::Conflict("浏览器 artifact 引用越界".to_string()));
    }
    let metadata = std::fs::metadata(&canonical_path)
        .map_err(|error| ApiError::internal_assembly("读取浏览器 artifact 元数据失败", error))?;
    if !metadata.is_file() {
        return Err(ApiError::NotFound("浏览器 artifact 不是文件".to_string()));
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
            .map(|artifact_id| browser_artifact_path(state, artifact_id))
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
    state: &ApiState,
    headers: &HeaderMap,
    declared: Option<BrowserClientPlatform>,
) -> BrowserClientPlatform {
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let observed = if super::is_trusted_desktop_renderer_request(state, headers) {
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

fn require_desktop_browser_capability(
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
    host_state: String,
    last_error_code: Option<String>,
    desktop_connection_generation: u64,
    platform_capabilities: BrowserPlatformCapabilities,
}

async fn capabilities(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<BrowserCapabilitiesQuery>,
) -> Json<BrowserCapabilitiesResponse> {
    Json(browser_capabilities_response(
        &state,
        request_client_platform(&state, &headers, query.client_platform),
    ))
}

fn browser_capabilities_response(
    state: &ApiState,
    client_platform: BrowserClientPlatform,
) -> BrowserCapabilitiesResponse {
    let host = state.browser_host_status();
    let capability = state.browser_capability_snapshot();
    BrowserCapabilitiesResponse {
        revision: host.revision,
        in_app_browser_enabled: capability.in_app_browser_enabled,
        browser_use_enabled: capability.browser_use_enabled,
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
    headers: HeaderMap,
    Json(request): Json<UpdateBrowserSettingsRequest>,
) -> Result<Json<BrowserCapabilitiesResponse>, ApiError> {
    require_desktop_browser_capability(&state, &headers, request.client_platform)?;
    state.update_browser_capability_settings(
        request.in_app_browser_enabled,
        request.browser_use_enabled,
    )?;
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
async fn browser_resources(
    State(state): State<ApiState>,
) -> Result<Json<BrowserResourcesResponse>, ApiError> {
    let reconciled = state.reconcile_browser_sessions_with_session_store()?;
    if reconciled > 0 {
        state.persist_browser_durable_state_for_api()?;
    }
    Ok(Json(browser_resources_response(&state)))
}

async fn reclaim_browser_resources(
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
        // 应用级（GPT Web）页面一律不参与资源回收。
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

fn browser_resources_response(state: &ApiState) -> BrowserResourcesResponse {
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
        // App 级（GPT Web）会话没有归属的 Magi 会话：不参与会话运行态统计，
        // 也不能被当成可回收对象；它只在列表里带一条常驻说明。
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
    /// `session` 或 `app`：应用级（GPT Web）会话不属于任何 Magi 会话。
    owner: String,
    workspace_id: Option<WorkspaceId>,
    session_id: Option<SessionId>,
    profile_id: BrowserProfileId,
    lifecycle: BrowserSessionLifecycle,
    active_tab_id: Option<BrowserTabId>,
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
struct BrowserResourceTabResponse {
    tab_id: BrowserTabId,
    browser_session_id: BrowserSessionId,
    /// 应用级 Tab 没有归属会话，前端据此展示常驻说明。
    session_id: Option<SessionId>,
    workspace_id: Option<WorkspaceId>,
    scope: String,
    session_title: Option<String>,
    lifecycle: BrowserTabLifecycle,
    url: String,
    title: String,
    created_at: UtcMillis,
    updated_at: UtcMillis,
    is_current_session: bool,
    is_current_tab: bool,
    session_running: bool,
    can_reclaim: bool,
    reclaim_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserResourcesResponse {
    max_tabs: usize,
    live_tabs: usize,
    available_tabs: usize,
    reclaimable_tabs: usize,
    tabs: Vec<BrowserResourceTabResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReclaimBrowserResourcesRequest {
    tab_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserResourceReclaimSkip {
    tab_id: String,
    reason: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReclaimBrowserResourcesResponse {
    reclaimed_count: usize,
    reclaimed_tab_ids: Vec<BrowserTabId>,
    skipped: Vec<BrowserResourceReclaimSkip>,
    resources: BrowserResourcesResponse,
}

#[derive(Clone, Debug)]
struct BrowserReclaimTarget {
    tab_id: BrowserTabId,
    browser_session_id: BrowserSessionId,
    session_id: SessionId,
    workspace_id: Option<WorkspaceId>,
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
    /// 用户直接操作页面后保持控制，代理需等用户交还。
    #[serde(default)]
    user_control_held: bool,
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
            user_control_held: false,
        }
    }
}

async fn create_session(
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

/// 应用级（GPT Web）浏览器会话的固定主页 Tab 标识。
///
/// 全局只有一个应用级会话，因此主页 Tab 用一个稳定 id；推理页面由推理通道
/// 按需创建，不在这里创建。
pub const WEB_MODEL_HOME_TAB_ID: &str = "browser-tab-web-model-home";

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateAppBrowserSessionRequest {
    #[serde(default)]
    client_platform: Option<BrowserClientPlatform>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnsureWebModelHomeRequest {
    #[serde(default)]
    client_platform: Option<BrowserClientPlatform>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateAppBrowserSessionResponse {
    session: BrowserSessionResponse,
    created: bool,
}

/// `GET /browser/sessions/app`：只读读取应用级（GPT Web）浏览器会话。
///
/// 与 `POST` 的关键区别：**不创建**。应用级会话只在用户主动打开 GPT Web 视图
/// 或设置页连接时创建；窗口启动、项目 / 会话切换必须能在不产生会话的前提下
/// 判断「是否存在应用级会话」，否则每次启动都会连带创建一个后台 ChatGPT 主页。
/// 会话不存在或已失效时返回 `session = null`。
async fn get_app_session(
    State(state): State<ApiState>,
) -> Result<Json<CurrentBrowserSessionResponse>, ApiError> {
    let session = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .app_session()
        .filter(|session| session.lifecycle != BrowserSessionLifecycle::Failed)
        .cloned();
    let session = session
        .map(|session| browser_session_response(&state, session))
        .transpose()?;
    Ok(Json(CurrentBrowserSessionResponse { session }))
}

/// `POST /browser/sessions/app`：取得应用级（GPT Web）浏览器会话。
///
/// 应用级会话全局唯一、id 持久稳定、使用固定持久分区 `persist:magi-web-model`，
/// 不属于任何 Magi 会话或工作区，因此**不暴露给会话的 `browser_*` 工具**。
/// 已经存在时直接返回既有会话（`created = false`），不新建——否则会留下无主
/// 会话与多余的物理 Surface。
async fn create_app_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateAppBrowserSessionRequest>,
) -> Result<(StatusCode, Json<CreateAppBrowserSessionResponse>), ApiError> {
    require_desktop_browser_capability(&state, &headers, request.client_platform)?;
    let existing = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .app_session()
        .is_some_and(|session| session.lifecycle != BrowserSessionLifecycle::Failed);
    let session = ensure_app_browser_session(&state)?;
    let status = if existing {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((
        status,
        Json(CreateAppBrowserSessionResponse {
            session: browser_session_response(&state, session)?,
            created: !existing,
        }),
    ))
}

/// 取得或创建应用级浏览器会话（幂等）。
///
/// 全局唯一、id 持久稳定；已存在且未失败时直接复用，绝不反复新建。
/// 创建时同时建立固定主页 Tab。
fn ensure_app_browser_session(state: &ApiState) -> Result<BrowserSession, ApiError> {
    let existing = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .app_session()
        .cloned();
    if let Some(existing) = existing {
        if existing.lifecycle != BrowserSessionLifecycle::Failed {
            ensure_app_home_tab(state, &existing.browser_session_id)?;
            return Ok(existing);
        }
        // 失败会话不能复用：它的 Tab 已经没有对应的 Browser Host 页面。
        state.mutate_browser_authority(|authority| {
            authority.transition_session(
                &existing.browser_session_id,
                BrowserSessionLifecycle::Closed,
                UtcMillis::now(),
            )
        })?;
    }

    let now = UtcMillis::now();
    let browser_session_id = BrowserSessionId::new(format!(
        "browser-session-app-{}-{}",
        now.0,
        BROWSER_SESSION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let session = state.mutate_browser_authority(|authority| {
        authority.create_session(CreateBrowserSession {
            browser_session_id: browser_session_id.clone(),
            owner: BrowserSessionOwner::App,
            profile_id: BrowserProfileId::new("browser-profile-default"),
            now,
        })?;
        authority.transition_session(&browser_session_id, BrowserSessionLifecycle::Ready, now)
    })?;
    ensure_app_home_tab(state, &browser_session_id)?;
    Ok(session)
}

/// 保证应用级会话的固定主页 Tab 存在且可用。
///
/// 主页固定在 GPT Web 入口；推理页面由推理通道按需创建，不在这里创建。
fn ensure_app_home_tab(
    state: &ApiState,
    browser_session_id: &BrowserSessionId,
) -> Result<BrowserTab, ApiError> {
    let tab_id = BrowserTabId::new(WEB_MODEL_HOME_TAB_ID);
    let existing = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .tab(&tab_id)
        .cloned();
    if let Some(existing) = existing {
        if existing.browser_session_id != *browser_session_id {
            return Err(ApiError::Conflict(
                "GPT Web 主页 Tab 归属了另一应用级浏览器会话".to_string(),
            ));
        }
        if existing.lifecycle != BrowserTabLifecycle::Closed {
            return Ok(existing);
        }
        // 应用级主页使用固定逻辑 id。孤儿清理或异常退出后的恢复可能留下 Closed 行，
        // 不能把它当作仍可驱动的主页返回，否则 GET/POST 会得到空 tabs，
        // Renderer 也无法重新挂载 guest。Authority 只允许在这里按新导航代次
        // 重开该固定主页，迟到的旧 Surface 会因 revision 被拒绝。
        let now = UtcMillis::now();
        return state.mutate_browser_authority(|authority| {
            authority.reopen_closed_tab(
                browser_session_id,
                &tab_id,
                magi_web_model::chatgpt_web_home_url(),
                now,
            )
        });
    }
    let now = UtcMillis::now();
    state.mutate_browser_authority(|authority| {
        authority.create_tab(CreateBrowserTab {
            tab_id: tab_id.clone(),
            browser_session_id: browser_session_id.clone(),
            url: magi_web_model::chatgpt_web_home_url(),
            now,
        })?;
        authority.transition_tab(&tab_id, BrowserTabLifecycle::Ready, now)
    })
}

/// `GET /browser/web-models/tunnel`：工具通道状态。
///
/// 只暴露状态与「具体缺哪一项」，**不含任何凭据内容**。
async fn get_web_model_tunnel(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<crate::web_model_channel::WebModelTunnelStatus>, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    Ok(Json(state.web_model.status()))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfigureWebModelTunnelRequest {
    /// 显式清除配置（连同已保存的 API 密钥）。
    #[serde(default)]
    clear: bool,
    #[serde(default)]
    tunnel_id: String,
    /// 运行时 API 密钥（仅含 Tunnels Read + Use）。留空表示沿用已保存的密钥；
    /// Magi 把它写进用户私有文件，之后永不返回。
    #[serde(default)]
    api_key: Option<String>,
    /// 连接器权限档：`read_only` / `edit` / `edit_trusted`（没有 `exec`）。
    #[serde(default)]
    tool_profile: Option<String>,
    /// 授权方式：`ask` / `always` / `deny`。缺省沿用当前设置。
    #[serde(default)]
    approval_mode: Option<String>,
}

/// `POST /browser/web-models/tunnel`：保存配置并在后台准备 / 启动通道。
///
/// settings 里只落 Tunnel id 与权限档；API 密钥由 Magi 写进 state root 下的私有文件，
/// 不进 settings、不进日志、不进诊断输出，也不会出现在任何响应里。
async fn configure_web_model_tunnel(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<ConfigureWebModelTunnelRequest>,
) -> Result<Json<crate::web_model_channel::WebModelTunnelStatus>, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    let config = if request.clear {
        None
    } else {
        Some(crate::web_model_channel::WebModelTunnelConfig {
            tunnel_id: request.tunnel_id.trim().to_string(),
            tool_profile: match request.tool_profile.as_deref().map(str::trim) {
                None | Some("") => None,
                Some(profile @ ("read_only" | "edit" | "edit_trusted")) => {
                    Some(profile.to_string())
                }
                Some(_) => {
                    return Err(ApiError::InvalidInput(
                        "连接器权限档只能是 read_only、edit 或 edit_trusted".to_string(),
                    ));
                }
            },
            approval_mode: Some(
                parse_approval_mode(request.approval_mode.as_deref())?
                    .unwrap_or_else(|| state.web_model.status().approval_mode),
            ),
        })
    };
    let stored = match config.as_ref() {
        Some(config) => serde_json::to_value(config)
            .map_err(|error| ApiError::internal_assembly("序列化 T3 通道配置失败", error))?,
        None => Value::Null,
    };
    state
        .settings_store
        .set_section(crate::web_model_channel::WEB_MODEL_TUNNEL_SECTION, stored)
        .map_err(crate::errors::settings_persistence_error)?;
    let status = state
        .web_model
        .clone()
        .configure(&state, config, request.api_key)
        .await
        .map_err(ApiError::InvalidInput)?;
    Ok(Json(status))
}

fn parse_approval_mode(value: Option<&str>) -> Result<Option<String>, ApiError> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(mode @ ("ask" | "always" | "deny")) => Ok(Some(mode.to_string())),
        Some(_) => Err(ApiError::InvalidInput(
            "授权方式只能是 ask（每次询问）、always（始终授权）或 deny（拒绝）".to_string(),
        )),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetWebModelToolPolicyRequest {
    #[serde(default)]
    tool_profile: Option<String>,
    #[serde(default)]
    approval_mode: Option<String>,
}

/// `POST /browser/web-models/tunnel/policy`：只改权限档 / 授权方式，下一次工具调用就生效，
/// 不重启通道（身份与归属每次调用现算，不缓存）。
async fn set_web_model_tool_policy(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<SetWebModelToolPolicyRequest>,
) -> Result<Json<crate::web_model_channel::WebModelTunnelStatus>, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    let tool_profile = match request.tool_profile.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(profile @ ("read_only" | "edit" | "edit_trusted")) => Some(profile.to_string()),
        Some(_) => {
            return Err(ApiError::InvalidInput(
                "连接器权限档只能是 read_only、edit 或 edit_trusted".to_string(),
            ));
        }
    };
    let approval_mode = parse_approval_mode(request.approval_mode.as_deref())?;
    let config = state.web_model.update_policy(tool_profile, approval_mode);
    let stored = serde_json::to_value(&config)
        .map_err(|error| ApiError::internal_assembly("序列化 GPT Web 工具策略失败", error))?;
    state
        .settings_store
        .set_section(crate::web_model_channel::WEB_MODEL_TUNNEL_SECTION, stored)
        .map_err(crate::errors::settings_persistence_error)?;
    Ok(Json(state.web_model.status()))
}

/// `GET /browser/web-models/tunnel/api-key`：设置里的「显示」按钮。
///
/// 完整密钥只经这一条桌面端专用接口返回（公网隧道来源会被 `require_desktop_browser_capability`
/// 拒绝）；状态接口只给末 4 位的遮罩预览。
async fn reveal_web_model_api_key(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    let api_key = state.web_model.reveal_api_key().ok_or_else(|| {
        ApiError::not_found("尚未保存运行时 API 密钥", "web-model-tunnel-api-key")
    })?;
    Ok(Json(serde_json::json!({ "apiKey": api_key })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NavigateWebModelPageRequest {
    target: crate::web_model_ops::WebModelPageTarget,
}

/// `POST /browser/web-models/navigate`：GPT Web 标签页顶部的快捷地址。
///
/// 目标只能是白名单枚举（对话页 / OpenAI 平台 Tunnels / Runtime API keys），不接受任意地址；
/// 槽位被会话占用时拒绝（409），不打断进行中的对话。
async fn navigate_web_model_page(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<NavigateWebModelPageRequest>,
) -> Result<Json<Value>, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    state
        .open_web_model_page(request.target)
        .await
        .map_err(crate::web_model_ops::web_model_api_error)?;
    Ok(Json(serde_json::json!({ "status": "ok" })))
}

/// `POST /browser/web-models/reload`：GPT Web 标签页的手动刷新（当前页面重新加载）。
async fn reload_web_model_page(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    state
        .reload_web_model_page()
        .await
        .map_err(crate::web_model_ops::web_model_api_error)?;
    Ok(Json(serde_json::json!({ "status": "ok" })))
}

/// `POST /browser/web-models/reset`：退出登录 / 清除 Web 数据后的收口。
///
/// 顺序固定：先撤销在飞回复，再失效投影（Web 模型立即从选择器消失），最后停通道。
/// 「清理浏览数据」本身仍走既有入口，本路由只做 daemon 侧收口。
async fn reset_web_models(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    // 清除登录态 / 页面数据也必须释放 daemon 内存中的单槽位，并取消在飞推理。
    state.forget_web_model_probe();
    state.stop_web_model().await;
    state.web_model.stop_tunnel(&state).await;
    Ok(Json(serde_json::json!({ "status": "login_required" })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WebModelClientRequest {
    #[serde(default)]
    client_platform: Option<BrowserClientPlatform>,
}

/// `POST /browser/web-models/stop`：设置里的“停止”与 Tab 里的“退出”。
///
/// 取消进行中的推理、销毁物理页面、释放单槽位；登录态保留。这是显式释放 GPT Web 的唯一入口
/// （关闭右栏只隐藏，不走这里）。
async fn stop_web_model(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<WebModelClientRequest>,
) -> Result<Json<Value>, ApiError> {
    require_desktop_browser_capability(&state, &headers, request.client_platform)?;
    state.stop_web_model().await;
    Ok(Json(serde_json::json!({ "status": "stopped" })))
}

fn connector_tools_digest_path(state: &ApiState) -> Option<std::path::PathBuf> {
    state
        .runtime_persistence()?
        .state_root()
        .map(|root| root.join("web-model").join("connector-tools.digest"))
}

fn read_connector_tools_digest(state: &ApiState) -> Option<String> {
    std::fs::read_to_string(connector_tools_digest_path(state)?)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn write_connector_tools_digest(state: &ApiState) {
    let (Some(path), Some(digest)) = (
        connector_tools_digest_path(state),
        crate::mcp_service::web_slot_tool_digest(state),
    ) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(error) = std::fs::write(&path, digest) {
        tracing::warn!(%error, "记录 ChatGPT 连接器工具目录指纹失败");
    }
}

/// `GET /browser/web-models/connector`：只读检查 ChatGPT 侧 Magi 连接器。
async fn get_web_connector(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    let mut status = state
        .web_connector_status()
        .await
        .map_err(crate::web_model_ops::web_model_api_error)?;
    // ChatGPT 设置页的已安装连接器行通常不展示工具数；以槽位 MCP 的实际
    // `tools/list` 目录为准，避免把页面文案缺失误报成 Magi 工具数量不可用。
    status.tool_count = crate::mcp_service::web_slot_tool_count(&state);
    // ChatGPT 缓存的是创建 / 上次刷新时的工具列表。Magi 记下当时推给它的目录指纹：
    // 指纹和现在不同（或从没记录过），就说明 ChatGPT 手里的列表可能已经过期。
    let tools_stale = status.exists.then(|| {
        let current = crate::mcp_service::web_slot_tool_digest(&state);
        current.is_some() && current != read_connector_tools_digest(&state)
    });
    Ok(Json(serde_json::json!({
        "supported": status.supported,
        "exists": status.exists,
        "enabled": status.enabled,
        "toolCount": status.tool_count,
        "toolsStale": tools_stale,
        "reason": status.reason,
    })))
}

/// `POST /browser/web-models/connector`：在 ChatGPT 中创建 / 启用 Magi 连接器（显式用户动作）。
async fn configure_web_connector(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<WebModelClientRequest>,
) -> Result<Json<Value>, ApiError> {
    require_desktop_browser_capability(&state, &headers, request.client_platform)?;
    let outcome = state
        .configure_web_connector()
        .await
        .map_err(crate::web_model_ops::web_model_api_error)?;
    // 创建或刷新成功：ChatGPT 此刻持有的就是 Magi 当前的目录，记下指纹。
    if outcome.configured && outcome.confirmed_enabled {
        write_connector_tools_digest(&state);
    }
    Ok(Json(serde_json::json!({
        "configured": outcome.configured,
        "confirmedEnabled": outcome.confirmed_enabled,
        "reason": outcome.reason,
    })))
}

/// `GET /browser/web-models/saved`：已保存对话列表（只读）。
async fn list_web_saved_conversations(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    let entries = state
        .list_web_saved_conversations()
        .await
        .map_err(crate::web_model_ops::web_model_api_error)?;
    Ok(Json(serde_json::json!({ "conversations": entries })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BindWebSavedConversationRequest {
    session_id: String,
    conversation_id: String,
    #[serde(default)]
    client_platform: Option<BrowserClientPlatform>,
}

/// `POST /browser/web-models/saved/bind`：把已保存的 ChatGPT 对话绑定到空白会话并单向导入历史。
async fn bind_web_saved_conversation(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<BindWebSavedConversationRequest>,
) -> Result<Json<Value>, ApiError> {
    require_desktop_browser_capability(&state, &headers, request.client_platform)?;
    let session_id = SessionId::new(request.session_id.trim());
    let binding = state
        .bind_saved_web_conversation(&session_id, request.conversation_id.trim())
        .await
        .map_err(crate::web_model_ops::web_model_api_error)?;
    Ok(Json(serde_json::json!({ "binding": binding })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SyncWebSavedConversationRequest {
    session_id: String,
    #[serde(default)]
    client_platform: Option<BrowserClientPlatform>,
}

/// `POST /browser/web-models/saved/sync`：手动重新同步已保存对话（Web → Magi）。
async fn sync_web_saved_conversation(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<SyncWebSavedConversationRequest>,
) -> Result<Json<Value>, ApiError> {
    require_desktop_browser_capability(&state, &headers, request.client_platform)?;
    let session_id = SessionId::new(request.session_id.trim());
    let imported = state
        .sync_saved_web_conversation(&session_id)
        .await
        .map_err(crate::web_model_ops::web_model_api_error)?;
    Ok(Json(serde_json::json!({ "importedExchanges": imported })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WebModelRuntimeQuery {
    #[serde(default)]
    client_platform: Option<BrowserClientPlatform>,
    #[serde(default)]
    session_id: Option<String>,
}

/// `GET /browser/web-models/runtime`：只读读取在飞 Web turn 的阶段、排队位置与
/// 账号消息计数。
///
/// 这是**投影**而不是业务事实：它只反映当前 daemon 进程内存里的调度状态，
/// 进程重启即清空；正文、工具调用与终态仍只在 canonical 里。
/// 引擎长期可用性由 `POST /browser/web-models/probe` 的结果与设置分区展示，
/// 两者不得互相替代。
async fn get_web_model_runtime(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<WebModelRuntimeQuery>,
) -> Result<Json<Value>, ApiError> {
    require_desktop_browser_capability(&state, &headers, query.client_platform)?;
    let session_filter = query
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let entries = state
        .web_model
        .runtime()
        .map(|runtime| runtime.snapshot(session_filter))
        .unwrap_or_default()
        .into_iter()
        // 占用的唯一事实是槽位表：运行态条目只是投影，槽位已释放 / 换了拥有者时不得继续展示。
        .filter(|entry| {
            state
                .web_model
                .bindings()
                .and_then(|slots| slots.owner())
                .is_some_and(|owner| owner.session_id == entry.session_id)
        })
        .map(|entry| {
            // 占用提示要显示会话名称而不是内部 id：标题是 daemon 的会话事实，这里只投影。
            let title = state
                .session_store
                .session(&magi_core::SessionId::new(entry.session_id.as_str()))
                .map(|session| session.title)
                .filter(|title| !title.trim().is_empty());
            let mut value = serde_json::to_value(&entry).unwrap_or(Value::Null);
            if let Some(object) = value.as_object_mut() {
                object.insert("sessionTitle".to_string(), Value::from(title));
                // 占用者所在项目：前端据此「转到该会话」。
                let owner_project = state
                    .web_model
                    .bindings()
                    .and_then(|slots| slots.owner())
                    .map(|owner| owner.project_id)
                    .filter(|project| !project.trim().is_empty());
                object.insert("workspaceId".to_string(), Value::from(owner_project));
            }
            value
        })
        .collect::<Vec<_>>();
    Ok(Json(serde_json::json!({
        "entries": entries,
        "quota": web_model_quota_notice(&state),
    })))
}

/// 账号额度提示（A16）：Web 引擎不参与 token 聚合，只按实际发送条数计量。
///
/// 提示的是「本地观察到的发送条数」，不是账号侧的真实剩余额度——后者只有
/// ChatGPT 页面自己知道，Magi 不猜也不伪造成计费数据。
fn web_model_quota_notice(state: &ApiState) -> Value {
    let entries = state
        .web_model
        .runtime()
        .map(|runtime| runtime.snapshot(None))
        .unwrap_or_default();
    let sent_messages = entries.iter().map(|entry| entry.sent_messages).sum::<u64>();
    serde_json::json!({
        "sentMessages": sent_messages,
        "countedBy": "web_model_sends",
        "excludedFromTokenBudget": true,
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProbeWebModelRequest {
    #[serde(default)]
    client_platform: Option<BrowserClientPlatform>,
}

/// 单次只读探测的归一化结论。**只有登录与页面可用性**：不读取、不复制网页的模型菜单。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProbeWebModelResponse {
    /// `ok` / `login_required` / `site_blocked` / `selectors_drift` / `desktop_unavailable` / `failed`。
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    account_hint: String,
    site_revision: String,
}

impl ProbeWebModelResponse {
    fn status_only(status: &'static str, reason: Option<&str>) -> Self {
        Self {
            status,
            reason: reason.map(str::to_string),
            account_hint: "unknown".to_string(),
            site_revision: String::new(),
        }
    }

    /// 归一化为 daemon 的可用性投影来源。
    fn to_probe_snapshot(&self) -> crate::state::WebModelProbeSnapshot {
        crate::state::WebModelProbeSnapshot {
            status: self.status.to_string(),
            account_hint: self.account_hint.clone(),
            probed_at: UtcMillis::now().0,
        }
    }
}

/// `POST /browser/web-models/probe`：只读探测登录与页面可用性。
///
/// 失败一律 fail closed：未登录或登录过期时 `status = login_required`，GPT Web 入口随之消失，
/// 绝不用上一次的结论冒充可用。单次 `failed`（命令层失败）不改变入口状态，因此不覆盖上一条结论。
async fn probe_web_model(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<ProbeWebModelRequest>,
) -> Result<Json<ProbeWebModelResponse>, ApiError> {
    let response = run_web_model_probe(&state, &headers, request.client_platform).await?;
    // 平台页面的探测不代表 ChatGPT 的可用性，不覆盖引擎可见性的结论。
    if response.status != "failed" && response.status != "platform_page" {
        state.record_web_model_probe(response.to_probe_snapshot());
    }
    Ok(Json(response))
}

/// `POST /browser/web-models/ensure-home`：只物化应用级 GPT Web 宿主。
///
/// 应用级会话在 daemon / Desktop 重启后会保留逻辑 Tab，但真实 `<webview>`
/// 需要等 Renderer 内容槽重新挂载后才能注册。右栏组件在完成挂载后调用这条
/// 路径，**只确保当前逻辑页面的物理宿主存在**，不把页面导航回主页；否则每次
/// GPT Web 临时/已保存对话的 URL 或导航代次变化都会被 UI 投影覆盖，表现为
/// “发送后对话消失”。它不是发现接口，也不要求用户已确认 Web 模型说明。
async fn ensure_web_model_home_surface(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<EnsureWebModelHomeRequest>,
) -> Result<Json<BrowserSessionResponse>, ApiError> {
    require_desktop_browser_capability(&state, &headers, request.client_platform)?;
    let client = require_browser_host(&state)?;
    let session = ensure_app_browser_session(&state)?;
    let tab = ensure_app_home_tab(&state, &session.browser_session_id)?;
    let tab_id = tab.tab_id.clone();

    // daemon 重启后 Authority 与 Desktop 页面的导航代次可能不一致，先重新对齐绑定。
    if tab.lifecycle == BrowserTabLifecycle::Ready {
        state.resync_web_home_surface().await;
    }

    // 重启或「停止 / 退出」之后逻辑 Tab 是 suspended，Desktop 里已经没有它的 Surface：
    // EnsureSurface 只能找到已物化的 Surface，所以先走恢复路径（打开登录态主页），
    // 不能让 Renderer 在一个不存在的 Surface 上无限重试。
    if tab.lifecycle != BrowserTabLifecycle::Ready {
        state
            .restore_web_home_page()
            .await
            .map_err(crate::web_model_ops::web_model_api_error)?;
    }

    let result = require_host_success(
        client
            .request(BrowserHostCommand::EnsureSurface {
                tab_id: tab_id.clone(),
            })
            .await,
        "物化 GPT Web 宿主失败",
    )?;
    let BrowserHostCommandResult::SurfaceBinding(binding) = result else {
        return Err(ApiError::InternalAssemblyError(
            "物化 GPT Web 宿主结果缺少 Surface binding".to_string(),
        ));
    };

    state.mutate_browser_authority(|authority| {
        let (accepted, _, _) = authority.accept_primary_surface(binding, UtcMillis::now())?;
        if !accepted {
            return Err(
                magi_browser_authority::BrowserAuthorityError::PrimarySurfaceUnavailable(
                    tab_id.clone(),
                ),
            );
        }
        Ok(())
    })?;

    let session = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .app_session()
        .cloned()
        .ok_or_else(|| ApiError::Conflict("GPT Web 应用级会话在恢复期间消失".to_string()))?;
    Ok(Json(browser_session_response(&state, session)?))
}

/// 探测主体。
async fn run_web_model_probe(
    state: &ApiState,
    headers: &HeaderMap,
    client_platform: Option<BrowserClientPlatform>,
) -> Result<ProbeWebModelResponse, ApiError> {
    require_desktop_browser_capability(state, headers, client_platform)?;

    let Some(client) = state.browser_host_client() else {
        return Ok(ProbeWebModelResponse::status_only(
            "desktop_unavailable",
            None,
        ));
    };

    let session = ensure_app_browser_session(state)?;
    let tab = ensure_app_home_tab(state, &session.browser_session_id)?;
    let tab_id = tab.tab_id.clone();

    // 只读探测前只确保当前页面的真实 Surface 存在，不能再次 RestorePage。
    // RestorePage 会按 initial_url 启动导航；当唯一 WebView 已经进入临时或已保存
    // 对话时，这会把用户正在使用的网页上下文重置成主页。

    // `restore_page` 后页面可能正好经历一次 guest / Worker / navigation
    // binding 交接。探测只读且无副作用，允许在这个窗口内重新读取当前
    // binding；写入和提交路径不走这里，避免把副作用命令自动重放。
    // 与 Desktop 页面重新对齐绑定（daemon 重启后导航代次可能不一致），否则探测会一直 stale。
    state.resync_web_home_surface().await;
    let mut probe_value = None;
    let mut last_probe_reason = "probe_failed".to_string();
    let mut attempt = 0usize;
    let mut seen_page_loading = false;
    loop {
        // 页面刚物化 / 刚跳转时 origin 还是 null：这是「还没加载好」不是失败，多等一会儿（约 20 秒）；
        // 其他交接类错误仍只重试几次，真实失败要尽快如实报告。
        // 粘性：页面在加载期间错误会在「origin 尚未就绪」和「绑定刚交接」之间来回切换，
        // 只要见过页面在加载，就一直按页面加载的窗口等。
        seen_page_loading |= last_probe_reason.contains("web_model_site_origin_invalid");
        let waiting_for_page = seen_page_loading;
        let limit = if waiting_for_page {
            WEB_MODEL_PROBE_PAGE_LOAD_RETRIES
        } else {
            WEB_MODEL_PROBE_RETRIES
        };
        if attempt > limit {
            break;
        }
        if attempt > 0 {
            tokio::time::sleep(if waiting_for_page {
                WEB_MODEL_PROBE_PAGE_LOAD_DELAY
            } else {
                WEB_MODEL_PROBE_RETRY_DELAY
            })
            .await;
        }
        attempt += 1;

        let surface = tokio::time::timeout(
            BROWSER_SURFACE_WAIT_TIMEOUT,
            client.request(BrowserHostCommand::EnsureSurface {
                tab_id: tab_id.clone(),
            }),
        )
        .await;
        let Ok(Ok(reply)) = surface else {
            last_probe_reason = "surface_unavailable".to_string();
            continue;
        };
        let Some(BrowserHostCommandResult::SurfaceBinding(binding)) =
            succeeded_result(reply.response.outcome)
        else {
            last_probe_reason = "surface_unavailable".to_string();
            continue;
        };
        // 应用级 Surface 也走不激活路径：探测不抢用户的右栏。
        let accepted = state.mutate_browser_authority(|authority| {
            let (accepted, _, _) =
                authority.accept_primary_surface(binding.clone(), UtcMillis::now())?;
            Ok(accepted)
        })?;
        if !accepted {
            last_probe_reason = "probe_browser_surface_stale".to_string();
            continue;
        }

        let probe = client
            .request(BrowserHostCommand::WebModelProbe {
                tab_id: tab_id.clone(),
            })
            .await;
        let Ok(reply) = probe else {
            last_probe_reason = "probe_failed".to_string();
            continue;
        };
        let outcome = reply.response.outcome;
        match outcome {
            BrowserHostCommandOutcome::Succeeded(result) => {
                if let BrowserHostCommandResult::Json { value } = *result {
                    probe_value = Some(value);
                    break;
                }
                last_probe_reason = "probe_result_invalid".to_string();
            }
            BrowserHostCommandOutcome::Failed(error)
            | BrowserHostCommandOutcome::Indeterminate(error) => {
                tracing::warn!(
                    attempt,
                    code = %error.code,
                    diagnostic = ?error.diagnostic,
                    "GPT Web 模型探测命令失败",
                );
                last_probe_reason = format!("probe_{}", error.code);
                if !retryable_web_model_probe_error(&error.code, &error.message) {
                    break;
                }
            }
            BrowserHostCommandOutcome::Cancelled => {
                last_probe_reason = "probe_cancelled".to_string();
                break;
            }
        }
    }
    let Some(value) = probe_value else {
        return Ok(ProbeWebModelResponse::status_only(
            "failed",
            Some(&last_probe_reason),
        ));
    };

    Ok(normalize_probe(&value))
}

/// 只允许对物理 Surface 交接错误重试 Web 模型探测。selector 漂移、登录过期、
/// 风控页和站点业务错误必须立即返回，不能通过重试掩盖真实诊断。
fn retryable_web_model_probe_error(code: &str, message: &str) -> bool {
    matches!(
        code,
        "web_model_site_origin_invalid"
            | "browser_surface_stale"
            | "browser_surface_not_found"
            | "browser_surface_content_unavailable"
            | "browser_cdp_session_stale"
            | "browser_debugger_detached"
            | "browser_worker_rebind_stale"
    ) || message.contains("browser_surface_stale")
        || message.contains("browser_cdp_session_stale")
        || message.contains("browser_debugger_detached")
        || message.contains("browser_worker_rebind_stale")
}

/// 把站点探测的原始结果归一化：只吃站点适配层的登录事实，不猜、不读缓存。
/// `signed_out` → `login_required`；`blocked` → `site_blocked`；已登录但 composer 不可用 → `selectors_drift`。
fn normalize_probe(value: &Value) -> ProbeWebModelResponse {
    let login_state = value
        .get("login_state")
        .and_then(Value::as_str)
        .unwrap_or("signed_out");
    let account_hint = value
        .get("account_hint")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let site_revision = value
        .get("site_revision")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let composer_available = value
        .get("composer_available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // 不在对话页（插件、设置等二级页面）时没有输入框是正常的：会话有效即可用，
    // 真正发送前驱动会先回到对话页。
    let off_chat_page = value.get("page_kind").and_then(Value::as_str) == Some("other");
    // 标签页停在 OpenAI 平台页面（Tunnels / API keys）：不是对话页，登录态与引擎可用性不由它判断。
    let on_platform_page = value.get("page_kind").and_then(Value::as_str) == Some("platform");
    let (status, reason) = match login_state {
        _ if on_platform_page => ("platform_page", None),
        "blocked" => ("site_blocked", Some("risk_page".to_string())),
        "signed_in" if composer_available || off_chat_page => ("ok", None),
        // 登录着却找不到输入框：站点改版，必须是显式失败。
        "signed_in" => ("selectors_drift", Some("composer_missing".to_string())),
        _ => ("login_required", None),
    };
    ProbeWebModelResponse {
        status,
        reason,
        account_hint,
        site_revision,
    }
}

/// 从 Host 命令结果中取出成功值；失败 / 取消都归一为 `None`。
fn succeeded_result(outcome: BrowserHostCommandOutcome) -> Option<BrowserHostCommandResult> {
    match outcome {
        BrowserHostCommandOutcome::Succeeded(result) => Some(*result),
        _ => None,
    }
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
        .filter(|session| session.owner_workspace_id() == workspace_id.as_ref())
        .cloned();
    let session = session
        .map(|session| browser_session_response(&state, session))
        .transpose()?;
    Ok(Json(CurrentBrowserSessionResponse { session }))
}

async fn close_session(
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateBrowserTabRequest {
    initial_url: String,
    #[serde(default)]
    client_platform: Option<BrowserClientPlatform>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetActiveBrowserTabRequest {
    tab_id: String,
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
    require_desktop_browser_capability(&state, &headers, request.client_platform)?;
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
async fn set_active_tab(
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

fn finish_browser_tab_creation(
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

async fn list_annotations(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
) -> Result<Json<Vec<BrowserAnnotationResponse>>, ApiError> {
    let tab_id = BrowserTabId::new(tab_id);
    let (_tab, session) = browser_tab_scope(&state, &tab_id)?;
    require_session_scope(&session)?;
    ensure_browser_ui_ready(&state)?;
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
    headers: HeaderMap,
    Path(tab_id): Path<String>,
    Json(request): Json<CreateBrowserAnnotationRequest>,
) -> Result<(StatusCode, Json<BrowserAnnotationResponse>), ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    let tab_id = BrowserTabId::new(tab_id);
    let (_tab_before_lock, session) = browser_tab_scope(&state, &tab_id)?;
    let comment = request.comment.trim().to_string();
    if comment.is_empty() || comment.chars().count() > 4_000 {
        return Err(ApiError::InvalidInput(
            "标记内容不能为空且不能超过 4000 个字符".to_string(),
        ));
    }
    // 用户接管可能需要中断当前 Session Turn。该异步操作必须在取得全局
    // Tab 控制锁之前完成，否则会与 close_session 的
    // session_turn_lock -> Tab 控制锁顺序形成反向等待。
    ensure_user_control_for_ui(&state, &session, &tab_id).await?;
    let _control_guard = state.browser_control_locks.lock_tab(&tab_id).await;
    // 获得控制锁后重新读取权威 Tab；请求等待期间可能已完成面板调整或导航。
    let (tab, session) = browser_tab_scope(&state, &tab_id)?;
    let (kind, navigation_revision, hit_x, hit_y, region) = match request.selection {
        BrowserAnnotationSelectionRequest::Element {
            navigation_revision,
            normalized_x,
            normalized_y,
        } => {
            validate_annotation_point(normalized_x, normalized_y)?;
            (
                BrowserAnnotationKind::Element,
                navigation_revision,
                normalized_x,
                normalized_y,
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
        device_scale_factor_millis: hit.device_scale_factor_millis,
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
        require_session_scope(&session)?,
        &tab.tab_id,
        screenshot_clip,
        &tab_id,
        hit.navigation_revision,
    )
    .await?;
    let artifact_id_for_cleanup = screenshot_artifact_id.clone();
    let now = UtcMillis::now();
    let annotation = match state.mutate_browser_authority(|authority| {
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
    }) {
        Ok(annotation) => annotation,
        Err(error) => {
            let _ = delete_browser_annotation_artifact(&state, &artifact_id_for_cleanup);
            return Err(error);
        }
    };
    sync_browser_annotations_to_host(&state, &annotation.tab_id).await?;
    publish_browser_event(
        &state,
        "browser.annotation.created",
        session.owner_workspace_id(),
        require_session_scope(&session)?,
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
    navigation_revision: u64,
) -> Result<String, ApiError> {
    let reply = require_browser_host(state)?
        .request(BrowserHostCommand::Screenshot {
            tab_id: host_tab_id.clone(),
            navigation_revision,
            target: None,
            // Chromium clipped capture 会改写 fixed viewport 的滚动位置。
            // 先捕获同一真实可见 viewport，再在内存中只做无损裁剪。
            clip: None,
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
    let viewport_bytes = reply.binary.ok_or_else(|| {
        ApiError::InternalAssemblyError("浏览器标记截图缺少二进制内容".to_string())
    })?;
    let bytes = crop_browser_screenshot(
        &viewport_bytes,
        magi_browser_authority::BrowserScreenshotFormat::Png,
        clip,
        None,
    )
    .map_err(|error| ApiError::InternalAssemblyError(error.to_string()))?;
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
    let path = state_root
        .join(crate::browser_tool_runtime::BROWSER_ARTIFACT_DIR)
        .join(&relative_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| ApiError::internal_assembly("创建浏览器标记截图目录失败", error))?;
    }
    magi_core::fs_atomic::write_atomic(&path, &bytes)
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

fn delete_browser_annotation_artifact(state: &ApiState, artifact_id: &str) -> Result<(), ApiError> {
    let Some(path) = browser_artifact_path(state, artifact_id)? else {
        return Ok(());
    };
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ApiError::internal_assembly(
            "清理失败的浏览器标记截图 artifact 失败",
            error,
        )),
    }
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
                normalized_x: x,
                normalized_y: y,
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
    headers: HeaderMap,
    Path(annotation_id): Path<String>,
    Json(request): Json<UpdateBrowserAnnotationStatusRequest>,
) -> Result<Json<BrowserAnnotationResponse>, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    let annotation_id = BrowserAnnotationId::new(annotation_id);
    let annotation = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .annotation(&annotation_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("浏览器标记不存在", annotation_id.as_str()))?;
    let (_tab, session) = browser_tab_scope(&state, &annotation.tab_id)?;
    ensure_user_control_for_ui(&state, &session, &annotation.tab_id).await?;
    let _control_guard = state
        .browser_control_locks
        .lock_tab(&annotation.tab_id)
        .await;
    let updated = state.mutate_browser_authority(|authority| {
        authority.update_annotation_status(&annotation_id, request.status, UtcMillis::now())
    })?;
    sync_browser_annotations_to_host(&state, &updated.tab_id).await?;
    publish_browser_event(
        &state,
        "browser.annotation.status_changed",
        session.owner_workspace_id(),
        require_session_scope(&session)?,
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
    headers: HeaderMap,
    Path(annotation_id): Path<String>,
    Json(request): Json<UpdateBrowserAnnotationCommentRequest>,
) -> Result<Json<BrowserAnnotationResponse>, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
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
    ensure_user_control_for_ui(&state, &session, &annotation.tab_id).await?;
    let _control_guard = state
        .browser_control_locks
        .lock_tab(&annotation.tab_id)
        .await;
    let updated = state.mutate_browser_authority(|authority| {
        authority.update_annotation_comment(&annotation_id, comment, UtcMillis::now())
    })?;
    sync_browser_annotations_to_host(&state, &updated.tab_id).await?;
    publish_browser_event(
        &state,
        "browser.annotation.updated",
        session.owner_workspace_id(),
        require_session_scope(&session)?,
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
            .session(&annotation.browser_session_id)
            .ok_or_else(|| {
                ApiError::not_found("浏览器会话不存在", annotation.browser_session_id.as_str())
            })?;
        if browser_session.magi_session_id() != Some(&session_id) {
            return Err(ApiError::NotFound("浏览器标记不存在".to_string()));
        }
        annotation
            .screenshot_artifact_id
            .clone()
            .ok_or_else(|| ApiError::NotFound("浏览器标记没有截图 artifact".to_string()))?
    };
    let path = browser_artifact_path(&state, &artifact_id)?
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

/// 浏览器工具（如 browser_screenshot）写入 `browser/artifacts/<session>/` 的图片。
/// 只按「会话 + 单段文件名」寻址，供该会话的工具卡片预览；个人会话没有工作区，
/// 不能走工作区文件预览接口。
async fn session_browser_artifact(
    State(state): State<ApiState>,
    Path((session_id, file_name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let single_segment = |value: &str| {
        let mut components = std::path::Path::new(value).components();
        matches!(
            (components.next(), components.next()),
            (Some(std::path::Component::Normal(_)), None)
        )
    };
    if !single_segment(&session_id) || !single_segment(&file_name) {
        return Err(ApiError::Conflict("浏览器 artifact 引用无效".to_string()));
    }
    if state
        .session_store
        .session(&SessionId::new(session_id.as_str()))
        .is_none()
    {
        return Err(ApiError::not_found("会话不存在", session_id.as_str()));
    }
    let content_type = match std::path::Path::new(&file_name)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("png") => "image/png",
        Some("jpg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => return Err(ApiError::Conflict("浏览器 artifact 不是图片".to_string())),
    };
    let path = browser_artifact_path(&state, &format!("{session_id}/{file_name}"))?
        .ok_or_else(|| ApiError::Conflict("浏览器 artifact 存储不可用".to_string()))?;
    let bytes = std::fs::read(&path)
        .map_err(|error| ApiError::internal_assembly("读取浏览器 artifact 失败", error))?;
    Ok((
        StatusCode::OK,
        [
            ("content-type", content_type),
            ("cache-control", "private, max-age=3600"),
        ],
        bytes,
    )
        .into_response())
}

async fn activate_tab(
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
async fn release_user_control(
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

async fn close_tab(
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

/// 关闭一个浏览器 Tab 的唯一路径：先提交逻辑关闭并发布事件，让 Renderer 卸载内容槽，
/// 再请求 Host 清理物理 guest。用户关闭与任务结束清理共用。
pub(crate) async fn close_browser_tab(
    state: &ApiState,
    tab_id: &BrowserTabId,
) -> Result<(), ApiError> {
    let (tab, session) = browser_tab_scope(state, tab_id)?;
    let _control_guard = state.browser_control_locks.lock_tab(tab_id).await;
    state.mutate_browser_authority(|authority| {
        authority.transition_tab(tab_id, BrowserTabLifecycle::Closed, UtcMillis::now())
    })?;
    // 先发布逻辑关闭事实，让 App Renderer 卸载其 <webview> 内容槽；
    // Host 的 ClosePage 只能清理已经由 Renderer 放弃所有权的物理 guest。
    // 不能反过来在 Renderer 仍持有 guest 时让 Main 直接 close WebContents。
    publish_browser_event(
        state,
        "browser.tab.closed",
        session.owner_workspace_id(),
        require_session_scope(&session)?,
        serde_json::json!({
            "browser_session_id": tab.browser_session_id,
            "tab_id": tab_id,
        }),
    );
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
    Ok(())
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

enum BrowserTabActivation {
    Retry(BrowserTab),
    Completed(BrowserSession),
}

fn prepare_browser_tab_activation(
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

fn apply_browser_tab_activation(
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

fn require_browser_host(
    state: &ApiState,
) -> Result<magi_browser_authority::BrowserHostClient, ApiError> {
    state
        .browser_host_client()
        .ok_or_else(|| ApiError::Conflict("桌面浏览器控制通道尚未启动".to_string()))
}

async fn wait_for_browser_primary_surface(
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

async fn sync_browser_annotations_to_host(
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

async fn ensure_user_control_for_ui(
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
        if let Err(error) = super::sessions::interrupt_session_turn_for_browser_takeover(
            state,
            require_session_scope(session)?,
            session.owner_workspace_id(),
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

fn ensure_browser_ui_ready(state: &ApiState) -> Result<(), ApiError> {
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

fn browser_tab_response_from_authority(
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

fn host_command_succeeded(outcome: &BrowserHostCommandOutcome) -> bool {
    matches!(outcome, BrowserHostCommandOutcome::Succeeded(_))
}

/// 会话作用域的 Magi 会话身份。
///
/// App 级（GPT Web）会话不属于任何 Magi 会话，`browser_*` 路由一律不暴露它
/// 。
fn require_session_scope(session: &BrowserSession) -> Result<&SessionId, ApiError> {
    session.magi_session_id().ok_or_else(|| {
        ApiError::not_found("浏览器会话不存在", "应用级浏览器会话不支持会话作用域操作")
    })
}

fn publish_browser_event(
    state: &ApiState,
    event_type: &str,
    workspace_id: Option<&WorkspaceId>,
    session_id: &SessionId,
    payload: serde_json::Value,
) {
    state.event_bus.publish(
        EventEnvelope::domain(
            EventId::unique(format!("event-{}", event_type.replace('.', "-"))),
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

    use axum::{
        Json,
        body::to_bytes,
        extract::{Path, Query, State},
        http::{HeaderMap, StatusCode, header::USER_AGENT},
    };
    use magi_browser_authority::{
        BrowserAnnotation, BrowserAnnotationAnchor, BrowserAnnotationAuthor, BrowserAnnotationKind,
        BrowserAnnotationStatus, BrowserHostRect, BrowserProfile, BrowserProfileKind,
        BrowserRegionAnnotationAnchor, BrowserSessionLifecycle, BrowserSessionOwner,
        BrowserTabLifecycle, BrowserViewport, CreateBrowserSession, CreateBrowserTab,
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
        AnnotationArtifactQuery, BrowserAnnotationAnchorResponse, BrowserClientPlatform,
        BrowserElementAnnotationAnchorResponse, BrowserRegionAnnotationAnchorResponse,
        BrowserTabActivation, CreateAppBrowserSessionRequest, DesktopConnectionClearRequest,
        DesktopConnectionRequest, ReclaimBrowserResourcesRequest, annotation_artifact,
        apply_browser_tab_activation, browser_platform_capabilities, browser_resources_response,
        browser_session_response, clear_desktop_connection, create_app_session,
        finish_browser_tab_creation, normalize_hit_bounds, prepare_browser_tab_activation,
        reclaim_browser_resources, register_desktop_connection, require_desktop_browser_capability,
        resolve_browser_annotation_context, session_browser_artifact,
    };
    use crate::{
        errors::ApiError,
        state::{ApiState, BrowserHostConnectionConfig, RuntimeStatePersistence},
    };

    fn capability_test_state(label: &str) -> ApiState {
        ApiState::new(
            label,
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::new()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
    }

    fn desktop_request_headers(state: &ApiState) -> HeaderMap {
        let token = "desktop-renderer-test-token";
        state.set_browser_host_connection_config(Some(BrowserHostConnectionConfig {
            socket_path: "/tmp/magi-browser-test.sock".to_string(),
            auth_token: token.to_string(),
            desktop_epoch: "desktop-renderer-test".to_string(),
            parent_pid: 1,
            generation: 1,
        }));
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            "Mozilla/5.0 @magi/desktop/3.0.51 Electron/43.4.0"
                .parse()
                .unwrap(),
        );
        headers.insert("x-magi-desktop-renderer-token", token.parse().unwrap());
        headers
    }

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
                    owner: BrowserSessionOwner::Session {
                        session_id: current_session_id.clone(),
                        workspace_id: Some(WorkspaceId::new(
                            "workspace-browser-annotation-current",
                        )),
                    },
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

    #[test]
    fn annotation_hit_bounds_are_normalized_against_the_chromium_viewport() {
        let normalized = normalize_hit_bounds(
            1280,
            720,
            BrowserHostRect {
                x: 640.0,
                y: 180.0,
                width: 120.0,
                height: 40.0,
            },
        )
        .expect("valid Chromium element bounds should normalize");

        assert_eq!(normalized.x, 0.5);
        assert_eq!(normalized.y, 0.25);
        assert!((normalized.width - 120.0 / 1280.0).abs() < 1e-12);
        assert!((normalized.height - 40.0 / 720.0).abs() < 1e-12);
    }

    #[test]
    fn annotation_hit_bounds_reject_an_empty_or_unavailable_viewport() {
        assert!(
            normalize_hit_bounds(
                0,
                720,
                BrowserHostRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
            )
            .is_err()
        );
        assert!(
            normalize_hit_bounds(
                1280,
                720,
                BrowserHostRect {
                    x: 10.0,
                    y: 20.0,
                    width: 0.0,
                    height: 40.0,
                },
            )
            .is_err()
        );
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
    async fn desktop_connection_registration_releases_owner_after_clear() {
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
        .await;
        assert!(matches!(replacement, Err(ApiError::Conflict(_))));

        let _ = clear_desktop_connection(
            axum::extract::State(state.clone()),
            axum::Json(DesktopConnectionClearRequest {
                desktop_epoch: "desktop-second".to_string(),
                parent_pid: 103,
                generation: 1,
            }),
        )
        .await
        .expect("stale clear should be an idempotent no-op");
        let current = state
            .browser_host_connection_config()
            .expect("first owner should remain registered");
        assert_eq!(current.desktop_epoch, "desktop-first");
        assert_eq!(current.generation, 1);

        let _ = clear_desktop_connection(
            axum::extract::State(state.clone()),
            axum::Json(DesktopConnectionClearRequest {
                desktop_epoch: "desktop-first".to_string(),
                parent_pid: 101,
                generation: 1,
            }),
        )
        .await
        .expect("owner should clear its active connection");
        assert!(state.browser_host_connection_config().is_none());

        let takeover_after_clear = register_desktop_connection(
            axum::extract::State(state.clone()),
            axum::Json(DesktopConnectionRequest {
                socket_path: "/tmp/magi-second.sock".to_string(),
                auth_token: "second-token".to_string(),
                desktop_epoch: "desktop-second".to_string(),
                parent_pid: 103,
                expected_generation: 1,
            }),
        )
        .await;
        let takeover_after_clear = takeover_after_clear
            .expect("a new Desktop owner should register after the previous owner clears");
        assert_eq!(takeover_after_clear.0.desktop_connection_generation, 2);

        let stale_clear = clear_desktop_connection(
            axum::extract::State(state.clone()),
            axum::Json(DesktopConnectionClearRequest {
                desktop_epoch: "desktop-first".to_string(),
                parent_pid: 101,
                generation: 1,
            }),
        )
        .await
        .expect("stale clear after takeover should remain idempotent");
        assert_eq!(stale_clear.0.desktop_connection_generation, 2);
        assert_eq!(
            state
                .browser_host_connection_config()
                .expect("replacement Desktop should remain registered")
                .desktop_epoch,
            "desktop-second"
        );
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

    #[tokio::test]
    async fn annotation_artifact_is_session_scoped_and_survives_persistence_boundary() {
        let (state, session_id, other_session_id, annotation_id) = annotation_fixture();
        let durable = state
            .browser_authority
            .lock()
            .expect("browser authority lock should hold")
            .durable_state();
        let state_root = tempfile::tempdir()
            .expect("artifact state root should create")
            .keep();
        std::fs::create_dir_all(
            state_root.join("browser/artifacts/session-browser-annotation-current"),
        )
        .expect("artifact directory should create");
        std::fs::write(
            state_root.join("browser/state.json"),
            serde_json::to_vec(&durable).expect("browser durable state should serialize"),
        )
        .expect("browser durable state should write");
        let artifact_bytes = vec![137, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3];
        std::fs::write(
            state_root.join("browser/artifacts/session-browser-annotation-current/annotation.png"),
            &artifact_bytes,
        )
        .expect("annotation artifact should write");
        let state = state.with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
            state_root.clone(),
            state_root.join("workspaces.json"),
            state_root.join("knowledge.json"),
        )));
        state
            .mutate_browser_authority(|authority| {
                authority.transition_session(
                    &BrowserSessionId::new("browser-session-annotation-current"),
                    BrowserSessionLifecycle::Ready,
                    UtcMillis(8),
                )?;
                authority.transition_tab(
                    &BrowserTabId::new("browser-tab-annotation-current"),
                    BrowserTabLifecycle::Ready,
                    UtcMillis(8),
                )?;
                Ok(())
            })
            .expect("restored browser annotation state should become ready");

        // 工具截图按「会话 + 单段文件名」读取：只能读已存在会话自己目录下的图片。
        state
            .session_store
            .create_session(session_id.clone(), "截图会话")
            .expect("session should create");
        let screenshot = session_browser_artifact(
            State(state.clone()),
            Path((session_id.to_string(), "annotation.png".to_string())),
        )
        .await
        .expect("session should read its own browser artifact");
        assert_eq!(
            screenshot
                .headers()
                .get("content-type")
                .map(|value| value.as_bytes()),
            Some(b"image/png".as_slice())
        );
        for (artifact_session, file_name) in [
            (session_id.to_string(), "../annotation.png".to_string()),
            (session_id.to_string(), "state.json".to_string()),
            ("..".to_string(), "state.json".to_string()),
            (other_session_id.to_string(), "annotation.png".to_string()),
        ] {
            assert!(
                session_browser_artifact(
                    State(state.clone()),
                    Path((artifact_session.clone(), file_name.clone())),
                )
                .await
                .is_err(),
                "{artifact_session}/{file_name} must be rejected"
            );
        }

        let response = annotation_artifact(
            State(state.clone()),
            Path(annotation_id.to_string()),
            Query(AnnotationArtifactQuery {
                session_id: session_id.to_string(),
            }),
        )
        .await
        .expect("current session should read its annotation artifact");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("image/png")
        );
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("artifact response body should read")
                .as_ref(),
            artifact_bytes.as_slice()
        );

        assert!(matches!(
            annotation_artifact(
                State(state.clone()),
                Path(annotation_id.to_string()),
                Query(AnnotationArtifactQuery {
                    session_id: other_session_id.to_string(),
                }),
            )
            .await,
            Err(ApiError::NotFound(_))
        ));
        assert!(matches!(
            super::browser_artifact_path(&state, "../outside.png"),
            Err(ApiError::Conflict(_))
        ));
        assert!(matches!(
            super::browser_artifact_path(&state, "missing.png"),
            Err(ApiError::NotFound(_))
        ));

        let (annotation_without_artifact, restored_snapshot_revision) = {
            let authority = state
                .browser_authority
                .lock()
                .expect("browser authority lock should hold");
            let annotation = authority
                .annotation(&annotation_id)
                .expect("fixture annotation should exist")
                .clone();
            let snapshot_revision = authority
                .tab(&BrowserTabId::new("browser-tab-annotation-current"))
                .expect("fixture tab should exist")
                .snapshot_revision;
            (annotation, snapshot_revision)
        };
        state
            .mutate_browser_authority(|authority| {
                let mut annotation = annotation_without_artifact.clone();
                annotation.annotation_id =
                    BrowserAnnotationId::new("browser-annotation-without-artifact");
                annotation.screenshot_artifact_id = None;
                annotation.sequence = 0;
                match &mut annotation.anchor {
                    BrowserAnnotationAnchor::Region(anchor) => {
                        anchor.snapshot_revision = restored_snapshot_revision;
                    }
                    BrowserAnnotationAnchor::Element(anchor) => {
                        anchor.snapshot_revision = restored_snapshot_revision;
                    }
                }
                authority.create_annotation(annotation)
            })
            .expect("annotation without artifact should create");
        assert!(matches!(
            annotation_artifact(
                State(state.clone()),
                Path("browser-annotation-without-artifact".to_string()),
                Query(AnnotationArtifactQuery {
                    session_id: session_id.to_string(),
                }),
            )
            .await,
            Err(ApiError::NotFound(_))
        ));

        state
            .mutate_browser_authority(|authority| {
                authority.transition_session(
                    &BrowserSessionId::new("browser-session-annotation-current"),
                    BrowserSessionLifecycle::Closed,
                    UtcMillis(9),
                )?;
                Ok(())
            })
            .expect("closing the browser session should succeed");
        let response_after_browser_close = annotation_artifact(
            State(state),
            Path(annotation_id.to_string()),
            Query(AnnotationArtifactQuery {
                session_id: session_id.to_string(),
            }),
        )
        .await
        .expect("historical annotation artifact should survive browser session close");
        assert_eq!(response_after_browser_close.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response_after_browser_close.into_body(), usize::MAX)
                .await
                .expect("closed-session artifact response body should read")
                .as_ref(),
            artifact_bytes.as_slice()
        );
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
                    owner: BrowserSessionOwner::Session {
                        session_id,
                        workspace_id: None,
                    },
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
        let state = capability_test_state("browser-client-platform-test");
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, "Mozilla/5.0".parse().unwrap());
        let error = require_desktop_browser_capability(&state, &headers, None)
            .expect_err("web clients must not operate the Desktop browser host");
        assert!(matches!(error, ApiError::CapabilityUnavailable { .. }));

        headers.insert(
            USER_AGENT,
            "Mozilla/5.0 @magi/desktop/3.0.51 Electron/43.4.0"
                .parse()
                .unwrap(),
        );
        let guest_error = require_desktop_browser_capability(&state, &headers, None)
            .expect_err("Electron guest without the App Renderer token must remain untrusted");
        assert!(matches!(
            guest_error,
            ApiError::CapabilityUnavailable { .. }
        ));

        headers = desktop_request_headers(&state);
        require_desktop_browser_capability(&state, &headers, None)
            .expect("Electron clients should be able to operate the Desktop browser host");
    }

    #[test]
    fn declared_desktop_platform_cannot_elevate_web_or_tunnel_requests() {
        let state = capability_test_state("browser-client-elevation-test");
        let mut web_headers = HeaderMap::new();
        web_headers.insert(USER_AGENT, "Mozilla/5.0".parse().unwrap());
        let web_error = require_desktop_browser_capability(
            &state,
            &web_headers,
            Some(BrowserClientPlatform::Desktop),
        )
        .expect_err("clientPlatform must not elevate a web request");
        assert!(matches!(web_error, ApiError::CapabilityUnavailable { .. }));

        let mut tunnel_headers = desktop_request_headers(&state);
        tunnel_headers.insert("cf-ray", "test".parse().unwrap());
        let tunnel_error = require_desktop_browser_capability(
            &state,
            &tunnel_headers,
            Some(BrowserClientPlatform::Desktop),
        )
        .expect_err("public tunnel requests must remain record-only");
        assert!(matches!(
            tunnel_error,
            ApiError::CapabilityUnavailable { .. }
        ));
    }

    #[test]
    fn explicit_non_desktop_platform_cannot_be_overridden_by_a_desktop_user_agent() {
        let state = capability_test_state("browser-client-downgrade-test");
        let headers = desktop_request_headers(&state);
        let error = require_desktop_browser_capability(
            &state,
            &headers,
            Some(BrowserClientPlatform::MobileWeb),
        )
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
                    owner: BrowserSessionOwner::Session {
                        session_id: session_id.clone(),
                        workspace_id: None,
                    },
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

    #[test]
    fn late_creation_failure_cannot_overwrite_ready_tab() {
        let (state, session_id, _, _) = annotation_fixture();
        let browser_session_id = BrowserSessionId::new("browser-session-annotation-current");
        let tab_id = BrowserTabId::new("browser-tab-annotation-current");

        finish_browser_tab_creation(
            &state,
            &None,
            &session_id,
            &browser_session_id,
            &tab_id,
            "late create failure",
        );

        let tab = state
            .browser_authority
            .lock()
            .expect("browser authority lock should hold")
            .tab(&tab_id)
            .cloned()
            .expect("ready tab should remain in authority");
        assert_eq!(tab.lifecycle, BrowserTabLifecycle::Ready);
    }

    #[test]
    fn immediate_activation_converges_a_creating_tab_to_ready() {
        let (state, _, _, _) = annotation_fixture();
        let browser_session_id = BrowserSessionId::new("browser-session-annotation-current");
        let tab_id = BrowserTabId::new("browser-tab-activation-creating");
        state
            .mutate_browser_authority(|authority| {
                authority.create_tab(CreateBrowserTab {
                    tab_id: tab_id.clone(),
                    browser_session_id,
                    url: "https://example.com/activation".to_string(),
                    now: UtcMillis(5),
                })?;
                Ok(())
            })
            .expect("creating tab fixture should succeed");

        let tab = state
            .mutate_browser_authority(|authority| {
                prepare_browser_tab_activation(authority, &tab_id)
            })
            .expect("creating tab should be eligible for activation");
        assert_eq!(tab.lifecycle, BrowserTabLifecycle::Creating);

        let resolution = state
            .mutate_browser_authority(|authority| {
                apply_browser_tab_activation(
                    authority,
                    &tab_id,
                    magi_browser_authority::BrowserHostPageState {
                        tab_id: tab_id.clone(),
                        url: "https://example.com/activation".to_string(),
                        origin: Some("https://example.com".to_string()),
                        title: "Activation".to_string(),
                        navigation_revision: 0,
                    },
                )
            })
            .expect("creating tab should converge after host restore");
        assert!(matches!(resolution, BrowserTabActivation::Completed(_)));
        assert_eq!(
            state
                .browser_authority
                .lock()
                .expect("browser authority lock should hold")
                .tab(&tab_id)
                .expect("activated tab should remain in authority")
                .lifecycle,
            BrowserTabLifecycle::Ready
        );
    }

    #[test]
    fn activation_retries_when_creation_failure_wins_the_authority_race() {
        let (state, session_id, _, _) = annotation_fixture();
        let browser_session_id = BrowserSessionId::new("browser-session-annotation-current");
        let tab_id = BrowserTabId::new("browser-tab-activation-race");
        state
            .mutate_browser_authority(|authority| {
                authority.create_tab(CreateBrowserTab {
                    tab_id: tab_id.clone(),
                    browser_session_id: browser_session_id.clone(),
                    url: "https://example.com/race".to_string(),
                    now: UtcMillis(5),
                })
            })
            .expect("creating tab fixture should succeed");

        let creating = state
            .mutate_browser_authority(|authority| {
                prepare_browser_tab_activation(authority, &tab_id)
            })
            .expect("creating tab should be eligible for activation");
        assert_eq!(creating.lifecycle, BrowserTabLifecycle::Creating);

        finish_browser_tab_creation(
            &state,
            &None,
            &session_id,
            &browser_session_id,
            &tab_id,
            "creation failed while activation was restoring",
        );

        let retry = state
            .mutate_browser_authority(|authority| {
                apply_browser_tab_activation(
                    authority,
                    &tab_id,
                    magi_browser_authority::BrowserHostPageState {
                        tab_id: tab_id.clone(),
                        url: "https://example.com/race".to_string(),
                        origin: Some("https://example.com".to_string()),
                        title: "Race".to_string(),
                        navigation_revision: creating.navigation_revision,
                    },
                )
            })
            .expect("crashed activation should request a retry");
        let suspended = match retry {
            BrowserTabActivation::Retry(tab) => tab,
            BrowserTabActivation::Completed(_) => {
                panic!("a crashed tab must retry with suspended revisions")
            }
        };
        assert_eq!(suspended.lifecycle, BrowserTabLifecycle::Suspended);

        let resolution = state
            .mutate_browser_authority(|authority| {
                apply_browser_tab_activation(
                    authority,
                    &tab_id,
                    magi_browser_authority::BrowserHostPageState {
                        tab_id: tab_id.clone(),
                        url: "https://example.com/race".to_string(),
                        origin: Some("https://example.com".to_string()),
                        title: "Race".to_string(),
                        navigation_revision: suspended.navigation_revision,
                    },
                )
            })
            .expect("retried suspended tab should converge after restore");
        assert!(matches!(resolution, BrowserTabActivation::Completed(_)));
        assert_eq!(
            state
                .browser_authority
                .lock()
                .expect("browser authority lock should hold")
                .tab(&tab_id)
                .expect("restored tab should remain in authority")
                .lifecycle,
            BrowserTabLifecycle::Ready
        );
    }

    #[tokio::test]
    async fn app_browser_session_is_singleton_with_a_stable_id_and_home_tab() {
        let state = capability_test_state("browser-app-session-test");
        state
            .mutate_browser_authority(|authority| {
                authority.register_profile(BrowserProfile {
                    profile_id: BrowserProfileId::new("browser-profile-default"),
                    kind: BrowserProfileKind::ManagedDefault,
                    data_path: tempfile::tempdir()
                        .expect("app session profile should create")
                        .keep(),
                    created_at: UtcMillis(1),
                    updated_at: UtcMillis(1),
                })
            })
            .expect("browser profile should register");
        let headers = desktop_request_headers(&state);

        let (status, Json(first)) = create_app_session(
            State(state.clone()),
            headers.clone(),
            Json(CreateAppBrowserSessionRequest::default()),
        )
        .await
        .expect("app session should create");
        assert_eq!(status, StatusCode::CREATED);
        assert!(first.created);
        assert_eq!(first.session.owner, "app");
        // 应用级会话不属于任何 Magi 会话或工作区。
        assert!(first.session.session_id.is_none());
        assert!(first.session.workspace_id.is_none());

        let (status, Json(second)) = create_app_session(
            State(state.clone()),
            headers,
            Json(CreateAppBrowserSessionRequest::default()),
        )
        .await
        .expect("app session should be reused");
        assert_eq!(status, StatusCode::OK);
        assert!(!second.created);
        assert_eq!(
            second.session.browser_session_id,
            first.session.browser_session_id
        );
        // 应用级会话固定带一个主页 Tab，且固定在 GPT Web 入口。
        assert_eq!(second.session.tabs.len(), 1);
        assert_eq!(
            second.session.tabs[0].url,
            magi_web_model::chatgpt_web_home_url()
        );
    }

    #[tokio::test]
    async fn app_browser_session_sessions_are_not_visible_to_session_browser_routes() {
        let state = capability_test_state("browser-app-session-scope-test");
        state
            .mutate_browser_authority(|authority| {
                authority.register_profile(BrowserProfile {
                    profile_id: BrowserProfileId::new("browser-profile-default"),
                    kind: BrowserProfileKind::ManagedDefault,
                    data_path: tempfile::tempdir()
                        .expect("app session profile should create")
                        .keep(),
                    created_at: UtcMillis(1),
                    updated_at: UtcMillis(1),
                })
            })
            .expect("browser profile should register");
        let headers = desktop_request_headers(&state);
        let Json(created) = create_app_session(
            State(state.clone()),
            headers,
            Json(CreateAppBrowserSessionRequest::default()),
        )
        .await
        .expect("app session should create")
        .1;

        // 应用级会话不参与任何会话的浏览器会话解析。
        let authority = state
            .browser_authority
            .lock()
            .expect("browser authority lock poisoned");
        assert!(
            authority
                .session_for_magi_session(&SessionId::new("session-app-scope"))
                .is_none()
        );
        let resource = authority
            .snapshot()
            .sessions
            .into_iter()
            .find(|session| session.browser_session_id == created.session.browser_session_id)
            .expect("app session should exist");
        assert!(resource.is_app_scope());
    }

    #[tokio::test]
    async fn reclaim_resources_closes_inactive_tab_even_without_browser_host() {
        let state = ApiState::new(
            "browser-resource-reclaim-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::new()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let session_id = SessionId::new("session-browser-resource-reclaim");
        let browser_session_id = BrowserSessionId::new("browser-session-resource-reclaim");
        let tab_id = BrowserTabId::new("browser-tab-resource-reclaim");
        let profile_id = BrowserProfileId::new("browser-profile-resource-reclaim");

        state
            .mutate_browser_authority(|authority| {
                authority.register_profile(BrowserProfile {
                    profile_id: profile_id.clone(),
                    kind: BrowserProfileKind::ManagedDefault,
                    data_path: tempfile::tempdir()
                        .expect("resource profile should create")
                        .keep(),
                    created_at: UtcMillis(1),
                    updated_at: UtcMillis(1),
                })?;
                authority.create_session(CreateBrowserSession {
                    browser_session_id: browser_session_id.clone(),
                    owner: BrowserSessionOwner::Session {
                        session_id: session_id.clone(),
                        workspace_id: None,
                    },
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
                authority.transition_tab(&tab_id, BrowserTabLifecycle::Suspended, UtcMillis(3))?;
                Ok(())
            })
            .expect("resource fixture should create");

        let before = browser_resources_response(&state);
        assert_eq!(before.live_tabs, 1);
        assert_eq!(before.reclaimable_tabs, 1);
        assert!(before.tabs[0].can_reclaim);

        let response = reclaim_browser_resources(
            axum::extract::State(state.clone()),
            desktop_request_headers(&state),
            axum::Json(ReclaimBrowserResourcesRequest {
                tab_ids: vec![tab_id.to_string()],
            }),
        )
        .await
        .expect("resource reclamation should not require a live Host");
        assert_eq!(response.0.reclaimed_count, 1);
        assert_eq!(response.0.reclaimed_tab_ids, vec![tab_id.clone()]);
        assert_eq!(response.0.resources.live_tabs, 0);
        assert_eq!(response.0.resources.reclaimable_tabs, 0);

        let authority = state
            .browser_authority
            .lock()
            .expect("browser authority lock should hold");
        assert_eq!(
            authority
                .session(&browser_session_id)
                .expect("browser session should be retained")
                .lifecycle,
            BrowserSessionLifecycle::Ready
        );
        assert!(
            authority
                .session(&browser_session_id)
                .expect("browser session should be retained")
                .tab_ids
                .is_empty()
        );
        assert_eq!(
            authority
                .tab(&tab_id)
                .expect("closed tab should remain in audit history")
                .lifecycle,
            BrowserTabLifecycle::Closed
        );
    }
    fn signed_in_probe() -> serde_json::Value {
        serde_json::json!({
            "site_revision": "chatgpt-web-7",
            "login_state": "signed_in",
            "composer_available": true,
            "account_hint": "plus",
        })
    }

    #[test]
    fn probe_requires_login_and_hides_the_engine_when_signed_out() {
        // 未登录 / 登录过期必须 fail closed：不得用上一次缓存的结论冒充可用。
        let mut signed_out = signed_in_probe();
        signed_out["login_state"] = serde_json::Value::String("signed_out".to_string());
        let response = super::normalize_probe(&signed_out);
        assert_eq!(response.status, "login_required");
        assert!(!response.to_probe_snapshot().is_visible_in_picker());
    }

    #[test]
    fn probe_maps_the_risk_page_to_site_blocked() {
        let mut blocked = signed_in_probe();
        blocked["login_state"] = serde_json::Value::String("blocked".to_string());
        let response = super::normalize_probe(&blocked);
        assert_eq!(response.status, "site_blocked");
        assert_eq!(response.reason.as_deref(), Some("risk_page"));
        assert!(!response.to_probe_snapshot().is_visible_in_picker());
    }

    #[test]
    fn probe_on_a_secondary_page_without_a_composer_is_still_signed_in() {
        // 插件、设置等二级页面没有输入框是正常的：会话有效就可用，不是改版也不是未登录。
        let mut secondary = signed_in_probe();
        secondary["composer_available"] = serde_json::Value::Bool(false);
        secondary["page_kind"] = serde_json::Value::String("other".to_string());
        let response = super::normalize_probe(&secondary);
        assert_eq!(response.status, "ok");
        assert!(response.reason.is_none());
    }

    #[test]
    fn probe_on_an_openai_platform_page_is_neutral_and_never_recorded() {
        // 标签页停在 Tunnels / API keys 页：既不是登录，也不是改版，更不能覆盖引擎可见性。
        let mut platform = signed_in_probe();
        platform["login_state"] = serde_json::Value::String("signed_out".to_string());
        platform["composer_available"] = serde_json::Value::Bool(false);
        platform["page_kind"] = serde_json::Value::String("platform".to_string());
        let response = super::normalize_probe(&platform);
        assert_eq!(response.status, "platform_page");
        assert!(response.reason.is_none());
    }

    #[test]
    fn probe_reports_selector_drift_when_signed_in_without_a_composer() {
        // 登录着却找不到输入框是显式失败，不是“可用”。
        let mut drifted = signed_in_probe();
        drifted["composer_available"] = serde_json::Value::Bool(false);
        let response = super::normalize_probe(&drifted);
        assert_eq!(response.status, "selectors_drift");
        assert_eq!(response.reason.as_deref(), Some("composer_missing"));
        assert!(!response.to_probe_snapshot().is_visible_in_picker());
    }

    #[test]
    fn probe_of_a_signed_in_page_only_reports_login_and_never_a_model_list() {
        let response = super::normalize_probe(&signed_in_probe());
        assert_eq!(response.status, "ok");
        assert_eq!(response.account_hint, "plus");
        assert!(response.to_probe_snapshot().is_visible_in_picker());
        let value = serde_json::to_value(&response).expect("probe response serializes");
        assert!(
            value.get("engines").is_none() && value.get("models").is_none(),
            "探测结论里不得出现网页的模型菜单"
        );
    }
}
