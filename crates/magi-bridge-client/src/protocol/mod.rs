mod adapter;
mod anthropic;
mod capability;
mod conformance;
mod openai_chat;
pub mod streaming;
mod tool_name_codec;
mod utils;

pub use adapter::{AdaptedRequest, AdaptedResponse, ProviderAdapter, ProviderFamily};
pub use anthropic::AnthropicMessagesAdapter;
pub use capability::{
    ModelCapabilityProfile, ThinkingKind, UNKNOWN_MODEL_DEFAULT, resolve_capability_profile,
};
pub use conformance::{ConformanceValidator, ConformanceViolation};
pub use openai_chat::OpenAiChatCompletionsAdapter;
pub use streaming::{SseLineParser, StreamAccumulator, parse_stream_event};
pub use tool_name_codec::ProviderToolNameCodec;
pub use utils::{
    convert_tool_calls_to_openai, convert_tool_results_to_openai, estimate_message_tokens,
    serialize_tool_definitions,
};
