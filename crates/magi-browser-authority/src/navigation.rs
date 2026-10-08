use crate::browser_network_policy as policy;
use std::net::IpAddr;
use std::sync::LazyLock;
use url::{Host, Url};

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BrowserNavigationUrlError {
    #[error("browser navigation URL must use http or https, or be about:blank")]
    UnsupportedScheme,
    #[error("browser navigation URL is invalid")]
    InvalidUrl,
    #[error("browser navigation URL must not contain user credentials")]
    UserCredentialsNotAllowed,
    #[error("browser navigation URL targets a blocked network endpoint")]
    BlockedNetworkTarget,
    #[error(
        "browser navigation URL targets a local-network address; enable LAN access in Settings → Browser to allow it"
    )]
    LanAccessDisabled,
}

/// 将 Browser Host 的页面状态收敛到可恢复的 URL 边界。
///
/// Chromium 在网络失败或内部错误页时可能返回 `chrome-error://` 等内部
/// scheme。它们只代表当前渲染失败，不能作为下一次恢复导航的输入；统一
/// 降级到空白页，避免重启后反复恢复同一个无效 URL。
pub fn normalize_browser_page_state(
    url: String,
    origin: Option<String>,
    title: String,
) -> (String, Option<String>, String) {
    if validate_browser_navigation_url(&url).is_ok() {
        return (url, origin, title);
    }
    ("about:blank".to_string(), None, String::new())
}

pub fn browser_navigation_origin(raw_url: &str) -> Option<String> {
    let url = Url::parse(raw_url.trim()).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.origin().ascii_serialization())
}

/// 校验所有进入 Browser Host 的 URL。浏览器不做逐 Origin 授权，但固定阻止危险
/// scheme、URL 凭据和云元数据 / 链路本地地址，避免本机网络边界被网页导航绕过。
/// 规则来自 `contracts/desktop-browser/network-policy.json`，Electron Main 使用同一份。
pub fn validate_browser_navigation_url(raw_url: &str) -> Result<(), BrowserNavigationUrlError> {
    validate_browser_navigation_url_with(raw_url, true)
}

/// 同 [`validate_browser_navigation_url`]，并按设置决定是否允许局域网私有网段。
/// 本机回环地址始终允许（开发服务器），Magi 自身端口由 Electron Main 在网络层拦截。
pub fn validate_browser_navigation_url_with(
    raw_url: &str,
    allow_lan_access: bool,
) -> Result<(), BrowserNavigationUrlError> {
    let url = Url::parse(raw_url.trim()).map_err(|_| BrowserNavigationUrlError::InvalidUrl)?;
    if url.as_str() == "about:blank" {
        return Ok(());
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err(BrowserNavigationUrlError::UnsupportedScheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(BrowserNavigationUrlError::UserCredentialsNotAllowed);
    }
    match classify_host(url.host().ok_or(BrowserNavigationUrlError::InvalidUrl)?) {
        HostClass::Blocked => Err(BrowserNavigationUrlError::BlockedNetworkTarget),
        HostClass::Lan if !allow_lan_access => Err(BrowserNavigationUrlError::LanAccessDisabled),
        HostClass::Loopback | HostClass::Lan | HostClass::Public => Ok(()),
    }
}

/// 目标主机的网络归类。回环（本机开发服务器）始终允许，局域网私有网段由设置决定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostClass {
    Blocked,
    Loopback,
    Lan,
    Public,
}

#[derive(Clone, Copy)]
struct Cidr {
    network: IpAddr,
    prefix: u32,
}

impl Cidr {
    fn parse(value: &str) -> Cidr {
        let (address, prefix) = value
            .split_once('/')
            .expect("generated CIDR must contain a prefix");
        Cidr {
            network: address
                .parse()
                .expect("generated CIDR must contain a valid address"),
            prefix: prefix
                .parse()
                .expect("generated CIDR must contain a valid prefix"),
        }
    }

    fn contains(&self, ip: IpAddr) -> bool {
        match (self.network, ip) {
            (IpAddr::V4(network), IpAddr::V4(ip)) => prefix_matches(
                u128::from(u32::from(network)),
                u128::from(u32::from(ip)),
                32,
                self.prefix,
            ),
            (IpAddr::V6(network), IpAddr::V6(ip)) => {
                prefix_matches(u128::from(network), u128::from(ip), 128, self.prefix)
            }
            _ => false,
        }
    }
}

fn prefix_matches(network: u128, ip: u128, bits: u32, prefix: u32) -> bool {
    if prefix == 0 {
        return true;
    }
    let shift = bits - prefix;
    (network >> shift) == (ip >> shift)
}

fn parse_cidrs(values: &[&str]) -> Vec<Cidr> {
    values.iter().map(|value| Cidr::parse(value)).collect()
}

static BLOCKED_CIDRS: LazyLock<Vec<Cidr>> = LazyLock::new(|| parse_cidrs(policy::BLOCKED_CIDRS));
static LOOPBACK_CIDRS: LazyLock<Vec<Cidr>> = LazyLock::new(|| parse_cidrs(policy::LOOPBACK_CIDRS));
static LAN_CIDRS: LazyLock<Vec<Cidr>> = LazyLock::new(|| parse_cidrs(policy::LAN_CIDRS));

fn hostname_matches(host: &str, names: &[&str]) -> bool {
    names.iter().any(|name| {
        host == *name
            || host
                .strip_suffix(name)
                .is_some_and(|rest| rest.ends_with('.'))
    })
}

fn classify_host(host: Host<&str>) -> HostClass {
    match host {
        Host::Domain(domain) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            if hostname_matches(&domain, policy::BLOCKED_HOSTNAMES) {
                HostClass::Blocked
            } else if hostname_matches(&domain, policy::LOOPBACK_HOSTNAMES) {
                HostClass::Loopback
            } else {
                HostClass::Public
            }
        }
        Host::Ipv4(ip) => classify_ip(IpAddr::V4(ip)),
        Host::Ipv6(ip) => classify_ip(IpAddr::V6(ip)),
    }
}

fn classify_ip(ip: IpAddr) -> HostClass {
    // IPv4 映射的 IPv6 地址（::ffff:a.b.c.d）与对应的 IPv4 地址是同一个目标。
    let mapped = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4),
        IpAddr::V4(_) => None,
    };
    let candidates = [Some(ip), mapped];
    let any = |cidrs: &[Cidr]| {
        candidates
            .iter()
            .flatten()
            .any(|candidate| cidrs.iter().any(|cidr| cidr.contains(*candidate)))
    };
    if any(&BLOCKED_CIDRS) {
        HostClass::Blocked
    } else if any(&LOOPBACK_CIDRS) {
        HostClass::Loopback
    } else if any(&LAN_CIDRS) {
        HostClass::Lan
    } else {
        HostClass::Public
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BrowserNavigationUrlError, HostClass, classify_host, validate_browser_navigation_url,
        validate_browser_navigation_url_with,
    };
    use url::Url;

    /// 与 Electron Main 共用的规则与用例，见 contracts/desktop-browser/network-policy.json。
    fn vectors(name: &str) -> Vec<String> {
        let source: serde_json::Value = serde_json::from_str(include_str!(
            "../../../contracts/desktop-browser/network-policy.json"
        ))
        .expect("network policy source must be valid JSON");
        source["vectors"][name]
            .as_array()
            .unwrap_or_else(|| panic!("vectors.{name} must be an array"))
            .iter()
            .map(|value| value.as_str().expect("vector must be a string").to_string())
            .collect()
    }

    fn class_of(raw: &str) -> HostClass {
        let url = Url::parse(raw).unwrap_or_else(|_| panic!("{raw}"));
        classify_host(url.host().unwrap_or_else(|| panic!("{raw}")))
    }

    #[test]
    fn shared_vectors_always_blocked_targets_are_rejected() {
        for url in vectors("alwaysBlocked") {
            assert_eq!(
                validate_browser_navigation_url(&url),
                Err(BrowserNavigationUrlError::BlockedNetworkTarget),
                "{url}"
            );
        }
    }

    #[test]
    fn shared_vectors_allowed_targets_are_accepted() {
        for url in vectors("allowed") {
            assert!(validate_browser_navigation_url(&url).is_ok(), "{url}");
        }
    }

    #[test]
    fn shared_vectors_classify_loopback_lan_and_public_targets() {
        for url in vectors("loopback") {
            assert_eq!(class_of(&url), HostClass::Loopback, "{url}");
            assert!(
                validate_browser_navigation_url_with(&url, false).is_ok(),
                "{url}"
            );
        }
        for url in vectors("lan") {
            assert_eq!(class_of(&url), HostClass::Lan, "{url}");
            assert!(
                validate_browser_navigation_url_with(&url, true).is_ok(),
                "{url}"
            );
            assert_eq!(
                validate_browser_navigation_url_with(&url, false),
                Err(BrowserNavigationUrlError::LanAccessDisabled),
                "{url}"
            );
        }
        for url in vectors("public") {
            assert_eq!(class_of(&url), HostClass::Public, "{url}");
            assert!(
                validate_browser_navigation_url_with(&url, false).is_ok(),
                "{url}"
            );
        }
    }

    #[test]
    fn accepts_http_https_and_about_blank() {
        for url in [
            "about:blank",
            "https://example.com/path",
            "http://127.0.0.1:38123/web.html",
        ] {
            assert!(validate_browser_navigation_url(url).is_ok(), "{url}");
        }
    }

    #[test]
    fn rejects_dangerous_schemes() {
        for url in [
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "file:///etc/passwd",
            "chrome://settings",
        ] {
            assert_eq!(
                validate_browser_navigation_url(url),
                Err(BrowserNavigationUrlError::UnsupportedScheme),
                "{url}"
            );
        }
    }

    #[test]
    fn rejects_credentials_and_malformed_urls() {
        assert_eq!(
            validate_browser_navigation_url("https://user:password@example.com"),
            Err(BrowserNavigationUrlError::UserCredentialsNotAllowed)
        );
        assert_eq!(
            validate_browser_navigation_url("https://"),
            Err(BrowserNavigationUrlError::InvalidUrl)
        );
    }
}
