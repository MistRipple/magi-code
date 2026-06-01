pub(crate) const PUBLIC_MODEL_INVOCATION_FAILURE_MESSAGE: &str = "模型服务暂时不可用，请稍后重试。";

pub(crate) fn public_model_invocation_error_message(_raw_error: &str) -> String {
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
}
