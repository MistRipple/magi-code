//! Streamable HTTP 传输（回环与网络模式共用同一个处理器）。
//!
//! 检查顺序固定：**Host → Origin → 认证 → 请求体解析 → 调用管线**。认证之前不解析请求体；
//! 认证失败对外统一为 401，不区分原因。只提供 `POST /mcp`：不提供 SSE 与会话续传，
//! 服务没有需要服务端主动推送的内容。
//!
//! 这是**独立入口**：不挂到 Magi 主应用、不共享其鉴权中间件、不暴露任何 `/api/*` 路由（M10）。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::protocol::{JSONRPC_PARSE_ERROR, JsonRpcRequest, error};
use crate::server::{ClientPrincipal, MAX_ARGUMENT_BYTES, McpServer};
use crate::token::TokenStore;

/// 本服务接受的 `MCP-Protocol-Version` 请求头取值。
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26"];

/// 请求体上限：参数上限加上 JSON-RPC 外层的余量。
const MAX_HTTP_BODY_BYTES: usize = MAX_ARGUMENT_BYTES + 64 * 1024;

#[derive(Clone, Debug)]
pub struct HttpConfig {
    /// 允许的 `Host` 主机名（不含端口，大小写不敏感）。回环默认只有本机名称；
    /// 走隧道时必须显式加入隧道主机名，否则一律拒绝（防 DNS 重绑定）。
    pub allowed_hosts: Vec<String>,
    /// 允许的 `Origin`。默认空：带 `Origin` 的浏览器跨站请求一律拒绝；
    /// 非浏览器的 MCP 客户端不带该头，不受影响。
    pub allowed_origins: Vec<String>,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            allowed_hosts: vec![
                "127.0.0.1".to_string(),
                "localhost".to_string(),
                "[::1]".to_string(),
            ],
            allowed_origins: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct HttpState {
    pub server: Arc<McpServer>,
    pub tokens: Arc<TokenStore>,
    pub config: HttpConfig,
    pub clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl HttpState {
    pub fn new(server: Arc<McpServer>, tokens: Arc<TokenStore>, config: HttpConfig) -> Self {
        Self {
            server,
            tokens,
            config,
            clock: Arc::new(system_now_ms),
        }
    }
}

fn system_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub fn router(state: HttpState) -> Router {
    Router::new()
        .route("/mcp", post(handle_post).fallback(method_not_allowed))
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .with_state(state)
}

async fn method_not_allowed() -> Response {
    let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST"));
    response
}

fn host_name(header_value: &str) -> String {
    let trimmed = header_value.trim();
    // IPv6 字面量形如 `[::1]:8080`，主机名部分包含方括号。
    if trimmed.starts_with('[') {
        return match trimmed.find(']') {
            Some(end) => trimmed[..=end].to_ascii_lowercase(),
            None => trimmed.to_ascii_lowercase(),
        };
    }
    trimmed
        .rsplit_once(':')
        .map_or(trimmed, |(host, _port)| host)
        .to_ascii_lowercase()
}

fn unauthorized() -> Response {
    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized" })),
    )
        .into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"magi-mcp\""),
    );
    response
}

fn bearer_secret(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, secret) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then_some(secret.trim())
        .filter(|secret| !secret.is_empty())
}

async fn handle_post(State(state): State<HttpState>, headers: HeaderMap, body: Bytes) -> Response {
    // 1) Host：防 DNS 重绑定。缺失或不在白名单一律拒绝。
    let host_allowed = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(host_name)
        .is_some_and(|host| {
            state
                .config
                .allowed_hosts
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(&host))
        });
    if !host_allowed {
        return StatusCode::FORBIDDEN.into_response();
    }

    // 2) Origin：浏览器发起的跨站请求一律拒绝，除非显式放行。
    if let Some(origin) = headers.get(header::ORIGIN) {
        let allowed = origin.to_str().ok().is_some_and(|origin| {
            state
                .config
                .allowed_origins
                .iter()
                .any(|allowed| allowed == origin)
        });
        if !allowed {
            return StatusCode::FORBIDDEN.into_response();
        }
    }

    // 3) 认证：统一 401，不区分令牌不存在 / 过期 / 已吊销。
    let now_ms = (state.clock)();
    let Some(secret) = bearer_secret(&headers) else {
        return unauthorized();
    };
    let record = match state.tokens.authenticate(secret, now_ms) {
        Ok(record) => record,
        Err(_) => return unauthorized(),
    };

    // 4) 协议版本头（已认证之后才回显具体原因）。
    if let Some(version) = headers
        .get("mcp-protocol-version")
        .and_then(|value| value.to_str().ok())
        && !SUPPORTED_PROTOCOL_VERSIONS.contains(&version)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "unsupported MCP-Protocol-Version" })),
        )
            .into_response();
    }

    // 5) 请求体与调用管线。
    let request: JsonRpcRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(parse_error) => {
            return Json(error(
                Value::Null,
                JSONRPC_PARSE_ERROR,
                format!("请求体不是合法 JSON-RPC：{parse_error}"),
            ))
            .into_response();
        }
    };
    let principal = ClientPrincipal::from(&record);
    match state.server.handle(&principal, request, now_ms).await {
        Some(response) => Json(response).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{ToolSchema, ToolSchemaProvider};
    use crate::path_guard::PathRequest;
    use crate::profile::Profile;
    use crate::server::{
        BoxFuture, InvocationOutcome, ToolBackend, ToolInvocation, WorkspaceResolver,
    };
    use crate::token::{AttributionMode, IssueTokenRequest};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    struct Schemas;
    impl ToolSchemaProvider for Schemas {
        fn schema_for(&self, internal_name: &str) -> Option<ToolSchema> {
            Some(ToolSchema {
                description: internal_name.to_string(),
                input_schema: json!({ "type": "object" }),
            })
        }
    }

    struct Backend;
    impl ToolBackend for Backend {
        fn schemas(&self) -> &dyn ToolSchemaProvider {
            static SCHEMAS: Schemas = Schemas;
            &SCHEMAS
        }
        fn path_requests(&self, _: &str, _: &Value) -> Vec<PathRequest> {
            Vec::new()
        }
        fn invoke<'a>(&'a self, _: ToolInvocation) -> BoxFuture<'a, InvocationOutcome> {
            Box::pin(async {
                InvocationOutcome {
                    text: "done".to_string(),
                    is_error: false,
                }
            })
        }
    }

    struct Root(std::path::PathBuf);
    impl WorkspaceResolver for Root {
        fn root_of(&self, _: &str) -> Option<std::path::PathBuf> {
            Some(self.0.clone())
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        router: Router,
        secret: String,
        tokens: Arc<TokenStore>,
    }

    fn fixture(config: HttpConfig) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Arc::new(TokenStore::new());
        let issued = tokens
            .issue(
                IssueTokenRequest {
                    client_name: "test".to_string(),
                    workspace_id: "ws".to_string(),
                    profile: Profile::Edit,
                    attribution: AttributionMode::External,
                    ttl_ms: None,
                },
                0,
            )
            .unwrap();
        let server = Arc::new(McpServer::new(
            "magi-mcp",
            "test",
            Arc::new(Backend),
            Arc::new(Root(dir.path().to_path_buf())),
        ));
        let mut state = HttpState::new(server, tokens.clone(), config);
        state.clock = Arc::new(|| 10);
        Fixture {
            _dir: dir,
            router: router(state),
            secret: issued.secret,
            tokens,
        }
    }

    fn post_request(secret: Option<&str>, host: &str, body: &str) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(header::HOST, host)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(secret) = secret {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {secret}"));
        }
        builder.body(Body::from(body.to_string())).unwrap()
    }

    const LIST: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

    async fn json_of(response: Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn authenticated_loopback_requests_reach_the_server() {
        let f = fixture(HttpConfig::default());
        let response = f
            .router
            .oneshot(post_request(Some(&f.secret), "127.0.0.1:8123", LIST))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_of(response).await;
        assert!(body["result"]["tools"].as_array().unwrap().len() >= 3);
    }

    #[tokio::test]
    async fn missing_wrong_and_revoked_tokens_are_all_plain_401() {
        let f = fixture(HttpConfig::default());
        for secret in [None, Some("magi_mcp_wrong"), Some("garbage")] {
            let response = f
                .router
                .clone()
                .oneshot(post_request(secret, "localhost", LIST))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{secret:?}");
            assert!(response.headers().contains_key(header::WWW_AUTHENTICATE));
        }
        f.tokens.revoke_all(5);
        let response = f
            .router
            .oneshot(post_request(Some(&f.secret), "localhost", LIST))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn unknown_hosts_and_browser_origins_are_rejected_before_authentication() {
        let f = fixture(HttpConfig::default());
        // 重绑定后的主机名：即使令牌正确也拒绝。
        let response = f
            .router
            .clone()
            .oneshot(post_request(Some(&f.secret), "evil.example.com", LIST))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let mut with_origin = post_request(Some(&f.secret), "localhost", LIST);
        with_origin.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example.com"),
        );
        let response = f.router.oneshot(with_origin).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn tunnel_hostnames_and_origins_must_be_allowed_explicitly() {
        let f = fixture(HttpConfig {
            allowed_hosts: vec!["mcp.example.com".to_string()],
            allowed_origins: vec!["https://claude.ai".to_string()],
        });
        let ok = f
            .router
            .clone()
            .oneshot(post_request(Some(&f.secret), "MCP.example.com", LIST))
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        // 默认的回环主机名在这份配置里不再放行。
        let denied = f
            .router
            .clone()
            .oneshot(post_request(Some(&f.secret), "localhost", LIST))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let mut allowed_origin = post_request(Some(&f.secret), "mcp.example.com", LIST);
        allowed_origin.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_static("https://claude.ai"),
        );
        assert_eq!(
            f.router.oneshot(allowed_origin).await.unwrap().status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn notifications_get_202_and_bad_json_gets_a_parse_error() {
        let f = fixture(HttpConfig::default());
        let notification = f
            .router
            .clone()
            .oneshot(post_request(
                Some(&f.secret),
                "localhost",
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(notification.status(), StatusCode::ACCEPTED);

        let bad = f
            .router
            .oneshot(post_request(Some(&f.secret), "localhost", "{not json"))
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::OK);
        assert_eq!(json_of(bad).await["error"]["code"], JSONRPC_PARSE_ERROR);
    }

    #[tokio::test]
    async fn only_post_is_served_and_unsupported_protocol_versions_are_refused() {
        let f = fixture(HttpConfig::default());
        let get = Request::builder()
            .method("GET")
            .uri("/mcp")
            .header(header::HOST, "localhost")
            .body(Body::empty())
            .unwrap();
        let response = f.router.clone().oneshot(get).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[header::ALLOW], "POST");

        let mut old = post_request(Some(&f.secret), "localhost", LIST);
        old.headers_mut().insert(
            "mcp-protocol-version",
            HeaderValue::from_static("2020-01-01"),
        );
        assert_eq!(
            f.router.oneshot(old).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn host_names_are_parsed_without_ports_and_with_ipv6() {
        assert_eq!(host_name("LocalHost:8080"), "localhost");
        assert_eq!(host_name("127.0.0.1"), "127.0.0.1");
        assert_eq!(host_name("[::1]:9000"), "[::1]");
        assert_eq!(host_name("mcp.example.com:443"), "mcp.example.com");
    }
}
