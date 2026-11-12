//! Magi MCP 服务的管理路由（开关、令牌、配置片段）。
//!
//! 管理面只对可信本机入口开放：经公网隧道到达的请求一律拒绝，网络客户端只能使用 MCP 入口本身，
//! 不能创建、查看或吊销令牌（设计 M8）。列表与状态不含哈希和原文；原文只通过本机的“查看原文”接口取得。

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::{get, patch, post, put},
};
use magi_conversation_runtime::ToolApprovalDecision;
use magi_core::WorkspaceId;
use magi_mcp_server::{ApprovalMode, AttributionMode, IssueTokenRequest, TokenPatch, TokenRecord};
use serde::{Deserialize, Serialize};

use super::is_public_tunnel_request;
use crate::mcp_runtime::{DirectAccessRequest, McpRuntimeError, McpServiceStatus};
use crate::{errors::ApiError, state::ApiState};

/// 令牌默认有效期（天）。创建时可改；`0` 表示不过期。
const DEFAULT_TOKEN_TTL_DAYS: u32 = 90;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/mcp-server/status", get(get_status))
        .route("/mcp-server/oauth/requests", get(list_oauth_requests))
        .route(
            "/mcp-server/oauth/requests/{request_id}",
            post(resolve_oauth_request),
        )
        .route("/mcp-server/enabled", post(set_enabled))
        .route("/mcp-server/network", post(set_network))
        .route("/mcp-server/direct", put(set_direct_access))
        .route(
            "/mcp-server/named-tunnel",
            put(set_named_tunnel).delete(clear_named_tunnel),
        )
        .route("/mcp-server/named-tunnel/verify", post(verify_named_tunnel))
        .route("/mcp-server/tokens", post(create_token))
        .route("/mcp-server/tokens/revoke-all", post(revoke_all))
        .route(
            "/mcp-server/tokens/{token_id}",
            patch(update_token).delete(revoke_token),
        )
        .route("/mcp-server/tokens/{token_id}/secret", get(view_secret))
        .route("/mcp-server/tokens/{token_id}/rotate", post(rotate_token))
        .route("/mcp-server/config-snippets", get(config_snippets))
        .route("/mcp-server/audit", get(list_audit).delete(clear_audit))
        .route("/mcp-server/sessions", get(list_sessions))
        .route(
            "/mcp-server/directory-requests",
            get(list_directory_requests),
        )
        .route(
            "/mcp-server/directory-requests/{session_id}",
            post(resolve_directory_request),
        )
        .route("/mcp-server/external", put(set_external_retention))
        .route("/mcp-server/approvals", get(list_approvals))
        .route("/mcp-server/approvals/resolve", post(resolve_approval))
}

fn require_local(headers: &HeaderMap) -> Result<(), ApiError> {
    if is_public_tunnel_request(headers) {
        return Err(ApiError::Forbidden(
            "MCP 服务的管理只允许在本机应用内操作".to_string(),
        ));
    }
    Ok(())
}

fn runtime_error(error: McpRuntimeError) -> ApiError {
    match error {
        McpRuntimeError::Token(message) => ApiError::InvalidInput(message),
        McpRuntimeError::Bind { .. } => ApiError::Conflict(error.to_string()),
        McpRuntimeError::Persist(_) => ApiError::internal_assembly("MCP 服务配置", error),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum TokenState {
    Active,
    Expired,
    Revoked,
    WorkspaceUnavailable,
}

/// 令牌的对外形态：不含哈希。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenDto {
    token_id: String,
    prefix: String,
    client_name: String,
    workspace_id: String,
    source_folder_ids: Vec<String>,
    default_source_folder_id: Option<String>,
    workspace_available: bool,
    approval_mode: ApprovalMode,
    attribution: AttributionMode,
    /// 是否允许经公网隧道使用。
    network: bool,
    created_at_ms: u64,
    expires_at_ms: Option<u64>,
    revoked_at_ms: Option<u64>,
    last_used_at_ms: Option<u64>,
    active: bool,
    state: TokenState,
    /// 是否可以重新查看原文（没有保存原文的令牌只能重新生成）。
    has_secret: bool,
}

impl TokenDto {
    fn from_record(state: &ApiState, record: &TokenRecord, now_ms: u64) -> Self {
        let workspace_available = record.attribution == AttributionMode::Conversation
            || record.project_directories.as_ref().is_some_and(|binding| {
                state
                    .workspace_registry
                    .validate_directory_binding(binding)
                    .is_ok()
            });
        Self {
            token_id: record.token_id.clone(),
            prefix: record.prefix.clone(),
            client_name: record.client_name.clone(),
            workspace_id: record.workspace_id.clone(),
            source_folder_ids: record
                .project_directories
                .as_ref()
                .map(|binding| {
                    binding
                        .source_folders
                        .iter()
                        .map(|folder| folder.source_folder_id.clone())
                        .collect()
                })
                .unwrap_or_default(),
            default_source_folder_id: record
                .project_directories
                .as_ref()
                .map(|binding| binding.default_source_folder_id.clone()),
            workspace_available,
            approval_mode: record.approval_mode,
            attribution: record.attribution,
            network: record.network,
            created_at_ms: record.created_at_ms,
            expires_at_ms: record.expires_at_ms,
            revoked_at_ms: record.revoked_at_ms,
            last_used_at_ms: record.last_used_at_ms,
            active: record.is_active(now_ms),
            state: if record.revoked_at_ms.is_some() {
                TokenState::Revoked
            } else if !record.is_active(now_ms) {
                TokenState::Expired
            } else if !workspace_available {
                TokenState::WorkspaceUnavailable
            } else {
                TokenState::Active
            },
            has_secret: state.mcp_service.token_has_secret(&record.token_id),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusResponse {
    #[serde(flatten)]
    status: McpServiceStatus,
    tokens: Vec<TokenDto>,
}

fn now_ms() -> u64 {
    magi_core::UtcMillis::now().0
}

async fn status_response(state: &ApiState) -> StatusResponse {
    let now = now_ms();
    StatusResponse {
        status: state.mcp_service.status().await,
        tokens: state
            .mcp_service
            .list_tokens()
            .iter()
            .map(|record| TokenDto::from_record(state, record, now))
            .collect(),
    }
}

async fn get_status(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<StatusResponse>, ApiError> {
    require_local(&headers)?;
    Ok(Json(status_response(&state).await))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetEnabledRequest {
    enabled: bool,
}

async fn set_enabled(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<SetEnabledRequest>,
) -> Result<Json<StatusResponse>, ApiError> {
    require_local(&headers)?;
    state
        .mcp_service
        .set_enabled(&state, request.enabled)
        .await
        .map_err(runtime_error)?;
    Ok(Json(status_response(&state).await))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetNetworkRequest {
    enabled: bool,
    /// 开启公网隧道必须显式确认风险。
    #[serde(default)]
    confirm_risk: bool,
}

/// 开关网络模式（Quick Tunnel）。开启需要用户确认风险，且至少有一个允许网络访问的有效令牌。
async fn set_network(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<SetNetworkRequest>,
) -> Result<Json<StatusResponse>, ApiError> {
    require_local(&headers)?;
    if request.enabled && !request.confirm_risk {
        return Err(ApiError::InvalidInput(
            "开启网络模式会把 MCP 服务暴露到公网，需要显式确认风险".to_string(),
        ));
    }
    state
        .mcp_service
        .set_network(&state, request.enabled)
        .await
        .map_err(runtime_error)?;
    Ok(Json(status_response(&state).await))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetDirectAccessRequest {
    enabled: bool,
    /// 监听地址，缺省 `0.0.0.0`（所有网卡）。
    #[serde(default)]
    bind_host: Option<String>,
    /// 客户端实际访问的域名或内网／公网 IP。
    #[serde(default)]
    public_hosts: Vec<String>,
    /// 固定监听端口（防火墙需放行）；缺省沿用当前端口。
    #[serde(default)]
    port: Option<u16>,
    /// 开启必须显式确认：网络直连用的是明文 HTTP，令牌在网络上不加密。
    #[serde(default)]
    confirm_risk: bool,
}

/// 网络直连：不经隧道，HTTP 入口监听指定网卡或所有网卡。
async fn set_direct_access(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<SetDirectAccessRequest>,
) -> Result<Json<StatusResponse>, ApiError> {
    require_local(&headers)?;
    if request.enabled && !request.confirm_risk {
        return Err(ApiError::InvalidInput(
            "网络直连用的是明文 HTTP，令牌在网络上不加密，需要显式确认风险".to_string(),
        ));
    }
    if request.port == Some(0) {
        return Err(ApiError::InvalidInput(
            "端口必须在 1–65535 之间".to_string(),
        ));
    }
    state
        .mcp_service
        .set_direct_access(
            &state,
            DirectAccessRequest {
                enabled: request.enabled,
                bind_host: request.bind_host.unwrap_or_else(|| "0.0.0.0".to_string()),
                public_hosts: request.public_hosts,
                port: request.port,
            },
        )
        .await
        .map_err(runtime_error)?;
    Ok(Json(status_response(&state).await))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetNamedTunnelRequest {
    hostname: String,
    /// 隧道令牌，或 Cloudflare 控制台复制的整条 `cloudflared service install <令牌>` 命令。
    token: String,
}

/// 保存命名隧道（自己的域名 + 隧道令牌）：地址固定，网络模式开着时 daemon 重启后自动恢复。
async fn set_named_tunnel(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<SetNamedTunnelRequest>,
) -> Result<Json<StatusResponse>, ApiError> {
    require_local(&headers)?;
    state
        .mcp_service
        .set_named_tunnel(&request.hostname, &request.token)
        .await
        .map_err(runtime_error)?;
    Ok(Json(status_response(&state).await))
}

async fn clear_named_tunnel(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<StatusResponse>, ApiError> {
    require_local(&headers)?;
    state
        .mcp_service
        .clear_named_tunnel()
        .await
        .map_err(runtime_error)?;
    Ok(Json(status_response(&state).await))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyNamedTunnelResponse {
    ok: bool,
    /// `ok` / `not_configured` / `network_off` / `dns_unresolved` / `tunnel_not_connected` /
    /// `origin_unreachable` / `unexpected`
    code: String,
    http_status: Option<u16>,
}

/// 从公网访问一次 `https://<域名>/mcp`（不带令牌）：返回 401 说明整条链路（DNS → Cloudflare →
/// 隧道 → Magi MCP）都通，且入口在要求令牌。
async fn verify_named_tunnel(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<VerifyNamedTunnelResponse>, ApiError> {
    require_local(&headers)?;
    let respond = |ok: bool, code: &str, http_status: Option<u16>| {
        Ok(Json(VerifyNamedTunnelResponse {
            ok,
            code: code.to_string(),
            http_status,
        }))
    };
    let Some(hostname) = state.mcp_service.named_tunnel_hostname() else {
        return respond(false, "not_configured", None);
    };
    let status = state.mcp_service.status().await;
    if !status.network.enabled || status.network.status != "running" {
        return respond(false, "network_off", None);
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!("Magi-MCP/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| ApiError::internal_assembly("连通性检查", error))?;
    // MCP 入口只接受 POST/DELETE；GET 会在认证之前返回 405，不能用于检查认证链路。
    match client
        .post(format!("https://{hostname}/mcp"))
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }))
        .send()
        .await
    {
        Ok(response) => {
            let code = response.status().as_u16();
            match code {
                401 => respond(true, "ok", Some(code)),
                // Cloudflare：隧道没有连接（1033）/ 源站不可达。
                530 | 1033 => respond(false, "tunnel_not_connected", Some(code)),
                502..=504 => respond(false, "origin_unreachable", Some(code)),
                _ => respond(false, "unexpected", Some(code)),
            }
        }
        Err(error) if error.is_connect() => respond(false, "dns_unresolved", None),
        Err(_) => respond(false, "unexpected", None),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateTokenRequest {
    client_name: String,
    #[serde(default)]
    workspace_id: String,
    #[serde(default)]
    attribution: AttributionMode,
    #[serde(default)]
    source_folder_ids: Option<Vec<String>>,
    #[serde(default)]
    source_folder_id: Option<String>,
    /// 需要人工确认的调用如何处理；默认每次询问。
    #[serde(default)]
    approval_mode: ApprovalMode,
    /// 有效期天数。缺省 90；`0` 表示不过期。
    #[serde(default)]
    ttl_days: Option<u32>,
    /// 允许这个令牌经公网隧道使用（网络模式）。默认只能在本机使用。
    #[serde(default)]
    allow_network: bool,
}

/// 有效期天数换算为毫秒；`0` 表示不过期。
fn ttl_days_to_ms(days: u32) -> Option<u64> {
    match days {
        0 => None,
        days => Some(u64::from(days) * DAY_MS),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateTokenResponse {
    /// 令牌原文，只在这一次响应里出现。
    secret: String,
    token: TokenDto,
}

async fn create_token(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateTokenRequest>,
) -> Result<Json<CreateTokenResponse>, ApiError> {
    require_local(&headers)?;
    if request.attribution == AttributionMode::External
        && state
            .workspace_root_path(&Some(WorkspaceId::new(request.workspace_id.trim())))
            .is_none()
    {
        return Err(ApiError::InvalidInput("工作区不存在".to_string()));
    }
    let ttl_ms = ttl_days_to_ms(request.ttl_days.unwrap_or(DEFAULT_TOKEN_TTL_DAYS));
    let project_directories = if request.attribution == AttributionMode::Conversation {
        if !request.workspace_id.is_empty()
            || request.source_folder_id.is_some()
            || request.source_folder_ids.is_some()
        {
            return Err(ApiError::InvalidInput(
                "逐会话选择目录的客户端不能预先绑定项目".into(),
            ));
        }
        None
    } else {
        Some(
            state
                .workspace_registry
                .capture_directory_binding(
                    &WorkspaceId::new(request.workspace_id.trim()),
                    request.source_folder_id.as_deref(),
                    request.source_folder_ids.as_deref(),
                )
                .map_err(|error| ApiError::InvalidInput(error.to_string()))?,
        )
    };
    if let Some(binding) = &project_directories {
        state
            .workspace_registry
            .validate_directory_binding(binding)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
    }
    let issued = state
        .mcp_service
        .issue_token(IssueTokenRequest {
            project_directories,
            client_name: request.client_name,
            workspace_id: request.workspace_id,
            approval_mode: request.approval_mode,
            attribution: request.attribution,
            ttl_ms,
            network: request.allow_network,
        })
        .await
        .map_err(runtime_error)?;
    Ok(Json(CreateTokenResponse {
        token: TokenDto::from_record(&state, &issued.record, now_ms()),
        secret: issued.secret.clone(),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateTokenRequest {
    #[serde(default)]
    client_name: Option<String>,
    /// 需要人工确认的调用如何处理；缺省不改。
    #[serde(default)]
    approval_mode: Option<ApprovalMode>,
    /// 从现在起重新计算的有效期天数；`0` 表示不过期。缺省不改。
    #[serde(default)]
    ttl_days: Option<u32>,
    #[serde(default)]
    allow_network: Option<bool>,
}

/// 编辑令牌的名称、授权方式、有效期与网络开关。工作区和归属方式不可改，原文不变，下一次调用起生效。
async fn update_token(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(token_id): Path<String>,
    Json(request): Json<UpdateTokenRequest>,
) -> Result<Json<StatusResponse>, ApiError> {
    require_local(&headers)?;
    let current = state
        .mcp_service
        .list_tokens()
        .into_iter()
        .find(|record| record.token_id == token_id)
        .ok_or_else(|| ApiError::not_found("令牌不存在", &token_id))?;

    let updated = state
        .mcp_service
        .update_token(
            &state,
            &token_id,
            TokenPatch {
                client_name: request.client_name,
                approval_mode: request.approval_mode,
                expires_at_ms: request
                    .ttl_days
                    .map(|days| ttl_days_to_ms(days).map(|ms| now_ms().saturating_add(ms))),
                network: request.allow_network,
            },
        )
        .await
        .map_err(runtime_error)?;
    audit_management(
        &state,
        &updated,
        "token.update",
        Some(describe_update(&current, &updated)),
    );
    Ok(Json(status_response(&state).await))
}

/// 审计里的“旧值 → 新值”摘要，只含配置，不含原文。
fn describe_update(before: &TokenRecord, after: &TokenRecord) -> String {
    let mut changes = Vec::new();
    if before.client_name != after.client_name {
        changes.push(format!(
            "name: {} → {}",
            before.client_name, after.client_name
        ));
    }
    if before.approval_mode != after.approval_mode {
        changes.push(format!(
            "approvalMode: {:?} → {:?}",
            before.approval_mode, after.approval_mode
        ));
    }
    if before.network != after.network {
        changes.push(format!("network: {} → {}", before.network, after.network));
    }
    if before.expires_at_ms != after.expires_at_ms {
        changes.push(format!(
            "expiresAtMs: {:?} → {:?}",
            before.expires_at_ms, after.expires_at_ms
        ));
    }
    if changes.is_empty() {
        "no change".to_string()
    } else {
        changes.join("; ")
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SecretResponse {
    secret: String,
}

fn audit_management(state: &ApiState, record: &TokenRecord, action: &str, detail: Option<String>) {
    state
        .mcp_service
        .audit()
        .record(crate::mcp_runtime::AuditEntry {
            session_id: None,
            token_id: record.token_id.clone(),
            client_name: record.client_name.clone(),
            workspace_id: record.workspace_id.clone(),
            tool: action.to_string(),
            requires_approval: false,
            paths: Vec::new(),
            outcome: "succeeded".to_string(),
            detail,
            at_ms: now_ms(),
        });
}

fn find_token(state: &ApiState, token_id: &str) -> Result<TokenRecord, ApiError> {
    state
        .mcp_service
        .list_tokens()
        .into_iter()
        .find(|record| record.token_id == token_id)
        .ok_or_else(|| ApiError::not_found("令牌不存在", token_id))
}

/// 重新查看令牌原文。令牌属于用户本机数据，只要是本机入口就可以查看，每次查看记入审计。
async fn view_secret(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(token_id): Path<String>,
) -> Result<Json<SecretResponse>, ApiError> {
    require_local(&headers)?;
    let record = find_token(&state, &token_id)?;
    let secret = state.mcp_service.token_secret(&token_id).ok_or_else(|| {
        ApiError::Conflict(
            "该令牌没有可查看的原文（未保存、已吊销或已过期），请重新生成".to_string(),
        )
    })?;
    audit_management(&state, &record, "token.view_secret", None);
    Ok(Json(SecretResponse { secret }))
}

/// 重新生成令牌原文：旧原文立即失效，客户端需要换成新的。
async fn rotate_token(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(token_id): Path<String>,
) -> Result<Json<CreateTokenResponse>, ApiError> {
    require_local(&headers)?;
    find_token(&state, &token_id)?;

    let (record, secret) = state
        .mcp_service
        .rotate_token(&state, &token_id)
        .await
        .map_err(runtime_error)?;
    audit_management(&state, &record, "token.rotate", None);
    Ok(Json(CreateTokenResponse {
        token: TokenDto::from_record(&state, &record, now_ms()),
        secret,
    }))
}

async fn revoke_token(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(token_id): Path<String>,
) -> Result<Json<StatusResponse>, ApiError> {
    require_local(&headers)?;
    if !state
        .mcp_service
        .revoke(&state, &token_id)
        .await
        .map_err(runtime_error)?
    {
        return Err(ApiError::not_found("令牌不存在", &token_id));
    }
    Ok(Json(status_response(&state).await))
}

async fn revoke_all(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<StatusResponse>, ApiError> {
    require_local(&headers)?;
    state
        .mcp_service
        .revoke_all(&state)
        .await
        .map_err(runtime_error)?;
    Ok(Json(status_response(&state).await))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigSnippets {
    url: Option<String>,
    /// 配置里的服务名（工作区 + 客户端名，便于在外部客户端识别操作目标）。
    server_name: String,
    /// 带令牌的配置里是否已经填入真实原文；否则是 `<MAGI_MCP_TOKEN>` 占位符。
    secret_filled: bool,
    /// HTTP 配置（Cursor、Claude Code 等）。
    http_json: Option<serde_json::Value>,
    /// stdio 配置（Claude Desktop 等本机客户端）：命令是 daemon 自带的 `mcp-relay` 中继。
    stdio_json: Option<serde_json::Value>,
    /// Claude Code 的一行命令。
    claude_cli: Option<String>,
    /// 公网地址的客户端配置（网络模式运行时）。Quick Tunnel 的地址每次开启都会变，命名隧道固定。
    remote_json: Option<serde_json::Value>,
    /// 公网地址的 Claude Code 一行命令。
    remote_claude_cli: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnippetsQuery {
    /// 指定令牌时，配置里直接填入该令牌的原文（本机、可重新查看的令牌）。
    #[serde(default)]
    token_id: Option<String>,
}

/// 配置里的服务名：`magi-<工作区>-<客户端>`，其余字符折叠成 `-`。
fn server_name_for(workspace_name: &str, client_name: &str) -> String {
    let mut slug = String::new();
    for ch in format!("{}-{}", workspace_name.trim(), client_name.trim()).chars() {
        if ch.is_alphanumeric() {
            slug.extend(ch.to_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "magi".to_string()
    } else {
        format!("magi-{slug}")
    }
}

/// daemon 自己的可执行文件就是 stdio 中继（`mcp-relay` 子命令）。
fn relay_command() -> String {
    std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "magi-daemon-app".to_string())
}

async fn config_snippets(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<SnippetsQuery>,
) -> Result<Json<ConfigSnippets>, ApiError> {
    require_local(&headers)?;
    let status = state.mcp_service.status().await;
    let (server_name, secret) = match query.token_id.as_deref() {
        Some(token_id) => {
            let record = find_token(&state, token_id)?;
            let secret = state.mcp_service.token_secret(token_id);
            if secret.is_some() {
                audit_management(&state, &record, "token.view_secret", None);
            }
            let workspace_name = crate::mcp_service::workspace_name(&state, &record.workspace_id)
                .unwrap_or_else(|| record.workspace_id.clone());
            (
                server_name_for(&workspace_name, &record.client_name),
                secret,
            )
        }
        None => ("magi".to_string(), None),
    };
    let secret_filled = secret.is_some();
    let token = secret.unwrap_or_else(|| "<MAGI_MCP_TOKEN>".to_string());
    Ok(Json(build_snippets(
        server_name,
        &token,
        secret_filled,
        status.url,
        status.stdio_endpoint.as_deref(),
        status
            .network
            .mcp_url
            .as_deref()
            .or(status.direct.mcp_url.as_deref()),
    )))
}

fn build_snippets(
    server_name: String,
    token: &str,
    secret_filled: bool,
    url: Option<String>,
    stdio_endpoint: Option<&str>,
    remote_url: Option<&str>,
) -> ConfigSnippets {
    let bearer = format!("Bearer {token}");
    let http_for = |url: &str| {
        serde_json::json!({
            "mcpServers": {
                server_name.as_str(): {
                    "url": url,
                    "headers": { "Authorization": bearer }
                }
            }
        })
    };
    let http_json = url.as_deref().map(http_for);
    let stdio_json = stdio_endpoint.map(|endpoint| {
        serde_json::json!({
            "mcpServers": {
                server_name.as_str(): {
                    "command": relay_command(),
                    "args": ["mcp-relay", "--stdio", "--endpoint", endpoint],
                    "env": { "MAGI_MCP_TOKEN": token }
                }
            }
        })
    });
    let claude_cli = url.as_deref().map(|url| {
        format!(
            "claude mcp add --transport http {server_name} {url} --header \"Authorization: {bearer}\""
        )
    });
    let remote_json = remote_url.map(http_for);
    let remote_claude_cli = remote_url.map(|url| {
        format!(
            "claude mcp add --transport http {server_name} {url} --header \"Authorization: {bearer}\""
        )
    });
    ConfigSnippets {
        url,
        server_name,
        secret_filled,
        http_json,
        stdio_json,
        claude_cli,
        remote_json,
        remote_claude_cli,
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExternalApprovalDto {
    session_id: String,
    approval_id: String,
    token_id: String,
    client_name: String,
    token_prefix: String,
    workspace_id: String,
    tool_name: String,
    /// 对应调用的 id（`mcp-…`；SafetyGate 的补充确认带 `-safety` 后缀），界面据此把确认放回调用卡片上。
    tool_call_id: String,
    /// 操作摘要（工具与路径），不含文件正文。
    summary: String,
    requested_at_ms: u64,
    /// 到期时间。超时后调用以错误收口，不会稍后补执行。
    expires_at_ms: u64,
}

fn pending_external_approvals(state: &ApiState) -> Vec<ExternalApprovalDto> {
    let mut approvals = Vec::new();
    for owner in crate::mcp_service::approval_owners(state) {
        let task_id = magi_conversation_runtime::external_approval::external_approval_task_id(
            &owner.token_id,
        );
        for pending in state
            .conversation_registry
            .tool_approvals()
            .pending_for_session(&owner.session_id)
        {
            // 按令牌任务标识筛选，只取属于该调用方的外部审批。
            if pending.task_id != task_id {
                continue;
            }
            approvals.push(ExternalApprovalDto {
                session_id: owner.session_id.to_string(),
                approval_id: pending.approval_id,
                token_id: owner.token_id.clone(),
                client_name: owner.client_name.clone(),
                token_prefix: owner.token_prefix.clone(),
                workspace_id: owner.workspace_id.clone(),
                tool_name: pending.tool_name,
                tool_call_id: pending.tool_call_id,
                summary: pending.reason,
                requested_at_ms: pending.requested_at.0,
                expires_at_ms: pending.requested_at.0.saturating_add(
                    crate::mcp_service::EXTERNAL_APPROVAL_TIMEOUT.as_millis() as u64,
                ),
            });
        }
    }
    approvals.sort_by_key(|approval| approval.requested_at_ms);
    approvals
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalsResponse {
    approvals: Vec<ExternalApprovalDto>,
}

/// 跨会话的外部待审批：不依赖用户当前打开的会话。
async fn list_approvals(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<ApprovalsResponse>, ApiError> {
    require_local(&headers)?;
    Ok(Json(ApprovalsResponse {
        approvals: pending_external_approvals(&state),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolveApprovalRequest {
    approval_id: String,
    decision: ExternalApprovalDecision,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExternalApprovalDecision {
    AllowOnce,
    Deny,
}

async fn resolve_approval(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<ResolveApprovalRequest>,
) -> Result<Json<ApprovalsResponse>, ApiError> {
    require_local(&headers)?;
    let owner = crate::mcp_service::approval_owners(&state)
        .into_iter()
        .find(|owner| {
            let task_id = magi_conversation_runtime::external_approval::external_approval_task_id(
                &owner.token_id,
            );
            state
                .conversation_registry
                .tool_approvals()
                .pending_for_session(&owner.session_id)
                .iter()
                .any(|pending| {
                    pending.approval_id == request.approval_id && pending.task_id == task_id
                })
        })
        .map(|owner| owner.session_id)
        .ok_or_else(|| ApiError::not_found("授权请求不存在或已处理", &request.approval_id))?;
    // 托盘与会话卡片必须沿同一结算入口发布 resolved 事件；否则外部工具已继续，
    // 会话中的待审批投影仍会锁住输入框。
    crate::interaction_service::resolve_approval(
        &state,
        &owner,
        &request.approval_id,
        match request.decision {
            ExternalApprovalDecision::AllowOnce => ToolApprovalDecision::AllowOnce,
            ExternalApprovalDecision::Deny => ToolApprovalDecision::Deny,
        },
    )?;
    Ok(Json(ApprovalsResponse {
        approvals: pending_external_approvals(&state),
    }))
}

/// 调用记录每页最多条数。
const AUDIT_PAGE_MAX: usize = 200;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AuditQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    token_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuditResponse {
    entries: Vec<crate::mcp_runtime::AuditEntry>,
    /// 符合过滤条件的总条数，用于分页。
    total: usize,
}

async fn list_audit(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<AuditQuery>,
) -> Result<Json<AuditResponse>, ApiError> {
    require_local(&headers)?;
    let limit = query.limit.unwrap_or(50).clamp(1, AUDIT_PAGE_MAX);
    let (entries, total) =
        state
            .mcp_service
            .audit()
            .page(limit, query.offset.unwrap_or(0), query.token_id.as_deref());
    Ok(Json(AuditResponse { entries, total }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClearAuditQuery {
    /// 只清理这个令牌的记录；缺省清空全部。
    #[serde(default)]
    token_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClearAuditResponse {
    removed: usize,
}

/// 清理调用记录。记录只用于回看，不参与授权或回退；清理在本机界面里经用户确认后进行。
async fn clear_audit(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<ClearAuditQuery>,
) -> Result<Json<ClearAuditResponse>, ApiError> {
    require_local(&headers)?;
    let removed = state
        .mcp_service
        .audit()
        .clear(query.token_id.as_deref())
        .map_err(|error| ApiError::internal_assembly("调用记录", error))?;
    // 外部会话里的调用过程是同一批调用的详细内容，清理记录时一并清除（外部客户端再次调用会建立新会话）。
    state
        .mcp_service
        .external_turns
        .clear_sessions(&state, query.token_id.as_deref())
        .await;
    Ok(Json(ClearAuditResponse { removed }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryRequestDto {
    session_id: String,
    client_name: String,
    conversation_id: String,
    title: String,
    requested_at_ms: u64,
}

async fn list_directory_requests(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Vec<DirectoryRequestDto>>, ApiError> {
    require_local(&headers)?;
    let tokens = state.mcp_service.list_tokens();
    Ok(Json(
        state
            .mcp_service
            .conversations
            .pending(&state)
            .into_iter()
            .filter_map(|record| {
                let token = tokens
                    .iter()
                    .find(|token| token.token_id == record.token_id)?;
                Some(DirectoryRequestDto {
                    session_id: record.session_id,
                    client_name: token.client_name.clone(),
                    conversation_id: record.conversation_id,
                    title: record.title,
                    requested_at_ms: record.requested_at_ms,
                })
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DirectoryDecision {
    /// None 表示拒绝。授权必须由可信本机界面提供明确项目和目录。
    workspace_id: Option<String>,
    source_folder_id: Option<String>,
}

async fn resolve_directory_request(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(request): Json<DirectoryDecision>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_local(&headers)?;
    let _pending = state
        .mcp_service
        .conversations
        .pending(&state)
        .into_iter()
        .find(|record| record.session_id == session_id)
        .ok_or_else(|| ApiError::not_found("目录选择请求不存在或已处理", &session_id))?;
    let directories = if let Some(workspace) = &request.workspace_id {
        let source = request
            .source_folder_id
            .as_ref()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| ApiError::InvalidInput("请选择本会话的执行目录".into()))?;
        Some(
            state
                .workspace_registry
                .capture_directory_binding(
                    &WorkspaceId::new(workspace),
                    Some(source),
                    Some(std::slice::from_ref(source)),
                )
                .map_err(|error| ApiError::InvalidInput(error.to_string()))?,
        )
    } else {
        None
    };
    state
        .mcp_service
        .conversations
        .decide(&state, &session_id, directories)
        .map_err(ApiError::InvalidInput)?;
    crate::external_turns::publish_changed(&state, &magi_core::SessionId::new(session_id));
    Ok(Json(serde_json::json!({"ok":true})))
}

/// 外部客户端（一个令牌）的外部会话。会话本身是普通会话，调用过程在它的 canonical 时间线里；
/// 这里只给侧栏提供入口所需的概要。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExternalSessionDto {
    title: Option<String>,
    token_id: String,
    session_id: String,
    client_name: String,
    workspace_id: Option<String>,
    workspace_available: bool,
    last_activity_at_ms: u64,
    /// 在途调用数（含等待人工确认的）。
    running: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionsResponse {
    sessions: Vec<ExternalSessionDto>,
}

/// 有活动的外部会话，最近活动的在前。
async fn list_sessions(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<SessionsResponse>, ApiError> {
    require_local(&headers)?;
    let tokens = state.mcp_service.list_tokens();
    let conversations = state.mcp_service.conversations.records();
    let mut sessions = state
        .session_store
        .sessions()
        .into_iter()
        .filter(|session| session.kind == magi_session_store::SessionKind::ExternalTool)
        .filter_map(|session| {
            let conversation = conversations
                .iter()
                .find(|record| record.session_id == session.session_id.as_str());
            let token_id = conversation
                .map(|record| record.token_id.clone())
                .or_else(|| {
                    session
                        .session_id
                        .as_str()
                        .strip_prefix(crate::mcp_service::EXTERNAL_SESSION_ID_PREFIX)
                        .map(str::to_string)
                })?;
            let client_name = tokens
                .iter()
                .find(|record| record.token_id == token_id)
                .map(|record| record.client_name.clone())
                .unwrap_or_else(|| {
                    session
                        .title
                        .strip_suffix(" · 外部工具")
                        .unwrap_or(&session.title)
                        .to_string()
                });
            Some(ExternalSessionDto {
                // 早期未命名聊天把 conversation_id 写成标题；展示投影不再
                // 泄露技术标识，保留后来用户重命名的标题，不修改历史记录。
                title: conversation
                    .filter(|record| {
                        !session.title.trim().is_empty()
                            && !(record.title.is_empty() && session.title == record.conversation_id)
                    })
                    .map(|_| session.title.clone()),
                token_id,
                running: state
                    .mcp_service
                    .external_turns
                    .in_flight(&session.session_id),
                session_id: session.session_id.to_string(),
                client_name,
                workspace_available: session.workspace_id.as_ref().is_some_and(|id| {
                    state
                        .workspace_root_path(&Some(WorkspaceId::new(id)))
                        .is_some()
                }),
                workspace_id: session.workspace_id,
                last_activity_at_ms: session.updated_at.0,
            })
        })
        .collect::<Vec<_>>();
    sessions.sort_by_key(|session| std::cmp::Reverse(session.last_activity_at_ms));
    Ok(Json(SessionsResponse { sessions }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetExternalRetentionRequest {
    save_content: bool,
    retention_days: u32,
}

/// 外部会话的隐私与留存：是否保存调用内容，以及保留多少天（0 表示不自动清理）。
async fn set_external_retention(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<SetExternalRetentionRequest>,
) -> Result<Json<StatusResponse>, ApiError> {
    require_local(&headers)?;
    state
        .mcp_service
        .set_retention(crate::mcp_runtime::ExternalRetention {
            save_content: request.save_content,
            retention_days: request.retention_days,
        })
        .await
        .map_err(runtime_error)?;
    state.mcp_service.external_turns.purge_expired(&state).await;
    Ok(Json(status_response(&state).await))
}

async fn list_oauth_requests(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Vec<magi_mcp_server::oauth::PendingConsent>>, ApiError> {
    require_local(&headers)?;
    Ok(Json(state.mcp_service.oauth.pending(now_ms())))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OAuthConsentRequest {
    token_id: Option<String>,
}

async fn resolve_oauth_request(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
    Json(request): Json<OAuthConsentRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_local(&headers)?;
    let token = request
        .token_id
        .map(|id| {
            state
                .mcp_service
                .list_tokens()
                .into_iter()
                .find(|token| token.token_id == id && token.is_active(now_ms()) && token.network)
                .ok_or_else(|| ApiError::InvalidInput("请选择有效的公网客户端".into()))
        })
        .transpose()?;
    state
        .mcp_service
        .oauth
        .resolve(&request_id, token.as_ref(), now_ms())
        .map_err(ApiError::InvalidInput)?;
    state
        .mcp_service
        .audit()
        .record(crate::mcp_runtime::AuditEntry {
            session_id: None,
            token_id: token
                .as_ref()
                .map(|t| t.token_id.clone())
                .unwrap_or_default(),
            client_name: token
                .as_ref()
                .map(|t| t.client_name.clone())
                .unwrap_or_default(),
            workspace_id: token
                .as_ref()
                .map(|t| t.workspace_id.clone())
                .unwrap_or_default(),
            tool: "oauth.consent".into(),
            requires_approval: true,
            paths: vec![],
            outcome: if token.is_some() {
                "succeeded"
            } else {
                "denied"
            }
            .into(),
            detail: None,
            at_ms: now_ms(),
        });
    Ok(Json(serde_json::json!({"ok":true})))
}

#[cfg(test)]
mod tests;
