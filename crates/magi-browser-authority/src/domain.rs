use std::path::PathBuf;

use magi_core::{
    BrowserAnnotationId, BrowserLeaseId, BrowserProfileId, BrowserSessionId, BrowserTabId,
    ExecutionOwnership, GoalId, SessionId, UtcMillis, WorkspaceId,
};
use serde::{Deserialize, Serialize};

use crate::BrowserSurfaceBinding;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserProfileKind {
    ManagedDefault,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserProfile {
    pub profile_id: BrowserProfileId,
    pub kind: BrowserProfileKind,
    pub data_path: PathBuf,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserSessionLifecycle {
    Creating,
    Ready,
    Recovering,
    Interrupted,
    Failed,
    Closed,
}

impl BrowserSessionLifecycle {
    pub fn is_open(self) -> bool {
        self != Self::Closed
    }

    /// 只有仍然拥有可恢复运行边界的会话才允许在 Browser Host 重启后恢复。
    /// Failed 是终态故障记录，不能重新物化为 Chromium 页面。
    pub fn is_recoverable(self) -> bool {
        matches!(
            self,
            Self::Creating | Self::Ready | Self::Recovering | Self::Interrupted
        )
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        matches!(
            (self, next),
            (
                Self::Creating,
                Self::Ready | Self::Interrupted | Self::Failed | Self::Closed
            ) | (
                Self::Ready,
                Self::Recovering | Self::Interrupted | Self::Failed | Self::Closed
            ) | (
                Self::Recovering,
                Self::Ready | Self::Interrupted | Self::Failed | Self::Closed
            ) | (Self::Interrupted, Self::Recovering | Self::Closed)
                | (Self::Failed, Self::Closed)
        )
    }
}

/// Browser Session 的归属作用域。
///
/// `Session` 与 Magi 会话保持同一作用域：个人会话没有伪造的 workspace。
/// `App` 是应用级唯一会话（GPT Web 模型），不属于任何会话或工作区，
/// 全局唯一、ID 持久稳定，登录态落在固定持久分区里。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrowserSessionOwner {
    Session {
        session_id: SessionId,
        workspace_id: Option<WorkspaceId>,
    },
    App,
}

impl BrowserSessionOwner {
    pub fn magi_session_id(&self) -> Option<&SessionId> {
        match self {
            Self::Session { session_id, .. } => Some(session_id),
            Self::App => None,
        }
    }

    pub fn workspace_id(&self) -> Option<&WorkspaceId> {
        match self {
            Self::Session { workspace_id, .. } => workspace_id.as_ref(),
            Self::App => None,
        }
    }

    pub fn is_app(&self) -> bool {
        matches!(self, Self::App)
    }

    pub fn matches_magi_session(&self, session_id: &SessionId) -> bool {
        matches!(self, Self::Session { session_id: owned, .. } if owned == session_id)
    }
}

impl BrowserSession {
    /// 该会话所属的 Magi 会话；应用级会话返回 `None`。
    pub fn magi_session_id(&self) -> Option<&SessionId> {
        self.owner.magi_session_id()
    }

    /// 该会话所属的工作区；应用级会话返回 `None`。
    pub fn owner_workspace_id(&self) -> Option<&WorkspaceId> {
        self.owner.workspace_id()
    }

    pub fn is_app_scope(&self) -> bool {
        self.owner.is_app()
    }

    /// 归属作用域的技术标识：`session` 或 `app`。
    pub fn owner_kind(&self) -> &'static str {
        match self.owner {
            BrowserSessionOwner::Session { .. } => "session",
            BrowserSessionOwner::App => "app",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserSession {
    pub browser_session_id: BrowserSessionId,
    pub owner: BrowserSessionOwner,
    pub profile_id: BrowserProfileId,
    pub lifecycle: BrowserSessionLifecycle,
    pub tab_ids: Vec<BrowserTabId>,
    pub runtime_epoch: u64,
    pub revision: u64,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserTabLifecycle {
    Creating,
    Ready,
    /// 逻辑 Tab 仍然存在，但当前 Browser Host 没有为它物化 Chromium Page。
    /// 该状态用于 daemon 重启、运行组件升级和 Host 的页面淘汰，不代表 Tab 丢失。
    Suspended,
    Crashed,
    Closed,
}

impl BrowserTabLifecycle {
    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        matches!(
            (self, next),
            (
                Self::Creating,
                Self::Ready | Self::Suspended | Self::Crashed | Self::Closed
            ) | (Self::Ready, Self::Suspended | Self::Crashed | Self::Closed)
                | (Self::Suspended, Self::Ready | Self::Crashed | Self::Closed)
                | (Self::Crashed, Self::Suspended | Self::Ready | Self::Closed)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserViewport {
    pub width: u32,
    pub height: u32,
    pub device_scale_factor_millis: u32,
    #[serde(default)]
    pub device_type: BrowserDeviceType,
}

impl Default for BrowserViewport {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
            device_scale_factor_millis: 1_000,
            device_type: BrowserDeviceType::Desktop,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserDeviceType {
    #[default]
    Desktop,
    Mobile,
}

impl BrowserDeviceType {
    pub fn for_dimensions(width: u32) -> Self {
        if width <= 600 {
            Self::Mobile
        } else {
            Self::Desktop
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserViewportMode {
    #[default]
    Auto,
    Fixed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserTab {
    pub tab_id: BrowserTabId,
    pub browser_session_id: BrowserSessionId,
    pub order: u32,
    pub lifecycle: BrowserTabLifecycle,
    pub url: String,
    pub origin: Option<String>,
    pub title: String,
    pub display_label: Option<String>,
    pub navigation_revision: u64,
    /// 页面快照版本由 Authority 分配并持久化，防止 daemon/Worker 重启后旧 element_ref
    /// 与新快照复用同一个 revision。
    pub snapshot_revision: u64,
    pub annotation_sequence: u64,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalControlBinding {
    pub goal_id: GoalId,
    pub control_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserLeaseLifecycle {
    Held,
    Released,
    Revoked,
    Expired,
}

impl BrowserLeaseLifecycle {
    pub fn is_terminal(self) -> bool {
        self != Self::Held
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserLeaseEndReason {
    OwnerReleased,
    UserTakeover,
    GoalPaused,
    TurnStopped,
    TaskFinished,
    SessionClosed,
    RuntimeUnavailable,
    RuntimeShutdown,
    RuntimeUpdateRequired,
    LeaseExpired,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserControlLease {
    pub lease_id: BrowserLeaseId,
    pub desktop_epoch: String,
    pub window_id: String,
    pub surface_revision: u64,
    pub tab_id: BrowserTabId,
    pub surface_id: String,
    pub web_contents_id: u32,
    pub target_id: String,
    pub browser_context_id: String,
    pub navigation_revision: u64,
    pub owner: ExecutionOwnership,
    pub turn_id: String,
    pub goal_binding: Option<GoalControlBinding>,
    pub fence: u64,
    pub lifecycle: BrowserLeaseLifecycle,
    pub end_reason: Option<BrowserLeaseEndReason>,
    pub acquired_at: UtcMillis,
    pub expires_at: UtcMillis,
    pub ended_at: Option<UtcMillis>,
}

impl BrowserControlLease {
    pub fn surface_binding(&self) -> BrowserSurfaceBinding {
        BrowserSurfaceBinding {
            desktop_epoch: self.desktop_epoch.clone(),
            window_id: self.window_id.clone(),
            surface_id: self.surface_id.clone(),
            surface_revision: self.surface_revision,
            tab_id: self.tab_id.clone(),
            web_contents_id: self.web_contents_id,
            target_id: self.target_id.clone(),
            browser_context_id: self.browser_context_id.clone(),
            navigation_revision: self.navigation_revision,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BrowserLeaseSelector {
    pub tab_id: Option<BrowserTabId>,
    pub surface_id: Option<String>,
    pub session_id: Option<SessionId>,
    pub workspace_id: Option<WorkspaceId>,
    pub mission_id: Option<magi_core::MissionId>,
    pub task_id: Option<magi_core::TaskId>,
    pub worker_id: Option<magi_core::WorkerId>,
    pub execution_chain_ref: Option<String>,
    pub goal_id: Option<GoalId>,
}

impl BrowserLeaseSelector {
    pub fn matches(&self, lease: &BrowserControlLease) -> bool {
        option_matches(&self.tab_id, Some(&lease.tab_id))
            && option_matches(&self.surface_id, Some(&lease.surface_id))
            && option_matches(&self.session_id, lease.owner.session_id.as_ref())
            && option_matches(&self.workspace_id, lease.owner.workspace_id.as_ref())
            && option_matches(&self.mission_id, lease.owner.mission_id.as_ref())
            && option_matches(&self.task_id, lease.owner.task_id.as_ref())
            && option_matches(&self.worker_id, lease.owner.worker_id.as_ref())
            && option_matches(
                &self.execution_chain_ref,
                lease.owner.execution_chain_ref.as_ref(),
            )
            && option_matches(
                &self.goal_id,
                lease.goal_binding.as_ref().map(|binding| &binding.goal_id),
            )
    }
}

fn option_matches<T: PartialEq>(expected: &Option<T>, actual: Option<&T>) -> bool {
    expected
        .as_ref()
        .is_none_or(|expected| actual == Some(expected))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAnnotationAuthor {
    User,
    Agent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAnnotationKind {
    Element,
    Region,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAnnotationStatus {
    Active,
    Resolved,
    Stale,
    Deleted,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserNormalizedRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrowserAnnotationAnchor {
    Element(Box<BrowserElementAnnotationAnchor>),
    Region(BrowserRegionAnnotationAnchor),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserElementAnnotationAnchor {
    pub url: String,
    pub origin: Option<String>,
    pub frame_path: Vec<String>,
    pub viewport: BrowserViewport,
    pub scroll_x: f64,
    pub scroll_y: f64,
    pub test_id: Option<String>,
    pub stable_id: Option<String>,
    pub aria_role: Option<String>,
    pub aria_name: Option<String>,
    pub tag_name: String,
    pub text_excerpt: Option<String>,
    pub css_path: String,
    pub ancestor_fingerprint: String,
    pub dom_fingerprint: String,
    pub bounding_box: BrowserNormalizedRect,
    pub snapshot_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserRegionAnnotationAnchor {
    pub url: String,
    pub origin: Option<String>,
    pub viewport: BrowserViewport,
    pub scroll_x: f64,
    pub scroll_y: f64,
    pub rect: BrowserNormalizedRect,
    pub snapshot_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserAnnotation {
    pub annotation_id: BrowserAnnotationId,
    pub browser_session_id: BrowserSessionId,
    pub tab_id: BrowserTabId,
    #[serde(default)]
    pub sequence: u64,
    pub author: BrowserAnnotationAuthor,
    pub kind: BrowserAnnotationKind,
    pub anchor: BrowserAnnotationAnchor,
    pub comment: String,
    pub status: BrowserAnnotationStatus,
    pub screenshot_artifact_id: Option<String>,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}
