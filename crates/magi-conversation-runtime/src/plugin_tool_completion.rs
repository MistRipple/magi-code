//! 工具完成意图只来自 daemon 封装的插件回执，随原 canonical 工具事实恢复。
//! 这里只读取结果；任务验收、Goal、权限与终态仍由原生执行链路裁决。
use magi_extension_host::protocol::PluginToolCompletion;
use magi_session_store::ThreadChatMessage;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn from_result(tool: &str, raw: &str) -> Option<PluginToolCompletion> {
    let payload: Value = serde_json::from_str(raw).ok()?;
    if payload.get("tool")?.as_str()? != tool || payload.get("status")?.as_str()? != "succeeded" {
        return None;
    }
    let completion: PluginToolCompletion =
        serde_json::from_value(payload.get("pluginCompletion")?.clone()).ok()?;
    if magi_plugin_system::model_tool_name(&completion.plugin_id, &completion.contribution_id)
        != tool
        || completion.content.trim().is_empty()
        || completion.content.len() > 1_048_576
    {
        return None;
    }
    Some(completion)
}

pub(crate) fn recover(history: &[ThreadChatMessage]) -> Result<Vec<PluginToolCompletion>, String> {
    let calls = history
        .iter()
        .flat_map(|message| &message.tool_calls)
        .map(|call| (call.id.as_str(), call.function.name.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut settled = BTreeSet::new();
    let mut completions = Vec::new();
    let mut unresolved = false;
    for message in history.iter().filter(|message| message.role == "tool") {
        let Some((id, raw)) = message
            .tool_call_id
            .as_deref()
            .zip(message.content.as_deref())
        else {
            continue;
        };
        let Some(tool) = calls.get(id) else { continue };
        let payload: Option<Value> = serde_json::from_str(raw).ok();
        let status = payload
            .as_ref()
            .and_then(|value| value.get("status"))
            .and_then(Value::as_str);
        unresolved |= matches!(
            status,
            Some(
                "interrupted"
                    | "outcome_unknown"
                    | "running"
                    | "pending"
                    | "awaiting_approval"
                    | "awaiting_user_input"
            )
        );
        settled.insert(id);
        if let Some(completion) = from_result(tool, raw) {
            completions.push(completion);
        }
    }
    if !completions.is_empty() && (unresolved || calls.keys().any(|id| !settled.contains(id))) {
        return Err("插件工具已提交完成意图，但同批工具仍有未确定结果；请先核对原调用，不能重放副作用或直接完成任务".into());
    }
    Ok(completions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_session_store::{ThreadChatToolCall, ThreadChatToolFunction};
    use serde_json::json;

    fn history() -> Vec<ThreadChatMessage> {
        let tool = magi_plugin_system::model_tool_name("acme.words", "finish");
        let mut call: ThreadChatMessage =
            serde_json::from_value(json!({"role":"assistant"})).unwrap();
        call.tool_calls.push(ThreadChatToolCall {
            id: "c".into(),
            kind: "function".into(),
            function: ThreadChatToolFunction {
                name: tool.clone(),
                arguments: "{}".into(),
            },
        });
        let result = serde_json::from_value(json!({"role":"tool","toolCallId":"c", "content":json!({"tool":tool,"status":"succeeded", "output":{"value":"actual"},"pluginCompletion":{"pluginId":"acme.words","contributionId":"finish","content":"实际业务完成"}}).to_string()})).unwrap();
        vec![call, result]
    }

    #[test]
    fn completion_requires_successful_plugin_identity_and_cannot_be_business_output() {
        let h = history();
        let tool = &h[0].tool_calls[0].function.name;
        let raw = h[1].content.as_deref().unwrap();
        assert_eq!(from_result(tool, raw).unwrap().content, "实际业务完成");
        let mut value: Value = serde_json::from_str(raw).unwrap();
        value["status"] = json!("failed");
        assert!(from_result(tool, &value.to_string()).is_none());
        value["status"] = json!("succeeded");
        assert!(from_result("file_read", &value.to_string()).is_none());
        value["output"]["pluginCompletion"] = value["pluginCompletion"].take();
        assert!(from_result(tool, &value.to_string()).is_none());
    }

    #[test]
    fn recovery_uses_committed_results_and_refuses_partial_or_uncertain_batches() {
        let mut h = history();
        assert_eq!(recover(&h).unwrap().len(), 1);
        h[0].tool_calls.push(ThreadChatToolCall {
            id: "other".into(),
            kind: "function".into(),
            function: ThreadChatToolFunction {
                name: "external_write".into(),
                arguments: "{}".into(),
            },
        });
        assert!(recover(&h).is_err());
        h.push(serde_json::from_value(json!({"role":"tool","toolCallId":"other", "content":json!({"tool":"external_write","status":"interrupted","execution":"unknown"}).to_string()})).unwrap());
        assert!(recover(&h).is_err());
        h.last_mut().unwrap().content =
            Some(json!({"tool":"external_write","status":"failed"}).to_string());
        assert_eq!(recover(&h).unwrap().len(), 1);
    }
}
