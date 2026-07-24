use magi_usage_authority::ReasoningEffort;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: ToolInputSchema,
    /// 工具身份来源仅供 Magi 的协议边界生成 wire name，绝不透传给上游。
    #[serde(default, skip_serializing)]
    pub origin: crate::types::ChatToolOrigin,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolInputSchema {
    #[serde(rename = "type")]
    pub kind: String,
    pub properties: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument_parse_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_arguments: Option<String>,
}

impl ToolCall {
    pub fn arguments_for_wire(&self) -> String {
        self.raw_arguments
            .clone()
            .unwrap_or_else(|| self.arguments.to_string())
    }
}

pub fn parse_tool_arguments(raw_arguments: &str) -> (Value, Option<String>) {
    if raw_arguments.trim().is_empty() {
        return (serde_json::json!({}), None);
    }

    match serde_json::from_str::<Value>(raw_arguments) {
        Ok(arguments) => (arguments, None),
        Err(error) => (
            Value::String(raw_arguments.to_string()),
            Some(error.to_string()),
        ),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeMetadata {
    pub file_path: String,
    pub change_type: String,
    pub additions: u32,
    pub deletions: u32,
    pub diff: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StandardizedToolResult {
    pub schema_version: String,
    pub source: String,
    pub tool_name: String,
    pub tool_call_id: String,
    pub status: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    pub tool_call_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standardized: Option<StandardizedToolResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_change: Option<FileChangeMetadata>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LlmContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: ImageSource },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageSource>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub kind: String,
    pub media_type: String,
    pub data: String,
}

#[derive(Clone, Debug, Default)]
pub struct ParsedToolResultModelContent {
    pub text: String,
    pub images: Vec<ImageSource>,
}

pub fn parse_tool_result_model_content(raw_content: Option<&str>) -> ParsedToolResultModelContent {
    let raw = raw_content.unwrap_or_default();
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return ParsedToolResultModelContent {
            text: raw.to_string(),
            images: Vec::new(),
        };
    };
    let Some(items) = value.get("model_content").and_then(Value::as_array) else {
        return ParsedToolResultModelContent {
            text: raw.to_string(),
            images: Vec::new(),
        };
    };

    let mut texts = Vec::new();
    let mut images = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = item
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
                {
                    texts.push(text.to_string());
                }
            }
            Some("image") => {
                if let Some(source) = parse_image_source(item.get("source")) {
                    images.push(source);
                }
            }
            _ => {}
        }
    }

    if images.is_empty() {
        return ParsedToolResultModelContent {
            text: raw.to_string(),
            images,
        };
    }

    let text = value
        .get("summary")
        .and_then(Value::as_str)
        .filter(|summary| !summary.trim().is_empty())
        .map(str::to_string)
        .or_else(|| (!texts.is_empty()).then(|| texts.join("\n")))
        .unwrap_or_else(|| "工具返回了一张图片。".to_string());

    ParsedToolResultModelContent { text, images }
}

fn parse_image_source(value: Option<&Value>) -> Option<ImageSource> {
    let object = value?.as_object()?;
    let kind = object
        .get("type")
        .or_else(|| object.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("base64");
    let media_type = object
        .get("media_type")
        .or_else(|| object.get("mediaType"))
        .and_then(Value::as_str)?;
    let data = object.get("data").and_then(Value::as_str)?;
    if kind != "base64" || !media_type.starts_with("image/") || data.trim().is_empty() {
        return None;
    }
    Some(ImageSource {
        kind: kind.to_string(),
        media_type: media_type.to_string(),
        data: data.to_string(),
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LlmMessageContent {
    Text(String),
    Blocks(Vec<LlmContentBlock>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: String,
    pub content: LlmMessageContent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmMessageParams {
    pub messages: Vec<LlmMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// 推理强度配置：必须透传到协议层（OpenAI Chat 走顶层 `reasoning_effort`，
    /// Anthropic Messages 走 `thinking.budget_tokens` 映射）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    Simple(String),
    Typed {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tool_arguments_preserves_non_empty_malformed_input() {
        let raw = r#"{"command":"printf hello""#;
        let (arguments, parse_error) = parse_tool_arguments(raw);

        assert_eq!(arguments, Value::String(raw.to_string()));
        assert!(parse_error.is_some());
    }

    #[test]
    fn tool_call_arguments_for_wire_prefers_raw_arguments() {
        let call = ToolCall {
            id: "tool-call-raw".to_string(),
            name: "shell_exec".to_string(),
            arguments: Value::String("parsed fallback".to_string()),
            argument_parse_error: Some("parse failed".to_string()),
            raw_arguments: Some(r#"{"command":"printf hello""#.to_string()),
        };

        assert_eq!(call.arguments_for_wire(), r#"{"command":"printf hello""#);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmResponse {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    pub usage: LlmUsage,
    pub stop_reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub cache_read_included_in_input: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmStreamChunkType {
    ContentStart,
    ContentDelta,
    ContentEnd,
    ToolCallStart,
    ToolCallDelta,
    ToolCallEnd,
    Thinking,
    Usage,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmStreamChunk {
    #[serde(rename = "type")]
    pub kind: LlmStreamChunkType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<PartialToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<LlmUsage>,
    /// LLM 返回的停止原因（如 "stop"、"length"、"tool_calls"、"end_turn" 等）。
    /// 仅在 ContentEnd 类型的 chunk 中出现。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PartialToolCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    /// OpenAI tool_call 在并行调用时通过 index 区分不同的 tool call。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
}

use once_cell::sync::Lazy;
use regex::Regex;

static SUMMARY_HIJACK_MAIN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)Your task is to create a detailed summary").unwrap());
static SUMMARY_HIJACK_NO_TOOLS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)IMPORTANT:\s*Do NOT use any tools").unwrap());
static SUMMARY_HIJACK_ANALYSIS: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)<analysis>").unwrap());
static SUMMARY_HIJACK_SUMMARY: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)<summary>").unwrap());

pub fn is_summary_hijack_text(text: &str) -> bool {
    let normalized = text.trim();
    if normalized.is_empty() {
        return false;
    }

    let has_main = SUMMARY_HIJACK_MAIN.is_match(normalized);
    let has_no_tools = SUMMARY_HIJACK_NO_TOOLS.is_match(normalized);
    let has_analysis = SUMMARY_HIJACK_ANALYSIS.is_match(normalized);
    let has_summary = SUMMARY_HIJACK_SUMMARY.is_match(normalized);
    let has_tag_pair = has_analysis && has_summary;

    if has_main && (has_no_tools || has_tag_pair) {
        return true;
    }
    has_no_tools && has_tag_pair
}

pub fn sanitize_summary_hijack_messages(messages: &[LlmMessage]) -> Vec<LlmMessage> {
    let mut sanitized = Vec::new();

    for message in messages {
        if message.role != "assistant" {
            sanitized.push(message.clone());
            continue;
        }

        match &message.content {
            LlmMessageContent::Text(text) => {
                if !is_summary_hijack_text(text) {
                    sanitized.push(message.clone());
                }
            }
            LlmMessageContent::Blocks(blocks) => {
                let has_tool_use = blocks
                    .iter()
                    .any(|b| matches!(b, LlmContentBlock::ToolUse { .. }));

                if !has_tool_use {
                    let merged_text: String = blocks
                        .iter()
                        .filter_map(|b| match b {
                            LlmContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    if !is_summary_hijack_text(&merged_text) {
                        sanitized.push(message.clone());
                    }
                    continue;
                }

                let filtered: Vec<LlmContentBlock> = blocks
                    .iter()
                    .filter(|b| match b {
                        LlmContentBlock::Text { text } => !is_summary_hijack_text(text),
                        _ => true,
                    })
                    .cloned()
                    .collect();

                if !filtered.is_empty() {
                    sanitized.push(LlmMessage {
                        role: message.role.clone(),
                        content: LlmMessageContent::Blocks(filtered),
                    });
                }
            }
        }
    }

    sanitized
}

pub fn sanitize_tool_order(messages: &[LlmMessage]) -> Vec<LlmMessage> {
    let has_tool_use = |msg: &LlmMessage| -> bool {
        matches!(&msg.content, LlmMessageContent::Blocks(blocks) if blocks.iter().any(|b| matches!(b, LlmContentBlock::ToolUse { .. })))
    };

    let is_tool_result_user = |msg: &LlmMessage| -> bool {
        msg.role == "user"
            && matches!(&msg.content, LlmMessageContent::Blocks(blocks) if blocks.iter().any(|b| matches!(b, LlmContentBlock::ToolResult { .. })))
    };

    let mut used_tool_ids = std::collections::HashSet::new();
    let mut synthetic_id_seq = 0u32;

    let allocate_tool_id = |preferred: Option<&str>,
                            used: &mut std::collections::HashSet<String>,
                            seq: &mut u32|
     -> String {
        let normalized = preferred
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(ref id) = normalized
            && !used.contains(id)
        {
            used.insert(id.clone());
            return id.clone();
        }
        loop {
            let candidate = format!("magi_tool_{}", *seq);
            *seq += 1;
            if !used.contains(&candidate) {
                used.insert(candidate.clone());
                return candidate;
            }
        }
    };

    let mut cleaned = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        let msg = &messages[i];

        if msg.role == "assistant" && has_tool_use(msg) {
            let next = messages.get(i + 1);
            let prev = cleaned.last();

            let valid_next = next.is_some_and(&is_tool_result_user);
            let valid_prev = prev.is_some_and(|p: &LlmMessage| p.role == "user");

            if !valid_next || !valid_prev {
                i += 1;
                continue;
            }

            if let LlmMessageContent::Blocks(blocks) = &msg.content {
                let mut ordered_tool_ids = Vec::new();
                let mut normalized_blocks = Vec::new();

                for block in blocks {
                    match block {
                        LlmContentBlock::ToolUse { id, name, input } => {
                            let new_id = allocate_tool_id(
                                Some(id),
                                &mut used_tool_ids,
                                &mut synthetic_id_seq,
                            );
                            ordered_tool_ids.push(new_id.clone());
                            normalized_blocks.push(LlmContentBlock::ToolUse {
                                id: new_id,
                                name: name.clone(),
                                input: input.clone(),
                            });
                        }
                        other => normalized_blocks.push(other.clone()),
                    }
                }

                if ordered_tool_ids.is_empty() {
                    i += 1;
                    continue;
                }

                let next_msg = &messages[i + 1];
                if let LlmMessageContent::Blocks(result_blocks) = &next_msg.content {
                    let available_ids = ordered_tool_ids.clone();
                    let available_set: std::collections::HashSet<String> =
                        available_ids.iter().cloned().collect();
                    let mut consumed = std::collections::HashSet::new();
                    let mut next_idx = 0usize;
                    let mut result_normalized = Vec::new();
                    let mut valid_count = 0u32;

                    for block in result_blocks {
                        match block {
                            LlmContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                                images,
                            } => {
                                let incoming = tool_use_id.trim().to_string();
                                let resolved = if !incoming.is_empty()
                                    && available_set.contains(&incoming)
                                    && !consumed.contains(&incoming)
                                {
                                    incoming
                                } else {
                                    let mut found = String::new();
                                    while next_idx < available_ids.len() {
                                        let candidate = &available_ids[next_idx];
                                        next_idx += 1;
                                        if !consumed.contains(candidate) {
                                            found = candidate.clone();
                                            break;
                                        }
                                    }
                                    found
                                };

                                if resolved.is_empty() {
                                    continue;
                                }

                                consumed.insert(resolved.clone());
                                valid_count += 1;
                                result_normalized.push(LlmContentBlock::ToolResult {
                                    tool_use_id: resolved,
                                    content: content.clone(),
                                    is_error: *is_error,
                                    images: images.clone(),
                                });
                            }
                            other => result_normalized.push(other.clone()),
                        }
                    }

                    if valid_count == 0 {
                        i += 2;
                        continue;
                    }

                    cleaned.push(LlmMessage {
                        role: "assistant".to_string(),
                        content: LlmMessageContent::Blocks(normalized_blocks),
                    });
                    cleaned.push(LlmMessage {
                        role: next_msg.role.clone(),
                        content: LlmMessageContent::Blocks(result_normalized),
                    });
                    i += 2;
                    continue;
                }
            }

            i += 1;
            continue;
        }

        if is_tool_result_user(msg) {
            let prev = cleaned.last();
            if prev.is_none_or(|p| !has_tool_use(p)) {
                if let LlmMessageContent::Blocks(blocks) = &msg.content {
                    let retained: Vec<LlmContentBlock> = blocks
                        .iter()
                        .filter(|b| !matches!(b, LlmContentBlock::ToolResult { .. }))
                        .cloned()
                        .collect();
                    if !retained.is_empty() {
                        cleaned.push(LlmMessage {
                            role: msg.role.clone(),
                            content: LlmMessageContent::Blocks(retained),
                        });
                    }
                }
                i += 1;
                continue;
            }
        }

        cleaned.push(msg.clone());
        i += 1;
    }

    cleaned
}
