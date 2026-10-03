//! Streamable HTTP 传输（回环与网络模式共用同一个处理器）。
//!
//! 检查顺序固定：**Host → Origin → 认证 → 请求体解析 → 调用管线**。认证之前不解析请求体；
//! 认证失败对外统一为 401，不区分原因。只提供 `POST /mcp`：不提供 SSE 与会话续传，
//! 服务没有需要服务端主动推送的内容。
//!
//! **对端地址决定“本机请求”**：监听公网地址（如 `0.0.0.0`）时，来自非回环地址的连接无论
//! `Host` 写什么都按网络请求处理——只认 `public_hosts` 里的主机名，只接受显式允许网络使用的令牌，
//! 受限流与失败退避约束，限流来源取真实对端地址（不信任客户端自带的 `X-Forwarded-For` 之类的头）。
//! 来自回环地址的请求（本机客户端、隧道或同机反向代理）保持原有规则。
//!
//! 这是**独立入口**：不挂到 Magi 主应用、不共享其鉴权中间件、不暴露任何 `/api/*` 路由（M10）。

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, RwLock};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::protocol::{JSONRPC_PARSE_ERROR, JsonRpcRequest, error};
use crate::rate_limit::RateLimiter;
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
    /// 公网隧道的主机名（网络模式运行期间由宿主写入，停止时清空）。
    ///
    /// 只有 `Host` 命中这里的请求才算“经隧道到达”：它们只接受显式允许网络使用的令牌，
    /// 并受限流与认证失败退避约束。回环请求不受这些限制。
    pub tunnel_hosts: Arc<RwLock<Vec<String>>>,
    /// 直接对公网开放（监听公网地址）时，客户端使用的主机名 / IP（不含端口）。
    /// 与 `tunnel_hosts` 一样，命中这里的请求按网络请求处理。
    pub public_hosts: Arc<RwLock<Vec<String>>>,
    pub limiter: Arc<RateLimiter>,
}

impl HttpState {
    pub fn new(server: Arc<McpServer>, tokens: Arc<TokenStore>, config: HttpConfig) -> Self {
        Self {
            server,
            tokens,
            config,
            clock: Arc::new(system_now_ms),
            tunnel_hosts: Arc::new(RwLock::new(Vec::new())),
            public_hosts: Arc::new(RwLock::new(Vec::new())),
            limiter: Arc::new(RateLimiter::new()),
        }
    }
}

/// 连接的对端 IP。只有用 `into_make_service_with_connect_info` 提供服务时才有；
/// 没有（例如测试里直接调用路由）时按“本机”处理。
pub struct Peer(pub Option<IpAddr>);

impl<S: Send + Sync> FromRequestParts<S> for Peer {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|info| info.0.ip()),
        ))
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

fn too_many_requests() -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({ "error": "too_many_requests" })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
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

fn host_in(list: &RwLock<Vec<String>>, host: Option<&str>) -> bool {
    host.is_some_and(|host| {
        list.read()
            .expect("host list poisoned")
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(host))
    })
}

/// 同机进程（隧道、反向代理）转交请求时带的真实来源；取不到就退化为统一的 `unknown`。
fn forwarded_source(headers: &HeaderMap) -> String {
    ["cf-connecting-ip", "x-forwarded-for"]
        .iter()
        .find_map(|name| {
            headers
                .get(*name)
                .and_then(|value| value.to_str().ok())
                .map(|value| value.split(',').next().unwrap_or_default().trim())
                .filter(|value| !value.is_empty() && value.len() <= 64)
        })
        .unwrap_or("unknown")
        .to_string()
}

async fn handle_post(
    State(state): State<HttpState>,
    Peer(peer): Peer,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // 1) Host：防 DNS 重绑定。缺失或不在白名单一律拒绝。
    //    对端不是回环地址（直接对公网开放）时，只认公网主机名，回环名称和隧道主机名都不算。
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(host_name);
    let remote_peer = peer.is_some_and(|ip| !ip.is_loopback());
    let is_loopback_host = !remote_peer
        && host.as_deref().is_some_and(|host| {
            state
                .config
                .allowed_hosts
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(host))
        });
    let via_network_host = host_in(&state.public_hosts, host.as_deref())
        || (!remote_peer && host_in(&state.tunnel_hosts, host.as_deref()));
    let via_tunnel = !is_loopback_host && via_network_host;
    if !is_loopback_host && !via_tunnel {
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

    // 3) 认证：统一 401，不区分令牌不存在 / 过期 / 已吊销 / 不允许网络使用。
    //    经隧道的请求先过退避检查，再认证；认证失败计入来源的失败次数。
    let now_ms = (state.clock)();
    let source = via_tunnel.then(|| match peer {
        // 直接对公网开放：来源就是真实对端地址，客户端自带的转发头不可信。
        Some(ip) if remote_peer => ip.to_string(),
        _ => forwarded_source(&headers),
    });
    if let Some(source) = &source
        && state.limiter.is_locked_out(source, now_ms)
    {
        return too_many_requests();
    }
    let record = match bearer_secret(&headers)
        .ok_or(())
        .and_then(|secret| state.tokens.authenticate(secret, now_ms).map_err(|_| ()))
        // 只在本机使用的令牌经隧道到达时，与“令牌不存在”不可区分。
        .and_then(|record| {
            if via_tunnel && !record.network {
                Err(())
            } else {
                Ok(record)
            }
        }) {
        Ok(record) => record,
        Err(()) => {
            if let Some(source) = &source {
                state.limiter.record_auth_failure(source, now_ms);
            }
            return unauthorized();
        }
    };
    if let Some(source) = &source {
        state.limiter.record_auth_success(source);
        if !state.limiter.allow_request(&record.token_id, now_ms) {
            return too_many_requests();
        }
    }

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
    use crate::rate_limit::MAX_AUTH_FAILURES as MAX_AUTH_FAILURES_FOR_TEST;
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
        fn path_requests(&self, _: &str, _: &Value, _: &std::path::Path) -> Vec<PathRequest> {
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
                    network: false,
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

    /// 直接对公网开放：本机令牌与网络令牌各一个，公网主机名由测试设置。
    struct DirectFixture {
        _dir: tempfile::TempDir,
        router: Router,
        local_secret: String,
        network_secret: String,
    }

    fn direct_fixture() -> DirectFixture {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Arc::new(TokenStore::new());
        let issue = |network: bool| {
            tokens
                .issue(
                    IssueTokenRequest {
                        client_name: "test".to_string(),
                        workspace_id: "ws".to_string(),
                        profile: Profile::Edit,
                        attribution: AttributionMode::External,
                        ttl_ms: None,
                        network,
                    },
                    0,
                )
                .unwrap()
                .secret
        };
        let (local_secret, network_secret) = (issue(false), issue(true));
        let server = Arc::new(McpServer::new(
            "magi-mcp",
            "test",
            Arc::new(Backend),
            Arc::new(Root(dir.path().to_path_buf())),
        ));
        let mut state = HttpState::new(server, tokens, HttpConfig::default());
        state.clock = Arc::new(|| 10);
        state
            .public_hosts
            .write()
            .unwrap()
            .push("203.0.113.5".to_string());
        DirectFixture {
            _dir: dir,
            router: router(state),
            local_secret,
            network_secret,
        }
    }

    fn from_peer(mut request: Request<Body>, peer: &str) -> Request<Body> {
        let addr: SocketAddr = format!("{peer}:50000").parse().unwrap();
        request.extensions_mut().insert(ConnectInfo(addr));
        request
    }

    #[tokio::test]
    async fn a_remote_peer_is_a_network_request_whatever_host_it_claims() {
        let f = direct_fixture();
        // 公网主机名 + 网络令牌：通过。
        let ok = f
            .router
            .clone()
            .oneshot(from_peer(
                post_request(Some(&f.network_secret), "203.0.113.5:8765", LIST),
                "198.51.100.9",
            ))
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);

        // 只在本机使用的令牌从公网来：与“令牌不存在”不可区分。
        let local_token = f
            .router
            .clone()
            .oneshot(from_peer(
                post_request(Some(&f.local_secret), "203.0.113.5", LIST),
                "198.51.100.9",
            ))
            .await
            .unwrap();
        assert_eq!(local_token.status(), StatusCode::UNAUTHORIZED);

        // 公网来的连接自称 Host: 127.0.0.1 / localhost，也不能当作本机请求。
        for claimed in ["127.0.0.1:8765", "localhost", "[::1]", "other.example.com"] {
            let response = f
                .router
                .clone()
                .oneshot(from_peer(
                    post_request(Some(&f.local_secret), claimed, LIST),
                    "198.51.100.9",
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{claimed}");
        }

        // 回环对端仍按本机规则：本机令牌 + 回环主机名可用。
        let local = f
            .router
            .oneshot(from_peer(
                post_request(Some(&f.local_secret), "127.0.0.1:8765", LIST),
                "127.0.0.1",
            ))
            .await
            .unwrap();
        assert_eq!(local.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn remote_auth_failures_are_throttled_per_real_peer_address_not_by_forwarded_headers() {
        let f = direct_fixture();
        let attempt = |peer: &str, forwarded: &str| {
            let mut request = post_request(Some("magi_mcp_wrong"), "203.0.113.5", LIST);
            request
                .headers_mut()
                .insert("x-forwarded-for", HeaderValue::from_str(forwarded).unwrap());
            from_peer(request, peer)
        };
        for index in 0..MAX_AUTH_FAILURES_FOR_TEST {
            // 客户端每次换一个自称的来源，也算在同一个真实地址上。
            let response = f
                .router
                .clone()
                .oneshot(attempt("198.51.100.9", &format!("10.0.0.{index}")))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let locked = f
            .router
            .clone()
            .oneshot(from_peer(
                post_request(Some(&f.network_secret), "203.0.113.5", LIST),
                "198.51.100.9",
            ))
            .await
            .unwrap();
        assert_eq!(locked.status(), StatusCode::TOO_MANY_REQUESTS);
        // 另一个真实地址不受影响。
        let other = f
            .router
            .oneshot(from_peer(
                post_request(Some(&f.network_secret), "203.0.113.5", LIST),
                "198.51.100.10",
            ))
            .await
            .unwrap();
        assert_eq!(other.status(), StatusCode::OK);
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

    // ── 网络模式：隧道主机、仅网络令牌、限流与退避 ─────────────────────────────

    struct TunnelFixture {
        router: Router,
        local_secret: String,
        network_secret: String,
        tunnel_hosts: Arc<RwLock<Vec<String>>>,
        _dir: tempfile::TempDir,
    }

    fn tunnel_fixture() -> TunnelFixture {
        let f = fixture(HttpConfig::default());
        let issued = f
            .tokens
            .issue(
                IssueTokenRequest {
                    client_name: "remote".to_string(),
                    workspace_id: "ws".to_string(),
                    profile: Profile::Edit,
                    attribution: AttributionMode::External,
                    ttl_ms: None,
                    network: true,
                },
                0,
            )
            .unwrap();
        let server = Arc::new(McpServer::new(
            "magi-mcp",
            "test",
            Arc::new(Backend),
            Arc::new(Root(f._dir.path().to_path_buf())),
        ));
        let mut state = HttpState::new(server, f.tokens.clone(), HttpConfig::default());
        state.clock = Arc::new(|| 10);
        let tunnel_hosts = state.tunnel_hosts.clone();
        tunnel_hosts
            .write()
            .unwrap()
            .push("quick-demo.trycloudflare.com".to_string());
        TunnelFixture {
            router: router(state),
            local_secret: f.secret.clone(),
            network_secret: issued.secret,
            tunnel_hosts,
            _dir: f._dir,
        }
    }

    fn tunnel_request(secret: Option<&str>, ip: &str) -> Request<Body> {
        let mut request = post_request(secret, "quick-demo.trycloudflare.com", LIST);
        request
            .headers_mut()
            .insert("cf-connecting-ip", HeaderValue::from_str(ip).unwrap());
        request
    }

    async fn status_of(f: &TunnelFixture, request: Request<Body>) -> StatusCode {
        f.router.clone().oneshot(request).await.unwrap().status()
    }

    #[tokio::test]
    async fn tunnel_requests_only_accept_network_tokens_and_unknown_hosts_are_refused() {
        let f = tunnel_fixture();
        assert_eq!(
            status_of(&f, tunnel_request(Some(&f.network_secret), "1.1.1.1")).await,
            StatusCode::OK
        );
        // 只在本机使用的令牌经隧道到达：与“令牌不存在”不可区分。
        assert_eq!(
            status_of(&f, tunnel_request(Some(&f.local_secret), "1.1.1.1")).await,
            StatusCode::UNAUTHORIZED
        );
        // 同一个本机令牌走回环仍然可用。
        assert_eq!(
            status_of(&f, post_request(Some(&f.local_secret), "127.0.0.1", LIST)).await,
            StatusCode::OK
        );
        // 不在名单里的主机名一律拒绝。
        let mut other = post_request(Some(&f.network_secret), "evil.example.com", LIST);
        other
            .headers_mut()
            .insert("cf-connecting-ip", HeaderValue::from_static("1.1.1.1"));
        assert_eq!(status_of(&f, other).await, StatusCode::FORBIDDEN);
        // 隧道停止（主机名清空）后，同样的请求立即被拒绝。
        f.tunnel_hosts.write().unwrap().clear();
        assert_eq!(
            status_of(&f, tunnel_request(Some(&f.network_secret), "1.1.1.1")).await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn repeated_bad_tokens_lock_the_source_out_even_for_a_valid_token() {
        let f = tunnel_fixture();
        for _ in 0..crate::rate_limit::MAX_AUTH_FAILURES {
            assert_eq!(
                status_of(&f, tunnel_request(Some("magi_mcp_wrong"), "9.9.9.9")).await,
                StatusCode::UNAUTHORIZED
            );
        }
        let locked = f
            .router
            .clone()
            .oneshot(tunnel_request(Some(&f.network_secret), "9.9.9.9"))
            .await
            .unwrap();
        assert_eq!(locked.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(locked.headers().contains_key(header::RETRY_AFTER));
        // 其它来源不受影响；回环也不受影响。
        assert_eq!(
            status_of(&f, tunnel_request(Some(&f.network_secret), "8.8.8.8")).await,
            StatusCode::OK
        );
        assert_eq!(
            status_of(&f, post_request(Some(&f.local_secret), "localhost", LIST)).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn per_token_rate_limit_applies_to_tunnel_requests_only() {
        let f = tunnel_fixture();
        for _ in 0..crate::rate_limit::MAX_REQUESTS_PER_WINDOW {
            assert_eq!(
                status_of(&f, tunnel_request(Some(&f.network_secret), "2.2.2.2")).await,
                StatusCode::OK
            );
        }
        assert_eq!(
            status_of(&f, tunnel_request(Some(&f.network_secret), "2.2.2.2")).await,
            StatusCode::TOO_MANY_REQUESTS
        );
    }
}
