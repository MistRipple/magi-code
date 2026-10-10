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
use magi_core::{EventId, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_plugin_system::{MAX_PACKAGE_BYTES, PluginPackage, PluginSource};
use serde::Deserialize;
use serde_json::Value;
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::BroadcastStream;

const PLUGIN_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

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
        .route("/plugins/manifests", get(manifests))
        .route("/plugins/commands", get(commands))
        .route("/plugins/workflows", get(workflows))
        .route("/plugins/{id}/manifest", get(manifest))
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
        .map_err(plugin_error)?
        .into_iter()
        .map(|(manifest, command)| {
            let digest = manager
                .package(&manifest.id)
                .map(|package| package.digest().to_string())
                .unwrap_or_default();
            serde_json::json!({
                "id": format!("plugin/{}/{}", manifest.id, command.id),
                "pluginId": manifest.id,
                "contributionId": command.id,
                "version": manifest.version,
                "digest": digest,
                "title": command.title,
                "description": command.description,
            })
        })
        .collect();
    Ok(Json(entries))
}

async fn workflows(
    State(state): State<ApiState>,
    Query(query): Query<ScopeQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    let manager = locked(&state, "读取插件工作流")?;
    let entries = manager
        .workflow_contributions_for_scope(&query.scope)
        .map_err(plugin_error)?
        .into_iter()
        .map(|(manifest, workflow)| {
            let digest = manager
                .package(&manifest.id)
                .map(|package| package.digest().to_string())
                .unwrap_or_default();
            serde_json::json!({
                "id": format!("plugin/{}/{}", manifest.id, workflow.id),
                "pluginId": manifest.id,
                "contributionId": workflow.id,
                "version": manifest.version,
                "digest": digest,
                "title": workflow.title,
                "description": workflow.description,
            })
        })
        .collect();
    Ok(Json(entries))
}

async fn manifest(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<magi_app_server_protocol::PluginManifest>, ApiError> {
    let manager = locked(&state, "读取插件清单")?;
    Ok(Json(
        manager
            .package(&id)
            .map_err(plugin_error)?
            .manifest()
            .clone(),
    ))
}

async fn resource_events(
    State(state): State<ApiState>,
    Path((id, resource)): Path<(String, String)>,
    Query(query): Query<ScopeQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let (initial_resource, receiver) = {
        let manager = locked(&state, "订阅插件资源")?;
        ensure_active_resource(&manager, &id, &query.scope, &resource)?;
        let initial_resource = manager
            .resources()
            .read(&id, &query.scope, &resource)
            .unwrap_or(magi_plugin_system::PluginResource {
                version: 0,
                value: Value::Null,
            });
        // The manager lock serializes this snapshot with every resource writer. The
        // event-bus cut is then registered before the lock is released, so a writer
        // can only appear in the initial snapshot or in the live stream.
        let (_snapshot, receiver) = state.event_bus.snapshot_and_subscribe();
        (initial_resource, receiver)
    };
    let initial = stream::once(future::ready(Ok::<Event, Infallible>(
        resource_sse_snapshot(&id, &query.scope, &resource, initial_resource),
    )));
    let live = BroadcastStream::new(receiver).filter_map(move |event| {
        future::ready(match event {
            Ok(envelope) => plugin_resource_event_matches(&envelope, &id, &query.scope, &resource)
                .then(|| Ok::<Event, Infallible>(resource_sse_event(envelope))),
            Err(_) => Some(Ok::<Event, Infallible>(resource_sse_reset())),
        })
    });
    Ok(Sse::new(initial.chain(live))
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(5))))
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
    let manager = locked(&state, "读取插件资源")?;
    if !manager
        .is_active_for_scope(&id, &query.scope)
        .map_err(plugin_error)?
    {
        return Err(ApiError::Forbidden("插件未激活".into()));
    }
    ensure_readable_resource(&manager, &id, &query.scope, &resource)?;
    let snapshot = manager
        .resources()
        .read(&id, &query.scope, &resource)
        .unwrap_or(magi_plugin_system::PluginResource {
            version: 0,
            value: Value::Null,
        });
    Ok(Json(snapshot))
}

async fn write_resource(
    State(state): State<ApiState>,
    Path((id, resource)): Path<(String, String)>,
    Query(query): Query<ScopeQuery>,
    Json(request): Json<ResourceWriteRequest>,
) -> Result<Json<magi_plugin_system::PluginResource>, ApiError> {
    let mut manager = locked(&state, "写入插件资源")?;
    ensure_active_resource(&manager, &id, &query.scope, &resource)?;
    let result = write_plugin_resource_with_event(
        &mut manager,
        &state.event_bus,
        &id,
        &query.scope,
        &resource,
        request.expected_version,
        request.value,
    );
    let result = result.map_err(|error| match error {
        magi_plugin_system::ResourceError::Conflict { .. } => ApiError::Conflict(error.to_string()),
        magi_plugin_system::ResourceError::Invalid(message) => ApiError::InvalidInput(message),
        magi_plugin_system::ResourceError::Storage(message) => {
            ApiError::internal_assembly("写入插件资源失败", message)
        }
    })?;
    Ok(Json(result))
}

pub fn write_plugin_resource_with_event(
    manager: &mut magi_plugin_system::PluginManager,
    event_bus: &InMemoryEventBus,
    id: &str,
    scope: &str,
    resource_id: &str,
    expected_version: u64,
    value: Value,
) -> Result<magi_plugin_system::PluginResource, magi_plugin_system::ResourceError> {
    let resource =
        manager
            .resources_mut()
            .write(id, scope, resource_id, expected_version, value)?;
    publish_plugin_resource_event(event_bus, id, scope, resource_id, &resource);
    Ok(resource)
}

pub fn write_plugin_settings_with_event(
    manager: &mut magi_plugin_system::PluginManager,
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
    let workspace_id = scope
        .strip_prefix("workspace:")
        .filter(|value| !value.is_empty())
        .map(WorkspaceId::new);
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
    .with_context(EventContext {
        workspace_id,
        ..EventContext::default()
    });
    event_bus.publish(event);
}

fn ensure_active_resource(
    manager: &magi_plugin_system::PluginManager,
    id: &str,
    scope: &str,
    resource: &str,
) -> Result<(), ApiError> {
    if !manager
        .is_active_for_scope(id, scope)
        .map_err(plugin_error)?
    {
        return Err(ApiError::Forbidden("插件未激活".into()));
    }
    ensure_readable_resource(manager, id, scope, resource)
}

fn ensure_readable_resource(
    manager: &magi_plugin_system::PluginManager,
    id: &str,
    scope: &str,
    resource: &str,
) -> Result<(), ApiError> {
    ensure_declared_resource(manager, id, resource)?;
    if !manager
        .permission_allowed(
            id,
            scope,
            magi_plugin_system::PluginPermissionKind::Storage,
            resource,
        )
        .map_err(plugin_error)?
    {
        return Err(ApiError::Forbidden("插件未获资源读取权限".into()));
    }
    Ok(())
}

async fn read_settings(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(query): Query<ScopeQuery>,
) -> Result<Json<magi_plugin_system::PluginResource>, ApiError> {
    let manager = locked(&state, "读取插件设置")?;
    if !manager
        .is_active_for_scope(&id, &query.scope)
        .map_err(plugin_error)?
    {
        return Err(ApiError::Forbidden("插件未激活".into()));
    }
    manager
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
    if !manager
        .is_active_for_scope(&id, &query.scope)
        .map_err(plugin_error)?
    {
        return Err(ApiError::Forbidden("插件未激活".into()));
    }
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

async fn install(
    State(state): State<ApiState>,
    Json(request): Json<InstallRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    let source = parse_source(&request.source)?;
    let bytes = match (request.archive_base64, &source) {
        (Some(encoded), _) => base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|_| ApiError::InvalidInput("插件包编码无效".into()))?,
        (None, PluginSource::Local { name }) => tokio::fs::read(name)
            .await
            .map_err(|error| ApiError::InvalidInput(format!("读取本地插件包失败: {error}")))?,
        (None, PluginSource::Center { url }) | (None, PluginSource::Address { url }) => {
            let client = reqwest::Client::builder()
                .timeout(PLUGIN_DOWNLOAD_TIMEOUT)
                .build()
                .map_err(|error| {
                    ApiError::internal_assembly("获取插件包失败", error.to_string())
                })?;
            let response = client
                .get(url)
                .send()
                .await
                .map_err(|error| ApiError::InvalidInput(format!("获取插件包失败: {error}")))?;
            if !response.status().is_success() {
                return Err(ApiError::InvalidInput(format!(
                    "获取插件包返回 HTTP {}",
                    response.status()
                )));
            }
            if response
                .content_length()
                .is_some_and(|size| size > MAX_PACKAGE_BYTES as u64)
            {
                return Err(ApiError::InvalidInput("插件包大小超限".into()));
            }
            let mut body = Vec::new();
            let mut response = response;
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| ApiError::InvalidInput(format!("读取插件包失败: {error}")))?
            {
                if body.len().saturating_add(chunk.len()) > MAX_PACKAGE_BYTES {
                    return Err(ApiError::InvalidInput("插件包大小超限".into()));
                }
                body.extend_from_slice(&chunk);
            }
            body
        }
    };
    let package = PluginPackage::from_archive(&bytes)
        .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
    let mut manager = state
        .plugin_manager
        .lock()
        .map_err(|_| ApiError::internal_assembly("安装插件失败", "插件锁已损坏"))?;
    manager.install(&package, source).map_err(plugin_error)?;
    manager.projection().map(Json).map_err(plugin_error)
}

async fn upgrade(
    State(state): State<ApiState>,
    Json(request): Json<InstallRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    let source = parse_source(&request.source)?;
    let bytes = match request.archive_base64 {
        Some(encoded) => base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|_| ApiError::InvalidInput("插件包编码无效".into()))?,
        None => {
            return Err(ApiError::InvalidInput(
                "升级必须直接提供已下载插件包".into(),
            ));
        }
    };
    let package = PluginPackage::from_archive(&bytes)
        .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
    let mut manager = state
        .plugin_manager
        .lock()
        .map_err(|_| ApiError::internal_assembly("升级插件失败", "插件锁已损坏"))?;
    manager.upgrade(&package, source).map_err(plugin_error)?;
    snapshot(&manager)
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
    mutate_scope(
        &state,
        |m| m.activate(&id, &request.scope).map(|_| ()),
        "激活插件",
    )
}
async fn deactivate(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<ScopeRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    mutate_scope(&state, |m| m.deactivate(&id, &request.scope), "停用插件")
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
    let mut manager = locked(&state, "卸载插件")?;
    manager.uninstall(&id).map_err(plugin_error)?;
    snapshot(&manager)
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

fn ensure_declared_resource(
    manager: &magi_plugin_system::PluginManager,
    id: &str,
    resource: &str,
) -> Result<(), ApiError> {
    let package = manager.package(id).map_err(plugin_error)?;
    if package
        .manifest()
        .contributions
        .resources
        .iter()
        .any(|item| item.id == resource)
    {
        Ok(())
    } else {
        Err(ApiError::NotFound("插件资源未声明".into()))
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

fn resource_sse_event(event: EventEnvelope) -> Event {
    Event::default()
        .json_data(&event.payload)
        .expect("plugin resource event payload must remain valid JSON")
}
fn parse_source(source: &str) -> Result<PluginSource, ApiError> {
    let (kind, value) = source.split_once(':').ok_or_else(|| {
        ApiError::InvalidInput("插件来源必须是 center:、address: 或 local:".into())
    })?;
    if value.trim().is_empty() {
        return Err(ApiError::InvalidInput("插件来源不能为空".into()));
    }
    match kind {
        "center" => Ok(PluginSource::Center {
            url: validated_remote_url(value)?,
        }),
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

    fn package() -> PluginPackage {
        let manifest = serde_json::json!({
            "sdkVersion": 1,
            "id": "acme.resources",
            "version": "1.0.0",
            "name": "Resources",
            "description": "",
            "backend": "plugin.mjs",
            "applicationInstance": false,
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
        PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap()
    }

    #[test]
    fn remote_plugin_sources_require_http_url() {
        assert!(parse_source("address:https://example.test/plugin.zip").is_ok());
        assert!(parse_source("center:file:///tmp/plugin.zip").is_err());
        assert!(parse_source("address:not-a-url").is_err());
    }

    async fn active_resource_state() -> (ApiState, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "magi-plugin-resource-events-{}-{}",
            std::process::id(),
            magi_core::UtcMillis::now().0
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut manager = PluginManager::open(&root).unwrap();
        manager
            .install(
                &package(),
                PluginSource::Local {
                    name: "test.zip".into(),
                },
            )
            .unwrap();
        manager.enable("acme.resources", "workspace:test").unwrap();
        manager
            .authorize(
                "acme.resources",
                "workspace:test",
                vec![PluginPermission {
                    kind: PluginPermissionKind::Storage,
                    scope: PluginScopeKind::Workspace,
                    targets: vec!["dashboard".into()],
                }],
            )
            .unwrap();
        manager
            .activate("acme.resources", "workspace:test")
            .unwrap();
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
    async fn resource_writes_publish_snapshot_and_scope_filtered_subscriptions() {
        let (state, root) = active_resource_state().await;
        let router = routes().with_state(state.clone());
        let stream = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri(
                        "/plugins/acme.resources/resources/dashboard/events?scope=workspace%3Atest",
                    )
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
                    .uri("/plugins/acme.resources/resources/dashboard?scope=workspace%3Atest")
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
        let _ = std::fs::remove_dir_all(root);
    }
}
