//! MCP 网络模式的隧道（cloudflared）：默认 Quick Tunnel（无需账号，**公网地址每次启动都会变**）；
//! 用户配置了命名隧道（自己的 Cloudflare 账号 + 域名）时改用固定域名，重启后地址不变。
//!
//! 复用 `TunnelManager` 的 cloudflared 检测 / 安装 / 启动 / 监控能力，但使用**独立实例**，
//! 只转发到 MCP 服务的回环端口，与 Magi 自己的“远程访问”隧道（转发到主应用端口）互不相干，
//! 也就不会把主应用暴露给 MCP 令牌持有者。
//!
//! 通过 [`TunnelProvider`] 抽象，运行时和测试不依赖真实的 cloudflared。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::tunnel::{RemoteAccessBinding, TunnelManager};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 隧道当前状态的快照。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TunnelSnapshot {
    /// `stopped` / `installing` / `starting` / `running` / `error`
    pub status: String,
    pub public_url: Option<String>,
    pub error: Option<String>,
}

/// 用户自己的 Cloudflare 命名隧道：固定域名 + 隧道令牌（只能运行这一条隧道）。
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NamedTunnel {
    pub hostname: String,
    pub token: String,
}

impl std::fmt::Debug for NamedTunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedTunnel")
            .field("hostname", &self.hostname)
            .field("token", &"<redacted>")
            .finish()
    }
}

pub(crate) trait TunnelProvider: Send + Sync {
    /// 开始把公网流量转发到 `127.0.0.1:port`。不等待就绪，调用方通过 `snapshot` 观察。
    /// `named` 为空时用 Quick Tunnel（随机地址），否则用命名隧道（固定域名）。
    fn start<'a>(&'a self, port: u16, named: Option<NamedTunnel>) -> BoxFuture<'a, ()>;
    fn snapshot<'a>(&'a self) -> BoxFuture<'a, TunnelSnapshot>;
    fn stop<'a>(&'a self) -> BoxFuture<'a, ()>;
}

/// cloudflared 隧道（Quick Tunnel 或命名隧道）。
#[derive(Default)]
pub(crate) struct QuickTunnelProvider {
    manager: Mutex<Option<TunnelManager>>,
}

impl QuickTunnelProvider {
    pub(crate) fn shared() -> Arc<dyn TunnelProvider> {
        Arc::new(Self::default())
    }
}

impl TunnelProvider for QuickTunnelProvider {
    fn start<'a>(&'a self, port: u16, named: Option<NamedTunnel>) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let mut slot = self.manager.lock().await;
            if let Some(previous) = slot.take() {
                previous.stop().await;
            }
            let manager = TunnelManager::new(port);
            match named {
                Some(named) => {
                    manager.start_named(&named.hostname, &named.token).await;
                }
                None => {
                    // MCP 入口自己用 Bearer 令牌鉴权，不使用远程访问那套 tunnel_token。
                    manager.start(RemoteAccessBinding::default()).await;
                }
            }
            *slot = Some(manager);
        })
    }

    fn snapshot<'a>(&'a self) -> BoxFuture<'a, TunnelSnapshot> {
        Box::pin(async move {
            let slot = self.manager.lock().await;
            match slot.as_ref() {
                None => TunnelSnapshot {
                    status: "stopped".to_string(),
                    ..TunnelSnapshot::default()
                },
                Some(manager) => {
                    let state = manager.get_state().await;
                    TunnelSnapshot {
                        status: state.status,
                        public_url: state.public_url,
                        error: state.error,
                    }
                }
            }
        })
    }

    fn stop<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Some(manager) = self.manager.lock().await.take() {
                manager.stop().await;
            }
        })
    }
}

/// 校验并规整用户填写的命名隧道：域名可带 `https://` 与路径，令牌可以是整条
/// `cloudflared service install <令牌>` 命令（Cloudflare 控制台默认复制的就是它）。
pub(crate) fn parse_named_tunnel(hostname: &str, token: &str) -> Result<NamedTunnel, String> {
    let hostname = normalize_hostname(hostname)?;
    let token = extract_tunnel_token(token)?;
    Ok(NamedTunnel { hostname, token })
}

fn normalize_hostname(input: &str) -> Result<String, String> {
    let trimmed = input.trim();
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let host = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let valid_label = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    };
    if host.len() > 253 || !host.contains('.') || !host.split('.').all(valid_label) {
        return Err("域名格式不正确，请填写例如 mcp.example.com".to_string());
    }
    if host.ends_with(".trycloudflare.com") || host == "trycloudflare.com" {
        return Err("trycloudflare.com 是临时地址，命名隧道需要你自己的域名".to_string());
    }
    if host.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
        return Err("请填写域名，不能是 IP 地址".to_string());
    }
    Ok(host)
}

fn extract_tunnel_token(input: &str) -> Result<String, String> {
    use base64::Engine as _;
    // 整条命令里令牌是最后一个词。
    let candidate = input
        .split_whitespace()
        .last()
        .ok_or_else(|| "请填写隧道令牌".to_string())?;
    let invalid =
        || "这不是有效的隧道令牌：应是 Cloudflare 隧道页面上以 eyJ 开头的一长串字符".to_string();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(candidate)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(candidate))
        .map_err(|_| invalid())?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).map_err(|_| invalid())?;
    let has = |key: &str| json.get(key).and_then(|value| value.as_str()).is_some();
    if has("a") && has("t") && has("s") {
        Ok(candidate.to_string())
    } else {
        Err(invalid())
    }
}

/// `https://xxx.trycloudflare.com/...` → `xxx.trycloudflare.com`。
pub(crate) fn host_of_public_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let host = rest.split(['/', ':', '?']).next()?.trim();
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// 测试用隧道：`start` 后立即“就绪”，地址可由测试指定。
    pub(crate) struct FakeTunnel {
        pub url: String,
        pub started_ports: StdMutex<Vec<u16>>,
        /// 每次启动时收到的命名隧道域名（Quick 启动记为 `None`）。
        pub started_named: StdMutex<Vec<Option<String>>>,
        running: StdMutex<bool>,
        current_url: StdMutex<String>,
    }

    impl FakeTunnel {
        pub(crate) fn new(url: &str) -> Arc<Self> {
            Arc::new(Self {
                url: url.to_string(),
                started_ports: StdMutex::new(Vec::new()),
                started_named: StdMutex::new(Vec::new()),
                running: StdMutex::new(false),
                current_url: StdMutex::new(url.to_string()),
            })
        }

        pub(crate) async fn snapshot_status(&self) -> String {
            self.snapshot().await.status
        }
    }

    impl TunnelProvider for FakeTunnel {
        fn start<'a>(&'a self, port: u16, named: Option<NamedTunnel>) -> BoxFuture<'a, ()> {
            Box::pin(async move {
                self.started_ports.lock().unwrap().push(port);
                *self.current_url.lock().unwrap() = match &named {
                    Some(named) => format!("https://{}", named.hostname),
                    None => self.url.clone(),
                };
                self.started_named
                    .lock()
                    .unwrap()
                    .push(named.map(|named| named.hostname));
                *self.running.lock().unwrap() = true;
            })
        }

        fn snapshot<'a>(&'a self) -> BoxFuture<'a, TunnelSnapshot> {
            Box::pin(async move {
                if *self.running.lock().unwrap() {
                    TunnelSnapshot {
                        status: "running".to_string(),
                        public_url: Some(self.current_url.lock().unwrap().clone()),
                        error: None,
                    }
                } else {
                    TunnelSnapshot {
                        status: "stopped".to_string(),
                        ..TunnelSnapshot::default()
                    }
                }
            })
        }

        fn stop<'a>(&'a self) -> BoxFuture<'a, ()> {
            Box::pin(async move {
                *self.running.lock().unwrap() = false;
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_token() -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .encode(r#"{"a":"acct","t":"11111111-2222-3333-4444-555555555555","s":"c2VjcmV0"}"#)
    }

    #[test]
    fn named_tunnel_input_is_normalised_and_validated() {
        let token = sample_token();
        let parsed = parse_named_tunnel("https://MCP.Example.com/mcp", &token).unwrap();
        assert_eq!(parsed.hostname, "mcp.example.com");
        assert_eq!(parsed.token, token);
        // 整条 install 命令也能粘贴。
        let pasted = format!("sudo cloudflared service install {token}");
        assert_eq!(
            parse_named_tunnel("mcp.example.com", &pasted)
                .unwrap()
                .token,
            token
        );
        // 令牌不会出现在 Debug 输出里。
        assert!(!format!("{parsed:?}").contains(&token));

        for bad in [
            "",
            "localhost",
            "a_b.example.com",
            "-x.example.com",
            "1.2.3.4",
            "x.trycloudflare.com",
        ] {
            assert!(parse_named_tunnel(bad, &token).is_err(), "{bad}");
        }
        for bad in ["", "   ", "not-a-token", "eyJhIjoxfQ=="] {
            assert!(parse_named_tunnel("mcp.example.com", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn host_is_extracted_from_quick_tunnel_urls() {
        assert_eq!(
            host_of_public_url("https://Quick-Demo.trycloudflare.com").as_deref(),
            Some("quick-demo.trycloudflare.com")
        );
        assert_eq!(
            host_of_public_url("https://a-b.trycloudflare.com/web.html?x=1").as_deref(),
            Some("a-b.trycloudflare.com")
        );
        assert_eq!(host_of_public_url("http://plain.example.com"), None);
        assert_eq!(host_of_public_url("https://"), None);
    }
}
