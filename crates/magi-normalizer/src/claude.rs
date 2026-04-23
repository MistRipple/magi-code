use magi_core::UtcMillis;

use crate::base::{
    BaseNormalizer, CallerContext, NormalizerConfig, NormalizerEvent,
    flush_pending_thinking_to_blocks,
};
use crate::types::*;

pub fn create_claude_normalizer(
    agent: &str,
    source: MessageSource,
    debug: bool,
    caller_context: CallerContext,
) -> ClaudeNormalizer {
    ClaudeNormalizer {
        normalizer: BaseNormalizer::new(NormalizerConfig {
            agent: agent.to_string(),
            default_source: source,
            debug,
            caller_context,
        }),
        json_buffer: String::new(),
        pending_tool_input_json: String::new(),
    }
}

pub struct ClaudeNormalizer {
    pub normalizer: BaseNormalizer,
    json_buffer: String,
    pending_tool_input_json: String,
}

impl ClaudeNormalizer {
    pub fn parse_chunk(&mut self, message_id: &str, chunk: &str) {
        self.json_buffer.push_str(chunk);

        let owned_lines: Vec<String> = self
            .json_buffer
            .split('\n')
            .map(|s| s.to_string())
            .collect();
        let last = owned_lines.last().cloned().unwrap_or_default();
        let complete_count = owned_lines.len().saturating_sub(1);
        self.json_buffer = last;

        for line in &owned_lines[..complete_count] {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<serde_json::Value>(trimmed) {
                Ok(event) => {
                    let unwrapped =
                        if event.get("type").and_then(|t| t.as_str()) == Some("stream_event") {
                            event.get("event").cloned().unwrap_or(event)
                        } else {
                            event
                        };
                    self.process_event(message_id, &unwrapped);
                }
                Err(_) => {
                    self.normalizer.process_text_delta(message_id, trimmed);
                    self.normalizer.process_text_delta(message_id, "\n");
                }
            }
        }
    }

    fn process_event(&mut self, message_id: &str, event: &serde_json::Value) {
        let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

        if event_type == "assistant" {
            if let Some(content) = event
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            {
                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            self.normalizer.process_text_delta(message_id, text);
                        }
                    }
                }
            }
            return;
        }

        if event_type == "result" {
            let text = event
                .get("result")
                .and_then(|r| {
                    r.as_str().map(|s| s.to_string()).or_else(|| {
                        r.get("content")
                            .or(r.get("output"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    })
                })
                .unwrap_or_default();
            if !text.is_empty() {
                self.normalizer.process_text_delta(message_id, &text);
            }
            return;
        }

        match event_type {
            "content_block_start" => {
                let block_type = event
                    .get("content_block")
                    .and_then(|b| b.get("type"))
                    .and_then(|t| t.as_str());
                if block_type == Some("tool_use") {
                    if let Some(block) = event.get("content_block") {
                        self.pending_tool_input_json.clear();
                        let tool_name = block
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown");
                        let tool_id = block
                            .get("id")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(generate_message_id);
                        self.normalizer.add_tool_call(
                            message_id,
                            ToolCallBlock {
                                tool_name: tool_name.to_string(),
                                tool_id,
                                status: ToolCallStatus::Running,
                                input: None,
                                output: None,
                                error: None,
                            },
                        );
                    }
                }
            }
            "content_block_delta" => {
                if let Some(delta) = event.get("delta") {
                    let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match delta_type {
                        "text_delta" => {
                            if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                if let Some(ctx) = self.normalizer.get_context_mut(message_id) {
                                    flush_pending_thinking_to_blocks(ctx);
                                }
                                self.normalizer.process_text_delta(message_id, text);
                            }
                        }
                        "thinking_delta" => {
                            if let Some(thinking) = delta.get("thinking").and_then(|t| t.as_str()) {
                                self.normalizer.process_thinking(message_id, thinking);
                            }
                        }
                        "input_json_delta" => {
                            if let Some(json) = delta.get("partial_json").and_then(|t| t.as_str()) {
                                self.pending_tool_input_json.push_str(json);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "content_block_stop" => {
                if !self.pending_tool_input_json.is_empty() {
                    if let Some(ctx) = self.normalizer.get_context_mut(message_id) {
                        if let Some(tool_id) = ctx.active_tool_calls.keys().last().cloned() {
                            if let Some(tc) = ctx.active_tool_calls.get_mut(&tool_id) {
                                let formatted = match serde_json::from_str::<serde_json::Value>(
                                    &self.pending_tool_input_json,
                                ) {
                                    Ok(v) => serde_json::to_string_pretty(&v)
                                        .unwrap_or_else(|_| self.pending_tool_input_json.clone()),
                                    Err(_) => self.pending_tool_input_json.clone(),
                                };
                                tc.input = Some(formatted);
                                let update = StreamUpdate::MergeBlock {
                                    message_id: message_id.to_string(),
                                    timestamp: UtcMillis::now().0,
                                    blocks: Some(vec![ContentBlock::ToolCall(tc.clone())]),
                                    token_usage: None,
                                };
                                self.normalizer.push_event(NormalizerEvent::Update(update));
                            }
                        }
                    }
                    self.pending_tool_input_json.clear();
                }
            }
            "tool_result" => {
                if let Some(result) = event.get("result") {
                    if let Some(ctx) = self.normalizer.get_context_mut(message_id) {
                        if let Some(tool_id) = ctx.active_tool_calls.keys().last().cloned() {
                            let (output, error) = if result.is_string() {
                                (result.as_str().map(|s| s.to_string()), None)
                            } else {
                                let is_error = result
                                    .get("is_error")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                let text = result
                                    .get("content")
                                    .or(result.get("output"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                if is_error {
                                    (None, Some(text))
                                } else {
                                    (Some(text), None)
                                }
                            };
                            let _ = ctx;
                            self.normalizer
                                .finish_tool_call(message_id, &tool_id, output, error);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    pub fn finalize(&mut self, _message_id: &str) {
        if !self.json_buffer.trim().is_empty() {
            self.json_buffer.clear();
        }
    }
}
