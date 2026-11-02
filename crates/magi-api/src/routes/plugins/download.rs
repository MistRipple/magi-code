//! 插件与目录的远程下载：唯一的出站入口，统一执行地址策略。
//!
//! 策略：只接受 HTTPS（本机回环地址允许 HTTP，用于开发与验收）；目标主机解析出的每个地址都必须
//! 是公网地址或回环地址——私网、链路本地（含云元数据地址）、未指定和组播地址一律拒绝；重定向由本模块
//! 逐跳手动跟随并对每一跳重新校验，且不允许从 HTTPS 降级到 HTTP；解析结果固定到连接上，避免校验后
//! DNS 重绑定到内网地址。需要内网来源的用户使用本地文件安装。

use super::*;
use std::net::{IpAddr, SocketAddr};

const MAX_REDIRECT_HOPS: usize = 3;

pub(super) fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// 协议与主机形态校验（不发起网络访问）。
pub(super) fn check_url_shape(url: &reqwest::Url) -> Result<(), ApiError> {
    let Some(host) = url.host_str() else {
        return Err(ApiError::InvalidInput(
            "插件来源只支持带主机的 HTTPS 地址".into(),
        ));
    };
    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_host(host) => Ok(()),
        _ => Err(ApiError::InvalidInput(
            "插件来源只支持 HTTPS 地址（本机回环地址除外）".into(),
        )),
    }
}

/// 地址是否允许作为出站目标：公网或回环。
pub(super) fn address_allowed(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => {
            if v4.is_loopback() {
                return true;
            }
            let o = v4.octets();
            !(v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1])) // 100.64.0.0/10 共享地址
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return address_allowed(IpAddr::V4(mapped));
            }
            if v6.is_loopback() {
                return true;
            }
            let first = v6.segments()[0];
            !(v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00 // fc00::/7 唯一本地
                || (first & 0xffc0) == 0xfe80) // fe80::/10 链路本地
        }
    }
}

/// 解析并校验目标主机，返回应固定到连接上的地址集合。
async fn resolve_allowed(url: &reqwest::Url) -> Result<Vec<SocketAddr>, ApiError> {
    check_url_shape(url)?;
    let host = url.host_str().unwrap_or_default().to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses: Vec<SocketAddr> = match host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|error| ApiError::InvalidInput(format!("解析 {host} 失败: {error}")))?
            .collect(),
    };
    if addresses.is_empty() || addresses.iter().any(|a| !address_allowed(a.ip())) {
        return Err(ApiError::InvalidInput(format!(
            "拒绝访问 {host}：目标不是公网或本机回环地址"
        )));
    }
    Ok(addresses)
}

pub(super) async fn download_bytes(
    url: &str,
    limit: usize,
    what: &str,
) -> Result<Vec<u8>, ApiError> {
    let mut current =
        reqwest::Url::parse(url).map_err(|_| ApiError::InvalidInput(format!("{what}地址无效")))?;
    let mut hops = 0;
    let mut response = loop {
        let addresses = resolve_allowed(&current).await?;
        let mut builder = reqwest::Client::builder()
            .timeout(PLUGIN_DOWNLOAD_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none());
        if let Some(host) = current.host_str()
            && host.parse::<IpAddr>().is_err()
        {
            builder = builder.resolve_to_addrs(host, &addresses);
        }
        let client = builder.build().map_err(|error| {
            ApiError::internal_assembly(&format!("获取{what}失败"), error.to_string())
        })?;
        let response = client
            .get(current.clone())
            .send()
            .await
            .map_err(|error| ApiError::InvalidInput(format!("获取{what}失败: {error}")))?;
        if !response.status().is_redirection() {
            break response;
        }
        hops += 1;
        let next = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|location| current.join(location).ok())
            .filter(|_| hops <= MAX_REDIRECT_HOPS)
            .ok_or_else(|| ApiError::InvalidInput(format!("获取{what}的重定向无效或过多")))?;
        if current.scheme() == "https" && next.scheme() != "https" {
            return Err(ApiError::InvalidInput(format!(
                "获取{what}的重定向不允许从 HTTPS 降级"
            )));
        }
        current = next;
    };
    if !response.status().is_success() {
        return Err(ApiError::InvalidInput(format!(
            "获取{what}返回 HTTP {}",
            response.status()
        )));
    }
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(ApiError::InvalidInput(format!("{what}大小超限")));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| ApiError::InvalidInput(format!("读取{what}失败: {error}")))?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(ApiError::InvalidInput(format!("{what}大小超限")));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    #[test]
    fn only_public_and_loopback_addresses_are_allowed() {
        for allowed in ["8.8.8.8", "127.0.0.1", "::1", "2606:4700::1111"] {
            assert!(address_allowed(ip(allowed)), "{allowed}");
        }
        for denied in [
            "10.0.0.1",
            "172.16.5.4",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "fd00::1",
            "fe80::1",
            "::",
            "::ffff:10.0.0.1",
        ] {
            assert!(!address_allowed(ip(denied)), "{denied}");
        }
    }

    #[test]
    fn plain_http_is_limited_to_loopback_hosts() {
        let check = |url: &str| check_url_shape(&reqwest::Url::parse(url).unwrap());
        assert!(check("https://plugins.example/p.zip").is_ok());
        assert!(check("http://127.0.0.1:8080/p.zip").is_ok());
        assert!(check("http://localhost/p.zip").is_ok());
        assert!(check("http://plugins.example/p.zip").is_err());
        assert!(check("ftp://plugins.example/p.zip").is_err());
    }

    #[tokio::test]
    async fn private_and_metadata_targets_are_refused_before_any_request() {
        for url in [
            "https://169.254.169.254/latest/meta-data",
            "https://10.1.2.3/p.zip",
            "https://[fd00::1]/p.zip",
        ] {
            let error = download_bytes(url, 1024, "插件包").await.unwrap_err();
            assert!(
                error.message().contains("拒绝访问"),
                "{url}: {}",
                error.message()
            );
        }
    }

    async fn serve_once(response: String) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 1024];
            let _ = socket.read(&mut buffer).await;
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{address}/p.zip")
    }

    #[tokio::test]
    async fn redirects_are_revalidated_per_hop() {
        let url = serve_once(
            "HTTP/1.1 302 Found\r\nLocation: https://169.254.169.254/x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into(),
        )
        .await;
        let error = download_bytes(&url, 1024, "插件包").await.unwrap_err();
        assert!(error.message().contains("拒绝访问"), "{}", error.message());

        let url = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc".into(),
        )
        .await;
        assert_eq!(download_bytes(&url, 1024, "插件包").await.unwrap(), b"abc");
    }
}
