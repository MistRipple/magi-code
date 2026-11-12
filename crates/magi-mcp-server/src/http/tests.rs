use super::*;
use crate::catalog::{ToolSchema, ToolSchemaProvider};
use crate::path_guard::PathRequest;
use crate::profile::ApprovalMode;
use crate::rate_limit::MAX_AUTH_FAILURES as MAX_AUTH_FAILURES_FOR_TEST;
use crate::server::{BoxFuture, InvocationOutcome, ToolBackend, ToolInvocation, WorkspaceResolver};
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
    state: HttpState,
    clock: Arc<std::sync::atomic::AtomicU64>,
}

fn fixture(config: HttpConfig) -> Fixture {
    fixture_with_backend(config, Arc::new(Backend))
}

fn fixture_with_backend(config: HttpConfig, backend: Arc<dyn ToolBackend>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let tokens = Arc::new(TokenStore::new());
    let issued = tokens
        .issue(
            IssueTokenRequest {
                project_directories: None,
                client_name: "test".to_string(),
                workspace_id: "ws".to_string(),
                approval_mode: ApprovalMode::Ask,
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
        backend,
        Arc::new(Root(dir.path().to_path_buf())),
    ));
    let mut state = HttpState::new(server, tokens.clone(), config);
    let clock = Arc::new(std::sync::atomic::AtomicU64::new(10));
    state.clock = {
        let clock = clock.clone();
        Arc::new(move || clock.load(std::sync::atomic::Ordering::SeqCst))
    };
    Fixture {
        _dir: dir,
        router: router(state.clone()),
        secret: issued.secret,
        tokens,
        state,
        clock,
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

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":0,"method":"initialize"}"#;
const CALL: &str =
    r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"magi.fs.read"}}"#;
const CANCEL: &str =
    r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#;

fn session_request(secret: &str, session: Option<&str>, body: &str) -> Request<Body> {
    let mut request = post_request(Some(secret), "localhost", body);
    if let Some(id) = session {
        request
            .headers_mut()
            .insert(SESSION_HEADER, id.parse().unwrap());
    }
    request
}

async fn initialize(f: &Fixture) -> String {
    let response = f
        .router
        .clone()
        .oneshot(session_request(&f.secret, None, INITIALIZE))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let id = response.headers()[SESSION_HEADER]
        .to_str()
        .unwrap()
        .to_string();
    assert!(json_of(response).await["result"].is_object());
    id
}

struct CancellableBackend(tokio::sync::mpsc::UnboundedSender<crate::server::RequestCancellation>);
impl ToolBackend for CancellableBackend {
    fn schemas(&self) -> &dyn ToolSchemaProvider {
        static SCHEMAS: Schemas = Schemas;
        &SCHEMAS
    }
    fn path_requests(&self, _: &str, _: &Value, _: &std::path::Path) -> Vec<PathRequest> {
        Vec::new()
    }
    fn invoke<'a>(&'a self, invocation: ToolInvocation) -> BoxFuture<'a, InvocationOutcome> {
        Box::pin(async move {
            self.0.send(invocation.cancellation.clone()).unwrap();
            while !invocation.cancellation.is_cancelled() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            InvocationOutcome {
                text: "cancelled".into(),
                is_error: true,
            }
        })
    }
}

#[tokio::test]
async fn shared_token_http_sessions_isolate_duplicate_ids_cancellation_and_termination() {
    let (tx, mut calls) = tokio::sync::mpsc::unbounded_channel();
    let f = fixture_with_backend(HttpConfig::default(), Arc::new(CancellableBackend(tx)));
    let first = initialize(&f).await;
    let second = initialize(&f).await;
    assert_ne!(first, second);
    assert_eq!(f.state.session_count(), 2);
    let mut tasks = Vec::new();
    let mut signals = Vec::new();
    // 两个有会话客户端和两个无会话客户端都使用 id=1，均须进入后端。
    for session in [Some(first.as_str()), Some(second.as_str()), None, None] {
        let router = f.router.clone();
        let request = session_request(&f.secret, session, CALL);
        tasks.push(tokio::spawn(async move {
            router.oneshot(request).await.unwrap()
        }));
        signals.push(
            tokio::time::timeout(std::time::Duration::from_secs(2), calls.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    let duplicate = f
        .router
        .clone()
        .oneshot(session_request(&f.secret, Some(&first), CALL))
        .await
        .unwrap();
    assert_eq!(json_of(duplicate).await["result"]["isError"], true);
    // 未携带会话头的取消不能猜测/命中任一客户端的请求。
    f.router
        .clone()
        .oneshot(session_request(&f.secret, None, CANCEL))
        .await
        .unwrap();
    assert!(signals.iter().all(|signal| !signal.is_cancelled()));
    f.router
        .clone()
        .oneshot(session_request(&f.secret, Some(&first), CANCEL))
        .await
        .unwrap();
    assert!(signals[0].is_cancelled());
    assert!(signals[1..].iter().all(|signal| !signal.is_cancelled()));
    let mut terminate = session_request(&f.secret, Some(&second), "");
    *terminate.method_mut() = Method::DELETE;
    assert_eq!(
        f.router.clone().oneshot(terminate).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    assert!(signals[1].is_cancelled());
    assert!(!signals[2].is_cancelled() && !signals[3].is_cancelled());
    assert_eq!(
        f.router
            .clone()
            .oneshot(session_request(&f.secret, Some(&second), LIST))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    for task in tasks.drain(..2) {
        let response = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(json_of(response).await["result"]["isError"], true);
    }
    for task in tasks {
        task.abort();
    }
}

#[tokio::test]
async fn session_ids_are_bound_to_authenticated_tokens_and_missing_headers_remain_stateless() {
    let f = fixture(HttpConfig::default());
    let id = initialize(&f).await;
    let other = f
        .tokens
        .issue(
            IssueTokenRequest {
                project_directories: None,
                client_name: "other".into(),
                workspace_id: "ws".into(),
                approval_mode: ApprovalMode::Ask,
                attribution: AttributionMode::External,
                ttl_ms: None,
                network: false,
            },
            0,
        )
        .unwrap();
    for method in [Method::POST, Method::DELETE] {
        let mut request = session_request(&other.secret, Some(&id), LIST);
        *request.method_mut() = method;
        assert_eq!(
            f.router.clone().oneshot(request).await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        f.router
            .clone()
            .oneshot(session_request(&f.secret, Some(&id), LIST))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        f.router
            .clone()
            .oneshot(session_request(&f.secret, None, LIST))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let mut delete = session_request(&f.secret, None, "");
    *delete.method_mut() = Method::DELETE;
    assert_eq!(
        f.router.oneshot(delete).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn session_count_excludes_revoked_credentials() {
    let f = fixture(HttpConfig::default());
    initialize(&f).await;
    assert_eq!(f.state.session_count(), 1);
    let token = &f.tokens.records()[0].token_id;
    f.tokens.revoke(token, (f.state.clock)());
    assert_eq!(f.state.session_count(), 0);
}

#[tokio::test]
async fn idle_sessions_expire_but_running_calls_remain_cancellable() {
    let (tx, mut calls) = tokio::sync::mpsc::unbounded_channel();
    let f = fixture_with_backend(HttpConfig::default(), Arc::new(CancellableBackend(tx)));
    let idle = initialize(&f).await;
    let active = initialize(&f).await;
    assert_eq!(f.state.session_count(), 2);
    let router = f.router.clone();
    let request = session_request(&f.secret, Some(&active), CALL);
    let task = tokio::spawn(async move { router.oneshot(request).await.unwrap() });
    tokio::time::timeout(std::time::Duration::from_secs(2), calls.recv())
        .await
        .unwrap()
        .unwrap();
    f.clock
        .store(SESSION_IDLE_MS + 11, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        f.state.session_count(),
        1,
        "idle sessions are excluded even before another request"
    );
    assert_eq!(
        f.router
            .clone()
            .oneshot(session_request(&f.secret, Some(&idle), LIST))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    f.router
        .clone()
        .oneshot(session_request(&f.secret, Some(&active), CANCEL))
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    // 初始化洪泛受限，清理空闲会话后仍可建立新连接。
    {
        let mut sessions = f.state.sessions.lock().unwrap();
        for i in sessions.len()..MAX_HTTP_SESSIONS {
            sessions.insert(
                format!("test-{i}"),
                HttpSession {
                    token_id: f.tokens.records()[0].token_id.clone(),
                    scope: Arc::new(f.state.server.connection_scope()),
                    last_used_ms: SESSION_IDLE_MS + 11,
                },
            );
        }
    }
    assert_eq!(
        f.router
            .clone()
            .oneshot(session_request(&f.secret, None, INITIALIZE))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    f.clock.store(
        2 * SESSION_IDLE_MS + 12,
        std::sync::atomic::Ordering::SeqCst,
    );
    initialize(&f).await;
    assert_eq!(f.state.sessions.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn oauth_http_discovery_consent_exchange_and_resource_authentication() {
    let f = fixture(HttpConfig::default());
    let token_id = f.tokens.records()[0].token_id.clone();
    f.tokens
        .update(
            &token_id,
            crate::token::TokenPatch {
                network: Some(true),
                ..Default::default()
            },
            10,
        )
        .unwrap();
    *f.state.tunnel_hosts.write().unwrap() = vec!["mcp.example.com".into()];
    let request = |method: &str, path: &str, content_type: &str, body: String| {
        Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "mcp.example.com")
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
            .unwrap()
    };
    let unauthenticated = f
        .router
        .clone()
        .oneshot(request("POST", "/mcp", "application/json", LIST.into()))
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    assert!(
        unauthenticated.headers()[header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap()
            .contains("https://mcp.example.com/.well-known/oauth-protected-resource/mcp")
    );
    let metadata = f
        .router
        .clone()
        .oneshot(request(
            "GET",
            "/.well-known/oauth-protected-resource/mcp",
            "application/json",
            String::new(),
        ))
        .await
        .unwrap();
    assert_eq!(metadata.status(), StatusCode::OK);
    assert_eq!(
        json_of(metadata).await["resource"],
        "https://mcp.example.com/mcp"
    );
    let metadata = json_of(
        f.router
            .clone()
            .oneshot(request(
                "GET",
                "/.well-known/oauth-authorization-server",
                "application/json",
                String::new(),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        metadata["code_challenge_methods_supported"],
        json!(["S256"])
    );
    let callback = "https://chatgpt.com/connector_platform_oauth_redirect";
    let registered = f
        .router
        .clone()
        .oneshot(request(
            "POST",
            "/oauth/register",
            "application/json",
            json!({"redirect_uris":[callback],"token_endpoint_auth_method":"none"}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(registered.status(), StatusCode::CREATED);
    let registered = json_of(registered).await;
    let verifier = "0123456789012345678901234567890123456789012345678901234567890123";
    use base64::Engine;
    use sha2::Digest;
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()));
    let mut auth_url = url::Url::parse("https://mcp.example.com/oauth/authorize").unwrap();
    auth_url.query_pairs_mut().extend_pairs([
        ("response_type", "code"),
        ("client_id", registered["client_id"].as_str().unwrap()),
        ("redirect_uri", callback),
        ("resource", "https://mcp.example.com/mcp"),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", "test-state"),
    ]);
    let started = f
        .router
        .clone()
        .oneshot(request(
            "GET",
            &format!("{}?{}", auth_url.path(), auth_url.query().unwrap()),
            "application/json",
            String::new(),
        ))
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::SEE_OTHER);
    let continuation =
        url::Url::parse(started.headers()[header::LOCATION].to_str().unwrap()).unwrap();
    let pending = f.state.oauth.pending(10);
    assert_eq!(pending.len(), 1);
    let parent = f.tokens.records()[0].clone();
    f.state
        .oauth
        .resolve(&pending[0].request_id, Some(&parent), 10)
        .unwrap();
    let approved = f
        .router
        .clone()
        .oneshot(request(
            "GET",
            &format!("{}?{}", continuation.path(), continuation.query().unwrap()),
            "application/json",
            String::new(),
        ))
        .await
        .unwrap();
    let redirect = url::Url::parse(approved.headers()[header::LOCATION].to_str().unwrap()).unwrap();
    let code = redirect
        .query_pairs()
        .find(|(k, _)| k == "code")
        .unwrap()
        .1
        .into_owned();
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    form.extend_pairs([
        ("grant_type", "authorization_code"),
        ("client_id", registered["client_id"].as_str().unwrap()),
        ("code", &code),
        ("code_verifier", verifier),
        ("redirect_uri", callback),
        ("resource", "https://mcp.example.com/mcp"),
    ]);
    let token_response = f
        .router
        .clone()
        .oneshot(request(
            "POST",
            "/oauth/token",
            "application/x-www-form-urlencoded",
            form.finish(),
        ))
        .await
        .unwrap();
    assert_eq!(token_response.status(), StatusCode::OK);
    assert_eq!(token_response.headers()[header::CACHE_CONTROL], "no-store");
    let token_response = json_of(token_response).await;
    let mut mcp = request("POST", "/mcp", "application/json", LIST.into());
    mcp.headers_mut().insert(
        header::AUTHORIZATION,
        format!(
            "Bearer {}",
            token_response["access_token"].as_str().unwrap()
        )
        .parse()
        .unwrap(),
    );
    assert_eq!(
        f.router.clone().oneshot(mcp).await.unwrap().status(),
        StatusCode::OK
    );
    // Host and Origin checks must run before parsing a DCR body.
    let mut invalid = request(
        "POST",
        "/oauth/register",
        "application/json",
        "not json".into(),
    );
    invalid
        .headers_mut()
        .insert(header::HOST, "evil.example.com".parse().unwrap());
    assert_eq!(
        f.router.clone().oneshot(invalid).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let mut cross_site = request(
        "POST",
        "/oauth/token",
        "application/x-www-form-urlencoded",
        String::new(),
    );
    cross_site
        .headers_mut()
        .insert(header::ORIGIN, "https://evil.example.com".parse().unwrap());
    assert_eq!(
        f.router.clone().oneshot(cross_site).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let mut forged_peer = request(
        "GET",
        "/.well-known/oauth-authorization-server",
        "application/json",
        String::new(),
    );
    forged_peer
        .extensions_mut()
        .insert(ConnectInfo("192.0.2.1:443".parse::<SocketAddr>().unwrap()));
    assert_eq!(
        f.router
            .clone()
            .oneshot(forged_peer)
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    // Localhost cannot impersonate the TLS issuer, even with an OAuth access token.
    let local = post_request(token_response["access_token"].as_str(), "localhost", LIST);
    assert_eq!(
        f.router.oneshot(local).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
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
    direct_fixture_for("203.0.113.5")
}

fn direct_fixture_for(host: &str) -> DirectFixture {
    let dir = tempfile::tempdir().unwrap();
    let tokens = Arc::new(TokenStore::new());
    let issue = |network: bool| {
        tokens
            .issue(
                IssueTokenRequest {
                    project_directories: None,
                    client_name: "test".to_string(),
                    workspace_id: "ws".to_string(),
                    approval_mode: ApprovalMode::Ask,
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
    state.public_hosts.write().unwrap().push(host.to_string());
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
async fn lan_addresses_use_the_same_network_token_and_host_checks() {
    for host in ["10.0.0.8", "192.168.1.8"] {
        let f = direct_fixture_for(host);
        for (secret, expected) in [
            (&f.network_secret, StatusCode::OK),
            (&f.local_secret, StatusCode::UNAUTHORIZED),
        ] {
            let response = f
                .router
                .clone()
                .oneshot(from_peer(
                    post_request(Some(secret), host, LIST),
                    "10.0.0.9",
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
        let response = f
            .router
            .oneshot(from_peer(
                post_request(Some(&f.network_secret), "10.0.0.99", LIST),
                "10.0.0.9",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
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
async fn unsupported_methods_and_protocol_versions_are_refused() {
    let f = fixture(HttpConfig::default());
    let get = Request::builder()
        .method("GET")
        .uri("/mcp")
        .header(header::HOST, "localhost")
        .body(Body::empty())
        .unwrap();
    let response = f.router.clone().oneshot(get).await.unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()[header::ALLOW], "POST, DELETE");

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
                project_directories: None,
                client_name: "remote".to_string(),
                workspace_id: "ws".to_string(),
                approval_mode: ApprovalMode::Ask,
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
