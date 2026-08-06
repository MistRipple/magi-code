use crate::{BrowserDeviceType, BrowserNormalizedRect};
use magi_core::{BrowserCommandId, BrowserLeaseId, BrowserTabId};
use serde::{Deserialize, Serialize};

pub const BROWSER_HOST_PROTOCOL_MAJOR: u16 = 1;
pub const BROWSER_HOST_PROTOCOL_MINOR: u16 = 8;
pub const DEFAULT_BROWSER_SNAPSHOT_NODE_LIMIT: u32 = 400;
pub const DEFAULT_BROWSER_SNAPSHOT_TEXT_LIMIT_BYTES: u32 = 32 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BrowserHostProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl BrowserHostProtocolVersion {
    pub const CURRENT: Self = Self {
        major: BROWSER_HOST_PROTOCOL_MAJOR,
        minor: BROWSER_HOST_PROTOCOL_MINOR,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserHostProtocolRange {
    pub minimum: BrowserHostProtocolVersion,
    pub maximum: BrowserHostProtocolVersion,
}

impl BrowserHostProtocolRange {
    pub fn contains(self, version: BrowserHostProtocolVersion) -> bool {
        self.minimum <= version && version <= self.maximum
    }

    pub fn is_valid(self) -> bool {
        self.minimum <= self.maximum && self.minimum.major == self.maximum.major
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserHostHandshake {
    pub protocol_version: BrowserHostProtocolVersion,
    pub runtime_version: String,
    pub host_version: String,
    pub playwright_version: String,
    pub chromium_version: String,
    pub process_id: u32,
    pub runtime_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserHostRequestEnvelope {
    pub request_id: BrowserCommandId,
    pub protocol_version: BrowserHostProtocolVersion,
    pub command: BrowserHostCommand,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum BrowserHostCommand {
    Ping,
    CreatePage {
        tab_id: BrowserTabId,
        initial_url: String,
        viewport: HostViewport,
        navigation_revision: u64,
        snapshot_revision: u64,
    },
    SetViewport {
        tab_id: BrowserTabId,
        viewport: HostViewport,
    },
    ClosePage {
        tab_id: BrowserTabId,
    },
    ActivatePage {
        tab_id: BrowserTabId,
    },
    Navigate {
        tab_id: BrowserTabId,
        control: BrowserHostControl,
        navigation: BrowserNavigation,
    },
    Snapshot {
        tab_id: BrowserTabId,
        limits: BrowserSnapshotLimits,
        subtree_ref: Option<String>,
    },
    Click {
        tab_id: BrowserTabId,
        control: BrowserHostControl,
        target: BrowserSnapshotTarget,
    },
    Type {
        tab_id: BrowserTabId,
        control: BrowserHostControl,
        target: BrowserSnapshotTarget,
        text: String,
        replace: bool,
    },
    Press {
        tab_id: BrowserTabId,
        control: BrowserHostControl,
        key: String,
    },
    Scroll {
        tab_id: BrowserTabId,
        control: BrowserHostControl,
        target: Option<BrowserSnapshotTarget>,
        delta_x: f64,
        delta_y: f64,
    },
    Screenshot {
        tab_id: BrowserTabId,
        target: Option<BrowserSnapshotTarget>,
        clip: Option<BrowserNormalizedRect>,
        full_page: bool,
        format: BrowserScreenshotFormat,
    },
    HitTest {
        tab_id: BrowserTabId,
        navigation_revision: u64,
        x: f64,
        y: f64,
    },
    StartScreencast {
        tab_id: BrowserTabId,
        format: BrowserScreencastFormat,
        quality: u8,
        max_width: u32,
        max_height: u32,
    },
    StopScreencast {
        tab_id: BrowserTabId,
    },
    UserInput {
        tab_id: BrowserTabId,
        control: BrowserHostControl,
        event: BrowserUserInputEvent,
    },
    UpdateControl {
        fence: u64,
        mode: BrowserHostControlMode,
    },
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum BrowserHostControl {
    Agent {
        lease_id: BrowserLeaseId,
        fence: u64,
    },
    User {
        fence: u64,
    },
}

impl BrowserHostControl {
    pub fn fence(&self) -> u64 {
        match self {
            Self::Agent { fence, .. } | Self::User { fence } => *fence,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserHostControlMode {
    Agent,
    User,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostViewport {
    pub width: u32,
    pub height: u32,
    pub device_scale_factor_millis: u32,
    pub device_type: BrowserDeviceType,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BrowserNavigation {
    Url { url: String },
    Back,
    Forward,
    Reload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserSnapshotLimits {
    pub max_nodes: u32,
    pub max_text_bytes: u32,
}

impl Default for BrowserSnapshotLimits {
    fn default() -> Self {
        Self {
            max_nodes: DEFAULT_BROWSER_SNAPSHOT_NODE_LIMIT,
            max_text_bytes: DEFAULT_BROWSER_SNAPSHOT_TEXT_LIMIT_BYTES,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserSnapshotTarget {
    pub snapshot_revision: u64,
    pub element_ref: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserScreenshotFormat {
    Png,
    Jpeg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserScreencastFormat {
    Jpeg,
    Png,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserUserInputEvent {
    MouseMove {
        x: f64,
        y: f64,
    },
    MouseDown {
        x: f64,
        y: f64,
        button: BrowserMouseButton,
        click_count: u8,
    },
    MouseUp {
        x: f64,
        y: f64,
        button: BrowserMouseButton,
        click_count: u8,
    },
    MouseWheel {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    },
    KeyDown {
        key: String,
        code: String,
        key_code: u32,
        modifiers: u8,
    },
    KeyUp {
        key: String,
        code: String,
        key_code: u32,
        modifiers: u8,
    },
    InsertText {
        text: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserMouseButton {
    None,
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserHostResponseEnvelope {
    pub request_id: BrowserCommandId,
    pub protocol_version: BrowserHostProtocolVersion,
    pub outcome: BrowserHostCommandOutcome,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum BrowserHostCommandOutcome {
    Succeeded(Box<BrowserHostCommandResult>),
    Failed(BrowserHostCommandError),
    Cancelled,
    Indeterminate(BrowserHostCommandError),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum BrowserHostCommandResult {
    Empty,
    Pong { monotonic_millis: u64 },
    PageState(BrowserHostPageState),
    Snapshot(BrowserHostSnapshot),
    BinaryPayload(BrowserHostBinaryPayload),
    HitTest(BrowserHostHitTest),
    ClipboardText(BrowserHostClipboardText),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserHostClipboardText {
    pub operation: BrowserClipboardOperation,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserClipboardOperation {
    Copy,
    Cut,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserHostPageState {
    pub tab_id: BrowserTabId,
    pub url: String,
    pub origin: Option<String>,
    pub title: String,
    pub navigation_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserHostSnapshot {
    pub tab_id: BrowserTabId,
    pub snapshot_revision: u64,
    pub root: BrowserSnapshotNode,
    pub returned_nodes: u32,
    pub total_nodes: u32,
    pub text_bytes: u32,
    pub truncated: bool,
    pub continuation_refs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserSnapshotNode {
    pub element_ref: String,
    pub role: Option<String>,
    pub name: Option<String>,
    pub value: Option<String>,
    pub description: Option<String>,
    pub disabled: bool,
    pub focused: bool,
    pub editable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitive_input_kind: Option<BrowserSensitiveInputKind>,
    pub visible: bool,
    pub bounds: Option<BrowserHostRect>,
    pub children: Vec<BrowserSnapshotNode>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserSensitiveInputKind {
    Password,
    OneTimeCode,
    PaymentCard,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserHostRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserHostBinaryPayload {
    pub payload_id: String,
    pub mime_type: String,
    pub byte_length: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserHostHitTest {
    pub frame_sequence: u64,
    pub navigation_revision: u64,
    pub scroll_x: f64,
    pub scroll_y: f64,
    pub element_ref: String,
    pub tag_name: String,
    pub test_id: Option<String>,
    pub stable_id: Option<String>,
    pub aria_role: Option<String>,
    pub aria_name: Option<String>,
    pub text_excerpt: Option<String>,
    pub css_path: String,
    pub ancestor_fingerprint: String,
    pub dom_fingerprint: String,
    pub bounds: BrowserHostRect,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserHostCommandError {
    pub code: String,
    pub message: String,
    pub recoverable: bool,
    pub side_effect_started: bool,
    pub diagnostic: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserHostEventEnvelope {
    pub protocol_version: BrowserHostProtocolVersion,
    pub sequence: u64,
    pub event: BrowserHostEvent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum BrowserHostEvent {
    Ready(BrowserHostHandshake),
    PageUpdated(BrowserHostPageState),
    PageCrashed {
        tab_id: BrowserTabId,
        diagnostic: Option<String>,
    },
    Console {
        tab_id: BrowserTabId,
        level: String,
        text: String,
    },
    Dialog {
        tab_id: BrowserTabId,
        dialog_type: String,
        message: String,
    },
    Download {
        tab_id: BrowserTabId,
        suggested_filename: String,
        state: String,
    },
    ScreencastFrame(BrowserScreencastFrame),
    BinaryPayloadReady(BrowserHostBinaryPayload),
    Heartbeat {
        monotonic_millis: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserScreencastFrame {
    pub tab_id: BrowserTabId,
    pub frame_sequence: u64,
    pub navigation_revision: u64,
    pub payload_id: String,
    pub mime_type: String,
    pub byte_length: u64,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
    pub device_scale_factor_millis: u32,
}
