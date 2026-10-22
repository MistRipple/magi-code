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
    CatalogEntry, ContributionRef, MAX_CATALOG_BYTES, MAX_PACKAGE_BYTES, PluginCatalog,
    PluginManager, PluginPackage, PluginSource, TrustedKey,
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
#[cfg(test)]
use center::{OFFICIAL_PLUGIN_CENTER_URL, is_official_center, read_center_config};
use center::{center, center_install, guide, write_center_config};

type InstallRequest = magi_app_server_protocol::PluginInstallRequest;
type ScopeRequest = magi_app_server_protocol::PluginScopeRequest;
type AuthorizeRequest = magi_app_server_protocol::PluginAuthorizeRequest;
type ResourceWriteRequest = magi_app_server_protocol::PluginResourceWriteRequest;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScopeQuery {
    scope: String,
}

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/plugins", get(list))
        .route("/plugins/bundled", get(bundled))
        .route("/plugins/center", get(center))
        .route(
            "/plugins/center/config",
            axum::routing::put(write_center_config),
        )
        .route("/plugins/center/install", post(center_install))
        .route("/plugins/bundled/{id}/install", post(install_bundled))
        .route("/plugins/guide", get(guide))
        .route("/plugins/manifests", get(manifests))
        .route("/plugins/commands", get(commands))
        .route("/plugins/workflows", get(workflows))
        .route(
            "/plugins/{id}/resources/{resource}/events",
            get(resource_events),
        )
        .route("/plugins/{id}/ui/{*path}", get(resource))
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

async fn bundled() -> Json<Vec<magi_app_server_protocol::PluginManifest>> {
    Json(crate::bundled_plugins::bundled_plugin_manifests())
}

async fn install_bundled(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    let package = crate::bundled_plugins::bundled_plugin_package(&id)?;
    let source = PluginSource::Builtin { name: id };
    install_or_upgrade(&state, &package, source).await?;
    snapshot(&*locked(&state, "读取内置插件安装结果")?)
}

async fn list(
    State(state): State<ApiState>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    let manager = state
        .plugin_manager
        .lock()
        .map_err(|_| ApiError::internal_assembly("读取插件状态失败", "插件锁已损坏"))?;
    manager.projection().map(Json).map_err(plugin_error)
}

async fn manifests(
    State(state): State<ApiState>,
    Query(query): Query<ScopeQuery>,
) -> Result<Json<Vec<magi_app_server_protocol::PluginManifest>>, ApiError> {
    let manager = locked(&state, "读取插件清单")?;
    let manifests = manager
        .manifests_for_scope(&query.scope)
        .map_err(plugin_error)?;
    Ok(Json(manifests))
}

async fn commands(
    State(state): State<ApiState>,
    Query(query): Query<ScopeQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    let manager = locked(&state, "读取插件命令")?;
    let entries = manager
        .command_contributions_for_scope(&query.scope)
        .map_err(plugin_error)?;
    Ok(Json(contribution_entries(&manager, entries)))
}

async fn workflows(
    State(state): State<ApiState>,
    Query(query): Query<ScopeQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    let manager = locked(&state, "读取插件工作流")?;
    let entries = manager
        .workflow_contributions_for_scope(&query.scope)
        .map_err(plugin_error)?;
    Ok(Json(contribution_entries(&manager, entries)))
}

/// 命令与工作流共用的投影：身份固定为 `plugin/{插件}/{贡献}`，并携带激活包摘要，
/// 使用方据此把一次运行固定到精确版本。
fn contribution_entries(
    manager: &PluginManager,
    entries: Vec<(
        magi_app_server_protocol::PluginManifest,
        magi_app_server_protocol::PluginNamedContribution,
    )>,
) -> Vec<serde_json::Value> {
    entries
        .into_iter()
        .map(|(manifest, contribution)| {
            let digest = manager
                .package(&manifest.id)
                .map(|package| package.digest().to_string())
                .unwrap_or_default();
            serde_json::json!({
                "id": ContributionRef::new(&manifest.id, &contribution.id).identity(),
                "pluginId": manifest.id,
                "contributionId": contribution.id,
                "version": manifest.version,
                "digest": digest,
                "title": contribution.title,
                "description": contribution.description,
            })
        })
        .collect()
}

async fn resource_events(
    State(state): State<ApiState>,
    Path((id, resource)): Path<(String, String)>,
    Query(query): Query<ScopeQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let (initial_resource, receiver) = {
        let manager = locked(&state, "订阅插件资源")?;
        let initial_resource = manager
            .read_resource(&id, &query.scope, &resource)
            .map_err(plugin_error)?;
        // The manager lock serializes this snapshot with every resource writer. The
        // event-bus cut is then registered before the lock is released, so a writer
        // can only appear in the initial snapshot or in the live stream.
        let (_snapshot, receiver) = state.event_bus.snapshot_and_subscribe();
        (initial_resource, receiver)
    };
    let initial = stream::once(future::ready(Ok::<Event, Infallible>(
        resource_sse_snapshot(&id, &query.scope, &resource, initial_resource),
    )));
    // 订阅随插件停用、撤权或升级失效：不能让已被撤销的视图继续收到资源变更。
    let revoked = resource_subscription_revoked(
        std::sync::Arc::clone(&state.plugin_manager),
        id.clone(),
        query.scope.clone(),
        resource.clone(),
    );
    let live = BroadcastStream::new(receiver)
        .filter_map(move |event| {
            future::ready(match event {
                Ok(envelope) => {
                    plugin_resource_event_matches(&envelope, &id, &query.scope, &resource)
                        .then(|| Ok::<Event, Infallible>(resource_sse_event(envelope)))
                }
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
    scope: String,
    resource: String,
) {
    loop {
        tokio::time::sleep(RESOURCE_SUBSCRIPTION_RECHECK).await;
        let allowed = manager
            .lock()
            .map(|manager| manager.resource_access_allowed(&id, &scope, &resource))
            .unwrap_or(false);
        if !allowed {
            return;
        }
    }
}

async fn resource(
    State(state): State<ApiState>,
    Path((id, path)): Path<(String, String)>,
    Query(query): Query<ScopeQuery>,
) -> Result<Response, ApiError> {
    let manager = locked(&state, "读取插件视图")?;
    if !manager
        .is_active_for_scope(&id, &query.scope)
        .map_err(plugin_error)?
    {
        return Err(ApiError::Forbidden("插件未激活".into()));
    }
    let resource_path = format!("ui/{path}");
    let package = manager.package(&id).map_err(plugin_error)?;
    let bytes = package
        .files()
        .get(&resource_path)
        .ok_or_else(|| ApiError::NotFound("插件视图资源不存在".into()))?;
    let content_type = match resource_path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
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
            "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; object-src 'none'; frame-ancestors 'self'; base-uri 'none'; form-action 'none'; connect-src 'none'",
        ),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

async fn read_resource(
    State(state): State<ApiState>,
    Path((id, resource)): Path<(String, String)>,
    Query(query): Query<ScopeQuery>,
) -> Result<Json<magi_plugin_system::PluginResource>, ApiError> {
    locked(&state, "读取插件资源")?
        .read_resource(&id, &query.scope, &resource)
        .map(Json)
        .map_err(plugin_error)
}

async fn write_resource(
    State(state): State<ApiState>,
    Path((id, resource)): Path<(String, String)>,
    Query(query): Query<ScopeQuery>,
    Json(request): Json<ResourceWriteRequest>,
) -> Result<Json<magi_plugin_system::PluginResource>, ApiError> {
    let mut manager = locked(&state, "写入插件资源")?;
    manager
        .ensure_editing_admitted(&id, &query.scope)
        .map_err(plugin_error)?;
    write_plugin_resource_with_event(
        &mut manager,
        &state.event_bus,
        &id,
        &query.scope,
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
    scope: &str,
    resource_id: &str,
    expected_version: u64,
    value: Value,
) -> Result<magi_plugin_system::PluginResource, magi_plugin_system::PluginError> {
    let resource = manager.write_resource(id, scope, resource_id, expected_version, value)?;
    publish_plugin_resource_event(event_bus, id, scope, resource_id, &resource);
    Ok(resource)
}

pub fn write_plugin_settings_with_event(
    manager: &mut PluginManager,
    event_bus: &InMemoryEventBus,
    id: &str,
    scope: &str,
    expected_version: u64,
    value: Value,
) -> Result<magi_plugin_system::PluginResource, magi_plugin_system::PluginError> {
    let resource = manager.write_settings(id, scope, expected_version, value)?;
    publish_plugin_resource_event(event_bus, id, scope, "__settings", &resource);
    Ok(resource)
}

fn publish_plugin_resource_event(
    event_bus: &InMemoryEventBus,
    id: &str,
    scope: &str,
    resource_id: &str,
    resource: &magi_plugin_system::PluginResource,
) {
    let event = EventEnvelope::projection(
        EventId::new(format!(
            "plugin-resource-{id}-{scope}-{resource_id}-{}",
            resource.version
        )),
        "plugin.resource.updated",
        serde_json::json!({
            "pluginId": id,
            "scope": scope,
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
    Query(query): Query<ScopeQuery>,
) -> Result<Json<magi_plugin_system::PluginResource>, ApiError> {
    locked(&state, "读取插件设置")?
        .read_settings(&id, &query.scope)
        .map(Json)
        .map_err(plugin_error)
}

async fn write_settings(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(query): Query<ScopeQuery>,
    Json(request): Json<ResourceWriteRequest>,
) -> Result<Json<magi_plugin_system::PluginResource>, ApiError> {
    let mut manager = locked(&state, "写入插件设置")?;
    manager
        .ensure_editing_admitted(&id, &query.scope)
        .map_err(plugin_error)?;
    write_plugin_settings_with_event(
        &mut manager,
        &state.event_bus,
        &id,
        &query.scope,
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

async fn download_bytes(url: &str, limit: usize, what: &str) -> Result<Vec<u8>, ApiError> {
    let client = reqwest::Client::builder()
        .timeout(PLUGIN_DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|error| {
            ApiError::internal_assembly(&format!("获取{what}失败"), error.to_string())
        })?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|error| ApiError::InvalidInput(format!("获取{what}失败: {error}")))?;
    if !response.status().is_success() {
        return Err(ApiError::InvalidInput(format!(
            "获取{what}返回 HTTP {}",
            response.status()
        )));
    }
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(ApiError::InvalidInput(format!("{what}大小超限")));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| ApiError::InvalidInput(format!("读取{what}失败: {error}")))?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(ApiError::InvalidInput(format!("{what}大小超限")));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
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
        request.expected_digest.as_deref(),
    )
    .await?;
    let mut manager = locked(&state, "安装插件")?;
    manager.install(&package, source).map_err(plugin_error)?;
    publish_plugin_registry_changed(&state);
    snapshot(&manager)
}

async fn upgrade(
    State(state): State<ApiState>,
    Json(request): Json<InstallRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    let source = parse_source(&request.source)?;
    let package = load_package(
        &source,
        request.archive_base64,
        request.expected_digest.as_deref(),
    )
    .await?;
    upgrade_installed_plugin(&state, &package, source).await?;
    {
        let manager = locked(&state, "读取插件升级结果")?;
        snapshot(&manager)
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

/// 升级的唯一入口：关闭该插件全部作用域的准入，等待（必要时取消）在途运行结算，
/// 再原子切换到新包。任一步失败都保持旧版本与旧激活状态不变。
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
    PluginManager::settle_runtime_leases(&state.plugin_manager, &drain)
        .await
        .map_err(plugin_error)?;
    // 旧写入者已停止：此时读取并转换数据。转换失败只让升级失败，旧版本与旧数据保持原状。
    let converted = convert_plugin_data(state, package).await?;
    {
        let mut manager = locked(state, "升级插件")?;
        manager
            .upgrade(package, source, &drain, converted)
            .map_err(plugin_error)?;
        drain.commit(&mut manager);
    }
    publish_plugin_registry_changed(state);
    release_application_resources(state, &package.manifest().id).await
}

/// 数据格式版本变化且已有数据时，由新版插件在 Worker 中转换（纯函数，无任何宿主能力）。
async fn convert_plugin_data(
    state: &ApiState,
    package: &PluginPackage,
) -> Result<Option<magi_plugin_system::ConvertedData>, ApiError> {
    let plan = locked(state, "读取插件待转换数据")?
        .plan_data_migration(package)
        .map_err(plugin_error)?;
    let Some(plan) = plan else {
        return Ok(None);
    };
    let migrator = state
        .plugin_data_migrator
        .clone()
        .ok_or_else(|| ApiError::InternalAssemblyError("插件数据转换执行器不可用".into()))?;
    let package = package.clone();
    tokio::task::spawn_blocking(move || migrator.convert(&package, &plan))
        .await
        .map_err(|_| ApiError::InternalAssemblyError("插件数据转换线程异常退出".into()))?
        .map(Some)
        .map_err(plugin_error)
}

/// GPT Web 的应用实例拥有唯一的页面、槽位和 Tunnel；应用级停用或升级提交后立即
/// 释放这些宿主资源，不能让通道在插件已不可调用时继续存活。
///
/// Tunnel 释放失败时保留待清理状态并返回冲突：生命周期操作不得在资源未真正释放时
/// 报告完成，调用方可以修复后重试。
async fn release_web_model_host(state: &ApiState) -> Result<(), ApiError> {
    state.forget_web_model_probe();
    state.stop_web_model().await;
    state.web_model.stop_tunnel(state).await.map_err(|error| {
        ApiError::Conflict(format!(
            "GPT Web 工具通道未能确认停止，已保留待清理状态，可重试：{error}"
        ))
    })
}

/// 第一方插件的应用级实例拥有宿主资源（GPT Web 的页面、槽位与 Tunnel；MCP 服务的监听与隧道）。
/// 应用级停用或升级提交后立即释放；其他插件没有宿主资源。
async fn release_application_resources(state: &ApiState, plugin_id: &str) -> Result<(), ApiError> {
    match plugin_id {
        magi_web_model::WEB_MODEL_PLUGIN_ID => release_web_model_host(state).await,
        crate::bundled_plugins::MCP_SERVER_PLUGIN_ID => {
            // 只停监听与隧道，不改持久化的开关：重新激活后按原配置恢复。
            state.mcp_service.release().await;
            Ok(())
        }
        _ => Ok(()),
    }
}

/// 应用级激活后按已保存的配置恢复宿主资源（持久配置只是“用户配置过”，不会在未激活时复活）。
fn restore_application_resources(state: &ApiState, plugin_id: &str) {
    let state = state.clone();
    match plugin_id {
        magi_web_model::WEB_MODEL_PLUGIN_ID => {
            tokio::spawn(async move {
                crate::web_model_channel::restore_configured_channel(&state).await;
            });
        }
        crate::bundled_plugins::MCP_SERVER_PLUGIN_ID => state.start_mcp_service_if_enabled(),
        _ => {}
    }
}

async fn authorize(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<AuthorizeRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    let mut manager = locked(&state, "授权插件")?;
    manager
        .authorize(&id, &request.scope, request.grants)
        .map_err(plugin_error)?;
    publish_plugin_registry_changed(&state);
    snapshot(&manager)
}
async fn enable(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<ScopeRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    mutate_scope(&state, |m| m.enable(&id, &request.scope), "启用插件")
}
async fn activate(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<ScopeRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    let result = mutate_scope(
        &state,
        |m| m.activate(&id, &request.scope).map(|_| ()),
        "激活插件",
    )?;
    if request.scope == "application" {
        restore_application_resources(&state, &id);
    }
    Ok(result)
}
async fn deactivate(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<ScopeRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    // 上一次停用已提交但 Tunnel 释放失败：插件已不可用，这里只重试待清理的资源释放。
    if id == magi_web_model::WEB_MODEL_PLUGIN_ID
        && request.scope == "application"
        && state.web_model.cleanup_pending()
        && !state.web_model_plugin_active()
    {
        release_web_model_host(&state).await?;
        let manager = locked(&state, "读取停用后的插件状态")?;
        return snapshot(&manager);
    }
    let drain = PluginManager::begin_deactivation(
        std::sync::Arc::clone(&state.plugin_manager),
        &id,
        &request.scope,
    )
    .map_err(plugin_error)?;
    // 先给在途运行一个自然完成的排空期；到期后统一取消并再给有限的结算窗口。
    // 仍未结算时保留激活状态并报告冲突，不在调用未结算时强行停用。
    PluginManager::settle_runtime_leases(&state.plugin_manager, &drain)
        .await
        .map_err(plugin_error)?;
    {
        let mut manager = locked(&state, "停用插件")?;
        manager
            .deactivate(&id, &request.scope)
            .map_err(plugin_error)?;
        drain.commit(&mut manager);
    }
    publish_plugin_registry_changed(&state);
    if request.scope == "application" {
        release_application_resources(&state, &id).await?;
    }
    {
        let manager = locked(&state, "读取停用后的插件状态")?;
        snapshot(&manager)
    }
}
async fn disable(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<ScopeRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    mutate_scope(&state, |m| m.disable(&id, &request.scope), "禁用插件")
}
async fn uninstall(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    if id == magi_web_model::WEB_MODEL_PLUGIN_ID && state.web_model.cleanup_pending() {
        return Err(ApiError::Conflict(
            "GPT Web 工具通道仍有待清理的托管运行时，请先重试停用后再卸载".into(),
        ));
    }
    let mut manager = locked(&state, "卸载插件")?;
    manager.uninstall(&id).map_err(plugin_error)?;
    publish_plugin_registry_changed(&state);
    snapshot(&manager)
}

// 只发布注册表失效事实；所有客户端经原清单入口读取，不复制激活或权限状态。
fn publish_plugin_registry_changed(state: &ApiState) {
    state.event_bus.publish(EventEnvelope::domain(
        EventId::unique("plugin-registry-changed"),
        "plugin.registry.changed",
        serde_json::json!({}),
    ));
}

fn mutate_scope<F>(
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
    snapshot(&manager)
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
fn snapshot(
    manager: &magi_plugin_system::PluginManager,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    manager.projection().map(Json).map_err(plugin_error)
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
    scope: &str,
    resource_id: &str,
) -> bool {
    event.event_type == "plugin.resource.updated"
        && event.payload.get("pluginId").and_then(Value::as_str) == Some(plugin_id)
        && event.payload.get("scope").and_then(Value::as_str) == Some(scope)
        && event.payload.get("resourceId").and_then(Value::as_str) == Some(resource_id)
}

fn resource_sse_snapshot(
    plugin_id: &str,
    scope: &str,
    resource_id: &str,
    resource: magi_plugin_system::PluginResource,
) -> Event {
    Event::default()
        .json_data(serde_json::json!({
            "pluginId": plugin_id,
            "scope": scope,
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
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(ApiError::InvalidInput(
            "插件来源只支持带主机的 HTTP(S) 地址".into(),
        ));
    }
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use futures_util::StreamExt;
    use magi_governance::GovernanceService;
    use magi_plugin_system::{
        PluginManager, PluginPackage, PluginPermission, PluginPermissionKind, PluginScopeKind,
        PluginSource,
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
            "sdkVersion": 1,
            "id": "acme.resources",
            "version": version,
            "name": "Resources",
            "description": "",
            "backend": "plugin.mjs",
            "permissions": [{
                "kind": "storage",
                "scope": "workspace",
                "targets": ["dashboard"]
            }],
            "dataSchemaVersion": 1,
            "settingsSchema": {"type":"object","additionalProperties":false},
            "contributions": {"resources":[{"id":"dashboard","title":"Dashboard","description":""}]}
        });
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("plugin.mjs", b"export default () => ({})".to_vec()),
        ] {
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

    async fn install_bundled_request(state: &ApiState) -> axum::response::Response {
        routes()
            .with_state(state.clone())
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/plugins/bundled/openai.chatgpt-web/install")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
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

    #[tokio::test]
    async fn bundled_gpt_web_install_uses_the_plugin_manager_contract() {
        let root = tempfile::tempdir().unwrap();
        let state = bare_state(root.path());
        let response = install_bundled_request(&state).await;
        assert_eq!(response.status(), StatusCode::OK);
        let manager = state.plugin_manager.lock().unwrap();
        let installed = manager.package("openai.chatgpt-web").unwrap();
        assert_eq!(installed.manifest().id, "openai.chatgpt-web");
        assert!(matches!(
            manager.state().plugins["openai.chatgpt-web"].source,
            PluginSource::Builtin { .. }
        ));
    }

    #[tokio::test]
    async fn bundled_install_upgrades_an_older_first_party_package_through_the_drain_path() {
        let root = tempfile::tempdir().unwrap();
        let state = bare_state(root.path());
        let mut older = crate::bundled_plugins::gpt_web_plugin_manifest();
        older.version = "0.9.0".into();
        let older =
            PluginPackage::from_manifest_and_source(older, "export default () => ({})").unwrap();
        state
            .plugin_manager
            .lock()
            .unwrap()
            .install(
                &older,
                PluginSource::Builtin {
                    name: "openai.chatgpt-web".into(),
                },
            )
            .unwrap();

        let response = install_bundled_request(&state).await;
        assert_eq!(response.status(), StatusCode::OK);
        let manager = state.plugin_manager.lock().unwrap();
        assert_eq!(
            manager
                .package("openai.chatgpt-web")
                .unwrap()
                .manifest()
                .version,
            crate::bundled_plugins::gpt_web_plugin_manifest().version
        );
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
        manager.enable("acme.resources", "application").unwrap();
        manager
            .authorize(
                "acme.resources",
                "application",
                vec![PluginPermission {
                    kind: PluginPermissionKind::Storage,
                    scope: PluginScopeKind::Workspace,
                    targets: vec!["dashboard".into()],
                }],
            )
            .unwrap();
        manager.activate("acme.resources", "application").unwrap();
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
    async fn resource_writes_publish_snapshot_and_subscription_updates() {
        let (state, root) = active_resource_state().await;
        let router = routes().with_state(state.clone());
        let stream = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/plugins/acme.resources/resources/dashboard/events?scope=application")
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
                    .uri("/plugins/acme.resources/resources/dashboard?scope=application")
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
                    .uri("/plugins/acme.resources/resources/dashboard/events?scope=application")
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
            .deactivate("acme.resources", "application")
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
    async fn upgrade_route_drains_the_plugin_and_requires_reauthorization() {
        let (state, root) = active_resource_state().await;
        let body = serde_json::json!({
            "source": "local:/tmp/acme-resources-1.1.0.zip",
            "archiveBase64": base64::engine::general_purpose::STANDARD.encode(archive("1.1.0")),
        });
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
        assert_eq!(plugin["enabledScopes"], serde_json::json!(["application"]));
        assert_eq!(plugin["activeScopes"], serde_json::json!([]));
        assert_eq!(plugin["grants"], serde_json::json!([]));
        assert_eq!(plugin["draining"], serde_json::json!([]));
        assert_eq!(list["lifecycle"]["drainTimeoutMs"], 30_000);
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
        use base64::Engine as _;
        let package = Arc::new(std::sync::Mutex::new(package_bytes.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let catalog = serde_json::to_vec(&serde_json::json!({
            "format": 1, "issuedAt": 1,
            "plugins": [{
                "id": "acme.resources", "name": "Resources", "description": "",
                "version": "1.0.0", "url": format!("{base}/package.zip"),
                "sha256": advertised_sha256, "size": package_bytes.len()
            }]
        }))
        .unwrap();
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
        let center = start_center(&signing, "center-1", archive.clone(), digest).await;
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

        // 发布服务被入侵：包内容被替换，但目录签名仍绑定原摘要，安装必须失败且不落盘。
        *center.package.lock().unwrap() = self::archive("1.0.1");
        let (status, _) = call(
            &state,
            "POST",
            "/plugins/center/install",
            Some(serde_json::json!({"id": "acme.resources"})),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
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
            Some(serde_json::json!({"id": "acme.resources"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["plugins"][0]["source"]["kind"], "center");
        let (_, body) = call(&state, "GET", "/plugins/center", None).await;
        assert_eq!(body["entries"][0]["state"], "installed");
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
            Some(serde_json::json!({"id": "acme.resources"})),
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
