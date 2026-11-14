#[cfg(test)]
use crate::llm_types::{LlmMessage, LlmMessageContent};
use crate::llm_types::{ToolChoice, canonicalize_json};
use crate::prompt_ir::PromptIr;
use crate::prompt_ir::compile_prompt_ir;
#[cfg(test)]
use crate::prompt_ir::normalize_chat_tool_call_history;
#[cfg(test)]
use crate::protocol::AdaptedResponse;
use crate::protocol::streaming::{
    SseLineParser, StreamAccumulator, parse_stream_event, parse_stream_provider_context,
};
use crate::protocol::{
    AdaptedRequest, AnthropicMessagesAdapter, OpenAiChatCompletionsAdapter, OpenAiResponsesAdapter,
    ProviderAdapter, ProviderFamily, ProviderToolNameCodec,
};
#[cfg(test)]
use crate::types::ChatMessageOrigin;
use crate::types::{
    BridgeClientError, BridgeErrorLayer, ModelBridgeClient, ModelInvocationRequest, ModelResponse,
    ModelRetryRuntimeEvent, ModelRetryRuntimePhase, ModelStreamingDelta,
    model_invocation_cancelled_error, model_invocation_error_is_cancelled,
};
use crate::{ModelInvocationOptions, ModelTransportEvent, ModelTransportObserver};
use magi_app_server_protocol::{ModelTransportOutcome, ModelTransportUsage};
use magi_usage_authority::ReasoningEffort;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant, SystemTime};

mod config;
use config::*;
pub use config::{
    HttpModelBridgeProtocol, ProviderCacheContract, ProviderCacheControl, ProviderCacheRouteKey,
    ProviderContinuation,
};
mod client;
pub use client::HttpModelBridgeClient;
use client::*;
mod usage;
#[cfg(test)]
pub use usage::ModelProbeRequest;
use usage::*;
mod gate;
use gate::*;
mod retry;
use retry::*;
mod transport;
use transport::*;
mod streaming;
use streaming::*;
mod endpoints;
use endpoints::*;

// 上下文压缩使用非流式辅助模型请求；30 秒不足以覆盖真实 Provider 的长历史摘要，
// 但取消仍由 execute_cancellable_http_post 的 cancellation channel 及时打断。
#[cfg(test)]
mod tests;
