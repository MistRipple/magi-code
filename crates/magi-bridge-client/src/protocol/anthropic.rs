use serde_json::{json, Value};

use super::adapter::{AdaptedRequest, AdaptedResponse, ProviderAdapter, ProviderFamily};
use super::utils::{
    convert_messages_to_anthropic, parse_anthropic_usage, serialize_anthropic_tool_definitions,
};
use crate::llm_types::{LlmMessageParams, ToolCall};

pub struct AnthropicMessagesAdapter;

impl ProviderAdapter for AnthropicMessagesAdapter {
    fn family(&self) -> ProviderFamily {
        ProviderFamily::Anthropic
    }

    fn build_request(
        &self,
        params: &LlmMessageParams,
        model: &str,
    ) -> Result<AdaptedRequest, String> {
        let (system_messages, non_system): (Vec<_>, Vec<_>) = params
            .messages
            .iter()
            .partition(|m| m.role == "system");

        let system_text = if let Some(ref sp) = params.system_prompt {
            sp.clone()
        } else {
            system_messages
                .iter()
                .filter_map(|m| match &m.content {
                    crate::llm_types::LlmMessageContent::Text(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let non_system_owned: Vec<_> = non_system.into_iter().cloned().collect();
        let messages = convert_messages_to_anthropic(&non_system_owned);

        let mut body = json!({
            "model": model,
            "messages": messages,
            "max_tokens": params.max_tokens.unwrap_or(4096),
        });

        if !system_text.is_empty() {
            body["system"] = json!(system_text);
        }
        if let Some(temperature) = params.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(ref tools) = params.tools {
            if !tools.is_empty() {
                body["tools"] = json!(serialize_anthropic_tool_definitions(tools));
                if let Some(ref tc) = params.tool_choice {
                    body["tool_choice"] = serde_json::to_value(tc).unwrap_or(json!({"type": "auto"}));
                }
            }
        }
        if let Some(stream) = params.stream {
            body["stream"] = json!(stream);
        }

        let extra_headers = vec![
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ];

        Ok(AdaptedRequest {
            url_path: "/v1/messages".to_string(),
            body,
            extra_headers,
        })
    }

    fn parse_response(&self, status: u16, body: &str) -> Result<AdaptedResponse, String> {
        if !(200..300).contains(&status) {
            return Err(format!(
                "Anthropic API error (status={}): {}",
                status,
                truncate(body, 512)
            ));
        }

        let envelope: Value =
            serde_json::from_str(body).map_err(|e| format!("invalid JSON response: {e}"))?;

        let stop_reason = envelope["stop_reason"]
            .as_str()
            .unwrap_or("end_turn")
            .to_string();

        let content_blocks = envelope["content"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for block in &content_blocks {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(text) = block["text"].as_str() {
                        text_parts.push(text.to_string());
                    }
                }
                Some("tool_use") => {
                    if let (Some(id), Some(name)) =
                        (block["id"].as_str(), block["name"].as_str())
                    {
                        let input = block["input"].clone();
                        let raw = serde_json::to_string(&input).ok();
                        tool_calls.push(ToolCall {
                            id: id.to_string(),
                            name: name.to_string(),
                            arguments: input,
                            argument_parse_error: None,
                            raw_arguments: raw,
                        });
                    }
                }
                _ => {}
            }
        }

        let usage = envelope
            .get("usage")
            .map(parse_anthropic_usage)
            .unwrap_or_default();

        Ok(AdaptedResponse {
            content: text_parts.join(""),
            tool_calls,
            usage,
            stop_reason,
            raw: Some(envelope),
        })
    }

    fn max_output_tokens_field(&self) -> &str {
        "max_tokens"
    }
}

fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.len() <= max {
        trimmed.to_string()
    } else {
        format!("{}...", &trimmed[..max])
    }
}
