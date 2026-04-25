use serde_json::Value;

use super::adapter::{AdaptedResponse, ProviderFamily};
use crate::llm_types::{LlmStreamChunk, LlmStreamChunkType, LlmUsage, PartialToolCall, ToolCall};

#[derive(Clone, Debug, Default)]
pub struct SseLineParser {
    buffer: String,
    current_event_type: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SseEvent {
    pub event_type: Option<String>,
    pub data: String,
}

impl SseLineParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, chunk: &str) -> Vec<SseEvent> {
        self.buffer.push_str(chunk);
        let mut events = Vec::new();
        let mut data_lines: Vec<String> = Vec::new();

        while let Some(newline_pos) = self.buffer.find('\n') {
            let line = self.buffer[..newline_pos]
                .trim_end_matches('\r')
                .to_string();
            self.buffer = self.buffer[newline_pos + 1..].to_string();

            if line.is_empty() {
                if !data_lines.is_empty() {
                    events.push(SseEvent {
                        event_type: self.current_event_type.take(),
                        data: data_lines.join("\n"),
                    });
                    data_lines.clear();
                }
                continue;
            }

            if let Some(value) = line
                .strip_prefix("data: ")
                .or_else(|| line.strip_prefix("data:"))
            {
                data_lines.push(value.to_string());
            } else if let Some(value) = line
                .strip_prefix("event: ")
                .or_else(|| line.strip_prefix("event:"))
            {
                self.current_event_type = Some(value.trim().to_string());
            }
        }

        events
    }
}

pub fn parse_stream_event(family: ProviderFamily, event: &SseEvent) -> Vec<LlmStreamChunk> {
    match family {
        ProviderFamily::OpenAiChat | ProviderFamily::OpenAiResponses => {
            parse_openai_stream_data(&event.data)
        }
        ProviderFamily::Anthropic => {
            parse_anthropic_stream_event(event.event_type.as_deref(), &event.data)
        }
        ProviderFamily::Gemini => parse_openai_stream_data(&event.data),
    }
}

fn parse_openai_stream_data(data: &str) -> Vec<LlmStreamChunk> {
    if data.trim() == "[DONE]" {
        return Vec::new();
    }

    let Ok(envelope) = serde_json::from_str::<Value>(data) else {
        return Vec::new();
    };

    let Some(choices) = envelope["choices"].as_array() else {
        if let Some(usage) = envelope.get("usage") {
            return vec![LlmStreamChunk {
                kind: LlmStreamChunkType::Usage,
                content: None,
                tool_call: None,
                thinking: None,
                usage: Some(parse_openai_usage_value(usage)),
                stop_reason: None,
            }];
        }
        return Vec::new();
    };

    let mut chunks = Vec::new();

    for choice in choices {
        let delta = &choice["delta"];

        if let Some(content) = delta["content"].as_str() {
            if !content.is_empty() {
                chunks.push(LlmStreamChunk {
                    kind: LlmStreamChunkType::ContentDelta,
                    content: Some(content.to_string()),
                    tool_call: None,
                    thinking: None,
                    usage: None,
                    stop_reason: None,
                });
            }
        }

        if let Some(reasoning) = delta["reasoning_content"].as_str() {
            if !reasoning.is_empty() {
                chunks.push(LlmStreamChunk {
                    kind: LlmStreamChunkType::Thinking,
                    content: None,
                    tool_call: None,
                    thinking: Some(reasoning.to_string()),
                    usage: None,
                    stop_reason: None,
                });
            }
        }

        if let Some(tool_calls) = delta["tool_calls"].as_array() {
            for tc in tool_calls {
                let func = &tc["function"];
                let id = tc["id"].as_str().map(str::to_string);
                let name = func["name"].as_str().map(str::to_string);
                let args = func["arguments"].as_str().map(str::to_string);
                let index = tc["index"].as_u64().map(|i| i as usize);

                let kind = if id.is_some() || name.is_some() {
                    LlmStreamChunkType::ToolCallStart
                } else {
                    LlmStreamChunkType::ToolCallDelta
                };

                chunks.push(LlmStreamChunk {
                    kind,
                    content: None,
                    tool_call: Some(PartialToolCall {
                        id,
                        name,
                        arguments: args.map(|a| Value::String(a)),
                        index,
                    }),
                    thinking: None,
                    usage: None,
                    stop_reason: None,
                });
            }
        }

        if let Some(reason) = choice["finish_reason"].as_str() {
            chunks.push(LlmStreamChunk {
                kind: LlmStreamChunkType::ContentEnd,
                content: None,
                tool_call: None,
                thinking: None,
                usage: None,
                stop_reason: Some(reason.to_string()),
            });
        }
    }

    if let Some(usage) = envelope.get("usage").filter(|u| !u.is_null()) {
        chunks.push(LlmStreamChunk {
            kind: LlmStreamChunkType::Usage,
            content: None,
            tool_call: None,
            thinking: None,
            usage: Some(parse_openai_usage_value(usage)),
            stop_reason: None,
        });
    }

    chunks
}

fn parse_anthropic_stream_event(event_type: Option<&str>, data: &str) -> Vec<LlmStreamChunk> {
    let Ok(envelope) = serde_json::from_str::<Value>(data) else {
        return Vec::new();
    };

    match event_type {
        Some("content_block_start") => {
            let block = &envelope["content_block"];
            match block["type"].as_str() {
                Some("text") => vec![LlmStreamChunk {
                    kind: LlmStreamChunkType::ContentStart,
                    content: block["text"].as_str().map(str::to_string),
                    tool_call: None,
                    thinking: None,
                    usage: None,
                    stop_reason: None,
                }],
                Some("tool_use") => vec![LlmStreamChunk {
                    kind: LlmStreamChunkType::ToolCallStart,
                    content: None,
                    tool_call: Some(PartialToolCall {
                        id: block["id"].as_str().map(str::to_string),
                        name: block["name"].as_str().map(str::to_string),
                        arguments: None,
                        index: None,
                    }),
                    thinking: None,
                    usage: None,
                    stop_reason: None,
                }],
                Some("thinking") => vec![LlmStreamChunk {
                    kind: LlmStreamChunkType::Thinking,
                    content: None,
                    tool_call: None,
                    thinking: block["thinking"].as_str().map(str::to_string),
                    usage: None,
                    stop_reason: None,
                }],
                _ => Vec::new(),
            }
        }
        Some("content_block_delta") => {
            let delta = &envelope["delta"];
            match delta["type"].as_str() {
                Some("text_delta") => vec![LlmStreamChunk {
                    kind: LlmStreamChunkType::ContentDelta,
                    content: delta["text"].as_str().map(str::to_string),
                    tool_call: None,
                    thinking: None,
                    usage: None,
                    stop_reason: None,
                }],
                Some("input_json_delta") => vec![LlmStreamChunk {
                    kind: LlmStreamChunkType::ToolCallDelta,
                    content: None,
                    tool_call: Some(PartialToolCall {
                        id: None,
                        name: None,
                        arguments: delta["partial_json"]
                            .as_str()
                            .map(|s| Value::String(s.to_string())),
                        index: None,
                    }),
                    thinking: None,
                    usage: None,
                    stop_reason: None,
                }],
                Some("thinking_delta") => vec![LlmStreamChunk {
                    kind: LlmStreamChunkType::Thinking,
                    content: None,
                    tool_call: None,
                    thinking: delta["thinking"].as_str().map(str::to_string),
                    usage: None,
                    stop_reason: None,
                }],
                _ => Vec::new(),
            }
        }
        Some("content_block_stop") => vec![LlmStreamChunk {
            kind: LlmStreamChunkType::ContentEnd,
            content: None,
            tool_call: None,
            thinking: None,
            usage: None,
            stop_reason: None,
        }],
        Some("message_start") => {
            let mut chunks = Vec::new();
            if let Some(usage) = envelope["message"].get("usage") {
                chunks.push(LlmStreamChunk {
                    kind: LlmStreamChunkType::Usage,
                    content: None,
                    tool_call: None,
                    thinking: None,
                    usage: Some(parse_anthropic_usage_value(usage)),
                    stop_reason: None,
                });
            }
            chunks
        }
        Some("message_delta") => {
            let mut chunks = Vec::new();
            // 捕获消息级别的 stop_reason
            let stop_reason = envelope["delta"]["stop_reason"]
                .as_str()
                .map(str::to_string);
            if stop_reason.is_some() {
                chunks.push(LlmStreamChunk {
                    kind: LlmStreamChunkType::ContentEnd,
                    content: None,
                    tool_call: None,
                    thinking: None,
                    usage: None,
                    stop_reason,
                });
            }
            if let Some(usage) = envelope.get("usage") {
                chunks.push(LlmStreamChunk {
                    kind: LlmStreamChunkType::Usage,
                    content: None,
                    tool_call: None,
                    thinking: None,
                    usage: Some(parse_anthropic_usage_value(usage)),
                    stop_reason: None,
                });
            }
            chunks
        }
        Some("message_stop") | Some("ping") | Some("error") => Vec::new(),
        _ => Vec::new(),
    }
}

fn parse_openai_usage_value(usage: &Value) -> LlmUsage {
    LlmUsage {
        input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: usage["prompt_tokens_details"]["cached_tokens"].as_u64(),
        cache_write_tokens: None,
    }
}

fn parse_anthropic_usage_value(usage: &Value) -> LlmUsage {
    LlmUsage {
        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: usage["cache_read_input_tokens"].as_u64(),
        cache_write_tokens: usage["cache_creation_input_tokens"].as_u64(),
    }
}

#[derive(Clone, Debug, Default)]
pub struct StreamAccumulator {
    content_parts: Vec<String>,
    thinking_parts: Vec<String>,
    active_tool_calls: Vec<ActiveToolCall>,
    usage: LlmUsage,
    stop_reason: Option<String>,
}

#[derive(Clone, Debug)]
struct ActiveToolCall {
    id: String,
    name: String,
    arguments_buffer: String,
}

impl StreamAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, chunk: &LlmStreamChunk) {
        match chunk.kind {
            LlmStreamChunkType::ContentStart | LlmStreamChunkType::ContentDelta => {
                if let Some(ref text) = chunk.content {
                    self.content_parts.push(text.clone());
                }
            }
            LlmStreamChunkType::ContentEnd => {
                // 捕获 stop_reason（来自 OpenAI finish_reason 或 Anthropic message_delta）
                if let Some(ref reason) = chunk.stop_reason {
                    self.stop_reason = Some(reason.clone());
                }
            }
            LlmStreamChunkType::ToolCallStart => {
                if let Some(ref tc) = chunk.tool_call {
                    self.active_tool_calls.push(ActiveToolCall {
                        id: tc.id.clone().unwrap_or_default(),
                        name: tc.name.clone().unwrap_or_default(),
                        arguments_buffer: String::new(),
                    });
                }
            }
            LlmStreamChunkType::ToolCallDelta => {
                if let Some(ref tc) = chunk.tool_call {
                    if let Some(args) = &tc.arguments {
                        let fragment = match args {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        // 使用 index 路由到正确的 tool call（OpenAI 并行调用），
                        // 无 index 时回退到最后一个（Anthropic 顺序调用）
                        let target_idx = tc
                            .index
                            .filter(|idx| *idx < self.active_tool_calls.len())
                            .or_else(|| {
                                if self.active_tool_calls.is_empty() {
                                    None
                                } else {
                                    Some(self.active_tool_calls.len() - 1)
                                }
                            });
                        if let Some(idx) = target_idx {
                            self.active_tool_calls[idx]
                                .arguments_buffer
                                .push_str(&fragment);
                        }
                    }
                }
            }
            LlmStreamChunkType::ToolCallEnd => {}
            LlmStreamChunkType::Thinking => {
                if let Some(ref text) = chunk.thinking {
                    self.thinking_parts.push(text.clone());
                }
            }
            LlmStreamChunkType::Usage => {
                if let Some(ref u) = chunk.usage {
                    self.usage.input_tokens = self.usage.input_tokens.max(u.input_tokens);
                    self.usage.output_tokens = self.usage.output_tokens.max(u.output_tokens);
                    if u.cache_read_tokens.is_some() {
                        self.usage.cache_read_tokens = u.cache_read_tokens;
                    }
                    if u.cache_write_tokens.is_some() {
                        self.usage.cache_write_tokens = u.cache_write_tokens;
                    }
                }
            }
        }
    }

    pub fn apply_all(&mut self, chunks: &[LlmStreamChunk]) {
        for chunk in chunks {
            self.apply(chunk);
        }
    }

    pub fn finalize(self) -> AdaptedResponse {
        let tool_calls: Vec<ToolCall> = self
            .active_tool_calls
            .into_iter()
            .map(|tc| {
                let args_parsed = serde_json::from_str::<Value>(&tc.arguments_buffer)
                    .unwrap_or(serde_json::json!({}));
                ToolCall {
                    id: tc.id,
                    name: tc.name,
                    arguments: args_parsed,
                    argument_parse_error: None,
                    raw_arguments: Some(tc.arguments_buffer),
                }
            })
            .collect();

        let stop_reason = self.stop_reason.unwrap_or_else(|| {
            if tool_calls.is_empty() {
                "end_turn".to_string()
            } else {
                "tool_use".to_string()
            }
        });

        AdaptedResponse {
            content: self.content_parts.join(""),
            tool_calls,
            usage: self.usage,
            stop_reason,
            raw: None,
        }
    }

    pub fn accumulated_content(&self) -> String {
        self.content_parts.join("")
    }

    pub fn accumulated_thinking(&self) -> String {
        self.thinking_parts.join("")
    }

    pub fn pending_tool_call_count(&self) -> usize {
        self.active_tool_calls.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parser_yields_events_from_chunked_input() {
        let mut parser = SseLineParser::new();

        let events = parser.feed("data: {\"type\":\"ping\"}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "{\"type\":\"ping\"}");
        assert!(events[0].event_type.is_none());
    }

    #[test]
    fn sse_parser_handles_event_type_prefix() {
        let mut parser = SseLineParser::new();

        let events = parser.feed("event: content_block_delta\ndata: {\"delta\":{}}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type.as_deref(), Some("content_block_delta"));
    }

    #[test]
    fn sse_parser_handles_split_chunks() {
        let mut parser = SseLineParser::new();

        let e1 = parser.feed("data: {\"part\"");
        assert!(e1.is_empty());

        let e2 = parser.feed(":1}\n\n");
        assert_eq!(e2.len(), 1);
        assert_eq!(e2[0].data, "{\"part\":1}");
    }

    #[test]
    fn sse_parser_handles_multiple_events_in_one_chunk() {
        let mut parser = SseLineParser::new();
        let events = parser.feed("data: {\"a\":1}\n\ndata: {\"b\":2}\n\n");
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn openai_stream_parses_content_delta() {
        let data = r#"{"choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let chunks = parse_openai_stream_data(data);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].kind, LlmStreamChunkType::ContentDelta);
        assert_eq!(chunks[0].content.as_deref(), Some("Hello"));
    }

    #[test]
    fn openai_stream_parses_tool_call_start_and_delta() {
        let start = r#"{"choices":[{"delta":{"tool_calls":[{"id":"call_1","function":{"name":"search","arguments":""}}]},"finish_reason":null}]}"#;
        let delta = r#"{"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"{\"q\":"}}]},"finish_reason":null}]}"#;

        let start_chunks = parse_openai_stream_data(start);
        assert_eq!(start_chunks.len(), 1);
        assert_eq!(start_chunks[0].kind, LlmStreamChunkType::ToolCallStart);
        assert_eq!(
            start_chunks[0].tool_call.as_ref().unwrap().id.as_deref(),
            Some("call_1")
        );
        assert_eq!(
            start_chunks[0].tool_call.as_ref().unwrap().name.as_deref(),
            Some("search")
        );

        let delta_chunks = parse_openai_stream_data(delta);
        assert_eq!(delta_chunks.len(), 1);
        assert_eq!(delta_chunks[0].kind, LlmStreamChunkType::ToolCallDelta);
    }

    #[test]
    fn openai_stream_parses_done_marker() {
        let chunks = parse_openai_stream_data("[DONE]");
        assert!(chunks.is_empty());
    }

    #[test]
    fn openai_stream_parses_usage() {
        let data = r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":50}}"#;
        let chunks = parse_openai_stream_data(data);
        let usage_chunk = chunks.iter().find(|c| c.kind == LlmStreamChunkType::Usage);
        assert!(usage_chunk.is_some());
        let u = usage_chunk.unwrap().usage.as_ref().unwrap();
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.output_tokens, 50);
    }

    #[test]
    fn openai_stream_parses_finish_reason() {
        let data = r#"{"choices":[{"delta":{"content":"end"},"finish_reason":"stop"}]}"#;
        let chunks = parse_openai_stream_data(data);
        assert!(
            chunks
                .iter()
                .any(|c| c.kind == LlmStreamChunkType::ContentDelta)
        );
        assert!(
            chunks
                .iter()
                .any(|c| c.kind == LlmStreamChunkType::ContentEnd)
        );
    }

    #[test]
    fn anthropic_stream_parses_text_block_lifecycle() {
        let start_chunks = parse_anthropic_stream_event(
            Some("content_block_start"),
            r#"{"content_block":{"type":"text","text":""}}"#,
        );
        assert_eq!(start_chunks.len(), 1);
        assert_eq!(start_chunks[0].kind, LlmStreamChunkType::ContentStart);

        let delta_chunks = parse_anthropic_stream_event(
            Some("content_block_delta"),
            r#"{"delta":{"type":"text_delta","text":"Hello"}}"#,
        );
        assert_eq!(delta_chunks.len(), 1);
        assert_eq!(delta_chunks[0].kind, LlmStreamChunkType::ContentDelta);
        assert_eq!(delta_chunks[0].content.as_deref(), Some("Hello"));

        let stop_chunks =
            parse_anthropic_stream_event(Some("content_block_stop"), r#"{"index":0}"#);
        assert_eq!(stop_chunks.len(), 1);
        assert_eq!(stop_chunks[0].kind, LlmStreamChunkType::ContentEnd);
    }

    #[test]
    fn anthropic_stream_parses_tool_use_block() {
        let start = parse_anthropic_stream_event(
            Some("content_block_start"),
            r#"{"content_block":{"type":"tool_use","id":"toolu_1","name":"search"}}"#,
        );
        assert_eq!(start.len(), 1);
        assert_eq!(start[0].kind, LlmStreamChunkType::ToolCallStart);
        assert_eq!(
            start[0].tool_call.as_ref().unwrap().id.as_deref(),
            Some("toolu_1")
        );

        let delta = parse_anthropic_stream_event(
            Some("content_block_delta"),
            r#"{"delta":{"type":"input_json_delta","partial_json":"{\"query\":"}}"#,
        );
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].kind, LlmStreamChunkType::ToolCallDelta);
    }

    #[test]
    fn anthropic_stream_parses_message_start_usage() {
        let chunks = parse_anthropic_stream_event(
            Some("message_start"),
            r#"{"message":{"usage":{"input_tokens":200,"output_tokens":0}}}"#,
        );
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].kind, LlmStreamChunkType::Usage);
        assert_eq!(chunks[0].usage.as_ref().unwrap().input_tokens, 200);
    }

    #[test]
    fn anthropic_stream_parses_message_delta_usage() {
        let chunks = parse_anthropic_stream_event(
            Some("message_delta"),
            r#"{"usage":{"output_tokens":42}}"#,
        );
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].usage.as_ref().unwrap().output_tokens, 42);
    }

    #[test]
    fn anthropic_stream_ignores_ping_and_stop() {
        assert!(parse_anthropic_stream_event(Some("ping"), "{}").is_empty());
        assert!(parse_anthropic_stream_event(Some("message_stop"), "{}").is_empty());
    }

    #[test]
    fn accumulator_collects_text_content() {
        let mut acc = StreamAccumulator::new();
        acc.apply(&LlmStreamChunk {
            kind: LlmStreamChunkType::ContentDelta,
            content: Some("Hello ".to_string()),
            tool_call: None,
            thinking: None,
            usage: None,
            stop_reason: None,
        });
        acc.apply(&LlmStreamChunk {
            kind: LlmStreamChunkType::ContentDelta,
            content: Some("world".to_string()),
            tool_call: None,
            thinking: None,
            usage: None,
            stop_reason: None,
        });

        assert_eq!(acc.accumulated_content(), "Hello world");
        let result = acc.finalize();
        assert_eq!(result.content, "Hello world");
        assert!(result.tool_calls.is_empty());
        assert_eq!(result.stop_reason, "end_turn");
    }

    #[test]
    fn accumulator_collects_tool_calls() {
        let mut acc = StreamAccumulator::new();
        acc.apply(&LlmStreamChunk {
            kind: LlmStreamChunkType::ToolCallStart,
            content: None,
            tool_call: Some(PartialToolCall {
                id: Some("call_1".to_string()),
                name: Some("search".to_string()),
                arguments: None,
                index: None,
            }),
            thinking: None,
            usage: None,
            stop_reason: None,
        });
        acc.apply(&LlmStreamChunk {
            kind: LlmStreamChunkType::ToolCallDelta,
            content: None,
            tool_call: Some(PartialToolCall {
                id: None,
                name: None,
                arguments: Some(Value::String(r#"{"q":"#.to_string())),
                index: None,
            }),
            thinking: None,
            usage: None,
            stop_reason: None,
        });
        acc.apply(&LlmStreamChunk {
            kind: LlmStreamChunkType::ToolCallDelta,
            content: None,
            tool_call: Some(PartialToolCall {
                id: None,
                name: None,
                arguments: Some(Value::String(r#""test"}"#.to_string())),
                index: None,
            }),
            thinking: None,
            usage: None,
            stop_reason: None,
        });

        assert_eq!(acc.pending_tool_call_count(), 1);
        let result = acc.finalize();
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].id, "call_1");
        assert_eq!(result.tool_calls[0].name, "search");
        assert_eq!(result.tool_calls[0].arguments["q"], "test");
        assert_eq!(result.stop_reason, "tool_use");
    }

    #[test]
    fn accumulator_merges_usage() {
        let mut acc = StreamAccumulator::new();
        acc.apply(&LlmStreamChunk {
            kind: LlmStreamChunkType::Usage,
            content: None,
            tool_call: None,
            thinking: None,
            usage: Some(LlmUsage {
                input_tokens: 100,
                output_tokens: 0,
                cache_read_tokens: Some(50),
                cache_write_tokens: None,
            }),
            stop_reason: None,
        });
        acc.apply(&LlmStreamChunk {
            kind: LlmStreamChunkType::Usage,
            content: None,
            tool_call: None,
            thinking: None,
            usage: Some(LlmUsage {
                input_tokens: 100,
                output_tokens: 42,
                cache_read_tokens: None,
                cache_write_tokens: None,
            }),
            stop_reason: None,
        });

        let result = acc.finalize();
        assert_eq!(result.usage.input_tokens, 100);
        assert_eq!(result.usage.output_tokens, 42);
        assert_eq!(result.usage.cache_read_tokens, Some(50));
    }

    #[test]
    fn end_to_end_openai_stream_to_response() {
        let sse_payload = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\" there\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n",
            "data: [DONE]\n\n",
        );

        let mut parser = SseLineParser::new();
        let mut acc = StreamAccumulator::new();

        let events = parser.feed(sse_payload);
        for event in &events {
            let chunks = parse_stream_event(ProviderFamily::OpenAiChat, event);
            acc.apply_all(&chunks);
        }

        let result = acc.finalize();
        assert_eq!(result.content, "Hi there");
        assert!(result.tool_calls.is_empty());
        assert_eq!(result.usage.input_tokens, 10);
        assert_eq!(result.usage.output_tokens, 5);
    }

    #[test]
    fn end_to_end_anthropic_stream_to_response() {
        let sse_payload = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":50,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\" Claude\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"usage\":{\"output_tokens\":8}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );

        let mut parser = SseLineParser::new();
        let mut acc = StreamAccumulator::new();

        let events = parser.feed(sse_payload);
        for event in &events {
            let chunks = parse_stream_event(ProviderFamily::Anthropic, event);
            acc.apply_all(&chunks);
        }

        let result = acc.finalize();
        assert_eq!(result.content, "Hello Claude");
        assert!(result.tool_calls.is_empty());
        assert_eq!(result.usage.input_tokens, 50);
        assert_eq!(result.usage.output_tokens, 8);
    }

    #[test]
    fn end_to_end_anthropic_tool_call_stream() {
        let sse_payload = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":100}}}\n\n",
            "event: content_block_start\ndata: {\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\"Let me search.\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: content_block_start\ndata: {\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_abc\",\"name\":\"search\"}}\n\n",
            "event: content_block_delta\ndata: {\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"query\\\": \"}}\n\n",
            "event: content_block_delta\ndata: {\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"rust\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":1}\n\n",
            "event: message_delta\ndata: {\"usage\":{\"output_tokens\":30}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );

        let mut parser = SseLineParser::new();
        let mut acc = StreamAccumulator::new();

        let events = parser.feed(sse_payload);
        for event in &events {
            let chunks = parse_stream_event(ProviderFamily::Anthropic, event);
            acc.apply_all(&chunks);
        }

        let result = acc.finalize();
        assert_eq!(result.content, "Let me search.");
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].id, "toolu_abc");
        assert_eq!(result.tool_calls[0].name, "search");
        assert_eq!(result.tool_calls[0].arguments["query"], "rust");
        assert_eq!(result.stop_reason, "tool_use");
    }

    #[test]
    fn accumulator_tracks_thinking_content() {
        let mut acc = StreamAccumulator::new();
        acc.apply(&LlmStreamChunk {
            kind: LlmStreamChunkType::Thinking,
            content: None,
            tool_call: None,
            thinking: Some("Let me think...".to_string()),
            usage: None,
            stop_reason: None,
        });
        acc.apply(&LlmStreamChunk {
            kind: LlmStreamChunkType::Thinking,
            content: None,
            tool_call: None,
            thinking: Some(" about this.".to_string()),
            usage: None,
            stop_reason: None,
        });
        assert_eq!(acc.accumulated_thinking(), "Let me think... about this.");
    }
}
