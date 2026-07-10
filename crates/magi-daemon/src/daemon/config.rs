use std::{net::SocketAddr, path::PathBuf};

use thiserror::Error;

#[derive(Clone, Debug)]
pub struct DaemonConfig {
    pub host: String,
    pub port: u16,
    pub service_name: String,
    pub state_root: PathBuf,
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
            state_root,
            open_browser: false,
        }
    }

    pub fn with_open_browser(mut self, open_browser: bool) -> Self {
        self.open_browser = open_browser;
        self
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
