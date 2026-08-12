use magi_core::public_runtime_excerpt;
use serde::Serialize;

pub(crate) const PUBLIC_MODEL_INVOCATION_FAILURE_MESSAGE: &str = "模型服务请求失败。";
pub(crate) const PUBLIC_MODEL_AUTH_FAILURE_MESSAGE: &str =
    "模型认证失败，请检查 API Key 或访问权限。";
pub(crate) const PUBLIC_MODEL_RATE_LIMIT_MESSAGE: &str = "模型服务当前受到限流，请稍后重试。";
pub(crate) const PUBLIC_MODEL_NOT_FOUND_MESSAGE: &str = "模型不可用，请检查模型名称和服务配置。";
pub(crate) const PUBLIC_MODEL_CONTEXT_LIMIT_MESSAGE: &str =
    "当前对话已超过模型上下文长度，请压缩上下文或开启新会话。";
pub(crate) const PUBLIC_MODEL_INVALID_REQUEST_MESSAGE: &str =
    "模型拒绝了当前请求，请检查该模型是否支持当前工具和请求格式。";
pub(crate) const PUBLIC_MODEL_REGION_UNAVAILABLE_MESSAGE: &str =
    "当前模型服务不支持此网络区域，请更换可用的服务节点或模型后重试。";
pub(crate) const PUBLIC_MODEL_TOOL_UNSUPPORTED_MESSAGE: &str =
    "当前模型拒绝了工具调用请求，请更换支持工具调用的模型或关闭工具后重试。";
pub(crate) const PUBLIC_MODEL_STREAM_INTERRUPTED_MESSAGE: &str = "模型响应流在完成前中断。";
pub(crate) const PUBLIC_MODEL_TIMEOUT_MESSAGE: &str = "模型服务在响应完成前超时。";
pub(crate) const PUBLIC_MODEL_EMPTY_RESPONSE_MESSAGE: &str =
    "模型服务返回了空响应，未生成正文或可执行工具调用。";
pub(crate) const PUBLIC_MODEL_OUTPUT_TRUNCATED_MESSAGE: &str =
    "模型输出达到长度上限，本轮响应未完整结束。";
pub(crate) const PUBLIC_MODEL_CONTENT_FILTERED_MESSAGE: &str = "模型服务因内容策略中止了本轮响应。";
pub(crate) const PUBLIC_MODEL_GENERATION_CANCELLED_MESSAGE: &str = "模型服务取消了本轮响应。";
pub(crate) const PUBLIC_MODEL_RESPONSE_INCOMPLETE_MESSAGE: &str = "模型服务未能完整结束本轮响应。";
pub(crate) const PUBLIC_MODEL_TOOL_CALL_MISSING_MESSAGE: &str =
    "模型声明需要执行工具，但没有返回可执行的工具调用。";
/// 传输层已经在未收到任何增量时完成自身重试；这里处理的是已经向用户输出过
/// 片段后缺少终止 SSE 的场景。该场景不能重放同一个请求，只允许有限次数基于片段续写，
/// 随后降级为一次非流式兜底，避免单个 Turn 无限叠加完整模型请求。
pub(crate) const MODEL_STREAM_INTERRUPTION_RECOVERY_MAX_ATTEMPTS: usize = 2;
/// 上游已经完成连接级重试后，任务运行时只在尚未向用户交付任何内容时额外重试一次。
/// 这层监督用于覆盖代理网关返回空流等无法在 HTTP 状态码层判断的暂态故障；一旦有
/// 可见片段，必须走续写恢复，不能重放请求。
pub(crate) const MODEL_PRE_OUTPUT_RECOVERY_MAX_ATTEMPTS: usize = 1;
pub(crate) const MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS: usize = 3;
pub(crate) const MODEL_EMPTY_RESPONSE_RECOVERY_PROMPT: &str = "上一轮模型没有返回用户可见正文或可执行工具调用。请不要只输出 thinking：现在直接输出完整的用户可见答复；如果确实需要工具，请直接调用工具；如果无法完成，请直接说明原因。";
pub(crate) const MODEL_EMPTY_RESPONSE_AFTER_TOOLS_RECOVERY_PROMPT: &str = "前面的工具调用已经完成，工具结果已在上下文中。请不要只输出 thinking，也不要重复已完成的工具调用；现在直接基于现有结果输出完整的用户可见答复。仅在确有缺失信息时调用新的必要工具。";
pub(crate) const PUBLIC_MODEL_IMAGE_INVOCATION_FAILURE_MESSAGE: &str =
    "当前模型暂不支持图片输入，请更换支持图片的模型后重试。";
pub(crate) const PUBLIC_MODEL_INVALID_IMAGE_INPUT_MESSAGE: &str =
    "图片输入无效，请重新选择图片后重试。";

pub const MODEL_FAILURE_SCHEMA_VERSION: &str = "model-failure.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFailureDiagnostic {
    pub schema_version: String,
    pub code: String,
    pub summary: String,
    pub detail: String,
    pub stage: String,
    pub retryable: bool,
    pub retry_attempts: usize,
}

impl ModelFailureDiagnostic {
    pub(crate) fn from_invocation(
        classification: ModelInvocationErrorClassification,
        raw_detail: &str,
        stage: &str,
        retry_attempts: usize,
    ) -> Self {
        let detail = public_runtime_excerpt(raw_detail, 4096);
        Self {
            schema_version: MODEL_FAILURE_SCHEMA_VERSION.to_string(),
            code: classification.code.to_string(),
            summary: classification.public_message.to_string(),
            detail: if detail.trim().is_empty() {
                "模型服务未返回可供诊断的错误详情。".to_string()
            } else {
                detail
            },
            stage: stage.to_string(),
            retryable: model_failure_is_user_retryable(classification.code),
            retry_attempts,
        }
    }

    pub(crate) fn empty_response(
        after_tool_calls: bool,
        retry_attempts: usize,
        response_observation: Option<&str>,
    ) -> Self {
        let mut detail = if after_tool_calls {
            format!(
                "工具调用已完成，但模型请求在连续 {} 次自动恢复后仍未返回用户可见正文或新的可执行工具调用。",
                retry_attempts
            )
        } else {
            format!(
                "模型请求成功结束，但连续 {} 次自动恢复后仍未返回用户可见正文或可执行工具调用。",
                retry_attempts
            )
        };
        if let Some(observation) = response_observation.filter(|value| !value.trim().is_empty()) {
            detail.push_str(" 最后一次响应状态：");
            detail.push_str(observation.trim());
        }
        Self {
            schema_version: MODEL_FAILURE_SCHEMA_VERSION.to_string(),
            code: if after_tool_calls {
                "model_empty_response_after_tools".to_string()
            } else {
                "model_empty_response".to_string()
            },
            summary: PUBLIC_MODEL_EMPTY_RESPONSE_MESSAGE.to_string(),
            detail,
            stage: "response_validation".to_string(),
            retryable: true,
            retry_attempts,
        }
    }

    pub(crate) fn incomplete_response(
        finish_reason: Option<&str>,
        visible_content_chars: usize,
    ) -> Self {
        let finish_reason = finish_reason
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("<missing>");
        let normalized_reason = finish_reason.to_ascii_lowercase();
        let (code, summary, retryable) = match normalized_reason.as_str() {
            "length" | "max_tokens" | "max_output_tokens" => (
                "model_output_truncated",
                PUBLIC_MODEL_OUTPUT_TRUNCATED_MESSAGE,
                true,
            ),
            "content_filter" | "safety" | "blocked" | "recitation" => (
                "model_content_filtered",
                PUBLIC_MODEL_CONTENT_FILTERED_MESSAGE,
                false,
            ),
            "cancelled" | "canceled" => (
                "model_generation_cancelled",
                PUBLIC_MODEL_GENERATION_CANCELLED_MESSAGE,
                true,
            ),
            _ => (
                "model_response_incomplete",
                PUBLIC_MODEL_RESPONSE_INCOMPLETE_MESSAGE,
                true,
            ),
        };
        Self {
            schema_version: MODEL_FAILURE_SCHEMA_VERSION.to_string(),
            code: code.to_string(),
            summary: summary.to_string(),
            detail: format!(
                "模型响应状态为 incomplete；finish_reason={finish_reason}；已接收用户可见正文字符数={visible_content_chars}。"
            ),
            stage: "response_validation".to_string(),
            retryable,
            retry_attempts: 0,
        }
    }

    pub(crate) fn missing_tool_call(finish_reason: Option<&str>) -> Self {
        let finish_reason = finish_reason
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("<missing>");
        Self {
            schema_version: MODEL_FAILURE_SCHEMA_VERSION.to_string(),
            code: "model_tool_call_missing".to_string(),
            summary: PUBLIC_MODEL_TOOL_CALL_MISSING_MESSAGE.to_string(),
            detail: format!(
                "模型响应状态为 requires_tool_execution，finish_reason={finish_reason}，但工具调用数组为空。"
            ),
            stage: "response_validation".to_string(),
            retryable: true,
            retry_attempts: 0,
        }
    }

    pub(crate) fn configuration_unavailable() -> Self {
        Self {
            schema_version: MODEL_FAILURE_SCHEMA_VERSION.to_string(),
            code: "model_configuration_unavailable".to_string(),
            summary: "当前会话没有可用的模型配置。".to_string(),
            detail: "未能从当前设置解析可用的模型客户端。".to_string(),
            stage: "model_configuration".to_string(),
            retryable: false,
            retry_attempts: 0,
        }
    }

    pub(crate) fn image_failure(raw_detail: &str, summary: String, retry_attempts: usize) -> Self {
        let detail = public_runtime_excerpt(raw_detail, 4096);
        let invalid_input = summary == PUBLIC_MODEL_INVALID_IMAGE_INPUT_MESSAGE;
        Self {
            schema_version: MODEL_FAILURE_SCHEMA_VERSION.to_string(),
            code: if invalid_input {
                "model_invalid_image_input".to_string()
            } else {
                "model_image_invocation_failed".to_string()
            },
            summary,
            detail: if detail.trim().is_empty() {
                "模型服务未返回可供诊断的图片请求错误详情。".to_string()
            } else {
                detail
            },
            stage: "request_dispatch".to_string(),
            retryable: !invalid_input,
            retry_attempts,
        }
    }
}

fn model_failure_is_user_retryable(code: &str) -> bool {
    !matches!(
        code,
        "model_auth_failed"
            | "model_context_limit"
            | "model_invalid_request"
            | "model_not_found"
            | "model_region_unavailable"
            | "model_tools_unsupported"
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModelInvocationErrorClassification {
    pub code: &'static str,
    pub public_message: &'static str,
    pub retryable_before_output: bool,
}

pub(crate) fn classify_model_invocation_error(
    raw_error: &str,
) -> ModelInvocationErrorClassification {
    let normalized = raw_error.to_ascii_lowercase();
    if contains_context_limit_error(&normalized) {
        return ModelInvocationErrorClassification {
            code: "model_context_limit",
            public_message: PUBLIC_MODEL_CONTEXT_LIMIT_MESSAGE,
            retryable_before_output: false,
        };
    }
    if contains_region_restriction_error(&normalized) {
        return ModelInvocationErrorClassification {
            code: "model_region_unavailable",
            public_message: PUBLIC_MODEL_REGION_UNAVAILABLE_MESSAGE,
            retryable_before_output: false,
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
            retryable_before_output: false,
        };
    }
    if contains_http_status(&normalized, 429)
        || normalized.contains("rate limit")
        || normalized.contains("too many requests")
    {
        return ModelInvocationErrorClassification {
            code: "model_rate_limited",
            public_message: PUBLIC_MODEL_RATE_LIMIT_MESSAGE,
            retryable_before_output: false,
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
            retryable_before_output: false,
        };
    }
    if contains_tool_unsupported_error(&normalized) {
        return ModelInvocationErrorClassification {
            code: "model_tools_unsupported",
            public_message: PUBLIC_MODEL_TOOL_UNSUPPORTED_MESSAGE,
            retryable_before_output: false,
        };
    }
    if contains_http_status(&normalized, 400) {
        return ModelInvocationErrorClassification {
            code: "model_invalid_request",
            public_message: PUBLIC_MODEL_INVALID_REQUEST_MESSAGE,
            retryable_before_output: false,
        };
    }
    if contains_stream_interruption_error(&normalized) {
        return ModelInvocationErrorClassification {
            code: "model_stream_interrupted",
            public_message: PUBLIC_MODEL_STREAM_INTERRUPTED_MESSAGE,
            retryable_before_output: false,
        };
    }
    if contains_timeout_error(&normalized) {
        return ModelInvocationErrorClassification {
            code: "model_timeout",
            public_message: PUBLIC_MODEL_TIMEOUT_MESSAGE,
            retryable_before_output: true,
        };
    }
    if contains_empty_response_error(&normalized) {
        return ModelInvocationErrorClassification {
            code: "model_empty_response",
            public_message: PUBLIC_MODEL_EMPTY_RESPONSE_MESSAGE,
            retryable_before_output: true,
        };
    }
    if contains_retryable_transport_error(&normalized) {
        return ModelInvocationErrorClassification {
            code: "model_invocation_failed",
            public_message: PUBLIC_MODEL_INVOCATION_FAILURE_MESSAGE,
            retryable_before_output: true,
        };
    }
    ModelInvocationErrorClassification {
        code: "model_invocation_failed",
        public_message: PUBLIC_MODEL_INVOCATION_FAILURE_MESSAGE,
        retryable_before_output: false,
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

fn contains_region_restriction_error(error: &str) -> bool {
    error.contains("location is not supported")
        || error.contains("user location is not supported")
        || error.contains("country is not supported")
        || error.contains("region is not supported")
        || error.contains("unsupported region")
        || error.contains("not available in your country")
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

fn contains_empty_response_error(error: &str) -> bool {
    error.contains("empty stream response")
        || error.contains("empty response")
        || error.contains("expected event stream")
}

fn contains_retryable_transport_error(error: &str) -> bool {
    error.contains("桥接调用失败[transport]")
        || error.contains("provider transport failed")
        || error.contains("overloaded")
        || error.contains("server is busy")
        || error.contains("servers are busy")
        || error.contains("service unavailable")
        || error.contains("temporarily unavailable")
        || error.contains("connection reset")
        || error.contains("connection aborted")
        || error.contains("connection closed")
        || error.contains("failed to connect")
        || error.contains("dns error")
        || contains_http_status(error, 408)
        || contains_http_status(error, 409)
        || contains_http_status(error, 500)
        || contains_http_status(error, 502)
        || contains_http_status(error, 503)
        || contains_http_status(error, 504)
        || contains_http_status(error, 529)
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

pub(crate) fn model_empty_response_recovery_prompt(after_tool_calls: bool) -> &'static str {
    if after_tool_calls {
        MODEL_EMPTY_RESPONSE_AFTER_TOOLS_RECOVERY_PROMPT
    } else {
        MODEL_EMPTY_RESPONSE_RECOVERY_PROMPT
    }
}

pub(crate) fn model_stream_interruption_recovery_prompt(
    has_partial_visible_content: bool,
) -> &'static str {
    if has_partial_visible_content {
        "上一轮模型响应在传输中断开，已保留此前可见内容。请从中断位置继续，不要重复已输出内容；若需要调用工具，重新输出完整且可执行的工具调用。"
    } else {
        "上一轮模型响应在传输结束前中断，未取得可复用的可见内容。请继续当前任务；若需要调用工具，重新输出完整且可执行的工具调用。"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_invocation_errors_use_public_message() {
        assert_eq!(
            ModelFailureDiagnostic::empty_response(false, 3, None).summary,
            PUBLIC_MODEL_EMPTY_RESPONSE_MESSAGE
        );
        assert_eq!(
            ModelFailureDiagnostic::empty_response(true, 3, None).summary,
            PUBLIC_MODEL_EMPTY_RESPONSE_MESSAGE
        );
        assert_eq!(
            public_model_invocation_error_message(
                "桥接调用失败[RemoteBusiness]: provider response invalid: empty stream response"
            ),
            PUBLIC_MODEL_EMPTY_RESPONSE_MESSAGE
        );
        assert_eq!(MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS, 3);
        assert_eq!(MODEL_PRE_OUTPUT_RECOVERY_MAX_ATTEMPTS, 1);
        assert_eq!(MODEL_STREAM_INTERRUPTION_RECOVERY_MAX_ATTEMPTS, 2);
        assert!(model_empty_response_recovery_prompt(false).contains("用户可见答复"));
        assert!(model_empty_response_recovery_prompt(true).contains("工具调用已经完成"));
        assert!(model_stream_interruption_recovery_prompt(true).contains("不要重复"));
        assert!(model_stream_interruption_recovery_prompt(false).contains("未取得"));
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
                "http_status=400 body={\"error\":{\"message\":\"User location is not supported for the API use.\"}}"
            ),
            PUBLIC_MODEL_REGION_UNAVAILABLE_MESSAGE
        );
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
            "模型响应流在完成前中断。"
        );
        assert_eq!(
            public_model_invocation_error_message(
                "provider stream interrupted: reading stream chunk failed: connection reset"
            ),
            "模型响应流在完成前中断。"
        );
        assert_eq!(
            public_model_invocation_error_message(
                "provider transport failed: operation timed out after 300 seconds"
            ),
            "模型服务在响应完成前超时。"
        );
    }

    #[test]
    fn model_invocation_errors_mark_only_pre_output_transient_failures_retryable() {
        let empty_stream = classify_model_invocation_error(
            "桥接调用失败[RemoteBusiness]: provider response invalid: empty stream response",
        );
        assert_eq!(empty_stream.code, "model_empty_response");
        assert!(empty_stream.retryable_before_output);

        let transport = classify_model_invocation_error(
            "桥接调用失败[Transport]: provider transport failed: connection reset by peer",
        );
        assert_eq!(transport.code, "model_invocation_failed");
        assert!(transport.retryable_before_output);

        let overloaded = classify_model_invocation_error(
            "桥接调用失败[RemoteBusiness]: provider stream rejected request: Our servers are currently overloaded. Please try again later. (error_code=-32006)",
        );
        assert_eq!(overloaded.code, "model_invocation_failed");
        assert!(overloaded.retryable_before_output);

        let invalid_request = classify_model_invocation_error(
            "桥接调用失败[RemoteBusiness]: provider rejected request (http_status=400)",
        );
        assert!(!invalid_request.retryable_before_output);
    }

    #[test]
    fn model_failure_diagnostic_preserves_core_error_after_redaction() {
        let raw_error = "provider response invalid: empty stream response; Authorization: Bearer secret-token; config=/Users/xie/.magi/settings.json";
        let diagnostic = ModelFailureDiagnostic::from_invocation(
            classify_model_invocation_error(raw_error),
            raw_error,
            "response_validation",
            1,
        );

        assert_eq!(diagnostic.code, "model_empty_response");
        assert_eq!(diagnostic.retry_attempts, 1);
        assert!(diagnostic.detail.contains("provider response invalid"));
        assert!(diagnostic.detail.contains("empty stream response"));
        assert!(diagnostic.detail.contains("[redacted]"));
        assert!(diagnostic.detail.contains("[path]"));
        assert!(!diagnostic.detail.contains("secret-token"));
        assert!(!diagnostic.detail.contains("/Users/xie"));
        assert_eq!(
            serde_json::to_value(&diagnostic).expect("diagnostic should serialize")["schemaVersion"],
            MODEL_FAILURE_SCHEMA_VERSION
        );
    }

    #[test]
    fn incomplete_response_diagnostic_preserves_real_finish_reason() {
        let truncated = ModelFailureDiagnostic::incomplete_response(Some("MAX_TOKENS"), 128);
        assert_eq!(truncated.code, "model_output_truncated");
        assert_eq!(truncated.summary, PUBLIC_MODEL_OUTPUT_TRUNCATED_MESSAGE);
        assert!(truncated.detail.contains("finish_reason=MAX_TOKENS"));
        assert!(truncated.detail.contains("正文字符数=128"));
        assert!(truncated.retryable);

        let filtered = ModelFailureDiagnostic::incomplete_response(Some("SAFETY"), 0);
        assert_eq!(filtered.code, "model_content_filtered");
        assert_eq!(filtered.summary, PUBLIC_MODEL_CONTENT_FILTERED_MESSAGE);
        assert!(!filtered.retryable);

        for reason in ["cancelled", "CANCELED"] {
            let cancelled = ModelFailureDiagnostic::incomplete_response(Some(reason), 0);
            assert_eq!(cancelled.code, "model_generation_cancelled");
            assert_eq!(cancelled.summary, PUBLIC_MODEL_GENERATION_CANCELLED_MESSAGE);
            assert!(cancelled.retryable);
        }

        let missing_tool = ModelFailureDiagnostic::missing_tool_call(Some("tool_use"));
        assert_eq!(missing_tool.code, "model_tool_call_missing");
        assert!(missing_tool.detail.contains("finish_reason=tool_use"));
        assert!(missing_tool.detail.contains("工具调用数组为空"));
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
