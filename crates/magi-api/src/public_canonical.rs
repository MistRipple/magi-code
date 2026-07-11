use magi_core::{PUBLIC_REDACTED_PATH, public_runtime_text};
use magi_event_bus::EventEnvelope;
use magi_session_store::{CanonicalTurn, CanonicalTurnItem};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

pub(crate) fn public_canonical_turn(mut turn: CanonicalTurn) -> CanonicalTurn {
    for item in &mut turn.items {
        public_canonical_turn_item_in_place(item);
    }
    turn
}

pub(crate) fn public_canonical_turn_item(mut item: CanonicalTurnItem) -> CanonicalTurnItem {
    public_canonical_turn_item_in_place(&mut item);
    item
}

pub(crate) fn public_event_envelope(mut event: EventEnvelope) -> EventEnvelope {
    event.payload = public_event_payload(event.payload);
    event
}

fn public_event_payload(mut payload: Value) -> Value {
    let Value::Object(object) = &mut payload else {
        return payload;
    };

    public_payload_field::<CanonicalTurn>(object, "canonical_turn", public_canonical_turn);
    public_payload_field::<CanonicalTurn>(object, "canonicalTurn", public_canonical_turn);
    public_payload_field::<CanonicalTurnItem>(object, "canonical_item", public_canonical_turn_item);
    public_payload_field::<CanonicalTurnItem>(object, "canonicalItem", public_canonical_turn_item);
    public_runtime_tool_text_fields_in_value(&mut payload);
    payload
}

fn public_payload_field<T>(
    object: &mut Map<String, Value>,
    key: &str,
    public_value: impl FnOnce(T) -> T,
) where
    T: DeserializeOwned + Serialize,
{
    let Some(value) = object.get_mut(key) else {
        return;
    };
    if value.is_null() {
        return;
    }
    let Ok(parsed) = serde_json::from_value::<T>(value.clone()) else {
        return;
    };
    if let Ok(next_value) = serde_json::to_value(public_value(parsed)) {
        *value = next_value;
    }
}

fn public_canonical_turn_item_in_place(item: &mut CanonicalTurnItem) {
    let Some(tool) = item.tool.as_mut() else {
        return;
    };
    let tool_name = tool.name.clone();
    tool.arguments = tool
        .arguments
        .take()
        .and_then(|value| public_canonical_tool_value(value, true, Some(&tool_name)));
    tool.result = tool
        .result
        .take()
        .and_then(|value| public_canonical_tool_value(value, false, None));
    tool.error = public_canonical_tool_text(tool.error.take());
}

fn public_canonical_tool_value(
    value: Value,
    preserve_raw_path_string: bool,
    tool_name: Option<&str>,
) -> Option<Value> {
    let original = value.clone();
    let public = public_runtime_text(&value.to_string());
    if public.is_empty() {
        return None;
    }
    let mut public_value = serde_json::from_str(&public)
        .ok()
        .or(Some(Value::String(public)))?;
    preserve_public_tool_path_labels(&original, &mut public_value, preserve_raw_path_string);
    preserve_public_apply_patch_path_labels(tool_name, &original, &mut public_value);
    Some(public_value)
}

fn public_canonical_tool_text(value: Option<String>) -> Option<String> {
    let value = value?.trim().to_string();
    if value.is_empty() {
        return None;
    }
    let public = public_runtime_text(&value);
    if public.is_empty() {
        None
    } else {
        Some(public)
    }
}

fn public_runtime_tool_text_fields_in_value(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if is_runtime_tool_text_field(key) {
                    public_runtime_tool_text_value(value);
                } else {
                    public_runtime_tool_text_fields_in_value(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                public_runtime_tool_text_fields_in_value(item);
            }
        }
        _ => {}
    }
}

fn is_runtime_tool_text_field(key: &str) -> bool {
    matches!(
        key,
        "tool_arguments"
            | "toolArguments"
            | "tool_result"
            | "toolResult"
            | "tool_error"
            | "toolError"
    )
}

fn public_runtime_tool_text_value(value: &mut Value) {
    if value.is_null() {
        return;
    }
    let raw = value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string());
    let public = public_runtime_text(&raw);
    if public.is_empty() {
        *value = Value::Null;
    } else {
        *value = Value::String(public);
    }
}

fn preserve_public_tool_path_labels(
    original: &Value,
    public: &mut Value,
    preserve_raw_path_string: bool,
) {
    match (original, public) {
        (Value::Object(original_object), Value::Object(public_object)) => {
            for (key, public_value) in public_object {
                let Some(original_value) = original_object.get(key) else {
                    continue;
                };
                if is_tool_path_label_key(key) {
                    preserve_public_tool_path_label_value(original_value, public_value);
                } else {
                    preserve_public_tool_path_labels(original_value, public_value, false);
                }
            }
        }
        (Value::Array(original_items), Value::Array(public_items)) => {
            for (original_item, public_item) in original_items.iter().zip(public_items.iter_mut()) {
                preserve_public_tool_path_labels(original_item, public_item, false);
            }
        }
        (Value::String(original_text), public_value) if preserve_raw_path_string => {
            if public_value == PUBLIC_REDACTED_PATH
                && let Some(label) = public_path_label(original_text)
            {
                *public_value = Value::String(label);
            }
        }
        _ => {}
    }
}

fn preserve_public_tool_path_label_value(original: &Value, public: &mut Value) {
    match (original, public) {
        (Value::String(original_text), Value::String(public_text))
            if public_text == PUBLIC_REDACTED_PATH =>
        {
            if let Some(label) = public_path_label(original_text) {
                *public_text = label;
            }
        }
        (Value::Array(original_items), Value::Array(public_items)) => {
            for (original_item, public_item) in original_items.iter().zip(public_items.iter_mut()) {
                preserve_public_tool_path_label_value(original_item, public_item);
            }
        }
        _ => {}
    }
}

fn preserve_public_apply_patch_path_labels(
    tool_name: Option<&str>,
    original: &Value,
    public: &mut Value,
) {
    if tool_name != Some("apply_patch") {
        return;
    }
    match (original, public) {
        (Value::Object(original_object), Value::Object(public_object)) => {
            for key in ["patch", "input", "text"] {
                let (Some(Value::String(original_patch)), Some(Value::String(public_patch))) =
                    (original_object.get(key), public_object.get_mut(key))
                else {
                    continue;
                };
                *public_patch = public_apply_patch_text(original_patch, public_patch);
                break;
            }
        }
        (Value::String(original_patch), Value::String(public_patch)) => {
            *public_patch = public_apply_patch_text(original_patch, public_patch);
        }
        _ => {}
    }
}

fn public_apply_patch_text(original_patch: &str, public_patch: &str) -> String {
    let original_lines = original_patch.lines().collect::<Vec<_>>();
    let mut public_lines = public_patch.lines().map(str::to_string).collect::<Vec<_>>();
    if original_lines.len() != public_lines.len() {
        return public_patch.to_string();
    }

    for (index, original_line) in original_lines.iter().enumerate() {
        let Some((prefix, original_path)) = apply_patch_path_header(original_line) else {
            continue;
        };
        let Some(label) = public_path_label(original_path) else {
            continue;
        };
        public_lines[index] = format!("{prefix}{label}");
    }
    public_lines.join("\n")
}

fn apply_patch_path_header(line: &str) -> Option<(&'static str, &str)> {
    for prefix in [
        "*** Add File: ",
        "*** Delete File: ",
        "*** Update File: ",
        "*** Move to: ",
    ] {
        if let Some(path) = line.strip_prefix(prefix) {
            return Some((prefix, path));
        }
    }
    None
}

fn is_tool_path_label_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "path"
            | "filepath"
            | "file_path"
            | "imagepath"
            | "image_path"
            | "dirpath"
            | "dir_path"
            | "targetpath"
            | "target_path"
            | "source"
            | "sourcepath"
            | "source_path"
            | "destination"
            | "destinationpath"
            | "destination_path"
            | "changed_paths"
            | "changedpaths"
            | "file_paths"
            | "filepaths"
            | "target_paths"
            | "targetpaths"
    )
}

fn public_path_label(path: &str) -> Option<String> {
    let trimmed = path.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return None;
    }
    if is_absolute_path_label(trimmed) {
        return trimmed
            .rsplit(['/', '\\'])
            .find(|segment| !segment.is_empty())
            .map(str::to_string);
    }
    Some(trimmed.to_string())
}

fn is_absolute_path_label(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with("\\\\")
        || path.as_bytes().get(0..3).is_some_and(|prefix| {
            prefix[0].is_ascii_alphabetic()
                && prefix[1] == b':'
                && (prefix[2] == b'\\' || prefix[2] == b'/')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{EventId, SessionId, ThreadId, UtcMillis};
    use magi_event_bus::EventContext;
    use magi_session_store::{
        CanonicalToolCall, CanonicalTurnItemKind, CanonicalTurnItemStatus, CanonicalTurnStatus,
        CanonicalTurnVisibility,
    };
    use serde_json::json;

    fn canonical_tool_item() -> CanonicalTurnItem {
        CanonicalTurnItem {
            session_id: SessionId::new("session-public-canonical"),
            turn_id: "turn-public-canonical".to_string(),
            turn_seq: 1,
            item_id: "item-public-canonical".to_string(),
            item_seq: 1,
            kind: CanonicalTurnItemKind::ToolCall,
            created_at: UtcMillis::now(),
            status: CanonicalTurnItemStatus::Completed,
            item_version: None,
            updated_at: UtcMillis::now(),
            title: Some("读取文件".to_string()),
            content: Some("工具卡片保留 /Users/xie/code/plain-text".to_string()),
            blocks: Vec::new(),
            tool: Some(CanonicalToolCall {
                call_id: "tool-public-canonical".to_string(),
                name: "read_file".to_string(),
                arguments: Some(json!({
                    "path": "/Users/xie/code/TEST/secret.txt",
                    "token": "sk-argument-secret"
                })),
                result: Some(json!({
                    "output": "read /private/tmp/magi/result with Bearer resulttoken"
                })),
                error: Some("failed at /var/folders/magi/cache with sk-error-secret".to_string()),
            }),
            worker: None,
            source_thread_id: ThreadId::new("thread-public-canonical"),
            visibility: CanonicalTurnVisibility { renderable: true },
            metadata: Default::default(),
        }
    }

    fn canonical_turn(item: CanonicalTurnItem) -> CanonicalTurn {
        CanonicalTurn {
            session_id: item.session_id.clone(),
            turn_id: item.turn_id.clone(),
            turn_seq: item.turn_seq,
            accepted_at: item.created_at,
            completed_at: Some(item.updated_at),
            status: CanonicalTurnStatus::Completed,
            response_duration_ms: Some(1),
            usage: None,
            items: vec![item],
            metadata: Default::default(),
        }
    }

    #[test]
    fn public_event_envelope_redacts_canonical_and_runtime_tool_payloads() {
        let item = canonical_tool_item();
        let turn = canonical_turn(item.clone());
        let event = EventEnvelope::domain(
            EventId::new("event-public-canonical"),
            "session.turn.item",
            json!({
                "session_id": "session-public-canonical",
                "item": {
                    "content": "工具卡片保留 /Users/xie/code/plain-text",
                    "toolArguments": "{\"path\":\"/Users/xie/code/TEST/raw.txt\",\"token\":\"sk-raw-argument\"}",
                    "toolResult": "raw result /private/tmp/magi/result with Bearer rawtoken",
                    "toolError": "raw error /var/folders/magi/cache with sk-raw-error"
                },
                "turn_items": [{
                    "content": "摘要保留 /Users/xie/code/plain-summary",
                    "tool_arguments": "{\"path\":\"/Users/xie/code/TEST/summary.txt\"}",
                    "tool_result": "summary result /private/tmp/magi/summary",
                    "tool_error": "summary error sk-summary-error"
                }],
                "canonical_turn": turn,
                "canonical_item": item
            }),
        )
        .with_context(EventContext {
            session_id: Some(SessionId::new("session-public-canonical")),
            ..EventContext::default()
        });

        let public = public_event_envelope(event);
        let payload_text = public.payload.to_string();

        assert!(!payload_text.contains("/Users/xie/code/TEST/secret.txt"));
        assert!(payload_text.contains("secret.txt"));
        assert!(!payload_text.contains("raw.txt"));
        assert!(!payload_text.contains("summary.txt"));
        assert!(!payload_text.contains("/private/tmp"));
        assert!(!payload_text.contains("/var/folders"));
        assert!(!payload_text.contains("argument-secret"));
        assert!(!payload_text.contains("rawtoken"));
        assert!(!payload_text.contains("summary-error"));
        assert_eq!(
            public.payload["canonical_turn"]["items"][0]["tool"]["arguments"]["path"],
            json!("secret.txt")
        );
        assert_eq!(
            public.payload["canonical_item"]["tool"]["arguments"]["path"],
            json!("secret.txt")
        );
        assert_eq!(
            public.payload["canonical_item"]["tool"]["arguments"]["token"],
            json!("[redacted]")
        );
        assert!(
            public.payload["item"]["toolResult"]
                .as_str()
                .expect("raw tool result should remain string")
                .contains("Bearer [redacted]")
        );
        assert!(
            public.payload["turn_items"][0]["tool_error"]
                .as_str()
                .expect("summary tool error should remain string")
                .contains("sk-[redacted]")
        );
        assert_eq!(
            public.payload["item"]["content"],
            json!("工具卡片保留 /Users/xie/code/plain-text")
        );
    }

    #[test]
    fn public_canonical_apply_patch_keeps_safe_patch_file_headers() {
        let mut item = canonical_tool_item();
        let tool = item.tool.as_mut().expect("tool should exist");
        tool.name = "apply_patch".to_string();
        let patch_text = [
            "*** Begin Patch",
            "*** Update File: /Users/xie/code/vibecode-test/index.html",
            "@@",
            "-<div>old</div>",
            "+<div>new</div>",
            "*** Update File: /Users/xie/code/vibecode-test/styles.css",
            "@@",
            "-.old { color: red; }",
            "+.new { color: blue; }",
            "+/* sk-live-secret /private/tmp/magi */",
            "*** End Patch",
        ]
        .join("\n");
        tool.arguments = Some(json!({
            "patch": patch_text
        }));
        tool.result = Some(json!({
            "status": "succeeded",
            "changed_paths": [
                "/Users/xie/code/vibecode-test/index.html",
                "/Users/xie/code/vibecode-test/styles.css"
            ],
        }));

        let public = public_canonical_turn_item(item);
        let tool = public.tool.expect("public tool should exist");
        let patch = tool.arguments.expect("arguments should exist")["patch"]
            .as_str()
            .expect("patch should stay string")
            .to_string();

        assert!(patch.contains("*** Update File: index.html"));
        assert!(patch.contains("*** Update File: styles.css"));
        assert!(patch.contains("+.new { color: blue; }"));
        assert!(!patch.contains("/Users/xie"));
        assert!(!patch.contains("/private/tmp"));
        assert!(!patch.contains("live-secret"));
        assert!(patch.contains("sk-[redacted]"));
        assert_eq!(
            tool.result.expect("result should exist")["changed_paths"],
            json!(["index.html", "styles.css"])
        );
    }
}
