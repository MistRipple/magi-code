use magi_core::AccessProfile;
use serde::{Deserialize, Serialize};

use crate::{BrowserToolAccess, BrowserToolKind};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserCapabilityUnavailableReason {
    BrowserUseDisabled,
    DesktopHostNotReady,
    HostProtocolIncompatible,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
        tool: BrowserToolKind,
        requested_access: BrowserToolAccess,
    ) -> Result<(), BrowserCapabilityRejection> {
        if let Some(reason) = self.unavailable_reason() {
            return Err(BrowserCapabilityRejection::Unavailable(reason));
        }
        if !self.allows_catalog_tool(tool) {
            return Err(BrowserCapabilityRejection::ToolNotVisible { tool });
        }
        let catalog_access = tool.catalog_access();
        if !catalog_access.allows(requested_access) {
            return Err(BrowserCapabilityRejection::AccessNotAllowed {
                tool,
                requested_access,
                catalog_access,
            });
        }
        Ok(())
    }
}

impl BrowserToolAccess {
    fn allows(self, requested: Self) -> bool {
        matches!(
            (self, requested),
            (Self::Mixed, _) | (Self::Read, Self::Read) | (Self::Write, Self::Write)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BrowserCapabilityRejection {
    #[error("browser capability is unavailable: {0:?}")]
    Unavailable(BrowserCapabilityUnavailableReason),
    #[error("browser tool is not visible in this capability snapshot: {tool:?}")]
    ToolNotVisible { tool: BrowserToolKind },
    #[error(
        "browser tool access is not allowed: tool={tool:?}, requested={requested_access:?}, catalog={catalog_access:?}"
    )]
    AccessNotAllowed {
        tool: BrowserToolKind,
        requested_access: BrowserToolAccess,
        catalog_access: BrowserToolAccess,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_snapshot() -> BrowserCapabilitySnapshot {
        BrowserCapabilitySnapshot {
            revision: 1,
            in_app_browser_enabled: true,
            browser_use_enabled: true,
            host_status: BrowserHostStatus::Ready,
            host_protocol_compatible: true,
            access_profile: AccessProfile::default(),
        }
    }

    #[test]
    fn read_only_pwa_cannot_be_called_with_write_action() {
        let snapshot = ready_snapshot();
        let error = snapshot
            .allows_execution(BrowserToolKind::Pwa, BrowserToolAccess::Write)
            .expect_err("PWA write operations are not part of the exposed capability");
        assert!(matches!(
            error,
            BrowserCapabilityRejection::AccessNotAllowed {
                tool: BrowserToolKind::Pwa,
                ..
            }
        ));
    }

    #[test]
    fn mixed_diagnostic_tools_allow_both_read_and_write_actions() {
        let snapshot = ready_snapshot();
        for tool in [BrowserToolKind::Heap, BrowserToolKind::ThirdParty] {
            snapshot
                .allows_execution(tool, BrowserToolAccess::Read)
                .expect("diagnostic reads should be allowed");
            snapshot
                .allows_execution(tool, BrowserToolAccess::Write)
                .expect("diagnostic state changes should be allowed");
        }
    }

    #[test]
    fn navigation_is_write_and_read_only_tools_remain_read_only() {
        let snapshot = ready_snapshot();
        snapshot
            .allows_execution(BrowserToolKind::Navigate, BrowserToolAccess::Write)
            .expect("navigation should be a write operation");
        snapshot
            .allows_execution(BrowserToolKind::Snapshot, BrowserToolAccess::Read)
            .expect("snapshot should be a read operation");
        assert!(
            snapshot
                .allows_execution(BrowserToolKind::Snapshot, BrowserToolAccess::Write)
                .is_err()
        );
    }

    #[test]
    fn capability_revision_is_metadata_and_does_not_invalidate_execution() {
        let mut snapshot = ready_snapshot();
        snapshot.revision = 42;
        snapshot
            .allows_execution(BrowserToolKind::Navigate, BrowserToolAccess::Write)
            .expect("a captured capability snapshot remains valid for its turn");
    }
}
