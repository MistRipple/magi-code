use serde_json::{Value, json};

use super::adapter::{AdaptedRequest, AdaptedResponse, ProviderAdapter, ProviderFamily};
use super::utils::convert_messages_to_openai;
use crate::llm_types::{LlmMessageParams, LlmUsage, ToolCall};

pub struct OpenAiResponsesAdapter;

impl ProviderAdapter for OpenAiResponsesAdapter {
    fn family(&self) -> ProviderFamily {
        ProviderFamily::OpenAiResponses
    }

    fn build_request(
        &self,
        params: &LlmMessageParams,
        model: &str,
    ) -> Result<AdaptedRequest, String> {
        let mut input = Vec::new();

        if let Some(ref system_prompt) = params.system_prompt {
            input.push(json!({
                "role": "developer",
                "content": system_prompt,
            }));
        }

        for msg in &params.messages {
            if msg.role == "system" {
                let text = match &msg.content {
                    crate::llm_types::LlmMessageContent::Text(t) => t.clone(),
                    _ => continue,
                };
                input.push(json!({
                    "role": "developer",
                    "content": text,
                }));
                continue;
            }

            let openai_msgs = convert_messages_to_openai(&[msg.clone()]);
            input.extend(openai_msgs);
        }

        let mut body = json!({
            "model": model,
            "input": input,
        });

        if let Some(max_tokens) = params.max_tokens {
            body["max_output_tokens"] = json!(max_tokens);
        }
        if let Some(temperature) = params.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(ref tools) = params.tools {
            if !tools.is_empty() {
                let tool_defs: Vec<Value> = tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "type": "function",
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": {
                                "type": tool.input_schema.kind,
                                "properties": tool.input_schema.properties,
                                "required": tool.input_schema.required,
                            }
                        })
                    })
                    .collect();
                body["tools"] = json!(tool_defs);
            }
        }
        if let Some(stream) = params.stream {
            body["stream"] = json!(stream);
        }

        Ok(AdaptedRequest {
            url_path: "/v1/responses".to_string(),
            body,
            extra_headers: Vec::new(),
        })
    }

    fn parse_response(&self, status: u16, body: &str) -> Result<AdaptedResponse, String> {
        if !(200..300).contains(&status) {
            return Err(format!(
                "OpenAI Responses API error (status={}): {}",
                status,
                truncate(body, 512)
            ));
        }

        let envelope: Value =
            serde_json::from_str(body).map_err(|e| format!("invalid JSON response: {e}"))?;

        let output = envelope["output"].as_array().cloned().unwrap_or_default();

        let mut text_parts = Vec::new();
        let mut thinking_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for item in &output {
            match item["type"].as_str() {
                Some("message") => {
                    if let Some(content) = item["content"].as_array() {
                        for block in content {
                            if block["type"].as_str() == Some("output_text") {
                                if let Some(text) = block["text"].as_str() {
                                    text_parts.push(text.to_string());
                                }
                            }
                        }
                    }
                }
                Some("reasoning") => {
                    collect_response_reasoning_text(item, &mut thinking_parts);
                }
                Some("function_call") => {
                    if let (Some(id), Some(name)) =
                        (item["call_id"].as_str(), item["name"].as_str())
                    {
                        let args_str = match item.get("arguments") {
                            Some(Value::String(value)) => value.clone(),
                            Some(Value::Null) | None => "{}".to_string(),
                            Some(value) => value.to_string(),
                        };
                        let arguments = serde_json::from_str(&args_str).unwrap_or(json!({}));
                        tool_calls.push(ToolCall {
                            id: id.to_string(),
                            name: name.to_string(),
                            arguments,
                            argument_parse_error: None,
                            raw_arguments: Some(args_str),
                        });
                    }
                }
                _ => {}
            }
        }

        let stop_reason = envelope["status"]
            .as_str()
            .unwrap_or("completed")
            .to_string();

        let usage = envelope
            .get("usage")
            .map(|u| {
                let cache_read_tokens = u["input_tokens_details"]["cached_tokens"].as_u64();
                LlmUsage {
                    input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
                    output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                    cache_read_tokens,
                    cache_write_tokens: None,
                    cache_read_included_in_input: cache_read_tokens.is_some(),
                }
            })
            .unwrap_or_default();

        Ok(AdaptedResponse {
            content: text_parts.join(""),
            thinking: {
                let thinking = thinking_parts.join("");
                (!thinking.trim().is_empty()).then_some(thinking)
            },
            tool_calls,
            usage,
            stop_reason,
            raw: Some(envelope),
        })
    }

    fn max_output_tokens_field(&self) -> &str {
        "max_output_tokens"
    }
}

fn collect_response_reasoning_text(item: &Value, thinking_parts: &mut Vec<String>) {
    if let Some(text) = item["text"].as_str() {
        thinking_parts.push(text.to_string());
    }
    let Some(summary) = item["summary"].as_array() else {
        return;
    };
    for block in summary {
        if let Some(text) = block["text"].as_str() {
            thinking_parts.push(text.to_string());
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_object_function_call_arguments() {
        let body = json!({
            "status": "completed",
            "output": [{
                "type": "function_call",
                "call_id": "call_shell",
                "name": "shell_exec",
                "arguments": { "command": "echo hello", "cwd": "/tmp" }
            }]
        })
        .to_string();

        let parsed = OpenAiResponsesAdapter
            .parse_response(200, &body)
            .expect("object arguments should be accepted");

        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "shell_exec");
        assert_eq!(parsed.tool_calls[0].arguments["command"], "echo hello");
        assert_eq!(
            serde_json::from_str::<Value>(parsed.tool_calls[0].raw_arguments.as_deref().unwrap())
                .expect("raw arguments should stay valid json")["cwd"],
            "/tmp"
        );
    }

    #[test]
    fn parses_cached_input_usage() {
        let body = json!({
            "status": "completed",
            "output": [{
                "type": "message",
                "content": [{ "type": "output_text", "text": "hello" }]
            }],
            "usage": {
                "input_tokens": 10,
                "input_tokens_details": { "cached_tokens": 4 },
                "output_tokens": 3,
                "total_tokens": 13
            }
        })
        .to_string();

        let parsed = OpenAiResponsesAdapter
            .parse_response(200, &body)
            .expect("usage should parse");

        assert_eq!(parsed.usage.input_tokens, 10);
        assert_eq!(parsed.usage.output_tokens, 3);
        assert_eq!(parsed.usage.cache_read_tokens, Some(4));
        assert!(parsed.usage.cache_read_included_in_input);
    }
}
