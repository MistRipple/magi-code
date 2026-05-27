pub(crate) fn provider_empty_assistant_response_error(after_tool_calls: bool) -> String {
    let reason = if after_tool_calls {
        "empty assistant response after tool calls"
    } else {
        "empty assistant response"
    };
    format!("桥接调用失败[RemoteBusiness]: provider response invalid: {reason}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_assistant_response_error_exposes_failure_layer() {
        assert_eq!(
            provider_empty_assistant_response_error(false),
            "桥接调用失败[RemoteBusiness]: provider response invalid: empty assistant response"
        );
        assert_eq!(
            provider_empty_assistant_response_error(true),
            "桥接调用失败[RemoteBusiness]: provider response invalid: empty assistant response after tool calls"
        );
    }
}
