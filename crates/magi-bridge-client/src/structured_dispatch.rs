use serde_json::Value;

use crate::llm_types::LlmResponse;

#[derive(Clone, Debug)]
pub struct StructuredDispatchResult {
    pub tool_name: String,
    pub arguments: Value,
    pub raw_content: Option<String>,
}

pub fn extract_structured_dispatch(response: &LlmResponse) -> Option<StructuredDispatchResult> {
    if response.tool_calls.len() == 1 {
        let tc = &response.tool_calls[0];
        return Some(StructuredDispatchResult {
            tool_name: tc.name.clone(),
            arguments: tc.arguments.clone(),
            raw_content: if response.content.is_empty() {
                None
            } else {
                Some(response.content.clone())
            },
        });
    }

    if let Ok(parsed) = serde_json::from_str::<Value>(&response.content) {
        if let Some(name) = parsed.get("tool").and_then(|v| v.as_str()) {
            let args = parsed.get("arguments").cloned().unwrap_or(Value::Object(Default::default()));
            return Some(StructuredDispatchResult {
                tool_name: name.to_string(),
                arguments: args,
                raw_content: Some(response.content.clone()),
            });
        }
    }

    None
}

pub fn is_structured_response(response: &LlmResponse) -> bool {
    !response.tool_calls.is_empty()
        || serde_json::from_str::<Value>(&response.content)
            .ok()
            .and_then(|v| v.get("tool").and_then(|t| t.as_str()).map(|_| true))
            .unwrap_or(false)
}
