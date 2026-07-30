use magi_bridge_client::ModelResponse;
use serde::Serialize;

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
    response: &ModelResponse,
    contract_profile: String,
) -> BridgeModelContractDto {
    let has_content = response
        .content
        .as_deref()
        .is_some_and(|content| !content.trim().is_empty());
    let has_finish_reason = response
        .finish_reason
        .as_deref()
        .is_some_and(|reason| !reason.trim().is_empty());
    let has_usage = response.usage.is_some();
    let tool_call_count = response.tool_calls.len();
    let tool_calls_valid = response.tool_calls.iter().all(|tool_call| {
        !tool_call.function.name.trim().is_empty()
            && !tool_call.function.arguments.trim().is_empty()
    });
    let contract_ok = (has_content || tool_call_count > 0) && tool_calls_valid;
    BridgeModelContractDto {
        contract_profile,
        payload_kind: "model_response".to_string(),
        contract_ok,
        has_content,
        has_finish_reason,
        has_usage,
        tool_call_count,
        blocking_reason: if !tool_calls_valid {
            Some("model response contains invalid tool_calls".to_string())
        } else if !has_content && tool_call_count == 0 {
            Some("model response missing content or tool_calls".to_string())
        } else {
            None
        },
    }
}
