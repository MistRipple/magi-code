use crate::{errors::ApiError, native_plugins::PluginRouteSession, state::ApiState};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine as _;
use magi_app_server_protocol::generated::{PluginRouteCallRequest, PluginRouteCallResult};
use magi_core::EventId;
use magi_event_bus::EventEnvelope;
use magi_plugin_system::{MAX_PACKAGE_BYTES, PluginSource};
use serde::Deserialize;
use serde_json::Value;

/// JSON 上传承载合法归档的 base64（每三字节占四字符），另留有界元数据空间。
const MAX_INSTALL_REQUEST_BYTES: usize = MAX_PACKAGE_BYTES.div_ceil(3) * 4 + 64 * 1024;
/// 路由 JSON 可承载最多 2 MiB 的 base64 字节及有界头部；其他 HTTP 接口保持原上限。
const MAX_ROUTE_REQUEST_BYTES: usize = 3 * 1024 * 1024 + 64 * 1024;
const TRUSTED_VIEW_CONTENT_SECURITY_POLICY: &str = "sandbox allow-scripts allow-downloads; default-src 'none'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; object-src 'none'; frame-ancestors 'self'; base-uri 'none'; form-action 'none'; connect-src 'none'";

mod assets;
mod download;
mod guide;
mod upstream;
use download::download_bytes;
use guide::guide;
pub use upstream::{NpmArchive, fetch_npm_archive, fetch_npm_packument, fetch_npm_release_archive};

type InstallRequest = magi_app_server_protocol::PluginInstallRequest;
type LifecycleRequest = magi_app_server_protocol::generated::PluginLifecycleRequest;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginQuery {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginMuxQuery {
    #[serde(default)]
    expected_digest: Option<String>,
    #[serde(default)]
    expected_instance_id: Option<String>,
}

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/plugins", get(list))
        .route("/plugins/upstream", get(upstream::search))
        .route("/plugins/upstream/icon", get(upstream::icons::icon))
        .route("/plugins/upstream/preview", post(upstream::preview))
        .route("/plugins/upstream/install", post(upstream::install))
        .route("/plugins/install/{request_id}/cancel", post(cancel_install))
        .route("/plugins/guide", get(guide))
        .route("/plugins/commands", get(commands))
        .route("/plugins/manifests", get(manifests))
        .route("/plugins/{id}/ui/{*path}", get(resource))
        .route(
            "/plugins/{id}/ui-v/{digest}/{*path}",
            get(versioned_resource),
        )
        .route(
            "/plugins/{id}/ui-v/{digest}/instance/{instance}/{*path}",
            get(assets::document_resource),
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
        .route("/plugins/{id}/mux", get(open_mux))
        .route("/plugins/{id}/slots", post(report_slots))
        .route("/plugins/host/restart", post(restart_host))
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
            post(install).layer(DefaultBodyLimit::max(MAX_INSTALL_REQUEST_BYTES)),
        )
        .route("/plugins/{id}/enable", post(enable))
        .route("/plugins/{id}/disable", post(disable))
        .route("/plugins/{id}/uninstall", post(uninstall))
}

fn install_request_id(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    headers
        .get("x-magi-plugin-request-id")
        .map(|header| {
            let id = header
                .to_str()
                .map_err(|_| ApiError::InvalidInput("插件安装请求 ID 无效".into()))?;
            validate_install_request_id(id)?;
            Ok(id.to_owned())
        })
        .transpose()
}

fn validate_install_request_id(id: &str) -> Result<(), ApiError> {
    if !(16..=64).contains(&id.len()) || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(ApiError::InvalidInput("插件安装请求 ID 无效".into()));
    }
    Ok(())
}

async fn cancel_install(
    State(state): State<ApiState>,
    Path(request_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    validate_install_request_id(&request_id)?;
    native_platform(&state)?
        .cancel_install(request_id)
        .await
        .map(Json)
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
    native_platform(&state)?
        .enabled_manifests(&manager)
        .map(Json)
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
    let (content_type, bytes) =
        native_platform(state)?.serve_ui_asset(&manager, id, path, digest)?;
    if let Some(mime) = magi_media::image_content_type(path) {
        return Ok(crate::image_response::image_response(mime, bytes));
    }
    let mut response = Response::new(bytes.into());
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static(TRUSTED_VIEW_CONTENT_SECURITY_POLICY),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

/// 已安装插件的图标。与视图资源不同，列表要展示所有已安装插件，所以不要求插件已启用；
/// 只返回清单声明且通过包校验的那一个图片，其他包内文件不经此接口出去。
async fn icon(State(state): State<ApiState>, Path(id): Path<String>) -> Result<Response, ApiError> {
    let manager = locked(&state, "读取插件图标")?;
    let icon = native_platform(&state)?
        .serve_icon(&manager, &id)?
        .ok_or_else(|| ApiError::NotFound("插件没有声明图标".into()))?;
    Ok(icon_response(icon.0, icon.1))
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
    Json(request): Json<PluginRouteCallRequest>,
) -> Result<Json<PluginRouteCallResult>, ApiError> {
    dispatch_plugin_route(&state, id, request).await.map(Json)
}

/// 视图经可信外壳建立到共享宿主 DSH Typert WebSocket Mux（`/api/remote.mux`）的流式连接。
/// 只允许 WebSocket 升级且目标固定为 `/api/remote.mux`，租约随插件实例或宿主代次自动吊销。
async fn open_mux(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(query): Query<PluginMuxQuery>,
    mut request: axum::http::Request<axum::body::Body>,
) -> Result<Response, ApiError> {
    let is_ws = request
        .headers()
        .get(axum::http::header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    if !is_ws {
        return Err(ApiError::InvalidInput(
            "插件流式网关仅接受 WebSocket 升级请求".into(),
        ));
    }
    *request.uri_mut() = axum::http::Uri::from_static("/api/remote.mux");
    native_platform(&state)?
        .forward_gateway_upgrade(
            id,
            query.expected_digest,
            query.expected_instance_id,
            request,
        )
        .await
}

async fn dispatch_plugin_route(
    state: &ApiState,
    id: String,
    request: PluginRouteCallRequest,
) -> Result<PluginRouteCallResult, ApiError> {
    let session = request
        .session_id
        .as_deref()
        .map(|id| resolve_route_session(state, id))
        .transpose()?;
    native_platform(state)?
        .call_gateway(id, request, session)
        .await
}

fn resolve_route_session(state: &ApiState, id: &str) -> Result<PluginRouteSession, ApiError> {
    use magi_core::{HostPath, WorkspaceId};

    if id.is_empty() || id.len() > 128 || id.trim() != id {
        return Err(ApiError::InvalidInput("插件请求会话身份无效".into()));
    }
    let id = crate::scope_binding::parse_session_id(Some(id))?;
    let session = state
        .session_store
        .session(&id)
        .filter(|session| session.kind.is_user())
        .ok_or_else(|| ApiError::session_not_found(id.as_str()))?;
    let initial_directory = session
        .initial_working_directory_ref
        .as_deref()
        .map(HostPath::from_path_ref)
        .transpose()
        .map_err(|error| ApiError::Conflict(format!("会话创建目录不可用：{error}")))?;
    let execution_directory = if let Some(workspace) = session.workspace_id.as_deref() {
        let workspace = state
            .workspace_registry
            .project(&WorkspaceId::new(workspace))
            .ok_or_else(|| ApiError::not_found("workspace 不存在", workspace))?;
        let roots = workspace.source_folder_roots();
        let selected = if let Some(initial) = &initial_directory {
            let initial = magi_tool_runtime::canonicalize_tool_permission_path(initial.as_path());
            roots
                .into_iter()
                .find(|root| magi_tool_runtime::canonicalize_tool_permission_path(root) == initial)
                .ok_or_else(|| ApiError::Conflict("会话创建目录已不是项目来源".into()))?
        } else if roots.len() == 1 {
            workspace.native_root_path()
        } else {
            return Err(ApiError::Conflict("会话缺少已确认的项目来源".into()));
        };
        state.session_execution_root_for_source(&id, &selected)?
    } else {
        state.personal_session_execution_root(&id)?
    };
    let execution_directory = execution_directory
        .into_os_string()
        .into_string()
        .map_err(|_| ApiError::Conflict("会话工作目录无法交给插件读取".into()))?;
    let initial_directory = initial_directory
        .map(|path| {
            path.as_path()
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| ApiError::Conflict("会话创建目录无法交给插件读取".into()))
        })
        .transpose()?;
    Ok(PluginRouteSession {
        id,
        created_at: session.created_at,
        initial_directory,
        execution_directory,
    })
}

async fn report_slots(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(report): Json<crate::native_plugins::PluginClientSlotsReport>,
) -> Result<StatusCode, ApiError> {
    native_platform(&state)?.report_client_slots(&id, report)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn restart_host(
    State(state): State<ApiState>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    native_platform(&state)?.restart_host().await?;
    publish_plugin_registry_changed(&state);
    snapshot(&state, &*locked(&state, "读取插件状态")?)
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

async fn load_archive_bytes(
    source: &PluginSource,
    archive_base64: Option<String>,
) -> Result<Vec<u8>, ApiError> {
    match (archive_base64, source) {
        (Some(encoded), _) => base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|_| ApiError::InvalidInput("插件包编码无效".into())),
        (None, PluginSource::Local { name }) => read_local_package(name).await,
        (None, PluginSource::Address { url }) => download_package(url).await,
        (None, PluginSource::Center { .. }) => Err(ApiError::InvalidInput(
            "插件中心来源必须通过插件中心目录安装".into(),
        )),
        (None, PluginSource::Builtin { .. }) => Err(ApiError::InvalidInput(
            "内置插件必须通过内置安装入口".into(),
        )),
        (None, PluginSource::Upstream { .. }) => Err(ApiError::InvalidInput(
            "上游生态来源必须通过上游插件入口安装".into(),
        )),
    }
}

/// 准备阶段只解析和校验原包，不写入状态。
async fn preview(
    State(state): State<ApiState>,
    Json(request): Json<InstallRequest>,
) -> Result<Json<magi_app_server_protocol::generated::PluginCandidatePreview>, ApiError> {
    let native = native_platform(&state)?;
    let source = parse_source(&request.source)?;
    let bytes = load_archive_bytes(&source, request.archive_base64).await?;
    native
        .preview_archive(request.source, bytes)
        .await
        .map(Json)
}

async fn install(
    State(state): State<ApiState>,
    Json(request): Json<InstallRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    let native = native_platform(&state)?;
    let expected = confirmed_digest(request.expected_digest.as_deref())?.to_owned();
    let source = parse_source(&request.source)?;
    let bytes = load_archive_bytes(&source, request.archive_base64).await?;
    native
        .install_archive(request.source, bytes, expected)
        .await?;
    publish_plugin_registry_changed(&state);
    snapshot(&state, &*locked(&state, "读取插件安装结果")?)
}

async fn commands(
    State(state): State<ApiState>,
    Query(_query): Query<PluginQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    Ok(Json(native_platform(&state)?.enabled_commands()))
}

async fn change_enabled(
    state: &ApiState,
    id: String,
    enabled: bool,
    request: LifecycleRequest,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    native_platform(state)?
        .set_enabled(id, enabled, request.expected_digest)
        .await?;
    publish_plugin_registry_changed(state);
    snapshot(state, &*locked(state, "读取插件状态")?)
}

async fn enable(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    change_enabled(&state, id, true, request).await
}

async fn disable(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    change_enabled(&state, id, false, request).await
}

async fn uninstall(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    native_platform(&state)?.uninstall(id).await?;
    publish_plugin_registry_changed(&state);
    snapshot(&state, &*locked(&state, "读取插件状态")?)
}

fn native_platform(state: &ApiState) -> Result<&dyn crate::NativePluginPlatform, ApiError> {
    state
        .native_plugin_platform
        .as_deref()
        .ok_or_else(|| ApiError::Conflict("DSH 插件平台未装配".into()))
}

// 只发布注册表失效事实；所有客户端经原清单入口读取，不复制启用或权限状态。
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
/// 插件列表投影 = 管理器的安装/生命周期事实 + 扩展宿主的运行状态（只读合并，不另存）。
fn snapshot(
    state: &ApiState,
    manager: &magi_plugin_system::PluginManager,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    let platform = native_platform(state)?;
    Ok(Json(magi_app_server_protocol::PluginList {
        plugins: platform.list_installed(manager)?,
        configuration_error: platform.configuration_error(),
    }))
}

fn parse_source(source: &str) -> Result<PluginSource, ApiError> {
    let (kind, value) = source
        .split_once(':')
        .ok_or_else(|| ApiError::InvalidInput("插件来源必须是 address: 或 local:".into()))?;
    if value.trim().is_empty() {
        return Err(ApiError::InvalidInput("插件来源不能为空".into()));
    }
    match kind {
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

#[cfg(test)]
mod tests;
