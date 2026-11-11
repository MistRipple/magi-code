//! 历史夹具写入 canonical 快照，再使用生产投影。生产库不提供直接追加 transcript 的入口。
use magi_core::{ThreadId, UtcMillis};
use magi_session_store::{SessionStore, ThreadChatMessage};
use serde_json::json;

pub(crate) fn with_messages(
    store: &SessionStore,
    thread_id: &ThreadId,
    messages: Vec<ThreadChatMessage>,
    now: UtcMillis,
) -> SessionStore {
    let mut state = store.export_state();
    let thread = state
        .thread_registry
        .iter()
        .find(|thread| &thread.thread_id == thread_id)
        .unwrap();
    let session = thread.session_id.clone();
    let seq = state
        .canonical_turns
        .iter()
        .filter(|turn| turn.session_id == session)
        .map(|turn| turn.turn_seq)
        .max()
        .unwrap_or(0)
        + 1;
    let turn = format!("fixture-history-{thread_id}-{seq}");
    let mut calls = std::collections::HashMap::new();
    let items = messages.iter().enumerate().map(|(index, message)| {
        for call in &message.tool_calls { calls.insert(call.id.clone(), call.clone()); }
        let mut item = json!({
            "sessionId": session, "turnId": turn, "turnSeq": seq,
            "itemId": format!("{turn}-{index}"), "itemSeq": index + 1,
            "createdAt": now, "updatedAt": now, "status": "completed",
            "sourceThreadId": thread_id, "content": message.content,
        });
        match message.role.as_str() {
            "user" => {
                item["kind"] = json!("user_message");
                if let Some(id) = &message.input_request_id { item["metadata"]["requestId"] = json!(id); }
            }
            "assistant" => {
                item["kind"] = json!("assistant_text");
                item["metadata"]["toolCalls"] = json!(message.tool_calls);
                item["metadata"]["providerContext"] = json!(message.provider_context);
            }
            "tool" => {
                let id = message.tool_call_id.as_ref().unwrap();
                let payload: serde_json::Value = serde_json::from_str(message.content.as_deref().unwrap_or("{}")).unwrap_or_default();
                let name = calls.get(id).map(|call| call.function.name.as_str())
                    .or_else(|| payload.get("tool").and_then(|value| value.as_str())).expect("tool fixture needs a name");
                item["kind"] = json!("tool_call");
                item["tool"] = json!({ "callId": id, "name": name,
                    "arguments": calls.get(id).map(|call| call.function.arguments.clone()).unwrap_or("{}".into()),
                    "result": message.content });
            }
            role => panic!("unsupported history fixture role: {role}"),
        }
        item
    }).collect::<Vec<_>>();
    state.canonical_turns.push(
        serde_json::from_value(json!({
            "sessionId": session, "turnId": turn, "turnSeq": seq,
            "acceptedAt": now, "completedAt": now, "status": "completed", "items": items,
        }))
        .unwrap(),
    );
    SessionStore::from_state_for_test(state)
}

/// 可切换的结果投影夹具：版本 0 表示插件停用。
pub(crate) struct ToolProjectionHooks {
    pub version: std::sync::atomic::AtomicUsize,
    pub calls: std::sync::atomic::AtomicUsize,
    pub compaction_sources: std::sync::Mutex<Vec<String>>,
}

impl ToolProjectionHooks {
    pub fn new() -> Self {
        Self {
            version: std::sync::atomic::AtomicUsize::new(1),
            calls: Default::default(),
            compaction_sources: Default::default(),
        }
    }
}

impl crate::extension_hooks::ExtensionHooks for ToolProjectionHooks {
    fn tool_call(
        &self,
        _: &crate::extension_hooks::ToolHookScope<'_>,
        _: &magi_bridge_client::ChatToolCall,
    ) -> crate::extension_hooks::ToolCallDecision {
        unreachable!()
    }
    fn tool_result(
        &self,
        _: &crate::extension_hooks::ToolHookScope<'_>,
        _: &magi_bridge_client::ChatToolCall,
        view: crate::extension_hooks::ToolResultView<'_>,
    ) -> Option<String> {
        use std::sync::atomic::Ordering;
        self.calls.fetch_add(1, Ordering::SeqCst);
        let version = self.version.load(Ordering::SeqCst);
        (version > 0 && view.projection.contains("SOURCE_SECRET"))
            .then(|| format!("[projection-v{version}]"))
    }
    fn compaction_before(
        &self,
        view: &crate::extension_hooks::CompactionView<'_>,
    ) -> crate::extension_hooks::CompactionDecision {
        self.compaction_sources
            .lock()
            .unwrap()
            .push(view.source.into());
        crate::extension_hooks::CompactionDecision::Proceed
    }
}

pub(crate) fn tool_projection_history(id: &str) -> Vec<ThreadChatMessage> {
    serde_json::from_value(json!([
        {"role": "assistant", "toolCalls": [{"id": id, "type": "function", "function": {"name": "projected_probe", "arguments": "{}"}}]},
        {"role": "tool", "toolCallId": id, "content": json!({"tool": "projected_probe", "status": "succeeded", "content": "SOURCE_SECRET"}).to_string()}
    ])).unwrap()
}
