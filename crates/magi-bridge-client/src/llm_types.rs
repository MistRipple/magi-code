use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: ToolInputSchema,
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
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub kind: String,
    pub media_type: String,
    pub data: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_idle_timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_hard_timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_policy: Option<RetryPolicy>,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryPolicy {
    #[serde(default)]
    pub max_retries: Option<u32>,
    #[serde(default)]
    pub base_delay_ms: Option<u64>,
    #[serde(default)]
    pub retry_delays_ms: Option<Vec<u64>>,
    #[serde(default)]
    pub retry_on_timeout: Option<bool>,
    #[serde(default)]
    pub retry_on_all_errors: Option<bool>,
    #[serde(default)]
    pub max_retry_duration_ms: Option<u64>,
    #[serde(default)]
    pub deterministic_error_streak_limit: Option<u32>,
    #[serde(default)]
    pub circuit_breaker: Option<CircuitBreakerConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircuitBreakerConfig {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub window_ms: Option<u64>,
    #[serde(default)]
    pub failure_threshold: Option<u32>,
    #[serde(default)]
    pub cooldown_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmResponse {
    pub content: String,
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
        if let Some(ref id) = normalized {
            if !used.contains(id) {
                used.insert(id.clone());
                return id.clone();
            }
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

            let valid_next = next.map_or(false, |n| is_tool_result_user(n));
            let valid_prev = prev.map_or(false, |p: &LlmMessage| p.role == "user");

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
            if prev.map_or(true, |p| !has_tool_use(p)) {
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
