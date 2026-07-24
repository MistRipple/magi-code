use magi_usage_authority::ReasoningEffort;
use serde_json::{Value, json};

use crate::llm_types::{
    LlmContentBlock, LlmMessage, LlmMessageContent, LlmUsage, ToolCall, ToolDefinition,
};

/// 将 `ReasoningEffort` 序列化为下游协议接受的字符串字面量。
///
/// 四档粒度（`low | medium | high | xhigh`）原样透传，由下游模型/网关自行解释。
/// magi 不在这里做向下兼容降级——把"极高"偷偷映射成"high"会让用户看见的等级
/// 与真实生效等级永久错位（见 Task #59 回归）。该函数被两个协议适配器共用：
/// - OpenAI Chat Completions 写入顶层 `reasoning_effort` 字段；
/// - Anthropic Messages 在 `thinking.effort`（Opus 4.7+）形态下写入同字段。
pub fn reasoning_effort_label(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
    }
}

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
                        LlmContentBlock::ToolResult {
                            content, images, ..
                        } => {
                            total += (content.len() as u64) / 4 + 10;
                            total += images.len() as u64 * 1000;
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
    let mut converted = Vec::new();
    for msg in messages {
        match &msg.content {
            LlmMessageContent::Text(text) => converted.push(json!({
                "role": msg.role,
                "content": text,
            })),
            LlmMessageContent::Blocks(blocks) => {
                append_openai_content_block_message(&mut converted, msg, blocks);
            }
        }
    }
    converted
}

fn append_openai_content_block_message(
    converted: &mut Vec<Value>,
    msg: &LlmMessage,
    blocks: &[LlmContentBlock],
) {
    let mut tool_calls_out = Vec::new();
    let mut text_parts = Vec::new();
    let mut image_parts = Vec::new();
    let mut tool_result_id = None;
    let mut tool_result_content = None;
    let mut tool_result_images = Vec::new();

    for block in blocks {
        match block {
            LlmContentBlock::Text { text } => {
                text_parts.push(text.clone());
            }
            LlmContentBlock::Image { source } => {
                image_parts.push(openai_image_part(source));
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
                images,
                ..
            } => {
                tool_result_id = Some(tool_use_id.clone());
                tool_result_content = Some(content.clone());
                tool_result_images = images.clone();
            }
        }
    }

    if let Some(tr_id) = tool_result_id {
        let text = tool_result_content.unwrap_or_default();
        converted.push(json!({
            "role": "tool",
            "tool_call_id": tr_id,
            "content": text,
        }));
        if !tool_result_images.is_empty() {
            let mut content_parts = vec![json!({
                "type": "text",
                "text": "The previous tool result includes image content for visual inspection.",
            })];
            content_parts.extend(tool_result_images.iter().map(openai_image_part));
            converted.push(json!({
                "role": "user",
                "content": content_parts,
            }));
        }
        return;
    }

    if !image_parts.is_empty() && tool_calls_out.is_empty() {
        let mut content_parts = text_parts
            .into_iter()
            .filter(|text| !text.is_empty())
            .map(|text| json!({"type": "text", "text": text}))
            .collect::<Vec<_>>();
        content_parts.extend(image_parts);
        converted.push(json!({
            "role": msg.role,
            "content": content_parts,
        }));
        return;
    }

    let mut obj = json!({
        "role": msg.role,
        "content": text_parts.join("\n"),
    });
    if !tool_calls_out.is_empty() {
        obj["tool_calls"] = json!(tool_calls_out);
    }
    converted.push(obj);
}

fn openai_image_part(source: &crate::llm_types::ImageSource) -> Value {
    json!({
        "type": "image_url",
        "image_url": {
            "url": format!("data:{};base64,{}", source.media_type, source.data),
        }
    })
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
                        .map(|block| match block {
                            LlmContentBlock::Text { text } => {
                                json!({"type": "text", "text": text})
                            }
                            LlmContentBlock::ToolUse { id, name, input } => json!({
                                "type": "tool_use",
                                "id": id,
                                "name": name,
                                "input": input,
                            }),
                            LlmContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                                images,
                            } => {
                                let content = if images.is_empty() {
                                    json!(content)
                                } else {
                                    let mut parts = Vec::new();
                                    if !content.trim().is_empty() {
                                        parts.push(json!({"type": "text", "text": content}));
                                    }
                                    parts.extend(images.iter().map(|source| {
                                        json!({
                                            "type": "image",
                                            "source": {
                                                "type": source.kind,
                                                "media_type": source.media_type,
                                                "data": source.data,
                                            }
                                        })
                                    }));
                                    json!(parts)
                                };
                                json!({
                                    "type": "tool_result",
                                    "tool_use_id": tool_use_id,
                                    "content": content,
                                    "is_error": is_error,
                                })
                            }
                            LlmContentBlock::Image { source } => json!({
                                "type": "image",
                                "source": {
                                    "type": source.kind,
                                    "media_type": source.media_type,
                                    "data": source.data,
                                }
                            }),
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
    use crate::llm_types::{ImageSource, ToolInputSchema};

    fn image_source() -> ImageSource {
        ImageSource {
            kind: "base64".to_string(),
            media_type: "image/png".to_string(),
            data: "AAA".to_string(),
        }
    }

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
            origin: crate::types::ChatToolOrigin::Builtin,
        }];

        let serialized = serialize_tool_definitions(&tools);
        let parameters = &serialized[0]["function"]["parameters"];

        assert_eq!(parameters["type"], "object");
        assert_eq!(parameters["properties"]["command"]["type"], "string");
        assert_eq!(parameters["required"][0], "command");
    }

    #[test]
    fn openai_tool_result_with_image_emits_tool_result_and_user_image() {
        let messages = vec![LlmMessage {
            role: "user".to_string(),
            content: LlmMessageContent::Blocks(vec![LlmContentBlock::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: "已读取图片".to_string(),
                is_error: false,
                images: vec![image_source()],
            }]),
        }];

        let converted = convert_messages_to_openai(&messages);

        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0]["role"], "tool");
        assert_eq!(converted[0]["tool_call_id"], "call_1");
        assert_eq!(converted[0]["content"], "已读取图片");
        assert_eq!(converted[1]["role"], "user");
        assert_eq!(
            converted[1]["content"][1]["image_url"]["url"],
            "data:image/png;base64,AAA"
        );
    }

    #[test]
    fn anthropic_tool_result_with_image_keeps_image_inside_tool_result_content() {
        let messages = vec![LlmMessage {
            role: "user".to_string(),
            content: LlmMessageContent::Blocks(vec![LlmContentBlock::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: "已读取图片".to_string(),
                is_error: false,
                images: vec![image_source()],
            }]),
        }];

        let converted = convert_messages_to_anthropic(&messages);

        assert_eq!(converted[0]["role"], "user");
        assert_eq!(converted[0]["content"][0]["type"], "tool_result");
        assert_eq!(converted[0]["content"][0]["content"][0]["type"], "text");
        assert_eq!(converted[0]["content"][0]["content"][1]["type"], "image");
        assert_eq!(
            converted[0]["content"][0]["content"][1]["source"]["media_type"],
            "image/png"
        );
    }
}
