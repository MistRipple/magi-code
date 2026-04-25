use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct BridgeModelContractDto {
    pub contract_profile: String,
    pub payload_kind: String,
    pub contract_ok: bool,
    pub has_content: bool,
    pub has_finish_reason: bool,
    pub has_usage: bool,
    pub tool_call_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocking_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct BridgeMcpDefaultRouteContractDto {
    pub route_status: String,
    pub route_target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_server: Option<String>,
    pub describe_ok: bool,
    pub blank_selection_ok: bool,
    pub contract_ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocking_reason: Option<String>,
}

pub(crate) fn evaluate_model_contract(
    payload: &str,
    contract_profile: String,
) -> BridgeModelContractDto {
    if payload.trim().is_empty() {
        return BridgeModelContractDto {
            contract_profile,
            payload_kind: "plain_text".to_string(),
            contract_ok: false,
            has_content: false,
            has_finish_reason: false,
            has_usage: false,
            tool_call_count: 0,
            blocking_reason: Some("bridge payload was empty".to_string()),
        };
    }

    if let Ok(Value::Object(payload)) = serde_json::from_str::<Value>(payload) {
        let has_content = payload
            .get("content")
            .and_then(Value::as_str)
            .map(|content| !content.trim().is_empty())
            .unwrap_or(false);
        let has_finish_reason = payload
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(|reason| !reason.trim().is_empty())
            .unwrap_or(false);
        let has_usage = payload.get("usage").is_some();
        let tool_calls = payload
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let tool_call_count = tool_calls.len();
        let tool_calls_valid = tool_calls.iter().all(|tool_call| {
            tool_call
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .map(|name| !name.trim().is_empty())
                .unwrap_or(false)
                && tool_call
                    .get("function")
                    .and_then(|function| function.get("arguments"))
                    .and_then(Value::as_str)
                    .is_some()
        });
        let contract_ok = (has_content || tool_call_count > 0) && tool_calls_valid;
        let blocking_reason = if !tool_calls_valid {
            Some("structured payload contains invalid tool_calls".to_string())
        } else if !has_content && tool_call_count == 0 {
            Some("structured payload missing content or tool_calls".to_string())
        } else {
            None
        };

        return BridgeModelContractDto {
            contract_profile,
            payload_kind: "structured_json".to_string(),
            contract_ok,
            has_content,
            has_finish_reason,
            has_usage,
            tool_call_count,
            blocking_reason,
        };
    }

    BridgeModelContractDto {
        contract_profile,
        payload_kind: "plain_text".to_string(),
        contract_ok: true,
        has_content: true,
        has_finish_reason: false,
        has_usage: false,
        tool_call_count: 0,
        blocking_reason: None,
    }
}
