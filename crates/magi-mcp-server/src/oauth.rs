//! OAuth is a delegation of an existing Magi client, never a second permissions owner.
//! Public clients use DCR + S256 PKCE. Consent can only be resolved by the local
//! management API. Access/refresh tokens contain no project grants and are checked
//! against the live parent token (including its key generation) on every use.
use std::{collections::HashMap, path::PathBuf, sync::Mutex};

use axum::{
    Extension, Json, Router,
    extract::{Form, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use crate::{
    http::HttpState,
    token::{TokenRecord, TokenStore, constant_time_eq},
};

const SCOPE: &str = "magi";
const PENDING_MS: u64 = 5 * 60 * 1000;
const ACCESS_MS: u64 = 60 * 60 * 1000;
const GRANT_MS: u64 = 90 * 24 * 60 * 60 * 1000;
const MAX_CLIENTS: usize = 1024;
const MAX_PENDING: usize = 64;
const MAX_GRANTS: usize = 2048;

#[derive(Clone, Serialize, Deserialize)]
struct Client {
    client_id: String,
    client_name: String,
    redirect_uris: Vec<String>,
    issuer: String,
}

#[derive(Clone, Deserialize)]
pub struct AuthorizationRequest {
    pub response_type: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    pub resource: String,
    pub scope: Option<String>,
    pub state: Option<String>,
}

#[derive(Clone)]
struct Pending {
    id: String,
    client_name: String,
    request: AuthorizationRequest,
    issuer: String,
    expires_at: u64,
    decision: Option<Consent>,
}

#[derive(Clone)]
enum Consent {
    Denied,
    Approved { code: String },
}

#[derive(Clone)]
struct Code {
    request: AuthorizationRequest,
    issuer: String,
    token_id: String,
    parent_hash: String,
    expires_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct Grant {
    client_id: String,
    issuer: String,
    resource: String,
    token_id: String,
    parent_hash: String,
    access_hash: String,
    refresh_hash: String,
    used_refresh_hashes: Vec<String>,
    access_expires_at: u64,
    expires_at: u64,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Persisted {
    clients: Vec<Client>,
    grants: Vec<Grant>,
}

#[derive(Default)]
struct Inner {
    persisted: Persisted,
    pending: HashMap<String, Pending>,
    codes: HashMap<String, Code>,
}

#[derive(Default)]
pub struct OAuthStore {
    inner: Mutex<Inner>,
    path: Option<PathBuf>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingConsent {
    pub request_id: String,
    pub client_name: String,
    pub redirect_uri: String,
    pub resource: String,
    pub expires_at_ms: u64,
}

fn random() -> Result<String, String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| "OAuth random source unavailable".to_string())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
fn hash(secret: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes()))
}
fn parent(tokens: &TokenStore, id: &str, generation: &str, now: u64) -> Option<TokenRecord> {
    tokens.records().into_iter().find(|token| {
        token.token_id == id
            && token.network
            && token.is_active(now)
            && constant_time_eq(token.hash.as_bytes(), generation.as_bytes())
    })
}

impl OAuthStore {
    pub fn load(path: PathBuf) -> Self {
        let persisted = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(%error, "OAuth store invalid; existing delegations are disabled");
                Persisted::default()
            }),
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(%error, "OAuth store unavailable; existing delegations are disabled");
                }
                Persisted::default()
            }
        };
        Self {
            inner: Mutex::new(Inner {
                persisted,
                ..Default::default()
            }),
            path: Some(path),
        }
    }

    // Hold the store lock through disk commit. Failed commits never publish credentials.
    fn commit(&self, inner: &mut Inner, next: Persisted) -> Result<(), String> {
        if let Some(path) = &self.path {
            let write = || -> Result<(), Box<dyn std::error::Error>> {
                use std::io::Write;
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let temp = path.with_extension(format!("{}.tmp", random()?));
                let result = (|| -> Result<(), Box<dyn std::error::Error>> {
                    let mut options = std::fs::OpenOptions::new();
                    options.write(true).create_new(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        options.mode(0o600);
                    }
                    let mut file = options.open(&temp)?;
                    file.write_all(&serde_json::to_vec(&next)?)?;
                    file.sync_all()?;
                    std::fs::rename(&temp, path)?;
                    Ok(())
                })();
                if result.is_err() {
                    let _ = std::fs::remove_file(&temp);
                }
                result
            };
            write().map_err(|_| "OAuth persistence failed".to_string())?;
        }
        inner.persisted = next;
        Ok(())
    }

    fn register(&self, issuer: &str, input: Registration) -> Result<Value, String> {
        if input.redirect_uris.is_empty()
            || input.redirect_uris.len() > 8
            || input
                .client_name
                .as_ref()
                .is_some_and(|name| name.len() > 160)
            || input
                .token_endpoint_auth_method
                .as_deref()
                .is_some_and(|method| method != "none")
            || input.grant_types.as_ref().is_some_and(|types| {
                types
                    .iter()
                    .any(|t| t != "authorization_code" && t != "refresh_token")
            })
            || input
                .response_types
                .as_ref()
                .is_some_and(|types| types != &["code"])
        {
            return Err("invalid_client_metadata".into());
        }
        for raw in &input.redirect_uris {
            let url = Url::parse(raw).map_err(|_| "invalid_redirect_uri")?;
            if raw.len() > 2048
                || url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err("invalid_redirect_uri".into());
            }
        }
        let client = Client {
            client_id: random()?,
            client_name: input
                .client_name
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "MCP client".into()),
            redirect_uris: input.redirect_uris,
            issuer: issuer.into(),
        };
        let mut inner = self.inner.lock().expect("OAuth store poisoned");
        if inner.persisted.clients.len() >= MAX_CLIENTS {
            return Err("temporarily_unavailable".into());
        }
        let mut next = inner.persisted.clone();
        next.clients.push(client.clone());
        self.commit(&mut inner, next)?;
        Ok(
            json!({"client_id":client.client_id,"client_name":client.client_name,"redirect_uris":client.redirect_uris,"token_endpoint_auth_method":"none","grant_types":["authorization_code","refresh_token"],"response_types":["code"]}),
        )
    }

    fn start(
        &self,
        issuer: &str,
        request: AuthorizationRequest,
        now: u64,
    ) -> Result<String, String> {
        let mut inner = self.inner.lock().expect("OAuth store poisoned");
        let client = inner
            .persisted
            .clients
            .iter()
            .find(|client| {
                client.client_id == request.client_id
                    && client.issuer == issuer
                    && client.redirect_uris.contains(&request.redirect_uri)
            })
            .ok_or("invalid_client")?
            .clone();
        if request.response_type != "code"
            || request.code_challenge_method != "S256"
            || request.code_challenge.len() != 43
            || URL_SAFE_NO_PAD
                .decode(&request.code_challenge)
                .map_or(true, |v| v.len() != 32)
            || request.resource != format!("{issuer}/mcp")
            || request.scope.as_deref().is_some_and(|s| s != SCOPE)
            || request.state.as_ref().is_some_and(|s| s.len() > 2048)
        {
            return Err("invalid_request".into());
        }
        inner.pending.retain(|_, p| p.expires_at > now);
        inner.codes.retain(|_, c| c.expires_at > now);
        if inner.pending.len() >= MAX_PENDING {
            return Err("temporarily_unavailable".into());
        }
        let id = random()?;
        inner.pending.insert(
            id.clone(),
            Pending {
                id: id.clone(),
                client_name: client.client_name,
                request,
                issuer: issuer.into(),
                expires_at: now.saturating_add(PENDING_MS),
                decision: None,
            },
        );
        Ok(id)
    }

    pub fn pending(&self, now: u64) -> Vec<PendingConsent> {
        let inner = self.inner.lock().expect("OAuth store poisoned");
        inner
            .pending
            .values()
            .filter(|p| p.expires_at > now && p.decision.is_none())
            .map(|p| PendingConsent {
                request_id: p.id.clone(),
                client_name: p.client_name.clone(),
                redirect_uri: p.request.redirect_uri.clone(),
                resource: p.request.resource.clone(),
                expires_at_ms: p.expires_at,
            })
            .collect()
    }

    pub fn resolve(&self, id: &str, token: Option<&TokenRecord>, now: u64) -> Result<(), String> {
        let mut inner = self.inner.lock().expect("OAuth store poisoned");
        let pending = inner
            .pending
            .get(id)
            .filter(|p| p.expires_at > now && p.decision.is_none())
            .ok_or("OAuth request expired or already resolved")?
            .clone();
        let decision = if let Some(token) = token {
            if !token.network || !token.is_active(now) {
                return Err("An active network client is required".into());
            }
            let code = random()?;
            inner.codes.insert(
                hash(&code),
                Code {
                    request: pending.request,
                    issuer: pending.issuer,
                    token_id: token.token_id.clone(),
                    parent_hash: token.hash.clone(),
                    expires_at: now.saturating_add(60_000),
                },
            );
            Consent::Approved { code }
        } else {
            Consent::Denied
        };
        inner.pending.get_mut(id).expect("pending exists").decision = Some(decision);
        Ok(())
    }

    fn page(&self, issuer: &str, id: &str, now: u64) -> Result<Response, String> {
        let mut inner = self.inner.lock().expect("OAuth store poisoned");
        let pending = inner
            .pending
            .get(id)
            .filter(|p| p.issuer == issuer && p.expires_at > now)
            .ok_or("Authorization request expired")?;
        if let Some(decision) = &pending.decision {
            let mut target =
                Url::parse(&pending.request.redirect_uri).map_err(|_| "invalid_redirect_uri")?;
            {
                let mut query = target.query_pairs_mut();
                query.append_pair("iss", issuer);
                if let Some(state) = &pending.request.state {
                    query.append_pair("state", state);
                }
                match decision {
                    Consent::Denied => {
                        query.append_pair("error", "access_denied");
                    }
                    Consent::Approved { code } => {
                        query.append_pair("code", code);
                    }
                }
            }
            inner.pending.remove(id);
            return Ok(Redirect::to(target.as_str()).into_response());
        }
        // No JavaScript, token input, or remote approval endpoint. Refresh only this
        // unguessable request capability; it can never approve itself.
        Ok(Html(format!("<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"refresh\" content=\"3\"><title>Magi · 授权连接</title><main><h1>在本机 Magi 中确认连接</h1><p>客户端：{}</p><p>回调地址：{}</p><p>打开 Magi → 设置 → MCP 服务，选择此客户端使用的权限并批准。此页面会自动继续。</p><p>每个新聊天首次使用时，仍需在 Magi 选择执行目录。没有发起此连接时，请在 Magi 拒绝。</p><p lang=\"en\">Approve this connection in Magi → Settings → MCP service. This page will continue automatically.</p></main></html>", escape(&pending.client_name), escape(&pending.request.redirect_uri))).into_response())
    }

    fn exchange(
        &self,
        issuer: &str,
        input: TokenRequest,
        tokens: &TokenStore,
        now: u64,
    ) -> Result<Value, String> {
        let mut inner = self.inner.lock().expect("OAuth store poisoned");
        let resource = format!("{issuer}/mcp");
        if input.resource.as_deref() != Some(resource.as_str())
            || input.scope.as_deref().is_some_and(|s| s != SCOPE)
        {
            return Err("invalid_target".into());
        }
        let mut next = inner.persisted.clone();
        next.grants.retain(|grant| grant.expires_at > now);
        let mut grant = match input.grant_type.as_str() {
            "authorization_code" => {
                let key = hash(input.code.as_deref().ok_or("invalid_grant")?);
                let code = inner.codes.get(&key).ok_or("invalid_grant")?;
                let verifier = input.code_verifier.as_deref().ok_or("invalid_grant")?;
                if code.expires_at <= now
                    || code.issuer != issuer
                    || code.request.client_id != input.client_id
                    || input.redirect_uri.as_ref() != Some(&code.request.redirect_uri)
                    || !(43..=128).contains(&verifier.len())
                    || !verifier
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
                    || !constant_time_eq(
                        hash(verifier).as_bytes(),
                        code.request.code_challenge.as_bytes(),
                    )
                {
                    return Err("invalid_grant".into());
                }
                let token = parent(tokens, &code.token_id, &code.parent_hash, now)
                    .ok_or("invalid_grant")?;
                if next.grants.len() >= MAX_GRANTS {
                    return Err("temporarily_unavailable".into());
                }
                let grant = Grant {
                    client_id: input.client_id,
                    issuer: issuer.into(),
                    resource,
                    token_id: token.token_id,
                    parent_hash: token.hash,
                    access_hash: String::new(),
                    refresh_hash: String::new(),
                    used_refresh_hashes: vec![],
                    access_expires_at: 0,
                    expires_at: token
                        .expires_at_ms
                        .unwrap_or(u64::MAX)
                        .min(now.saturating_add(GRANT_MS)),
                };
                inner.codes.remove(&key);
                grant
            }
            "refresh_token" => {
                let key = hash(input.refresh_token.as_deref().ok_or("invalid_grant")?);
                if let Some(index) = next.grants.iter().position(|g| {
                    g.client_id == input.client_id
                        && g.issuer == issuer
                        && g.used_refresh_hashes.contains(&key)
                }) {
                    next.grants.remove(index);
                    self.commit(&mut inner, next)?;
                    return Err("invalid_grant".into());
                }
                let index = next
                    .grants
                    .iter()
                    .position(|g| {
                        g.client_id == input.client_id
                            && g.issuer == issuer
                            && constant_time_eq(g.refresh_hash.as_bytes(), key.as_bytes())
                    })
                    .ok_or("invalid_grant")?;
                let mut grant = next.grants.remove(index);
                parent(tokens, &grant.token_id, &grant.parent_hash, now).ok_or("invalid_grant")?;
                // Bound rotation history; a connection is reauthorized after 2048 refreshes.
                if grant.used_refresh_hashes.len() >= 2048 {
                    return Err("invalid_grant".into());
                }
                grant.used_refresh_hashes.push(key);
                grant
            }
            _ => return Err("unsupported_grant_type".into()),
        };
        let access = format!("magi_oauth_{}", random()?);
        let refresh = random()?;
        grant.access_hash = hash(&access);
        grant.refresh_hash = hash(&refresh);
        grant.access_expires_at = now.saturating_add(ACCESS_MS).min(grant.expires_at);
        let expires_in = grant.access_expires_at.saturating_sub(now) / 1000;
        next.grants.push(grant);
        self.commit(&mut inner, next)?;
        Ok(
            json!({"access_token":access,"token_type":"Bearer","expires_in":expires_in,"refresh_token":refresh,"scope":SCOPE}),
        )
    }

    pub fn authenticate(
        &self,
        secret: &str,
        issuer: &str,
        tokens: &TokenStore,
        now: u64,
    ) -> Option<TokenRecord> {
        let inner = self.inner.lock().expect("OAuth store poisoned");
        let key = hash(secret);
        let grant = inner.persisted.grants.iter().find(|g| {
            g.issuer == issuer
                && g.resource == format!("{issuer}/mcp")
                && g.access_expires_at > now
                && g.expires_at > now
                && constant_time_eq(g.access_hash.as_bytes(), key.as_bytes())
        })?;
        parent(tokens, &grant.token_id, &grant.parent_hash, now)
    }
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[derive(Deserialize)]
struct Registration {
    redirect_uris: Vec<String>,
    client_name: Option<String>,
    token_endpoint_auth_method: Option<String>,
    grant_types: Option<Vec<String>>,
    response_types: Option<Vec<String>>,
}
#[derive(Deserialize)]
struct TokenRequest {
    grant_type: String,
    client_id: String,
    resource: Option<String>,
    scope: Option<String>,
    code: Option<String>,
    code_verifier: Option<String>,
    redirect_uri: Option<String>,
    refresh_token: Option<String>,
}

// OAuth is only advertised for a running HTTPS tunnel. Direct cleartext and
// localhost never become an authorization issuer through forwarded headers.
pub(crate) fn issuer(
    state: &HttpState,
    peer: Option<std::net::IpAddr>,
    headers: &HeaderMap,
) -> Option<String> {
    if peer.is_some_and(|p| !p.is_loopback()) {
        return None;
    }
    let host = headers.get(header::HOST)?.to_str().ok()?;
    state
        .tunnel_hosts
        .read()
        .ok()?
        .iter()
        .find(|allowed| allowed.eq_ignore_ascii_case(host))
        .map(|host| format!("https://{host}"))
}
fn context(
    state: &HttpState,
    peer: Option<std::net::IpAddr>,
    headers: &HeaderMap,
) -> Result<String, Response> {
    let issuer =
        issuer(state, peer, headers).ok_or_else(|| StatusCode::FORBIDDEN.into_response())?;
    if headers
        .get(header::ORIGIN)
        .is_some_and(|origin| origin.to_str().ok() != Some(issuer.as_str()))
    {
        return Err(StatusCode::FORBIDDEN.into_response());
    }
    let source = crate::http::forwarded_source(headers);
    if !state
        .limiter
        .allow_request(&format!("oauth:{source}"), (state.clock)())
    {
        return Err(StatusCode::TOO_MANY_REQUESTS.into_response());
    }
    Ok(issuer)
}
#[derive(Clone)]
struct OAuthOrigin(String);

async fn guard(
    State(state): State<HttpState>,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let peer = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.ip());
    let origin = match context(&state, peer, request.headers()) {
        Ok(origin) => origin,
        Err(response) => return secure(response),
    };
    request.extensions_mut().insert(OAuthOrigin(origin));
    let path = request.uri().path().to_string();
    let response = next.run(request).await;
    if response.status().is_client_error() || response.status().is_server_error() {
        tracing::warn!(
            path,
            status = response.status().as_u16(),
            "OAuth HTTP request rejected"
        );
    }
    secure(response)
}

fn secure(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert(header::PRAGMA, "no-cache".parse().unwrap());
    headers.insert("referrer-policy", "no-referrer".parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert(
        "content-security-policy",
        "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"
            .parse()
            .unwrap(),
    );
    response
}
fn failure(message: String) -> Response {
    secure((StatusCode::BAD_REQUEST, Json(json!({"error":message}))).into_response())
}

pub(crate) fn routes(state: HttpState) -> Router<HttpState> {
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_resource),
        )
        .route("/.well-known/oauth-authorization-server", get(metadata))
        .route("/oauth/register", post(register))
        .route("/oauth/authorize", get(authorize))
        .route("/oauth/token", post(token))
        .layer(axum::extract::DefaultBodyLimit::max(32 * 1024))
        .route_layer(axum::middleware::from_fn_with_state(state, guard))
}
async fn protected_resource(Extension(OAuthOrigin(origin)): Extension<OAuthOrigin>) -> Response {
    secure(Json(json!({"resource":format!("{origin}/mcp"),"authorization_servers":[origin],"scopes_supported":[SCOPE],"bearer_methods_supported":["header"]})).into_response())
}
async fn metadata(Extension(OAuthOrigin(origin)): Extension<OAuthOrigin>) -> Response {
    secure(Json(json!({"issuer":origin,"authorization_endpoint":format!("{origin}/oauth/authorize"),"token_endpoint":format!("{origin}/oauth/token"),"registration_endpoint":format!("{origin}/oauth/register"),"response_types_supported":["code"],"grant_types_supported":["authorization_code","refresh_token"],"token_endpoint_auth_methods_supported":["none"],"code_challenge_methods_supported":["S256"],"scopes_supported":[SCOPE],"authorization_response_iss_parameter_supported":true})).into_response())
}
async fn register(
    State(state): State<HttpState>,
    Extension(OAuthOrigin(origin)): Extension<OAuthOrigin>,
    Json(input): Json<Registration>,
) -> Response {
    match state.oauth.register(&origin, input) {
        Ok(body) => secure((StatusCode::CREATED, Json(body)).into_response()),
        Err(message) => failure(message),
    }
}
async fn authorize(
    State(state): State<HttpState>,
    Extension(OAuthOrigin(origin)): Extension<OAuthOrigin>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let now = (state.clock)();
    if let Some(id) = query.get("request_id") {
        return match state.oauth.page(&origin, id, now) {
            Ok(response) => secure(response),
            Err(message) => failure(message),
        };
    }
    let input = match serde_json::from_value::<AuthorizationRequest>(json!(query)) {
        Ok(input) => input,
        Err(_) => return failure("invalid_request".into()),
    };
    match state.oauth.start(&origin, input, now) {
        Ok(id) => secure(
            Redirect::to(&format!("{origin}/oauth/authorize?request_id={id}")).into_response(),
        ),
        Err(message) => failure(message),
    }
}
async fn token(
    State(state): State<HttpState>,
    Extension(OAuthOrigin(origin)): Extension<OAuthOrigin>,
    Form(input): Form<TokenRequest>,
) -> Response {
    let grant_type = match input.grant_type.as_str() {
        "authorization_code" => "authorization_code",
        "refresh_token" => "refresh_token",
        _ => "unsupported",
    };
    let resource_present = input.resource.is_some();
    match state
        .oauth
        .exchange(&origin, input, &state.tokens, (state.clock)())
    {
        Ok(body) => secure(Json(body).into_response()),
        Err(message) => {
            tracing::warn!(grant_type, resource_present, error = %message, "OAuth token exchange rejected");
            failure(message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ApprovalMode, AttributionMode, IssueTokenRequest, TokenPatch};
    const ISSUER: &str = "https://mcp.example.com";
    const CALLBACK: &str = "https://chatgpt.com/connector_platform_oauth_redirect";
    const VERIFIER: &str = "0123456789012345678901234567890123456789012345678901234567890123";
    fn client(store: &OAuthStore) -> String {
        store
            .register(
                ISSUER,
                Registration {
                    redirect_uris: vec![CALLBACK.into()],
                    client_name: Some("ChatGPT".into()),
                    token_endpoint_auth_method: Some("none".into()),
                    grant_types: None,
                    response_types: None,
                },
            )
            .unwrap()["client_id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn request(id: &str) -> AuthorizationRequest {
        AuthorizationRequest {
            response_type: "code".into(),
            client_id: id.into(),
            redirect_uri: CALLBACK.into(),
            code_challenge: hash(VERIFIER),
            code_challenge_method: "S256".into(),
            resource: format!("{ISSUER}/mcp"),
            scope: Some(SCOPE.into()),
            state: Some("client-state".into()),
        }
    }
    fn tokens() -> (TokenStore, TokenRecord) {
        let tokens = TokenStore::new();
        let issued = tokens
            .issue(
                IssueTokenRequest {
                    client_name: "ChatGPT".into(),
                    workspace_id: "".into(),
                    project_directories: None,
                    approval_mode: ApprovalMode::Always,
                    attribution: AttributionMode::Conversation,
                    network: true,
                    ttl_ms: Some(GRANT_MS),
                },
                0,
            )
            .unwrap();
        (tokens, issued.record)
    }
    fn code(store: &OAuthStore, id: &str, token: &TokenRecord) -> String {
        let pending = store.start(ISSUER, request(id), 10).unwrap();
        store.resolve(&pending, Some(token), 20).unwrap();
        let response = store.page(ISSUER, &pending, 30).unwrap();
        let url = Url::parse(response.headers()[header::LOCATION].to_str().unwrap()).unwrap();
        assert!(
            url.query_pairs()
                .any(|(k, v)| k == "state" && v == "client-state")
        );
        assert!(url.query_pairs().any(|(k, v)| k == "iss" && v == ISSUER));
        url.query_pairs()
            .find(|(k, _)| k == "code")
            .unwrap()
            .1
            .into_owned()
    }
    fn exchange_input(client: &str, code: &str) -> TokenRequest {
        TokenRequest {
            grant_type: "authorization_code".into(),
            client_id: client.into(),
            resource: Some(format!("{ISSUER}/mcp")),
            scope: None,
            code: Some(code.into()),
            code_verifier: Some(VERIFIER.into()),
            redirect_uri: Some(CALLBACK.into()),
            refresh_token: None,
        }
    }
    fn refresh_input(client: &str, refresh: &str) -> TokenRequest {
        TokenRequest {
            grant_type: "refresh_token".into(),
            client_id: client.into(),
            resource: Some(format!("{ISSUER}/mcp")),
            scope: None,
            code: None,
            code_verifier: None,
            redirect_uri: None,
            refresh_token: Some(refresh.into()),
        }
    }
    #[test]
    fn consent_is_local_single_use_and_redirect_and_pkce_are_bound() {
        let store = OAuthStore::default();
        let id = client(&store);
        let (tokens, token) = tokens();
        let mut bad = request(&id);
        bad.redirect_uri = "https://attacker.example/".into();
        assert!(store.start(ISSUER, bad, 10).is_err());
        let mut bad = request(&id);
        bad.code_challenge_method = "plain".into();
        assert!(store.start(ISSUER, bad, 10).is_err());
        let mut bad = request(&id);
        bad.resource = "https://other.example/mcp".into();
        assert!(store.start(ISSUER, bad, 10).is_err());
        let pending = store.start(ISSUER, request(&id), 10).unwrap();
        assert_eq!(store.pending(11).len(), 1);
        assert_eq!(
            store.page(ISSUER, &pending, 11).unwrap().status(),
            StatusCode::OK
        );
        assert!(store.page("https://other.example", &pending, 11).is_err());
        assert!(
            store
                .resolve(&pending, Some(&token), PENDING_MS + 11)
                .is_err()
        );
        let c = code(&store, &id, &token);
        let mut bad = exchange_input(&id, &c);
        bad.code_verifier = Some("X".repeat(64));
        assert!(store.exchange(ISSUER, bad, &tokens, 50).is_err());
        let mut bad = exchange_input(&id, &c);
        bad.client_id = "other".into();
        assert!(store.exchange(ISSUER, bad, &tokens, 50).is_err());
        let mut bad = exchange_input(&id, &c);
        bad.redirect_uri = Some("https://attacker.example/".into());
        assert!(store.exchange(ISSUER, bad, &tokens, 50).is_err());
        let body = store
            .exchange(ISSUER, exchange_input(&id, &c), &tokens, 50)
            .unwrap();
        assert!(
            store
                .exchange(ISSUER, exchange_input(&id, &c), &tokens, 51)
                .is_err()
        );
        let access = body["access_token"].as_str().unwrap();
        assert!(store.authenticate(access, ISSUER, &tokens, 60).is_some());
        assert!(
            store
                .authenticate(access, "https://other.example", &tokens, 60)
                .is_none()
        );
        assert!(
            store
                .authenticate(access, ISSUER, &tokens, ACCESS_MS + 50)
                .is_none()
        );
    }
    #[test]
    fn refresh_rotation_survives_restart_and_replay_revokes_the_family() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oauth.json");
        let store = OAuthStore::load(path.clone());
        let id = client(&store);
        let (tokens, token) = tokens();
        let c = code(&store, &id, &token);
        let first = store
            .exchange(ISSUER, exchange_input(&id, &c), &tokens, 50)
            .unwrap();
        let restored = OAuthStore::load(path.clone());
        assert!(
            restored
                .authenticate(first["access_token"].as_str().unwrap(), ISSUER, &tokens, 60)
                .is_some()
        );
        let second = restored
            .exchange(
                ISSUER,
                refresh_input(&id, first["refresh_token"].as_str().unwrap()),
                &tokens,
                70,
            )
            .unwrap();
        assert_ne!(first["refresh_token"], second["refresh_token"]);
        assert!(
            restored
                .exchange(
                    ISSUER,
                    refresh_input(&id, first["refresh_token"].as_str().unwrap()),
                    &tokens,
                    80
                )
                .is_err()
        );
        assert!(
            restored
                .authenticate(
                    second["access_token"].as_str().unwrap(),
                    ISSUER,
                    &tokens,
                    90
                )
                .is_none()
        );
        assert!(
            restored
                .exchange(
                    ISSUER,
                    refresh_input(&id, second["refresh_token"].as_str().unwrap()),
                    &tokens,
                    90
                )
                .is_err()
        );
        let disk = std::fs::read_to_string(&path).unwrap();
        assert!(!disk.contains(first["access_token"].as_str().unwrap()));
        assert!(!disk.contains(first["refresh_token"].as_str().unwrap()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn parent_policy_is_live_and_expiry_revocation_rotation_and_network_removal_apply() {
        for change in ["policy", "expire", "revoke", "rotate", "network"] {
            let store = OAuthStore::default();
            let id = client(&store);
            let (tokens, token) = tokens();
            let c = code(&store, &id, &token);
            let body = store
                .exchange(ISSUER, exchange_input(&id, &c), &tokens, 50)
                .unwrap();
            match change {
                "policy" => {
                    tokens
                        .update(
                            &token.token_id,
                            TokenPatch {
                                approval_mode: Some(ApprovalMode::Ask),
                                ..Default::default()
                            },
                            60,
                        )
                        .unwrap();
                }
                "expire" => {
                    tokens
                        .update(
                            &token.token_id,
                            TokenPatch {
                                expires_at_ms: Some(Some(60)),
                                ..Default::default()
                            },
                            60,
                        )
                        .unwrap();
                }
                "revoke" => {
                    tokens.revoke(&token.token_id, 60);
                }
                "rotate" => {
                    tokens.rotate(&token.token_id).unwrap();
                }
                "network" => {
                    tokens
                        .update(
                            &token.token_id,
                            TokenPatch {
                                network: Some(false),
                                ..Default::default()
                            },
                            60,
                        )
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let access =
                store.authenticate(body["access_token"].as_str().unwrap(), ISSUER, &tokens, 70);
            if change == "policy" {
                assert_eq!(access.unwrap().approval_mode, ApprovalMode::Ask);
            } else {
                assert!(access.is_none(), "{change}");
                assert!(
                    store
                        .exchange(
                            ISSUER,
                            refresh_input(&id, body["refresh_token"].as_str().unwrap()),
                            &tokens,
                            70
                        )
                        .is_err()
                );
            }
        }
    }
    #[test]
    fn denial_does_not_issue_a_code_and_expired_codes_cannot_exchange() {
        let store = OAuthStore::default();
        let id = client(&store);
        let (tokens, token) = tokens();
        let pending = store.start(ISSUER, request(&id), 10).unwrap();
        store.resolve(&pending, None, 20).unwrap();
        let response = store.page(ISSUER, &pending, 30).unwrap();
        let url = Url::parse(response.headers()[header::LOCATION].to_str().unwrap()).unwrap();
        assert!(
            url.query_pairs()
                .any(|(k, v)| k == "error" && v == "access_denied")
        );
        assert!(url.query_pairs().any(|(k, v)| k == "iss" && v == ISSUER));
        assert!(!url.query_pairs().any(|(k, _)| k == "code"));
        assert!(store.page(ISSUER, &pending, 40).is_err());
        let c = code(&store, &id, &token);
        assert!(
            store
                .exchange(ISSUER, exchange_input(&id, &c), &tokens, 60_020)
                .is_err()
        );
    }
    #[test]
    fn persistence_failure_never_publishes_a_client_or_access_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = OAuthStore {
            path: Some(dir.path().into()),
            ..Default::default()
        };
        assert!(
            store
                .register(
                    ISSUER,
                    Registration {
                        redirect_uris: vec![CALLBACK.into()],
                        client_name: None,
                        token_endpoint_auth_method: None,
                        grant_types: None,
                        response_types: None
                    }
                )
                .is_err()
        );
        assert!(store.inner.lock().unwrap().persisted.clients.is_empty());
    }
}
