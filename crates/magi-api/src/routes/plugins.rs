use crate::{errors::ApiError, state::ApiState};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{
        Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use base64::Engine as _;
use futures_util::{Stream, StreamExt, future, stream};
use magi_app_server_protocol::generated::{
    PluginCenterConfig, PluginCenterEntry, PluginCenterEntryState, PluginCenterInstallRequest,
    PluginCenterKey, PluginCenterProjection,
};
use magi_core::EventId;
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_plugin_system::{
    CatalogEntry, MAX_CATALOG_BYTES, MAX_PACKAGE_BYTES, PluginCatalog, PluginManager,
    PluginPackage, PluginSource, TrustedKey,
};
use serde::Deserialize;
use serde_json::Value;
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::BroadcastStream;

/// 资源订阅检查插件是否仍激活并持有授权的间隔；失效后立即关闭订阅。
const RESOURCE_SUBSCRIPTION_RECHECK: Duration = Duration::from_millis(500);
/// JSON 上传承载合法归档的 base64（每三字节占四字符），另留有界元数据空间。
const MAX_INSTALL_REQUEST_BYTES: usize = MAX_PACKAGE_BYTES.div_ceil(3) * 4 + 64 * 1024;
/// 路由 JSON 可承载最多 2 MiB 的 base64 字节及有界头部；其他 HTTP 接口保持原上限。
const MAX_ROUTE_REQUEST_BYTES: usize = 3 * 1024 * 1024 + 64 * 1024;
const VIEW_CONTENT_SECURITY_POLICY: &str = "sandbox allow-scripts allow-downloads; default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; object-src 'none'; frame-ancestors 'self'; base-uri 'none'; form-action 'none'; connect-src 'none'";
const TRUSTED_VIEW_CONTENT_SECURITY_POLICY: &str = "sandbox allow-scripts allow-downloads; default-src 'none'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; object-src 'none'; frame-ancestors 'self'; base-uri 'none'; form-action 'none'; connect-src 'none'";

mod assets;
mod center;
mod download;
mod operations;
pub use operations::PluginOperations;
mod upstream;
#[cfg(test)]
use center::{OFFICIAL_PLUGIN_CENTER_URL, is_official_center, read_center_config};
use center::{center, center_install, guide, write_center_config};
use download::download_bytes;

type InstallRequest = magi_app_server_protocol::PluginInstallRequest;
type ActivationRequest = magi_app_server_protocol::PluginActivationRequest;
type AuthorizeRequest = magi_app_server_protocol::PluginAuthorizeRequest;
type ResourceWriteRequest = magi_app_server_protocol::PluginResourceWriteRequest;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginQuery {}

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/plugins", get(list))
        .route("/plugins/center", get(center))
        .route(
            "/plugins/center/config",
            axum::routing::put(write_center_config),
        )
        .route("/plugins/center/install", post(center_install))
        .route("/plugins/upstream", get(upstream::search))
        .route("/plugins/upstream/icon", get(upstream::icons::icon))
        .route("/plugins/upstream/preview", post(upstream::preview))
        .route("/plugins/upstream/install", post(upstream::install))
        .route("/plugins/guide", get(guide))
        .route("/plugins/commands", get(commands))
        .route("/plugins/{id}/jobs", get(list_jobs).post(create_job))
        .route("/plugins/{id}/jobs/{job}/cancel", post(cancel_job))
        .route("/plugins/manifests", get(manifests))
        .route(
            "/plugins/{id}/resources/{resource}/events",
            get(resource_events),
        )
        .route("/plugins/{id}/ui/{*path}", get(resource))
        .route(
            "/plugins/{id}/ui-v/{digest}/{*path}",
            get(versioned_resource),
        )
        .route("/plugins/{id}/icon", get(icon))
        .route(
            "/plugins/{id}/assets/{digest}/{instance}/{*path}",
            get(assets::resource),
        )
        .route(
            "/plugins/{id}/routes",
            post(call_route).layer(DefaultBodyLimit::max(MAX_ROUTE_REQUEST_BYTES)),
        )
        .route("/plugins/{id}/composer/admit", post(admit_composer))
        .route("/plugins/{id}/views/admit", post(admit_view))
        .route("/plugins/{id}/settings/schema", get(settings_schema))
        .route(
            "/plugins/{id}/resources/{resource}",
            get(read_resource).put(write_resource),
        )
        .route(
            "/plugins/{id}/settings",
            get(read_settings).put(write_settings),
        )
        .route(
            "/plugins/preview",
            post(preview).layer(DefaultBodyLimit::max(MAX_INSTALL_REQUEST_BYTES)),
        )
        .route(
            "/plugins/install",
            post(install).layer(DefaultBodyLimit::max(MAX_INSTALL_REQUEST_BYTES)),
        )
        .route(
            "/plugins/upgrade",
            post(upgrade).layer(DefaultBodyLimit::max(MAX_INSTALL_REQUEST_BYTES)),
        )
        .route("/plugins/{id}/enable", post(enable))
        .route("/plugins/{id}/authorize", post(authorize))
        .route("/plugins/{id}/activate", post(activate))
        .route("/plugins/{id}/deactivate", post(deactivate))
        .route("/plugins/{id}/disable", post(disable))
        .route("/plugins/{id}/uninstall", post(uninstall))
}

async fn list(
    State(state): State<ApiState>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    let manager = state
        .plugin_manager
        .lock()
        .map_err(|_| ApiError::internal_assembly("读取插件状态失败", "插件锁已损坏"))?;
    snapshot(&state, &manager)
}

async fn manifests(
    State(state): State<ApiState>,
    Query(_query): Query<PluginQuery>,
) -> Result<Json<Vec<magi_app_server_protocol::PluginManifest>>, ApiError> {
    let manager = locked(&state, "读取插件清单")?;
    let manifests = manager.manifests().map_err(plugin_error)?;
    Ok(Json(manifests))
}

async fn resource_events(
    State(state): State<ApiState>,
    Path((id, resource)): Path<(String, String)>,
    Query(_query): Query<PluginQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let (initial_resource, receiver) = {
        let manager = locked(&state, "订阅插件资源")?;
        let initial_resource = manager
            .read_resource(&id, &resource)
            .map_err(plugin_error)?;
        // The manager lock serializes this snapshot with every resource writer. The
        // event-bus cut is then registered before the lock is released, so a writer
        // can only appear in the initial snapshot or in the live stream.
        let (_snapshot, receiver) = state.event_bus.snapshot_and_subscribe();
        (initial_resource, receiver)
    };
    let initial = stream::once(future::ready(Ok::<Event, Infallible>(
        resource_sse_snapshot(&id, &resource, initial_resource),
    )));
    // 订阅随插件停用、撤权或升级失效：不能让已被撤销的视图继续收到资源变更。
    let revoked = resource_subscription_revoked(
        std::sync::Arc::clone(&state.plugin_manager),
        id.clone(),
        resource.clone(),
    );
    let live = BroadcastStream::new(receiver)
        .filter_map(move |event| {
            future::ready(match event {
                Ok(envelope) => plugin_resource_event_matches(&envelope, &id, &resource)
                    .then(|| Ok::<Event, Infallible>(resource_sse_event(envelope))),
                Err(_) => Some(Ok::<Event, Infallible>(resource_sse_reset())),
            })
        })
        .take_until(revoked);
    let closed = stream::once(future::ready(
        Ok::<Event, Infallible>(resource_sse_closed()),
    ));
    Ok(Sse::new(initial.chain(live).chain(closed))
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(5))))
}

/// 轮询到订阅所需的激活状态、声明和授权不再成立为止。
async fn resource_subscription_revoked(
    manager: std::sync::Arc<std::sync::Mutex<PluginManager>>,
    id: String,
    resource: String,
) {
    loop {
        tokio::time::sleep(RESOURCE_SUBSCRIPTION_RECHECK).await;
        let allowed = manager
            .lock()
            .map(|manager| manager.resource_access_allowed(&id, &resource))
            .unwrap_or(false);
        if !allowed {
            return;
        }
    }
}

async fn resource(
    State(state): State<ApiState>,
    Path((id, path)): Path<(String, String)>,
    Query(_query): Query<PluginQuery>,
) -> Result<Response, ApiError> {
    resource_response(&state, &id, &path, None)
}

async fn versioned_resource(
    State(state): State<ApiState>,
    Path((id, digest, path)): Path<(String, String, String)>,
    Query(_query): Query<PluginQuery>,
) -> Result<Response, ApiError> {
    resource_response(&state, &id, &path, Some(&digest))
}

fn resource_response(
    state: &ApiState,
    id: &str,
    path: &str,
    digest: Option<&str>,
) -> Result<Response, ApiError> {
    let manager = locked(state, "读取插件视图")?;
    if !manager.is_active(id).map_err(plugin_error)? {
        return Err(ApiError::Forbidden("插件未激活".into()));
    }
    let resource_path = format!("ui/{path}");
    let package = manager.package(id).map_err(plugin_error)?;
    if digest.is_some_and(|expected| expected != package.digest()) {
        return Err(ApiError::Conflict("插件页面版本已失效".into()));
    }
    let bytes = package
        .files()
        .get(&resource_path)
        .ok_or_else(|| ApiError::NotFound("插件视图资源不存在".into()))?;
    if let Some(mime) = magi_media::image_content_type(&resource_path) {
        return Ok(crate::image_response::image_response(mime, bytes.clone()));
    }
    let extension = resource_path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let content_type = match extension.as_str() {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    };
    let mut response = Response::new(bytes.clone().into());
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static(
            if manager
                .permission_allowed(
                    id,
                    magi_plugin_system::PluginPermissionKind::Extension,
                    "full-trust",
                )
                .map_err(plugin_error)?
            {
                TRUSTED_VIEW_CONTENT_SECURITY_POLICY
            } else {
                VIEW_CONTENT_SECURITY_POLICY
            },
        ),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

/// 已安装插件的图标。与视图资源不同，列表要展示所有已安装插件，所以不要求插件已激活；
/// 只返回清单声明且通过包校验的那一个图片，其他包内文件不经此接口出去。
async fn icon(State(state): State<ApiState>, Path(id): Path<String>) -> Result<Response, ApiError> {
    let manager = locked(&state, "读取插件图标")?;
    let package = manager.package(&id).map_err(plugin_error)?;
    let (content_type, bytes) = package
        .icon()
        .ok_or_else(|| ApiError::NotFound("插件没有声明图标".into()))?;
    Ok(icon_response(content_type, bytes.to_vec()))
}

fn icon_response(content_type: &'static str, bytes: Vec<u8>) -> Response {
    let mut response = crate::image_response::image_response(content_type, bytes);
    response.headers_mut().insert(
        "cache-control",
        HeaderValue::from_static("private, max-age=300"),
    );
    response
}

/// 视图经消息桥调用本插件宿主里注册的后端路由。API 只转交，授权与准入由执行端口判定。
async fn call_route(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(_query): Query<PluginQuery>,
    Json(request): Json<magi_app_server_protocol::generated::PluginRouteCallRequest>,
) -> Result<Json<magi_app_server_protocol::generated::PluginRouteCallResult>, ApiError> {
    let executor = state
        .plugin_route_executor
        .clone()
        .ok_or_else(|| ApiError::Conflict("扩展宿主未运行".into()))?;
    executor.call(id, request).await.map(Json)
}

/// 可见消息投影归原 Renderer；daemon 校验当前包及会话视图的独立读取授权。
async fn admit_view(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<magi_app_server_protocol::generated::PluginViewAdmissionRequest>,
) -> Result<StatusCode, ApiError> {
    use magi_app_server_protocol::PluginViewPlacement;
    let manager = locked(&state, "校验插件会话视图授权")?;
    if !matches!(
        request.placement,
        PluginViewPlacement::ComposerLeft | PluginViewPlacement::ChatAssistant
    ) || !manager.is_active(&id).map_err(plugin_error)?
        || !manager
            .permission_allowed(
                &id,
                magi_plugin_system::PluginPermissionKind::SessionRead,
                "render",
            )
            .map_err(plugin_error)?
    {
        return Err(ApiError::Forbidden(
            "插件未激活或缺少会话视图读取授权".into(),
        ));
    }
    let package = manager.package(&id).map_err(plugin_error)?;
    if package.digest() != request.expected_digest {
        return Err(ApiError::Conflict("插件页面属于旧版安装包".into()));
    }
    if !package
        .manifest()
        .contributions
        .views
        .iter()
        .any(|view| view.entry == request.entry && view.placements.contains(&request.placement))
    {
        return Err(ApiError::Forbidden("插件未声明此会话视图位置".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// 输入草稿仍由当前 Renderer 的 InputArea 拥有；daemon 裁决调用插件的版本与授权。
async fn admit_composer(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<magi_app_server_protocol::generated::PluginComposerAdmissionRequest>,
) -> Result<StatusCode, ApiError> {
    let manager = locked(&state, "校验插件输入授权")?;
    let target = match request.target {
        Some(magi_app_server_protocol::generated::PluginComposerTarget::Text) => "text",
        None | Some(magi_app_server_protocol::generated::PluginComposerTarget::Images) => "images",
    };
    if !manager.is_active(&id).map_err(plugin_error)?
        || !manager
            .permission_allowed(
                &id,
                magi_plugin_system::PluginPermissionKind::Composer,
                target,
            )
            .map_err(plugin_error)?
    {
        return Err(ApiError::Forbidden(format!(
            "插件未激活或缺少 composer:[{target}] 授权"
        )));
    }
    let package = manager.package(&id).map_err(plugin_error)?;
    if package.digest() != request.expected_digest {
        return Err(ApiError::Conflict("插件页面属于旧版安装包".into()));
    }
    if !package
        .manifest()
        .contributions
        .views
        .iter()
        .any(|view| view.entry == request.entry)
    {
        return Err(ApiError::Forbidden("插件页面未在当前清单声明".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn read_resource(
    State(state): State<ApiState>,
    Path((id, resource)): Path<(String, String)>,
    Query(_query): Query<PluginQuery>,
) -> Result<Json<magi_plugin_system::PluginResource>, ApiError> {
    locked(&state, "读取插件资源")?
        .read_resource(&id, &resource)
        .map(Json)
        .map_err(plugin_error)
}

async fn write_resource(
    State(state): State<ApiState>,
    Path((id, resource)): Path<(String, String)>,
    Query(_query): Query<PluginQuery>,
    Json(request): Json<ResourceWriteRequest>,
) -> Result<Json<magi_plugin_system::PluginResource>, ApiError> {
    let mut manager = locked(&state, "写入插件资源")?;
    manager.ensure_editing_admitted(&id).map_err(plugin_error)?;
    write_plugin_resource_with_event(
        &mut manager,
        &state.event_bus,
        &id,
        &resource,
        request.expected_version,
        request.value,
    )
    .map(Json)
    .map_err(plugin_error)
}

/// 资源写入的唯一发布入口：视图消息桥、HTTP 与插件后台 SDK 都经此提交并通知订阅。
pub fn write_plugin_resource_with_event(
    manager: &mut PluginManager,
    event_bus: &InMemoryEventBus,
    id: &str,
    resource_id: &str,
    expected_version: u64,
    value: Value,
) -> Result<magi_plugin_system::PluginResource, magi_plugin_system::PluginError> {
    let resource = manager.write_resource(id, resource_id, expected_version, value)?;
    publish_plugin_resource_event(event_bus, id, resource_id, &resource);
    Ok(resource)
}

pub fn write_plugin_settings_with_event(
    manager: &mut PluginManager,
    event_bus: &InMemoryEventBus,
    id: &str,
    expected_version: u64,
    value: Value,
) -> Result<magi_plugin_system::PluginResource, magi_plugin_system::PluginError> {
    let resource = manager.write_settings(id, expected_version, value)?;
    publish_plugin_resource_event(event_bus, id, "__settings", &resource);
    Ok(resource)
}

fn publish_plugin_resource_event(
    event_bus: &InMemoryEventBus,
    id: &str,
    resource_id: &str,
    resource: &magi_plugin_system::PluginResource,
) {
    let event = EventEnvelope::projection(
        EventId::new(format!(
            "plugin-resource-{id}-{resource_id}-{}",
            resource.version
        )),
        "plugin.resource.updated",
        serde_json::json!({
            "pluginId": id,
            "resourceId": resource_id,
            "version": resource.version,
            "value": resource.value,
        }),
    )
    .with_context(EventContext::default());
    event_bus.publish(event);
}

async fn read_settings(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(_query): Query<PluginQuery>,
) -> Result<Json<magi_plugin_system::PluginResource>, ApiError> {
    locked(&state, "读取插件设置")?
        .read_settings(&id)
        .map(Json)
        .map_err(plugin_error)
}

async fn settings_schema(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let manager = locked(&state, "读取插件设置 schema")?;
    manager.read_settings(&id).map_err(plugin_error)?;
    Ok(Json(
        serde_json::json!({"schema": manager.runtime_manifest(&id, true).map_err(plugin_error)?.settings_schema}),
    ))
}

async fn write_settings(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(_query): Query<PluginQuery>,
    Json(request): Json<ResourceWriteRequest>,
) -> Result<Json<magi_plugin_system::PluginResource>, ApiError> {
    let digest = {
        let manager = locked(&state, "校验插件设置")?;
        manager.ensure_editing_admitted(&id).map_err(plugin_error)?;
        manager
            .validate_settings(&id, &request.value)
            .map_err(plugin_error)?;
        let package = manager.package(&id).map_err(plugin_error)?;
        package
            .manifest()
            .extension
            .as_ref()
            .is_some_and(|extension| {
                extension.runtime_contributions.contains(
                    &magi_app_server_protocol::generated::PluginRuntimeContributionKind::Settings,
                )
            })
            .then(|| package.digest().to_string())
    };
    if digest.is_some() {
        state
            .extension_status
            .as_ref()
            .ok_or_else(|| ApiError::Conflict("扩展宿主未就绪".into()))?
            .validate_settings(id.clone(), request.value.clone())
            .await
            .map_err(ApiError::InvalidInput)?;
    }
    let mut manager = locked(&state, "写入插件设置")?;
    if digest.as_deref().is_some_and(|digest| {
        manager
            .package(&id)
            .map_or(true, |package| package.digest() != digest)
    }) {
        return Err(ApiError::Conflict("设置校验后插件版本已变化".into()));
    }
    manager.ensure_editing_admitted(&id).map_err(plugin_error)?;
    write_plugin_settings_with_event(
        &mut manager,
        &state.event_bus,
        &id,
        request.expected_version,
        request.value,
    )
    .map(Json)
    .map_err(plugin_error)
}

/// 把来源解析为已校验的不可变包。安装与升级共用，来源规则只在这里判定。
async fn load_package(
    source: &PluginSource,
    archive_base64: Option<String>,
    expected_digest: Option<&str>,
) -> Result<PluginPackage, ApiError> {
    let bytes = match (archive_base64, source) {
        (Some(encoded), _) => base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|_| ApiError::InvalidInput("插件包编码无效".into()))?,
        (None, PluginSource::Local { name }) => read_local_package(name).await?,
        (None, PluginSource::Address { url }) => download_package(url).await?,
        (None, PluginSource::Center { .. }) => {
            return Err(ApiError::InvalidInput(
                "插件中心来源必须通过插件中心目录安装".into(),
            ));
        }
        (None, PluginSource::Builtin { .. }) => {
            return Err(ApiError::InvalidInput(
                "内置插件必须通过内置安装入口".into(),
            ));
        }
        (None, PluginSource::Upstream { .. }) => {
            return Err(ApiError::InvalidInput(
                "上游生态来源必须通过上游插件入口安装".into(),
            ));
        }
    };
    let package = PluginPackage::from_archive(&bytes)
        .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
    // 预览确认的是具体摘要：来源内容在确认后被替换时必须拒绝，不能让用户批准 A 却安装 B。
    if let Some(expected) = expected_digest
        && expected != package.digest()
    {
        return Err(ApiError::Conflict(
            "候选插件包与预览时不一致，请重新预览后再确认".into(),
        ));
    }
    Ok(package)
}

/// 安装与升级只接受用户在预览中确认过的摘要：没有预览凭据的写入请求一律拒绝，
/// 所有来源（本地、地址、内联包）统一走“先预览、再确认”。
fn confirmed_digest(expected: Option<&str>) -> Result<&str, ApiError> {
    expected
        .filter(|digest| !digest.trim().is_empty())
        .ok_or_else(|| ApiError::InvalidInput("安装前必须先预览候选包并携带预览摘要".into()))
}

/// 读取本地插件包。路径必须是绝对路径，读取量以包大小上限为界，避免把任意大文件读入内存。
async fn read_local_package(path: &str) -> Result<Vec<u8>, ApiError> {
    use tokio::io::AsyncReadExt;
    if !std::path::Path::new(path).is_absolute() {
        return Err(ApiError::InvalidInput("本地插件包必须使用绝对路径".into()));
    }
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|error| ApiError::InvalidInput(format!("读取本地插件包失败: {error}")))?;
    let mut bytes = Vec::new();
    file.take(MAX_PACKAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| ApiError::InvalidInput(format!("读取本地插件包失败: {error}")))?;
    if bytes.len() > MAX_PACKAGE_BYTES {
        return Err(ApiError::InvalidInput("插件包大小超限".into()));
    }
    Ok(bytes)
}

async fn download_package(url: &str) -> Result<Vec<u8>, ApiError> {
    download_bytes(url, MAX_PACKAGE_BYTES, "插件包").await
}

/// 准备阶段：解析并校验候选包，返回与已安装版本的差异，不写入任何状态。
async fn preview(
    State(state): State<ApiState>,
    Json(request): Json<InstallRequest>,
) -> Result<Json<magi_app_server_protocol::generated::PluginCandidatePreview>, ApiError> {
    let source = parse_source(&request.source)?;
    let package = load_package(&source, request.archive_base64, None).await?;
    let manager = locked(&state, "预览插件候选包")?;
    manager
        .preview(&package, &source)
        .map(Json)
        .map_err(plugin_error)
}

async fn install(
    State(state): State<ApiState>,
    Json(request): Json<InstallRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    let source = parse_source(&request.source)?;
    let package = load_package(
        &source,
        request.archive_base64,
        Some(confirmed_digest(request.expected_digest.as_deref())?),
    )
    .await?;
    let mut manager = locked(&state, "安装插件")?;
    manager.install(&package, source).map_err(plugin_error)?;
    publish_plugin_registry_changed(&state);
    snapshot(&state, &manager)
}

async fn upgrade(
    State(state): State<ApiState>,
    Json(request): Json<InstallRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    let source = parse_source(&request.source)?;
    let package = load_package(
        &source,
        request.archive_base64,
        Some(confirmed_digest(request.expected_digest.as_deref())?),
    )
    .await?;
    upgrade_installed_plugin(&state, &package, source).await?;
    {
        let manager = locked(&state, "读取插件升级结果")?;
        snapshot(&state, &manager)
    }
}

/// 安装的唯一入口：已安装则走排空升级（相同摘要为空操作），否则直接安装。
/// 内置包、插件中心与用户包都经此进入，不再各自判断。
async fn install_or_upgrade(
    state: &ApiState,
    package: &PluginPackage,
    source: PluginSource,
) -> Result<(), ApiError> {
    PluginOperations::from_state(state)
        .install_or_upgrade(package, source, None)
        .await
}

/// 升级的唯一入口：关闭该插件全局准入，再原子切换到新包。任一步失败都保持旧版本与旧激活状态不变。
async fn upgrade_installed_plugin(
    state: &ApiState,
    package: &PluginPackage,
    source: PluginSource,
) -> Result<(), ApiError> {
    PluginOperations::from_state(state)
        .upgrade(package, source, None)
        .await
}

/// 已激活插件声明的用户命令。身份固定为 `plugin/{插件}/{贡献}`，供输入框斜杠菜单使用。
async fn commands(
    State(state): State<ApiState>,
    Query(_query): Query<PluginQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    let manager = locked(&state, "读取插件命令")?;
    let entries = manager.command_contributions().map_err(plugin_error)?;
    Ok(Json(
        entries
            .into_iter()
            .map(|(manifest, command)| {
                serde_json::json!({
                    "id": magi_plugin_system::ContributionRef::new(&manifest.id, &command.id).identity(),
                    "pluginId": manifest.id,
                    "contributionId": command.id,
                    "version": manifest.version,
                    "title": command.title,
                    "description": command.description,
                })
            })
            .collect(),
    ))
}

async fn authorize(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<AuthorizeRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    PluginOperations::from_state(&state)
        .authorize(&id, request.grants, None, request.expected_digest.as_deref())
        .await?;
    snapshot(&state, &*locked(&state, "读取授权结果")?)
}
async fn enable(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<ActivationRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    PluginOperations::from_state(&state)
        .lifecycle(
            "enable",
            &id,
            None,
            operations::LifecycleChange::for_digest(request.expected_digest.as_deref()),
        )
        .await?;
    snapshot(&state, &*locked(&state, "读取插件状态")?)
}
async fn activate(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<ActivationRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    PluginOperations::from_state(&state)
        .lifecycle(
            "activate",
            &id,
            None,
            operations::LifecycleChange::for_digest(request.expected_digest.as_deref()),
        )
        .await?;
    snapshot(&state, &*locked(&state, "读取插件状态")?)
}
async fn deactivate(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<ActivationRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    PluginOperations::from_state(&state)
        .lifecycle(
            "deactivate",
            &id,
            None,
            operations::LifecycleChange::for_digest(request.expected_digest.as_deref()),
        )
        .await?;
    snapshot(&state, &*locked(&state, "读取插件状态")?)
}

async fn disable(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<ActivationRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    PluginOperations::from_state(&state)
        .lifecycle(
            "disable",
            &id,
            None,
            operations::LifecycleChange::for_digest(request.expected_digest.as_deref()),
        )
        .await?;
    snapshot(&state, &*locked(&state, "读取插件状态")?)
}
async fn uninstall(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    PluginOperations::from_state(&state)
        .lifecycle("uninstall", &id, None, Default::default())
        .await?;
    snapshot(&state, &*locked(&state, "读取插件状态")?)
}

// 只发布注册表失效事实；所有客户端经原清单入口读取，不复制激活或权限状态。
fn publish_plugin_registry_changed(state: &ApiState) {
    state.event_bus.publish(EventEnvelope::domain(
        EventId::unique("plugin-registry-changed"),
        "plugin.registry.changed",
        serde_json::json!({}),
    ));
}

fn locked<'a>(
    state: &'a ApiState,
    action: &str,
) -> Result<std::sync::MutexGuard<'a, magi_plugin_system::PluginManager>, ApiError> {
    state
        .plugin_manager
        .lock()
        .map_err(|_| ApiError::internal_assembly(action, "插件锁已损坏"))
}
/// 插件列表投影 = 管理器的安装/授权/生命周期事实 + 扩展宿主的运行状态（只读合并，不另存）。
fn snapshot(
    state: &ApiState,
    manager: &magi_plugin_system::PluginManager,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    let mut list = manager.projection().map_err(plugin_error)?;
    PluginOperations::from_state(state).decorate_runtime(manager, &mut list)?;
    Ok(Json(list))
}
fn plugin_error(error: magi_plugin_system::PluginError) -> ApiError {
    match error {
        magi_plugin_system::PluginError::Conflict(message) => ApiError::Conflict(message),
        magi_plugin_system::PluginError::NotAuthorized(message) => ApiError::Forbidden(message),
        magi_plugin_system::PluginError::NotFound(message) => ApiError::NotFound(message),
        magi_plugin_system::PluginError::InvalidPackage(message) => ApiError::InvalidInput(message),
        err @ magi_plugin_system::PluginError::Resource(
            magi_plugin_system::ResourceError::Conflict { .. },
        ) => ApiError::Conflict(err.to_string()),
        magi_plugin_system::PluginError::Resource(magi_plugin_system::ResourceError::Invalid(
            message,
        )) => ApiError::InvalidInput(message),
        magi_plugin_system::PluginError::Resource(magi_plugin_system::ResourceError::Storage(
            message,
        )) => ApiError::internal_assembly("插件资源操作失败", message),
        other => ApiError::internal_assembly("插件操作失败", other),
    }
}

fn plugin_resource_event_matches(
    event: &EventEnvelope,
    plugin_id: &str,
    resource_id: &str,
) -> bool {
    event.event_type == "plugin.resource.updated"
        && event.payload.get("pluginId").and_then(Value::as_str) == Some(plugin_id)
        && event.payload.get("resourceId").and_then(Value::as_str) == Some(resource_id)
}

fn resource_sse_snapshot(
    plugin_id: &str,
    resource_id: &str,
    resource: magi_plugin_system::PluginResource,
) -> Event {
    Event::default()
        .json_data(serde_json::json!({
            "pluginId": plugin_id,
            "resourceId": resource_id,
            "version": resource.version,
            "value": resource.value,
        }))
        .expect("plugin resource snapshot must remain valid JSON")
}

fn resource_sse_reset() -> Event {
    Event::default()
        .json_data(serde_json::json!({ "reset": true }))
        .expect("plugin resource reset event must remain valid JSON")
}

/// 订阅因插件停用、撤权或升级而结束；视图据此提示并释放订阅，而不是静默停更。
fn resource_sse_closed() -> Event {
    Event::default()
        .json_data(serde_json::json!({ "closed": true, "reason": "plugin_inactive" }))
        .expect("plugin resource closed event must remain valid JSON")
}

fn resource_sse_event(event: EventEnvelope) -> Event {
    Event::default()
        .json_data(&event.payload)
        .expect("plugin resource event payload must remain valid JSON")
}
fn parse_source(source: &str) -> Result<PluginSource, ApiError> {
    let (kind, value) = source
        .split_once(':')
        .ok_or_else(|| ApiError::InvalidInput("插件来源必须是 address: 或 local:".into()))?;
    if value.trim().is_empty() {
        return Err(ApiError::InvalidInput("插件来源不能为空".into()));
    }
    match kind {
        // 插件中心来源只能经已签名目录解析（`/plugins/center/install`），不接受自报地址。
        "center" => Err(ApiError::InvalidInput(
            "插件中心来源必须通过插件中心目录安装".into(),
        )),
        "address" => Ok(PluginSource::Address {
            url: validated_remote_url(value)?,
        }),
        "local" => Ok(PluginSource::Local { name: value.into() }),
        _ => Err(ApiError::InvalidInput("插件来源类型无效".into())),
    }
}

fn validated_remote_url(value: &str) -> Result<String, ApiError> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| ApiError::InvalidInput("插件来源地址无效".into()))?;
    download::check_url_shape(&url)?;
    Ok(url.to_string())
}

async fn list_jobs(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<magi_app_server_protocol::generated::PluginJobSchedule>>, ApiError> {
    state.plugin_jobs.list(&id).map(Json)
}
async fn create_job(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<magi_app_server_protocol::generated::PluginJobRequest>,
) -> Result<Json<magi_app_server_protocol::generated::PluginJobSchedule>, ApiError> {
    state.plugin_jobs.create(&state, &id, request).map(Json)
}
async fn cancel_job(
    State(state): State<ApiState>,
    Path((id, job)): Path<(String, String)>,
) -> Result<Json<magi_app_server_protocol::generated::PluginJobSchedule>, ApiError> {
    state.plugin_jobs.cancel(&state, &id, &job).await.map(Json)
}

#[cfg(test)]
mod tests;
