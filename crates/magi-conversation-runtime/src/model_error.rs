pub(crate) const PUBLIC_MODEL_INVOCATION_FAILURE_MESSAGE: &str = "模型服务暂时不可用，请稍后重试。";
pub(crate) const PUBLIC_MODEL_IMAGE_INVOCATION_FAILURE_MESSAGE: &str =
    "当前模型暂不支持图片输入，请更换支持图片的模型后重试。";
pub(crate) const PUBLIC_MODEL_INVALID_IMAGE_INPUT_MESSAGE: &str =
    "图片输入无效，请重新选择图片后重试。";

pub(crate) fn public_model_invocation_error_message(_raw_error: &str) -> String {
    PUBLIC_MODEL_INVOCATION_FAILURE_MESSAGE.to_string()
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
