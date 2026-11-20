//! Transport for an already admitted plugin document. The caller owns the
//! public origin and supplies the plugin-relative request target after routing.
use axum::{
    BoxError,
    body::Body,
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri, Version,
        header,
    },
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use http_body::{Body as HttpBody, Frame, SizeHint};
use hyper::{client::conn::http1, upgrade::OnUpgrade};
use hyper_util::rt::TokioIo;
use magi_api::ApiError;
use magi_api::native_plugins::PluginRouteSession;
use magi_plugin_system::{
    LoadingActivation,
    loading_runtime::{LoadingRuntime, LoadingWebRoutes, PluginRouteLease, PluginRoutePage},
};
use std::{
    pin::Pin,
    sync::{Arc, RwLock},
    task::{Context, Poll},
    time::Duration,
};
use tokio::{net::TcpStream, task::AbortHandle};

const RESPONSE_HEAD_TIMEOUT: Duration = Duration::from_secs(300);

pub struct PluginWebGateway {
    routes: RwLock<Arc<LoadingWebRoutes>>,
    authority: HeaderValue,
}

impl PluginWebGateway {
    pub async fn open(runtime: &LoadingRuntime, public_origin: &str) -> Result<Self, ApiError> {
        let authority = parse_origin_authority(public_origin)?;
        let routes = runtime
            .open_web_routes(public_origin)
            .await
            .map_err(gateway_error)?;
        Ok(Self {
            routes: RwLock::new(Arc::new(routes)),
            authority,
        })
    }

    pub(crate) fn from_routes(
        routes: LoadingWebRoutes,
        public_origin: &str,
    ) -> Result<Self, ApiError> {
        let authority = parse_origin_authority(public_origin)?;
        Ok(Self {
            routes: RwLock::new(Arc::new(routes)),
            authority,
        })
    }

    pub(crate) fn retarget(&self, routes: LoadingWebRoutes) {
        *self.routes.write().expect("plugin gateway routes") = Arc::new(routes);
    }

    pub fn bind_page(&self, activation: &LoadingActivation) -> Result<PluginRoutePage, ApiError> {
        let routes = self.routes.read().expect("plugin gateway routes").clone();
        routes.bind_page(activation).map_err(|_| revoked())
    }

    pub fn bind_host_page(&self) -> Result<PluginRoutePage, ApiError> {
        let routes = self.routes.read().expect("plugin gateway routes").clone();
        routes.bind_host_page().map_err(|_| revoked())
    }

    /// Relay one request without retries, HTTP redirects, a system proxy or a
    /// reusable upstream connection. Page and call scopes stay bound to it.
    pub async fn forward(
        &self,
        page: &PluginRoutePage,
        request: Request<Body>,
    ) -> Result<Response<Body>, ApiError> {
        let routes = self.routes.read().expect("plugin gateway routes").clone();
        let lease = Arc::new(routes.admit(page).map_err(|_| revoked())?);
        let request = self.prepare_request(request, lease.clone())?;
        tokio::select! {
            biased;
            _ = lease.revoked() => Err(revoked()),
            result = tokio::time::timeout(RESPONSE_HEAD_TIMEOUT, exchange(lease.clone(), request)) => {
                result.map_err(|_| ApiError::Conflict("插件路由响应超时".into()))?
            }
        }
    }

    fn prepare_request(
        &self,
        mut request: Request<Body>,
        lease: Arc<PluginRouteLease>,
    ) -> Result<ForwardRequest, ApiError> {
        let target = request.uri();
        if target.scheme().is_some()
            || target.authority().is_some()
            || !target.path().starts_with('/')
            || target.path().starts_with("//")
            || target.path().contains('\\')
            || matches!(*request.method(), Method::CONNECT | Method::TRACE)
        {
            return Err(invalid("插件请求必须使用页面内有效路径"));
        }
        let upgrade = requested_upgrade(&mut request)?;
        // Extensions describe the browser-side connection; the private
        // request carries only its method, URI and admitted HTTP headers.
        let (mut parts, body) = request.into_parts();
        let session = parts.extensions.remove::<PluginRouteSession>();
        parts.extensions.clear();
        parts.version = Version::HTTP_11;
        filter_headers(&mut parts.headers);
        if let Some(session) = session {
            let identity = serde_json::to_vec(&session).map_err(gateway_error)?;
            if identity.len() > 8192 {
                return Err(invalid("插件会话身份超出传输上限"));
            }
            let header =
                HeaderValue::from_str(&URL_SAFE_NO_PAD.encode(identity)).map_err(gateway_error)?;
            parts.headers.insert("x-magi-route-session", header);
        }
        parts.headers.insert(header::HOST, self.authority.clone());
        set_connection(
            &mut parts.headers,
            upgrade.as_ref().map(|upgrade| &upgrade.protocol),
        );
        Ok(ForwardRequest {
            request: Request::from_parts(parts, RelayBody::new(body, lease, None)),
            upgrade,
        })
    }
}

struct ForwardRequest {
    request: Request<RelayBody<Body>>,
    upgrade: Option<UpgradeRequest>,
}

struct UpgradeRequest {
    protocol: HeaderValue,
    client: OnUpgrade,
}

async fn exchange(
    lease: Arc<PluginRouteLease>,
    mut upstream: ForwardRequest,
) -> Result<Response<Body>, ApiError> {
    let (address, proof) = lease.target().map_err(gateway_error)?;
    for (name, value) in proof {
        let mut value = HeaderValue::from_str(&value).map_err(gateway_error)?;
        value.set_sensitive(true);
        upstream
            .request
            .headers_mut()
            .insert(HeaderName::from_static(name), value);
    }
    let socket = TcpStream::connect(address).await.map_err(gateway_error)?;
    if upstream.upgrade.is_some() {
        // An abandoned upgraded connection must abort, even if the plugin
        // keeps its write half open after receiving a normal TCP FIN.
        socket.set_zero_linger().map_err(gateway_error)?;
    }
    let (mut sender, connection) = http1::handshake(TokioIo::new(socket))
        .await
        .map_err(gateway_error)?;
    let connection_lease = lease.clone();
    let driver = tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = connection_lease.revoked() => {},
            result = connection.with_upgrades() => if let Err(error) = result {
                tracing::debug!(%error, "插件私有 HTTP 连接结束");
            },
        }
    });
    let driver = ConnectionGuard(driver.abort_handle());
    lease.assert_current().map_err(|_| revoked())?;
    let mut response = sender
        .send_request(upstream.request)
        .await
        .map_err(gateway_error)?;
    lease.assert_current().map_err(|_| revoked())?;

    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
        let upgrade = upstream
            .upgrade
            .ok_or_else(|| gateway_error("宿主返回了未请求的协议升级"))?;
        if response
            .headers()
            .get(header::UPGRADE)
            .is_none_or(|protocol| {
                !protocol
                    .as_bytes()
                    .eq_ignore_ascii_case(upgrade.protocol.as_bytes())
            })
        {
            return Err(gateway_error("插件升级协议与请求不符"));
        }
        let private = hyper::upgrade::on(&mut response);
        let (mut parts, _) = response.into_parts();
        filter_headers(&mut parts.headers);
        set_connection(&mut parts.headers, Some(&upgrade.protocol));
        tokio::spawn(relay_upgrade(lease, upgrade.client, private, driver));
        return Ok(Response::from_parts(parts, Body::empty()));
    }

    let (mut parts, body) = response.into_parts();
    filter_headers(&mut parts.headers);
    parts
        .headers
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(Response::from_parts(
        parts,
        Body::new(RelayBody::new(body, lease, Some(driver))),
    ))
}

async fn relay_upgrade(
    lease: Arc<PluginRouteLease>,
    client: OnUpgrade,
    private: OnUpgrade,
    _driver: ConnectionGuard,
) {
    let (client, private) = tokio::select! {
        biased;
        _ = lease.revoked() => return,
        result = async { tokio::try_join!(client, private) } => match result {
            Ok(connections) => connections,
            Err(_) => return,
        },
    };
    let (mut client_reader, mut client_writer) = tokio::io::split(TokioIo::new(client));
    let (mut private_reader, mut private_writer) = tokio::io::split(TokioIo::new(private));
    // The page owns the upgraded connection. When either endpoint ends, drop
    // both ends instead of waiting for the remaining half of an orphaned socket.
    tokio::select! {
        biased;
        _ = lease.revoked() => {},
        _ = tokio::io::copy(&mut client_reader, &mut private_writer) => {},
        _ = tokio::io::copy(&mut private_reader, &mut client_writer) => {},
    }
}

struct ConnectionGuard(AbortHandle);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct RelayBody<B> {
    inner: B,
    lease: Arc<PluginRouteLease>,
    driver: Option<ConnectionGuard>,
    finished: bool,
}

impl<B> RelayBody<B> {
    fn new(inner: B, lease: Arc<PluginRouteLease>, driver: Option<ConnectionGuard>) -> Self {
        Self {
            inner,
            lease,
            driver,
            finished: false,
        }
    }
}

impl<B: HttpBody + Unpin> HttpBody for RelayBody<B>
where
    B::Error: Into<BoxError>,
{
    type Data = B::Data;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.finished {
            return Poll::Ready(None);
        }
        if self.lease.assert_current().is_err() {
            self.finished = true;
            self.driver.take();
            return Poll::Ready(Some(
                Err(std::io::Error::other("插件路由调用已撤销").into()),
            ));
        }
        let next = std::task::ready!(Pin::new(&mut self.inner).poll_frame(cx));
        if next.is_none() {
            self.finished = true;
        }
        Poll::Ready(next.map(|result| {
            result
                .map_err(Into::into)
                .map(|frame| match frame.into_trailers() {
                    Ok(mut trailers) => {
                        filter_headers(&mut trailers);
                        Frame::trailers(trailers)
                    }
                    Err(frame) => frame,
                })
        }))
    }

    fn is_end_stream(&self) -> bool {
        self.finished || self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

fn requested_upgrade(request: &mut Request<Body>) -> Result<Option<UpgradeRequest>, ApiError> {
    let Some(protocol) = request.headers().get(header::UPGRADE).cloned() else {
        return Ok(None);
    };
    if request.method() != Method::GET
        || !request.body().is_end_stream()
        || !connection_headers(request.headers()).any(|name| name.eq_ignore_ascii_case("upgrade"))
        || !protocol
            .to_str()
            .is_ok_and(|value| !value.is_empty() && !value.contains([',', ' ', '\t']))
    {
        return Err(invalid("插件协议升级请求无效"));
    }
    Ok(Some(UpgradeRequest {
        protocol,
        client: hyper::upgrade::on(request),
    }))
}

fn connection_headers(headers: &HeaderMap) -> impl Iterator<Item = &str> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(',').map(str::trim))
}

fn set_connection(headers: &mut HeaderMap, protocol: Option<&HeaderValue>) {
    if let Some(protocol) = protocol {
        headers.insert(header::UPGRADE, protocol.clone());
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    } else {
        headers.insert(header::CONNECTION, HeaderValue::from_static("close"));
    }
}

fn filter_headers(headers: &mut HeaderMap) {
    let nominated: Vec<_> = connection_headers(headers)
        .filter_map(|name| HeaderName::from_bytes(name.as_bytes()).ok())
        .collect();
    for name in nominated {
        headers.remove(name);
    }
    let private: Vec<_> = headers
        .keys()
        .filter(|name| {
            let name = name.as_str();
            matches!(
                name,
                "connection"
                    | "keep-alive"
                    | "transfer-encoding"
                    | "te"
                    | "trailer"
                    | "upgrade"
                    | "cookie"
                    | "set-cookie"
                    | "authorization"
                    | "forwarded"
                    | "alt-svc"
                    | "clear-site-data"
                    | "service-worker-allowed"
                    | "host"
            ) || name.starts_with("proxy-")
                || name.starts_with("x-forwarded-")
                || name.starts_with("x-magi-")
        })
        .cloned()
        .collect();
    for name in private {
        headers.remove(name);
    }
}

fn parse_origin_authority(public_origin: &str) -> Result<HeaderValue, ApiError> {
    let origin = public_origin
        .parse::<Uri>()
        .map_err(|_| invalid("插件网关地址无效"))?;
    let authority = origin
        .authority()
        .ok_or_else(|| invalid("插件网关缺少公开地址"))?;
    HeaderValue::from_str(authority.as_str()).map_err(|_| invalid("插件网关地址无效"))
}

fn invalid(message: &str) -> ApiError {
    ApiError::InvalidInput(message.into())
}
fn revoked() -> ApiError {
    ApiError::Conflict("插件页面的运行实例已失效，请重新打开页面".into())
}
fn gateway_error(error: impl std::fmt::Display) -> ApiError {
    tracing::debug!(%error, "插件私有路由传输失败");
    ApiError::InternalAssemblyError("插件路由通信失败".into())
}
