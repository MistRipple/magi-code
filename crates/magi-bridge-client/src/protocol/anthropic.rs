use crate::prompt_ir::PromptIr;
use serde_json::{Value, json};

use super::adapter::{AdaptedRequest, AdaptedResponse, ProviderAdapter, ProviderFamily};
use super::capability::{ThinkingKind, resolve_capability_profile};
use super::utils::{
    convert_messages_to_anthropic, parse_anthropic_usage, reasoning_effort_label,
    serialize_anthropic_tool_definitions, truncate,
};
use crate::llm_types::{ToolCall, ToolChoice};
use magi_usage_authority::ReasoningEffort;

/// Anthropic Extended Thinking 默认预算（tokens）。
///
/// 仅在 `ThinkingKind::BudgetTokens` 形态、且能力表未提供 `default_budget_tokens`、
/// 且调用方未传 `reasoning_effort` 时使用。Anthropic 要求 `max_tokens > budget_tokens`。
const DEFAULT_THINKING_BUDGET_TOKENS: u32 = 4096;
/// 启用 thinking 时 `max_tokens` 的最低值（必须严格大于 budget）。
const MIN_MAX_TOKENS_WITH_THINKING: u32 = 8192;

/// 将 `ReasoningEffort` 映射为 Anthropic `thinking.budget_tokens` 数值。
///
/// 仅在 `ThinkingKind::BudgetTokens` 形态下使用（legacy Claude 3.7 / 4.x 系列）。
/// Opus 4.7+ 的 Adaptive Thinking 走 `thinking.effort` 字符串路径，不经过该函数。
fn reasoning_effort_budget_tokens(effort: ReasoningEffort) -> u32 {
    match effort {
        ReasoningEffort::None => 0,
        ReasoningEffort::Minimal => 1024,
        ReasoningEffort::Low => 1024,
        ReasoningEffort::Medium => 4096,
        ReasoningEffort::High => 16384,
        ReasoningEffort::Xhigh => 32768,
        ReasoningEffort::Max | ReasoningEffort::Ultra => 65536,
    }
}

pub struct AnthropicMessagesAdapter;

impl ProviderAdapter for AnthropicMessagesAdapter {
    fn family(&self) -> ProviderFamily {
        ProviderFamily::Anthropic
    }

    fn build_request(&self, params: &PromptIr, model: &str) -> Result<AdaptedRequest, String> {
        // Anthropic 没有独立的 developer role，system 字段又排在整段 messages 之前。
        // 因此只有开头那一段 system/developer 消息收敛进 system；对话开始之后出现的
        // （例如易变尾部里的执行步骤快照）留在原位置按 user 发送，片段自带的信任标记
        // 仍写在正文里。把它们并进 system 会让每轮变化的内容挡在全部历史前面。
        let conversation_starts_at = params
            .messages
            .iter()
            .position(|message| {
                !matches!(message.role.as_str(), "system" | "developer")
                    || message.origin == crate::ChatMessageOrigin::RoundContext
            })
            .unwrap_or(params.messages.len());
        let mut system_parts = Vec::new();
        let mut static_parts = Vec::new();
        let mut static_prefix_open = true;
        for message in &params.messages[..conversation_starts_at] {
            if let crate::llm_types::LlmMessageContent::Text(text) = &message.content {
                if static_prefix_open
                    && matches!(
                        message.origin,
                        crate::ChatMessageOrigin::StaticPolicy | crate::ChatMessageOrigin::Skill
                    )
                {
                    static_parts.push(text.as_str());
                } else {
                    static_prefix_open = false;
                    system_parts.push(text.as_str());
                }
            }
        }
        let map_role = |message: &crate::llm_types::LlmMessage| {
            let mut message = message.clone();
            if matches!(message.role.as_str(), "system" | "developer") {
                message.role = "user".into();
            }
            message
        };
        // 历史与尾部分别渲染。协议可能合并消息或展开图片，不能用原消息条数
        // 对已经转换的数组做减法，否则断点可能落进易变尾部。
        let history_end = params.history_end().max(conversation_starts_at);
        let history = params.messages[conversation_starts_at..history_end]
            .iter()
            .map(map_role)
            .collect::<Vec<_>>();
        let tail = params.messages[history_end..]
            .iter()
            .map(map_role)
            .collect::<Vec<_>>();
        let mut messages = convert_messages_to_anthropic(&history);
        if let Some(message) = messages
            .iter_mut()
            .rev()
            .find(|message| message["role"] == "user")
        {
            mark_cache_breakpoint(message);
        }
        messages.extend(convert_messages_to_anthropic(&tail));
        let static_text = static_parts.join("\n");
        let system_text = system_parts.join("\n");

        // thinking / beta header / temperature 全部由能力表驱动；
        // 不再保留"配置覆盖能力"的 OR 语义——未知 / 不支持 thinking 的模型
        // 即便调用方传了 reasoning_effort 也绝不写入 thinking 字段，
        // 避免向后端注入会触发 400 的可选字段。
        let mut capability = *resolve_capability_profile(model);
        if let Some(cap) = &params.reasoning_capability {
            capability.thinking_kind = if cap.supported_efforts.is_empty()
                || params.reasoning_effort == Some(ReasoningEffort::None)
                || params.reasoning_effort.is_none()
            {
                ThinkingKind::None
            } else {
                match cap.transport {
                    crate::reasoning::ReasoningTransport::AnthropicBudget => {
                        ThinkingKind::BudgetTokens
                    }
                    crate::reasoning::ReasoningTransport::AnthropicEffort => ThinkingKind::Effort,
                    _ => ThinkingKind::None,
                }
            };
        }
        let configured_effort = params.reasoning_effort;
        let thinking_value = build_thinking_value(
            capability.thinking_kind,
            capability.supports_thinking_display,
            capability.default_budget_tokens,
            configured_effort,
        );
        let thinking_budget_floor = thinking_value
            .as_ref()
            .and_then(|tv| tv["budget_tokens"].as_u64())
            .map(|n| n as u32);

        if params.max_tokens == Some(0) {
            return Err("executor_output_limit_incompatible: 输出上限必须大于零".into());
        }
        if params
            .max_tokens
            .zip(thinking_budget_floor)
            .is_some_and(|(limit, budget)| limit <= budget)
        {
            return Err("executor_output_limit_incompatible: 单次输出上限必须大于当前 Anthropic thinking 预算，请提高上限或调整推理强度".into());
        }
        let max_tokens = match (
            params.max_tokens,
            thinking_value.as_ref(),
            thinking_budget_floor,
        ) {
            (Some(limit), _, _) => limit,
            (None, Some(_), Some(budget)) => budget
                .saturating_add(4096)
                .max(MIN_MAX_TOKENS_WITH_THINKING),
            (None, Some(_), None) => MIN_MAX_TOKENS_WITH_THINKING,
            (None, None, _) => 4096,
        };

        let mut body = json!({
            "model": model,
            "messages": messages,
            "max_tokens": max_tokens,
        });

        if !static_text.is_empty() || !system_text.is_empty() {
            body["system"] = build_system_field(&static_text, &system_text);
        }

        if let Some(thinking) = thinking_value {
            body["thinking"] = thinking;
            // Anthropic 约束：thinking 启用时 temperature 必须 = 1。
            body["temperature"] = json!(1);
            // Adaptive Thinking only mode（Effort 类型）：推理强度走顶层
            // output_config.effort，而非 thinking 内字段（服务端契约）。
            if matches!(capability.thinking_kind, ThinkingKind::Effort) {
                let label = configured_effort
                    .map(reasoning_effort_label)
                    .unwrap_or("medium");
                body["output_config"] = json!({ "effort": label });
            }
        } else if let Some(temperature) = params.temperature {
            body["temperature"] = json!(temperature);
        }
        if params
            .temperature
            .is_some_and(|v| body.get("temperature").and_then(Value::as_f64) != Some(v))
        {
            return Err("Anthropic thinking 与请求的 temperature 冲突".into());
        }
        if !params.stop.is_empty() {
            body["stop_sequences"] = json!(params.stop);
        }

        if let Some(ref tools) = params.tools
            && !tools.is_empty()
        {
            let mut definitions = serialize_anthropic_tool_definitions(tools);
            if let Some(last) = definitions.last_mut() {
                last["cache_control"] = json!({"type": "ephemeral"});
            }
            body["tools"] = json!(definitions);
            if let Some(ref tc) = params.tool_choice {
                body["tool_choice"] = translate_tool_choice_for_anthropic(tc);
            }
        }
        if let Some(stream) = params.stream {
            body["stream"] = json!(stream);
        }

        let mut extra_headers = vec![("anthropic-version".to_string(), "2023-06-01".to_string())];
        for (name, value) in capability.beta_headers {
            extra_headers.push((name.to_string(), value.to_string()));
        }

        Ok(AdaptedRequest {
            url_path: "/v1/messages".to_string(),
            body,
            extra_headers,
        })
    }

    fn parse_response(&self, status: u16, body: &str) -> Result<AdaptedResponse, String> {
        if !(200..300).contains(&status) {
            return Err(format!(
                "Anthropic API error (status={}): {}",
                status,
                truncate(body, 512)
            ));
        }

        let envelope: Value =
            serde_json::from_str(body).map_err(|e| format!("invalid JSON response: {e}"))?;

        let stop_reason = envelope["stop_reason"]
            .as_str()
            .unwrap_or("end_turn")
            .to_string();

        let content_blocks = envelope["content"].as_array().cloned().unwrap_or_default();

        let mut text_parts = Vec::new();
        let mut thinking_parts = Vec::new();
        let mut tool_calls = Vec::new();
        let mut provider_context = Vec::new();

        for block in &content_blocks {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(text) = block["text"].as_str() {
                        text_parts.push(text.to_string());
                    }
                }
                Some("thinking") => {
                    if let Some(thinking) = block["thinking"].as_str() {
                        thinking_parts.push(thinking.to_string());
                    }
                    if block["signature"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
                    {
                        provider_context.push(crate::types::ModelProviderContext {
                            provider: "anthropic".to_string(),
                            kind: "thinking".to_string(),
                            data: block.clone(),
                        });
                    }
                }
                Some("redacted_thinking") => {
                    provider_context.push(crate::types::ModelProviderContext {
                        provider: "anthropic".to_string(),
                        kind: "redacted_thinking".to_string(),
                        data: block.clone(),
                    });
                }
                Some("tool_use") => {
                    if let (Some(id), Some(name)) = (block["id"].as_str(), block["name"].as_str()) {
                        let input = block["input"].clone();
                        let raw = serde_json::to_string(&input).ok();
                        tool_calls.push(ToolCall {
                            id: id.to_string(),
                            name: name.to_string(),
                            arguments: input,
                            argument_parse_error: None,
                            raw_arguments: raw,
                        });
                    }
                }
                _ => {}
            }
        }

        let usage = envelope
            .get("usage")
            .map(parse_anthropic_usage)
            .unwrap_or_default();

        Ok(AdaptedResponse {
            content: text_parts.join(""),
            thinking: {
                let thinking = thinking_parts.join("");
                (!thinking.trim().is_empty()).then_some(thinking)
            },
            tool_calls,
            usage,
            stop_reason,
            raw: Some(envelope),
            provider_context,
        })
    }

    fn max_output_tokens_field(&self) -> &str {
        "max_tokens"
    }
}

/// 将运行时统一的工具选择语义翻译为 Anthropic Messages 协议。
///
/// `required` 在 OpenAI Chat 中表示“至少调用一个工具”；Anthropic 对应 `any`。
/// 该转换保持在协议适配器内，使上层只表达能力语义而不携带提供方分支。
fn translate_tool_choice_for_anthropic(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Simple(kind) if kind == "required" => json!({"type": "any"}),
        ToolChoice::Simple(kind) if kind == "auto" => json!({"type": "auto"}),
        ToolChoice::Simple(kind) if kind == "none" => json!({"type": "auto"}),
        ToolChoice::Simple(kind) => json!({"type": kind}),
        ToolChoice::Typed {
            kind,
            name: Some(name),
        } => {
            json!({"type": kind, "name": name})
        }
        ToolChoice::Typed { kind, name: None } => json!({"type": kind}),
    }
}

/// 根据能力表 `thinking_kind` 字段构造 Anthropic `thinking` JSON 载荷。
///
/// - `None`：模型不支持 thinking，返回 `None`，主体不写 `thinking` 字段；
/// - `BudgetTokens`（3.7 / 4.x legacy）：写 `{ type:"enabled", budget_tokens }`，
///   优先用调用方 `reasoning_effort` 映射的预算，否则用能力表 default，再否则用全局默认预算；
/// - `Effort`（4.7+ Adaptive Thinking only mode）：写 `{ type:"adaptive" }`，
///   推理强度由顶层 `output_config.effort` 控制（见 body 组装处），不再用
///   已被服务端拒绝的 `{ type:"enabled", effort }` 形态；
/// - `Adaptive`：写 `{ type:"adaptive" }`，模型自行决定预算，可带 `display`。
fn build_thinking_value(
    kind: ThinkingKind,
    supports_display: bool,
    default_budget: u32,
    configured_effort: Option<ReasoningEffort>,
) -> Option<Value> {
    let attach_display = |mut value: Value| -> Value {
        if supports_display {
            value["display"] = json!("summarized");
        }
        value
    };

    match kind {
        ThinkingKind::None => None,
        ThinkingKind::BudgetTokens => {
            let budget = configured_effort
                .map(reasoning_effort_budget_tokens)
                .unwrap_or_else(|| {
                    if default_budget > 0 {
                        default_budget
                    } else {
                        DEFAULT_THINKING_BUDGET_TOKENS
                    }
                });
            Some(attach_display(json!({
                "type": "enabled",
                "budget_tokens": budget,
            })))
        }
        // Effort 与 Adaptive 都使用 adaptive thinking；二者区别仅在于
        // Effort 会在顶层附加 output_config.effort（由调用方推理强度驱动）。
        ThinkingKind::Effort | ThinkingKind::Adaptive => {
            Some(attach_display(json!({ "type": "adaptive" })))
        }
    }
}

/// 只根据可信 IR 层级放置静态断点，正文中的标记字符串没有控制语义。
fn build_system_field(static_text: &str, session_text: &str) -> Value {
    if static_text.is_empty() {
        return json!(session_text);
    }
    let mut blocks = vec![json!({
        "type": "text", "text": static_text, "cache_control": {"type": "ephemeral"}
    })];
    if !session_text.is_empty() {
        blocks.push(json!({"type": "text", "text": session_text}));
    }
    json!(blocks)
}

/// 给 user 消息的末个受支持内容块打 `ephemeral` 断点；字符串内容先转成等价 text block。
/// 不碰 assistant 的 thinking/signature，也不改变 `tool_result` 结构。
fn mark_cache_breakpoint(message: &mut Value) {
    if let Some(text) = message["content"].as_str().filter(|text| !text.is_empty()) {
        message["content"] = json!([{"type": "text", "text": text}]);
    }
    if let Some(block) = message["content"]
        .as_array_mut()
        .and_then(|blocks| blocks.last_mut())
        && matches!(
            block["type"].as_str(),
            Some("text" | "image" | "tool_result")
        )
    {
        block["cache_control"] = json!({"type": "ephemeral"});
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_types::{LlmMessage, LlmMessageContent};

    fn base_params(messages: Vec<LlmMessage>) -> PromptIr {
        PromptIr {
            prompt_cache_key: None,

            reasoning_capability: None,
            messages,
            max_tokens: None,
            temperature: None,
            stop: Vec::new(),
            tools: None,
            stream: None,

            tool_choice: None,
            reasoning_effort: None,
        }
    }

    #[test]
    fn build_system_field_returns_plain_string_without_boundary() {
        let value = build_system_field("", "plain system prompt");
        assert_eq!(value, json!("plain system prompt"));
    }

    #[test]
    fn cache_breakpoints_cover_tools_static_system_and_rolling_tool_results() {
        let mut params = base_params(vec![
            LlmMessage {
                origin: crate::ChatMessageOrigin::StaticPolicy,
                role: "system".into(),
                content: LlmMessageContent::Text("static".into()),
            },
            LlmMessage {
                origin: crate::ChatMessageOrigin::SystemInstruction,
                role: "system".into(),
                content: LlmMessageContent::Text("dynamic".into()),
            },
            LlmMessage {
                origin: Default::default(),
                role: "user".into(),
                content: LlmMessageContent::Blocks(vec![
                    crate::llm_types::LlmContentBlock::ToolResult {
                        tool_use_id: "call-1".into(),
                        content: "result".into(),
                        is_error: false,
                        images: vec![],
                    },
                ]),
            },
        ]);
        params.tools = Some(vec![crate::llm_types::ToolDefinition {
            name: "file_read".into(),
            description: "read".into(),
            origin: crate::types::ChatToolOrigin::Builtin,
            input_schema: crate::llm_types::ToolInputSchema::from_json_schema(
                &json!({"type":"object"}),
            ),
        }]);
        let body = AnthropicMessagesAdapter
            .build_request(&params, "claude-test")
            .unwrap()
            .body;
        assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert!(body["system"][1].get("cache_control").is_none());
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(body["messages"][0]["content"][0]["content"], "result");
        let text_params = base_params(vec![LlmMessage {
            origin: Default::default(),
            role: "user".into(),
            content: LlmMessageContent::Text("work".into()),
        }]);
        let text_body = AnthropicMessagesAdapter
            .build_request(&text_params, "claude-test")
            .unwrap()
            .body;
        assert_eq!(
            text_body["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }

    fn text_message(role: &str, text: &str) -> LlmMessage {
        LlmMessage {
            origin: Default::default(),
            role: role.into(),
            content: LlmMessageContent::Text(text.into()),
        }
    }

    fn tool_round(id: &str) -> [LlmMessage; 2] {
        [
            LlmMessage {
                origin: Default::default(),
                role: "assistant".into(),
                content: LlmMessageContent::Blocks(vec![
                    crate::llm_types::LlmContentBlock::ToolUse {
                        id: id.into(),
                        name: "file_read".into(),
                        input: json!({}),
                    },
                ]),
            },
            LlmMessage {
                origin: Default::default(),
                role: "user".into(),
                content: LlmMessageContent::Blocks(vec![
                    crate::llm_types::LlmContentBlock::ToolResult {
                        tool_use_id: id.into(),
                        content: format!("result of {id}"),
                        is_error: false,
                        images: vec![],
                    },
                ]),
            },
        ]
    }

    fn request_with_round_tail(rounds: usize, tail: &[(&str, &str)]) -> serde_json::Value {
        let mut messages = vec![
            LlmMessage {
                origin: crate::ChatMessageOrigin::StaticPolicy,
                ..text_message("system", "static")
            },
            text_message("system", "dynamic"),
            text_message("user", "goal"),
        ];
        for round in 0..rounds {
            messages.extend(tool_round(&format!("call-{round}")));
        }
        messages.extend(tail.iter().map(|(role, text)| LlmMessage {
            origin: crate::ChatMessageOrigin::RoundContext,
            ..text_message(role, text)
        }));
        let params = base_params(messages);
        AnthropicMessagesAdapter
            .build_request(&params, "claude-test")
            .unwrap()
            .body
    }

    #[test]
    fn round_tail_never_enters_system_and_never_carries_the_rolling_breakpoint() {
        let body = request_with_round_tail(
            1,
            &[("developer", "STEPS-SNAPSHOT"), ("user", "TASK-CONTEXT")],
        );
        assert!(!body["system"].to_string().contains("STEPS-SNAPSHOT"));
        let messages = body["messages"].as_array().unwrap();
        // goal, tool_use, tool_result, 两条尾部消息
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(
            messages[2]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        for tail in &messages[3..] {
            assert_eq!(tail["role"], "user");
            assert!(!tail.to_string().contains("cache_control"));
        }
        assert!(messages[3].to_string().contains("STEPS-SNAPSHOT"));
    }

    /// 提供方缓存按渲染后的完整前缀匹配，不看 `cache_control` 标记本身；
    /// 比较前去掉标记，其余字节必须逐一相同。
    fn without_markers(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => serde_json::Value::Object(
                map.iter()
                    .filter(|(key, _)| key.as_str() != "cache_control")
                    .map(|(key, value)| (key.clone(), without_markers(value)))
                    .collect(),
            ),
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(without_markers).collect())
            }
            other => other.clone(),
        }
    }

    #[test]
    fn consecutive_tool_loop_requests_keep_the_previous_request_as_a_prefix() {
        let first =
            request_with_round_tail(1, &[("developer", "STEPS rev=1"), ("user", "CONTEXT a")]);
        let next =
            request_with_round_tail(2, &[("developer", "STEPS rev=2"), ("user", "CONTEXT b")]);
        assert_eq!(first["system"], next["system"]);
        assert_eq!(first["tools"], next["tools"]);
        let first_history = &first["messages"].as_array().unwrap()[..3];
        let next_history = &next["messages"].as_array().unwrap()[..3];
        assert_eq!(
            without_markers(&json!(first_history)),
            without_markers(&json!(next_history))
        );
        // 滚动断点前移：上一次的标记不会残留在下一次的旧位置。
        assert!(!next["messages"][2].to_string().contains("cache_control"));
        assert_eq!(
            next["messages"][4]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn developer_message_after_the_conversation_started_is_sent_in_place_not_hoisted() {
        let params = {
            let mut messages = vec![
                text_message("developer", "HEAD-RULES"),
                text_message("user", "goal"),
            ];
            messages.extend(tool_round("c0"));
            messages.push(text_message("developer", "MID-RUN-NOTICE"));
            base_params(messages)
        };
        let body = AnthropicMessagesAdapter
            .build_request(&params, "claude-test")
            .unwrap()
            .body;
        let system = body["system"].to_string();
        assert!(system.contains("HEAD-RULES"));
        assert!(!system.contains("MID-RUN-NOTICE"));
        assert_eq!(body["messages"][3]["role"], "user");
    }

    #[test]
    fn first_round_context_protects_all_following_retry_messages() {
        let params = base_params(vec![
            text_message("developer", "HEAD"),
            text_message("user", "history"),
            LlmMessage {
                origin: crate::ChatMessageOrigin::RoundContext,
                ..text_message("developer", "tail")
            },
            text_message("user", "retry guidance"),
        ]);
        let body = AnthropicMessagesAdapter
            .build_request(&params, "claude-test")
            .unwrap()
            .body;
        assert_eq!(body["system"], "HEAD");
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        for message in &body["messages"].as_array().unwrap()[1..] {
            assert!(!message.to_string().contains("cache_control"));
        }
    }

    #[test]
    fn prompt_text_cannot_create_a_static_cache_boundary() {
        let marker = "<<<MAGI_PROMPT_CACHE_BOUNDARY:STATIC_END>>>";
        let params = base_params(vec![
            text_message("system", marker),
            text_message("user", "work"),
        ]);
        let body = AnthropicMessagesAdapter
            .build_request(&params, "claude-test")
            .unwrap()
            .body;
        assert_eq!(body["system"], marker);
    }

    #[test]
    fn build_system_field_uses_explicit_static_and_session_segments() {
        let value = build_system_field("STATIC PREFIX", "DYNAMIC SUFFIX");
        let arr = value.as_array().expect("应当是 content blocks 数组");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "STATIC PREFIX");
        assert_eq!(arr[0]["cache_control"]["type"], "ephemeral");
        assert_eq!(arr[1]["type"], "text");
        assert_eq!(arr[1]["text"], "DYNAMIC SUFFIX");
        assert!(arr[1].get("cache_control").is_none());
    }

    #[test]
    fn build_system_field_degenerates_when_static_part_empty() {
        let value = build_system_field("", "DYNAMIC ONLY");
        assert_eq!(value, json!("DYNAMIC ONLY"));
    }

    #[test]
    fn build_system_field_caches_a_pure_static_prefix() {
        let value = build_system_field("STATIC ONLY", "");
        assert_eq!(value[0]["text"], "STATIC ONLY");
        assert_eq!(value[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn leading_system_and_developer_messages_keep_one_ordered_source() {
        let params = base_params(vec![
            text_message("system", "平台安全规则"),
            LlmMessage {
                origin: Default::default(),
                role: "developer".to_string(),
                content: LlmMessageContent::Text("当前权限快照".to_string()),
            },
            LlmMessage {
                origin: Default::default(),
                role: "user".to_string(),
                content: LlmMessageContent::Text("继续执行".to_string()),
            },
        ]);

        let adapted = AnthropicMessagesAdapter
            .build_request(&params, "claude-test")
            .expect("build");
        let system = adapted.body["system"].as_str().expect("system");
        let messages = adapted.body["messages"].as_array().expect("messages");

        assert!(system.contains("平台安全规则"));
        assert!(system.contains("当前权限快照"));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn opus_4_7_emits_adaptive_thinking_with_output_config_and_beta_header() {
        let mut params = base_params(vec![LlmMessage {
            origin: Default::default(),
            role: "user".to_string(),
            content: LlmMessageContent::Text("hi".to_string()),
        }]);
        params.reasoning_effort = Some(ReasoningEffort::Xhigh);
        let adapted = AnthropicMessagesAdapter
            .build_request(&params, "claude-opus-4-7-20260520")
            .expect("build");
        assert_eq!(adapted.body["thinking"]["type"], "adaptive");
        assert_eq!(adapted.body["output_config"]["effort"], "xhigh");
        assert_eq!(adapted.body["thinking"]["display"], "summarized");
        assert!(adapted.body["thinking"].get("effort").is_none());
        assert!(adapted.body["thinking"].get("budget_tokens").is_none());
        assert_eq!(adapted.body["temperature"], 1);
        assert!(
            adapted
                .extra_headers
                .iter()
                .any(|(n, v)| { n == "anthropic-beta" && v == "task-budgets-2026-03-13" })
        );
    }

    #[test]
    fn opus_4_7_default_effort_is_medium() {
        let params = base_params(vec![LlmMessage {
            origin: Default::default(),
            role: "user".to_string(),
            content: LlmMessageContent::Text("hi".to_string()),
        }]);
        let adapted = AnthropicMessagesAdapter
            .build_request(&params, "claude-opus-4-7-20260520")
            .expect("build");
        assert_eq!(adapted.body["thinking"]["type"], "adaptive");
        assert_eq!(adapted.body["output_config"]["effort"], "medium");
    }

    #[test]
    fn tool_round_keeps_extended_thinking_enabled() {
        let mut params = base_params(vec![LlmMessage {
            origin: Default::default(),
            role: "user".to_string(),
            content: LlmMessageContent::Text("请查询后回答".to_string()),
        }]);
        params.reasoning_effort = Some(ReasoningEffort::High);
        params.tools = Some(vec![crate::llm_types::ToolDefinition {
            name: "search".to_string(),
            description: "查询资料".to_string(),
            input_schema: crate::llm_types::ToolInputSchema {
                kind: "object".to_string(),
                properties: json!({}),
                required: None,
                additional_keywords: serde_json::Map::new(),
            },
            origin: crate::types::ChatToolOrigin::Builtin,
        }]);

        let adapted = AnthropicMessagesAdapter
            .build_request(&params, "claude-opus-4-7-20260520")
            .expect("build");

        assert_eq!(adapted.body["thinking"]["type"], "adaptive");
        assert_eq!(adapted.body["output_config"]["effort"], "high");
        assert_eq!(adapted.body["temperature"], 1);
        assert_eq!(adapted.body["tools"][0]["name"], "search");
    }

    #[test]
    fn response_and_next_request_round_trip_signed_thinking_block() {
        let response = AnthropicMessagesAdapter
            .parse_response(
                200,
                &json!({
                    "content": [
                        {
                            "type": "thinking",
                            "thinking": "需要先查询",
                            "signature": "signed-context"
                        },
                        {
                            "type": "tool_use",
                            "id": "tool-1",
                            "name": "search",
                            "input": {"query": "Magi"}
                        }
                    ],
                    "stop_reason": "tool_use",
                    "usage": {"input_tokens": 10, "output_tokens": 5}
                })
                .to_string(),
            )
            .expect("response should parse");

        assert_eq!(response.provider_context.len(), 1);
        assert_eq!(
            response.provider_context[0].data["signature"],
            "signed-context"
        );

        let params = base_params(vec![LlmMessage {
            origin: Default::default(),
            role: "assistant".to_string(),
            content: LlmMessageContent::Blocks(vec![
                crate::llm_types::LlmContentBlock::ProviderContext {
                    context: response.provider_context[0].clone(),
                },
                crate::llm_types::LlmContentBlock::ToolUse {
                    id: "tool-1".to_string(),
                    name: "search".to_string(),
                    input: json!({"query": "Magi"}),
                },
            ]),
        }]);
        let request = AnthropicMessagesAdapter
            .build_request(&params, "claude-opus-4-7-20260520")
            .expect("request should build");

        assert_eq!(
            request.body["messages"][0]["content"][0]["type"],
            "thinking"
        );
        assert_eq!(
            request.body["messages"][0]["content"][0]["signature"],
            "signed-context"
        );
        assert_eq!(
            request.body["messages"][0]["content"][1]["type"],
            "tool_use"
        );
    }

    #[test]
    fn opus_4_8_uses_adaptive_thinking_profile() {
        let mut params = base_params(vec![LlmMessage {
            origin: Default::default(),
            role: "user".to_string(),
            content: LlmMessageContent::Text("hi".to_string()),
        }]);
        params.reasoning_effort = Some(ReasoningEffort::High);
        let adapted = AnthropicMessagesAdapter
            .build_request(&params, "claude-opus-4-8-20260815")
            .expect("build");
        assert_eq!(adapted.body["thinking"]["type"], "adaptive");
        assert_eq!(adapted.body["output_config"]["effort"], "high");
        assert!(adapted.body["thinking"].get("budget_tokens").is_none());
    }

    #[test]
    fn explicit_output_limit_is_never_raised_to_the_thinking_default() {
        let mut params = base_params(vec![LlmMessage {
            origin: Default::default(),
            role: "user".into(),
            content: LlmMessageContent::Text("bounded".into()),
        }]);
        params.reasoning_effort = Some(ReasoningEffort::High);
        params.max_tokens = Some(1024);
        assert!(
            AnthropicMessagesAdapter
                .build_request(&params, "claude-opus-4-5-20250930")
                .unwrap_err()
                .contains("executor_output_limit_incompatible")
        );
        params.max_tokens = Some(17_000);
        let request = AnthropicMessagesAdapter
            .build_request(&params, "claude-opus-4-5-20250930")
            .unwrap();
        assert_eq!(request.body["max_tokens"], 17_000);
        assert_eq!(request.body["thinking"]["budget_tokens"], 16_384);
        params.max_tokens = Some(1024);
        let request = AnthropicMessagesAdapter
            .build_request(&params, "claude-opus-4-8-20260815")
            .unwrap();
        assert_eq!(request.body["max_tokens"], 1024);
    }

    #[test]
    fn legacy_claude_uses_budget_tokens_from_effort() {
        let mut params = base_params(vec![LlmMessage {
            origin: Default::default(),
            role: "user".to_string(),
            content: LlmMessageContent::Text("hi".to_string()),
        }]);
        params.reasoning_effort = Some(ReasoningEffort::High);
        let adapted = AnthropicMessagesAdapter
            .build_request(&params, "claude-opus-4-5-20250930")
            .expect("build");
        assert_eq!(adapted.body["thinking"]["type"], "enabled");
        assert_eq!(adapted.body["thinking"]["budget_tokens"], 16384);
        assert!(adapted.body["thinking"].get("effort").is_none());
        assert!(adapted.body["thinking"].get("display").is_none());
        let max_tokens = adapted.body["max_tokens"].as_u64().expect("max_tokens");
        assert!(max_tokens > 16384);
        // legacy 模型不带 beta header
        assert!(
            !adapted
                .extra_headers
                .iter()
                .any(|(n, _)| n == "anthropic-beta")
        );
    }

    #[test]
    fn unknown_model_omits_thinking_even_with_configured_effort() {
        let mut params = base_params(vec![LlmMessage {
            origin: Default::default(),
            role: "user".to_string(),
            content: LlmMessageContent::Text("hi".to_string()),
        }]);
        params.reasoning_effort = Some(ReasoningEffort::High);
        let adapted = AnthropicMessagesAdapter
            .build_request(&params, "unknown-claude-variant")
            .expect("build");
        assert!(
            adapted.body.get("thinking").is_none(),
            "未知模型不得注入 thinking 字段"
        );
    }

    #[test]
    fn truncates_multibyte_error_body_without_panicking() {
        let body = "错".repeat(300);
        let error = AnthropicMessagesAdapter
            .parse_response(500, &body)
            .expect_err("非成功响应必须返回协议错误");
        assert!(error.contains("Anthropic API error"));
        assert!(error.ends_with("..."));
    }
}
