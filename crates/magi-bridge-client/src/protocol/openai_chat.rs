use serde_json::{Value, json};

use super::adapter::{AdaptedRequest, AdaptedResponse, ProviderAdapter, ProviderFamily};
use super::capability::resolve_capability_profile;
use super::utils::{
    convert_messages_to_openai, parse_openai_usage, reasoning_effort_label,
    serialize_tool_definitions,
};
use crate::cache_boundary::PROMPT_CACHE_BOUNDARY;
use crate::llm_types::{
    LlmMessageContent, LlmMessageParams, ToolCall, ToolChoice, parse_tool_arguments,
};

pub struct OpenAiChatCompletionsAdapter;

impl ProviderAdapter for OpenAiChatCompletionsAdapter {
    fn family(&self) -> ProviderFamily {
        ProviderFamily::OpenAiChat
    }

    fn build_request(
        &self,
        params: &LlmMessageParams,
        model: &str,
    ) -> Result<AdaptedRequest, String> {
        // PROMPT_CACHE_BOUNDARY 标记仅服务于 Anthropic content-block 切分，
        // 对 ChatCompletions 协议没有语义；在喂进 convert_messages_to_openai
        // 之前过滤掉，避免泄漏到 OpenAI 兼容模型的 prompt。
        let filtered_messages: Vec<crate::llm_types::LlmMessage> = params
            .messages
            .iter()
            .filter(|m| !is_cache_boundary_marker(&m.content))
            .cloned()
            .collect();
        let messages = convert_messages_to_openai(&filtered_messages);

        let mut body = json!({
            "model": model,
            "messages": messages,
        });

        if let Some(max_tokens) = params.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }
        if let Some(temperature) = params.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(ref tools) = params.tools
            && !tools.is_empty()
        {
            body["tools"] = json!(serialize_tool_definitions(tools));
            let capability = resolve_capability_profile(model);
            body["tool_choice"] = if capability.supports_forced_tool_choice {
                translate_tool_choice_for_openai(params.tool_choice.as_ref())
            } else {
                json!("auto")
            };
        }
        if let Some(stream) = params.stream {
            body["stream"] = json!(stream);
            if stream {
                body["stream_options"] = json!({"include_usage": true});
            }
        }

        // reasoning_effort 顶层字段仅在能力表声明的模型上注入；未声明的模型
        // （包括未知模型）即便调用方传了 effort 也不写入——盲发可能触发 400。
        let capability = resolve_capability_profile(model);
        if capability.supports_openai_reasoning_effort
            && let Some(effort) = params.reasoning_effort
        {
            body["reasoning_effort"] = json!(reasoning_effort_label(effort));
        }

        let mut extra_headers = Vec::new();
        for (name, value) in capability.beta_headers {
            extra_headers.push((name.to_string(), value.to_string()));
        }

        Ok(AdaptedRequest {
            url_path: "/v1/chat/completions".to_string(),
            body,
            extra_headers,
        })
    }

    fn parse_response(&self, status: u16, body: &str) -> Result<AdaptedResponse, String> {
        if !(200..300).contains(&status) {
            return Err(format!(
                "OpenAI API error (status={}): {}",
                status,
                truncate(body, 512)
            ));
        }

        let envelope: Value =
            serde_json::from_str(body).map_err(|e| format!("invalid JSON response: {e}"))?;

        let choices = envelope["choices"]
            .as_array()
            .ok_or("missing choices array")?;

        if choices.is_empty() {
            return Err("empty choices array".to_string());
        }

        let choice = &choices[0];
        let message = &choice["message"];
        let finish_reason = choice["finish_reason"]
            .as_str()
            .unwrap_or("stop")
            .to_string();

        let content = message["content"]
            .as_str()
            .map(|s| s.to_string())
            .or_else(|| message["refusal"].as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        let thinking = message["reasoning_content"]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned);

        let tool_calls = parse_openai_tool_calls(&message["tool_calls"]);

        let usage = envelope
            .get("usage")
            .map(parse_openai_usage)
            .unwrap_or_default();

        Ok(AdaptedResponse {
            content,
            thinking,
            tool_calls,
            usage,
            stop_reason: finish_reason,
            raw: Some(envelope),
        })
    }
}

fn is_cache_boundary_marker(content: &LlmMessageContent) -> bool {
    match content {
        LlmMessageContent::Text(text) => text.as_str() == PROMPT_CACHE_BOUNDARY,
        LlmMessageContent::Blocks(_) => false,
    }
}

/// 把统一的 `ToolChoice` enum 翻译成 OpenAI Chat Completions 协议形态。
///
/// - `ToolChoice::Simple(s)`（"auto" / "none" / "required"）：原样作为字符串透传；
/// - `ToolChoice::Typed { kind, name: Some(n) }`：OpenAI 强制工具调用是
///   `{"type":"function","function":{"name":n}}` 形态；适配器在此处翻译，
///   不让 OpenAI 调用路径泄漏 Anthropic 风格 `{"type":"tool","name":...}`；
/// - `ToolChoice::Typed { kind, name: None }`：退回到字符串字面量 `kind`；
/// - `None`：默认 `"auto"`。
fn translate_tool_choice_for_openai(choice: Option<&ToolChoice>) -> Value {
    match choice {
        None => json!("auto"),
        Some(ToolChoice::Simple(label)) => json!(label),
        Some(ToolChoice::Typed { kind, name }) => match name {
            Some(name) => json!({
                "type": "function",
                "function": {"name": name},
            }),
            None => json!(kind),
        },
    }
}

fn parse_openai_tool_calls(value: &Value) -> Vec<ToolCall> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|tc| {
            let id = tc["id"].as_str()?.to_string();
            let func = &tc["function"];
            let name = func["name"].as_str()?.to_string();
            let args_raw = match &func["arguments"] {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let (arguments, argument_parse_error) = parse_tool_arguments(&args_raw);
            Some(ToolCall {
                id,
                name,
                arguments,
                argument_parse_error,
                raw_arguments: Some(args_raw),
            })
        })
        .collect()
}

fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.len() <= max {
        trimmed.to_string()
    } else {
        format!("{}...", &trimmed[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            reasoning_effort: None,
        }
    }

    #[test]
    fn cache_boundary_marker_message_is_stripped() {
        let params = base_params(vec![
            LlmMessage {
                role: "system".to_string(),
                content: LlmMessageContent::Text("static prompt".to_string()),
            },
            LlmMessage {
                role: "system".to_string(),
                content: LlmMessageContent::Text(PROMPT_CACHE_BOUNDARY.to_string()),
            },
            LlmMessage {
                role: "system".to_string(),
                content: LlmMessageContent::Text("dynamic prompt".to_string()),
            },
        ]);
        let adapted = OpenAiChatCompletionsAdapter
            .build_request(&params, "gpt-4o")
            .expect("build");
        let messages = adapted.body["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["content"], "static prompt");
        assert_eq!(messages[1]["content"], "dynamic prompt");
        let text = adapted.body.to_string();
        assert!(!text.contains(PROMPT_CACHE_BOUNDARY));
    }

    #[test]
    fn reasoning_effort_only_emitted_on_capability_models() {
        let mut params = base_params(vec![LlmMessage {
            role: "user".to_string(),
            content: LlmMessageContent::Text("hi".to_string()),
        }]);
        params.reasoning_effort = Some(magi_usage_authority::ReasoningEffort::Xhigh);

        // 能力表声明支持 → 写入
        let adapted = OpenAiChatCompletionsAdapter
            .build_request(&params, "gpt-5-turbo")
            .expect("build");
        assert_eq!(adapted.body["reasoning_effort"], "xhigh");

        // 未知模型 → 不得写入
        let adapted2 = OpenAiChatCompletionsAdapter
            .build_request(&params, "qwen-max")
            .expect("build");
        assert!(adapted2.body.get("reasoning_effort").is_none());
    }

    #[test]
    fn tool_choice_typed_translates_to_openai_shape() {
        let mut params = base_params(vec![LlmMessage {
            role: "user".to_string(),
            content: LlmMessageContent::Text("classify".to_string()),
        }]);
        params.tools = Some(vec![crate::llm_types::ToolDefinition {
            name: "classify".to_string(),
            description: "test".to_string(),
            input_schema: crate::llm_types::ToolInputSchema {
                kind: "object".to_string(),
                properties: json!({}),
                required: None,
            },
        }]);
        params.tool_choice = Some(ToolChoice::Typed {
            kind: "tool".to_string(),
            name: Some("classify".to_string()),
        });
        let adapted = OpenAiChatCompletionsAdapter
            .build_request(&params, "gpt-4o")
            .expect("build");
        assert_eq!(adapted.body["tool_choice"]["type"], "function");
        assert_eq!(adapted.body["tool_choice"]["function"]["name"], "classify");
    }

    #[test]
    fn deepseek_v4_uses_automatic_tool_choice_in_thinking_mode() {
        let mut params = base_params(vec![LlmMessage {
            role: "user".to_string(),
            content: LlmMessageContent::Text("执行工具".to_string()),
        }]);
        params.tools = Some(vec![crate::llm_types::ToolDefinition {
            name: "shell_exec".to_string(),
            description: "执行命令".to_string(),
            input_schema: crate::llm_types::ToolInputSchema {
                kind: "object".to_string(),
                properties: json!({}),
                required: None,
            },
        }]);
        params.tool_choice = Some(ToolChoice::Typed {
            kind: "tool".to_string(),
            name: Some("shell_exec".to_string()),
        });

        let adapted = OpenAiChatCompletionsAdapter
            .build_request(&params, "DeepSeek-V4-Flash")
            .expect("build");
        assert_eq!(adapted.body["tool_choice"], "auto");
    }

    #[test]
    fn streaming_sets_include_usage() {
        let mut params = base_params(vec![LlmMessage {
            role: "user".to_string(),
            content: LlmMessageContent::Text("hi".to_string()),
        }]);
        params.stream = Some(true);
        let adapted = OpenAiChatCompletionsAdapter
            .build_request(&params, "gpt-4o")
            .expect("build");
        assert_eq!(adapted.body["stream"], true);
        assert_eq!(adapted.body["stream_options"]["include_usage"], true);
    }
}
