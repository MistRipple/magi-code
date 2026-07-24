//! 内部工具名称与上游协议名称之间的唯一编解码边界。
//!
//! Magi 的运行时、会话记录和工具注册表只使用规范工具名。上游模型只看见
//! 由当前 ToolSurface 派生的安全 wire name；无论模型、协议或工具来源如何变化，
//! 都不会把本地工具名与上游原生能力混在同一个命名空间内。

use super::adapter::AdaptedResponse;
use crate::llm_types::{
    LlmContentBlock, LlmMessageContent, LlmMessageParams, LlmStreamChunk, ToolChoice,
};
use crate::types::ChatToolOrigin;
use std::collections::{BTreeMap, BTreeSet};

const MAX_WIRE_NAME_LEN: usize = 64;
const WIRE_PREFIX_LEN: usize = 28;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderToolNameCodec {
    canonical_to_wire: BTreeMap<String, String>,
    wire_to_canonical: BTreeMap<String, String>,
}

impl ProviderToolNameCodec {
    /// 从完整请求的 ToolSurface 构造当前调用唯一的名称映射。
    ///
    /// 除当前轮的工具定义外，历史 tool_use 与指定 tool_choice 也会纳入映射，
    /// 使多轮请求中的工具名始终保持可逆。没有归属信息的旧历史记录被明确标为
    /// `unspecified`，而非把它们原样暴露给上游。
    pub fn for_params(params: &LlmMessageParams) -> Self {
        let mut identities = BTreeMap::new();
        for tool in params.tools.as_deref().unwrap_or_default() {
            register_identity(&mut identities, &tool.name, tool.origin);
        }
        if let Some(ToolChoice::Typed {
            name: Some(name), ..
        }) = params.tool_choice.as_ref()
        {
            register_identity(&mut identities, name, ChatToolOrigin::Unspecified);
        }
        for message in &params.messages {
            let LlmMessageContent::Blocks(blocks) = &message.content else {
                continue;
            };
            for block in blocks {
                if let LlmContentBlock::ToolUse { name, .. } = block {
                    register_identity(&mut identities, name, ChatToolOrigin::Unspecified);
                }
            }
        }

        let mut canonical_to_wire = BTreeMap::new();
        let mut wire_to_canonical = BTreeMap::new();
        let mut assigned_wire_names = BTreeSet::new();
        for (canonical_name, origin) in identities {
            let wire_name = allocate_wire_name(&canonical_name, origin, &assigned_wire_names);
            assigned_wire_names.insert(wire_name.clone());
            canonical_to_wire.insert(canonical_name.clone(), wire_name.clone());
            wire_to_canonical.insert(wire_name, canonical_name);
        }

        Self {
            canonical_to_wire,
            wire_to_canonical,
        }
    }

    pub fn encode_name(&self, canonical_name: &str) -> String {
        self.canonical_to_wire
            .get(canonical_name)
            .cloned()
            .unwrap_or_else(|| canonical_name.to_string())
    }

    /// 未在当前 ToolSurface 中出现的名称必须保持为未知名称，交由唯一工具执行器
    /// 拒绝；绝不能猜测或还原成任意本地工具名。
    pub fn decode_name(&self, wire_name: &str) -> String {
        self.wire_to_canonical
            .get(wire_name)
            .cloned()
            .unwrap_or_else(|| wire_name.to_string())
    }

    /// 在请求离开 Magi 前统一编码。覆盖工具定义、工具选择及历史 assistant
    /// tool-call，确保同一轮和后续轮次使用同一套 wire 名称。
    pub fn encode_request_params(&self, params: &mut LlmMessageParams) {
        if let Some(tools) = params.tools.as_mut() {
            for tool in tools {
                tool.name = self.encode_name(&tool.name);
            }
        }
        if let Some(ToolChoice::Typed {
            name: Some(name), ..
        }) = params.tool_choice.as_mut()
        {
            *name = self.encode_name(name);
        }
        for message in &mut params.messages {
            let LlmMessageContent::Blocks(blocks) = &mut message.content else {
                continue;
            };
            for block in blocks {
                if let LlmContentBlock::ToolUse { name, .. } = block {
                    *name = self.encode_name(name);
                }
            }
        }
    }

    /// 在上游响应进入 Magi 前统一解码，运行时之后只可能看到规范工具名或明确的
    /// 未知名称，后者不能匹配任何已注册 executor。
    pub fn decode_adapted_response(&self, mut response: AdaptedResponse) -> AdaptedResponse {
        for tool_call in &mut response.tool_calls {
            tool_call.name = self.decode_name(&tool_call.name);
        }
        response
    }

    /// SSE 的 tool-call 与非流式响应走同一个入站边界，避免流式会话把 wire
    /// 名称写入 session 或交给工具运行时。
    pub fn decode_stream_chunks(&self, chunks: &mut [LlmStreamChunk]) {
        for chunk in chunks {
            if let Some(tool_call) = chunk.tool_call.as_mut()
                && let Some(name) = tool_call.name.as_mut()
            {
                *name = self.decode_name(name);
            }
        }
    }
}

fn register_identity(
    identities: &mut BTreeMap<String, ChatToolOrigin>,
    canonical_name: &str,
    origin: ChatToolOrigin,
) {
    let canonical_name = canonical_name.trim();
    if canonical_name.is_empty() {
        return;
    }
    identities
        .entry(canonical_name.to_string())
        .and_modify(|existing| {
            if *existing == ChatToolOrigin::Unspecified && origin != ChatToolOrigin::Unspecified {
                *existing = origin;
            }
        })
        .or_insert(origin);
}

fn allocate_wire_name(
    canonical_name: &str,
    origin: ChatToolOrigin,
    assigned_wire_names: &BTreeSet<String>,
) -> String {
    let namespace = origin_namespace(origin);
    let readable = readable_component(canonical_name);
    let hash = stable_tool_identity_hash(origin, canonical_name);
    let base = format!("magi_{namespace}_{readable}_{hash:016x}");
    if !assigned_wire_names.contains(&base) {
        return base;
    }

    // 64 位哈希碰撞极低，但 ToolSurface 仍必须在碰撞时保持一一映射。输入已按
    // canonical identity 排序，因此同一工具面下的后缀分配是稳定的。
    for suffix in 1usize.. {
        let suffix = format!("_{suffix}");
        let prefix_len = MAX_WIRE_NAME_LEN.saturating_sub(suffix.len());
        let candidate = format!("{}{}", &base[..prefix_len.min(base.len())], suffix);
        if !assigned_wire_names.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("unbounded collision suffix allocation must find a free name")
}

fn origin_namespace(origin: ChatToolOrigin) -> &'static str {
    match origin {
        ChatToolOrigin::Builtin => "builtin",
        ChatToolOrigin::ExternalMcp => "mcp",
        ChatToolOrigin::Skill => "skill",
        ChatToolOrigin::Unspecified => "legacy",
    }
}

fn readable_component(canonical_name: &str) -> String {
    let mut output = String::new();
    let mut previous_separator = false;
    for byte in canonical_name.bytes() {
        let character = match byte {
            b'A'..=b'Z' => (byte as char).to_ascii_lowercase(),
            b'a'..=b'z' | b'0'..=b'9' => byte as char,
            _ => '_',
        };
        if character == '_' {
            if previous_separator {
                continue;
            }
            previous_separator = true;
        } else {
            previous_separator = false;
        }
        output.push(character);
        if output.len() == WIRE_PREFIX_LEN {
            break;
        }
    }
    let output = output.trim_matches('_');
    if output.is_empty() {
        "tool".to_string()
    } else {
        output.to_string()
    }
}

fn stable_tool_identity_hash(origin: ChatToolOrigin, canonical_name: &str) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET_BASIS;
    for byte in origin_namespace(origin)
        .bytes()
        .chain(std::iter::once(0))
        .chain(canonical_name.as_bytes().iter().copied())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_types::{
        LlmMessage, LlmStreamChunkType, PartialToolCall, ToolDefinition, ToolInputSchema,
    };
    use serde_json::json;

    fn tool(name: &str, origin: ChatToolOrigin) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: "test".to_string(),
            input_schema: ToolInputSchema {
                kind: "object".to_string(),
                properties: json!({}),
                required: None,
            },
            origin,
        }
    }

    fn params() -> LlmMessageParams {
        LlmMessageParams {
            messages: vec![LlmMessage {
                role: "assistant".to_string(),
                content: LlmMessageContent::Blocks(vec![LlmContentBlock::ToolUse {
                    id: "call-1".to_string(),
                    name: "web_search".to_string(),
                    input: json!({"query": "Magi"}),
                }]),
            }],
            max_tokens: None,
            temperature: None,
            tools: Some(vec![
                tool("web_search", ChatToolOrigin::Builtin),
                tool("agent_spawn", ChatToolOrigin::Builtin),
                tool("mcp__filesystem__read", ChatToolOrigin::ExternalMcp),
                tool("skill__review__inspect", ChatToolOrigin::Skill),
            ]),
            stream: Some(true),
            system_prompt: None,
            tool_choice: Some(ToolChoice::Typed {
                kind: "tool".to_string(),
                name: Some("web_search".to_string()),
            }),
            reasoning_effort: None,
        }
    }

    #[test]
    fn every_tool_source_uses_safe_wire_names_independent_of_model_name() {
        let params = params();
        let codec = ProviderToolNameCodec::for_params(&params);
        for canonical_name in [
            "web_search",
            "agent_spawn",
            "mcp__filesystem__read",
            "skill__review__inspect",
        ] {
            let wire_name = codec.encode_name(canonical_name);
            assert_ne!(wire_name, canonical_name);
            assert!(wire_name.starts_with("magi_"));
            assert!(wire_name.len() <= MAX_WIRE_NAME_LEN);
            assert!(
                wire_name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            );
            assert_eq!(codec.decode_name(&wire_name), canonical_name);
        }
        assert_ne!(
            codec.encode_name("web_search"),
            codec.encode_name("mcp__filesystem__read")
        );
    }

    #[test]
    fn codec_encodes_definitions_choices_and_history_with_one_mapping() {
        let mut params = params();
        let codec = ProviderToolNameCodec::for_params(&params);
        let web_search_wire_name = codec.encode_name("web_search");

        codec.encode_request_params(&mut params);

        let tools = params.tools.expect("tools should remain present");
        assert!(tools.iter().all(|tool| tool.name.starts_with("magi_")));
        assert!(matches!(
            params.tool_choice,
            Some(ToolChoice::Typed { name: Some(name), .. }) if name == web_search_wire_name
        ));
        let LlmMessageContent::Blocks(blocks) = &params.messages[0].content else {
            panic!("assistant message should retain blocks");
        };
        assert!(matches!(
            &blocks[0],
            LlmContentBlock::ToolUse { name, .. } if name == &web_search_wire_name
        ));
    }

    #[test]
    fn unknown_wire_name_cannot_be_decoded_to_a_registered_tool() {
        let params = params();
        let codec = ProviderToolNameCodec::for_params(&params);
        assert_eq!(codec.decode_name("web_search"), "web_search");
        assert_eq!(
            codec.decode_name("magi_builtin_forged_1234"),
            "magi_builtin_forged_1234"
        );
    }

    #[test]
    fn stream_response_decoding_returns_canonical_tool_identity() {
        let params = params();
        let codec = ProviderToolNameCodec::for_params(&params);
        let wire_name = codec.encode_name("web_search");
        let mut chunks = vec![LlmStreamChunk {
            kind: LlmStreamChunkType::ToolCallStart,
            content: None,
            tool_call: Some(PartialToolCall {
                id: Some("call-1".to_string()),
                name: Some(wire_name),
                arguments: None,
                index: Some(0),
            }),
            thinking: None,
            usage: None,
            stop_reason: None,
        }];

        codec.decode_stream_chunks(&mut chunks);

        assert_eq!(
            chunks[0]
                .tool_call
                .as_ref()
                .and_then(|call| call.name.as_deref()),
            Some("web_search")
        );
    }
}
