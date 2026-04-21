use crate::base::{
    flush_pending_thinking_to_blocks, BaseNormalizer, CallerContext, NormalizerConfig,
};
use crate::types::*;

pub fn create_gemini_normalizer(
    agent: &str,
    source: MessageSource,
    debug: bool,
    caller_context: CallerContext,
) -> GeminiNormalizer {
    GeminiNormalizer {
        normalizer: BaseNormalizer::new(NormalizerConfig {
            agent: agent.to_string(),
            default_source: source,
            debug,
            caller_context,
        }),
        json_buffer: String::new(),
    }
}

pub struct GeminiNormalizer {
    pub normalizer: BaseNormalizer,
    json_buffer: String,
}

impl GeminiNormalizer {
    pub fn parse_chunk(&mut self, message_id: &str, chunk: &str) {
        let trimmed = chunk.trim();
        if trimmed.starts_with('{') || !self.json_buffer.is_empty() {
            self.json_buffer.push_str(chunk);
            match serde_json::from_str::<serde_json::Value>(&self.json_buffer) {
                Ok(data) => {
                    self.process_json_data(message_id, &data);
                    self.json_buffer.clear();
                }
                Err(_) => {}
            }
        } else {
            if let Some(ctx) = self.normalizer.get_context_mut(message_id) {
                flush_pending_thinking_to_blocks(ctx);
            }
            self.normalizer.process_text_delta(message_id, chunk);
        }
    }

    fn process_json_data(&mut self, message_id: &str, data: &serde_json::Value) {
        let data_type = data.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match data_type {
            "text" => {
                if let Some(content) = data.get("content").and_then(|c| c.as_str()) {
                    if let Some(ctx) = self.normalizer.get_context_mut(message_id) {
                        flush_pending_thinking_to_blocks(ctx);
                    }
                    self.normalizer.process_text_delta(message_id, content);
                }
            }
            "thinking" | "reasoning" => {
                let thinking = data
                    .get("content")
                    .or(data.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !thinking.is_empty() {
                    self.normalizer.process_thinking(message_id, thinking);
                }
            }
            "tool_call" | "function_call" => {
                let tool_name = data
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown");
                let tool_id = data
                    .get("id")
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(generate_message_id);
                let raw_input = data.get("args").or(data.get("input"));
                let input_str = raw_input
                    .map(|v| serde_json::to_string_pretty(v).unwrap_or_default());
                self.normalizer.add_tool_call(
                    message_id,
                    ToolCallBlock {
                        tool_name: tool_name.to_string(),
                        tool_id,
                        status: ToolCallStatus::Running,
                        input: input_str,
                        output: None,
                        error: None,
                    },
                );
            }
            "tool_result" | "function_result" => {
                if let Some(ctx) = self.normalizer.get_context_mut(message_id) {
                    if let Some(tool_id) = ctx.active_tool_calls.keys().last().cloned() {
                        let output = data
                            .get("result")
                            .or(data.get("output"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let error = data.get("error").and_then(|e| e.as_str()).map(|s| s.to_string());
                        let _ = ctx;
                        self.normalizer.finish_tool_call(
                            message_id,
                            &tool_id,
                            if error.is_some() { None } else { Some(output) },
                            error,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    pub fn finalize(&mut self, _message_id: &str) {
        self.json_buffer.clear();
    }
}
