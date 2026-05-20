use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::llm_types::{LlmMessageParams, LlmResponse, LlmUsage};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFamily {
    OpenAiChat,
    Anthropic,
    Gemini,
}

impl ProviderFamily {
    pub fn from_provider_string(provider: &str) -> Self {
        let lower = provider.to_ascii_lowercase();
        if lower.contains("anthropic") || lower.contains("claude") {
            Self::Anthropic
        } else if lower.contains("gemini") || lower.contains("google") {
            Self::Gemini
        } else {
            Self::OpenAiChat
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_family_detects_anthropic_aliases() {
        assert_eq!(
            ProviderFamily::from_provider_string("anthropic"),
            ProviderFamily::Anthropic
        );
        assert_eq!(
            ProviderFamily::from_provider_string("claude-sonnet"),
            ProviderFamily::Anthropic
        );
    }

    #[test]
    fn provider_family_detects_google_aliases_as_gemini() {
        assert_eq!(
            ProviderFamily::from_provider_string("gemini"),
            ProviderFamily::Gemini
        );
        assert_eq!(
            ProviderFamily::from_provider_string("google-ai"),
            ProviderFamily::Gemini
        );
    }
}

#[derive(Clone, Debug)]
pub struct AdaptedRequest {
    pub url_path: String,
    pub body: Value,
    pub extra_headers: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct AdaptedResponse {
    pub content: String,
    pub thinking: Option<String>,
    pub tool_calls: Vec<crate::llm_types::ToolCall>,
    pub usage: LlmUsage,
    pub stop_reason: String,
    pub raw: Option<Value>,
}

impl From<AdaptedResponse> for LlmResponse {
    fn from(r: AdaptedResponse) -> Self {
        LlmResponse {
            content: r.content,
            thinking: r.thinking,
            tool_calls: r.tool_calls,
            usage: r.usage,
            stop_reason: r.stop_reason,
        }
    }
}

pub trait ProviderAdapter: Send + Sync {
    fn family(&self) -> ProviderFamily;

    fn build_request(
        &self,
        params: &LlmMessageParams,
        model: &str,
    ) -> Result<AdaptedRequest, String>;

    fn parse_response(&self, status: u16, body: &str) -> Result<AdaptedResponse, String>;

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn max_output_tokens_field(&self) -> &str {
        "max_tokens"
    }
}
