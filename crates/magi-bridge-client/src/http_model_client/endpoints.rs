use super::*;

pub(super) fn read_non_empty_env(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
pub(super) fn build_anthropic_messages_url_for_test(base_url: &str) -> Result<String, String> {
    let request = AnthropicMessagesAdapter
        .build_request(
            &PromptIr {
                prompt_cache_key: None,

                reasoning_capability: None,
                messages: vec![LlmMessage {
                    origin: Default::default(),
                    role: "user".to_string(),
                    content: LlmMessageContent::Text("ping".to_string()),
                }],
                max_tokens: None,
                temperature: None,
                stop: vec![],
                tools: None,
                stream: None,

                tool_choice: None,
                reasoning_effort: None,
            },
            "claude-test",
        )
        .expect("anthropic adapter should build test request");
    build_protocol_endpoint_url(base_url, &request.url_path)
}

pub(super) fn build_protocol_endpoint_url(
    base_url: &str,
    endpoint_path: &str,
) -> Result<String, String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err("base_url must not be empty".to_string());
    }
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err("base_url must start with http:// or https://".to_string());
    }

    let normalized = trimmed.trim_end_matches('/');
    let endpoint_path = endpoint_path.trim();
    let endpoint_suffix = endpoint_path
        .trim_matches('/')
        .strip_prefix("v1/")
        .unwrap_or_else(|| endpoint_path.trim_matches('/'))
        .trim_matches('/');
    let endpoint_suffix = (!endpoint_suffix.is_empty())
        .then_some(endpoint_suffix)
        .ok_or_else(|| "endpoint_path must include an endpoint leaf".to_string())?;
    if normalized.ends_with(endpoint_path) || normalized.ends_with(&format!("/{endpoint_suffix}")) {
        return Ok(normalized.to_string());
    }
    if normalized.ends_with("/v1") {
        return Ok(format!("{normalized}/{endpoint_suffix}"));
    }

    Ok(format!("{normalized}{endpoint_path}"))
}

/// 安全地将字节切片解码为 UTF-8 字符串，不丢弃尾部不完整的多字节序列。
/// 返回 (已解码的合法字符串, 已消费的字节数)。
/// 尾部不完整的字节（1-3 字节）由调用方保留，等待下次读取后拼接。
pub(super) fn decode_utf8_safe(bytes: &[u8]) -> (String, usize) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), bytes.len()),
        Err(e) => {
            let valid_up_to = e.valid_up_to();
            if valid_up_to == 0 {
                // 全部都是不完整的字节，等待下一次读取
                return (String::new(), 0);
            }
            // SAFETY: from_utf8 已验证 bytes[..valid_up_to] 是合法 UTF-8
            let valid_str = unsafe { std::str::from_utf8_unchecked(&bytes[..valid_up_to]) };
            (valid_str.to_string(), valid_up_to)
        }
    }
}

pub(super) fn truncate_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "<empty>".to_string();
    }
    const MAX_CHARS: usize = 512;
    let mut collected: String = trimmed.chars().take(MAX_CHARS).collect();
    if trimmed.chars().count() > MAX_CHARS {
        collected.push_str("...");
    }
    collected
}

pub(super) fn provider_error_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    match value.get("error")? {
        Value::String(message) => {
            Some(message.trim().to_string()).filter(|value| !value.is_empty())
        }
        Value::Object(error) => error
            .get("message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        _ => None,
    }
}

pub(super) fn provider_non_sse_stream_message(body: &str) -> String {
    if let Some(message) = provider_error_message(body) {
        return format!("provider rejected request: {message}");
    }
    format!(
        "provider response invalid: expected event stream, body={}",
        truncate_body(body)
    )
}

pub(super) fn provider_error_retry_after(error: &BridgeClientError) -> Option<Duration> {
    match error {
        BridgeClientError::HttpStatusFailed { retry_after, .. } => *retry_after,
        _ => None,
    }
}

pub(super) fn provider_http_status_error(
    status: u16,
    response_body: &str,
    retry_after: Option<Duration>,
) -> BridgeClientError {
    let message = if let Some(message) = provider_error_message(response_body) {
        format!("provider rejected request: {message}")
    } else {
        format!(
            "provider rejected request: body={}",
            truncate_body(response_body)
        )
    };
    BridgeClientError::HttpStatusFailed {
        layer: BridgeErrorLayer::RemoteBusiness,
        code: Some(-32006),
        http_status: status,
        message,
        retry_after,
    }
}

pub(super) fn is_forced_tool_choice_rejection(
    status: u16,
    response_body: &str,
    request_body: &Value,
    provider_family: ProviderFamily,
) -> bool {
    status == 400
        && is_forced_tool_choice_request(request_body, provider_family)
        && is_forced_tool_choice_rejection_message(response_body, true)
}

pub(super) fn is_forced_tool_choice_rejection_error(
    error: &BridgeClientError,
    request_body: &Value,
    provider_family: ProviderFamily,
) -> bool {
    is_forced_tool_choice_request(request_body, provider_family)
        && is_forced_tool_choice_rejection_message(
            &error.to_string(),
            error.http_status() == Some(400),
        )
}

pub(super) fn is_forced_tool_choice_request(
    request_body: &Value,
    provider_family: ProviderFamily,
) -> bool {
    !matches!(
        tool_choice_enforcement(request_body, provider_family),
        ToolChoiceEnforcement::Automatic
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToolChoiceEnforcement {
    Exact,
    Required,
    Automatic,
}

pub(super) fn tool_choice_enforcement(
    request_body: &Value,
    provider_family: ProviderFamily,
) -> ToolChoiceEnforcement {
    match provider_family {
        ProviderFamily::OpenAiChat if request_body["tool_choice"].is_object() => {
            ToolChoiceEnforcement::Exact
        }
        ProviderFamily::OpenAiChat
            if request_body["tool_choice"]
                .as_str()
                .is_some_and(|choice| choice == "required") =>
        {
            ToolChoiceEnforcement::Required
        }
        ProviderFamily::OpenAiResponses if request_body["tool_choice"].is_object() => {
            ToolChoiceEnforcement::Exact
        }
        ProviderFamily::OpenAiResponses
            if request_body["tool_choice"]
                .as_str()
                .is_some_and(|choice| choice == "required") =>
        {
            ToolChoiceEnforcement::Required
        }
        ProviderFamily::Anthropic
            if request_body["tool_choice"]["type"]
                .as_str()
                .is_some_and(|kind| kind == "tool") =>
        {
            ToolChoiceEnforcement::Exact
        }
        ProviderFamily::Anthropic
            if request_body["tool_choice"]["type"]
                .as_str()
                .is_some_and(|kind| kind == "any") =>
        {
            ToolChoiceEnforcement::Required
        }
        _ => ToolChoiceEnforcement::Automatic,
    }
}

pub(super) fn is_forced_tool_choice_rejection_message(
    message: &str,
    allow_opaque_gateway_error: bool,
) -> bool {
    let normalized = message.to_ascii_lowercase().replace('_', " ");
    // 部分 OpenAI 兼容网关会吞掉上游的 tool_choice 细节，只保留这一泛化错误。
    // 仅在当前请求确实使用了强制工具选择时，调用方才会把它用于能力协商。
    let opaque_upstream_rejection = normalized.contains("upstream request failed");
    let mentions_forced_choice = normalized.contains("tool choice")
        || normalized.contains("tool selection")
        || normalized.contains("function calling");
    let rejection = normalized.contains("not support")
        || normalized.contains("unsupported")
        || normalized.contains("does not allow")
        || normalized.contains("not available")
        || normalized.contains("invalid")
        || normalized.contains("rejected")
        || normalized.contains("only supports")
        || normalized.contains("supports only")
        || normalized.contains("cannot")
        || normalized.contains("can't")
        || normalized.contains("must be")
        || normalized.contains("must use");
    (allow_opaque_gateway_error && opaque_upstream_rejection)
        || (mentions_forced_choice && rejection)
}

pub(super) fn request_with_next_tool_choice_enforcement(
    request: &HttpModelRequest,
    provider_family: ProviderFamily,
) -> Option<(ForcedToolChoiceCapability, HttpModelRequest)> {
    let mut fallback = request.clone();
    match tool_choice_enforcement(&request.body, provider_family) {
        ToolChoiceEnforcement::Exact => {
            fallback.body["tool_choice"] = match provider_family {
                ProviderFamily::OpenAiChat | ProviderFamily::OpenAiResponses => json!("required"),
                ProviderFamily::Anthropic => json!({"type": "any"}),
            };
            Some((ForcedToolChoiceCapability::Required, fallback))
        }
        ToolChoiceEnforcement::Required => {
            fallback.body["tool_choice"] = match provider_family {
                ProviderFamily::OpenAiChat | ProviderFamily::OpenAiResponses => json!("auto"),
                ProviderFamily::Anthropic => json!({"type": "auto"}),
            };
            Some((ForcedToolChoiceCapability::AutoOnly, fallback))
        }
        ToolChoiceEnforcement::Automatic => None,
    }
}

pub(super) fn execute_with_tool_choice_fallback<F>(
    initial_request: HttpModelRequest,
    provider_family: ProviderFamily,
    mut execute: F,
) -> ToolChoiceFallbackHttpResult
where
    F: FnMut(HttpModelRequest) -> HttpPostResult,
{
    let mut request = initial_request;
    let mut fallback_capability = None;
    loop {
        match execute(request.clone()) {
            Ok((status, body, retry_after))
                if is_forced_tool_choice_rejection(
                    status,
                    &body,
                    &request.body,
                    provider_family,
                ) =>
            {
                let Some((capability, next_request)) =
                    request_with_next_tool_choice_enforcement(&request, provider_family)
                else {
                    return Ok((request, status, body, retry_after, fallback_capability));
                };
                fallback_capability = Some(capability);
                request = next_request;
            }
            Ok(result) => return Ok((request, result.0, result.1, result.2, fallback_capability)),
            Err(error)
                if is_forced_tool_choice_rejection_error(
                    &error,
                    &request.body,
                    provider_family,
                ) =>
            {
                let Some((capability, next_request)) =
                    request_with_next_tool_choice_enforcement(&request, provider_family)
                else {
                    return Err(error);
                };
                fallback_capability = Some(capability);
                request = next_request;
            }
            Err(error) => return Err(error),
        }
    }
}

pub(super) fn capability_from_accepted_tool_choice_request(
    request: &HttpModelRequest,
    provider_family: ProviderFamily,
) -> Option<ForcedToolChoiceCapability> {
    match tool_choice_enforcement(&request.body, provider_family) {
        ToolChoiceEnforcement::Exact => Some(ForcedToolChoiceCapability::Exact),
        ToolChoiceEnforcement::Required => Some(ForcedToolChoiceCapability::Required),
        // 普通自动选择请求不能证明提供方不支持强制语义，不能污染后续需要工具的
        // 同连接调用；只有从 required 明确降级后的请求才会写入 AutoOnly。
        ToolChoiceEnforcement::Automatic => None,
    }
}
