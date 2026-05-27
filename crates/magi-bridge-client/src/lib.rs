pub mod auto_compaction;
pub mod base_adapter;
pub mod cache_boundary;
mod clients;
pub mod conversation_compaction;
pub mod decision_engine;
mod dispatch;
pub mod execution_outcome;
pub mod final_text_policy;
mod http_model_client;
pub mod llm_types;
mod local_process_protocol;
mod mcp_client;
mod mcp_loopback;
pub mod micro_compaction;
mod model_loopback;
pub mod orchestrator_adapter;
pub mod orchestrator_termination;
pub mod protocol;
pub mod round_policy;
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
pub use http_model_client::{HttpModelBridgeClient, HttpModelBridgeProtocol};
pub use local_process_protocol::{
    BridgeServerCommandCapabilityProfile, BridgeServerContextResolutionBoundary,
    BridgeServerHandshake, BridgeServerHealth, BridgeServerKind, BridgeServerServiceCatalog,
    BridgeServerServiceDescriptor, BridgeServerSessionDescriptor, BridgeServerShellManifest,
    BridgeServerShellProfile, BridgeServerWorkspaceContext, LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD,
    LOCAL_BRIDGE_HANDSHAKE_METHOD, LOCAL_BRIDGE_HEALTH_METHOD, LOCAL_BRIDGE_PROTOCOL_VERSION,
};
pub use mcp_client::{McpServerConfig, McpToolInfo, StdioMcpBridgeClient};
pub use mcp_loopback::{run_mcp_bridge_loopback_server, run_mcp_manager_server};
pub use model_loopback::run_model_bridge_loopback_server;
pub use transport::{JsonRpcStdioTransport, JsonRpcStdioTransportConfig};
pub use types::{
    BridgeBindingDispatchPlan, BridgeBindingKind, BridgeBindingReference, BridgeClientError,
    BridgeDispatchAction, BridgeDispatchInput, BridgeDispatchResult, BridgeErrorLayer,
    BridgeResponse, BridgeTransport, BridgeTransportError, BridgeTransportRequest,
    BridgeTransportResponse, ChatCompletionPayload, ChatMessage, ChatToolCall, ChatToolChoice,
    ChatToolChoiceFunction, ChatToolDefinition, ChatToolFunction, ChatToolFunctionDefinition,
    LOOPBACK_MCP_SERVER_NAME, LOOPBACK_MCP_TOOL_NAME, LOOPBACK_MODEL_PROVIDER, McpBridgeClient,
    McpManagerDescribeServerResponse, McpManagerLifecycleEvent, McpManagerLifecycleEventKind,
    McpManagerListServersResponse, McpManagerServerHealthUpdateRequest,
    McpManagerServerLifecycleState, McpManagerServerOperationResponse,
    McpManagerServerRegistrationRequest, McpManagerServerSelectionRequest, McpToolCallRequest,
    ModelBridgeClient, ModelInvocationRequest, ModelStreamingDelta,
};
