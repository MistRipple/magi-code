use serde_json::{Value, json};

use super::adapter::{AdaptedRequest, AdaptedResponse, ProviderAdapter, ProviderFamily};
use super::capability::resolve_capability_profile;
use super::utils::reasoning_effort_label;
use crate::cache_boundary::PROMPT_CACHE_BOUNDARY;
use crate::llm_types::{
    ImageSource, LlmContentBlock, LlmMessage, LlmMessageContent, LlmMessageParams, ToolCall,
    ToolChoice, ToolDefinition, parse_tool_arguments,
};
use crate::types::ModelProviderContext;

/// OpenAI Responses API 的统一适配器。
///
/// Responses 与 Chat Completions 共用鉴权方式，但请求项、工具调用和流事件完全不同，
/// 因此不能仅替换 URL。适配器负责把 Magi 的消息模型转换成 Responses input items，
/// 并把 reasoning/function_call 等 output items 还原为运行时统一结构。
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
        for message in &params.messages {
            append_response_input_items(&mut input, message)?;
        }

        let mut body = json!({
            "model": model,
            "input": input,
        });

        if let Some(instructions) = params
            .system_prompt
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            body["instructions"] = json!(instructions);
        }
        if let Some(max_output_tokens) = params.max_tokens {
            body["max_output_tokens"] = json!(max_output_tokens);
        }
        if let Some(temperature) = params.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(tools) = params.tools.as_ref().filter(|tools| !tools.is_empty()) {
            body["tools"] = json!(serialize_response_tools(tools));
            body["tool_choice"] = translate_tool_choice(params.tool_choice.as_ref());
        }
        if let Some(stream) = params.stream {
            body["stream"] = json!(stream);
        }

        let capability = resolve_capability_profile(model);
        if capability.supports_openai_reasoning_effort
            && let Some(effort) = params.reasoning_effort
        {
            body["reasoning"] = json!({"effort": reasoning_effort_label(effort)});
        }

        let mut extra_headers = Vec::new();
        for (name, value) in capability.beta_headers {
            extra_headers.push((name.to_string(), value.to_string()));
        }

        Ok(AdaptedRequest {
            url_path: "/v1/responses".to_string(),
            body,
            extra_headers,
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

        let envelope: Value = serde_json::from_str(body)
            .map_err(|error| format!("invalid JSON response: {error}"))?;
        parse_responses_envelope(&envelope)
    }

    fn max_output_tokens_field(&self) -> &str {
        "max_output_tokens"
    }
}

fn append_response_input_items(input: &mut Vec<Value>, message: &LlmMessage) -> Result<(), String> {
    match &message.content {
        LlmMessageContent::Text(text) => {
            if text != PROMPT_CACHE_BOUNDARY {
                input.push(response_message_item(
                    &message.role,
                    vec![response_text_part(&message.role, text)],
                ));
            }
        }
        LlmMessageContent::Blocks(blocks) => {
            let mut message_parts = Vec::new();
            for block in blocks {
                match block {
                    LlmContentBlock::Text { text } => {
                        if !text.is_empty() && text != PROMPT_CACHE_BOUNDARY {
                            message_parts.push(response_text_part(&message.role, text));
                        }
                    }
                    LlmContentBlock::Image { source } => {
                        message_parts.push(response_image_part(source));
                    }
                    LlmContentBlock::ToolUse {
                        id,
                        name,
                        input: args,
                    } => {
                        flush_response_message(input, &message.role, &mut message_parts);
                        input.push(json!({
                            "type": "function_call",
                            "call_id": id,
                            "name": name,
                            "arguments": args.to_string(),
                        }));
                    }
                    LlmContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                        images,
                    } => {
                        flush_response_message(input, &message.role, &mut message_parts);
                        input.push(response_tool_output_item(
                            tool_use_id,
                            content,
                            *is_error,
                            images,
                        ));
                    }
                    LlmContentBlock::ProviderContext { context }
                        if context.provider == "openai_responses"
                            && context.kind == "reasoning"
                            && context.data["type"].as_str() == Some("reasoning") =>
                    {
                        flush_response_message(input, &message.role, &mut message_parts);
                        input.push(context.data.clone());
                    }
                    LlmContentBlock::ProviderContext { .. } => {}
                }
            }
            flush_response_message(input, &message.role, &mut message_parts);
        }
    }
    Ok(())
}

fn flush_response_message(input: &mut Vec<Value>, role: &str, parts: &mut Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    input.push(response_message_item(role, std::mem::take(parts)));
}

fn response_message_item(role: &str, content: Vec<Value>) -> Value {
    json!({
        "type": "message",
        "role": role,
        "content": content,
    })
}

fn response_text_part(role: &str, text: &str) -> Value {
    json!({
        "type": if role == "assistant" { "output_text" } else { "input_text" },
        "text": text,
    })
}

fn response_image_part(source: &ImageSource) -> Value {
    json!({
        "type": "input_image",
        "image_url": format!("data:{};base64,{}", source.media_type, source.data),
    })
}

fn response_tool_output_item(
    call_id: &str,
    content: &str,
    is_error: bool,
    images: &[ImageSource],
) -> Value {
    let output = if images.is_empty() {
        Value::String(if is_error {
            format!("[tool_error]\n{content}")
        } else {
            content.to_string()
        })
    } else {
        let mut parts = Vec::new();
        if !content.trim().is_empty() {
            parts.push(json!({
                "type": "input_text",
                "text": if is_error { format!("[tool_error]\n{content}") } else { content.to_string() },
            }));
        }
        parts.extend(images.iter().map(response_image_part));
        Value::Array(parts)
    };
    json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": output,
    })
}

fn serialize_response_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
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
                },
            })
        })
        .collect()
}

fn translate_tool_choice(choice: Option<&ToolChoice>) -> Value {
    match choice {
        None => json!("auto"),
        Some(ToolChoice::Simple(label)) => json!(label),
        Some(ToolChoice::Typed {
            name: Some(name), ..
        }) => json!({
            "type": "function",
            "name": name,
        }),
        Some(ToolChoice::Typed { kind, name: None }) => json!(kind),
    }
}

fn parse_responses_envelope(envelope: &Value) -> Result<AdaptedResponse, String> {
    if envelope["status"].as_str() == Some("failed") {
        let message = envelope["error"]["message"]
            .as_str()
            .filter(|message| !message.trim().is_empty())
            .unwrap_or("response failed without an error message");
        return Err(format!("OpenAI Responses response failed: {message}"));
    }
    let output = envelope
        .get("output")
        .and_then(Value::as_array)
        .ok_or("missing output array")?;

    let mut content_parts = Vec::new();
    let mut thinking_parts = Vec::new();
    let mut tool_calls = Vec::new();
    let mut provider_context = Vec::new();

    for item in output {
        match item["type"].as_str() {
            Some("message") => {
                if let Some(content) = item["content"].as_array() {
                    for part in content {
                        if matches!(part["type"].as_str(), Some("output_text" | "text"))
                            && let Some(text) = part["text"].as_str()
                        {
                            content_parts.push(text.to_string());
                        }
                        if part["type"].as_str() == Some("refusal")
                            && let Some(text) = part["refusal"].as_str()
                        {
                            content_parts.push(text.to_string());
                        }
                    }
                }
            }
            Some("reasoning") => {
                if let Some(summary) = item["summary"].as_array() {
                    for part in summary {
                        if let Some(text) = part["text"].as_str() {
                            thinking_parts.push(text.to_string());
                        }
                    }
                }
                provider_context.push(ModelProviderContext {
                    provider: "openai_responses".to_string(),
                    kind: "reasoning".to_string(),
                    data: item.clone(),
                });
            }
            Some("function_call") => {
                let name = item["name"].as_str().ok_or("function_call missing name")?;
                let raw_arguments = item["arguments"].as_str().unwrap_or_default().to_string();
                let (arguments, argument_parse_error) = parse_tool_arguments(&raw_arguments);
                let call_id = item["call_id"]
                    .as_str()
                    .or_else(|| item["id"].as_str())
                    .filter(|value| !value.trim().is_empty())
                    .ok_or("function_call missing call_id")?;
                tool_calls.push(ToolCall {
                    id: call_id.to_string(),
                    name: name.to_string(),
                    arguments,
                    argument_parse_error,
                    raw_arguments: Some(raw_arguments),
                });
            }
            _ => {}
        }
    }

    if content_parts.is_empty()
        && let Some(text) = envelope["output_text"].as_str()
        && !text.trim().is_empty()
    {
        content_parts.push(text.to_string());
    }

    let stop_reason = if !tool_calls.is_empty() {
        "tool_calls".to_string()
    } else {
        match envelope["status"].as_str() {
            Some("incomplete") => envelope["incomplete_details"]["reason"]
                .as_str()
                .unwrap_or("incomplete")
                .to_string(),
            Some("failed") => "error".to_string(),
            _ => "stop".to_string(),
        }
    };

    Ok(AdaptedResponse {
        content: content_parts.join(""),
        thinking: (!thinking_parts.is_empty()).then(|| thinking_parts.join("")),
        tool_calls,
        usage: parse_responses_usage(envelope.get("usage")),
        stop_reason,
        raw: Some(envelope.clone()),
        provider_context,
    })
}

fn parse_responses_usage(usage: Option<&Value>) -> crate::llm_types::LlmUsage {
    let Some(usage) = usage else {
        return crate::llm_types::LlmUsage::default();
    };
    let cache_read_tokens = usage["input_tokens_details"]["cached_tokens"].as_u64();
    crate::llm_types::LlmUsage {
        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens,
        cache_write_tokens: None,
        cache_read_included_in_input: cache_read_tokens.is_some(),
    }
}

fn truncate(value: &str, max: usize) -> String {
    let trimmed = value.trim();
    if trimmed.len() <= max {
        trimmed.to_string()
    } else {
        let mut end = max;
        while !trimmed.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        format!("{}...", &trimmed[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_types::{LlmMessage, LlmMessageContent, LlmMessageParams, ToolInputSchema};

    fn params(messages: Vec<LlmMessage>) -> LlmMessageParams {
        LlmMessageParams {
            messages,
            max_tokens: Some(512),
            temperature: None,
            tools: None,
            stream: Some(true),
            system_prompt: None,
            tool_choice: None,
            reasoning_effort: None,
        }
    }

    #[test]
    fn builds_responses_message_and_function_output_items() {
        let mut request_params = params(vec![
            LlmMessage {
                role: "user".to_string(),
                content: LlmMessageContent::Text("run pwd".to_string()),
            },
            LlmMessage {
                role: "assistant".to_string(),
                content: LlmMessageContent::Blocks(vec![LlmContentBlock::ToolUse {
                    id: "call_1".to_string(),
                    name: "shell_exec".to_string(),
                    input: json!({"command": "pwd"}),
                }]),
            },
            LlmMessage {
                role: "user".to_string(),
                content: LlmMessageContent::Blocks(vec![LlmContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: "/tmp".to_string(),
                    is_error: false,
                    images: Vec::new(),
                }]),
            },
        ]);
        request_params.tools = Some(vec![ToolDefinition {
            name: "shell_exec".to_string(),
            description: "run a shell command".to_string(),
            input_schema: ToolInputSchema {
                kind: "object".to_string(),
                properties: json!({"command": {"type": "string"}}),
                required: Some(vec!["command".to_string()]),
            },
            origin: crate::types::ChatToolOrigin::Builtin,
        }]);
        let request = OpenAiResponsesAdapter
            .build_request(&request_params, "gpt-5")
            .expect("request should build");
        assert_eq!(request.url_path, "/v1/responses");
        assert_eq!(request.body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(request.body["input"][1]["type"], "function_call");
        assert_eq!(request.body["input"][2]["type"], "function_call_output");
        assert_eq!(request.body["tools"][0]["name"], "shell_exec");
        assert_eq!(request.body["max_output_tokens"], 512);
    }

    #[test]
    fn parses_responses_text_reasoning_and_function_call() {
        let response = OpenAiResponsesAdapter
            .parse_response(
                200,
                &json!({
                    "status": "completed",
                    "output": [
                        {"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": "先判断"}]},
                        {"type": "function_call", "call_id": "call_1", "name": "shell_exec", "arguments": "{\"command\":\"pwd\"}"}
                    ],
                    "usage": {"input_tokens": 10, "output_tokens": 4, "input_tokens_details": {"cached_tokens": 3}}
                })
                .to_string(),
            )
            .expect("response should parse");
        assert_eq!(response.tool_calls[0].id, "call_1");
        assert_eq!(response.tool_calls[0].arguments["command"], "pwd");
        assert_eq!(response.thinking.as_deref(), Some("先判断"));
        assert_eq!(response.provider_context[0].provider, "openai_responses");
        assert_eq!(response.usage.input_tokens, 10);
        assert_eq!(response.usage.cache_read_tokens, Some(3));
    }

    #[test]
    fn truncates_multibyte_error_body_without_panicking() {
        let body = "错".repeat(300);
        let error = OpenAiResponsesAdapter
            .parse_response(500, &body)
            .expect_err("非成功响应必须返回协议错误");
        assert!(error.contains("OpenAI Responses API error"));
        assert!(error.ends_with("..."));
    }
}
