pub mod cache_boundary;
mod clients;
mod dispatch;
mod executor_capabilities;
mod executor_receipt;
pub mod final_text_policy;
mod http_model_client;
mod image_generation;
pub mod llm_types;
mod local_process_protocol;
mod mcp_client;
mod mcp_loopback;
mod model_loopback;
pub mod protocol;
pub mod structured_dispatch;
pub mod tool_concurrency;
mod transport;
mod types;

#[cfg(test)]
mod tests;

pub use clients::{
    JsonRpcBridgeServerProbeClient, JsonRpcMcpBridgeClient, JsonRpcMcpManagerClient,
    JsonRpcModelBridgeClient,
};
pub use dispatch::BridgeDispatchRuntime;
pub use executor_capabilities::{
    native_executor_capabilities, text_executor_capabilities, validate_executor_request,
    validate_executor_response,
};
pub use executor_receipt::{
    EXECUTOR_RECEIPT_KIND, ExecutorCommandEvidence, ExecutorCommandStatus, ExecutorFailureEvidence,
    ExecutorReceipt, executor_receipts,
};

pub use http_model_client::{HttpModelBridgeClient, HttpModelBridgeProtocol};
pub use image_generation::{GeneratedImage, HttpImageGenerationClient, ImageGenerationRequest};
pub use local_process_protocol::{
    BridgeServerCommandCapabilityProfile, BridgeServerContextResolutionBoundary,
    BridgeServerHandshake, BridgeServerHealth, BridgeServerKind, BridgeServerServiceCatalog,
    BridgeServerServiceDescriptor, BridgeServerSessionDescriptor, BridgeServerShellManifest,
    BridgeServerShellProfile, BridgeServerWorkspaceContext, LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD,
    LOCAL_BRIDGE_HANDSHAKE_METHOD, LOCAL_BRIDGE_HEALTH_METHOD, LOCAL_BRIDGE_PROTOCOL_VERSION,
};
pub use magi_app_server_protocol::{
    ExecutorApprovalMode, ExecutorCancellationMode, ExecutorCapabilities, ExecutorContextOwnership,
    ExecutorInputModality, ExecutorResumeMode, ExecutorSteerMode, ExecutorToolMode,
};
pub use mcp_client::{
    HttpMcpServerConfig, McpServerClient, McpServerConfig, McpServerConnectionConfig, McpToolInfo,
    StdioMcpBridgeClient, StreamableHttpMcpBridgeClient,
};
pub use mcp_loopback::{run_mcp_bridge_loopback_server, run_mcp_manager_server};
pub use model_loopback::run_model_bridge_loopback_server;
pub use transport::{JsonRpcStdioTransport, JsonRpcStdioTransportConfig};
pub use types::{
    BridgeBindingDispatchPlan, BridgeBindingKind, BridgeBindingReference, BridgeClientError,
    BridgeDispatchAction, BridgeDispatchInput, BridgeDispatchResponse, BridgeDispatchResult,
    BridgeErrorLayer, BridgeResponse, BridgeTransport, BridgeTransportError,
    BridgeTransportRequest, BridgeTransportResponse, ChatCompletionPayload, ChatMessage,
    ChatMessageOrigin, ChatToolCall, ChatToolChoice, ChatToolChoiceFunction, ChatToolDefinition,
    ChatToolFunction, ChatToolFunctionDefinition, ChatToolOrigin, ContextOverflowInfo,
    LOOPBACK_MCP_SERVER_NAME, LOOPBACK_MCP_TOOL_NAME, LOOPBACK_MODEL_PROVIDER, McpBridgeClient,
    McpManagerDescribeServerResponse, McpManagerLifecycleEvent, McpManagerLifecycleEventKind,
    McpManagerListServersResponse, McpManagerServerHealthUpdateRequest,
    McpManagerServerLifecycleState, McpManagerServerOperationResponse,
    McpManagerServerRegistrationRequest, McpManagerServerSelectionRequest, McpToolCallRequest,
    ModelBridgeClient, ModelExecutionScope, ModelInvocationRequest, ModelProviderContext,
    ModelResponse, ModelResponseStatus, ModelRetryRuntimeEvent, ModelRetryRuntimePhase,
    ModelStreamingDelta, model_invocation_cancelled_error, model_invocation_error_is_cancelled,
};
