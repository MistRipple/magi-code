use serde_json::Value;

use super::adapter::{AdaptedResponse, ProviderFamily};
use crate::llm_types::{
    LlmStreamChunk, LlmStreamChunkType, LlmUsage, PartialToolCall, ToolCall, parse_tool_arguments,
};
use crate::types::ModelProviderContext;

#[derive(Clone, Debug)]
pub enum ProviderContextStreamDelta {
    Start {
        index: usize,
        context: ModelProviderContext,
    },
    Append {
        index: usize,
        field: &'static str,
        value: String,
    },
    ReasoningContentAppend {
        field: &'static str,
        value: String,
    },
}

pub fn parse_stream_provider_context(
    family: ProviderFamily,
    event: &SseEvent,
) -> Option<ProviderContextStreamDelta> {
    let envelope = serde_json::from_str::<Value>(&event.data).ok()?;
    if family == ProviderFamily::OpenAiChat {
        let (field, value) = envelope["choices"].as_array()?.iter().find_map(|choice| {
            ["reasoning_content", "reasoning_text", "reasoning"]
                .into_iter()
                .find_map(|field| choice["delta"][field].as_str().map(|value| (field, value)))
        })?;
        return (!value.is_empty()).then_some(ProviderContextStreamDelta::ReasoningContentAppend {
            field,
            value: value.to_string(),
        });
    }
    if family == ProviderFamily::OpenAiResponses {
        let item = envelope.get("item")?;
        if !matches!(
            item["type"].as_str(),
            Some("message" | "reasoning" | "function_call")
        ) {
            return None;
        }
        if event.event_type.as_deref() == Some("response.output_item.done") {
            return Some(ProviderContextStreamDelta::Start {
                // `done` 才携带可重放的完整原始 item；`added` 中的函数参数和
                // reasoning 加密上下文通常尚未齐全，不能提前持久化。
                index: responses_output_index(&envelope).unwrap_or_else(|| {
                    item["id"]
                        .as_str()
                        .or_else(|| item["call_id"].as_str())
                        .map(stable_provider_context_index)
                        .unwrap_or(usize::MAX)
                }),
                context: ModelProviderContext {
                    provider: "openai_responses".to_string(),
                    kind: "response_output_item".to_string(),
                    data: item.clone(),
                },
            });
        }
        return None;
    }
    if family != ProviderFamily::Anthropic {
        return None;
    }
    let index = envelope["index"].as_u64()? as usize;
    match event.event_type.as_deref() {
        Some("content_block_start") => {
            let block = envelope.get("content_block")?;
            let kind = block["type"].as_str()?;
            if !matches!(kind, "thinking" | "redacted_thinking") {
                return None;
            }
            Some(ProviderContextStreamDelta::Start {
                index,
                context: ModelProviderContext {
                    provider: "anthropic".to_string(),
                    kind: kind.to_string(),
                    data: block.clone(),
                },
            })
        }
        Some("content_block_delta") => {
            let delta = envelope.get("delta")?;
            match delta["type"].as_str()? {
                "thinking_delta" => Some(ProviderContextStreamDelta::Append {
                    index,
                    field: "thinking",
                    value: delta["thinking"].as_str()?.to_string(),
                }),
                "signature_delta" => Some(ProviderContextStreamDelta::Append {
                    index,
                    field: "signature",
                    value: delta["signature"].as_str()?.to_string(),
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

#[derive(Clone, Debug, Default)]
pub struct SseLineParser {
    buffer: String,
    current_event_type: Option<String>,
    /// 已收到的 `data:` 行，等待空行 terminator 触发提交。
    ///
    /// 必须是结构体字段而非 `feed()` 局部变量——上游 chunked transfer
    /// 经常把 `event: foo\n` / `data: bar\n` / `\n` 拆成多个 HTTP chunk，
    /// reqwest 解码后每次 `read()` 可能只返回一条 `data:` 行（不带 terminator）。
    /// 若 data_lines 局部化，跨 `feed()` 调用就会被丢弃，所有 text_delta
    /// 都会被静默吞掉，最终 final_content 为空。
    pending_data_lines: Vec<String>,
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

        while let Some(newline_pos) = self.buffer.find('\n') {
            let line = self.buffer[..newline_pos]
                .trim_end_matches('\r')
                .to_string();
            self.buffer = self.buffer[newline_pos + 1..].to_string();

            if line.is_empty() {
                let event_type = self.current_event_type.take();
                if !self.pending_data_lines.is_empty() {
                    events.push(SseEvent {
                        event_type,
                        data: self.pending_data_lines.join("\n"),
                    });
                    self.pending_data_lines.clear();
                }
                continue;
            }

            if let Some(value) = line
                .strip_prefix("data: ")
                .or_else(|| line.strip_prefix("data:"))
            {
                self.pending_data_lines.push(value.to_string());
            } else if let Some(value) = line
                .strip_prefix("event: ")
                .or_else(|| line.strip_prefix("event:"))
            {
                self.current_event_type = Some(value.trim().to_string());
            }
        }

        events
    }

    pub fn finish(&mut self) -> Vec<SseEvent> {
        if self.buffer.is_empty() && self.pending_data_lines.is_empty() {
            self.current_event_type = None;
            return Vec::new();
        }
        self.feed("\n\n")
    }
}

pub fn parse_stream_event(family: ProviderFamily, event: &SseEvent) -> Vec<LlmStreamChunk> {
    match family {
        ProviderFamily::OpenAiChat => parse_openai_stream_data(&event.data),
        ProviderFamily::OpenAiResponses => {
            parse_openai_responses_stream_event(event.event_type.as_deref(), &event.data)
        }
        ProviderFamily::Anthropic => {
            parse_anthropic_stream_event(event.event_type.as_deref(), &event.data)
        }
    }
}

fn parse_openai_responses_stream_event(
    event_type: Option<&str>,
    data: &str,
) -> Vec<LlmStreamChunk> {
    if data.trim() == "[DONE]" {
        return Vec::new();
    }
    let Ok(envelope) = serde_json::from_str::<Value>(data) else {
        return Vec::new();
    };
    let empty = || LlmStreamChunk {
        kind: LlmStreamChunkType::ContentDelta,
        content: None,
        tool_call: None,
        thinking: None,
        usage: None,
        stop_reason: None,
    };

    match event_type {
        Some("response.output_text.delta") => {
            let mut chunk = empty();
            chunk.content = envelope["delta"].as_str().map(str::to_string);
            vec![chunk]
        }
        Some("response.refusal.delta") => {
            let mut chunk = empty();
            chunk.content = envelope["delta"].as_str().map(str::to_string);
            vec![chunk]
        }
        Some("response.reasoning_summary_text.delta" | "response.reasoning_text.delta") => {
            vec![LlmStreamChunk {
                kind: LlmStreamChunkType::Thinking,
                content: None,
                tool_call: None,
                thinking: envelope["delta"].as_str().map(str::to_string),
                usage: None,
                stop_reason: None,
            }]
        }
        Some("response.output_item.added") => parse_responses_output_item_start(&envelope),
        Some("response.function_call_arguments.delta") => vec![LlmStreamChunk {
            kind: LlmStreamChunkType::ToolCallDelta,
            content: None,
            tool_call: Some(PartialToolCall {
                id: None,
                name: None,
                arguments: envelope["delta"]
                    .as_str()
                    .map(|value| Value::String(value.to_string())),
                index: responses_output_index(&envelope),
            }),
            thinking: None,
            usage: None,
            stop_reason: None,
        }],
        Some("response.function_call_arguments.done") => vec![LlmStreamChunk {
            kind: LlmStreamChunkType::ToolCallEnd,
            content: None,
            tool_call: Some(PartialToolCall {
                id: None,
                name: None,
                arguments: envelope["arguments"]
                    .as_str()
                    .map(|value| Value::String(value.to_string())),
                index: responses_output_index(&envelope),
            }),
            thinking: None,
            usage: None,
            stop_reason: None,
        }],
        Some("response.output_item.done") => parse_responses_output_item_done(&envelope),
        Some("response.completed") => responses_terminal_chunks("stop", &envelope),
        Some("response.incomplete") => responses_terminal_chunks(
            envelope["response"]["incomplete_details"]["reason"]
                .as_str()
                .unwrap_or("incomplete"),
            &envelope,
        ),
        Some("response.failed") => responses_terminal_chunks("error", &envelope),
        _ => parse_responses_usage_event(&envelope),
    }
}

fn parse_responses_output_item_start(envelope: &Value) -> Vec<LlmStreamChunk> {
    let item = &envelope["item"];
    match item["type"].as_str() {
        Some("message") => vec![LlmStreamChunk {
            kind: LlmStreamChunkType::ContentStart,
            content: None,
            tool_call: None,
            thinking: None,
            usage: None,
            stop_reason: None,
        }],
        Some("function_call") => vec![LlmStreamChunk {
            kind: LlmStreamChunkType::ToolCallStart,
            content: None,
            tool_call: Some(PartialToolCall {
                id: item["call_id"]
                    .as_str()
                    .or_else(|| item["id"].as_str())
                    .map(str::to_string),
                name: item["name"].as_str().map(str::to_string),
                arguments: item["arguments"]
                    .as_str()
                    .map(|value| Value::String(value.to_string())),
                index: responses_output_index(envelope),
            }),
            thinking: None,
            usage: None,
            stop_reason: None,
        }],
        _ => Vec::new(),
    }
}

fn parse_responses_output_item_done(envelope: &Value) -> Vec<LlmStreamChunk> {
    let item = &envelope["item"];
    match item["type"].as_str() {
        Some("message") => vec![LlmStreamChunk {
            kind: LlmStreamChunkType::ContentEnd,
            content: None,
            tool_call: None,
            thinking: None,
            usage: None,
            stop_reason: None,
        }],
        Some("function_call") => vec![LlmStreamChunk {
            kind: LlmStreamChunkType::ToolCallEnd,
            content: None,
            tool_call: Some(PartialToolCall {
                id: item["call_id"]
                    .as_str()
                    .or_else(|| item["id"].as_str())
                    .map(str::to_string),
                name: item["name"].as_str().map(str::to_string),
                arguments: item["arguments"]
                    .as_str()
                    .map(|value| Value::String(value.to_string())),
                index: responses_output_index(envelope),
            }),
            thinking: None,
            usage: None,
            stop_reason: None,
        }],
        _ => Vec::new(),
    }
}

fn responses_output_index(envelope: &Value) -> Option<usize> {
    envelope["output_index"]
        .as_u64()
        .and_then(|index| usize::try_from(index).ok())
}

fn responses_terminal_chunk(reason: &str) -> LlmStreamChunk {
    LlmStreamChunk {
        kind: LlmStreamChunkType::ContentEnd,
        content: None,
        tool_call: None,
        thinking: None,
        usage: None,
        stop_reason: Some(reason.to_string()),
    }
}

fn responses_terminal_chunks(reason: &str, envelope: &Value) -> Vec<LlmStreamChunk> {
    let mut chunks = vec![responses_terminal_chunk(reason)];
    chunks.extend(parse_responses_usage_event(envelope));
    chunks
}

fn parse_responses_usage_event(envelope: &Value) -> Vec<LlmStreamChunk> {
    let usage = envelope
        .get("response")
        .and_then(|response| response.get("usage"))
        .or_else(|| envelope.get("usage"));
    let Some(usage) = usage.filter(|usage| !usage.is_null()) else {
        return Vec::new();
    };
    vec![LlmStreamChunk {
        kind: LlmStreamChunkType::Usage,
        content: None,
        tool_call: None,
        thinking: None,
        usage: Some(parse_responses_usage_value(usage)),
        stop_reason: None,
    }]
}

fn parse_responses_usage_value(usage: &Value) -> LlmUsage {
    let cache_read_tokens = usage["input_tokens_details"]["cached_tokens"].as_u64();
    LlmUsage {
        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens,
        cache_write_tokens: None,
        cache_read_included_in_input: cache_read_tokens.is_some(),
    }
}

fn stable_provider_context_index(value: &str) -> usize {
    value.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as usize)
    })
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

        if let Some(content) = openai_stream_text(&delta["content"])
            && !content.is_empty()
        {
            chunks.push(LlmStreamChunk {
                kind: LlmStreamChunkType::ContentDelta,
                content: Some(content),
                tool_call: None,
                thinking: None,
                usage: None,
                stop_reason: None,
            });
        }

        if let Some(reasoning) = delta["reasoning_content"]
            .as_str()
            .or_else(|| delta["reasoning_text"].as_str())
            .or_else(|| delta["reasoning"].as_str())
            && !reasoning.is_empty()
        {
            chunks.push(LlmStreamChunk {
                kind: LlmStreamChunkType::Thinking,
                content: None,
                tool_call: None,
                thinking: Some(reasoning.to_string()),
                usage: None,
                stop_reason: None,
            });
        }

        if let Some(tool_calls) = delta["tool_calls"].as_array() {
            for tc in tool_calls {
                let func = &tc["function"];
                let id = tc["id"].as_str().map(str::to_string);
                let name = func["name"].as_str().map(str::to_string);
                let args = match &func["arguments"] {
                    Value::Null => None,
                    Value::String(arguments) => Some(arguments.clone()),
                    arguments => Some(arguments.to_string()),
                };
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
                        arguments: args.map(Value::String),
                        index,
                    }),
                    thinking: None,
                    usage: None,
                    stop_reason: None,
                });
            }
        }

        if let Some(reason) = choice["finish_reason"]
            .as_str()
            .or_else(|| choice["stop_reason"].as_str())
        {
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

fn openai_stream_text(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    let parts = value.as_array()?;
    let text = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<String>();
    (!text.is_empty()).then_some(text)
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
        Some("message_stop") => vec![LlmStreamChunk {
            kind: LlmStreamChunkType::ContentEnd,
            content: None,
            tool_call: None,
            thinking: None,
            usage: None,
            stop_reason: None,
        }],
        Some("ping") | Some("error") => Vec::new(),
        _ => Vec::new(),
    }
}

fn parse_openai_usage_value(usage: &Value) -> LlmUsage {
    let cache_read_tokens = usage["prompt_tokens_details"]["cached_tokens"].as_u64();
    LlmUsage {
        input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens,
        cache_write_tokens: None,
        cache_read_included_in_input: cache_read_tokens.is_some(),
    }
}

fn parse_anthropic_usage_value(usage: &Value) -> LlmUsage {
    LlmUsage {
        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: usage["cache_read_input_tokens"].as_u64(),
        cache_write_tokens: usage["cache_creation_input_tokens"].as_u64(),
        cache_read_included_in_input: false,
    }
}

#[derive(Clone, Debug, Default)]
pub struct StreamAccumulator {
    content: String,
    thinking: String,
    active_tool_calls: Vec<ActiveToolCall>,
    usage: LlmUsage,
    stop_reason: Option<String>,
    terminal: bool,
    // provider context 是下一轮请求必须原样回放的协议数据。使用 Vec 保留
    // Responses 的 output_index；不能使用 BTreeMap 对 provider id 哈希后排序。
    provider_context: Vec<(usize, ModelProviderContext)>,
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
                    self.content.push_str(text);
                }
            }
            LlmStreamChunkType::ContentEnd => {
                // 捕获 stop_reason（来自 OpenAI finish_reason 或 Anthropic message_delta）
                if let Some(ref reason) = chunk.stop_reason {
                    self.terminal = true;
                    if self.stop_reason.is_none() {
                        self.stop_reason = Some(reason.clone());
                    }
                }
            }
            LlmStreamChunkType::ToolCallStart => {
                if let Some(ref tc) = chunk.tool_call {
                    let initial_arguments = tool_call_argument_fragment(tc.arguments.as_ref());
                    if let Some(index) = tc.index.filter(|idx| *idx < self.active_tool_calls.len())
                    {
                        if let Some(id) = tc.id.as_ref().filter(|id| !id.is_empty()) {
                            self.active_tool_calls[index].id = id.clone();
                        }
                        if let Some(name) = tc.name.as_ref().filter(|name| !name.is_empty()) {
                            self.active_tool_calls[index].name = name.clone();
                        }
                        if let Some(fragment) = initial_arguments {
                            self.active_tool_calls[index]
                                .arguments_buffer
                                .push_str(&fragment);
                        }
                    } else {
                        self.active_tool_calls.push(ActiveToolCall {
                            id: tc.id.clone().unwrap_or_default(),
                            name: tc.name.clone().unwrap_or_default(),
                            arguments_buffer: initial_arguments.unwrap_or_default(),
                        });
                    }
                }
            }
            LlmStreamChunkType::ToolCallDelta => {
                if let Some(ref tc) = chunk.tool_call
                    && let Some(fragment) = tool_call_argument_fragment(tc.arguments.as_ref())
                {
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
            LlmStreamChunkType::ToolCallEnd => {
                if let Some(ref tc) = chunk.tool_call {
                    let target_idx = tc
                        .index
                        .filter(|idx| *idx < self.active_tool_calls.len())
                        .or_else(|| {
                            tc.id.as_deref().and_then(|id| {
                                self.active_tool_calls
                                    .iter()
                                    .position(|active| active.id == id)
                            })
                        })
                        .or_else(|| {
                            if self.active_tool_calls.is_empty() {
                                None
                            } else {
                                Some(self.active_tool_calls.len() - 1)
                            }
                        });
                    let Some(fragment) = tool_call_argument_fragment(tc.arguments.as_ref()) else {
                        return;
                    };
                    if let Some(idx) = target_idx {
                        if let Some(id) = tc.id.as_ref().filter(|id| !id.is_empty()) {
                            self.active_tool_calls[idx].id = id.clone();
                        }
                        if let Some(name) = tc.name.as_ref().filter(|name| !name.is_empty()) {
                            self.active_tool_calls[idx].name = name.clone();
                        }
                        self.active_tool_calls[idx].arguments_buffer = fragment;
                    } else {
                        self.active_tool_calls.push(ActiveToolCall {
                            id: tc.id.clone().unwrap_or_default(),
                            name: tc.name.clone().unwrap_or_default(),
                            arguments_buffer: fragment,
                        });
                    }
                }
            }
            LlmStreamChunkType::Thinking => {
                if let Some(ref text) = chunk.thinking {
                    self.thinking.push_str(text);
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
                    self.usage.cache_read_included_in_input |= u.cache_read_included_in_input;
                }
            }
        }
    }

    pub fn apply_all(&mut self, chunks: &[LlmStreamChunk]) {
        for chunk in chunks {
            self.apply(chunk);
        }
    }

    pub fn apply_provider_context(&mut self, delta: ProviderContextStreamDelta) {
        match delta {
            ProviderContextStreamDelta::Start { index, context } => {
                self.upsert_provider_context(index, context);
            }
            ProviderContextStreamDelta::Append {
                index,
                field,
                value,
            } => {
                let context = self.provider_context_mut(index, || ModelProviderContext {
                    provider: "anthropic".to_string(),
                    kind: "thinking".to_string(),
                    data: serde_json::json!({"type": "thinking"}),
                });
                let object = context
                    .data
                    .as_object_mut()
                    .expect("provider context data is initialized as an object");
                let target = object
                    .entry(field)
                    .or_insert_with(|| Value::String(String::new()));
                if let Value::String(current) = target {
                    current.push_str(&value);
                }
            }
            ProviderContextStreamDelta::ReasoningContentAppend { field, value } => {
                let context = self.provider_context_mut(usize::MAX, || ModelProviderContext {
                    provider: "openai_chat".to_string(),
                    kind: "reasoning".to_string(),
                    data: serde_json::json!({field: ""}),
                });
                let current = context.data[field].as_str().unwrap_or_default().to_string();
                context.data[field] = Value::String(format!("{current}{value}"));
            }
        }
    }

    fn upsert_provider_context(&mut self, index: usize, context: ModelProviderContext) {
        if let Some((_, existing)) = self
            .provider_context
            .iter_mut()
            .find(|(existing_index, _)| *existing_index == index)
        {
            *existing = context;
            return;
        }
        let insert_at = self
            .provider_context
            .iter()
            .position(|(existing_index, _)| *existing_index > index)
            .unwrap_or(self.provider_context.len());
        self.provider_context.insert(insert_at, (index, context));
    }

    fn provider_context_mut(
        &mut self,
        index: usize,
        create: impl FnOnce() -> ModelProviderContext,
    ) -> &mut ModelProviderContext {
        if let Some(position) = self
            .provider_context
            .iter()
            .position(|(existing_index, _)| *existing_index == index)
        {
            return &mut self.provider_context[position].1;
        }
        self.provider_context.push((index, create()));
        &mut self
            .provider_context
            .last_mut()
            .expect("新建的 provider context 必须存在")
            .1
    }

    pub fn finalize(self) -> AdaptedResponse {
        let tool_calls: Vec<ToolCall> = self
            .active_tool_calls
            .into_iter()
            .enumerate()
            .map(|(index, tc)| {
                let id = if tc.id.trim().is_empty() {
                    super::utils::compatible_tool_call_id(index, &tc.name, &tc.arguments_buffer)
                } else {
                    tc.id
                };
                let (arguments, argument_parse_error) = parse_tool_arguments(&tc.arguments_buffer);
                ToolCall {
                    id,
                    name: tc.name,
                    arguments,
                    argument_parse_error,
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
            content: self.content,
            thinking: (!self.thinking.trim().is_empty()).then_some(self.thinking),
            tool_calls,
            usage: self.usage,
            stop_reason,
            raw: None,
            provider_context: self
                .provider_context
                .into_iter()
                .map(|(_, context)| context)
                .collect(),
        }
    }

    pub fn accumulated_content(&self) -> String {
        self.content.clone()
    }

    pub fn accumulated_thinking(&self) -> String {
        self.thinking.clone()
    }

    pub fn pending_tool_call_count(&self) -> usize {
        self.active_tool_calls.len()
    }

    pub fn saw_terminal(&self) -> bool {
        self.terminal
    }
}

fn tool_call_argument_fragment(arguments: Option<&Value>) -> Option<String> {
    arguments.map(|args| match args {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_types::{LlmContentBlock, LlmMessage, LlmMessageContent, LlmMessageParams};
    use crate::protocol::{OpenAiResponsesAdapter, ProviderAdapter};

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

    /// 回归测试：上游 chunked transfer 经常把 `event: foo\n` / `data: bar\n` / `\n`
    /// 拆成多个 HTTP chunk，reqwest 解码后每次 `read()` 可能恰好按行返回。
    /// 历史上 `data_lines` 是 `feed()` 局部变量，会跨调用丢失，导致所有 text_delta
    /// 被静默吞掉，final_content 为空。本测试锁定字段化行为不再回退。
    #[test]
    fn sse_parser_preserves_pending_data_across_feeds_when_terminator_arrives_later() {
        let mut parser = SseLineParser::new();

        // chunk 1 只含 event 行（无 \n\n terminator）
        let e1 = parser.feed("event: content_block_delta\n");
        assert!(e1.is_empty());

        // chunk 2 只含 data 行（仍无 terminator）
        let e2 = parser.feed("data: {\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n");
        assert!(e2.is_empty());

        // chunk 3 才送来空行 terminator —— 此时必须能拼出完整事件
        let e3 = parser.feed("\n");
        assert_eq!(e3.len(), 1, "terminator 单独到达时事件应当被发出");
        assert_eq!(e3[0].event_type.as_deref(), Some("content_block_delta"));
        assert_eq!(
            e3[0].data,
            "{\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}"
        );
    }

    #[test]
    fn sse_parser_handles_multiple_events_in_one_chunk() {
        let mut parser = SseLineParser::new();
        let events = parser.feed("data: {\"a\":1}\n\ndata: {\"b\":2}\n\n");
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn sse_parser_flushes_final_event_without_blank_line() {
        let mut parser = SseLineParser::new();

        assert!(parser.feed("data: [DONE]\n").is_empty());
        let events = parser.finish();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "[DONE]");
    }

    #[test]
    fn sse_parser_does_not_leak_event_type_across_empty_event() {
        let mut parser = SseLineParser::new();

        assert!(parser.feed("event: ping\n\n").is_empty());
        let events = parser.feed("data: {\"ok\":true}\n\n");

        assert_eq!(events.len(), 1);
        assert!(events[0].event_type.is_none());
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
    fn openai_stream_parses_content_part_arrays_from_compatible_providers() {
        let data = r#"{"choices":[{"delta":{"content":[{"type":"text","text":"Hel"},{"type":"text","text":"lo"}]},"finish_reason":null}]}"#;
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
    fn openai_stream_normalizes_compatible_tool_call_without_id() {
        let chunks = parse_openai_stream_data(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"search","arguments":{"query":"Magi"}}}]},"stop_reason":"tool_calls"}]}"#,
        );
        let mut accumulator = StreamAccumulator::new();
        accumulator.apply_all(&chunks);

        let result = accumulator.finalize();

        assert_eq!(result.stop_reason, "tool_calls");
        assert_eq!(result.tool_calls.len(), 1);
        assert!(result.tool_calls[0].id.starts_with("call_compat_0_"));
        assert_eq!(result.tool_calls[0].arguments["query"], "Magi");
    }

    #[test]
    fn openai_stream_preserves_reasoning_field_name() {
        let event = SseEvent {
            event_type: None,
            data: r#"{"choices":[{"delta":{"reasoning_text":"先分析"},"finish_reason":null}]}"#
                .to_string(),
        };
        let mut accumulator = StreamAccumulator::new();
        if let Some(delta) = parse_stream_provider_context(ProviderFamily::OpenAiChat, &event) {
            accumulator.apply_provider_context(delta);
        }
        accumulator.apply_all(&parse_stream_event(ProviderFamily::OpenAiChat, &event));

        let response = accumulator.finalize();

        assert_eq!(response.thinking.as_deref(), Some("先分析"));
        assert_eq!(response.provider_context.len(), 1);
        assert_eq!(response.provider_context[0].provider, "openai_chat");
        assert_eq!(
            response.provider_context[0].data["reasoning_text"],
            "先分析"
        );
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
    fn anthropic_stream_accumulates_signed_thinking_context() {
        let start = SseEvent {
            event_type: Some("content_block_start".to_string()),
            data: r#"{"index":0,"content_block":{"type":"thinking","thinking":""}}"#.to_string(),
        };
        let thinking = SseEvent {
            event_type: Some("content_block_delta".to_string()),
            data: r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"分析"}}"#.to_string(),
        };
        let signature = SseEvent {
            event_type: Some("content_block_delta".to_string()),
            data: r#"{"index":0,"delta":{"type":"signature_delta","signature":"signed"}}"#
                .to_string(),
        };
        let mut accumulator = StreamAccumulator::new();
        for event in [&start, &thinking, &signature] {
            if let Some(delta) = parse_stream_provider_context(ProviderFamily::Anthropic, event) {
                accumulator.apply_provider_context(delta);
            }
            accumulator.apply_all(&parse_stream_event(ProviderFamily::Anthropic, event));
        }

        let response = accumulator.finalize();
        assert_eq!(response.thinking.as_deref(), Some("分析"));
        assert_eq!(response.provider_context.len(), 1);
        assert_eq!(response.provider_context[0].data["thinking"], "分析");
        assert_eq!(response.provider_context[0].data["signature"], "signed");
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
    fn anthropic_stream_ignores_ping_and_marks_message_stop() {
        assert!(parse_anthropic_stream_event(Some("ping"), "{}").is_empty());
        let chunks = parse_anthropic_stream_event(Some("message_stop"), "{}");
        assert_eq!(chunks.len(), 1);
        assert!(matches!(chunks[0].kind, LlmStreamChunkType::ContentEnd));
        assert!(chunks[0].stop_reason.is_none());
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
        assert!(!acc.saw_terminal());
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
        assert!(!acc.saw_terminal());
        let result = acc.finalize();
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].id, "call_1");
        assert_eq!(result.tool_calls[0].name, "search");
        assert_eq!(result.tool_calls[0].arguments["q"], "test");
        assert_eq!(result.stop_reason, "tool_use");
    }

    #[test]
    fn accumulator_marks_tool_call_without_arguments_as_invalid() {
        let mut acc = StreamAccumulator::new();
        acc.apply(&LlmStreamChunk {
            kind: LlmStreamChunkType::ToolCallStart,
            content: None,
            tool_call: Some(PartialToolCall {
                id: Some("call-empty".to_string()),
                name: Some("shell_exec".to_string()),
                arguments: None,
                index: None,
            }),
            thinking: None,
            usage: None,
            stop_reason: None,
        });

        let result = acc.finalize();

        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].raw_arguments.as_deref(), Some(""));
        assert_eq!(
            result.tool_calls[0].argument_parse_error.as_deref(),
            Some("tool arguments are empty")
        );
    }

    #[test]
    fn accumulator_preserves_openai_arguments_on_tool_call_start() {
        let mut parser = SseLineParser::new();
        let mut acc = StreamAccumulator::new();
        let sse_payload = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"shell_exec\",\"arguments\":\"{\\\"command\\\":\\\"pwd\\\",\\\"access_mode\\\":\\\"read_only\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );

        for event in parser.feed(sse_payload) {
            let chunks = parse_stream_event(ProviderFamily::OpenAiChat, &event);
            acc.apply_all(&chunks);
        }

        let result = acc.finalize();
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].id, "call_1");
        assert_eq!(result.tool_calls[0].name, "shell_exec");
        assert_eq!(result.tool_calls[0].arguments["command"], "pwd");
        assert_eq!(result.tool_calls[0].arguments["access_mode"], "read_only");
        assert_eq!(
            result.tool_calls[0].raw_arguments.as_deref(),
            Some(r#"{"command":"pwd","access_mode":"read_only"}"#)
        );
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
                cache_read_included_in_input: true,
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
                cache_read_included_in_input: false,
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

        assert!(acc.saw_terminal());
        let result = acc.finalize();
        assert_eq!(result.content, "Hi there");
        assert!(result.tool_calls.is_empty());
        assert_eq!(result.usage.input_tokens, 10);
        assert_eq!(result.usage.output_tokens, 5);
    }

    #[test]
    fn end_to_end_responses_stream_to_response() {
        let sse_payload = concat!(
            "event: response.output_item.added\ndata: {\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\",\"summary\":[]}}\n\n",
            "event: response.reasoning_summary_text.delta\ndata: {\"delta\":\"先判断\"}\n\n",
            "event: response.output_item.added\ndata: {\"output_index\":1,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"shell_exec\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\ndata: {\"item_id\":\"fc_1\",\"delta\":\"{\\\"command\\\":\\\"pwd\\\"}\"}\n\n",
            "event: response.function_call_arguments.done\ndata: {\"item_id\":\"fc_1\",\"arguments\":\"{\\\"command\\\":\\\"pwd\\\"}\"}\n\n",
            "event: response.output_item.done\ndata: {\"output_index\":1,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"shell_exec\",\"arguments\":\"{\\\"command\\\":\\\"pwd\\\"}\"}}\n\n",
            "event: response.output_item.done\ndata: {\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\",\"encrypted_content\":\"encrypted-context\",\"content\":[{\"type\":\"reasoning_text\",\"text\":\"先判断\"}]}}\n\n",
            "event: response.completed\ndata: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":4}}}\n\n",
        );

        let mut parser = SseLineParser::new();
        let mut accumulator = StreamAccumulator::new();
        for event in parser.feed(sse_payload) {
            if let Some(context) =
                parse_stream_provider_context(ProviderFamily::OpenAiResponses, &event)
            {
                accumulator.apply_provider_context(context);
            }
            accumulator.apply_all(&parse_stream_event(ProviderFamily::OpenAiResponses, &event));
        }

        let result = accumulator.finalize();
        assert_eq!(result.thinking.as_deref(), Some("先判断"));
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].id, "call_1");
        assert_eq!(result.tool_calls[0].arguments["command"], "pwd");
        assert_eq!(result.stop_reason, "stop");
        assert_eq!(result.provider_context.len(), 2);
        assert_eq!(result.provider_context[0].data["id"], "rs_1");
        assert_eq!(
            result.provider_context[0].data["encrypted_content"],
            "encrypted-context"
        );
        assert_eq!(result.provider_context[1].data["id"], "fc_1");
        assert_eq!(result.provider_context[1].data["call_id"], "call_1");
        assert_eq!(result.usage.input_tokens, 10);
        assert_eq!(result.usage.output_tokens, 4);

        let assistant_blocks = result
            .provider_context
            .into_iter()
            .map(|context| LlmContentBlock::ProviderContext { context })
            .chain(std::iter::once(LlmContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "shell_exec".to_string(),
                input: serde_json::json!({"command": "pwd"}),
            }))
            .collect();
        let request = OpenAiResponsesAdapter
            .build_request(
                &LlmMessageParams {
                    messages: vec![
                        LlmMessage {
                            role: "assistant".to_string(),
                            content: LlmMessageContent::Blocks(assistant_blocks),
                        },
                        LlmMessage {
                            role: "user".to_string(),
                            content: LlmMessageContent::Blocks(vec![LlmContentBlock::ToolResult {
                                tool_use_id: "call_1".to_string(),
                                content: "/workspace".to_string(),
                                is_error: false,
                                images: Vec::new(),
                            }]),
                        },
                    ],
                    max_tokens: None,
                    temperature: None,
                    tools: None,
                    stream: Some(true),
                    system_prompt: None,
                    tool_choice: None,
                    reasoning_effort: None,
                },
                "deepseek-v4-flash-0731",
            )
            .expect("request should build");
        let input = request.body["input"].as_array().expect("input array");
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["id"], "rs_1");
        assert_eq!(input[0]["encrypted_content"], "encrypted-context");
        assert_eq!(input[1]["id"], "fc_1");
        assert_eq!(input[1]["call_id"], "call_1");
        assert_eq!(input[2]["type"], "function_call_output");
    }

    #[test]
    fn responses_parallel_tool_deltas_route_by_output_index() {
        let sse_payload = concat!(
            "event: response.output_item.added\ndata: {\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"\"}}\n\n",
            "event: response.output_item.added\ndata: {\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_2\",\"name\":\"shell_exec\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\ndata: {\"output_index\":0,\"delta\":\"{\\\"path\\\":\\\"README.md\\\"}\"}\n\n",
            "event: response.function_call_arguments.delta\ndata: {\"output_index\":1,\"delta\":\"{\\\"command\\\":\\\"pwd\\\"}\"}\n\n",
            "event: response.function_call_arguments.done\ndata: {\"output_index\":0,\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}\n\n",
            "event: response.function_call_arguments.done\ndata: {\"output_index\":1,\"arguments\":\"{\\\"command\\\":\\\"pwd\\\"}\"}\n\n",
            "event: response.completed\ndata: {\"response\":{\"status\":\"completed\"}}\n\n",
        );

        let mut parser = SseLineParser::new();
        let mut accumulator = StreamAccumulator::new();
        for event in parser.feed(sse_payload) {
            accumulator.apply_all(&parse_stream_event(ProviderFamily::OpenAiResponses, &event));
        }

        let result = accumulator.finalize();
        assert_eq!(result.tool_calls.len(), 2);
        assert_eq!(result.tool_calls[0].id, "call_1");
        assert_eq!(result.tool_calls[0].arguments["path"], "README.md");
        assert_eq!(result.tool_calls[1].id, "call_2");
        assert_eq!(result.tool_calls[1].arguments["command"], "pwd");
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
