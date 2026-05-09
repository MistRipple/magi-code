use crate::local_process_protocol::BridgeServerServiceDescriptor;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use thiserror::Error;

pub const LOOPBACK_MODEL_PROVIDER: &str = "loopback-model";
pub const LOOPBACK_MCP_SERVER_NAME: &str = "loopback-mcp";
pub const LOOPBACK_MCP_TOOL_NAME: &str = "echo.inspect";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum BridgeBindingKind {
    Model,
    Mcp,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum BridgeDispatchAction {
    ModelPrompt,
    McpToolCall,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum BridgeErrorLayer {
    Transport,
    Protocol,
    RemoteBusiness,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelInvocationRequest {
    pub provider: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<ChatMessage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ChatToolDefinition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ChatToolChoice>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatToolChoice {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ChatToolChoiceFunction,
}

impl ChatToolChoice {
    pub fn force_function(name: impl Into<String>) -> Self {
        Self {
            kind: "function".to_string(),
            function: ChatToolChoiceFunction { name: name.into() },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatToolChoiceFunction {
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ChatToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ChatToolFunction,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatToolFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatToolDefinition {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ChatToolFunctionDefinition,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatToolFunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatCompletionPayload {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(
        default,
        rename = "reasoning_content",
        alias = "thinking",
        skip_serializing_if = "Option::is_none"
    )]
    pub thinking: Option<String>,
    #[serde(default)]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub usage: Option<Value>,
    #[serde(default)]
    pub tool_calls: Vec<ChatToolCall>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelStreamingDelta {
    pub content: String,
    pub thinking: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpToolCallRequest {
    pub server_name: String,
    pub tool_name: String,
    pub input: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpManagerServerSelectionRequest {
    #[serde(default)]
    pub server_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpManagerServerHealthUpdateRequest {
    pub server_name: String,
    pub health_status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpManagerServerRegistrationRequest {
    pub server_name: String,
    pub server_version: String,
    pub capability_profile: String,
    pub selection_key: String,
    #[serde(default = "default_mcp_manager_implementation_source")]
    pub implementation_source: String,
    #[serde(default = "default_mcp_manager_registration_health")]
    pub health_status: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub tool_names: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpManagerLifecycleEvent {
    pub server_name: String,
    pub event_kind: McpManagerLifecycleEventKind,
    pub previous_state: McpManagerServerLifecycleState,
    pub new_state: McpManagerServerLifecycleState,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpManagerLifecycleEventKind {
    Registered,
    Started,
    Stopped,
    HealthChanged,
    Deregistered,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpManagerServerLifecycleState {
    Registered,
    Running,
    Stopped,
    Failed,
    Deregistered,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpManagerListServersResponse {
    pub manager: BridgeServerServiceDescriptor,
    pub servers: Vec<BridgeServerServiceDescriptor>,
    pub selection_targets: Vec<String>,
    pub default_route_status: String,
    pub default_route_target: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpManagerDescribeServerResponse {
    pub manager: BridgeServerServiceDescriptor,
    pub server: BridgeServerServiceDescriptor,
    pub lifecycle_events: Vec<McpManagerLifecycleEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpManagerServerOperationResponse {
    pub operation: String,
    pub manager: BridgeServerServiceDescriptor,
    pub server: BridgeServerServiceDescriptor,
    pub lifecycle_event_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_event: Option<McpManagerLifecycleEvent>,
    pub server_events: Vec<McpManagerLifecycleEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeBindingReference {
    pub binding_id: String,
    pub tool_name: String,
    pub bridge_kind: BridgeBindingKind,
    pub dispatch_action: BridgeDispatchAction,
    pub bridge_target: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeBindingDispatchPlan {
    pub source_skill_ids: Vec<String>,
    pub bindings: Vec<BridgeBindingReference>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeDispatchInput {
    pub binding_id: String,
    pub payload: String,
    pub working_directory: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeDispatchResult {
    pub binding_id: String,
    pub bridge_kind: BridgeBindingKind,
    pub dispatch_action: BridgeDispatchAction,
    pub response: BridgeResponse,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeResponse {
    pub ok: bool,
    pub payload: String,
}

impl BridgeResponse {
    pub fn parse_chat_payload(&self) -> ChatCompletionPayload {
        serde_json::from_str::<ChatCompletionPayload>(&self.payload).unwrap_or(
            ChatCompletionPayload {
                content: Some(self.payload.clone()),
                thinking: None,
                finish_reason: None,
                usage: None,
                tool_calls: Vec::new(),
            },
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeTransportRequest {
    pub method: String,
    pub params: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeTransportResponse {
    pub payload: Value,
}

#[derive(Debug, Error)]
pub enum BridgeTransportError {
    #[error("transport layer error: {message}")]
    Transport { message: String },
    #[error("protocol layer error: {message}")]
    Protocol { message: String },
    #[error("remote business error [{code}]: {message}")]
    RemoteBusiness {
        code: i64,
        message: String,
        data: Option<Value>,
    },
}

impl BridgeTransportError {
    pub fn layer(&self) -> BridgeErrorLayer {
        match self {
            Self::Transport { .. } => BridgeErrorLayer::Transport,
            Self::Protocol { .. } => BridgeErrorLayer::Protocol,
            Self::RemoteBusiness { .. } => BridgeErrorLayer::RemoteBusiness,
        }
    }

    pub fn code(&self) -> Option<i64> {
        match self {
            Self::RemoteBusiness { code, .. } => Some(*code),
            Self::Transport { .. } | Self::Protocol { .. } => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum BridgeClientError {
    #[error("桥接调用失败[{layer:?}]: {message}")]
    CallFailed {
        layer: BridgeErrorLayer,
        code: Option<i64>,
        message: String,
    },
    #[error("无效的宿主桥接目标: binding={binding_id}, target={bridge_target}")]
    InvalidBindingTarget {
        binding_id: String,
        bridge_target: String,
    },
    #[error(
        "桥接绑定类型与派发动作不兼容: binding={binding_id}, kind={bridge_kind:?}, action={dispatch_action:?}"
    )]
    IncompatibleBindingAction {
        binding_id: String,
        bridge_kind: BridgeBindingKind,
        dispatch_action: BridgeDispatchAction,
    },
    #[error("缺少桥接 client: {bridge_kind:?}")]
    MissingClient { bridge_kind: BridgeBindingKind },
    #[error("缺少桥接绑定: {binding_id}")]
    MissingBinding { binding_id: String },
    #[error("缺少宿主工作目录: {binding_id}")]
    MissingWorkingDirectory { binding_id: String },
}

impl BridgeClientError {
    pub fn layer(&self) -> Option<BridgeErrorLayer> {
        match self {
            Self::CallFailed { layer, .. } => Some(*layer),
            Self::InvalidBindingTarget { .. }
            | Self::IncompatibleBindingAction { .. }
            | Self::MissingClient { .. }
            | Self::MissingBinding { .. }
            | Self::MissingWorkingDirectory { .. } => None,
        }
    }

    pub fn code(&self) -> Option<i64> {
        match self {
            Self::CallFailed { code, .. } => *code,
            Self::InvalidBindingTarget { .. }
            | Self::IncompatibleBindingAction { .. }
            | Self::MissingClient { .. }
            | Self::MissingBinding { .. }
            | Self::MissingWorkingDirectory { .. } => None,
        }
    }
}

pub trait BridgeTransport: Send + Sync {
    fn call(
        &self,
        request: BridgeTransportRequest,
    ) -> Result<BridgeTransportResponse, BridgeTransportError>;
}

pub trait ModelBridgeClient: Send + Sync {
    fn invoke(&self, request: ModelInvocationRequest) -> Result<BridgeResponse, BridgeClientError>;

    /// 流式调用 LLM,每次收到内容或 thinking 增量时调用 `on_delta` 回调并传入已累积快照。
    /// 实现方必须显式声明流式行为:真流式实现接收 SSE 增量,非流式实现必须返回错误而非静默降级。
    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<BridgeResponse, BridgeClientError>;
}

pub trait McpBridgeClient: Send + Sync {
    fn call_tool(&self, request: McpToolCallRequest) -> Result<BridgeResponse, BridgeClientError>;
}

pub(crate) type SharedBridgeTransport = Arc<dyn BridgeTransport>;

fn default_mcp_manager_implementation_source() -> String {
    "loopback-server-prehost".to_string()
}

fn default_mcp_manager_registration_health() -> String {
    "healthy".to_string()
}

impl std::fmt::Display for McpManagerLifecycleEventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registered => write!(f, "registered"),
            Self::Started => write!(f, "started"),
            Self::Stopped => write!(f, "stopped"),
            Self::HealthChanged => write!(f, "health_changed"),
            Self::Deregistered => write!(f, "deregistered"),
        }
    }
}

impl std::fmt::Display for McpManagerServerLifecycleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registered => write!(f, "registered"),
            Self::Running => write!(f, "running"),
            Self::Stopped => write!(f, "stopped"),
            Self::Failed => write!(f, "failed"),
            Self::Deregistered => write!(f, "deregistered"),
        }
    }
}
