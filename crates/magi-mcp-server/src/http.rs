//! Streamable HTTP 传输（回环与网络模式共用同一个处理器）。
//!
//! 检查顺序固定：**Host → Origin → 认证 → 请求体解析 → 调用管线**。认证之前不解析请求体；
//! 认证失败对外统一为 401，不区分原因。`initialize` 签发传输会话以隔离请求与取消；
//! 不带会话头的调用按独立请求处理，不能通过通知取消。不提供 SSE 与会话续传，
//! 服务没有需要服务端主动推送的内容。
//!
//! **对端地址决定“本机请求”**：监听公网地址（如 `0.0.0.0`）时，来自非回环地址的连接无论
//! `Host` 写什么都按网络请求处理——只认 `public_hosts` 里的主机名，只接受显式允许网络使用的令牌，
//! 受限流与失败退避约束，限流来源取真实对端地址（不信任客户端自带的 `X-Forwarded-For` 之类的头）。
//! 来自回环地址的请求（本机客户端、隧道或同机反向代理）保持原有规则。
//!
//! 这是**独立入口**：不挂到 Magi 主应用、不共享其鉴权中间件、不暴露任何 `/api/*` 路由（M10）。

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, RwLock};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::protocol::{JSONRPC_PARSE_ERROR, JsonRpcRequest, error};
use crate::rate_limit::RateLimiter;
use crate::server::{ClientPrincipal, ConnectionScope, MAX_ARGUMENT_BYTES, McpServer};
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

const SESSION_HEADER: &str = "mcp-session-id";
const SESSION_IDLE_MS: u64 = 30 * 60 * 1000;
const MAX_HTTP_SESSIONS: usize = 1024;

struct HttpSession {
    token_id: String,
    scope: Arc<ConnectionScope>,
    last_used_ms: u64,
}

#[derive(Clone)]
pub struct HttpState {
    pub server: Arc<McpServer>,
    pub tokens: Arc<TokenStore>,
    pub oauth: Arc<crate::oauth::OAuthStore>,
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
    sessions: Arc<Mutex<HashMap<String, HttpSession>>>,
}

impl HttpState {
    /// 尚未终止或闲置过期的 HTTP 传输会话；不代表在线客户端数量。
    pub fn session_count(&self) -> usize {
        let now = (self.clock)();
        let records = self.tokens.records();
        self.sessions
            .lock()
            .expect("HTTP sessions poisoned")
            .values()
            .filter(|session| {
                (now.saturating_sub(session.last_used_ms) < SESSION_IDLE_MS
                    || Arc::strong_count(&session.scope) > 1)
                    && records
                        .iter()
                        .any(|record| record.token_id == session.token_id && record.is_active(now))
            })
            .count()
    }

    pub fn new(server: Arc<McpServer>, tokens: Arc<TokenStore>, config: HttpConfig) -> Self {
        Self {
            server,
            tokens,
            oauth: Arc::new(crate::oauth::OAuthStore::default()),
            config,
            clock: Arc::new(system_now_ms),
            tunnel_hosts: Arc::new(RwLock::new(Vec::new())),
            public_hosts: Arc::new(RwLock::new(Vec::new())),
            limiter: Arc::new(RateLimiter::new()),
            sessions: Arc::new(Mutex::new(HashMap::new())),
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
        .route(
            "/mcp",
            post(handle_post)
                .delete(handle_post)
                .fallback(method_not_allowed),
        )
        .merge(crate::oauth::routes(state.clone()))
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .with_state(state)
}

async fn method_not_allowed() -> Response {
    let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST, DELETE"));
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

fn unauthorized(issuer: Option<&str>) -> Response {
    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized" })),
    )
        .into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        issuer
            .and_then(|issuer| HeaderValue::from_str(&format!("Bearer realm=\"magi-mcp\", resource_metadata=\"{issuer}/.well-known/oauth-protected-resource/mcp\", scope=\"magi\"")).ok())
            .unwrap_or_else(|| HeaderValue::from_static("Bearer realm=\"magi-mcp\"")),
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
pub(crate) fn forwarded_source(headers: &HeaderMap) -> String {
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
    method: Method,
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
    let oauth_issuer = crate::oauth::issuer(&state, peer, &headers);
    let record = match bearer_secret(&headers)
        .ok_or(())
        .and_then(|secret| {
            state
                .tokens
                .authenticate(secret, now_ms)
                .ok()
                .or_else(|| {
                    state.oauth.authenticate(
                        secret,
                        oauth_issuer.as_deref()?,
                        &state.tokens,
                        now_ms,
                    )
                })
                .ok_or(())
        })
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
            return unauthorized(oauth_issuer.as_deref());
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

    let request = if method == Method::POST {
        match serde_json::from_slice::<JsonRpcRequest>(&body) {
            Ok(request) => Some(request),
            Err(parse_error) => {
                return Json(error(
                    Value::Null,
                    JSONRPC_PARSE_ERROR,
                    format!("请求体不是合法 JSON-RPC：{parse_error}"),
                ))
                .into_response();
            }
        }
    } else {
        None
    };
    let principal = ClientPrincipal::from(&record);
    // 传输会话只隔离请求与取消，不改变令牌对应的 Magi 会话、工作区和权限。
    // 无会话请求各用独立 scope，不能用相同的 JSON-RPC ID 取消其他客户端的调用。
    if method == Method::DELETE && !headers.contains_key(SESSION_HEADER) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let (scope, registration) = {
        let mut sessions = state.sessions.lock().expect("HTTP sessions poisoned");
        sessions.retain(|_, session| {
            now_ms.saturating_sub(session.last_used_ms) < SESSION_IDLE_MS
                || Arc::strong_count(&session.scope) > 1
        });
        let scope = if let Some(id) = headers.get(SESSION_HEADER) {
            let Ok(id) = id.to_str() else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            let Some(session) = sessions
                .get_mut(id)
                .filter(|s| s.token_id == record.token_id)
            else {
                return StatusCode::NOT_FOUND.into_response();
            };
            if method == Method::DELETE {
                let session = sessions.remove(id).expect("session exists");
                session.scope.cancel();
                return StatusCode::NO_CONTENT.into_response();
            }
            session.last_used_ms = now_ms;
            session.scope.clone()
        } else {
            Arc::new(state.server.connection_scope())
        };
        // 与 DELETE 共用锁：登记完成后才允许会话终止，避免漏掉刚进入的调用。
        let registration = request
            .as_ref()
            .and_then(|request| state.server.prepare_request(&principal, request, scope.id));
        (scope, registration)
    };
    let request = request.expect("DELETE returned above");
    let initialize = request.method == "initialize" && !headers.contains_key(SESSION_HEADER);
    match state
        .server
        .handle_prepared(&principal, request, now_ms, registration)
        .await
    {
        Some(response) => {
            let mut session_header = None;
            if initialize && response.get("result").is_some() {
                let mut random = [0u8; 32];
                if getrandom::fill(&mut random).is_err() {
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
                let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
                let mut sessions = state.sessions.lock().expect("HTTP sessions poisoned");
                if sessions.len() >= MAX_HTTP_SESSIONS {
                    return too_many_requests();
                }
                sessions.insert(
                    id.clone(),
                    HttpSession {
                        token_id: record.token_id,
                        scope,
                        last_used_ms: now_ms,
                    },
                );
                session_header = Some(HeaderValue::from_str(&id).expect("hex session ID"));
            }
            let mut response = Json(response).into_response();
            if let Some(id) = session_header {
                response.headers_mut().insert(SESSION_HEADER, id);
            }
            response
        }
        None => StatusCode::ACCEPTED.into_response(),
    }
}

#[cfg(test)]
mod tests;
