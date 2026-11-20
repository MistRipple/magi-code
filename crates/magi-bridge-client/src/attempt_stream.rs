//! 一次模型调用尝试的紧凑流记录：把协议层的原始块 `LlmStreamChunk`（带到达时间）归一成 DSH
//! 的 `AssistantStreamRecord`，供会话日志持久化。
//!
//! 运行时约束：
//! - 记录的是协议层原始块，不是给界面用、会合并增量的 `ModelStreamingDelta`；
//! - DSH 的块协议带 `index`，原始块没有，这里按块首次出现的顺序分配（与 DSH 的适配器做同样的归一）；
//! - 相邻同类增量合并为一条记录，但保留每个增量的边界与相对时间（`dt`）；
//! - 使用 `replace` 语义的提供方没有增量流，调用方应记为不完整，不要喂给这里。

use crate::llm_types::{LlmStreamChunk, LlmStreamChunkType};
use crate::types::{BridgeClientError, BridgeErrorLayer, model_invocation_error_is_cancelled};
use serde_json::{Value, json};

/// 单条已展开的实时助手流块（DSH `TimedStreamChunk`），与持久化紧凑流经
/// `expandAssistantStream` 展开后的每一项一一对应。
#[derive(Clone, Debug, PartialEq)]
pub struct LiveStreamChunk {
    pub time: u64,
    pub chunk: Value,
}

/// 失败或取消的模型调用尝试在 DSH `finish` 块里携带的结构化失败事实（`LlmFailure`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptStreamFailure {
    pub message: String,
    pub code: String,
    pub status: Option<u16>,
}

impl AttemptStreamFailure {
    /// 从桥接层错误归一为 DSH `LlmFailure` 与是否属于取消（`aborted`）。
    pub fn from_bridge_error(error: &BridgeClientError) -> (Self, bool) {
        let raw_message = error.to_string();
        let message = if raw_message.trim().is_empty() {
            "model invocation failed".to_string()
        } else {
            raw_message
        };
        if model_invocation_error_is_cancelled(error) {
            return (
                Self {
                    message,
                    code: "CANCELLED".to_string(),
                    status: None,
                },
                true,
            );
        }
        if let Some(status) = error
            .http_status()
            .filter(|status| (100..=599).contains(status))
        {
            return (
                Self {
                    message,
                    code: format!("provider_http_{status}"),
                    status: Some(status),
                },
                false,
            );
        }
        let code = match error.layer() {
            Some(BridgeErrorLayer::Transport) => "MODEL_TRANSPORT_FAILED",
            Some(BridgeErrorLayer::Protocol) => "MODEL_PROTOCOL_ERROR",
            Some(BridgeErrorLayer::RemoteBusiness) => "MODEL_PROVIDER_REJECTED",
            None => "MODEL_CALL_FAILED",
        };
        (
            Self {
                message,
                code: code.to_string(),
                status: None,
            },
            false,
        )
    }

    fn to_json(&self) -> Value {
        let message = if self.message.trim().is_empty() {
            "model invocation failed"
        } else {
            self.message.as_str()
        };
        let code = if self.code.trim().is_empty() {
            "MODEL_CALL_FAILED"
        } else {
            self.code.as_str()
        };
        let mut value = json!({
            "message": message,
            "code": code,
        });
        if let Some(status) = self.status.filter(|status| (100..=599).contains(status)) {
            value["status"] = json!(status);
        }
        value
    }
}

/// 上游给出的终止原因归一后的 DSH `finish` 种类。
fn finish_kind(stop_reason: Option<&str>, has_tool_calls: bool) -> Option<&'static str> {
    match stop_reason {
        Some("stop" | "end_turn" | "completed" | "stop_sequence") => Some("stop"),
        Some("tool_calls" | "tool_use" | "function_call") => Some("tool-calls"),
        Some("length" | "max_tokens" | "max_output_tokens") => Some("max-tokens"),
        // 没有明确原因：有工具调用就是工具调用结束，否则无法判定，不编造。
        None if has_tool_calls => Some("tool-calls"),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Reasoning,
    ToolCall,
}

struct Block {
    index: usize,
    kind: BlockKind,
    text: String,
    id: String,
    name: String,
    /// 原始块里的工具索引（并行调用用它路由），没有则为 `None`。
    route: Option<usize>,
}

/// 增量地把一次尝试的原始块压成紧凑记录。
#[derive(Default)]
pub struct AttemptStreamRecorder {
    records: Vec<Value>,
    blocks: Vec<Block>,
    text_block: Option<usize>,
    reasoning_block: Option<usize>,
    /// 上一条可合并的增量记录：记录下标与最后一次时间。
    last_run: Option<(usize, u64)>,
    last_tool: Option<usize>,
}

impl AttemptStreamRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    fn raw(&mut self, time: u64, chunk: Value, live: Option<&mut Vec<LiveStreamChunk>>) {
        self.last_run = None;
        if let Some(live) = live {
            live.push(LiveStreamChunk {
                time,
                chunk: chunk.clone(),
            });
        }
        self.records
            .push(json!({"type": "chunk", "time": time, "chunk": chunk}));
    }

    fn open(
        &mut self,
        time: u64,
        kind: BlockKind,
        id: &str,
        name: &str,
        route: Option<usize>,
        live: Option<&mut Vec<LiveStreamChunk>>,
    ) -> usize {
        let index = self.blocks.len();
        self.blocks.push(Block {
            index,
            kind,
            text: String::new(),
            id: id.to_string(),
            name: name.to_string(),
            route,
        });
        let block_type = match kind {
            BlockKind::Text => "text",
            BlockKind::Reasoning => "reasoning",
            BlockKind::ToolCall => "tool-call",
        };
        self.raw(
            time,
            json!({"type": "block-start", "index": index, "blockType": block_type}),
            live,
        );
        index
    }

    /// 追加一条 text/reasoning 增量；同一块相邻增量合并进同一条记录。
    fn push_text(
        &mut self,
        time: u64,
        kind: BlockKind,
        block: usize,
        text: &str,
        live: Option<&mut Vec<LiveStreamChunk>>,
    ) {
        if let Some(live) = live {
            let chunk_type = if kind == BlockKind::Text {
                "text-delta"
            } else {
                "reasoning-delta"
            };
            live.push(LiveStreamChunk {
                time,
                chunk: json!({
                    "type": chunk_type,
                    "index": block,
                    "text": text,
                }),
            });
        }
        let record_type = if kind == BlockKind::Text {
            "text-chunks"
        } else {
            "reasoning-chunks"
        };
        if let Some((position, last_time)) = self.last_run
            && time >= last_time
            && self.records[position]["type"] == record_type
            && self.records[position]["index"] == block
        {
            let record = &mut self.records[position];
            record["dt"]
                .as_array_mut()
                .expect("dt array")
                .push(json!(time - last_time));
            record["texts"]
                .as_array_mut()
                .expect("texts array")
                .push(json!(text));
            self.last_run = Some((position, time));
            return;
        }
        self.records.push(json!({
            "type": record_type, "time0": time, "index": block, "dt": [], "texts": [text],
        }));
        self.last_run = Some((self.records.len() - 1, time));
    }

    fn push_tool_delta(
        &mut self,
        time: u64,
        block: usize,
        with_name: bool,
        fragment: &str,
        decode_name: &dyn Fn(&str) -> String,
        mut live: Option<&mut Vec<LiveStreamChunk>>,
    ) {
        let (id, name) = {
            let block = &self.blocks[block];
            (block.id.clone(), block.name.clone())
        };
        // 没有 id 或名字为空的增量不合并，按原样记成原始块（与 DSH 的合并规则一致）。
        if id.is_empty() || (with_name && name.is_empty()) {
            let mut chunk = json!({
                "type": "tool-call-delta", "index": block, "id": id, "argumentsDelta": fragment,
            });
            if with_name {
                chunk["name"] = json!(name);
            }
            if let Some(live_chunks) = live.as_deref_mut() {
                let mut live_chunk = chunk.clone();
                if with_name {
                    live_chunk["name"] = json!(decode_name(&name));
                }
                live_chunks.push(LiveStreamChunk {
                    time,
                    chunk: live_chunk,
                });
            }
            self.raw(time, chunk, None);
            return;
        }
        if let Some(live_chunks) = live {
            let mut live_chunk = json!({
                "type": "tool-call-delta",
                "index": block,
                "id": id,
                "argumentsDelta": fragment,
            });
            if with_name {
                live_chunk["name"] = json!(decode_name(&name));
            }
            live_chunks.push(LiveStreamChunk {
                time,
                chunk: live_chunk,
            });
        }
        if let Some((position, last_time)) = self.last_run
            && time >= last_time
            && !with_name
            && self.records[position]["type"] == "tool-call-chunks"
            && self.records[position]["index"] == block
            && self.records[position]["id"] == id
            && self.records[position].get("name").is_none()
        {
            let record = &mut self.records[position];
            record["dt"]
                .as_array_mut()
                .expect("dt array")
                .push(json!(time - last_time));
            record["args"]
                .as_array_mut()
                .expect("args array")
                .push(json!(fragment));
            self.last_run = Some((position, time));
            return;
        }
        let mut record = json!({
            "type": "tool-call-chunks", "time0": time, "index": block, "dt": [],
            "id": id, "args": [fragment],
        });
        if with_name {
            record["name"] = json!(name);
        }
        self.records.push(record);
        self.last_run = Some((self.records.len() - 1, time));
    }

    fn tool_block(&self, route: Option<usize>) -> Option<usize> {
        route
            .and_then(|route| {
                self.blocks.iter().position(|block| {
                    block.kind == BlockKind::ToolCall && block.route == Some(route)
                })
            })
            .or(self.last_tool)
    }

    /// 记录一个带到达时间（毫秒）的原始块。
    pub fn push(&mut self, time: u64, chunk: &LlmStreamChunk) {
        self.push_inner(time, chunk, &|name| name.to_string(), None);
    }

    /// 同 [`push`](Self::push)，同时同步返回这次推进产生的所有展开实时块（工具名已实时还原为规范名）。
    pub fn push_emitting(
        &mut self,
        time: u64,
        chunk: &LlmStreamChunk,
        decode_name: &dyn Fn(&str) -> String,
    ) -> Vec<LiveStreamChunk> {
        let mut live = Vec::new();
        self.push_inner(time, chunk, decode_name, Some(&mut live));
        live
    }

    fn push_inner(
        &mut self,
        time: u64,
        chunk: &LlmStreamChunk,
        decode_name: &dyn Fn(&str) -> String,
        mut live: Option<&mut Vec<LiveStreamChunk>>,
    ) {
        match chunk.kind {
            LlmStreamChunkType::ContentStart | LlmStreamChunkType::ContentDelta => {
                let Some(text) = chunk.content.as_deref().filter(|text| !text.is_empty()) else {
                    return;
                };
                let block = match self.text_block {
                    Some(block) => block,
                    None => {
                        let block =
                            self.open(time, BlockKind::Text, "", "", None, live.as_deref_mut());
                        self.text_block = Some(block);
                        block
                    }
                };
                self.blocks[block].text.push_str(text);
                self.push_text(time, BlockKind::Text, block, text, live);
            }
            LlmStreamChunkType::Thinking => {
                let Some(text) = chunk.thinking.as_deref().filter(|text| !text.is_empty()) else {
                    return;
                };
                let block = match self.reasoning_block {
                    Some(block) => block,
                    None => {
                        let block = self.open(
                            time,
                            BlockKind::Reasoning,
                            "",
                            "",
                            None,
                            live.as_deref_mut(),
                        );
                        self.reasoning_block = Some(block);
                        block
                    }
                };
                self.blocks[block].text.push_str(text);
                self.push_text(time, BlockKind::Reasoning, block, text, live);
            }
            LlmStreamChunkType::ToolCallStart => {
                let Some(call) = chunk.tool_call.as_ref() else {
                    return;
                };
                let existing = call.index.and_then(|route| {
                    self.tool_block(Some(route))
                        .filter(|block| self.blocks[*block].route == Some(route))
                });
                let block = match existing {
                    Some(block) => {
                        if let Some(id) = call.id.as_deref().filter(|id| !id.is_empty()) {
                            self.blocks[block].id = id.to_string();
                        }
                        if let Some(name) = call.name.as_deref().filter(|name| !name.is_empty()) {
                            self.blocks[block].name = name.to_string();
                        }
                        block
                    }
                    None => self.open(
                        time,
                        BlockKind::ToolCall,
                        call.id.as_deref().unwrap_or_default(),
                        call.name.as_deref().unwrap_or_default(),
                        call.index,
                        live.as_deref_mut(),
                    ),
                };
                self.last_tool = Some(block);
                let fragment = fragment(call.arguments.as_ref());
                self.blocks[block]
                    .text
                    .push_str(fragment.as_deref().unwrap_or_default());
                // 首个增量带名字（DSH 约定名字只在第一个增量出现）。
                self.push_tool_delta(
                    time,
                    block,
                    true,
                    fragment.as_deref().unwrap_or_default(),
                    decode_name,
                    live,
                );
            }
            LlmStreamChunkType::ToolCallDelta => {
                let Some(call) = chunk.tool_call.as_ref() else {
                    return;
                };
                let Some(fragment) = fragment(call.arguments.as_ref()) else {
                    return;
                };
                let Some(block) = self.tool_block(call.index) else {
                    return;
                };
                self.last_tool = Some(block);
                self.blocks[block].text.push_str(&fragment);
                self.push_tool_delta(time, block, false, &fragment, decode_name, live);
            }
            LlmStreamChunkType::ToolCallEnd => {
                let Some(call) = chunk.tool_call.as_ref() else {
                    return;
                };
                let Some(block) = self.tool_block(call.index).or_else(|| {
                    call.id.as_deref().and_then(|id| {
                        self.blocks
                            .iter()
                            .position(|block| block.kind == BlockKind::ToolCall && block.id == id)
                    })
                }) else {
                    return;
                };
                if let Some(id) = call.id.as_deref().filter(|id| !id.is_empty()) {
                    self.blocks[block].id = id.to_string();
                }
                if let Some(name) = call.name.as_deref().filter(|name| !name.is_empty()) {
                    self.blocks[block].name = name.to_string();
                }
                // 终态参数是权威全文：它与已累积的增量不同时以它为准（与流累积器一致）。
                if let Some(full) = fragment(call.arguments.as_ref()) {
                    self.blocks[block].text = full;
                }
            }
            // 用量在回执里，终止原因在结束时给出，这两类不是流内容。
            LlmStreamChunkType::ContentEnd | LlmStreamChunkType::Usage => {}
        }
    }

    /// 成功结束：关闭所有块（`block-end` 带装配后的块），再给出 `finish`。
    /// 终止原因无法归一时返回 `None`——调用方据此记为不完整，不编造 `finish`。
    pub fn finish(self, time: u64, stop_reason: Option<&str>) -> Option<Vec<Value>> {
        self.finish_with(time, stop_reason, &|name| name.to_string())
    }

    /// 同 [`finish`](Self::finish)，并把工具调用名从线上名字还原成规范名（线上名字经过编码，
    /// 最终消息里的工具名是规范名，流记录必须与之一致）。
    pub fn finish_with(
        self,
        time: u64,
        stop_reason: Option<&str>,
        decode_name: &dyn Fn(&str) -> String,
    ) -> Option<Vec<Value>> {
        self.finish_with_emitting(time, stop_reason, decode_name)
            .map(|(records, _)| records)
    }

    /// 同 [`finish_with`](Self::finish_with)，同时返回收尾阶段产出的 `block-end` 与 `finish` 实时块。
    pub fn finish_with_emitting(
        mut self,
        time: u64,
        stop_reason: Option<&str>,
        decode_name: &dyn Fn(&str) -> String,
    ) -> Option<(Vec<Value>, Vec<LiveStreamChunk>)> {
        let has_tool_calls = self
            .blocks
            .iter()
            .any(|block| block.kind == BlockKind::ToolCall);
        let reason = finish_kind(stop_reason, has_tool_calls)?;
        let mut live = Vec::new();
        let blocks = std::mem::take(&mut self.blocks);
        for block in blocks {
            let assembled = match block.kind {
                BlockKind::Text => json!({"type": "text", "text": block.text}),
                BlockKind::Reasoning => json!({"type": "reasoning", "text": block.text}),
                BlockKind::ToolCall => json!({
                    "type": "tool-call",
                    "id": block.id,
                    "name": block.name,
                    "arguments": block.text,
                }),
            };
            let live_assembled = match block.kind {
                BlockKind::ToolCall => json!({
                    "type": "tool-call",
                    "id": block.id,
                    "name": decode_name(&block.name),
                    "arguments": block.text,
                }),
                _ => assembled.clone(),
            };
            live.push(LiveStreamChunk {
                time,
                chunk: json!({"type": "block-end", "index": block.index, "block": live_assembled}),
            });
            self.raw(
                time,
                json!({"type": "block-end", "index": block.index, "block": assembled}),
                None,
            );
        }
        self.raw(
            time,
            json!({"type": "finish", "reason": {"kind": reason}}),
            Some(&mut live),
        );
        self.decode_tool_names(decode_name);
        Some((self.records, live))
    }

    /// 失败或取消结束：未关闭的块不写 `block-end`，直接追加带失败事实的 `finish` 块。
    pub fn fail(self, time: u64, failure: AttemptStreamFailure, aborted: bool) -> Vec<Value> {
        self.fail_with(time, failure, aborted, &|name| name.to_string())
    }

    /// 同 [`fail`](Self::fail)，并把已记录的工具调用名从线上名字还原成规范名。
    pub fn fail_with(
        self,
        time: u64,
        failure: AttemptStreamFailure,
        aborted: bool,
        decode_name: &dyn Fn(&str) -> String,
    ) -> Vec<Value> {
        self.fail_with_emitting(time, failure, aborted, decode_name)
            .0
    }

    /// 同 [`fail_with`](Self::fail_with)，同时返回末尾 `finish` 实时块。
    pub fn fail_with_emitting(
        mut self,
        time: u64,
        failure: AttemptStreamFailure,
        aborted: bool,
        decode_name: &dyn Fn(&str) -> String,
    ) -> (Vec<Value>, Vec<LiveStreamChunk>) {
        let mut live = Vec::new();
        let kind = if aborted { "aborted" } else { "error" };
        self.raw(
            time,
            json!({
                "type": "finish",
                "reason": {
                    "kind": kind,
                    "failure": failure.to_json(),
                },
            }),
            Some(&mut live),
        );
        self.decode_tool_names(decode_name);
        (self.records, live)
    }

    fn decode_tool_names(&mut self, decode_name: &dyn Fn(&str) -> String) {
        for record in &mut self.records {
            let is_tool_block_end = record["chunk"]["type"] == "block-end"
                && record["chunk"]["block"]["type"] == "tool-call";
            let slot = match record["type"].as_str() {
                Some("tool-call-chunks") => record.get_mut("name"),
                Some("chunk") if record["chunk"]["type"] == "tool-call-delta" => {
                    record["chunk"].get_mut("name")
                }
                Some("chunk") if is_tool_block_end => record["chunk"]["block"].get_mut("name"),
                _ => None,
            };
            if let Some(slot) = slot
                && let Some(wire) = slot.as_str()
            {
                *slot = json!(decode_name(wire));
            }
        }
    }
}

fn fragment(arguments: Option<&Value>) -> Option<String> {
    arguments.map(|value| match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_types::PartialToolCall;

    fn chunk(kind: LlmStreamChunkType) -> LlmStreamChunk {
        LlmStreamChunk {
            kind,
            content: None,
            tool_call: None,
            thinking: None,
            usage: None,
            stop_reason: None,
        }
    }
    fn text(kind: LlmStreamChunkType, value: &str) -> LlmStreamChunk {
        LlmStreamChunk {
            content: Some(value.into()),
            ..chunk(kind)
        }
    }
    fn thinking(value: &str) -> LlmStreamChunk {
        LlmStreamChunk {
            thinking: Some(value.into()),
            ..chunk(LlmStreamChunkType::Thinking)
        }
    }
    fn call(
        kind: LlmStreamChunkType,
        id: Option<&str>,
        name: Option<&str>,
        arguments: Option<&str>,
        index: Option<usize>,
    ) -> LlmStreamChunk {
        LlmStreamChunk {
            tool_call: Some(PartialToolCall {
                id: id.map(Into::into),
                name: name.map(Into::into),
                arguments: arguments.map(|value| Value::String(value.into())),
                index,
            }),
            ..chunk(kind)
        }
    }
    fn types(records: &[Value]) -> Vec<String> {
        records
            .iter()
            .map(|record| {
                let kind = record["type"].as_str().unwrap();
                if kind == "chunk" {
                    format!("chunk:{}", record["chunk"]["type"].as_str().unwrap())
                } else {
                    kind.to_string()
                }
            })
            .collect()
    }

    #[test]
    fn adjacent_deltas_merge_but_keep_their_boundaries_and_relative_times() {
        let mut recorder = AttemptStreamRecorder::new();
        recorder.push(100, &text(LlmStreamChunkType::ContentStart, "你"));
        recorder.push(130, &text(LlmStreamChunkType::ContentDelta, "好"));
        recorder.push(190, &text(LlmStreamChunkType::ContentDelta, "！"));
        let records = recorder.finish(200, Some("stop")).unwrap();
        assert_eq!(
            types(&records),
            [
                "chunk:block-start",
                "text-chunks",
                "chunk:block-end",
                "chunk:finish"
            ]
        );
        assert_eq!(records[1]["time0"], 100);
        assert_eq!(records[1]["dt"], json!([30, 60]));
        assert_eq!(records[1]["texts"], json!(["你", "好", "！"]));
        assert_eq!(
            records[2]["chunk"]["block"],
            json!({"type": "text", "text": "你好！"})
        );
        assert_eq!(records[3]["chunk"]["reason"], json!({"kind": "stop"}));
    }

    #[test]
    fn reasoning_then_text_get_their_own_blocks_in_arrival_order() {
        let mut recorder = AttemptStreamRecorder::new();
        recorder.push(1, &thinking("想"));
        recorder.push(2, &thinking("一想"));
        recorder.push(3, &text(LlmStreamChunkType::ContentDelta, "答"));
        let records = recorder.finish(4, Some("end_turn")).unwrap();
        assert_eq!(
            types(&records),
            [
                "chunk:block-start",
                "reasoning-chunks",
                "chunk:block-start",
                "text-chunks",
                "chunk:block-end",
                "chunk:block-end",
                "chunk:finish"
            ]
        );
        assert_eq!(records[0]["chunk"]["blockType"], "reasoning");
        assert_eq!(records[2]["chunk"]["blockType"], "text");
        assert_eq!(records[2]["chunk"]["index"], 1);
        assert_eq!(
            records[4]["chunk"]["block"],
            json!({"type": "reasoning", "text": "想一想"})
        );
    }

    #[test]
    fn parallel_tool_calls_route_by_index_and_assemble_raw_arguments() {
        let mut recorder = AttemptStreamRecorder::new();
        recorder.push(
            1,
            &call(
                LlmStreamChunkType::ToolCallStart,
                Some("c1"),
                Some("shell_exec"),
                Some(""),
                Some(0),
            ),
        );
        recorder.push(
            2,
            &call(
                LlmStreamChunkType::ToolCallStart,
                Some("c2"),
                Some("read_file"),
                Some(""),
                Some(1),
            ),
        );
        recorder.push(
            3,
            &call(
                LlmStreamChunkType::ToolCallDelta,
                None,
                None,
                Some("{\"command\":"),
                Some(0),
            ),
        );
        recorder.push(
            4,
            &call(
                LlmStreamChunkType::ToolCallDelta,
                None,
                None,
                Some("{\"path\":\"a\"}"),
                Some(1),
            ),
        );
        recorder.push(
            5,
            &call(
                LlmStreamChunkType::ToolCallDelta,
                None,
                None,
                Some("\"ls\"}"),
                Some(0),
            ),
        );
        let records = recorder.finish(6, Some("tool_calls")).unwrap();
        let ends: Vec<_> = records
            .iter()
            .filter(|record| record["chunk"]["type"] == "block-end")
            .map(|record| record["chunk"]["block"].clone())
            .collect();
        assert_eq!(
            ends,
            [
                json!({"type": "tool-call", "id": "c1", "name": "shell_exec", "arguments": "{\"command\":\"ls\"}"}),
                json!({"type": "tool-call", "id": "c2", "name": "read_file", "arguments": "{\"path\":\"a\"}"}),
            ]
        );
        assert_eq!(
            records.last().unwrap()["chunk"]["reason"],
            json!({"kind": "tool-calls"})
        );
    }

    #[test]
    fn a_final_tool_call_end_replaces_the_accumulated_arguments() {
        let mut recorder = AttemptStreamRecorder::new();
        recorder.push(
            1,
            &call(
                LlmStreamChunkType::ToolCallStart,
                Some("c1"),
                Some("t"),
                Some("{\"a\""),
                None,
            ),
        );
        recorder.push(
            2,
            &call(
                LlmStreamChunkType::ToolCallEnd,
                Some("c1"),
                None,
                Some("{\"a\":1}"),
                None,
            ),
        );
        let records = recorder.finish(3, None).unwrap();
        let end = records
            .iter()
            .find(|record| record["chunk"]["type"] == "block-end")
            .unwrap();
        assert_eq!(end["chunk"]["block"]["arguments"], "{\"a\":1}");
        // 没有终止原因但有工具调用：按工具调用结束。
        assert_eq!(
            records.last().unwrap()["chunk"]["reason"],
            json!({"kind": "tool-calls"})
        );
    }

    #[test]
    fn an_unknown_stop_reason_is_not_invented_into_a_finish() {
        let mut recorder = AttemptStreamRecorder::new();
        recorder.push(1, &text(LlmStreamChunkType::ContentDelta, "x"));
        assert!(recorder.finish(2, Some("weird_reason")).is_none());
        let mut recorder = AttemptStreamRecorder::new();
        recorder.push(1, &text(LlmStreamChunkType::ContentDelta, "x"));
        assert!(recorder.finish(2, None).is_none());
    }

    #[test]
    fn wire_tool_names_are_decoded_to_the_canonical_names_everywhere() {
        let mut recorder = AttemptStreamRecorder::new();
        recorder.push(
            1,
            &call(
                LlmStreamChunkType::ToolCallStart,
                Some("c1"),
                Some("wire_shell"),
                Some("{}"),
                None,
            ),
        );
        let records = recorder
            .finish_with(2, Some("tool_calls"), &|name| {
                if name == "wire_shell" {
                    "shell_exec".into()
                } else {
                    name.into()
                }
            })
            .unwrap();
        let rendered = serde_json::to_string(&records).unwrap();
        assert!(!rendered.contains("wire_shell"), "{rendered}");
        let end = records
            .iter()
            .find(|record| record["chunk"]["type"] == "block-end")
            .unwrap();
        assert_eq!(end["chunk"]["block"]["name"], "shell_exec");
        assert_eq!(records[1]["name"], "shell_exec");
    }

    #[test]
    fn empty_frames_and_non_content_chunks_record_nothing() {
        let mut recorder = AttemptStreamRecorder::new();
        recorder.push(1, &text(LlmStreamChunkType::ContentDelta, ""));
        recorder.push(2, &chunk(LlmStreamChunkType::Usage));
        recorder.push(3, &chunk(LlmStreamChunkType::ContentEnd));
        let records = recorder.finish(4, Some("stop")).unwrap();
        assert_eq!(types(&records), ["chunk:finish"]);
    }

    #[test]
    fn time_going_backwards_starts_a_new_run_instead_of_a_negative_gap() {
        let mut recorder = AttemptStreamRecorder::new();
        recorder.push(50, &text(LlmStreamChunkType::ContentDelta, "a"));
        recorder.push(40, &text(LlmStreamChunkType::ContentDelta, "b"));
        let records = recorder.finish(60, Some("stop")).unwrap();
        let runs: Vec<_> = records
            .iter()
            .filter(|record| record["type"] == "text-chunks")
            .collect();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[1]["time0"], 40);
    }

    #[test]
    fn failed_and_aborted_attempts_leave_open_blocks_unclosed_and_decode_tool_names() {
        let mut recorder = AttemptStreamRecorder::new();
        recorder.push(1000, &thinking("先查"));
        recorder.push(1015, &text(LlmStreamChunkType::ContentDelta, "正在"));
        recorder.push(
            1030,
            &call(
                LlmStreamChunkType::ToolCallStart,
                Some("c1"),
                Some("wire_shell"),
                Some("{\"a\":"),
                Some(0),
            ),
        );
        let failure = AttemptStreamFailure {
            message: "provider rejected request: rate limit".into(),
            code: "provider_http_429".into(),
            status: Some(429),
        };
        let records = recorder.fail_with(1050, failure, false, &|name| {
            if name == "wire_shell" {
                "shell_exec".into()
            } else {
                name.into()
            }
        });
        assert_eq!(
            types(&records),
            [
                "chunk:block-start",
                "reasoning-chunks",
                "chunk:block-start",
                "text-chunks",
                "chunk:block-start",
                "tool-call-chunks",
                "chunk:finish",
            ]
        );
        assert!(
            records
                .iter()
                .all(|record| record["chunk"]["type"] != "block-end"),
            "failed attempt must not emit block-end for open blocks"
        );
        assert_eq!(records[5]["name"], "shell_exec");
        assert_eq!(
            records.last().unwrap()["chunk"],
            json!({
                "type": "finish",
                "reason": {
                    "kind": "error",
                    "failure": {
                        "message": "provider rejected request: rate limit",
                        "code": "provider_http_429",
                        "status": 429,
                    }
                }
            })
        );

        let mut aborted_recorder = AttemptStreamRecorder::new();
        aborted_recorder.push(2000, &text(LlmStreamChunkType::ContentDelta, "半截"));
        let aborted = aborted_recorder.fail(
            2020,
            AttemptStreamFailure {
                message: "model invocation cancelled".into(),
                code: "CANCELLED".into(),
                status: None,
            },
            true,
        );
        assert_eq!(
            types(&aborted),
            ["chunk:block-start", "text-chunks", "chunk:finish"]
        );
        assert_eq!(
            aborted.last().unwrap()["chunk"],
            json!({
                "type": "finish",
                "reason": {
                    "kind": "aborted",
                    "failure": {
                        "message": "model invocation cancelled",
                        "code": "CANCELLED",
                    }
                }
            })
        );
    }

    #[test]
    fn bridge_errors_map_to_stable_failure_codes_and_aborted_flag() {
        let (cancelled, aborted) =
            AttemptStreamFailure::from_bridge_error(&crate::model_invocation_cancelled_error());
        assert!(aborted);
        assert_eq!(cancelled.code, "CANCELLED");
        assert_eq!(cancelled.status, None);

        let (http, aborted) =
            AttemptStreamFailure::from_bridge_error(&BridgeClientError::HttpStatusFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32006),
                http_status: 503,
                message: "service unavailable".into(),
                retry_after: None,
            });
        assert!(!aborted);
        assert_eq!(http.code, "provider_http_503");
        assert_eq!(http.status, Some(503));

        let (transport, aborted) =
            AttemptStreamFailure::from_bridge_error(&BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Transport,
                code: Some(-32005),
                message: "broken pipe".into(),
            });
        assert!(!aborted);
        assert_eq!(transport.code, "MODEL_TRANSPORT_FAILED");
        assert_eq!(transport.status, None);

        let (protocol, _) =
            AttemptStreamFailure::from_bridge_error(&BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Protocol,
                code: None,
                message: "bad frame".into(),
            });
        assert_eq!(protocol.code, "MODEL_PROTOCOL_ERROR");

        let (business, _) =
            AttemptStreamFailure::from_bridge_error(&BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32006),
                message: "rejected".into(),
            });
        assert_eq!(business.code, "MODEL_PROVIDER_REJECTED");
    }

    #[test]
    fn failed_and_aborted_fixtures_match_recorder_output_exactly() {
        let mut failed = AttemptStreamRecorder::new();
        failed.push(1000, &thinking("想"));
        failed.push(1010, &thinking("查"));
        failed.push(1020, &text(LlmStreamChunkType::ContentDelta, "正在"));
        failed.push(
            1030,
            &call(
                LlmStreamChunkType::ToolCallStart,
                Some("c1"),
                Some("shell_exec"),
                Some("{\"a\":"),
                Some(0),
            ),
        );
        let failed_records = failed.fail(
            1050,
            AttemptStreamFailure {
                message: "provider rejected request: service unavailable".into(),
                code: "provider_http_503".into(),
                status: Some(503),
            },
            false,
        );
        let failed_fixture: Vec<Value> = serde_json::from_str(include_str!(
            "../../magi-extension-host/runtime/dsh/test/fixtures/attempt-stream-failed.json"
        ))
        .expect("failed fixture json");
        assert_eq!(failed_records, failed_fixture);

        let mut aborted = AttemptStreamRecorder::new();
        aborted.push(2000, &text(LlmStreamChunkType::ContentDelta, "半"));
        aborted.push(2015, &text(LlmStreamChunkType::ContentDelta, "截"));
        let aborted_records = aborted.fail(
            2030,
            AttemptStreamFailure {
                message: "model invocation cancelled".into(),
                code: "CANCELLED".into(),
                status: None,
            },
            true,
        );
        let aborted_fixture: Vec<Value> = serde_json::from_str(include_str!(
            "../../magi-extension-host/runtime/dsh/test/fixtures/attempt-stream-aborted.json"
        ))
        .expect("aborted fixture json");
        assert_eq!(aborted_records, aborted_fixture);
    }

    fn expand_compact(records: &[Value]) -> Vec<LiveStreamChunk> {
        let mut out = Vec::new();
        for record in records {
            match record["type"].as_str() {
                Some("chunk") => out.push(LiveStreamChunk {
                    time: record["time"].as_u64().expect("time"),
                    chunk: record["chunk"].clone(),
                }),
                Some("text-chunks" | "reasoning-chunks") => {
                    let kind = if record["type"] == "text-chunks" {
                        "text-delta"
                    } else {
                        "reasoning-delta"
                    };
                    let mut time = record["time0"].as_u64().expect("time0");
                    let index = record["index"].as_u64().expect("index");
                    let dts = record["dt"].as_array().expect("dt");
                    let texts = record["texts"].as_array().expect("texts");
                    for (i, text) in texts.iter().enumerate() {
                        if i > 0 {
                            time += dts[i - 1].as_u64().expect("dt[i]");
                        }
                        out.push(LiveStreamChunk {
                            time,
                            chunk: json!({ "type": kind, "index": index, "text": text }),
                        });
                    }
                }
                Some("tool-call-chunks") => {
                    let mut time = record["time0"].as_u64().expect("time0");
                    let index = record["index"].as_u64().expect("index");
                    let id = record["id"].as_str().expect("id");
                    let name = record.get("name").and_then(Value::as_str);
                    let dts = record["dt"].as_array().expect("dt");
                    let args = record["args"].as_array().expect("args");
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            time += dts[i - 1].as_u64().expect("dt[i]");
                        }
                        let mut chunk = json!({
                            "type": "tool-call-delta",
                            "index": index,
                            "id": id,
                            "argumentsDelta": arg,
                        });
                        if i == 0
                            && let Some(name) = name
                        {
                            chunk["name"] = json!(name);
                        }
                        out.push(LiveStreamChunk { time, chunk });
                    }
                }
                other => panic!("unexpected record type {other:?}"),
            }
        }
        out
    }

    #[test]
    fn live_stream_chunks_match_expanded_compact_records_exactly() {
        let decode = |wire: &str| wire.replace("wire__", "plugin/");
        let mut recorder = AttemptStreamRecorder::new();
        let mut live = Vec::new();
        live.extend(recorder.push_emitting(1000, &thinking("先想"), &decode));
        live.extend(recorder.push_emitting(1012, &thinking("再查"), &decode));
        live.extend(recorder.push_emitting(
            1025,
            &text(LlmStreamChunkType::ContentDelta, "好"),
            &decode,
        ));
        live.extend(recorder.push_emitting(
            1030,
            &call(
                LlmStreamChunkType::ToolCallStart,
                Some("call_1"),
                Some("wire__search"),
                Some("{\"q\":"),
                Some(0),
            ),
            &decode,
        ));
        live.extend(recorder.push_emitting(
            1045,
            &call(
                LlmStreamChunkType::ToolCallDelta,
                None,
                None,
                Some("\"rust\"}"),
                Some(0),
            ),
            &decode,
        ));
        let (records, tail) = recorder
            .finish_with_emitting(1060, Some("tool_calls"), &decode)
            .expect("normalizable stop reason");
        live.extend(tail);
        assert_eq!(live, expand_compact(&records));
        assert!(
            live.iter()
                .any(|item| item.chunk["type"] == "tool-call-delta"
                    && item.chunk["name"] == "plugin/search"),
            "tool name must be decoded immediately on live chunks"
        );

        // 失败路径同样逐块一致。
        let mut failed = AttemptStreamRecorder::new();
        let mut failed_live = Vec::new();
        failed_live.extend(failed.push_emitting(
            2000,
            &text(LlmStreamChunkType::ContentDelta, "半"),
            &decode,
        ));
        let (failed_records, failed_tail) = failed.fail_with_emitting(
            2010,
            AttemptStreamFailure {
                message: "timeout".into(),
                code: "MODEL_TRANSPORT_FAILED".into(),
                status: None,
            },
            false,
            &decode,
        );
        failed_live.extend(failed_tail);
        assert_eq!(failed_live, expand_compact(&failed_records));
    }
}
