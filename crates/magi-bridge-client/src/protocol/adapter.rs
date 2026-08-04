use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

use crate::llm_types::{LlmMessageParams, LlmResponse, LlmUsage, ToolCall};
use crate::types::{ChatCompletionPayload, ChatToolCall, ChatToolFunction, ModelResponse};

use super::utils::compatible_tool_call_id;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFamily {
    OpenAiChat,
    OpenAiResponses,
    Anthropic,
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
    pub provider_context: Vec<crate::types::ModelProviderContext>,
}

fn normalize_tool_calls(tool_calls: Vec<ToolCall>) -> Vec<ToolCall> {
    let mut normalized = Vec::with_capacity(tool_calls.len());
    let mut seen_by_id = HashMap::<String, (String, String)>::new();
    let mut used_ids = HashSet::<String>::new();

    for (index, mut tool_call) in tool_calls.into_iter().enumerate() {
        let arguments = tool_call.arguments_for_wire().to_string();
        let original_id = tool_call.id.trim().to_string();

        if let Some((seen_name, seen_arguments)) = seen_by_id.get(&original_id)
            && *seen_name == tool_call.name
            && *seen_arguments == arguments
        {
            continue;
        }

        if original_id.is_empty() || used_ids.contains(&original_id) {
            let base = compatible_tool_call_id(index, &tool_call.name, &arguments);
            let mut candidate = base.clone();
            let mut suffix = 1usize;
            while used_ids.contains(&candidate) {
                candidate = format!("{base}_{suffix}");
                suffix += 1;
            }
            tool_call.id = candidate;
        } else {
            tool_call.id = original_id.clone();
        }

        if !original_id.is_empty() {
            seen_by_id.insert(original_id, (tool_call.name.clone(), arguments));
        }
        used_ids.insert(tool_call.id.clone());
        normalized.push(tool_call);
    }

    normalized
}

impl From<AdaptedResponse> for LlmResponse {
    fn from(mut r: AdaptedResponse) -> Self {
        r.tool_calls = normalize_tool_calls(r.tool_calls);
        LlmResponse {
            content: r.content,
            thinking: r.thinking,
            tool_calls: r.tool_calls,
            usage: r.usage,
            stop_reason: r.stop_reason,
            provider_context: r.provider_context,
        }
    }
}

impl From<AdaptedResponse> for ModelResponse {
    fn from(mut response: AdaptedResponse) -> Self {
        response.tool_calls = normalize_tool_calls(response.tool_calls);
        let tool_calls = response
            .tool_calls
            .into_iter()
            .map(|tool_call| {
                let arguments = tool_call.arguments_for_wire();
                ChatToolCall {
                    id: tool_call.id,
                    kind: "function".to_string(),
                    function: ChatToolFunction {
                        name: tool_call.name,
                        arguments,
                    },
                }
            })
            .collect();

        Self::from_chat_payload(ChatCompletionPayload {
            content: (!response.content.is_empty()).then_some(response.content),
            thinking: response.thinking,
            finish_reason: Some(response.stop_reason),
            usage: serde_json::to_value(response.usage).ok(),
            tool_calls,
            provider_context: response.provider_context,
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_call(id: &str, name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: serde_json::from_str(arguments).expect("arguments should be json"),
            argument_parse_error: None,
            raw_arguments: Some(arguments.to_string()),
        }
    }

    fn response(tool_calls: Vec<ToolCall>) -> AdaptedResponse {
        AdaptedResponse {
            content: String::new(),
            thinking: None,
            tool_calls,
            usage: LlmUsage::default(),
            stop_reason: "tool_calls".to_string(),
            raw: Some(json!({})),
            provider_context: Vec::new(),
        }
    }

    #[test]
    fn identical_duplicate_tool_calls_are_coalesced_at_protocol_boundary() {
        let model_response = ModelResponse::from(response(vec![
            tool_call("call_duplicate", "file_read", r#"{"path":"package.json"}"#),
            tool_call("call_duplicate", "file_read", r#"{"path":"package.json"}"#),
            tool_call("call_unique", "file_read", r#"{"path":"tsconfig.json"}"#),
        ]));

        assert_eq!(model_response.tool_calls.len(), 2);
        assert_eq!(model_response.tool_calls[0].id, "call_duplicate");
        assert_eq!(model_response.tool_calls[1].id, "call_unique");
    }

    #[test]
    fn conflicting_duplicate_tool_call_ids_are_rewritten_without_losing_calls() {
        let model_response = ModelResponse::from(response(vec![
            tool_call("call_duplicate", "file_read", r#"{"path":"package.json"}"#),
            tool_call("call_duplicate", "file_read", r#"{"path":"tsconfig.json"}"#),
        ]));

        assert_eq!(model_response.tool_calls.len(), 2);
        assert_eq!(model_response.tool_calls[0].id, "call_duplicate");
        assert_ne!(model_response.tool_calls[1].id, "call_duplicate");
        assert_ne!(
            model_response.tool_calls[0].id,
            model_response.tool_calls[1].id
        );
    }
}
