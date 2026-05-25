use serde_json::{Value, json};

use super::adapter::{AdaptedRequest, AdaptedResponse, ProviderAdapter, ProviderFamily};
use super::capability::{ThinkingKind, resolve_capability_profile};
use super::utils::{
    convert_messages_to_anthropic, parse_anthropic_usage, reasoning_effort_label,
    serialize_anthropic_tool_definitions,
};
use crate::cache_boundary::split_at_cache_boundary;
use crate::llm_types::{LlmMessageParams, ToolCall};
use magi_usage_authority::ReasoningEffort;

/// Anthropic Extended Thinking 默认预算（tokens）。
///
/// 仅在 `ThinkingKind::BudgetTokens` 形态、且能力表未提供 `default_budget_tokens`、
/// 且调用方未传 `reasoning_effort` 时使用。Anthropic 要求 `max_tokens > budget_tokens`。
const FALLBACK_THINKING_BUDGET_TOKENS: u32 = 4096;
/// 启用 thinking 时 `max_tokens` 的最低值（必须严格大于 budget）。
const MIN_MAX_TOKENS_WITH_THINKING: u32 = 8192;

/// 将 `ReasoningEffort` 映射为 Anthropic `thinking.budget_tokens` 数值。
///
/// 仅在 `ThinkingKind::BudgetTokens` 形态下使用（legacy Claude 3.7 / 4.x 系列）。
/// Opus 4.7+ 的 Adaptive Thinking 走 `thinking.effort` 字符串路径，不经过该函数。
fn reasoning_effort_budget_tokens(effort: ReasoningEffort) -> u32 {
    match effort {
        ReasoningEffort::Low => 1024,
        ReasoningEffort::Medium => 4096,
        ReasoningEffort::High => 16384,
        ReasoningEffort::Xhigh => 32768,
    }
}

pub struct AnthropicMessagesAdapter;

impl ProviderAdapter for AnthropicMessagesAdapter {
    fn family(&self) -> ProviderFamily {
        ProviderFamily::Anthropic
    }

    fn build_request(
        &self,
        params: &LlmMessageParams,
        model: &str,
    ) -> Result<AdaptedRequest, String> {
        let (system_messages, non_system): (Vec<_>, Vec<_>) =
            params.messages.iter().partition(|m| m.role == "system");

        let system_text = if let Some(ref sp) = params.system_prompt {
            sp.clone()
        } else {
            system_messages
                .iter()
                .filter_map(|m| match &m.content {
                    crate::llm_types::LlmMessageContent::Text(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let non_system_owned: Vec<_> = non_system.into_iter().cloned().collect();
        let messages = convert_messages_to_anthropic(&non_system_owned);

        // thinking / beta header / temperature 全部由能力表驱动；
        // 不再保留"配置覆盖能力"的 OR 语义——未知 / 不支持 thinking 的模型
        // 即便调用方传了 reasoning_effort 也绝不写入 thinking 字段，
        // 避免向后端注入会触发 400 的可选字段。
        let capability = resolve_capability_profile(model);
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

        let max_tokens = match (thinking_value.as_ref(), thinking_budget_floor) {
            (Some(_), Some(budget)) => {
                let floor = budget
                    .saturating_add(4096)
                    .max(MIN_MAX_TOKENS_WITH_THINKING);
                params.max_tokens.unwrap_or(floor).max(floor)
            }
            (Some(_), None) => params
                .max_tokens
                .unwrap_or(MIN_MAX_TOKENS_WITH_THINKING)
                .max(MIN_MAX_TOKENS_WITH_THINKING),
            (None, _) => params.max_tokens.unwrap_or(4096),
        };

        let mut body = json!({
            "model": model,
            "messages": messages,
            "max_tokens": max_tokens,
        });

        if !system_text.is_empty() {
            body["system"] = build_system_field(&system_text);
        }

        if let Some(thinking) = thinking_value {
            body["thinking"] = thinking;
            // Anthropic 约束：thinking 启用时 temperature 必须 = 1。
            body["temperature"] = json!(1);
        } else if let Some(temperature) = params.temperature {
            body["temperature"] = json!(temperature);
        }

        if let Some(ref tools) = params.tools {
            if !tools.is_empty() {
                body["tools"] = json!(serialize_anthropic_tool_definitions(tools));
                if let Some(ref tc) = params.tool_choice {
                    body["tool_choice"] =
                        serde_json::to_value(tc).unwrap_or(json!({"type": "auto"}));
                }
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
        })
    }

    fn max_output_tokens_field(&self) -> &str {
        "max_tokens"
    }
}

/// 根据能力表 `thinking_kind` 字段构造 Anthropic `thinking` JSON 载荷。
///
/// - `None`：模型不支持 thinking，返回 `None`，主体不写 `thinking` 字段；
/// - `BudgetTokens`（3.7 / 4.x legacy）：写 `{ type:"enabled", budget_tokens }`，
///   优先用调用方 `reasoning_effort` 映射的预算，否则用能力表 default，再否则用全局 fallback；
/// - `Effort`（4.7+ Adaptive Thinking only mode）：写 `{ type:"enabled", effort:"low|medium|high|xhigh" }`，
///   未配置时默认 `medium`，并尊重 `supports_thinking_display` 追加 `display:"summarized"`；
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
                        FALLBACK_THINKING_BUDGET_TOKENS
                    }
                });
            Some(attach_display(json!({
                "type": "enabled",
                "budget_tokens": budget,
            })))
        }
        ThinkingKind::Effort => {
            let label = configured_effort
                .map(reasoning_effort_label)
                .unwrap_or("medium");
            Some(attach_display(json!({
                "type": "enabled",
                "effort": label,
            })))
        }
        ThinkingKind::Adaptive => Some(attach_display(json!({ "type": "adaptive" }))),
    }
}

fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.len() <= max {
        trimmed.to_string()
    } else {
        format!("{}...", &trimmed[..max])
    }
}

/// 根据 system_text 中是否含 [`PROMPT_CACHE_BOUNDARY`] 标记，决定 `system`
/// 字段的形态：
///
/// - 含标记：切成 `[static_prefix, dynamic_suffix]` 两段 content blocks，
///   静态前缀打 `cache_control: {type: ephemeral}`，命中后输入计费按 1/10。
/// - 不含标记 / 任一段为空：回退到原有单 string 形态，保持向后兼容
///   （没有提示词侧 boundary 的旧调用方不受影响）。
///
/// [`PROMPT_CACHE_BOUNDARY`]: crate::cache_boundary::PROMPT_CACHE_BOUNDARY
fn build_system_field(system_text: &str) -> Value {
    let Some((static_part, dynamic_part)) = split_at_cache_boundary(system_text) else {
        return json!(system_text);
    };
    let static_trimmed = static_part.trim();
    let dynamic_trimmed = dynamic_part.trim();
    // 任一段为空意味着 boundary 出现在最前或最后——退化场景，不值得
    // 引入 cache breakpoint，回退到去掉标记后的单 string。
    if static_trimmed.is_empty() || dynamic_trimmed.is_empty() {
        let cleaned = system_text.replace(crate::cache_boundary::PROMPT_CACHE_BOUNDARY, "");
        return json!(cleaned);
    }
    json!([
        {
            "type": "text",
            "text": static_trimmed,
            "cache_control": {"type": "ephemeral"}
        },
        {
            "type": "text",
            "text": dynamic_trimmed
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_boundary::PROMPT_CACHE_BOUNDARY;
    use crate::llm_types::{LlmMessage, LlmMessageContent, LlmMessageParams};

    fn base_params(messages: Vec<LlmMessage>) -> LlmMessageParams {
        LlmMessageParams {
            messages,
            max_tokens: None,
            temperature: None,
            tools: None,
            stream: None,
            system_prompt: None,
            tool_choice: None,
            timeout_ms: None,
            stream_idle_timeout_ms: None,
            stream_hard_timeout_ms: None,
            retry_policy: None,
            reasoning_effort: None,
        }
    }

    #[test]
    fn build_system_field_returns_plain_string_without_boundary() {
        let value = build_system_field("plain system prompt");
        assert_eq!(value, json!("plain system prompt"));
    }

    #[test]
    fn build_system_field_returns_blocks_array_with_boundary() {
        let text = format!("STATIC PREFIX{}DYNAMIC SUFFIX", PROMPT_CACHE_BOUNDARY);
        let value = build_system_field(&text);
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
        let text = format!("{}DYNAMIC ONLY", PROMPT_CACHE_BOUNDARY);
        let value = build_system_field(&text);
        assert_eq!(value, json!("DYNAMIC ONLY"));
    }

    #[test]
    fn build_system_field_degenerates_when_dynamic_part_empty() {
        let text = format!("STATIC ONLY{}", PROMPT_CACHE_BOUNDARY);
        let value = build_system_field(&text);
        assert_eq!(value, json!("STATIC ONLY"));
    }

    #[test]
    fn opus_4_7_emits_effort_thinking_with_display_and_beta_header() {
        let mut params = base_params(vec![LlmMessage {
            role: "user".to_string(),
            content: LlmMessageContent::Text("hi".to_string()),
        }]);
        params.reasoning_effort = Some(ReasoningEffort::Xhigh);
        let adapted = AnthropicMessagesAdapter
            .build_request(&params, "claude-opus-4-7-20260520")
            .expect("build");
        assert_eq!(adapted.body["thinking"]["type"], "enabled");
        assert_eq!(adapted.body["thinking"]["effort"], "xhigh");
        assert_eq!(adapted.body["thinking"]["display"], "summarized");
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
            role: "user".to_string(),
            content: LlmMessageContent::Text("hi".to_string()),
        }]);
        let adapted = AnthropicMessagesAdapter
            .build_request(&params, "claude-opus-4-7-20260520")
            .expect("build");
        assert_eq!(adapted.body["thinking"]["effort"], "medium");
    }

    #[test]
    fn legacy_claude_uses_budget_tokens_from_effort() {
        let mut params = base_params(vec![LlmMessage {
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
}
