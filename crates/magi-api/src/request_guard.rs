//! daemon 本机入口的请求守卫。
//!
//! daemon 监听在本机端口上，浏览器里任何网页都能向它发请求。没有守卫时：
//! - DNS 重绑定：攻击者域名在第二次解析时指向 127.0.0.1，页面就成了「同源」，可以调用审批、
//!   发起会话等全部接口；
//! - 跨站写请求：页面向本机接口发 POST；
//! - HTML 预览与 API 同源：预览里的脚本（可能来自模型生成或被注入的内容）可以直接调用同源接口。
//!
//! 守卫只做三件互相独立的事，公网隧道请求由隧道令牌另行认证，不在这里处理：
//! 1. `Host` 必须是本机、局域网、单标签主机名或显式允许的域名；
//! 2. 写请求如果带 `Origin`，必须与 `Host` 同源；
//! 3. 预览主机（`*.localhost` 子域）只能读取预览资源，不能访问任何 API 或工作台页面。

use axum::{
    Json,
    extract::Request,
    http::{HeaderMap, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use magi_browser_authority::{PREVIEW_HOSTNAME, PREVIEW_PATH_PREFIX};
use std::net::IpAddr;
use std::sync::LazyLock;

/// 额外允许的 `Host`（逗号分隔，条目以 `.` 开头表示后缀）。例如 `MAGI_ALLOWED_HOSTS=magi.example.com,.corp.example`。
const ALLOWED_HOSTS_ENV: &str = "MAGI_ALLOWED_HOSTS";

/// 常见的私有网络域名后缀：局域网设备名、Tailscale 与隧道域名。攻击者无法在这些后缀下注册可被重绑定的名字。
const TRUSTED_HOST_SUFFIXES: &[&str] = &[
    ".local",
    ".lan",
    ".home.arpa",
    ".internal",
    ".ts.net",
    ".trycloudflare.com",
];

static EXTRA_ALLOWED_HOSTS: LazyLock<Vec<String>> = LazyLock::new(|| {
    std::env::var(ALLOWED_HOSTS_ENV)
        .map(|value| parse_allowed_hosts(&value))
        .unwrap_or_default()
});

fn parse_allowed_hosts(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|entry| entry.trim().to_ascii_lowercase())
        .filter(|entry| !entry.is_empty())
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
enum GuardRejection {
    HostNotAllowed,
    OriginNotAllowed,
    PreviewHostRestricted,
}

impl GuardRejection {
    fn code(&self) -> &'static str {
        match self {
            Self::HostNotAllowed => "HOST_NOT_ALLOWED",
            Self::OriginNotAllowed => "ORIGIN_NOT_ALLOWED",
            Self::PreviewHostRestricted => "PREVIEW_HOST_RESTRICTED",
        }
    }

    fn message(&self) -> &'static str {
        match self {
            Self::HostNotAllowed => {
                "不允许通过这个主机名访问 Magi；如需使用自定义域名，请在 MAGI_ALLOWED_HOSTS 中声明"
            }
            Self::OriginNotAllowed => "不允许跨来源的写请求",
            Self::PreviewHostRestricted => "预览来源只能读取网页预览资源",
        }
    }
}

pub(crate) async fn enforce_request_guard(request: Request, next: Next) -> Response {
    match check_request(
        request.method(),
        request.uri().path(),
        request.headers(),
        &EXTRA_ALLOWED_HOSTS,
    ) {
        Ok(()) => next.run(request).await,
        Err(rejection) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error_code": rejection.code(),
                "message": rejection.message(),
            })),
        )
            .into_response(),
    }
}

fn check_request(
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    extra_allowed_hosts: &[String],
) -> Result<(), GuardRejection> {
    if crate::routes::is_public_tunnel_request(headers) {
        return Ok(());
    }
    let host_header = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    let hostname = host_header.as_deref().map(hostname_of);

    if let Some(hostname) = hostname
        && !host_allowed(hostname, extra_allowed_hosts)
    {
        return Err(GuardRejection::HostNotAllowed);
    }

    if hostname == Some(PREVIEW_HOSTNAME) {
        let read_only = matches!(*method, Method::GET | Method::HEAD);
        if !read_only || !path.starts_with(PREVIEW_PATH_PREFIX) {
            return Err(GuardRejection::PreviewHostRestricted);
        }
        return Ok(());
    }

    let is_write = !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    if is_write
        && let Some(origin) = headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
    {
        let origin_authority = origin
            .split_once("://")
            .map(|(_, authority)| authority.trim_end_matches('/').to_ascii_lowercase());
        if origin_authority.as_deref() != host_header.as_deref() {
            return Err(GuardRejection::OriginNotAllowed);
        }
    }
    Ok(())
}

/// 本机访问时，网页预览改在独立来源（`PREVIEW_HOSTNAME`）上提供，与 API 不同源。
/// 非本机访问（局域网、公网隧道）无法解析该主机名，仍使用当前来源。
pub(crate) fn isolated_preview_origin(headers: &HeaderMap) -> Option<String> {
    if crate::routes::is_public_tunnel_request(headers) {
        return None;
    }
    let authority = headers
        .get(header::HOST)?
        .to_str()
        .ok()?
        .trim()
        .to_ascii_lowercase();
    let hostname = hostname_of(&authority);
    let is_loopback = hostname == "localhost"
        || hostname == "[::1]"
        || hostname
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback());
    if !is_loopback {
        return None;
    }
    let port = authority.strip_prefix(hostname).unwrap_or_default();
    Some(format!("http://{PREVIEW_HOSTNAME}{port}"))
}

/// 去掉端口后的主机名；IPv6 字面量保留方括号。
fn hostname_of(authority: &str) -> &str {
    if authority.starts_with('[') {
        return authority
            .find(']')
            .map_or(authority, |end| &authority[..=end]);
    }
    authority
        .rsplit_once(':')
        .map_or(authority, |(host, port)| {
            if port.chars().all(|c| c.is_ascii_digit()) {
                host
            } else {
                authority
            }
        })
}

fn host_allowed(hostname: &str, extra_allowed_hosts: &[String]) -> bool {
    let hostname = hostname.trim_end_matches('.');
    if hostname.starts_with('[') && hostname.ends_with(']') {
        return hostname[1..hostname.len() - 1].parse::<IpAddr>().is_ok();
    }
    if hostname.parse::<IpAddr>().is_ok() {
        return true;
    }
    if hostname == "localhost" || hostname.ends_with(".localhost") {
        return true;
    }
    // 单标签主机名（局域网里的设备名）不可能是公网域名。
    if !hostname.contains('.') {
        return true;
    }
    if TRUSTED_HOST_SUFFIXES
        .iter()
        .any(|suffix| hostname.ends_with(suffix))
    {
        return true;
    }
    extra_allowed_hosts.iter().any(|allowed| {
        if let Some(suffix) = allowed.strip_prefix('.') {
            hostname == suffix || hostname.ends_with(allowed.as_str())
        } else {
            hostname == allowed
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                header::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
                HeaderValue::from_str(value).expect("header value"),
            );
        }
        map
    }

    fn check(method: Method, path: &str, pairs: &[(&str, &str)]) -> Result<(), GuardRejection> {
        check_request(&method, path, &headers(pairs), &[])
    }

    #[test]
    fn local_and_lan_hosts_are_allowed() {
        for host in [
            "127.0.0.1:38123",
            "localhost:38123",
            "[::1]:38123",
            "192.168.1.20:38123",
            "my-mac:38123",
            "my-mac.local:38123",
            "box.tailnet-name.ts.net",
            "abc.trycloudflare.com",
            "app.localhost:38123",
        ] {
            assert_eq!(
                check(Method::GET, "/api/x", &[("host", host)]),
                Ok(()),
                "{host}"
            );
        }
    }

    #[test]
    fn rebinding_hostnames_are_rejected_unless_declared() {
        for host in [
            "evil.example.com:38123",
            "attacker.io",
            "127.0.0.1.evil.com:38123",
        ] {
            assert_eq!(
                check(Method::GET, "/api/x", &[("host", host)]),
                Err(GuardRejection::HostNotAllowed),
                "{host}"
            );
        }
        let extra = parse_allowed_hosts("magi.example.com, .corp.example");
        let allowed =
            |host: &str| check_request(&Method::GET, "/", &headers(&[("host", host)]), &extra);
        assert_eq!(allowed("magi.example.com:38123"), Ok(()));
        assert_eq!(allowed("a.corp.example"), Ok(()));
        assert_eq!(allowed("corp.example"), Ok(()));
        assert_eq!(
            allowed("other.example.com"),
            Err(GuardRejection::HostNotAllowed)
        );
    }

    #[test]
    fn missing_host_is_not_a_browser_request() {
        assert_eq!(check(Method::POST, "/api/x", &[]), Ok(()));
    }

    #[test]
    fn writes_require_same_origin_when_origin_is_present() {
        let ok = [
            ("host", "127.0.0.1:38123"),
            ("origin", "http://127.0.0.1:38123"),
        ];
        assert_eq!(check(Method::POST, "/api/session/turn", &ok), Ok(()));
        for origin in [
            "http://evil.example.com",
            "http://site.localhost:38123",
            "http://localhost:38123",
            "null",
        ] {
            assert_eq!(
                check(
                    Method::POST,
                    "/api/session/tool-approval",
                    &[("host", "127.0.0.1:38123"), ("origin", origin)]
                ),
                Err(GuardRejection::OriginNotAllowed),
                "{origin}"
            );
        }
        // 没有 Origin 的写请求来自非浏览器客户端，放行；读请求不检查 Origin。
        assert_eq!(
            check(Method::POST, "/api/x", &[("host", "127.0.0.1:38123")]),
            Ok(())
        );
        assert_eq!(
            check(
                Method::GET,
                "/api/x",
                &[
                    ("host", "127.0.0.1:38123"),
                    ("origin", "http://evil.example.com")
                ]
            ),
            Ok(())
        );
    }

    #[test]
    fn preview_host_only_serves_preview_assets() {
        let host = [("host", "site.localhost:38123")];
        assert_eq!(
            check(Method::GET, "/api/files/site/t/ws/index.html", &host),
            Ok(())
        );
        assert_eq!(
            check(Method::HEAD, "/api/files/site/t/ws/a.js", &host),
            Ok(())
        );
        for (method, path) in [
            (Method::GET, "/api/session/tool-approvals"),
            (Method::GET, "/web.html"),
            (Method::GET, "/api/files/site-open"),
            (Method::POST, "/api/files/site/t/ws/index.html"),
            (Method::POST, "/api/session/tool-approval"),
        ] {
            assert_eq!(
                check(method.clone(), path, &host),
                Err(GuardRejection::PreviewHostRestricted),
                "{method} {path}"
            );
        }
    }

    #[test]
    fn local_requests_get_an_isolated_preview_origin() {
        let origin = |pairs: &[(&str, &str)]| isolated_preview_origin(&headers(pairs));
        assert_eq!(
            origin(&[("host", "127.0.0.1:38123")]).as_deref(),
            Some("http://site.localhost:38123")
        );
        assert_eq!(
            origin(&[("host", "localhost:38123")]).as_deref(),
            Some("http://site.localhost:38123")
        );
        assert_eq!(
            origin(&[("host", "[::1]:38123")]).as_deref(),
            Some("http://site.localhost:38123")
        );
        assert_eq!(origin(&[("host", "192.168.1.20:38123")]), None);
        assert_eq!(
            origin(&[("host", "localhost:38123"), ("cf-ray", "abc")]),
            None
        );
        assert_eq!(origin(&[]), None);
    }

    #[test]
    fn public_tunnel_requests_are_left_to_tunnel_auth() {
        assert_eq!(
            check(
                Method::POST,
                "/api/x",
                &[
                    ("host", "localhost:38123"),
                    ("cf-ray", "abc"),
                    ("origin", "https://x.trycloudflare.com"),
                ]
            ),
            Ok(())
        );
    }
}
