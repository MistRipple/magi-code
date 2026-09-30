use magi_core::{PUBLIC_REDACTED_PATH, public_runtime_text};
use magi_event_bus::EventEnvelope;
use magi_session_store::{CanonicalTurn, CanonicalTurnItem, CanonicalTurnItemKind};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

pub(crate) fn public_canonical_turn(mut turn: CanonicalTurn) -> CanonicalTurn {
    for item in &mut turn.items {
        public_canonical_turn_item_in_place(item);
    }
    turn
}

/// 历史分页中单个工具结果字符串保留的最大字符数。
///
/// 切换会话的耗时必须只取决于“当前这一页”，而单个 turn 可以包含任意多、任意大的
/// 工具输出（文件读取、shell stdout 等）。历史页只带每段长文本的开头，并在 item
/// metadata 的 `historyCompaction` 中标记被截断；完整内容由 `/messages/item` 按需取回。
/// 存储与实时事件流不受影响，仍是完整事实。
pub(crate) const HISTORY_TOOL_RESULT_STRING_LIMIT: usize = 2 * 1024;

/// 单个历史页序列化后的字节预算。超出时从最旧的 turn 开始少返回，剩余部分仍通过
/// 现有 cursor 向上翻页；至少保留最新的一个 turn，保证分页语义不变。
pub(crate) const HISTORY_PAGE_BYTE_BUDGET: usize = 1024 * 1024;

/// 按字节预算裁掉页首过旧的 turn，返回是否发生了裁剪。
pub(crate) fn trim_history_page_to_budget(turns: &mut Vec<CanonicalTurn>, budget: usize) -> bool {
    let mut used = 0usize;
    let mut keep_from = turns.len();
    for (index, turn) in turns.iter().enumerate().rev() {
        used += serde_json::to_vec(turn).map_or(0, |bytes| bytes.len());
        if used > budget && keep_from < turns.len() {
            break;
        }
        keep_from = index;
    }
    if keep_from == 0 {
        return false;
    }
    turns.drain(..keep_from);
    true
}

/// 历史页中单个 turn 最多下发的最新条目数。
///
/// turn 是分页与不变量的单位，但一个 agent 长任务可以在同一个 turn 内产生上千条目。
/// 页成本必须只取决于“展示的这一页”，所以超长 turn 只带最新一段，更早的步骤由
/// `/messages/turn-items` 按条目游标向前补齐，并合并回同一个 turn。
pub(crate) const HISTORY_TURN_ITEM_WINDOW: usize = 150;

/// turn.metadata 中记录被窗口折叠的更早条目：`{ omittedItemCount, beforeItemSeq }`。
pub(crate) const HISTORY_WINDOW_METADATA_KEY: &str = "historyWindow";

pub(crate) fn history_page_canonical_turn(mut turn: CanonicalTurn) -> CanonicalTurn {
    apply_history_item_window(&mut turn, HISTORY_TURN_ITEM_WINDOW);
    for item in &mut turn.items {
        compact_history_tool_result(item);
    }
    public_canonical_turn(turn)
}

/// 只保留 turn 最新的 `window` 个条目，外加始终置顶的用户消息（turn 的提问不能丢）。
fn apply_history_item_window(turn: &mut CanonicalTurn, window: usize) {
    turn.normalize();
    if turn.items.len() <= window {
        return;
    }
    let keep_from = turn.items.len() - window;
    let boundary_seq = turn.items[keep_from].item_seq;
    let mut kept = Vec::with_capacity(window + 1);
    let mut omitted = 0usize;
    for (index, item) in std::mem::take(&mut turn.items).into_iter().enumerate() {
        if index >= keep_from || item.kind == CanonicalTurnItemKind::UserMessage {
            kept.push(item);
        } else {
            omitted += 1;
        }
    }
    turn.items = kept;
    turn.metadata.insert(
        HISTORY_WINDOW_METADATA_KEY.to_string(),
        serde_json::json!({ "omittedItemCount": omitted, "beforeItemSeq": boundary_seq }),
    );
}

/// 单个条目的历史页表示：与 `history_page_canonical_turn` 中的条目口径完全一致。
pub(crate) fn history_page_canonical_item(mut item: CanonicalTurnItem) -> CanonicalTurnItem {
    compact_history_tool_result(&mut item);
    public_canonical_turn_item(item)
}

fn compact_history_tool_result(item: &mut CanonicalTurnItem) {
    let Some(result) = item.tool.as_mut().and_then(|tool| tool.result.as_mut()) else {
        return;
    };
    let omitted_chars = truncate_long_strings(result, HISTORY_TOOL_RESULT_STRING_LIMIT);
    if omitted_chars > 0 {
        item.metadata.insert(
            "historyCompaction".to_string(),
            serde_json::json!({ "resultTruncated": true, "omittedChars": omitted_chars }),
        );
    }
}

/// 递归截断超长字符串，返回被省略的字符总数。
fn truncate_long_strings(value: &mut Value, limit: usize) -> usize {
    match value {
        Value::String(text) => {
            // 字节数不超过上限时字符数必然不超过，避免对每个短字符串做字符计数。
            if text.len() <= limit {
                return 0;
            }
            let Some((cut, _)) = text.char_indices().nth(limit) else {
                return 0;
            };
            let omitted = text[cut..].chars().count();
            text.truncate(cut);
            omitted
        }
        Value::Array(items) => items
            .iter_mut()
            .map(|item| truncate_long_strings(item, limit))
            .sum(),
        Value::Object(object) => object
            .values_mut()
            .map(|item| truncate_long_strings(item, limit))
            .sum(),
        _ => 0,
    }
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

/// 只服务于模型 provider 重放的内部 metadata，不是对外事实，前端也不消费。
/// 历史页与实时事件统一在这里剔除，保证同一 item 版本在两条通道上的内容一致。
const INTERNAL_ITEM_METADATA_KEYS: [&str; 2] = ["providerContext", "toolCalls"];

fn public_canonical_turn_item_in_place(item: &mut CanonicalTurnItem) {
    for key in INTERNAL_ITEM_METADATA_KEYS {
        item.metadata.remove(key);
    }
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
    fn history_page_truncates_long_tool_result_strings_and_marks_the_item() {
        let mut item = canonical_tool_item();
        let long = "长".repeat(HISTORY_TOOL_RESULT_STRING_LIMIT + 5);
        item.tool.as_mut().unwrap().result =
            Some(json!({ "stdout": long, "nested": [{ "content": "short" }], "exitCode": 0 }));
        let mut turn = canonical_turn(item);

        turn = history_page_canonical_turn(turn);

        let result = turn.items[0].tool.as_ref().unwrap().result.clone().unwrap();
        assert_eq!(
            result["stdout"].as_str().unwrap().chars().count(),
            HISTORY_TOOL_RESULT_STRING_LIMIT
        );
        assert_eq!(result["nested"][0]["content"], "short");
        assert_eq!(result["exitCode"], 0);
        assert_eq!(
            turn.items[0].metadata["historyCompaction"],
            json!({ "resultTruncated": true, "omittedChars": 5 })
        );
    }

    #[test]
    fn page_budget_drops_oldest_turns_but_always_keeps_the_newest() {
        let make = |id: &str, text: &str| {
            let mut turn = canonical_turn(canonical_tool_item());
            turn.turn_id = id.to_string();
            turn.items[0].content = Some(text.to_string());
            turn
        };
        let mut turns = vec![
            make("t1", &"a".repeat(600)),
            make("t2", &"b".repeat(600)),
            make("t3", &"c".repeat(600)),
        ];
        let one = serde_json::to_vec(&turns[0]).unwrap().len();
        assert!(trim_history_page_to_budget(&mut turns, one * 2 + 10));
        assert_eq!(
            turns
                .iter()
                .map(|turn| turn.turn_id.as_str())
                .collect::<Vec<_>>(),
            ["t2", "t3"]
        );

        let mut giant = vec![make("t4", &"d".repeat(4000))];
        assert!(!trim_history_page_to_budget(&mut giant, 10));
        assert_eq!(giant.len(), 1);

        let mut small = vec![make("t5", "x"), make("t6", "y")];
        assert!(!trim_history_page_to_budget(&mut small, 1 << 20));
        assert_eq!(small.len(), 2);
    }

    #[test]
    fn public_items_drop_internal_provider_replay_metadata() {
        let mut item = canonical_tool_item();
        item.metadata
            .insert("providerContext".to_string(), json!([{ "data": "x" }]));
        item.metadata
            .insert("toolCalls".to_string(), json!([{ "id": "call" }]));
        item.metadata
            .insert("requestId".to_string(), json!("req-1"));
        let public = public_canonical_turn_item(item);
        assert!(!public.metadata.contains_key("providerContext"));
        assert!(!public.metadata.contains_key("toolCalls"));
        assert_eq!(public.metadata["requestId"], "req-1");
    }

    #[test]
    fn history_page_windows_long_turns_to_the_newest_items_and_keeps_the_question() {
        let mut turn = canonical_turn(canonical_tool_item());
        let template = turn.items[0].clone();
        turn.items = (1..=400usize)
            .map(|seq| {
                let mut item = template.clone();
                item.item_seq = seq;
                item.item_id = format!("item-{seq:04}");
                if seq == 1 {
                    item.kind = CanonicalTurnItemKind::UserMessage;
                    item.tool = None;
                }
                item
            })
            .collect();

        let public = history_page_canonical_turn(turn);

        assert_eq!(public.items.len(), HISTORY_TURN_ITEM_WINDOW + 1);
        assert_eq!(public.items[0].item_id, "item-0001", "用户消息必须始终保留");
        assert_eq!(public.items.last().unwrap().item_id, "item-0400");
        let first_windowed = 400 - HISTORY_TURN_ITEM_WINDOW + 1;
        assert_eq!(public.items[1].item_seq, first_windowed);
        assert_eq!(
            public.metadata[HISTORY_WINDOW_METADATA_KEY],
            json!({
                "omittedItemCount": 400 - HISTORY_TURN_ITEM_WINDOW - 1,
                "beforeItemSeq": first_windowed
            })
        );
    }

    #[test]
    fn history_page_does_not_mark_short_turns_as_windowed() {
        let turn = history_page_canonical_turn(canonical_turn(canonical_tool_item()));
        assert!(!turn.metadata.contains_key(HISTORY_WINDOW_METADATA_KEY));
    }

    #[test]
    fn history_page_leaves_short_tool_results_untouched() {
        let mut item = canonical_tool_item();
        item.tool.as_mut().unwrap().result = Some(json!({ "stdout": "ok" }));
        let turn = history_page_canonical_turn(canonical_turn(item));
        assert_eq!(
            turn.items[0].tool.as_ref().unwrap().result,
            Some(json!({ "stdout": "ok" }))
        );
        assert!(!turn.items[0].metadata.contains_key("historyCompaction"));
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

    #[test]
    fn public_canonical_browser_navigate_keeps_http_url() {
        let mut item = canonical_tool_item();
        let tool = item.tool.as_mut().expect("tool should exist");
        tool.name = "browser_navigate".to_string();
        tool.arguments = Some(json!({
            "url": "https://cn.bing.com/search?q=magi-code",
            "include_snapshot": true,
        }));

        let public = public_canonical_turn_item(item);
        let arguments = public
            .tool
            .expect("public tool should exist")
            .arguments
            .expect("arguments should exist");

        assert_eq!(
            arguments["url"],
            json!("https://cn.bing.com/search?q=magi-code")
        );
        assert_ne!(arguments["url"], json!("http[path]"));
    }
}
