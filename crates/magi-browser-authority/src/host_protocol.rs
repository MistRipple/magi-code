use crate::{BrowserDeviceType, BrowserNormalizedRect};
use magi_core::{BrowserCommandId, BrowserLeaseId, BrowserSessionId, BrowserTabId};
use serde::{Deserialize, Serialize};

pub const BROWSER_HOST_PROTOCOL_MAJOR: u16 = 3;
pub const BROWSER_HOST_PROTOCOL_MINOR: u16 = 2;
pub const DEFAULT_BROWSER_SNAPSHOT_NODE_LIMIT: u32 = 160;
pub const DEFAULT_BROWSER_SNAPSHOT_TEXT_LIMIT_BYTES: u32 = 16 * 1024;

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
    pub desktop_version: String,
    pub electron_version: String,
    pub chromium_version: String,
    pub process_id: u32,
    pub desktop_epoch: String,
    pub worker_epoch: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserSurfaceBinding {
    pub desktop_epoch: String,
    pub window_id: String,
    pub surface_id: String,
    pub surface_revision: u64,
    pub tab_id: BrowserTabId,
    pub web_contents_id: u32,
    pub target_id: String,
    pub browser_context_id: String,
    pub navigation_revision: u64,
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
        browser_session_id: BrowserSessionId,
        initial_url: String,
        logical_viewport: BrowserLogicalViewport,
        navigation_revision: u64,
        snapshot_revision: u64,
        #[serde(default)]
        allow_page_eviction: bool,
    },
    /// 将 Authority 中的逻辑 Tab 物化为 Chromium Page；已存在的 Page 则只做激活。
    /// Browser Host 不在启动时登记全部逻辑 Tab，避免恢复 Tab 占用 Host 的物理记录槽位。
    RestorePage {
        tab_id: BrowserTabId,
        browser_session_id: BrowserSessionId,
        initial_url: String,
        logical_viewport: BrowserLogicalViewport,
        navigation_revision: u64,
        snapshot_revision: u64,
        #[serde(default)]
        allow_page_eviction: bool,
    },
    /// 调整 Chromium 自身的设备仿真视口。原生显示区域始终由桌面父容器管理。
    SetLogicalViewport {
        tab_id: BrowserTabId,
        viewport: BrowserLogicalViewport,
    },
    /// 读取当前桌面 Surface 的运行态视口。该数据不进入 Authority 持久化。
    GetLogicalViewport {
        tab_id: BrowserTabId,
    },
    /// 将 Authority 中的持久化标记同步到当前 Chromium 文档的页面标记层。
    /// 这是浏览器 UI 状态，不改变页面内容，也不进入会话共享布局状态。
    SetAnnotations {
        tab_id: BrowserTabId,
        annotations: Vec<serde_json::Value>,
    },
    ClosePage {
        tab_id: BrowserTabId,
    },
    Navigate {
        tab_id: BrowserTabId,
        control: BrowserHostControl,
        navigation: BrowserNavigation,
    },
    Snapshot {
        tab_id: BrowserTabId,
        navigation_revision: u64,
        snapshot_revision: u64,
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
        submit_key: Option<String>,
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
    /// 受控的 Chromium DevTools 高级操作。具体动作仍由 Magi 工具目录提供，
    /// Host 只执行白名单 operation，避免把任意 CDP 调用暴露给模型。
    Devtools {
        tab_id: BrowserTabId,
        control: Option<BrowserHostControl>,
        operation: String,
        arguments: serde_json::Value,
    },
    Screenshot {
        tab_id: BrowserTabId,
        target: Option<BrowserSnapshotTarget>,
        clip: Option<BrowserNormalizedRect>,
        full_page: bool,
        format: BrowserScreenshotFormat,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quality: Option<u8>,
    },
    HitTest {
        tab_id: BrowserTabId,
        navigation_revision: u64,
        x: f64,
        y: f64,
    },
    UpdateControl {
        tab_id: BrowserTabId,
        surface_id: String,
        control: BrowserHostControlUpdate,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum BrowserHostControlUpdate {
    Agent {
        lease_id: BrowserLeaseId,
        fence: u64,
    },
    User {
        fence: u64,
    },
    Released {
        fence: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum BrowserLogicalViewport {
    Auto,
    Fixed {
        width: u32,
        height: u32,
        device_scale_factor_millis: u32,
        device_type: BrowserDeviceType,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BrowserNavigation {
    Url {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        handle_before_unload: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        init_script: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u32>,
    },
    Back {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u32>,
    },
    Forward {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u32>,
    },
    Reload {
        #[serde(default)]
        ignore_cache: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        handle_before_unload: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u32>,
    },
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
    Webp,
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
    Json { value: serde_json::Value },
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
    pub navigation_revision: u64,
    pub snapshot_revision: u64,
    pub root: BrowserSnapshotNode,
    pub returned_nodes: u32,
    pub total_nodes: u32,
    pub text_bytes: u32,
    pub truncated: bool,
    pub continuation_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accessibility_tree: Vec<serde_json::Value>,
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
    pub navigation_revision: u64,
    pub viewport_width: u32,
    pub viewport_height: u32,
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
    PrimarySurfaceChanged {
        binding: BrowserSurfaceBinding,
    },
    UserTakeover {
        binding: BrowserSurfaceBinding,
    },
    ControlRevoked {
        binding: BrowserSurfaceBinding,
        reason: String,
    },
    PageUpdated {
        binding: BrowserSurfaceBinding,
        page_state: BrowserHostPageState,
    },
    PageFailed {
        binding: BrowserSurfaceBinding,
        reason: String,
    },
    LoadingChanged {
        binding: BrowserSurfaceBinding,
        loading: bool,
    },
    PageCrashed {
        binding: BrowserSurfaceBinding,
        diagnostic: Option<String>,
    },
    Console {
        tab_id: BrowserTabId,
        level: String,
        text: String,
    },
    Dialog {
        tab_id: BrowserTabId,
        dialog_id: u64,
        dialog_type: String,
        message: String,
    },
    Download {
        tab_id: BrowserTabId,
        suggested_filename: String,
        state: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        byte_length: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    PopupBlocked {
        binding: BrowserSurfaceBinding,
        url: String,
    },
    AgentCursor(BrowserAgentCursor),
    BinaryPayloadReady(BrowserHostBinaryPayload),
    Heartbeat {
        monotonic_millis: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserAgentCursor {
    pub tab_id: BrowserTabId,
    pub visible: bool,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub action: Option<BrowserAgentCursorAction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAgentCursorAction {
    Move,
    Click,
    Drag,
    Type,
    Scroll,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_handshake_matches_typescript_contract() {
        let handshake = BrowserHostHandshake {
            protocol_version: BrowserHostProtocolVersion::CURRENT,
            desktop_version: "desktop-test".to_string(),
            electron_version: "electron-test".to_string(),
            chromium_version: "chromium-test".to_string(),
            process_id: 42,
            desktop_epoch: "desktop-epoch".to_string(),
            worker_epoch: "worker-epoch".to_string(),
        };

        assert_eq!(
            serde_json::to_value(handshake).expect("serialize desktop handshake"),
            serde_json::json!({
                "protocol_version": { "major": 3, "minor": 2 },
                "desktop_version": "desktop-test",
                "electron_version": "electron-test",
                "chromium_version": "chromium-test",
                "process_id": 42,
                "desktop_epoch": "desktop-epoch",
                "worker_epoch": "worker-epoch"
            })
        );
    }

    #[test]
    fn create_page_and_json_result_match_typescript_contract() {
        let command = BrowserHostCommand::CreatePage {
            tab_id: BrowserTabId::new("tab-1"),
            browser_session_id: BrowserSessionId::new("browser-session-1"),
            initial_url: "https://example.com/".to_string(),
            logical_viewport: BrowserLogicalViewport::Auto,
            navigation_revision: 3,
            snapshot_revision: 5,
            allow_page_eviction: false,
        };
        assert_eq!(
            serde_json::to_value(command).expect("serialize create_page"),
            serde_json::json!({
                "type": "create_page",
                "payload": {
                    "tab_id": "tab-1",
                    "browser_session_id": "browser-session-1",
                    "initial_url": "https://example.com/",
                    "logical_viewport": { "mode": "auto" },
                    "navigation_revision": 3,
                    "snapshot_revision": 5,
                    "allow_page_eviction": false
                }
            })
        );

        assert_eq!(
            serde_json::to_value(BrowserHostCommandResult::Json {
                value: serde_json::json!({ "title": "Example" }),
            })
            .expect("serialize json result"),
            serde_json::json!({
                "type": "json",
                "payload": { "value": { "title": "Example" } }
            })
        );
        assert_eq!(
            serde_json::to_value(BrowserHostCommand::GetLogicalViewport {
                tab_id: BrowserTabId::new("tab-1"),
            })
            .expect("serialize get logical viewport"),
            serde_json::json!({
                "type": "get_logical_viewport",
                "payload": { "tab_id": "tab-1" }
            })
        );
    }

    #[test]
    fn surface_control_events_carry_the_complete_binding() {
        let binding = BrowserSurfaceBinding {
            desktop_epoch: "desktop-epoch".to_string(),
            window_id: "window-1".to_string(),
            surface_id: "surface-1".to_string(),
            surface_revision: 7,
            tab_id: BrowserTabId::new("tab-1"),
            web_contents_id: 23,
            target_id: "target-1".to_string(),
            browser_context_id: "context-1".to_string(),
            navigation_revision: 11,
        };
        assert_eq!(
            serde_json::to_value(BrowserHostEvent::PrimarySurfaceChanged {
                binding: binding.clone(),
            })
            .expect("serialize primary surface event"),
            serde_json::json!({
                "type": "primary_surface_changed",
                "payload": { "binding": binding }
            })
        );
        assert_eq!(
            serde_json::to_value(BrowserHostEvent::UserTakeover {
                binding: binding.clone(),
            })
            .expect("serialize user takeover event"),
            serde_json::json!({
                "type": "user_takeover",
                "payload": { "binding": binding }
            })
        );
        assert_eq!(
            serde_json::to_value(BrowserHostEvent::ControlRevoked {
                binding: binding.clone(),
                reason: "user_takeover".to_string(),
            })
            .expect("serialize control revoked event"),
            serde_json::json!({
                "type": "control_revoked",
                "payload": {
                    "binding": binding,
                    "reason": "user_takeover"
                }
            })
        );
        assert_eq!(
            serde_json::to_value(BrowserHostEvent::PageCrashed {
                binding,
                diagnostic: Some("render-process-gone".to_string()),
            })
            .expect("serialize page crashed event"),
            serde_json::json!({
                "type": "page_crashed",
                "payload": {
                    "binding": {
                        "desktop_epoch": "desktop-epoch",
                        "window_id": "window-1",
                        "surface_id": "surface-1",
                        "surface_revision": 7,
                        "tab_id": "tab-1",
                        "web_contents_id": 23,
                        "target_id": "target-1",
                        "browser_context_id": "context-1",
                        "navigation_revision": 11
                    },
                    "diagnostic": "render-process-gone"
                }
            })
        );
        assert_eq!(
            serde_json::to_value(BrowserHostEvent::PageUpdated {
                binding: BrowserSurfaceBinding {
                    desktop_epoch: "desktop-epoch".to_string(),
                    window_id: "window-1".to_string(),
                    surface_id: "surface-1".to_string(),
                    surface_revision: 7,
                    tab_id: BrowserTabId::new("tab-1"),
                    web_contents_id: 23,
                    target_id: "target-1".to_string(),
                    browser_context_id: "context-1".to_string(),
                    navigation_revision: 11,
                },
                page_state: BrowserHostPageState {
                    tab_id: BrowserTabId::new("tab-1"),
                    url: "https://example.com/".to_string(),
                    origin: Some("https://example.com".to_string()),
                    title: "Example".to_string(),
                    navigation_revision: 11,
                },
            })
            .expect("serialize page updated event"),
            serde_json::json!({
                "type": "page_updated",
                "payload": {
                    "binding": {
                        "desktop_epoch": "desktop-epoch",
                        "window_id": "window-1",
                        "surface_id": "surface-1",
                        "surface_revision": 7,
                        "tab_id": "tab-1",
                        "web_contents_id": 23,
                        "target_id": "target-1",
                        "browser_context_id": "context-1",
                        "navigation_revision": 11
                    },
                    "page_state": {
                        "tab_id": "tab-1",
                        "url": "https://example.com/",
                        "origin": "https://example.com",
                        "title": "Example",
                        "navigation_revision": 11
                    }
                }
            })
        );
    }
}
