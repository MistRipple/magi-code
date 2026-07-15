pub(crate) const PUBLIC_MODEL_INVOCATION_FAILURE_MESSAGE: &str = "模型服务暂时不可用，请稍后重试。";
pub(crate) const PUBLIC_MODEL_AUTH_FAILURE_MESSAGE: &str =
    "模型认证失败，请检查 API Key 或访问权限。";
pub(crate) const PUBLIC_MODEL_RATE_LIMIT_MESSAGE: &str = "模型服务当前受到限流，请稍后重试。";
pub(crate) const PUBLIC_MODEL_NOT_FOUND_MESSAGE: &str = "模型不可用，请检查模型名称和服务配置。";
pub(crate) const PUBLIC_MODEL_CONTEXT_LIMIT_MESSAGE: &str =
    "当前对话已超过模型上下文长度，请压缩上下文或开启新会话。";
pub(crate) const PUBLIC_MODEL_INVALID_REQUEST_MESSAGE: &str =
    "模型请求不可用，请检查模型名称和请求参数。";
pub(crate) const PUBLIC_MODEL_IMAGE_INVOCATION_FAILURE_MESSAGE: &str =
    "当前模型暂不支持图片输入，请更换支持图片的模型后重试。";
pub(crate) const PUBLIC_MODEL_INVALID_IMAGE_INPUT_MESSAGE: &str =
    "图片输入无效，请重新选择图片后重试。";

pub(crate) fn public_model_invocation_error_message(raw_error: &str) -> String {
    let normalized = raw_error.to_ascii_lowercase();
    if contains_context_limit_error(&normalized) {
        return PUBLIC_MODEL_CONTEXT_LIMIT_MESSAGE.to_string();
    }
    if contains_http_status(&normalized, 401)
        || contains_http_status(&normalized, 403)
        || normalized.contains("invalid api key")
        || normalized.contains("incorrect api key")
        || normalized.contains("unauthorized")
        || normalized.contains("forbidden")
        || normalized.contains("authentication")
    {
        return PUBLIC_MODEL_AUTH_FAILURE_MESSAGE.to_string();
    }
    if contains_http_status(&normalized, 429)
        || normalized.contains("rate limit")
        || normalized.contains("too many requests")
    {
        return PUBLIC_MODEL_RATE_LIMIT_MESSAGE.to_string();
    }
    if contains_http_status(&normalized, 404)
        || normalized.contains("model not found")
        || normalized.contains("model_not_found")
        || normalized.contains("unknown model")
    {
        return PUBLIC_MODEL_NOT_FOUND_MESSAGE.to_string();
    }
    if contains_http_status(&normalized, 400) {
        return PUBLIC_MODEL_INVALID_REQUEST_MESSAGE.to_string();
    }
    PUBLIC_MODEL_INVOCATION_FAILURE_MESSAGE.to_string()
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
        || error.contains("too many tokens")
        || error.contains("token limit")
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
    PUBLIC_MODEL_INVOCATION_FAILURE_MESSAGE.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_invocation_errors_use_public_message() {
        assert_eq!(
            provider_empty_assistant_response_error(false),
            PUBLIC_MODEL_INVOCATION_FAILURE_MESSAGE
        );
        assert_eq!(
            provider_empty_assistant_response_error(true),
            PUBLIC_MODEL_INVOCATION_FAILURE_MESSAGE
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
