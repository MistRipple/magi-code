use serde_json::{Value, json};

use crate::llm_types::{
    LlmContentBlock, LlmMessage, LlmMessageContent, LlmUsage, ToolCall, ToolDefinition,
};

pub fn estimate_message_tokens(messages: &[LlmMessage]) -> u64 {
    let mut total = 0u64;
    for msg in messages {
        total += 4;
        match &msg.content {
            LlmMessageContent::Text(text) => {
                total += (text.len() as u64) / 4;
            }
            LlmMessageContent::Blocks(blocks) => {
                for block in blocks {
                    match block {
                        LlmContentBlock::Text { text } => {
                            total += (text.len() as u64) / 4;
                        }
                        LlmContentBlock::ToolUse { input, .. } => {
                            total += (input.to_string().len() as u64) / 4 + 20;
                        }
                        LlmContentBlock::ToolResult { content, .. } => {
                            total += (content.len() as u64) / 4 + 10;
                        }
                        LlmContentBlock::Image { .. } => {
                            total += 1000;
                        }
                    }
                }
            }
        }
    }
    total
}

pub fn serialize_tool_definitions(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": {
                        "type": tool.input_schema.kind,
                        "properties": tool.input_schema.properties,
                        "required": tool.input_schema.required,
                    },
                }
            })
        })
        .collect()
}

pub fn serialize_anthropic_tool_definitions(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": {
                    "type": tool.input_schema.kind,
                    "properties": tool.input_schema.properties,
                    "required": tool.input_schema.required,
                }
            })
        })
        .collect()
}

pub fn convert_tool_calls_to_openai(tool_calls: &[ToolCall]) -> Vec<Value> {
    tool_calls
        .iter()
        .map(|tc| {
            json!({
                "id": tc.id,
                "type": "function",
                "function": {
                    "name": tc.name,
                    "arguments": tc.arguments_for_wire(),
                }
            })
        })
        .collect()
}

pub fn convert_tool_results_to_openai(tool_call_id: &str, content: &str) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": tool_call_id,
        "content": content,
    })
}

pub fn convert_messages_to_openai(messages: &[LlmMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(|msg| {
            let content_val = match &msg.content {
                LlmMessageContent::Text(text) => json!(text),
                LlmMessageContent::Blocks(blocks) => {
                    let mut tool_calls_out = Vec::new();
                    let mut text_parts = Vec::new();
                    let mut tool_result_id = None;
                    let mut tool_result_content = None;
                    let mut _is_error = false;

                    for block in blocks {
                        match block {
                            LlmContentBlock::Text { text } => {
                                text_parts.push(text.clone());
                            }
                            LlmContentBlock::ToolUse { id, name, input } => {
                                tool_calls_out.push(json!({
                                    "id": id,
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "arguments": input.to_string(),
                                    }
                                }));
                            }
                            LlmContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error: err,
                            } => {
                                tool_result_id = Some(tool_use_id.clone());
                                tool_result_content = Some(content.clone());
                                _is_error = *err;
                            }
                            LlmContentBlock::Image { .. } => {}
                        }
                    }

                    if let Some(tr_id) = tool_result_id {
                        return json!({
                            "role": "tool",
                            "tool_call_id": tr_id,
                            "content": tool_result_content.unwrap_or_default(),
                        });
                    }

                    let mut obj = json!({
                        "role": msg.role,
                        "content": text_parts.join("\n"),
                    });
                    if !tool_calls_out.is_empty() {
                        obj["tool_calls"] = json!(tool_calls_out);
                    }
                    return obj;
                }
            };

            json!({
                "role": msg.role,
                "content": content_val,
            })
        })
        .collect()
}

pub fn convert_messages_to_anthropic(messages: &[LlmMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(|msg| {
            let content_val = match &msg.content {
                LlmMessageContent::Text(text) => json!(text),
                LlmMessageContent::Blocks(blocks) => {
                    let parts: Vec<Value> = blocks
                        .iter()
                        .filter_map(|block| match block {
                            LlmContentBlock::Text { text } => {
                                Some(json!({"type": "text", "text": text}))
                            }
                            LlmContentBlock::ToolUse { id, name, input } => Some(json!({
                                "type": "tool_use",
                                "id": id,
                                "name": name,
                                "input": input,
                            })),
                            LlmContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            } => Some(json!({
                                "type": "tool_result",
                                "tool_use_id": tool_use_id,
                                "content": content,
                                "is_error": is_error,
                            })),
                            LlmContentBlock::Image { source } => Some(json!({
                                "type": "image",
                                "source": {
                                    "type": source.kind,
                                    "media_type": source.media_type,
                                    "data": source.data,
                                }
                            })),
                        })
                        .collect();
                    json!(parts)
                }
            };

            json!({
                "role": msg.role,
                "content": content_val,
            })
        })
        .collect()
}

pub fn parse_openai_usage(usage: &Value) -> LlmUsage {
    let cache_read_tokens = usage["prompt_tokens_details"]["cached_tokens"].as_u64();
    LlmUsage {
        input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens,
        cache_write_tokens: None,
        cache_read_included_in_input: cache_read_tokens.is_some(),
    }
}

pub fn parse_anthropic_usage(usage: &Value) -> LlmUsage {
    LlmUsage {
        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: usage["cache_read_input_tokens"].as_u64(),
        cache_write_tokens: usage["cache_creation_input_tokens"].as_u64(),
        cache_read_included_in_input: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_types::ToolInputSchema;

    #[test]
    fn serialize_tool_definitions_preserves_required_schema() {
        let tools = vec![ToolDefinition {
            name: "shell_exec".to_string(),
            description: "Run a shell command".to_string(),
            input_schema: ToolInputSchema {
                kind: "object".to_string(),
                properties: json!({
                    "command": {
                        "type": "string"
                    }
                }),
                required: Some(vec!["command".to_string()]),
            },
        }];

        let serialized = serialize_tool_definitions(&tools);
        let parameters = &serialized[0]["function"]["parameters"];

        assert_eq!(parameters["type"], "object");
        assert_eq!(parameters["properties"]["command"]["type"], "string");
        assert_eq!(parameters["required"][0], "command");
    }
}
