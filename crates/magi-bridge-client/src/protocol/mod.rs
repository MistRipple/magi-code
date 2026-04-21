mod adapter;
mod anthropic;
mod capability;
mod conformance;
mod openai_chat;
mod openai_responses;
pub mod streaming;
mod utils;

pub use adapter::{ProviderAdapter, ProviderFamily, AdaptedRequest, AdaptedResponse};
pub use anthropic::AnthropicMessagesAdapter;
pub use capability::{CapabilityRegistry, ModelCapability};
pub use conformance::{ConformanceValidator, ConformanceViolation};
pub use openai_chat::OpenAiChatCompletionsAdapter;
pub use openai_responses::OpenAiResponsesAdapter;
pub use streaming::{SseLineParser, StreamAccumulator, parse_stream_event};
pub use utils::{
    estimate_message_tokens, serialize_tool_definitions, convert_tool_calls_to_openai,
    convert_tool_results_to_openai,
};
