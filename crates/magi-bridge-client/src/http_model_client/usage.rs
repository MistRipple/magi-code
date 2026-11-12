use super::*;

pub(super) fn transport_started(
    body: &Value,
    observer: &ModelTransportObserver<'_>,
) -> Result<(), BridgeClientError> {
    observer(&ModelTransportEvent::Started {
        request_fingerprint: magi_core::hex_lower(Sha256::digest(body.to_string().as_bytes())),
    })
}

pub(super) fn transport_finished<T>(
    result: &Result<T, BridgeClientError>,
    observer: &ModelTransportObserver<'_>,
) -> Result<(), BridgeClientError> {
    observer(&ModelTransportEvent::Finished {
        outcome: match result {
            Ok(_) => ModelTransportOutcome::Succeeded,
            Err(error) if model_invocation_error_is_cancelled(error) => {
                ModelTransportOutcome::Cancelled
            }
            Err(_) => ModelTransportOutcome::Failed,
        },
    })
}

pub(super) fn transport_usage(usage: &crate::llm_types::LlmUsage) -> ModelTransportUsage {
    ModelTransportUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        cache_read_included_in_input: usage.cache_read_included_in_input,
    }
}

pub(super) fn model_response_transport_usage(
    response: &crate::types::ModelResponse,
) -> Option<ModelTransportUsage> {
    let usage = response.usage.as_ref()?.clone();
    let usage = serde_json::from_value::<crate::llm_types::LlmUsage>(usage).ok()?;
    Some(transport_usage(&usage))
}

pub(super) fn has_reported_token_counts(usage: &Value) -> bool {
    [
        "input_tokens",
        "output_tokens",
        "prompt_tokens",
        "completion_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ]
    .iter()
    .any(|field| usage.get(*field).and_then(Value::as_u64).is_some())
}

pub(super) fn observed_response_usage(
    family: ProviderFamily,
    body: &str,
) -> Option<ModelTransportUsage> {
    // 缺少 usage 不是零用量；只有提供方明确返回时才规范化并记录。
    let raw: Value = serde_json::from_str(body).ok()?;
    raw.get("usage")
        .filter(|usage| has_reported_token_counts(usage))?;
    let response = match family {
        ProviderFamily::OpenAiChat => OpenAiChatCompletionsAdapter.parse_response(200, body),
        ProviderFamily::OpenAiResponses => OpenAiResponsesAdapter.parse_response(200, body),
        ProviderFamily::Anthropic => AnthropicMessagesAdapter.parse_response(200, body),
    }
    .ok()?;
    Some(transport_usage(&response.usage))
}

#[cfg(test)]
pub type ModelProbeRequest = (String, serde_json::Value, Vec<(String, String)>);
