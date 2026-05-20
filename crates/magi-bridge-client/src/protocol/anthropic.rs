use serde_json::{Value, json};

use super::adapter::{AdaptedRequest, AdaptedResponse, ProviderAdapter, ProviderFamily};
use super::capability::supports_extended_thinking;
use super::utils::{
    convert_messages_to_anthropic, parse_anthropic_usage, serialize_anthropic_tool_definitions,
};
use crate::llm_types::{LlmMessageParams, ToolCall};
use magi_usage_authority::ReasoningEffort;

/// Anthropic Extended Thinking 默认预算（tokens）。
///
/// 仅在调用方未显式指定 `reasoning_effort` 时使用。Anthropic 要求 `max_tokens > budget_tokens`。
/// 这里取一个中等值，足够大多数推理场景，同时保证 `max_tokens.unwrap_or(4096)` 仍有可用余量。
const DEFAULT_THINKING_BUDGET_TOKENS: u32 = 4096;
/// 启用 thinking 时 `max_tokens` 的最低值（必须严格大于 budget）。
const MIN_MAX_TOKENS_WITH_THINKING: u32 = 8192;

/// 将 `ReasoningEffort` 映射为 Anthropic `thinking.budget_tokens` 数值。
///
/// 上限对齐官方文档「thinking budget」可用区间：
/// - Low: 1024 — 轻量推理；
/// - Medium: 4096 — 与默认值一致；
/// - High: 16384 — 复杂任务；
/// - Xhigh: 32768 — 极限推理预算。
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

        // 触发 thinking 的两条路径：
        //   1) 调用方显式配置了 `reasoning_effort`（用户在 UI 显式选了等级，必须强制启用）；
        //   2) 模型能力声明支持 extended thinking（fallback 默认开关）。
        // 这里采用 OR 语义：只要任一条件成立就启用 thinking，确保配置永远生效。
        let configured_effort = params.reasoning_effort;
        let thinking_enabled = configured_effort.is_some() || supports_extended_thinking(model);

        let budget_tokens = configured_effort
            .map(reasoning_effort_budget_tokens)
            .unwrap_or(DEFAULT_THINKING_BUDGET_TOKENS);

        let min_max_tokens_with_thinking = budget_tokens.saturating_add(4096);

        let max_tokens = if thinking_enabled {
            params
                .max_tokens
                .unwrap_or(min_max_tokens_with_thinking.max(MIN_MAX_TOKENS_WITH_THINKING))
                .max(min_max_tokens_with_thinking.max(MIN_MAX_TOKENS_WITH_THINKING))
        } else {
            params.max_tokens.unwrap_or(4096)
        };

        let mut body = json!({
            "model": model,
            "messages": messages,
            "max_tokens": max_tokens,
        });

        if !system_text.is_empty() {
            body["system"] = json!(system_text);
        }

        if thinking_enabled {
            body["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": budget_tokens,
            });
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

        let extra_headers = vec![("anthropic-version".to_string(), "2023-06-01".to_string())];

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

fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.len() <= max {
        trimmed.to_string()
    } else {
        format!("{}...", &trimmed[..max])
    }
}
