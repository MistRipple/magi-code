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
use magi_mcp_server::{AttributionMode, IssueTokenRequest, Profile, TokenPatch, TokenRecord};
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
        .route("/mcp-server/approvals", get(list_approvals))
        .route("/mcp-server/approvals/resolve", post(resolve_approval))
        .route("/mcp-server/audit", get(list_audit).delete(clear_audit))
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

/// 令牌的对外形态：不含哈希。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenDto {
    token_id: String,
    prefix: String,
    client_name: String,
    workspace_id: String,
    profile: Profile,
    attribution: AttributionMode,
    /// 是否允许经公网隧道使用。
    network: bool,
    created_at_ms: u64,
    expires_at_ms: Option<u64>,
    revoked_at_ms: Option<u64>,
    last_used_at_ms: Option<u64>,
    active: bool,
    /// 是否可以重新查看原文（没有保存原文的令牌只能重新生成）。
    has_secret: bool,
}

impl TokenDto {
    fn from_record(state: &ApiState, record: &TokenRecord, now_ms: u64) -> Self {
        Self {
            token_id: record.token_id.clone(),
            prefix: record.prefix.clone(),
            client_name: record.client_name.clone(),
            workspace_id: record.workspace_id.clone(),
            profile: record.profile,
            attribution: record.attribution,
            network: record.network,
            created_at_ms: record.created_at_ms,
            expires_at_ms: record.expires_at_ms,
            revoked_at_ms: record.revoked_at_ms,
            last_used_at_ms: record.last_used_at_ms,
            active: record.is_active(now_ms),
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
    /// 客户端实际使用的公网域名 / IP。
    #[serde(default)]
    public_hosts: Vec<String>,
    /// 固定监听端口（防火墙需放行）；缺省沿用当前端口。
    #[serde(default)]
    port: Option<u16>,
    /// 开启必须显式确认：直接对公网开放用的是明文 HTTP，令牌在网络上不加密。
    #[serde(default)]
    confirm_risk: bool,
}

/// 直接对公网开放：不经隧道，HTTP 入口改为监听公网地址。
async fn set_direct_access(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<SetDirectAccessRequest>,
) -> Result<Json<StatusResponse>, ApiError> {
    require_local(&headers)?;
    if request.enabled && !request.confirm_risk {
        return Err(ApiError::InvalidInput(
            "直接对公网开放用的是明文 HTTP，令牌在网络上不加密，需要显式确认风险".to_string(),
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
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| ApiError::internal_assembly("连通性检查", error))?;
    match client.get(format!("https://{hostname}/mcp")).send().await {
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
    workspace_id: String,
    profile: Profile,
    /// 有效期天数。缺省 90；`0` 表示不过期。
    #[serde(default)]
    ttl_days: Option<u32>,
    /// 免确认写入（`edit_trusted`）与命令执行（`exec`）必须显式确认风险。
    #[serde(default)]
    confirm_high_risk: bool,
    /// 允许这个令牌经公网隧道使用（网络模式）。默认只能在本机使用；
    /// 网络令牌的权限档不得高于 `edit`。
    #[serde(default)]
    allow_network: bool,
}

/// 创建与编辑共用的权限校验：高风险权限档要显式确认，网络令牌不得高于 `edit`。
fn validate_scope(
    profile: Profile,
    network: bool,
    confirm_high_risk: bool,
) -> Result<(), ApiError> {
    let high_risk = matches!(profile, Profile::EditTrusted | Profile::Exec);
    if high_risk && !confirm_high_risk {
        return Err(ApiError::InvalidInput(
            "免确认写入与命令执行权限需要显式确认风险".to_string(),
        ));
    }
    if network && high_risk {
        return Err(ApiError::InvalidInput(
            "允许网络访问的令牌权限档不能高于“编辑”（写入前需要确认）".to_string(),
        ));
    }
    Ok(())
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
    if state
        .workspace_root_path(&Some(WorkspaceId::new(request.workspace_id.trim())))
        .is_none()
    {
        return Err(ApiError::InvalidInput("工作区不存在".to_string()));
    }
    validate_scope(
        request.profile,
        request.allow_network,
        request.confirm_high_risk,
    )?;
    let ttl_ms = ttl_days_to_ms(request.ttl_days.unwrap_or(DEFAULT_TOKEN_TTL_DAYS));
    let issued = state
        .mcp_service
        .issue_token(IssueTokenRequest {
            client_name: request.client_name,
            workspace_id: request.workspace_id,
            profile: request.profile,
            // 令牌只归到自己的外部工具会话；follow_web_slot 由 GPT Web 槽位内部签发。
            attribution: AttributionMode::External,
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
    #[serde(default)]
    profile: Option<Profile>,
    /// 从现在起重新计算的有效期天数；`0` 表示不过期。缺省不改。
    #[serde(default)]
    ttl_days: Option<u32>,
    #[serde(default)]
    allow_network: Option<bool>,
    /// 把权限档改成免确认写入或命令执行时必须显式确认风险。
    #[serde(default)]
    confirm_high_risk: bool,
}

/// 编辑令牌的名称、权限档、有效期与网络开关。工作区和归属方式不可改，原文不变，下一次调用起生效。
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
    if current.attribution != AttributionMode::External {
        return Err(ApiError::InvalidInput(
            "GPT Web 内部令牌由系统管理，不能编辑".to_string(),
        ));
    }
    let profile = request.profile.unwrap_or(current.profile);
    let network = request.allow_network.unwrap_or(current.network);
    // 只在权限档真的被改成高风险档时才要求确认；没改权限档的编辑不重复确认。
    let confirmed = request.confirm_high_risk || profile == current.profile;
    validate_scope(profile, network, confirmed)?;
    let updated = state
        .mcp_service
        .update_token(
            &state,
            &token_id,
            TokenPatch {
                client_name: request.client_name,
                profile: request.profile,
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
    if before.profile != after.profile {
        changes.push(format!(
            "profile: {:?} → {:?}",
            before.profile, after.profile
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
    let current = find_token(&state, &token_id)?;
    if current.attribution != AttributionMode::External {
        return Err(ApiError::InvalidInput(
            "GPT Web 内部令牌由系统管理，不能重新生成".to_string(),
        ));
    }
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
    /// 配置里的服务名（带上客户端名，避免多套配置互相覆盖）。
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

/// 配置里的服务名：`magi-` + 客户端名里的字母数字（中文等保留），其余折叠成 `-`。
fn server_name_for(client_name: &str) -> String {
    let mut slug = String::new();
    for ch in client_name.trim().chars() {
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
            (server_name_for(&record.client_name), secret)
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
    approval_id: String,
    token_id: String,
    client_name: String,
    token_prefix: String,
    workspace_id: String,
    tool_name: String,
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
            // 槽位拥有者的会话里也会有它自己的普通审批，只取属于该调用方的外部审批。
            if pending.task_id != task_id {
                continue;
            }
            approvals.push(ExternalApprovalDto {
                approval_id: pending.approval_id,
                token_id: owner.token_id.clone(),
                client_name: owner.client_name.clone(),
                token_prefix: owner.token_prefix.clone(),
                workspace_id: owner.workspace_id.clone(),
                tool_name: pending.tool_name,
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
    decision: ToolApprovalDecision,
}

async fn resolve_approval(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<ResolveApprovalRequest>,
) -> Result<Json<ApprovalsResponse>, ApiError> {
    require_local(&headers)?;
    // 外部审批只支持“允许一次 / 拒绝”。
    if request.decision == ToolApprovalDecision::AllowForTurn {
        return Err(ApiError::InvalidInput(
            "外部调用的授权只支持允许一次或拒绝".to_string(),
        ));
    }
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
    state
        .conversation_registry
        .tool_approvals()
        .resolve(&owner, &request.approval_id, request.decision)
        .map_err(ApiError::Conflict)?;
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
    Ok(Json(ClearAuditResponse { removed }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use magi_core::AbsolutePath;
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use tower::ServiceExt;

    fn state_with_workspace() -> (ApiState, tempfile::TempDir) {
        let state = ApiState::new(
            "magi-test",
            std::sync::Arc::new(InMemoryEventBus::new(16)),
            std::sync::Arc::new(SessionStore::default()),
            std::sync::Arc::new(WorkspaceStore::default()),
            std::sync::Arc::new(GovernanceService::default()),
        );
        let dir = tempfile::tempdir().unwrap();
        state
            .workspace_registry
            .register(
                WorkspaceId::new("ws-1"),
                AbsolutePath::new(dir.path().display().to_string()),
            )
            .unwrap();
        (state, dir)
    }

    async fn call(
        state: &ApiState,
        method: &str,
        path: &str,
        body: serde_json::Value,
        tunnel: bool,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(format!("/api{path}"))
            .header("content-type", "application/json");
        if tunnel {
            builder = builder.header("cf-ray", "abc");
        }
        let response = super::super::build_router(state.clone())
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    #[tokio::test]
    async fn token_lifecycle_never_leaks_hash_and_shows_secret_once() {
        let (state, _dir) = state_with_workspace();
        let (status, created) = call(
            &state,
            "POST",
            "/mcp-server/tokens",
            serde_json::json!({"clientName": "cursor", "workspaceId": "ws-1", "profile": "edit"}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let secret = created["secret"].as_str().unwrap().to_string();
        assert!(secret.starts_with("magi_mcp_"));
        assert_eq!(created["token"]["active"], true);
        assert!(
            created["token"]["expiresAtMs"].is_number(),
            "默认 90 天有效期"
        );

        let (_, listed) = call(
            &state,
            "GET",
            "/mcp-server/status",
            serde_json::json!({}),
            false,
        )
        .await;
        let text = listed.to_string();
        assert!(!text.contains(&secret), "状态不得回显令牌原文");
        assert!(!text.to_lowercase().contains("hash"), "状态不得包含哈希");
        assert_eq!(listed["tokens"].as_array().unwrap().len(), 1);

        let token_id = created["token"]["tokenId"].as_str().unwrap();
        let (status, after) = call(
            &state,
            "DELETE",
            &format!("/mcp-server/tokens/{token_id}"),
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(after["tokens"][0]["active"], false);

        let (status, _) = call(
            &state,
            "DELETE",
            "/mcp-server/tokens/nope",
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn high_risk_profiles_and_unknown_workspaces_are_rejected() {
        let (state, _dir) = state_with_workspace();
        for profile in ["edit_trusted", "exec"] {
            let (status, body) = call(
                &state,
                "POST",
                "/mcp-server/tokens",
                serde_json::json!({"clientName": "x", "workspaceId": "ws-1", "profile": profile}),
                false,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{profile}: {body}");
        }
        let (status, _) = call(
            &state,
            "POST",
            "/mcp-server/tokens",
            serde_json::json!({"clientName": "x", "workspaceId": "ws-1", "profile": "edit_trusted", "confirmHighRisk": true}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(
            &state,
            "POST",
            "/mcp-server/tokens",
            serde_json::json!({"clientName": "x", "workspaceId": "missing", "profile": "edit"}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    async fn create(state: &ApiState, body: serde_json::Value) -> (String, String) {
        let (status, created) = call(state, "POST", "/mcp-server/tokens", body, false).await;
        assert_eq!(status, StatusCode::OK, "{created}");
        (
            created["token"]["tokenId"].as_str().unwrap().to_string(),
            created["secret"].as_str().unwrap().to_string(),
        )
    }

    #[tokio::test]
    async fn a_token_secret_can_be_viewed_again_until_the_token_is_revoked() {
        let (state, _dir) = state_with_workspace();
        let (token_id, secret) = create(
            &state,
            serde_json::json!({"clientName": "cursor", "workspaceId": "ws-1", "profile": "exec", "confirmHighRisk": true}),
        )
        .await;
        let path = format!("/mcp-server/tokens/{token_id}/secret");
        let (status, viewed) = call(&state, "GET", &path, serde_json::json!({}), false).await;
        assert_eq!(status, StatusCode::OK, "{viewed}");
        assert_eq!(viewed["secret"], secret, "任何权限档都可以重新查看");
        let (_, listed) = call(
            &state,
            "GET",
            "/mcp-server/status",
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(listed["tokens"][0]["hasSecret"], true);
        assert!(!listed.to_string().contains(&secret), "列表仍不回显原文");
        assert!(
            state
                .mcp_service
                .audit()
                .recent(10, Some(&token_id))
                .iter()
                .any(|entry| entry.tool == "token.view_secret"),
            "每次查看原文都进审计"
        );

        let (status, _) = call(
            &state,
            "DELETE",
            &format!("/mcp-server/tokens/{token_id}"),
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(&state, "GET", &path, serde_json::json!({}), false).await;
        assert_eq!(status, StatusCode::CONFLICT, "吊销后原文被清除");
    }

    #[tokio::test]
    async fn editing_a_token_keeps_the_secret_and_enforces_the_same_risk_rules_as_creation() {
        let (state, _dir) = state_with_workspace();
        let (token_id, secret) = create(
            &state,
            serde_json::json!({"clientName": "cursor", "workspaceId": "ws-1", "profile": "edit"}),
        )
        .await;
        let path = format!("/mcp-server/tokens/{token_id}");
        // 改名 + 有效期不动权限档：不需要风险确认。
        let (status, body) = call(
            &state,
            "PATCH",
            &path,
            serde_json::json!({"clientName": "Cursor 主机", "ttlDays": 0}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["tokens"][0]["clientName"], "Cursor 主机");
        assert!(
            body["tokens"][0]["expiresAtMs"].is_null(),
            "ttlDays=0 不过期"
        );
        let audited = state.mcp_service.audit().recent(10, Some(&token_id));
        let update = audited
            .iter()
            .find(|entry| entry.tool == "token.update")
            .unwrap();
        let detail = update.detail.clone().unwrap();
        assert!(detail.contains("name: cursor → Cursor 主机"), "{detail}");
        assert!(!detail.contains(&secret), "审计不含原文");
        assert_eq!(
            state.mcp_service.token_secret(&token_id).as_deref(),
            Some(secret.as_str())
        );

        // 升到高风险权限档需要确认。
        let (status, _) = call(
            &state,
            "PATCH",
            &path,
            serde_json::json!({"profile": "exec"}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, body) = call(
            &state,
            "PATCH",
            &path,
            serde_json::json!({"profile": "exec", "confirmHighRisk": true}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["tokens"][0]["profile"], "exec");
        // 高风险权限档不能同时开放网络；反过来开放网络的令牌也不能升到高风险档。
        let (status, _) = call(
            &state,
            "PATCH",
            &path,
            serde_json::json!({"allowNetwork": true}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(
            &state,
            "PATCH",
            &path,
            serde_json::json!({"profile": "edit", "allowNetwork": true}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(
            &state,
            "PATCH",
            &path,
            serde_json::json!({"profile": "exec", "confirmHighRisk": true}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // 工作区不可改（字段被拒绝），未知令牌 404。
        let (status, _) = call(
            &state,
            "PATCH",
            &path,
            serde_json::json!({"workspaceId": "ws-2"}),
            false,
        )
        .await;
        assert!(status.is_client_error());
        let (status, _) = call(
            &state,
            "PATCH",
            "/mcp-server/tokens/nope",
            serde_json::json!({"clientName": "x"}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn rotating_replaces_the_secret_and_the_old_one_stops_authenticating() {
        let (state, _dir) = state_with_workspace();
        let (token_id, old_secret) = create(
            &state,
            serde_json::json!({"clientName": "cursor", "workspaceId": "ws-1", "profile": "edit"}),
        )
        .await;
        let (status, rotated) = call(
            &state,
            "POST",
            &format!("/mcp-server/tokens/{token_id}/rotate"),
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{rotated}");
        let new_secret = rotated["secret"].as_str().unwrap().to_string();
        assert_ne!(new_secret, old_secret);
        assert_eq!(state.mcp_service.token_secret(&token_id), Some(new_secret));
    }

    #[tokio::test]
    async fn snippets_are_per_client_with_the_real_secret_and_a_working_stdio_command() {
        let (state, _dir) = state_with_workspace();
        let (token_id, secret) = create(
            &state,
            serde_json::json!({"clientName": "Cursor Work", "workspaceId": "ws-1", "profile": "edit"}),
        )
        .await;
        let (status, generic) = call(
            &state,
            "GET",
            "/mcp-server/config-snippets",
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(generic["serverName"], "magi");
        assert_eq!(generic["secretFilled"], false);
        assert!(!generic.to_string().contains(&secret));

        let (status, snippets) = call(
            &state,
            "GET",
            &format!("/mcp-server/config-snippets?tokenId={token_id}"),
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(snippets["serverName"], "magi-cursor-work");
        assert_eq!(snippets["secretFilled"], true);
        // 服务没启动时没有地址，片段正文为空；正文由纯函数构造，下面直接验证。
        let built = build_snippets(
            "magi-cursor-work".to_string(),
            &secret,
            true,
            Some("http://127.0.0.1:1/mcp".to_string()),
            Some("/tmp/magi.sock"),
            Some("https://mcp.example.com/mcp"),
        );
        let json = serde_json::to_string(&built).unwrap();
        assert!(json.contains(&format!("Bearer {secret}")));
        assert_eq!(
            built.stdio_json.as_ref().unwrap()["mcpServers"]["magi-cursor-work"]["args"][0],
            "mcp-relay"
        );
        assert!(
            built
                .claude_cli
                .unwrap()
                .starts_with("claude mcp add --transport http magi-cursor-work ")
        );
        let remote = built.remote_claude_cli.as_deref().unwrap();
        assert!(remote.contains("https://mcp.example.com/mcp"), "{remote}");
        assert!(remote.contains(&format!("Bearer {secret}")));
        assert_eq!(
            built.remote_json.as_ref().unwrap()["mcpServers"]["magi-cursor-work"]["url"],
            "https://mcp.example.com/mcp"
        );
        assert_eq!(server_name_for("  ///  "), "magi");
        assert_eq!(server_name_for("我的 Claude"), "magi-我的-claude");
        let _ = relay_command();
    }

    #[tokio::test]
    async fn named_tunnel_routes_validate_save_and_never_echo_the_token() {
        use base64::Engine as _;
        let token = base64::engine::general_purpose::STANDARD
            .encode(r#"{"a":"acct","t":"11111111-2222-3333-4444-555555555555","s":"c2VjcmV0"}"#);
        let (state, _dir) = state_with_workspace();
        let (status, _) = call(
            &state,
            "PUT",
            "/mcp-server/named-tunnel",
            serde_json::json!({"hostname": "mcp.example.com", "token": "oops"}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, saved) = call(
            &state,
            "PUT",
            "/mcp-server/named-tunnel",
            serde_json::json!({"hostname": "mcp.example.com", "token": format!("cloudflared service install {token}")}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(saved["network"]["mode"], "named");
        assert_eq!(saved["network"]["namedHostname"], "mcp.example.com");
        assert!(!saved.to_string().contains(&token), "接口不得回显隧道令牌");

        // 网络模式没开时，连通性检查不发请求，直接说明原因。
        let (status, verify) = call(
            &state,
            "POST",
            "/mcp-server/named-tunnel/verify",
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(verify["code"], "network_off");

        let (status, cleared) = call(
            &state,
            "DELETE",
            "/mcp-server/named-tunnel",
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cleared["network"]["mode"], "quick");
        let (_, verify) = call(
            &state,
            "POST",
            "/mcp-server/named-tunnel/verify",
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(verify["code"], "not_configured");
    }

    #[tokio::test]
    async fn audit_is_paged_and_can_be_cleared_per_client_or_entirely() {
        let (state, _dir) = state_with_workspace();
        let (a, _) = create(
            &state,
            serde_json::json!({"clientName": "a", "workspaceId": "ws-1", "profile": "edit"}),
        )
        .await;
        let (b, _) = create(
            &state,
            serde_json::json!({"clientName": "b", "workspaceId": "ws-1", "profile": "edit"}),
        )
        .await;
        for (token, count) in [(&a, 5), (&b, 3)] {
            for _ in 0..count {
                let record = find_token(&state, token).unwrap();
                audit_management(&state, &record, "token.view_secret", None);
            }
        }
        let (status, page) = call(
            &state,
            "GET",
            "/mcp-server/audit?limit=3&offset=0",
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["total"], 8);
        assert_eq!(page["entries"].as_array().unwrap().len(), 3);
        let (_, last) = call(
            &state,
            "GET",
            "/mcp-server/audit?limit=3&offset=6",
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(last["entries"].as_array().unwrap().len(), 2);
        let (_, only_b) = call(
            &state,
            "GET",
            &format!("/mcp-server/audit?tokenId={b}"),
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(only_b["total"], 3);

        let (status, cleared) = call(
            &state,
            "DELETE",
            &format!("/mcp-server/audit?tokenId={b}"),
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cleared["removed"], 3);
        let (_, rest) = call(
            &state,
            "GET",
            "/mcp-server/audit",
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(rest["total"], 5);
        let (_, all) = call(
            &state,
            "DELETE",
            "/mcp-server/audit",
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(all["removed"], 5);
        let (_, empty) = call(
            &state,
            "GET",
            "/mcp-server/audit",
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(empty["total"], 0);

        // 公网来源不能读取或清理。
        for method in ["GET", "DELETE"] {
            let (status, _) = call(
                &state,
                method,
                "/mcp-server/audit",
                serde_json::json!({}),
                true,
            )
            .await;
            assert!(
                status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN,
                "{method}"
            );
        }
    }

    #[tokio::test]
    async fn direct_access_requires_risk_confirmation_a_network_token_and_valid_input() {
        let (state, _dir) = state_with_workspace();
        let enable = |extra: serde_json::Value| {
            let mut body = serde_json::json!({
                "enabled": true,
                "bindHost": "127.0.0.1",
                "publicHosts": ["203.0.113.5"],
            });
            body.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            body
        };
        // 没确认风险。
        let (status, _) = call(
            &state,
            "PUT",
            "/mcp-server/direct",
            enable(serde_json::json!({})),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // 没有允许网络使用的令牌。
        let confirmed = serde_json::json!({"confirmRisk": true});
        let (status, body) = call(
            &state,
            "PUT",
            "/mcp-server/direct",
            enable(confirmed.clone()),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        create(
            &state,
            serde_json::json!({"clientName": "remote", "workspaceId": "ws-1", "profile": "edit", "allowNetwork": true}),
        )
        .await;
        for bad in [
            serde_json::json!({"confirmRisk": true, "publicHosts": []}),
            serde_json::json!({"confirmRisk": true, "publicHosts": ["0.0.0.0"]}),
            serde_json::json!({"confirmRisk": true, "bindHost": "example.com"}),
            serde_json::json!({"confirmRisk": true, "port": 0}),
        ] {
            let (status, body) = call(
                &state,
                "PUT",
                "/mcp-server/direct",
                enable(bad.clone()),
                false,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        }
        let (status, ok) = call(
            &state,
            "PUT",
            "/mcp-server/direct",
            enable(confirmed),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{ok}");
        assert_eq!(ok["direct"]["listening"], true);
        assert_eq!(ok["direct"]["publicHosts"][0], "203.0.113.5");

        // 公网来源不能管理；关闭不需要确认。
        let (status, _) = call(
            &state,
            "PUT",
            "/mcp-server/direct",
            serde_json::json!({"enabled": false}),
            true,
        )
        .await;
        assert!(status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN);
        let (status, off) = call(
            &state,
            "PUT",
            "/mcp-server/direct",
            serde_json::json!({"enabled": false}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(off["direct"]["enabled"], false);
    }

    #[test]
    fn require_local_rejects_public_tunnel_requests_itself() {
        let mut headers = HeaderMap::new();
        assert!(require_local(&headers).is_ok());
        headers.insert("cf-ray", "abc".parse().unwrap());
        assert!(matches!(
            require_local(&headers),
            Err(ApiError::Forbidden(_))
        ));
    }

    #[tokio::test]
    async fn management_is_forbidden_from_public_tunnel_requests() {
        let (state, _dir) = state_with_workspace();
        for (method, path) in [
            ("GET", "/mcp-server/status"),
            ("POST", "/mcp-server/enabled"),
            ("POST", "/mcp-server/tokens"),
            ("POST", "/mcp-server/tokens/revoke-all"),
            ("GET", "/mcp-server/config-snippets"),
        ] {
            let (status, _) = call(
                &state,
                method,
                path,
                serde_json::json!({"enabled": true, "clientName": "x", "workspaceId": "ws-1", "profile": "edit"}),
                true,
            )
            .await;
            // 公网隧道请求先被隧道鉴权拒绝（401）；即便绕过它，管理路由本身也会返回 403。
            assert!(
                status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN,
                "{method} {path} -> {status}"
            );
        }
    }

    #[tokio::test]
    async fn external_approvals_are_listed_across_sessions_and_resolvable_once() {
        let (state, _dir) = state_with_workspace();
        let (_, created) = call(
            &state,
            "POST",
            "/mcp-server/tokens",
            serde_json::json!({"clientName": "cursor", "workspaceId": "ws-1", "profile": "edit"}),
            false,
        )
        .await;
        let token_id = created["token"]["tokenId"].as_str().unwrap().to_string();
        let session_id = crate::mcp_service::external_session_id(&token_id);
        let waiter = match state
            .conversation_registry
            .tool_approvals()
            .request_with_arguments(
                magi_conversation_runtime::PendingToolApproval {
                    approval_id: "approval-1".to_string(),
                    session_id: session_id.clone(),
                    task_id:
                        magi_conversation_runtime::external_approval::external_approval_task_id(
                            &token_id,
                        ),
                    turn_id: "external:t:c".to_string(),
                    tool_call_id: "c".to_string(),
                    tool_name: "file_write".to_string(),
                    reason: "magi.fs.write: a.txt".to_string(),
                    requested_at: magi_core::UtcMillis::now(),
                    agent: None,
                },
                "{}",
            )
            .unwrap()
        {
            magi_conversation_runtime::ToolApprovalRequestOutcome::Pending(waiter) => waiter,
            _ => panic!("应进入待审批"),
        };

        let (status, listed) = call(
            &state,
            "GET",
            "/mcp-server/approvals",
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed["approvals"][0]["approvalId"], "approval-1");
        assert_eq!(listed["approvals"][0]["clientName"], "cursor");
        assert_eq!(listed["approvals"][0]["summary"], "magi.fs.write: a.txt");

        let (status, _) = call(
            &state,
            "POST",
            "/mcp-server/approvals/resolve",
            serde_json::json!({"approvalId": "approval-1", "decision": "allow_for_turn"}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "外部审批不支持“本轮允许”");

        let (status, after) = call(
            &state,
            "POST",
            "/mcp-server/approvals/resolve",
            serde_json::json!({"approvalId": "approval-1", "decision": "allow_once"}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{after}");
        assert!(after["approvals"].as_array().unwrap().is_empty());
        assert_eq!(
            waiter.decision_rx.recv().unwrap(),
            magi_conversation_runtime::ToolApprovalDecision::AllowOnce
        );

        let (status, _) = call(
            &state,
            "POST",
            "/mcp-server/approvals/resolve",
            serde_json::json!({"approvalId": "approval-1", "decision": "deny"}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "已处理的审批不能再次处理");
    }

    #[tokio::test]
    async fn audit_lists_recent_calls_newest_first_and_filters_by_token() {
        let (state, _dir) = state_with_workspace();
        let entry = |token: &str, at_ms: u64| crate::mcp_runtime::AuditEntry {
            token_id: token.to_string(),
            client_name: "cursor".to_string(),
            workspace_id: "ws-1".to_string(),
            tool: "magi.fs.read".to_string(),
            requires_approval: false,
            paths: vec!["a.txt".to_string()],
            outcome: "succeeded".to_string(),
            detail: None,
            at_ms,
        };
        state.mcp_service.audit().record(entry("t1", 1));
        state.mcp_service.audit().record(entry("t2", 2));
        state.mcp_service.audit().record(entry("t1", 3));

        let (_, all) = call(
            &state,
            "GET",
            "/mcp-server/audit",
            serde_json::json!({}),
            false,
        )
        .await;
        let times = all["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["atMs"].as_u64().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(times, vec![3, 2, 1]);
        let (_, only) = call(
            &state,
            "GET",
            "/mcp-server/audit?tokenId=t1&limit=1",
            serde_json::json!({}),
            false,
        )
        .await;
        assert_eq!(only["entries"].as_array().unwrap().len(), 1);
        assert_eq!(only["entries"][0]["atMs"], 3);
    }

    #[tokio::test]
    async fn network_tokens_are_capped_at_edit_and_network_mode_needs_risk_confirmation() {
        let (state, _dir) = state_with_workspace();
        for profile in ["edit_trusted", "exec"] {
            let (status, body) = call(
                &state,
                "POST",
                "/mcp-server/tokens",
                serde_json::json!({"clientName": "x", "workspaceId": "ws-1", "profile": profile,
                    "confirmHighRisk": true, "allowNetwork": true}),
                false,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{profile}: {body}");
        }
        // 没有允许网络访问的令牌时不能开启；开启还需要显式确认风险。
        let (status, _) = call(
            &state,
            "POST",
            "/mcp-server/network",
            serde_json::json!({"enabled": true}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "未确认风险");
        let (status, body) = call(
            &state,
            "POST",
            "/mcp-server/network",
            serde_json::json!({"enabled": true, "confirmRisk": true}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "没有网络令牌: {body}");

        let (status, created) = call(
            &state,
            "POST",
            "/mcp-server/tokens",
            serde_json::json!({"clientName": "remote", "workspaceId": "ws-1", "profile": "edit", "allowNetwork": true}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        assert_eq!(created["token"]["network"], true);
        let (_, local) = call(
            &state,
            "POST",
            "/mcp-server/tokens",
            serde_json::json!({"clientName": "local", "workspaceId": "ws-1", "profile": "edit"}),
            false,
        )
        .await;
        assert_eq!(local["token"]["network"], false, "默认只能在本机使用");
    }
}
