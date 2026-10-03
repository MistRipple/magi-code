//! MCP 网络模式的隧道：Quick Tunnel（cloudflared，无需账号，**公网地址每次启动都会变**）。
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

pub(crate) trait TunnelProvider: Send + Sync {
    /// 开始把公网流量转发到 `127.0.0.1:port`。不等待就绪，调用方通过 `snapshot` 观察。
    fn start<'a>(&'a self, port: u16) -> BoxFuture<'a, ()>;
    fn snapshot<'a>(&'a self) -> BoxFuture<'a, TunnelSnapshot>;
    fn stop<'a>(&'a self) -> BoxFuture<'a, ()>;
}

/// cloudflared Quick Tunnel。
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
    fn start<'a>(&'a self, port: u16) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let mut slot = self.manager.lock().await;
            if let Some(previous) = slot.take() {
                previous.stop().await;
            }
            let manager = TunnelManager::new(port);
            // MCP 入口自己用 Bearer 令牌鉴权，不使用远程访问那套 tunnel_token。
            manager.start(RemoteAccessBinding::default()).await;
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
        running: StdMutex<bool>,
    }

    impl FakeTunnel {
        pub(crate) fn new(url: &str) -> Arc<Self> {
            Arc::new(Self {
                url: url.to_string(),
                started_ports: StdMutex::new(Vec::new()),
                running: StdMutex::new(false),
            })
        }

        pub(crate) async fn snapshot_status(&self) -> String {
            self.snapshot().await.status
        }
    }

    impl TunnelProvider for FakeTunnel {
        fn start<'a>(&'a self, port: u16) -> BoxFuture<'a, ()> {
            Box::pin(async move {
                self.started_ports.lock().unwrap().push(port);
                *self.running.lock().unwrap() = true;
            })
        }

        fn snapshot<'a>(&'a self) -> BoxFuture<'a, TunnelSnapshot> {
            Box::pin(async move {
                if *self.running.lock().unwrap() {
                    TunnelSnapshot {
                        status: "running".to_string(),
                        public_url: Some(self.url.clone()),
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
