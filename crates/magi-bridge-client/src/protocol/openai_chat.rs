use serde_json::{Value, json};

use super::adapter::{AdaptedRequest, AdaptedResponse, ProviderAdapter, ProviderFamily};
use super::utils::{convert_messages_to_openai, parse_openai_usage, serialize_tool_definitions};
use crate::llm_types::{LlmMessageParams, ToolCall, parse_tool_arguments};

pub struct OpenAiChatCompletionsAdapter;

impl ProviderAdapter for OpenAiChatCompletionsAdapter {
    fn family(&self) -> ProviderFamily {
        ProviderFamily::OpenAiChat
    }

    fn build_request(
        &self,
        params: &LlmMessageParams,
        model: &str,
    ) -> Result<AdaptedRequest, String> {
        let messages = convert_messages_to_openai(&params.messages);

        let mut body = json!({
            "model": model,
            "messages": messages,
        });

        if let Some(max_tokens) = params.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }
        if let Some(temperature) = params.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(ref tools) = params.tools {
            if !tools.is_empty() {
                body["tools"] = json!(serialize_tool_definitions(tools));
                if let Some(ref tc) = params.tool_choice {
                    body["tool_choice"] = serde_json::to_value(tc).unwrap_or(json!("auto"));
                } else {
                    body["tool_choice"] = json!("auto");
                }
            }
        }
        if let Some(stream) = params.stream {
            body["stream"] = json!(stream);
            if stream {
                body["stream_options"] = json!({"include_usage": true});
            }
        }

        Ok(AdaptedRequest {
            url_path: "/v1/chat/completions".to_string(),
            body,
            extra_headers: Vec::new(),
        })
    }

    fn parse_response(&self, status: u16, body: &str) -> Result<AdaptedResponse, String> {
        if !(200..300).contains(&status) {
            return Err(format!(
                "OpenAI API error (status={}): {}",
                status,
                truncate(body, 512)
            ));
        }

        let envelope: Value =
            serde_json::from_str(body).map_err(|e| format!("invalid JSON response: {e}"))?;

        let choices = envelope["choices"]
            .as_array()
            .ok_or("missing choices array")?;

        if choices.is_empty() {
            return Err("empty choices array".to_string());
        }

        let choice = &choices[0];
        let message = &choice["message"];
        let finish_reason = choice["finish_reason"]
            .as_str()
            .unwrap_or("stop")
            .to_string();

        let content = message["content"]
            .as_str()
            .map(|s| s.to_string())
            .or_else(|| message["refusal"].as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        let thinking = message["reasoning_content"]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned);

        let tool_calls = parse_openai_tool_calls(&message["tool_calls"]);

        let usage = envelope
            .get("usage")
            .map(parse_openai_usage)
            .unwrap_or_default();

        Ok(AdaptedResponse {
            content,
            thinking,
            tool_calls,
            usage,
            stop_reason: finish_reason,
            raw: Some(envelope),
        })
    }
}

fn parse_openai_tool_calls(value: &Value) -> Vec<ToolCall> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|tc| {
            let id = tc["id"].as_str()?.to_string();
            let func = &tc["function"];
            let name = func["name"].as_str()?.to_string();
            let args_raw = match &func["arguments"] {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let (arguments, argument_parse_error) = parse_tool_arguments(&args_raw);
            Some(ToolCall {
                id,
                name,
                arguments,
                argument_parse_error,
                raw_arguments: Some(args_raw),
            })
        })
        .collect()
}

fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.len() <= max {
        trimmed.to_string()
    } else {
        format!("{}...", &trimmed[..max])
    }
}
