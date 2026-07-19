pub(crate) const PUBLIC_MODEL_INVOCATION_FAILURE_MESSAGE: &str = "模型请求未完成，可直接继续重试。";
pub(crate) const PUBLIC_MODEL_AUTH_FAILURE_MESSAGE: &str =
    "模型认证失败，请检查 API Key 或访问权限。";
pub(crate) const PUBLIC_MODEL_RATE_LIMIT_MESSAGE: &str = "模型服务当前受到限流，请稍后重试。";
pub(crate) const PUBLIC_MODEL_NOT_FOUND_MESSAGE: &str = "模型不可用，请检查模型名称和服务配置。";
pub(crate) const PUBLIC_MODEL_CONTEXT_LIMIT_MESSAGE: &str =
    "当前对话已超过模型上下文长度，请压缩上下文或开启新会话。";
pub(crate) const PUBLIC_MODEL_INVALID_REQUEST_MESSAGE: &str =
    "模型拒绝了当前请求，请检查该模型是否支持当前工具和请求格式。";
pub(crate) const PUBLIC_MODEL_TOOL_UNSUPPORTED_MESSAGE: &str =
    "当前模型拒绝了工具调用请求，请更换支持工具调用的模型或关闭工具后重试。";
pub(crate) const PUBLIC_MODEL_STREAM_INTERRUPTED_MESSAGE: &str = "模型响应流中断，可直接继续重试。";
pub(crate) const PUBLIC_MODEL_TIMEOUT_MESSAGE: &str = "模型响应超时，可直接继续重试。";
pub(crate) const PUBLIC_MODEL_EMPTY_RESPONSE_MESSAGE: &str =
    "模型本轮未返回有效内容，可直接继续重试。";
pub(crate) const PUBLIC_MODEL_IMAGE_INVOCATION_FAILURE_MESSAGE: &str =
    "当前模型暂不支持图片输入，请更换支持图片的模型后重试。";
pub(crate) const PUBLIC_MODEL_INVALID_IMAGE_INPUT_MESSAGE: &str =
    "图片输入无效，请重新选择图片后重试。";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModelInvocationErrorClassification {
    pub code: &'static str,
    pub public_message: &'static str,
}

pub(crate) fn classify_model_invocation_error(
    raw_error: &str,
) -> ModelInvocationErrorClassification {
    let normalized = raw_error.to_ascii_lowercase();
    if contains_context_limit_error(&normalized) {
        return ModelInvocationErrorClassification {
            code: "model_context_limit",
            public_message: PUBLIC_MODEL_CONTEXT_LIMIT_MESSAGE,
        };
    }
    if contains_http_status(&normalized, 401)
        || contains_http_status(&normalized, 403)
        || normalized.contains("invalid api key")
        || normalized.contains("incorrect api key")
        || normalized.contains("unauthorized")
        || normalized.contains("forbidden")
        || normalized.contains("authentication")
    {
        return ModelInvocationErrorClassification {
            code: "model_auth_failed",
            public_message: PUBLIC_MODEL_AUTH_FAILURE_MESSAGE,
        };
    }
    if contains_http_status(&normalized, 429)
        || normalized.contains("rate limit")
        || normalized.contains("too many requests")
    {
        return ModelInvocationErrorClassification {
            code: "model_rate_limited",
            public_message: PUBLIC_MODEL_RATE_LIMIT_MESSAGE,
        };
    }
    if contains_http_status(&normalized, 404)
        || normalized.contains("model not found")
        || normalized.contains("model_not_found")
        || normalized.contains("unknown model")
    {
        return ModelInvocationErrorClassification {
            code: "model_not_found",
            public_message: PUBLIC_MODEL_NOT_FOUND_MESSAGE,
        };
    }
    if contains_tool_unsupported_error(&normalized) {
        return ModelInvocationErrorClassification {
            code: "model_tools_unsupported",
            public_message: PUBLIC_MODEL_TOOL_UNSUPPORTED_MESSAGE,
        };
    }
    if contains_http_status(&normalized, 400) {
        return ModelInvocationErrorClassification {
            code: "model_invalid_request",
            public_message: PUBLIC_MODEL_INVALID_REQUEST_MESSAGE,
        };
    }
    if contains_stream_interruption_error(&normalized) {
        return ModelInvocationErrorClassification {
            code: "model_stream_interrupted",
            public_message: PUBLIC_MODEL_STREAM_INTERRUPTED_MESSAGE,
        };
    }
    if contains_timeout_error(&normalized) {
        return ModelInvocationErrorClassification {
            code: "model_timeout",
            public_message: PUBLIC_MODEL_TIMEOUT_MESSAGE,
        };
    }
    ModelInvocationErrorClassification {
        code: "model_invocation_failed",
        public_message: PUBLIC_MODEL_INVOCATION_FAILURE_MESSAGE,
    }
}

pub(crate) fn extract_model_context_limit(raw_error: &str) -> Option<u64> {
    let normalized = raw_error.to_ascii_lowercase().replace([',', '_'], "");
    const MARKERS: &[&str] = &[
        "maximum context length is ",
        "maximum context length: ",
        "maximum allowed input tokens is ",
        "maximum allowed input is ",
        "max context length is ",
        "maxcontextlength\":",
        "context window is ",
        "context window: ",
        "token limit is ",
        "token limit: ",
    ];
    for marker in MARKERS {
        if let Some(index) = normalized.find(marker)
            && let Some(limit) = first_unsigned_integer(&normalized[index + marker.len()..])
            && (crate::model_context_window::MIN_MODEL_CONTEXT_WINDOW
                ..=crate::model_context_window::MAX_MODEL_CONTEXT_WINDOW)
                .contains(&limit)
        {
            return Some(limit);
        }
    }

    for (index, _) in normalized.match_indices('>') {
        let suffix = &normalized[index + 1..];
        let Some(limit) = first_unsigned_integer(suffix) else {
            continue;
        };
        let qualifier = suffix
            .find(|character: char| !character.is_ascii_digit() && !character.is_whitespace())
            .map(|end| &suffix[..suffix.len().min(end.saturating_add(32))])
            .unwrap_or(suffix);
        if qualifier.contains("maximum")
            && (crate::model_context_window::MIN_MODEL_CONTEXT_WINDOW
                ..=crate::model_context_window::MAX_MODEL_CONTEXT_WINDOW)
                .contains(&limit)
        {
            return Some(limit);
        }
    }
    None
}

fn first_unsigned_integer(value: &str) -> Option<u64> {
    let start = value.find(|character: char| character.is_ascii_digit())?;
    let digits = value[start..]
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>();
    digits.parse().ok()
}

#[cfg(test)]
pub(crate) fn public_model_invocation_error_message(raw_error: &str) -> String {
    classify_model_invocation_error(raw_error)
        .public_message
        .to_string()
}

fn contains_http_status(error: &str, status: u16) -> bool {
    error.contains(&format!("http_status={status}"))
        || error.contains(&format!("http status {status}"))
        || error.contains(&format!("status code {status}"))
}

fn contains_context_limit_error(error: &str) -> bool {
    error.contains("context length")
        || error.contains("context window")
        || error.contains("maximum context")
        || error.contains("maximum allowed input")
        || error.contains("max input length")
        || error.contains("input length limit")
        || error.contains("input token limit")
        || error.contains("input tokens exceeded")
        || error.contains("prompt is too long")
        || error.contains("request too large for model")
        || error.contains("context_length_exceeded")
        || error.contains("too many tokens")
        || error.contains("token limit")
}

fn contains_tool_unsupported_error(error: &str) -> bool {
    (error.contains("tool") || error.contains("function call"))
        && (error.contains("not support")
            || error.contains("unsupported")
            || error.contains("does not allow")
            || error.contains("not available"))
}

fn contains_stream_interruption_error(error: &str) -> bool {
    error.contains("incomplete stream")
        || error.contains("missing terminal")
        || error.contains("stream interrupted")
        || error.contains("stream closed")
        || error.contains("unexpected eof")
}

fn contains_timeout_error(error: &str) -> bool {
    error.contains("timed out") || error.contains("timeout") || error.contains("deadline exceeded")
}

pub(crate) fn public_model_image_invocation_error_message(raw_error: &str) -> String {
    let normalized = raw_error.to_ascii_lowercase();
    if normalized.contains("does not represent a valid image")
        || (normalized.contains("invalid_request_error") && normalized.contains("image"))
        || (normalized.contains("invalid") && normalized.contains("image data"))
    {
        return PUBLIC_MODEL_INVALID_IMAGE_INPUT_MESSAGE.to_string();
    }
    if normalized.contains("empty stream response") || normalized.contains("missing image") {
        return PUBLIC_MODEL_IMAGE_INVOCATION_FAILURE_MESSAGE.to_string();
    }
    PUBLIC_MODEL_INVOCATION_FAILURE_MESSAGE.to_string()
}

pub(crate) fn provider_empty_assistant_response_error(_after_tool_calls: bool) -> String {
    PUBLIC_MODEL_EMPTY_RESPONSE_MESSAGE.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_invocation_errors_use_public_message() {
        assert_eq!(
            provider_empty_assistant_response_error(false),
            PUBLIC_MODEL_EMPTY_RESPONSE_MESSAGE
        );
        assert_eq!(
            provider_empty_assistant_response_error(true),
            PUBLIC_MODEL_EMPTY_RESPONSE_MESSAGE
        );
        assert_eq!(
            public_model_invocation_error_message(
                "桥接调用失败[RemoteBusiness]: provider response invalid: empty stream response"
            ),
            PUBLIC_MODEL_INVOCATION_FAILURE_MESSAGE
        );
    }

    #[test]
    fn model_invocation_errors_classify_provider_failures_without_leaking_body() {
        assert_eq!(
            public_model_invocation_error_message("http_status=401 body=secret-api-key"),
            "模型认证失败，请检查 API Key 或访问权限。"
        );
        assert_eq!(
            public_model_invocation_error_message("http_status=429 body=rate limited"),
            "模型服务当前受到限流，请稍后重试。"
        );
        assert_eq!(
            public_model_invocation_error_message("http_status=404 body=model not found"),
            "模型不可用，请检查模型名称和服务配置。"
        );
        assert_eq!(
            public_model_invocation_error_message(
                "http_status=400 body=context length exceeded for this request"
            ),
            "当前对话已超过模型上下文长度，请压缩上下文或开启新会话。"
        );
        assert!(
            !public_model_invocation_error_message("http_status=401 body=secret-api-key")
                .contains("secret-api-key")
        );
    }

    #[test]
    fn model_invocation_errors_distinguish_request_shape_stream_and_timeout_failures() {
        assert_eq!(
            public_model_invocation_error_message(
                "http_status=400 body={\"error\":{\"message\":\"This model does not support tools\"}}"
            ),
            "当前模型拒绝了工具调用请求，请更换支持工具调用的模型或关闭工具后重试。"
        );
        assert_eq!(
            public_model_invocation_error_message(
                "http_status=400 body={\"error\":{\"message\":\"maximum allowed input tokens exceeded\"}}"
            ),
            PUBLIC_MODEL_CONTEXT_LIMIT_MESSAGE
        );
        assert_eq!(
            public_model_invocation_error_message(
                "provider stream interrupted: missing terminal SSE event"
            ),
            "模型响应流中断，可直接继续重试。"
        );
        assert_eq!(
            public_model_invocation_error_message(
                "provider transport failed: operation timed out after 300 seconds"
            ),
            "模型响应超时，可直接继续重试。"
        );
    }

    #[test]
    fn model_context_limit_extraction_covers_common_provider_messages() {
        assert_eq!(
            extract_model_context_limit(
                "maximum context length is 262144 tokens, however you requested 620000"
            ),
            Some(262_144)
        );
        assert_eq!(
            extract_model_context_limit("prompt is too long: 620000 > 262144 maximum"),
            Some(262_144)
        );
        assert_eq!(
            extract_model_context_limit("body={\"max_context_length\":200000}"),
            Some(200_000)
        );
        assert_eq!(extract_model_context_limit("context length exceeded"), None);
    }

    #[test]
    fn image_model_invocation_errors_use_image_capability_message() {
        assert_eq!(
            public_model_image_invocation_error_message(
                "桥接调用失败[RemoteBusiness]: provider response invalid: empty stream response"
            ),
            PUBLIC_MODEL_IMAGE_INVOCATION_FAILURE_MESSAGE
        );
        assert_eq!(
            public_model_image_invocation_error_message(
                "桥接调用失败[Transport]: provider transport failed"
            ),
            PUBLIC_MODEL_INVOCATION_FAILURE_MESSAGE
        );
    }

    #[test]
    fn image_model_invocation_errors_use_invalid_image_message() {
        assert_eq!(
            public_model_image_invocation_error_message(
                "桥接调用失败[RemoteBusiness]: http_status=400 body={\"error\":{\"message\":\"The image data you provided does not represent a valid image.\",\"type\":\"invalid_request_error\"}}"
            ),
            PUBLIC_MODEL_INVALID_IMAGE_INPUT_MESSAGE
        );
    }
}
