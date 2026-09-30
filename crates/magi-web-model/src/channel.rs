//! T3 通道就绪状态（设计基线 §5.7.4、A10；实现计划 4.9）。
//!
//! T3 需要一条「外部平台能回调本机」的通道。通道有两条正式形态：Magi Connect
//! 设备连接层（优先）与 OpenAI Tunnel（Connect 未就绪时的正式交付）。两条形态对
//! 推理通道只暴露同一件事：**通道现在能不能把 `tools/call` 送到本机 harness**。
//!
//! 本模块是这件事的唯一载体：
//! - 通道托管方（daemon 装配层）负责把真实状态写进来；
//! - 推理通道只读它来决定档位：T3 请求在通道未就绪时**必须降档到 T2**，
//!   并把「具体缺哪一项」原样带出去，不允许假装 T3 可用（A15、§5.7.0）。
//!
//! 状态只驻进程内存：它描述的是「此刻能不能用」，不是可持久化事实（A20）。

use std::sync::Mutex;

/// 通道就绪状态。`code` 直接进入 UI 的「具体缺哪一项」，不包含任何凭据信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebModelChannelStatus {
    pub ready: bool,
    /// 稳定原因码：`running` / `tunnel_not_configured` / `client_missing` /
    /// `client_checksum_mismatch` / `credential_missing` / `start_failed` /
    /// `connect_not_ready` / `harness_unavailable`。
    pub code: String,
    /// 用户可读的「具体缺哪一项」。
    pub detail: String,
}

impl WebModelChannelStatus {
    /// 尚未配置任何 T3 通道。
    pub fn not_configured() -> Self {
        Self {
            ready: false,
            code: "tunnel_not_configured".to_string(),
            detail: "尚未配置 T3 通道：需要 OpenAI Tunnel（Tunnel 与仅含 Tunnels Read + Use 的 API 密钥）或 Magi Connect 设备连接".to_string(),
        }
    }

    pub fn running(detail: impl Into<String>) -> Self {
        Self {
            ready: true,
            code: "running".to_string(),
            detail: detail.into(),
        }
    }

    pub fn unavailable(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            ready: false,
            code: code.into(),
            detail: detail.into(),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }
}

impl Default for WebModelChannelStatus {
    fn default() -> Self {
        Self::not_configured()
    }
}

/// 进程内共享的通道状态单元。
///
/// 通道托管方写、推理通道读；两者都只持一个 `Arc`，不引入第二份事实源。
pub struct WebModelChannelState {
    status: Mutex<WebModelChannelStatus>,
}

impl WebModelChannelState {
    pub fn new() -> Self {
        Self {
            status: Mutex::new(WebModelChannelStatus::default()),
        }
    }

    pub fn with_status(status: WebModelChannelStatus) -> Self {
        Self {
            status: Mutex::new(status),
        }
    }

    pub fn set(&self, status: WebModelChannelStatus) {
        let mut guard = self.status.lock().expect("web model channel lock poisoned");
        *guard = status;
    }

    pub fn get(&self) -> WebModelChannelStatus {
        self.status
            .lock()
            .expect("web model channel lock poisoned")
            .clone()
    }
}

impl Default for WebModelChannelState {
    fn default() -> Self {
        Self::new()
    }
}

/// 把 harness/tunnel 侧的托管状态翻译成通道状态。
///
/// `Connect` 未就绪时一律以 OpenAI Tunnel 的托管状态为准（A10：Connect 优先，
/// 未就绪则以 OpenAI Tunnel 正式交付）。
pub fn status_from_tunnel(
    tunnel: Option<&crate::harness::TunnelClientStatus>,
) -> WebModelChannelStatus {
    let Some(status) = tunnel else {
        return WebModelChannelStatus::not_configured();
    };
    use crate::harness::TunnelClientStatus;
    match status {
        TunnelClientStatus::Running { pid } => {
            WebModelChannelStatus::running(format!("OpenAI Tunnel 运行中（pid {pid}）"))
        }
        other => WebModelChannelStatus::unavailable(other.code(), other.detail()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::TunnelClientStatus;

    #[test]
    fn a_missing_tunnel_config_is_not_ready() {
        let status = status_from_tunnel(None);
        assert!(!status.ready);
        assert_eq!(status.code, "tunnel_not_configured");
    }

    #[test]
    fn a_running_tunnel_is_ready_and_carries_no_credential() {
        let status = status_from_tunnel(Some(&TunnelClientStatus::Running { pid: 42 }));
        assert!(status.ready);
        assert_eq!(status.code, "running");
        assert!(status.detail.contains("42"));
    }

    #[test]
    fn a_missing_credential_reports_the_exact_missing_piece() {
        let status = status_from_tunnel(Some(&TunnelClientStatus::MissingCredential {
            path: "/tmp/key".to_string(),
        }));
        assert!(!status.ready);
        assert_eq!(status.code, "credential_missing");
        assert!(status.detail.contains("/tmp/key"));
    }

    #[test]
    fn the_shared_cell_round_trips_the_latest_status() {
        let cell = WebModelChannelState::new();
        assert!(!cell.get().ready);
        cell.set(WebModelChannelStatus::running("ok"));
        assert!(cell.get().ready);
    }
}
