//! 正文 Prompt IR 的共同编译入口。Context Authority 提供带可信来源的消息，
//! 摘要和 Provider renderer 消费相同的规范化规则；不从正文识别缓存控制指令。
use crate::llm_types::{
    LlmContentBlock, LlmMessage, LlmMessageContent, ToolChoice, ToolDefinition, ToolInputSchema,
    parse_tool_result_model_content,
};
use crate::{ChatMessageOrigin, ModelInvocationRequest};
use magi_usage_authority::ReasoningEffort;
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone, Debug)]
pub struct PromptIr {
    pub prompt_cache_key: Option<String>,
    pub reasoning_capability: Option<crate::reasoning::ReasoningCapability>,
    pub messages: Vec<LlmMessage>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub stop: Vec<String>,
    pub tools: Option<Vec<ToolDefinition>>,
    pub stream: Option<bool>,
    pub tool_choice: Option<ToolChoice>,
    /// 推理强度配置：必须透传到协议层（OpenAI Chat 走顶层 `reasoning_effort`，
    /// OpenAI Responses 走 `reasoning.effort`，Anthropic Messages 走
    /// `thinking.budget_tokens` 映射）。
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl PromptIr {
    /// 第一个易变段之后的所有消息都属于本轮尾部；禁止渲染器把它们前移到 system。
    pub fn history_end(&self) -> usize {
        self.messages
            .iter()
            .position(|message| message.origin == ChatMessageOrigin::RoundContext)
            .unwrap_or(self.messages.len())
    }

    pub fn volatile_tail_messages(&self) -> usize {
        self.messages.len() - self.history_end()
    }
}

pub fn compile_prompt_ir(
    request: &ModelInvocationRequest,
    stream: bool,
    reasoning_effort: Option<ReasoningEffort>,
    max_output_tokens: Option<u32>,
) -> PromptIr {
    let normalized = request
        .messages
        .as_ref()
        .map(|messages| normalize_chat_tool_call_history(messages));
    let messages = normalized
        .map(|messages| messages.iter().map(llm_message_from_chat_message).collect())
        .unwrap_or_else(|| {
            vec![LlmMessage {
                origin: Default::default(),
                role: "user".to_string(),
                content: LlmMessageContent::Text(request.prompt.clone()),
            }]
        });
    PromptIr {
        prompt_cache_key: request.prompt_cache_key.clone(),
        reasoning_capability: None,
        messages,
        max_tokens: max_output_tokens,
        temperature: None,
        stop: Vec::new(),
        tools: request
            .tools
            .as_ref()
            .filter(|tools| !tools.is_empty())
            .map(|tools| {
                crate::canonical_tool_surface(tools)
                    .iter()
                    .map(tool_definition_from_chat_tool)
                    .collect::<Vec<_>>()
            }),
        stream: Some(stream),

        tool_choice: request
            .tool_choice
            .as_ref()
            .map(|choice| ToolChoice::Typed {
                kind: "tool".to_string(),
                name: Some(choice.function.name.clone()),
            }),
        reasoning_effort,
    }
}

pub(crate) fn normalize_chat_tool_call_history(
    messages: &[crate::types::ChatMessage],
) -> Vec<crate::types::ChatMessage> {
    let mut normalized = Vec::with_capacity(messages.len());
    let mut result_ids = HashMap::<String, VecDeque<Option<String>>>::new();

    for message in messages {
        if !message.tool_calls.is_empty() {
            let mut message = message.clone();
            let mut calls = Vec::with_capacity(message.tool_calls.len());
            let mut seen = HashMap::<String, (String, String)>::new();
            let mut used_ids = HashSet::<String>::new();

            for (index, mut tool_call) in message.tool_calls.into_iter().enumerate() {
                let original_id = tool_call.id.trim().to_string();
                // 派生 ID 和去重必须使用与正文相同的参数形态；否则缺少 ID
                // 的等价调用会仅因 JSON 空白/键顺序不同而改写整段历史前缀。
                let canonical_arguments =
                    serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments)
                        .map(|value| crate::llm_types::canonicalize_json(&value).to_string())
                        .unwrap_or_else(|_| tool_call.function.arguments.clone());
                let fingerprint = (tool_call.function.name.clone(), canonical_arguments.clone());
                if !original_id.is_empty() && seen.get(&original_id) == Some(&fingerprint) {
                    result_ids.entry(original_id).or_default().push_back(None);
                    continue;
                }

                if original_id.is_empty() || used_ids.contains(&original_id) {
                    let base = crate::protocol::compatible_tool_call_id(
                        index,
                        &tool_call.function.name,
                        &canonical_arguments,
                    );
                    let mut candidate = base.clone();
                    let mut suffix = 1usize;
                    while used_ids.contains(&candidate) {
                        candidate = format!("{base}_{suffix}");
                        suffix += 1;
                    }
                    tool_call.id = candidate;
                } else {
                    tool_call.id = original_id.clone();
                }

                seen.insert(original_id.clone(), fingerprint);
                used_ids.insert(tool_call.id.clone());
                result_ids
                    .entry(original_id)
                    .or_default()
                    .push_back(Some(tool_call.id.clone()));
                calls.push(tool_call);
            }

            message.tool_calls = calls;
            normalized.push(message);
            continue;
        }

        if let Some(original_id) = message.tool_call_id.as_ref()
            && let Some(ids) = result_ids.get_mut(original_id.trim())
            && let Some(normalized_id) = ids.pop_front()
        {
            if let Some(normalized_id) = normalized_id {
                let mut message = message.clone();
                message.tool_call_id = Some(normalized_id);
                normalized.push(message);
            }
            continue;
        }

        normalized.push(message.clone());
    }

    normalized
}

fn llm_message_from_chat_message(message: &crate::types::ChatMessage) -> LlmMessage {
    if let Some(tool_call_id) = message.tool_call_id.as_ref() {
        let tool_result_content = parse_tool_result_model_content(message.content.as_deref());
        return LlmMessage {
            origin: message.context_origin,
            role: "user".to_string(),
            content: LlmMessageContent::Blocks(vec![LlmContentBlock::ToolResult {
                tool_use_id: tool_call_id.clone(),
                content: tool_result_content.text,
                is_error: tool_result_content.is_error,
                images: tool_result_content.images,
            }]),
        };
    }

    let mut blocks = message
        .provider_context
        .iter()
        .cloned()
        .map(|context| LlmContentBlock::ProviderContext { context })
        .collect::<Vec<_>>();
    if let Some(content) = message
        .content
        .as_ref()
        .filter(|content| !content.is_empty())
    {
        blocks.push(LlmContentBlock::Text {
            text: content.clone(),
        });
    }
    for image in &message.images {
        blocks.push(LlmContentBlock::Image {
            source: image.clone(),
        });
    }
    for tool_call in &message.tool_calls {
        let input = crate::llm_types::canonicalize_json(
            &serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments)
                .unwrap_or_else(|_| json!({})),
        );
        blocks.push(LlmContentBlock::ToolUse {
            id: tool_call.id.clone(),
            name: tool_call.function.name.clone(),
            input,
        });
    }

    let content = if blocks.is_empty()
        || (message.provider_context.is_empty()
            && blocks.len() == 1
            && message.images.is_empty()
            && message.tool_calls.is_empty())
    {
        LlmMessageContent::Text(message.content.clone().unwrap_or_default())
    } else {
        LlmMessageContent::Blocks(blocks)
    };
    LlmMessage {
        origin: message.context_origin,
        role: message.role.clone(),
        content,
    }
}

fn tool_definition_from_chat_tool(tool: &crate::types::ChatToolDefinition) -> ToolDefinition {
    ToolDefinition {
        name: tool.function.name.clone(),
        description: tool.function.description.clone(),
        input_schema: ToolInputSchema::from_json_schema(&tool.function.parameters),
        origin: tool.origin,
    }
}
