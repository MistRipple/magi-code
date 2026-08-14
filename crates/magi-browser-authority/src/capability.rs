use magi_core::AccessProfile;
use serde::{Deserialize, Serialize};

/// Electron Main 托管的真实浏览器控制通道状态。
///
/// Chromium、Electron 和 Automation Worker 都属于同一个桌面发行包，
/// 因此 daemon 不再维护“浏览器运行组件安装器”状态。这里仅反映
/// 当前 Desktop Control Socket 是否可用于 Agent 工具调用。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserHostStatus {
    #[default]
    Stopped,
    Starting,
    Ready,
    Reconnecting,
    Failed,
}

impl BrowserHostStatus {
    pub fn is_usable(self) -> bool {
        matches!(self, Self::Ready)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserToolKind {
    Navigate,
    Snapshot,
    Click,
    Type,
    Press,
    Scroll,
    Screenshot,
    Tabs,
    Viewport,
    WaitFor,
    Hover,
    Drag,
    FillForm,
    Dialog,
    UploadFile,
    ClickAt,
    Evaluate,
    Console,
    Network,
    Emulate,
    Performance,
    Lighthouse,
    Heap,
    ThirdParty,
    WebMcp,
    Pwa,
}

impl BrowserToolKind {
    pub const ALL: [Self; 26] = [
        Self::Navigate,
        Self::Snapshot,
        Self::Click,
        Self::Type,
        Self::Press,
        Self::Scroll,
        Self::Screenshot,
        Self::Tabs,
        Self::Viewport,
        Self::WaitFor,
        Self::Hover,
        Self::Drag,
        Self::FillForm,
        Self::Dialog,
        Self::UploadFile,
        Self::ClickAt,
        Self::Evaluate,
        Self::Console,
        Self::Network,
        Self::Emulate,
        Self::Performance,
        Self::Lighthouse,
        Self::Heap,
        Self::ThirdParty,
        Self::WebMcp,
        Self::Pwa,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Navigate => "browser_navigate",
            Self::Snapshot => "browser_snapshot",
            Self::Click => "browser_click",
            Self::Type => "browser_type",
            Self::Press => "browser_press",
            Self::Scroll => "browser_scroll",
            Self::Screenshot => "browser_screenshot",
            Self::Tabs => "browser_tabs",
            Self::Viewport => "browser_viewport",
            Self::WaitFor => "browser_wait_for",
            Self::Hover => "browser_hover",
            Self::Drag => "browser_drag",
            Self::FillForm => "browser_fill_form",
            Self::Dialog => "browser_dialog",
            Self::UploadFile => "browser_upload_file",
            Self::ClickAt => "browser_click_at",
            Self::Evaluate => "browser_evaluate",
            Self::Console => "browser_console",
            Self::Network => "browser_network",
            Self::Emulate => "browser_emulate",
            Self::Performance => "browser_performance",
            Self::Lighthouse => "browser_lighthouse",
            Self::Heap => "browser_heap",
            Self::ThirdParty => "browser_third_party",
            Self::WebMcp => "browser_webmcp",
            Self::Pwa => "browser_pwa",
        }
    }

    /// 文件系统授权需要独立的 Desktop 授权管线，当前版本不会隐式读取本地文件。
    pub fn is_supported(self) -> bool {
        !matches!(self, Self::UploadFile)
    }

    pub fn catalog_access(self) -> BrowserToolAccess {
        match self {
            Self::Navigate
            | Self::Snapshot
            | Self::Screenshot
            | Self::WaitFor
            | Self::Console
            | Self::Network
            | Self::Performance
            | Self::Lighthouse
            | Self::Heap => BrowserToolAccess::Read,
            Self::Tabs
            | Self::Viewport
            | Self::Dialog
            | Self::Emulate
            | Self::ThirdParty
            | Self::WebMcp => BrowserToolAccess::Mixed,
            Self::Pwa => BrowserToolAccess::Read,
            Self::Click
            | Self::Type
            | Self::Press
            | Self::Scroll
            | Self::Hover
            | Self::Drag
            | Self::FillForm
            | Self::UploadFile
            | Self::ClickAt
            | Self::Evaluate => BrowserToolAccess::Write,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserToolAccess {
    Read,
    Write,
    Mixed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserCapabilityUnavailableReason {
    BrowserUseDisabled,
    DesktopHostNotReady,
    HostProtocolIncompatible,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserCapabilitySnapshot {
    pub revision: u64,
    pub in_app_browser_enabled: bool,
    pub browser_use_enabled: bool,
    pub host_status: BrowserHostStatus,
    pub host_protocol_compatible: bool,
    pub access_profile: AccessProfile,
}

impl BrowserCapabilitySnapshot {
    pub fn unavailable_reason(&self) -> Option<BrowserCapabilityUnavailableReason> {
        if !self.browser_use_enabled {
            return Some(BrowserCapabilityUnavailableReason::BrowserUseDisabled);
        }
        if !self.host_status.is_usable() {
            return Some(BrowserCapabilityUnavailableReason::DesktopHostNotReady);
        }
        if !self.host_protocol_compatible {
            return Some(BrowserCapabilityUnavailableReason::HostProtocolIncompatible);
        }
        None
    }

    pub fn visible_tools(&self) -> Vec<BrowserToolKind> {
        if self.unavailable_reason().is_some() {
            return Vec::new();
        }
        BrowserToolKind::ALL
            .into_iter()
            .filter(|tool| self.allows_catalog_tool(*tool))
            .collect()
    }

    pub fn allows_catalog_tool(&self, tool: BrowserToolKind) -> bool {
        if self.unavailable_reason().is_some() {
            return false;
        }
        tool.is_supported()
    }

    pub fn allows_execution(
        &self,
        catalog_revision: u64,
        tool: BrowserToolKind,
        _requested_access: BrowserToolAccess,
    ) -> Result<(), BrowserCapabilityRejection> {
        if catalog_revision != self.revision {
            return Err(BrowserCapabilityRejection::SnapshotRevisionMismatch {
                catalog_revision,
                current_revision: self.revision,
            });
        }
        if let Some(reason) = self.unavailable_reason() {
            return Err(BrowserCapabilityRejection::Unavailable(reason));
        }
        if !self.allows_catalog_tool(tool) {
            return Err(BrowserCapabilityRejection::ToolNotVisible { tool });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BrowserCapabilityRejection {
    #[error(
        "browser capability snapshot revision changed: catalog={catalog_revision}, current={current_revision}"
    )]
    SnapshotRevisionMismatch {
        catalog_revision: u64,
        current_revision: u64,
    },
    #[error("browser capability is unavailable: {0:?}")]
    Unavailable(BrowserCapabilityUnavailableReason),
    #[error("browser tool is not visible in this capability snapshot: {tool:?}")]
    ToolNotVisible { tool: BrowserToolKind },
}
