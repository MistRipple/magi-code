use crate::{errors::ApiError, state::ApiState};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
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

const PLUGIN_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// 资源订阅检查插件是否仍激活并持有授权的间隔；失效后立即关闭订阅。
const RESOURCE_SUBSCRIPTION_RECHECK: Duration = Duration::from_millis(500);

mod center;
mod download;
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
        .route("/plugins/{id}/icon", get(icon))
        .route("/plugins/{id}/routes", post(call_route))
        .route("/plugins/{id}/settings/schema", get(settings_schema))
        .route(
            "/plugins/{id}/resources/{resource}",
            get(read_resource).put(write_resource),
        )
        .route(
            "/plugins/{id}/settings",
            get(read_settings).put(write_settings),
        )
        .route("/plugins/preview", post(preview))
        .route("/plugins/install", post(install))
        .route("/plugins/upgrade", post(upgrade))
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
    let manager = locked(&state, "读取插件视图")?;
    if !manager.is_active(&id).map_err(plugin_error)? {
        return Err(ApiError::Forbidden("插件未激活".into()));
    }
    let resource_path = format!("ui/{path}");
    let package = manager.package(&id).map_err(plugin_error)?;
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
            "sandbox allow-scripts; default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; object-src 'none'; frame-ancestors 'self'; base-uri 'none'; form-action 'none'; connect-src 'none'",
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
    let installed = locked(state, "读取已安装插件")?
        .state()
        .plugins
        .contains_key(&package.manifest().id);
    if installed {
        upgrade_installed_plugin(state, package, source).await
    } else {
        locked(state, "安装插件")?
            .install(package, source)
            .map_err(plugin_error)?;
        publish_plugin_registry_changed(state);
        Ok(())
    }
}

/// 升级的唯一入口：关闭该插件全局准入，再原子切换到新包。任一步失败都保持旧版本与旧激活状态不变。
async fn upgrade_installed_plugin(
    state: &ApiState,
    package: &PluginPackage,
    source: PluginSource,
) -> Result<(), ApiError> {
    let drain = PluginManager::begin_upgrade_drain(
        std::sync::Arc::clone(&state.plugin_manager),
        &package.manifest().id,
    )
    .map_err(plugin_error)?;
    quiesce_extension(state, &package.manifest().id).await?;
    {
        let mut manager = locked(state, "升级插件")?;
        if let Err(error) = manager.upgrade(package, source, &drain) {
            drop(manager);
            publish_plugin_registry_changed(state);
            return Err(plugin_error(error));
        }
        drain.commit(&mut manager);
    }
    publish_plugin_registry_changed(state);
    Ok(())
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
    let active = locked(&state, "读取插件授权")?
        .is_active(&id)
        .map_err(plugin_error)?;
    let drain = if active {
        let drain =
            PluginManager::begin_deactivation(std::sync::Arc::clone(&state.plugin_manager), &id)
                .map_err(plugin_error)?;
        quiesce_extension(&state, &id).await?;
        Some(drain)
    } else {
        None
    };
    let mut manager = locked(&state, "授权插件")?;
    if let Err(error) = manager.authorize(&id, request.grants) {
        drop(manager);
        publish_plugin_registry_changed(&state);
        return Err(plugin_error(error));
    }
    if let Some(drain) = drain {
        drain.commit(&mut manager);
    }
    publish_plugin_registry_changed(&state);
    snapshot(&state, &manager)
}
async fn enable(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(_request): Json<ActivationRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    mutate_plugin(&state, |m| m.enable(&id), "启用插件")
}
async fn activate(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(_request): Json<ActivationRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    mutate_plugin(&state, |m| m.activate(&id).map(|_| ()), "激活插件")
}
async fn deactivate(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(_request): Json<ActivationRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    let drain =
        PluginManager::begin_deactivation(std::sync::Arc::clone(&state.plugin_manager), &id)
            .map_err(plugin_error)?;
    quiesce_extension(&state, &id).await?;
    {
        let mut manager = locked(&state, "停用插件")?;
        if let Err(error) = manager.deactivate(&id) {
            drop(manager);
            publish_plugin_registry_changed(&state);
            return Err(plugin_error(error));
        }
        drain.commit(&mut manager);
    }
    publish_plugin_registry_changed(&state);
    {
        let manager = locked(&state, "读取停用后的插件状态")?;
        snapshot(&state, &manager)
    }
}

async fn quiesce_extension(state: &ApiState, id: &str) -> Result<(), ApiError> {
    let executable = locked(state, "读取扩展宿主")?
        .package(id)
        .map_err(plugin_error)?
        .manifest()
        .extension
        .is_some();
    if executable && let Some(source) = &state.extension_status {
        source
            .quiesce(id.to_string())
            .await
            .map_err(|reason| ApiError::Conflict(format!("扩展停止失败：{reason}")))?;
    }
    Ok(())
}
async fn disable(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(_request): Json<ActivationRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    mutate_plugin(&state, |m| m.disable(&id), "禁用插件")
}
async fn uninstall(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    let mut manager = locked(&state, "卸载插件")?;
    manager.uninstall(&id).map_err(plugin_error)?;
    publish_plugin_registry_changed(&state);
    snapshot(&state, &manager)
}

// 只发布注册表失效事实；所有客户端经原清单入口读取，不复制激活或权限状态。
fn publish_plugin_registry_changed(state: &ApiState) {
    state.event_bus.publish(EventEnvelope::domain(
        EventId::unique("plugin-registry-changed"),
        "plugin.registry.changed",
        serde_json::json!({}),
    ));
}

fn mutate_plugin<F>(
    state: &ApiState,
    mut operation: F,
    action: &str,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError>
where
    F: FnMut(&mut magi_plugin_system::PluginManager) -> Result<(), magi_plugin_system::PluginError>,
{
    let mut manager = locked(state, action)?;
    operation(&mut manager).map_err(plugin_error)?;
    publish_plugin_registry_changed(state);
    snapshot(state, &manager)
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
    if let Some(source) = state.extension_status.as_ref() {
        for plugin in &mut list.plugins {
            plugin.runtime = source.status(&plugin.manifest.id);
        }
    }
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
mod tests {
    use super::*;
    use axum::body::Body;
    use futures_util::StreamExt;
    use magi_governance::GovernanceService;
    use magi_plugin_system::{
        PluginManager, PluginPackage, PluginPermission, PluginPermissionKind, PluginSource,
    };
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use std::{
        io::{Cursor, Write},
        sync::Arc,
        time::Duration,
    };
    use tokio::time::timeout;
    use tower::util::ServiceExt;
    use zip::ZipWriter;

    fn archive(version: &str) -> Vec<u8> {
        let manifest = serde_json::json!({
            "sdkVersion": 2,
            "id": "acme.resources",
            "version": version,
            "name": "Resources",
            "description": "",
            "permissions": [{
                "kind": "storage",
                "targets": ["dashboard"]
            }],
            "dataSchemaVersion": 1,
            "settingsSchema": {"type":"object","additionalProperties":false},
            "contributions": {"resources":[{"id":"dashboard","title":"Dashboard","description":""}]}
        });
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        {
            let (name, bytes) = ("manifest.json", serde_json::to_vec(&manifest).unwrap());
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn package() -> PluginPackage {
        PluginPackage::from_archive(&archive("1.0.0")).unwrap()
    }

    #[test]
    fn remote_plugin_sources_require_http_url() {
        assert!(parse_source("address:https://example.test/plugin.zip").is_ok());
        assert!(parse_source("center:https://example.test/plugin.zip").is_err());
        assert!(parse_source("address:not-a-url").is_err());
    }

    fn bare_state(root: &std::path::Path) -> ApiState {
        let manager = PluginManager::open(root).unwrap();
        ApiState::new(
            "magi-test",
            Arc::new(magi_event_bus::InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_plugin_manager(Arc::new(std::sync::Mutex::new(manager)))
    }

    async fn post_json(
        state: &ApiState,
        uri: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = routes()
            .with_state(state.clone())
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test]
    async fn the_official_center_is_the_default_and_clearing_the_address_restores_it() {
        let root = tempfile::tempdir().unwrap();
        let state = bare_state(root.path());
        assert_eq!(read_center_config(&state).url, OFFICIAL_PLUGIN_CENTER_URL);
        assert!(is_official_center(OFFICIAL_PLUGIN_CENTER_URL));
        assert!(!is_official_center("https://example.test/catalog.json"));

        let custom = PluginCenterConfig {
            url: "https://example.test/catalog.json".into(),
            trusted_keys: Vec::new(),
        };
        let _ = write_center_config(State(state.clone()), Json(custom))
            .await
            .unwrap();
        assert_eq!(
            read_center_config(&state).url,
            "https://example.test/catalog.json"
        );

        // 地址与公钥都留空：清除已保存的配置，回到官方插件中心。
        let cleared = PluginCenterConfig {
            url: String::new(),
            trusted_keys: Vec::new(),
        };
        let restored = write_center_config(State(state.clone()), Json(cleared))
            .await
            .unwrap();
        assert_eq!(restored.0.url, OFFICIAL_PLUGIN_CENTER_URL);
        assert_eq!(read_center_config(&state).url, OFFICIAL_PLUGIN_CENTER_URL);
    }

    #[tokio::test]
    async fn authoring_guide_is_served_from_the_repository_document() {
        let root = tempfile::tempdir().unwrap();
        let response = routes()
            .with_state(bare_state(root.path()))
            .oneshot(
                axum::http::Request::builder()
                    .uri("/plugins/guide")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.starts_with("# Magi 插件作者指南"), "{text:.60}");
        assert!(!text.contains("](../"), "界面内的链接不能依赖仓库相对路径");
    }

    #[tokio::test]
    async fn preview_reports_the_candidate_without_installing_and_install_binds_the_digest() {
        let root = tempfile::tempdir().unwrap();
        let state = bare_state(root.path());
        let encoded = base64::engine::general_purpose::STANDARD.encode(archive("1.0.0"));
        let request = |digest: Option<&str>| {
            let mut body = serde_json::json!({"source": "local:a.zip", "archiveBase64": encoded});
            if let Some(digest) = digest {
                body["expectedDigest"] = digest.into();
            }
            body
        };

        let (status, preview) = post_json(&state, "/plugins/preview", request(None)).await;
        assert_eq!(status, StatusCode::OK, "{preview}");
        assert_eq!(preview["operation"], "install");
        assert_eq!(preview["addedPermissions"][0]["kind"], "storage");
        assert!(
            state
                .plugin_manager
                .lock()
                .unwrap()
                .state()
                .plugins
                .is_empty()
        );
        let digest = preview["digest"].as_str().unwrap().to_owned();

        // 确认的摘要与实际内容不一致时必须拒绝，且不留下任何安装状态。
        let (status, _) = post_json(&state, "/plugins/install", request(Some("0000"))).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            state
                .plugin_manager
                .lock()
                .unwrap()
                .state()
                .plugins
                .is_empty()
        );

        let (status, _) = post_json(&state, "/plugins/install", request(Some(&digest))).await;
        assert_eq!(status, StatusCode::OK);
        let (_, again) = post_json(&state, "/plugins/preview", request(None)).await;
        assert_eq!(again["operation"], "unchanged");
    }

    async fn active_resource_state() -> (ApiState, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let mut manager = PluginManager::open(root.path()).unwrap();
        manager
            .install(
                &package(),
                PluginSource::Local {
                    name: "test.zip".into(),
                },
            )
            .unwrap();
        manager.enable("acme.resources").unwrap();
        manager
            .authorize(
                "acme.resources",
                vec![PluginPermission {
                    kind: PluginPermissionKind::Storage,

                    targets: vec!["dashboard".into()],
                }],
            )
            .unwrap();
        manager.activate("acme.resources").unwrap();
        let manager = Arc::new(std::sync::Mutex::new(manager));
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
        let api_state = ApiState::new(
            "magi-test",
            event_bus,
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_plugin_manager(manager);
        (api_state, root)
    }

    #[tokio::test]
    async fn view_resources_carry_a_response_level_sandbox() {
        let manifest = serde_json::json!({
            "sdkVersion": 2,
            "id": "acme.viewer",
            "version": "1.0.0",
            "name": "Viewer",
            "description": "",
            "permissions": [],
            "dataSchemaVersion": 1,
            "settingsSchema": {"type":"object","additionalProperties":false},
            "contributions": {"views":[{
                "id": "page", "title": "Page", "entry": "ui/index.html",
                "placements": ["main"]
            }]}
        });
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("ui/index.html", b"<!doctype html><title>x</title>".to_vec()),
        ] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        let package = PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut manager = PluginManager::open(root.path()).unwrap();
        manager
            .install(
                &package,
                PluginSource::Local {
                    name: "viewer.zip".into(),
                },
            )
            .unwrap();
        manager.enable("acme.viewer").unwrap();
        manager.authorize("acme.viewer", Vec::new()).unwrap();
        manager.activate("acme.viewer").unwrap();
        let state = ApiState::new(
            "magi-test",
            Arc::new(magi_event_bus::InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_plugin_manager(Arc::new(std::sync::Mutex::new(manager)));
        let response = routes()
            .with_state(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/plugins/acme.viewer/ui/index.html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let csp = response
            .headers()
            .get("content-security-policy")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(csp.starts_with("sandbox allow-scripts;"), "{csp}");
        assert!(csp.contains("connect-src 'none'"), "{csp}");
        drop(root);
    }

    #[tokio::test]
    async fn resource_writes_publish_snapshot_and_subscription_updates() {
        let (state, root) = active_resource_state().await;
        let router = routes().with_state(state.clone());
        let stream = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/plugins/acme.resources/resources/dashboard/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body()
            .into_data_stream();

        let response = router
            .oneshot(
                axum::http::Request::builder()
                    .method("PUT")
                    .uri("/plugins/acme.resources/resources/dashboard")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"expectedVersion":0,"value":{"count":1}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            status,
            axum::http::StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&body_bytes)
        );

        let mut stream = stream;
        let initial = timeout(Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&initial).contains("\"version\":0"));
        let body = timeout(Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let text = String::from_utf8_lossy(&body).to_string();
        assert!(text.contains("\"version\":1"), "{text}");
        assert!(text.contains("\"count\":1"), "{text}");
        drop(root);
    }
    #[tokio::test]
    async fn resource_subscription_closes_when_the_plugin_is_deactivated() {
        let (state, root) = active_resource_state().await;
        let router = routes().with_state(state.clone());
        let mut stream = router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/plugins/acme.resources/resources/dashboard/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body()
            .into_data_stream();
        let initial = timeout(Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&initial).contains("\"version\":0"));

        state
            .plugin_manager
            .lock()
            .unwrap()
            .deactivate("acme.resources")
            .unwrap();
        let closed = timeout(Duration::from_secs(3), stream.next())
            .await
            .expect("撤销后订阅必须在复核间隔内收到关闭事件")
            .unwrap()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&closed).contains("\"closed\":true"),
            "{}",
            String::from_utf8_lossy(&closed)
        );
        assert!(
            timeout(Duration::from_secs(1), stream.next())
                .await
                .unwrap()
                .is_none(),
            "关闭事件之后流必须结束"
        );
        drop(root);
    }

    #[tokio::test]
    async fn upgrade_route_switches_the_package_and_requires_reauthorization() {
        let (state, root) = active_resource_state().await;
        let body = serde_json::json!({
            "source": "local:/tmp/acme-resources-1.1.0.zip",
            "archiveBase64": base64::engine::general_purpose::STANDARD.encode(archive("1.1.0")),
        });
        // 没有预览摘要的升级一律拒绝。
        let (status, _) = post_json(&state, "/plugins/upgrade", body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let digest = PluginPackage::from_archive(&archive("1.1.0"))
            .unwrap()
            .digest()
            .to_string();
        let mut body = body;
        body["expectedDigest"] = digest.into();
        let response = routes()
            .with_state(state.clone())
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/plugins/upgrade")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let list: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let plugin = &list["plugins"][0];
        assert_eq!(plugin["manifest"]["version"], "1.1.0");
        assert_eq!(plugin["enabled"], true);
        assert_eq!(plugin["active"], false);
        assert_eq!(plugin["grants"], serde_json::json!([]));
        assert_eq!(plugin["source"]["kind"], "local");
        drop(root);
    }

    #[tokio::test]
    async fn local_package_sources_must_be_absolute_paths() {
        let error = read_local_package("relative/plugin.zip").await.unwrap_err();
        assert!(matches!(error, ApiError::InvalidInput(_)));
    }
    /// 本地发布服务：同一个地址提供已签名目录和（可被替换内容的）发布包。
    struct CenterFixture {
        base: String,
        package: Arc<std::sync::Mutex<Vec<u8>>>,
        _server: tokio::task::JoinHandle<()>,
    }

    async fn start_center(
        signing: &ring::signature::Ed25519KeyPair,
        key_id: &str,
        package_bytes: Vec<u8>,
        advertised_sha256: String,
    ) -> CenterFixture {
        start_center_with(
            signing,
            key_id,
            package_bytes,
            advertised_sha256,
            serde_json::json!({}),
        )
        .await
    }

    async fn start_center_with(
        signing: &ring::signature::Ed25519KeyPair,
        key_id: &str,
        package_bytes: Vec<u8>,
        advertised_sha256: String,
        extra: serde_json::Value,
    ) -> CenterFixture {
        use base64::Engine as _;
        let package = Arc::new(std::sync::Mutex::new(package_bytes.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mut catalog = serde_json::json!({
            "format": 1, "issuedAt": 1,
            "plugins": [{
                "id": "acme.resources", "name": "Resources", "description": "",
                "version": "1.0.0", "url": format!("{base}/package.zip"),
                "sha256": advertised_sha256, "size": package_bytes.len()
            }]
        });
        for (key, value) in extra.as_object().unwrap() {
            catalog[key] = value.clone();
        }
        let catalog = serde_json::to_vec(&catalog).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD;
        let envelope = serde_json::to_vec(&serde_json::json!({
            "payload": b64.encode(&catalog),
            "keyId": key_id,
            "signature": b64.encode(ring::signature::Ed25519KeyPair::sign(signing, &catalog).as_ref()),
        }))
        .unwrap();
        let served = Arc::clone(&package);
        let app = axum::Router::new()
            .route(
                "/catalog.json",
                axum::routing::get(move || {
                    let envelope = envelope.clone();
                    async move { envelope }
                }),
            )
            .route(
                "/package.zip",
                axum::routing::get(move || {
                    let served = Arc::clone(&served);
                    async move { served.lock().unwrap().clone() }
                }),
            );
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        CenterFixture {
            base,
            package,
            _server: server,
        }
    }

    fn center_signing_key() -> (ring::signature::Ed25519KeyPair, String) {
        use base64::Engine as _;
        use ring::signature::KeyPair;
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public = base64::engine::general_purpose::STANDARD.encode(pair.public_key());
        (pair, public)
    }

    async fn call(
        state: &ApiState,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = axum::http::Request::builder().method(method).uri(uri);
        let body = match body {
            Some(value) => {
                builder = builder.header("content-type", "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = routes()
            .with_state(state.clone())
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test]
    async fn signed_center_catalog_installs_only_the_exact_published_package() {
        let (signing, public) = center_signing_key();
        let archive = archive("1.0.0");
        let digest = package().digest().to_string();
        let center = start_center(&signing, "center-1", archive.clone(), digest.clone()).await;
        let root = tempfile::tempdir().unwrap();
        let state = bare_state(root.path());

        // 未配置插件中心：没有条目也没有错误。
        let (status, body) = call(&state, "GET", "/plugins/center", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["entries"], serde_json::json!([]));

        let config = serde_json::json!({
            "url": format!("{}/catalog.json", center.base),
            "trustedKeys": [{"keyId": "center-1", "publicKey": public}],
        });
        let (status, _) = call(&state, "PUT", "/plugins/center/config", Some(config)).await;
        assert_eq!(status, StatusCode::OK);
        let (_, body) = call(&state, "GET", "/plugins/center", None).await;
        assert_eq!(body["error"], serde_json::Value::Null, "{body}");
        assert_eq!(body["entries"][0]["state"], "available");
        assert_eq!(body["entries"][0]["sha256"], digest);

        // 用户确认的是旧摘要：目录已变化时拒绝，不安装未经确认的候选。
        let (status, _) = call(
            &state,
            "POST",
            "/plugins/center/install",
            Some(serde_json::json!({"id": "acme.resources", "expectedSha256": "0".repeat(64)})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);

        // 发布服务被入侵：包内容被替换，但目录签名仍绑定原摘要，安装必须失败且不落盘。
        *center.package.lock().unwrap() = self::archive("1.0.1");
        let (status, rejection) = call(
            &state,
            "POST",
            "/plugins/center/install",
            Some(serde_json::json!({"id": "acme.resources", "expectedSha256": digest})),
        )
        .await;
        // 替换内容要么超出目录声明的大小（下载阶段 400），要么与签名摘要不符（403）；都不得安装。
        assert!(
            matches!(status, StatusCode::FORBIDDEN | StatusCode::BAD_REQUEST),
            "{status} {rejection}"
        );
        assert!(
            state
                .plugin_manager
                .lock()
                .unwrap()
                .state()
                .plugins
                .is_empty()
        );

        *center.package.lock().unwrap() = archive;
        let (status, body) = call(
            &state,
            "POST",
            "/plugins/center/install",
            Some(serde_json::json!({"id": "acme.resources", "expectedSha256": digest})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["plugins"][0]["source"]["kind"], "center");
        let (_, body) = call(&state, "GET", "/plugins/center", None).await;
        assert_eq!(body["entries"][0]["state"], "installed");
    }

    #[tokio::test]
    async fn plugin_list_merges_the_extension_runtime_status_without_storing_it() {
        use magi_app_server_protocol::generated::{PluginRuntimeState, PluginRuntimeStatus};
        struct Board;
        impl crate::ExtensionStatusSource for Board {
            fn status(&self, plugin_id: &str) -> Option<PluginRuntimeStatus> {
                (plugin_id == "acme.resources").then(|| PluginRuntimeStatus {
                    state: PluginRuntimeState::Restarting,
                    reason: Some("宿主进程退出".into()),
                    generation: 0,
                    crashes: 2,
                    blocking: vec!["tool.call".into()],
                })
            }
        }
        let (state, root) = active_resource_state().await;
        // 没有状态来源：不带 runtime 字段。
        let (_, body) = call(&state, "GET", "/plugins", None).await;
        assert!(body["plugins"][0].get("runtime").is_none(), "{body}");
        // 有状态来源：合并进每个插件的投影，来源是宿主监管而不是插件管理器。
        let state = state.with_extension_status(std::sync::Arc::new(Board));
        let (_, body) = call(&state, "GET", "/plugins", None).await;
        let runtime = &body["plugins"][0]["runtime"];
        assert_eq!(runtime["state"], "restarting");
        assert_eq!(runtime["reason"], "宿主进程退出");
        assert_eq!(runtime["crashes"], 2);
        assert_eq!(runtime["blocking"], serde_json::json!(["tool.call"]));
        // 变更类操作返回的列表同样带运行状态。
        let (_, body) = call(
            &state,
            "POST",
            "/plugins/acme.resources/deactivate",
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(
            body["plugins"][0]["runtime"]["state"], "restarting",
            "{body}"
        );
        drop(root);
    }

    #[tokio::test]
    async fn expired_and_revoked_center_catalogs_offer_nothing_to_install() {
        let (signing, public) = center_signing_key();
        let digest = package().digest().to_string();
        let configure = |base: &str| {
            serde_json::json!({
                "url": format!("{base}/catalog.json"),
                "trustedKeys": [{"keyId": "center-1", "publicKey": public}],
            })
        };
        let body =
            |digest: &str| serde_json::json!({"id": "acme.resources", "expectedSha256": digest});

        // 已过期的签名目录：不展示条目，也不能安装。
        let expired = start_center_with(
            &signing,
            "center-1",
            archive("1.0.0"),
            digest.clone(),
            serde_json::json!({"sequence": 1, "expiresAt": 1}),
        )
        .await;
        let root = tempfile::tempdir().unwrap();
        let state = bare_state(root.path());
        call(
            &state,
            "PUT",
            "/plugins/center/config",
            Some(configure(&expired.base)),
        )
        .await;
        let (_, view) = call(&state, "GET", "/plugins/center", None).await;
        assert_eq!(view["entries"], serde_json::json!([]));
        assert!(view["error"].as_str().unwrap().contains("过期"), "{view}");
        let (status, _) = call(
            &state,
            "POST",
            "/plugins/center/install",
            Some(body(&digest)),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // 被撤销的版本：不再出现在目录里，按版本指定也不能安装。
        let revoked = start_center_with(
            &signing, "center-1", archive("1.0.0"), digest.clone(),
            serde_json::json!({"sequence": 2, "revoked": [{"id": "acme.resources", "reason": "漏洞"}]}),
        )
        .await;
        call(
            &state,
            "PUT",
            "/plugins/center/config",
            Some(configure(&revoked.base)),
        )
        .await;
        let (_, view) = call(&state, "GET", "/plugins/center", None).await;
        assert_eq!(view["entries"], serde_json::json!([]), "{view}");
        let mut pinned = body(&digest);
        pinned["version"] = "1.0.0".into();
        let (status, _) = call(&state, "POST", "/plugins/center/install", Some(pinned)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // 序号回退：该地址已见过更高序号，序号 1 的目录视为回滚。
        let rolled_back = start_center_with(
            &signing,
            "center-1",
            archive("1.0.0"),
            digest.clone(),
            serde_json::json!({"sequence": 1}),
        )
        .await;
        state
            .settings_store
            .set_section(
                "pluginCenterState",
                serde_json::json!({ format!("{}/catalog.json", rolled_back.base): 2 }),
            )
            .unwrap();
        call(
            &state,
            "PUT",
            "/plugins/center/config",
            Some(configure(&rolled_back.base)),
        )
        .await;
        let (_, view) = call(&state, "GET", "/plugins/center", None).await;
        assert!(
            view["error"].as_str().unwrap_or("<none>").contains("回滚"),
            "{view}"
        );
    }

    #[tokio::test]
    async fn center_catalog_signed_by_an_untrusted_key_exposes_nothing() {
        let (signing, _) = center_signing_key();
        let (_, other_public) = center_signing_key();
        let center = start_center(&signing, "center-1", archive("1.0.0"), "a".repeat(64)).await;
        let root = tempfile::tempdir().unwrap();
        let state = bare_state(root.path());
        let config = serde_json::json!({
            "url": format!("{}/catalog.json", center.base),
            "trustedKeys": [{"keyId": "center-1", "publicKey": other_public}],
        });
        call(&state, "PUT", "/plugins/center/config", Some(config)).await;
        let (_, body) = call(&state, "GET", "/plugins/center", None).await;
        assert_eq!(body["entries"], serde_json::json!([]));
        assert!(body["error"].as_str().unwrap().contains("签名"), "{body}");
        let (status, _) = call(
            &state,
            "POST",
            "/plugins/center/install",
            Some(serde_json::json!({"id": "acme.resources", "expectedSha256": "a".repeat(64)})),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn center_config_rejects_malformed_keys_and_generic_center_sources() {
        let root = tempfile::tempdir().unwrap();
        let state = bare_state(root.path());
        let (status, _) = call(
            &state,
            "PUT",
            "/plugins/center/config",
            Some(serde_json::json!({
                "url": "https://plugins.example/catalog.json",
                "trustedKeys": [{"keyId": "k", "publicKey": "AAAA"}],
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(
            &state,
            "POST",
            "/plugins/install",
            Some(serde_json::json!({"source": "center:https://plugins.example/p.zip"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "中心来源不能自报地址");
    }
}
