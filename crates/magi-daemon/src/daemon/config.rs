use std::{
    env,
    net::SocketAddr,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use thiserror::Error;

static STARTUP_NONCE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(crate) fn new_startup_nonce() -> String {
    let mut entropy = [0_u8; 16];
    if getrandom::fill(&mut entropy).is_ok() {
        return entropy.iter().map(|byte| format!("{byte:02x}")).collect();
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!(
        "fallback-{timestamp}-{}-{}",
        std::process::id(),
        STARTUP_NONCE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn env_or_default(name: &str, default: impl FnOnce() -> String) -> String {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(default)
}

#[derive(Clone, Debug)]
pub struct DaemonConfig {
    pub host: String,
    pub port: u16,
    pub service_name: String,
    pub product_version: String,
    pub build_identity: String,
    pub startup_nonce: String,
    pub state_root: PathBuf,
    pub web_dist_root: Option<PathBuf>,
    pub open_browser: bool,
}

impl DaemonConfig {
    pub fn new(
        host: impl Into<String>,
        port: u16,
        service_name: impl Into<String>,
        state_root: impl Into<PathBuf>,
    ) -> Self {
        let state_root = state_root.into();
        Self {
            host: host.into(),
            port,
            service_name: service_name.into(),
            product_version: env_or_default("MAGI_PRODUCT_VERSION", || {
                env!("CARGO_PKG_VERSION").to_string()
            }),
            build_identity: env_or_default("MAGI_BUILD_ID", || "source".to_string()),
            startup_nonce: env_or_default("MAGI_DAEMON_START_NONCE", new_startup_nonce),
            state_root,
            web_dist_root: None,
            open_browser: false,
        }
    }

    pub fn with_open_browser(mut self, open_browser: bool) -> Self {
        self.open_browser = open_browser;
        self
    }

    pub fn with_web_dist_root(mut self, web_dist_root: impl Into<PathBuf>) -> Self {
        self.web_dist_root = Some(web_dist_root.into());
        self
    }

    pub fn with_identity(
        mut self,
        product_version: impl Into<String>,
        build_identity: impl Into<String>,
        startup_nonce: impl Into<String>,
    ) -> Self {
        self.product_version = product_version.into();
        self.build_identity = build_identity.into();
        self.startup_nonce = startup_nonce.into();
        self
    }

    pub fn identity(&self) -> magi_api::DaemonIdentity {
        magi_api::DaemonIdentity {
            product_version: self.product_version.clone(),
            build_identity: self.build_identity.clone(),
            startup_nonce: self.startup_nonce.clone(),
        }
    }

    pub fn socket_addr(&self) -> Result<SocketAddr, DaemonError> {
        format!("{}:{}", self.host, self.port)
            .parse()
            .map_err(DaemonError::InvalidAddress)
    }
}

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("监听地址非法: {0}")]
    InvalidAddress(#[source] std::net::AddrParseError),
    #[error("网络监听失败: {0}")]
    Io(#[from] std::io::Error),
    #[error("状态序列化失败: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("daemon 内部错误: {0}")]
    Internal(String),
}

impl DaemonError {
    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }
}
