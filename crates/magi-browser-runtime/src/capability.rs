use magi_core::AccessProfile;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserRuntimeComponentStatus {
    NotInstalled,
    Downloading,
    Verifying,
    Installed,
    UpdateAvailable,
    UpdateRequired,
    Failed,
}

impl BrowserRuntimeComponentStatus {
    pub fn is_usable(self) -> bool {
        matches!(self, Self::Installed | Self::UpdateAvailable)
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
    Screencast,
    Heap,
    Extensions,
    ThirdParty,
    WebMcp,
    Pwa,
}

impl BrowserToolKind {
    pub const ALL: [Self; 28] = [
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
        Self::Screencast,
        Self::Heap,
        Self::Extensions,
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
            Self::Screencast => "browser_screencast",
            Self::Heap => "browser_heap",
            Self::Extensions => "browser_extensions",
            Self::ThirdParty => "browser_third_party",
            Self::WebMcp => "browser_webmcp",
            Self::Pwa => "browser_pwa",
        }
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
            | Self::Screencast
            | Self::Extensions
            | Self::ThirdParty
            | Self::WebMcp
            | Self::Pwa => BrowserToolAccess::Mixed,
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
    RuntimeNotInstalled,
    RuntimeInstalling,
    RuntimeUpdateRequired,
    RuntimeFailed,
    HostProtocolIncompatible,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserCapabilitySnapshot {
    pub revision: u64,
    pub in_app_browser_enabled: bool,
    pub browser_use_enabled: bool,
    pub runtime_status: BrowserRuntimeComponentStatus,
    pub host_protocol_compatible: bool,
    pub access_profile: AccessProfile,
}

impl BrowserCapabilitySnapshot {
    pub fn unavailable_reason(&self) -> Option<BrowserCapabilityUnavailableReason> {
        if !self.browser_use_enabled {
            return Some(BrowserCapabilityUnavailableReason::BrowserUseDisabled);
        }
        let runtime_reason = match self.runtime_status {
            BrowserRuntimeComponentStatus::NotInstalled => {
                Some(BrowserCapabilityUnavailableReason::RuntimeNotInstalled)
            }
            BrowserRuntimeComponentStatus::Downloading
            | BrowserRuntimeComponentStatus::Verifying => {
                Some(BrowserCapabilityUnavailableReason::RuntimeInstalling)
            }
            BrowserRuntimeComponentStatus::UpdateRequired => {
                Some(BrowserCapabilityUnavailableReason::RuntimeUpdateRequired)
            }
            BrowserRuntimeComponentStatus::Failed => {
                Some(BrowserCapabilityUnavailableReason::RuntimeFailed)
            }
            BrowserRuntimeComponentStatus::Installed
            | BrowserRuntimeComponentStatus::UpdateAvailable => None,
        };
        if runtime_reason.is_some() {
            return runtime_reason;
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

    pub fn allows_catalog_tool(&self, _tool: BrowserToolKind) -> bool {
        if self.unavailable_reason().is_some() {
            return false;
        }
        true
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
