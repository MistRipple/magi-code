//! Magi MCP 服务的管理路由（开关、令牌、配置片段）。
//!
//! 管理面只对可信本机入口开放：经公网隧道到达的请求一律拒绝，网络客户端只能使用 MCP 入口本身，
//! 不能创建或吊销令牌（设计 M8）。令牌原文只在创建响应里出现一次，列表与状态不含哈希。

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::{delete, get, post},
};
use magi_conversation_runtime::ToolApprovalDecision;
use magi_core::WorkspaceId;
use magi_mcp_server::{AttributionMode, IssueTokenRequest, Profile, TokenRecord};
use serde::{Deserialize, Serialize};

use super::is_public_tunnel_request;
use crate::mcp_runtime::{McpRuntimeError, McpServiceStatus};
use crate::{errors::ApiError, state::ApiState};

/// 令牌默认有效期（天）。创建时可改；`0` 表示不过期。
const DEFAULT_TOKEN_TTL_DAYS: u32 = 90;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/mcp-server/status", get(get_status))
        .route("/mcp-server/enabled", post(set_enabled))
        .route("/mcp-server/network", post(set_network))
        .route("/mcp-server/tokens", post(create_token))
        .route("/mcp-server/tokens/revoke-all", post(revoke_all))
        .route("/mcp-server/tokens/{token_id}", delete(revoke_token))
        .route("/mcp-server/config-snippets", get(config_snippets))
        .route("/mcp-server/approvals", get(list_approvals))
        .route("/mcp-server/approvals/resolve", post(resolve_approval))
        .route("/mcp-server/audit", get(list_audit))
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
}

impl TokenDto {
    fn from_record(record: &TokenRecord, now_ms: u64) -> Self {
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
            .map(|record| TokenDto::from_record(record, now))
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
    if matches!(request.profile, Profile::EditTrusted | Profile::Exec) && !request.confirm_high_risk
    {
        return Err(ApiError::InvalidInput(
            "免确认写入与命令执行权限需要显式确认风险".to_string(),
        ));
    }
    if request.allow_network && matches!(request.profile, Profile::EditTrusted | Profile::Exec) {
        return Err(ApiError::InvalidInput(
            "允许网络访问的令牌权限档不能高于“编辑”（写入前需要确认）".to_string(),
        ));
    }
    let ttl_ms = match request.ttl_days.unwrap_or(DEFAULT_TOKEN_TTL_DAYS) {
        0 => None,
        days => Some(u64::from(days) * DAY_MS),
    };
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
        token: TokenDto::from_record(&issued.record, now_ms()),
        secret: issued.secret.clone(),
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
    /// 令牌以占位符出现；已有令牌原文不会被回显。
    http_json: Option<serde_json::Value>,
    /// stdio 配置（Claude Desktop、Cursor 等本机客户端）。令牌通过环境变量提供。
    stdio_json: Option<serde_json::Value>,
    /// 公网地址的客户端配置（网络模式运行时）。地址每次开启都会变。
    remote_json: Option<serde_json::Value>,
}

async fn config_snippets(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<ConfigSnippets>, ApiError> {
    require_local(&headers)?;
    let status = state.mcp_service.status().await;
    let url = status.url;
    let http_json = url.as_ref().map(|url| {
        serde_json::json!({
            "mcpServers": {
                "magi": {
                    "url": url,
                    "headers": { "Authorization": "Bearer <MAGI_MCP_TOKEN>" }
                }
            }
        })
    });
    let stdio_json = status.stdio_endpoint.as_ref().map(|endpoint| {
        serde_json::json!({
            "mcpServers": {
                "magi": {
                    "command": "magi-mcp",
                    "args": ["--stdio", "--endpoint", endpoint],
                    "env": { "MAGI_MCP_TOKEN": "<MAGI_MCP_TOKEN>" }
                }
            }
        })
    });
    let remote_json = status.network.mcp_url.as_ref().map(|url| {
        serde_json::json!({
            "mcpServers": {
                "magi": {
                    "url": url,
                    "headers": { "Authorization": "Bearer <MAGI_MCP_TOKEN>" }
                }
            }
        })
    });
    Ok(Json(ConfigSnippets {
        url,
        http_json,
        stdio_json,
        remote_json,
    }))
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AuditQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    token_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuditResponse {
    entries: Vec<crate::mcp_runtime::AuditEntry>,
}

async fn list_audit(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<AuditQuery>,
) -> Result<Json<AuditResponse>, ApiError> {
    require_local(&headers)?;
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    Ok(Json(AuditResponse {
        entries: state
            .mcp_service
            .audit()
            .recent(limit, query.token_id.as_deref()),
    }))
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
            .request(magi_conversation_runtime::PendingToolApproval {
                approval_id: "approval-1".to_string(),
                session_id: session_id.clone(),
                task_id: magi_conversation_runtime::external_approval::external_approval_task_id(
                    &token_id,
                ),
                turn_id: "external:t:c".to_string(),
                tool_call_id: "c".to_string(),
                tool_name: "file_write".to_string(),
                reason: "magi.fs.write: a.txt".to_string(),
                requested_at: magi_core::UtcMillis::now(),
            })
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
