//! 插件 API 的发布物与目录下载：统一执行地址策略与有界 GET 恢复。
//!
//! 策略：只接受 HTTPS（本机回环地址允许 HTTP，用于开发与验收）；目标主机解析出的每个地址都必须
//! 是公网地址或回环地址——私网、链路本地（含云元数据地址）、未指定和组播地址一律拒绝；重定向由本模块
//! 逐跳手动跟随并对每一跳重新校验，且不允许从 HTTPS 降级到 HTTP；解析结果固定到连接上，避免校验后
//! DNS 重绑定到内网地址。需要内网来源的用户使用本地文件安装。

use super::*;
use std::net::{IpAddr, SocketAddr};

const MAX_REDIRECT_HOPS: usize = 3;
const MAX_ATTEMPTS: usize = 3;
const MIN_DOWNLOAD_BUDGET: Duration = Duration::from_secs(120);
// 按允许的发布物体积预留 128 KiB/s 的传输预算，避免 36 MiB 原包
// 与几 KiB 元数据共用总时限；仍受连续无数据、大小和尝试次数约束。
fn download_budget(limit: usize) -> Duration {
    MIN_DOWNLOAD_BUDGET.max(Duration::from_secs((limit as u64).div_ceil(128 * 1024)))
}
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(30);

// 仅重试未提交任何安装状态的 GET。每次丢弃不完整正文、从原地址重新校验；
// 不拼接无法证明属于同一版本的部分内容，也不重跑翻译、安装或激活。
enum DownloadFailure {
    Permanent(ApiError),
    Transient { message: String, delay: Duration },
}
impl From<ApiError> for DownloadFailure {
    fn from(error: ApiError) -> Self {
        Self::Permanent(error)
    }
}

fn transport_failure(what: &str, stage: &str, error: reqwest::Error) -> DownloadFailure {
    // 保留底层原因用于定位断流／代理／TLS 错误，去掉可能含凭据的请求 URL。
    let error = error.without_url();
    let message = format!("{stage}{what}失败: {error:#}");
    tracing::warn!(what, stage, error = ?error, "插件下载传输失败");
    DownloadFailure::Transient {
        message,
        delay: Duration::ZERO,
    }
}

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
async fn resolve_allowed(url: &reqwest::Url) -> Result<Vec<SocketAddr>, DownloadFailure> {
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
            .map_err(|error| DownloadFailure::Transient {
                message: format!("解析插件来源主机 {host} 失败: {error}"),
                delay: Duration::ZERO,
            })?
            .collect(),
    };
    if addresses.is_empty() || addresses.iter().any(|a| !address_allowed(a.ip())) {
        return Err(
            ApiError::InvalidInput(format!("拒绝访问 {host}：目标不是公网或本机回环地址")).into(),
        );
    }
    Ok(addresses)
}

pub(super) async fn download_bytes(
    url: &str,
    limit: usize,
    what: &str,
) -> Result<Vec<u8>, ApiError> {
    let source =
        reqwest::Url::parse(url).map_err(|_| ApiError::InvalidInput(format!("{what}地址无效")))?;
    check_url_shape(&source)?;
    let budget = download_budget(limit);
    let download = async {
        for attempt in 1..=MAX_ATTEMPTS {
            match download_attempt(&source, limit, what).await {
                Ok(bytes) => return Ok(bytes),
                Err(DownloadFailure::Permanent(error)) => return Err(error),
                Err(DownloadFailure::Transient { message, delay }) => {
                    if attempt == MAX_ATTEMPTS {
                        return Err(ApiError::InvalidInput(format!(
                            "{message}；已尝试 {attempt} 次，请检查网络后重试"
                        )));
                    }
                    tracing::warn!(what, attempt, message, "插件下载将在预算内重新获取");
                    let backoff = Duration::from_millis(250 * 4u64.pow(attempt as u32 - 1));
                    tokio::time::sleep(delay.max(backoff).min(budget)).await;
                }
            }
        }
        unreachable!("download attempts must be nonzero")
    };
    tokio::time::timeout(budget, download).await.map_err(|_| {
        ApiError::InvalidInput(format!(
            "获取{what}超过 {} 秒，已停止下载；请检查网络后重试",
            budget.as_secs()
        ))
    })?
}

async fn download_attempt(
    source: &reqwest::Url,
    limit: usize,
    what: &str,
) -> Result<Vec<u8>, DownloadFailure> {
    let mut current = source.clone();
    let mut hops = 0;
    let mut response = loop {
        let addresses = resolve_allowed(&current).await?;
        let mut builder = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
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
            .map_err(|error| transport_failure(what, "获取", error))?;
        // 304 不是可跟随跳转，也不能作为完整发布物成功返回。
        if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
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
            return Err(
                ApiError::InvalidInput(format!("获取{what}的重定向不允许从 HTTPS 降级")).into(),
            );
        }
        current = next;
    };
    let status = response.status();
    if matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504) {
        let delay = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or_default();
        return Err(DownloadFailure::Transient {
            message: format!("获取{what}返回 HTTP {status}"),
            delay,
        });
    }
    // 未发送 Range，不接受局部正文或空成功冒充整个包。
    if status != reqwest::StatusCode::OK {
        return Err(ApiError::InvalidInput(format!("获取{what}返回 HTTP {status}")).into());
    }
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(ApiError::InvalidInput(format!("{what}大小超限")).into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| transport_failure(what, "读取", error))?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(ApiError::InvalidInput(format!("{what}大小超限")).into());
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

    // 真实 TCP 断流：不能由 mock 的完整正文掩盖 reqwest 的流读取错误。
    async fn serve(responses: &[&str]) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let responses: Vec<_> = responses.iter().map(|value| value.to_string()).collect();
        let task = tokio::spawn(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0u8; 4096];
                let size = socket.read(&mut buffer).await.unwrap();
                assert!(buffer[..size].starts_with(b"GET /p.zip HTTP/1.1"));
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        (format!("http://{address}/p.zip"), task)
    }

    #[tokio::test]
    async fn redirects_are_revalidated_per_hop() {
        let (url, server) = serve(&[
            "HTTP/1.1 302 Found\r\nLocation: https://169.254.169.254/x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                ,
        ])
        .await;
        let error = download_bytes(&url, 1024, "插件包").await.unwrap_err();
        assert!(error.message().contains("拒绝访问"), "{}", error.message());
        server.await.unwrap();

        let (url, server) =
            serve(&["HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc"]).await;
        assert_eq!(download_bytes(&url, 1024, "插件包").await.unwrap(), b"abc");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn broken_body_is_discarded_before_retrying_the_whole_get() {
        let (url, server) = serve(&[
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nold",
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nnewer",
        ])
        .await;
        assert_eq!(download_bytes(&url, 5, "插件包").await.unwrap(), b"newer");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn temporary_http_failure_recovers_without_replaying_installation() {
        let (url, server) = serve(&[
            "HTTP/1.1 503 Unavailable\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc",
        ]).await;
        assert_eq!(download_bytes(&url, 3, "插件包").await.unwrap(), b"abc");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn retries_stop_at_the_limit_with_an_actionable_error() {
        let unavailable =
            "HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (url, server) = serve(&[unavailable; MAX_ATTEMPTS]).await;
        let error = download_bytes(&url, 3, "插件包").await.unwrap_err();
        assert!(error.message().contains("503"));
        assert!(error.message().contains("已尝试 3 次"));
        assert!(error.message().contains("重试"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn permanent_status_partial_response_and_size_limit_do_not_retry() {
        for (response, expected) in [
            (
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "404",
            ),
            (
                "HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc",
                "206",
            ),
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabcd",
                "大小超限",
            ),
            (
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nabcd\r\n0\r\n\r\n",
                "大小超限",
            ),
        ] {
            let (url, server) = serve(&[response]).await;
            let error = download_bytes(&url, 3, "插件包").await.unwrap_err();
            assert!(error.message().contains(expected), "{}", error.message());
            assert!(!error.message().contains("已尝试"));
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn retry_revalidates_redirects_and_stops_on_policy_failure() {
        let (url, server) = serve(&[
            "HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 302 Found\r\nLocation: https://169.254.169.254/x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ]).await;
        let error = download_bytes(&url, 3, "插件包").await.unwrap_err();
        assert!(error.message().contains("拒绝访问"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn retry_after_is_respected() {
        let (url, server) = serve(&[
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc",
        ]).await;
        let started = tokio::time::Instant::now();
        assert_eq!(download_bytes(&url, 3, "插件包").await.unwrap(), b"abc");
        assert!(started.elapsed() >= Duration::from_secs(1));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cancellation_during_backoff_does_not_leave_a_background_retry() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/p.zip", listener.local_addr().unwrap());
        let (sent, received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            socket.read(&mut request).await.unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            socket.shutdown().await.unwrap();
            sent.send(()).unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(600), listener.accept())
                    .await
                    .is_err()
            );
        });
        let request = tokio::spawn(async move { download_bytes(&url, 3, "插件包").await });
        received.await.unwrap();
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        server.await.unwrap();
    }
}
