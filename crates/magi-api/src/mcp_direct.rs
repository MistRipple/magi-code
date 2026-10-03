//! 直接对公网开放（监听公网地址，不经隧道）的输入校验。
//!
//! 监听地址是 IP（`0.0.0.0` 表示所有网卡）；公网主机名是客户端实际使用的域名或 IP，
//! 它同时是 HTTP 入口 `Host` 白名单的内容（防 DNS 重绑定），所以必须规整成不含端口和路径的形态。

use std::net::{IpAddr, Ipv6Addr};

/// 同时登记的公网主机名上限。
pub(crate) const MAX_PUBLIC_HOSTS: usize = 8;

pub(crate) fn parse_bind_host(input: &str) -> Result<IpAddr, String> {
    input
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .map_err(|_| "监听地址必须是 IP，例如 0.0.0.0（所有网卡）或某块网卡的 IP".to_string())
}

/// 规整一个公网主机名：去掉协议、路径和端口，域名转小写，IPv6 用方括号。
pub(crate) fn normalize_public_host(input: &str) -> Result<String, String> {
    let trimmed = input.trim();
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .trim();
    let host = if let Some(inner) = authority.strip_prefix('[') {
        inner.split(']').next().unwrap_or_default().to_string()
    } else if authority.matches(':').count() == 1 {
        authority.split(':').next().unwrap_or_default().to_string()
    } else {
        authority.to_string()
    };
    if host.is_empty() {
        return Err("请填写客户端用来访问的域名或 IP".to_string());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
            return Err(
                "公网地址不能是 0.0.0.0、回环或组播地址，请填写客户端实际使用的 IP 或域名"
                    .to_string(),
            );
        }
        return Ok(match ip {
            IpAddr::V6(v6) => format!("[{}]", Ipv6Addr::to_string(&v6)),
            IpAddr::V4(v4) => v4.to_string(),
        });
    }
    let lower = host.trim_end_matches('.').to_ascii_lowercase();
    let valid_label = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    };
    if lower == "localhost" || lower.len() > 253 || !lower.split('.').all(valid_label) {
        return Err("域名格式不正确，请填写例如 mcp.example.com".to_string());
    }
    Ok(lower)
}

pub(crate) fn normalize_public_hosts(inputs: &[String]) -> Result<Vec<String>, String> {
    let mut hosts: Vec<String> = Vec::new();
    for input in inputs.iter().filter(|input| !input.trim().is_empty()) {
        let host = normalize_public_host(input)?;
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    if hosts.is_empty() {
        return Err("请至少填写一个客户端用来访问的域名或 IP".to_string());
    }
    if hosts.len() > MAX_PUBLIC_HOSTS {
        return Err(format!("最多登记 {MAX_PUBLIC_HOSTS} 个公网地址"));
    }
    Ok(hosts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_hosts_must_be_ip_addresses() {
        assert_eq!(parse_bind_host("0.0.0.0").unwrap().to_string(), "0.0.0.0");
        assert_eq!(parse_bind_host(" [::] ").unwrap().to_string(), "::");
        assert_eq!(
            parse_bind_host("192.168.1.8").unwrap().to_string(),
            "192.168.1.8"
        );
        for bad in ["", "example.com", "0.0.0", "1.2.3.4:80"] {
            assert!(parse_bind_host(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn public_hosts_are_normalised_to_bare_host_names() {
        for (input, expected) in [
            ("203.0.113.5", "203.0.113.5"),
            ("203.0.113.5:8765", "203.0.113.5"),
            ("http://203.0.113.5:8765/mcp", "203.0.113.5"),
            ("https://MCP.Example.com/", "mcp.example.com"),
            ("mcp.example.com:443", "mcp.example.com"),
            ("[2001:db8::1]:8765", "[2001:db8::1]"),
            ("2001:db8::1", "[2001:db8::1]"),
        ] {
            assert_eq!(normalize_public_host(input).unwrap(), expected, "{input}");
        }
        for bad in [
            "",
            "0.0.0.0",
            "127.0.0.1",
            "::1",
            "localhost",
            "a_b.example.com",
            "-x.com",
            "https:///x",
        ] {
            assert!(normalize_public_host(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn at_least_one_host_is_required_and_duplicates_collapse() {
        assert!(normalize_public_hosts(&[]).is_err());
        assert!(normalize_public_hosts(&["  ".to_string()]).is_err());
        let hosts = normalize_public_hosts(&[
            "203.0.113.5".to_string(),
            "http://203.0.113.5:80".to_string(),
            "mcp.example.com".to_string(),
        ])
        .unwrap();
        assert_eq!(hosts, ["203.0.113.5", "mcp.example.com"]);
        let many: Vec<String> = (0..9).map(|i| format!("h{i}.example.com")).collect();
        assert!(normalize_public_hosts(&many).is_err());
    }
}
