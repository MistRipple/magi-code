mod dispatch;
mod executor_capabilities;
mod executor_receipt;
mod http_model_client;
mod image_generation;
pub mod llm_types;
mod mcp_client;
pub mod prompt_ir;
pub mod protocol;
pub mod reasoning;
mod request_fingerprint;
pub mod tool_concurrency;
mod types;

#[cfg(test)]
mod tests;

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
pub use magi_app_server_protocol::{
    ExecutorApprovalMode, ExecutorCancellationMode, ExecutorCapabilities, ExecutorContextOwnership,
    ExecutorInputModality, ExecutorResumeMode, ExecutorSteerMode, ExecutorToolMode,
};
pub use mcp_client::{
    HttpMcpServerConfig, McpServerClient, McpServerConfig, McpServerConnectionConfig, McpToolInfo,
    StdioMcpBridgeClient, StreamableHttpMcpBridgeClient,
};
pub use types::{
    BridgeBindingDispatchPlan, BridgeBindingKind, BridgeBindingReference, BridgeClientError,
    BridgeDispatchAction, BridgeDispatchInput, BridgeDispatchResponse, BridgeDispatchResult,
    BridgeErrorLayer, BridgeResponse, ChatCompletionPayload, ChatMessage, ChatMessageOrigin,
    ChatToolCall, ChatToolChoice, ChatToolChoiceFunction, ChatToolDefinition, ChatToolFunction,
    ChatToolFunctionDefinition, ChatToolOrigin, ContextOverflowInfo, McpBridgeClient,
    McpToolCallRequest, ModelBridgeClient, ModelCacheIdentity, ModelExecutionScope,
    ModelInvocationOptions, ModelInvocationRequest, ModelProviderContext, ModelResponse,
    ModelResponseStatus, ModelRetryRuntimeEvent, ModelRetryRuntimePhase, ModelStreamingDelta,
    ModelTransportEvent, ModelTransportObserver, ModelTransportRecorder, canonical_tool_surface,
    model_invocation_cancelled_error, model_invocation_error_is_cancelled,
};
