use crate::base::{
    flush_pending_thinking_to_blocks, BaseNormalizer, CallerContext, NormalizerConfig,
};
use crate::types::*;

pub fn create_codex_normalizer(
    agent: &str,
    source: MessageSource,
    debug: bool,
    caller_context: CallerContext,
) -> CodexNormalizer {
    CodexNormalizer {
        normalizer: BaseNormalizer::new(NormalizerConfig {
            agent: agent.to_string(),
            default_source: source,
            debug,
            caller_context,
        }),
        json_buffer: String::new(),
    }
}

pub struct CodexNormalizer {
    pub normalizer: BaseNormalizer,
    json_buffer: String,
}

impl CodexNormalizer {
    pub fn parse_chunk(&mut self, message_id: &str, chunk: &str) {
        let combined = format!("{}{}", self.json_buffer, chunk);
        let lines: Vec<&str> = combined.split('\n').collect();
        let (complete_lines, remainder) = lines.split_at(lines.len().saturating_sub(1));
        self.json_buffer = remainder.first().unwrap_or(&"").to_string();

        let mut plain_text = String::new();
        for line in complete_lines {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                plain_text.push('\n');
                continue;
            }
            match serde_json::from_str::<serde_json::Value>(trimmed) {
                Ok(event) => {
                    if !plain_text.is_empty() {
                        self.normalizer
                            .process_text_delta(message_id, &plain_text);
                        plain_text.clear();
                    }
                    self.process_json_event(message_id, &event);
                }
                Err(_) => {
                    plain_text.push_str(line);
                    plain_text.push('\n');
                }
            }
        }
        if !plain_text.is_empty() {
            if let Some(ctx) = self.normalizer.get_context_mut(message_id) {
                flush_pending_thinking_to_blocks(ctx);
            }
            self.normalizer
                .process_text_delta(message_id, &plain_text);
        }
    }

    fn process_json_event(&mut self, message_id: &str, event: &serde_json::Value) {
        if let Some(text) = extract_text(event) {
            if let Some(ctx) = self.normalizer.get_context_mut(message_id) {
                flush_pending_thinking_to_blocks(ctx);
            }
            self.normalizer.process_text_delta(message_id, &text);
        }

        if let Some(item) = event.get("item") {
            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if let Some(item_text) = extract_text(item) {
                if item_type == "reasoning" {
                    self.normalizer.process_thinking(message_id, &item_text);
                    return;
                }
                if let Some(ctx) = self.normalizer.get_context_mut(message_id) {
                    flush_pending_thinking_to_blocks(ctx);
                }
                self.normalizer
                    .process_text_delta(message_id, &item_text);
            }
        }

        if let Some(delta) = event.get("delta") {
            if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                if let Some(ctx) = self.normalizer.get_context_mut(message_id) {
                    flush_pending_thinking_to_blocks(ctx);
                }
                self.normalizer.process_text_delta(message_id, text);
            }
        }
    }

    pub fn finalize(&mut self, _message_id: &str) {
        self.json_buffer.clear();
    }
}

fn extract_text(value: &serde_json::Value) -> Option<String> {
    value
        .get("text")
        .or(value.get("content"))
        .or(value.get("message"))
        .and_then(|v| {
            if let Some(s) = v.as_str() {
                return Some(s.to_string());
            }
            if v.is_object() {
                return v
                    .get("text")
                    .or(v.get("content"))
                    .and_then(|inner| inner.as_str())
                    .map(|s| s.to_string());
            }
            None
        })
}
