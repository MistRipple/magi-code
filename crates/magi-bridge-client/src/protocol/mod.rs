mod adapter;
mod anthropic;
mod capability;
mod conformance;
mod openai_chat;
pub mod streaming;
mod utils;

pub use adapter::{AdaptedRequest, AdaptedResponse, ProviderAdapter, ProviderFamily};
pub use anthropic::AnthropicMessagesAdapter;
pub use capability::{CapabilityRegistry, ModelCapability, supports_extended_thinking};
pub use conformance::{ConformanceValidator, ConformanceViolation};
pub use openai_chat::OpenAiChatCompletionsAdapter;
pub use streaming::{SseLineParser, StreamAccumulator, parse_stream_event};
pub use utils::{
    convert_tool_calls_to_openai, convert_tool_results_to_openai, estimate_message_tokens,
    serialize_tool_definitions,
};
