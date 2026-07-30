use crate::{llm_types::ImageSource, local_process_protocol::BridgeServerServiceDescriptor};
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
    pub images: Vec<ImageSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ChatToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// assistant 响应携带的提供方私有上下文。
    ///
    /// 该上下文属于整条 assistant message，而不是某个工具调用。运行时只负责
    /// 持久化，协议适配器负责校验并按原协议回放。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_context: Vec<ModelProviderContext>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ChatToolFunction,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatToolFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatToolDefinition {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ChatToolFunctionDefinition,
    /// 工具所属的运行时域。该字段只在 Magi 的模型协议边界使用，不会透传给上游。
    ///
    /// 上游保留字或原生能力与本地工具冲突时，必须依据来源生成 wire name；不能
    /// 通过维护一份名称白名单猜测工具身份。
    #[serde(default)]
    pub origin: ChatToolOrigin,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatToolOrigin {
    /// Magi 内置工具与 Magi 自身的协调能力。
    Builtin,
    /// 外部 MCP server 暴露的工具。
    ExternalMcp,
    /// Skill 安装或激活后提供的自定义工具。
    Skill,
    /// 旧会话或第三方 bridge 生成的未分类工具；不参与 provider 专属改名。
    #[default]
    Unspecified,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatToolFunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatCompletionPayload {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(
        default,
        rename = "reasoning_content",
        alias = "thinking",
        alias = "reasoning",
        skip_serializing_if = "Option::is_none"
    )]
    pub thinking: Option<String>,
    #[serde(default)]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub usage: Option<Value>,
    #[serde(default)]
    pub tool_calls: Vec<ChatToolCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_context: Vec<ModelProviderContext>,
}

/// 提供方私有但必须跨轮回放的模型上下文。
///
/// `data` 保存已经由协议适配器校验过的完整 wire block；业务运行时只负责持久化，
/// 不解释其中字段。重新请求时由同一提供方适配器决定是否以及如何回放。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderContext {
    pub provider: String,
    pub kind: String,
    pub data: Value,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelResponseStatus {
    Completed,
    RequiresToolExecution,
    Incomplete,
}

/// 所有模型协议在 bridge 边界归一化后的唯一响应结构。
///
/// 会话、辅助模型和产品展示层只能消费该结构，不再解析提供方 JSON 字符串。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelResponse {
    pub status: ModelResponseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ChatToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_context: Vec<ModelProviderContext>,
}

impl ModelResponse {
    pub fn completed(content: impl Into<String>) -> Self {
        Self {
            status: ModelResponseStatus::Completed,
            content: Some(content.into()),
            thinking: None,
            tool_calls: Vec::new(),
            usage: None,
            finish_reason: Some("stop".to_string()),
            provider_context: Vec::new(),
        }
    }

    pub fn from_chat_payload(payload: ChatCompletionPayload) -> Self {
        let status = model_response_status(payload.finish_reason.as_deref(), &payload.tool_calls);
        Self {
            status,
            content: payload.content,
            thinking: payload.thinking,
            tool_calls: payload.tool_calls,
            usage: payload.usage,
            finish_reason: payload.finish_reason,
            provider_context: payload.provider_context,
        }
    }

    pub fn is_actionable(&self) -> bool {
        self.content
            .as_deref()
            .is_some_and(|content| !content.trim().is_empty())
            || !self.tool_calls.is_empty()
    }
}

fn model_response_status(
    finish_reason: Option<&str>,
    tool_calls: &[ChatToolCall],
) -> ModelResponseStatus {
    let normalized_finish_reason = finish_reason.map(|reason| reason.trim().to_ascii_lowercase());
    let finish_reason = normalized_finish_reason.as_deref();
    if !tool_calls.is_empty()
        || matches!(
            finish_reason,
            Some("tool_calls" | "tool_use" | "function_call")
        )
    {
        return ModelResponseStatus::RequiresToolExecution;
    }
    if matches!(
        finish_reason,
        Some(
            "length"
                | "max_tokens"
                | "max_output_tokens"
                | "content_filter"
                | "safety"
                | "blocked"
                | "recitation"
                | "error"
                | "cancelled"
                | "canceled"
        )
    ) {
        return ModelResponseStatus::Incomplete;
    }
    ModelResponseStatus::Completed
}

#[cfg(test)]
mod model_response_tests {
    use super::*;

    #[test]
    fn response_status_normalizes_cross_provider_finish_reasons() {
        for reason in [
            "length",
            "MAX_TOKENS",
            "max_output_tokens",
            "content_filter",
            "SAFETY",
            "RECITATION",
            "error",
            "cancelled",
        ] {
            assert_eq!(
                model_response_status(Some(reason), &[]),
                ModelResponseStatus::Incomplete,
                "finish_reason={reason}"
            );
        }
        assert_eq!(
            model_response_status(Some("TOOL_USE"), &[]),
            ModelResponseStatus::RequiresToolExecution
        );
        assert_eq!(
            model_response_status(Some("STOP"), &[]),
            ModelResponseStatus::Completed
        );
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelStreamingDelta {
    pub content: String,
    pub thinking: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ModelRetryRuntimePhase {
    Scheduled,
    AttemptStarted,
    Settled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRetryRuntimeEvent {
    pub phase: ModelRetryRuntimePhase,
    pub attempt: usize,
    pub max_attempts: usize,
    pub delay_ms: Option<u64>,
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
    pub response: BridgeDispatchResponse,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "response")]
pub enum BridgeDispatchResponse {
    Model(ModelResponse),
    Mcp(BridgeResponse),
}

impl BridgeDispatchResponse {
    pub fn is_success(&self) -> bool {
        match self {
            Self::Model(_) => true,
            Self::Mcp(response) => response.ok,
        }
    }

    pub fn payload(&self) -> &str {
        match self {
            Self::Model(response) => response.content.as_deref().unwrap_or_default(),
            Self::Mcp(response) => &response.payload,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeResponse {
    pub ok: bool,
    pub payload: String,
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
    #[error("桥接调用失败[{layer:?}]: {message} (http_status={http_status})")]
    HttpStatusFailed {
        layer: BridgeErrorLayer,
        code: Option<i64>,
        http_status: u16,
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
            Self::HttpStatusFailed { layer, .. } => Some(*layer),
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
            Self::HttpStatusFailed { code, .. } => *code,
            Self::InvalidBindingTarget { .. }
            | Self::IncompatibleBindingAction { .. }
            | Self::MissingClient { .. }
            | Self::MissingBinding { .. }
            | Self::MissingWorkingDirectory { .. } => None,
        }
    }

    pub fn http_status(&self) -> Option<u16> {
        match self {
            Self::HttpStatusFailed { http_status, .. } => Some(*http_status),
            Self::CallFailed { .. }
            | Self::InvalidBindingTarget { .. }
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
    fn invoke(&self, request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError>;

    fn invoke_with_cancellation(
        &self,
        request: ModelInvocationRequest,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        if is_cancelled() {
            return Err(model_invocation_cancelled_error());
        }
        self.invoke(request)
    }

    /// 流式调用 LLM,每次收到内容或 thinking 增量时调用 `on_delta` 回调并传入已累积快照。
    /// 实现方必须显式声明流式行为:真流式实现接收 SSE 增量,非流式实现必须返回错误而非静默降级。
    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError>;

    fn invoke_streaming_with_retry_events(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
        _on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_streaming(request, on_delta)
    }

    fn invoke_streaming_with_cancellation(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
        on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        if is_cancelled() {
            return Err(model_invocation_cancelled_error());
        }
        self.invoke_streaming_with_retry_events(request, on_delta, on_retry)
    }
}

pub fn model_invocation_cancelled_error() -> BridgeClientError {
    BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Transport,
        code: Some(-32800),
        message: "model invocation cancelled".to_string(),
    }
}

pub fn model_invocation_error_is_cancelled(error: &BridgeClientError) -> bool {
    matches!(
        error,
        BridgeClientError::CallFailed {
            code: Some(-32800),
            ..
        }
    )
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
