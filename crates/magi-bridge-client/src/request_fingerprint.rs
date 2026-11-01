//! 模型请求前缀指纹诊断。
//!
//! 仅在设置 `MAGI_MODEL_REQUEST_FINGERPRINT_DIR` 时启用：把每次请求拆成
//! tools / instructions / input 各项的哈希写入 JSONL，用于比对连续请求的第一个
//! 不同位置，定位提示词缓存失效点。只写哈希、角色、长度和系统/开发者片段的
//! 片段标识，不写任何用户或工具内容；`MAGI_MODEL_REQUEST_FINGERPRINT_FULL` 额外打开时
//! 才会写完整请求体，只用于本机字节级比对。
//!
//! 每条记录还带 `prefix`：与同一会话键、同一模型、同为带工具/不带工具的上一次请求相比，
//! 前缀是否只是追加（`append_only`，上一次请求末尾的易变尾部除外），还是被改写
//! （`history_rewritten`，附第一个不同的位置和片段标识），或是工具表、指令、参数变了；
//! 没有工具表的单消息请求是独立调用（`standalone_request`），不适用追加合同。

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

const FINGERPRINT_DIR_ENV: &str = "MAGI_MODEL_REQUEST_FINGERPRINT_DIR";

struct PreviousRequest {
    hashes: Vec<String>,
    tags: Vec<Option<String>>,
    volatile_tail_items: usize,
    tools_hash: Option<String>,
    instructions_hash: Option<String>,
    other_hash: String,
}

const MAX_TRACKED_STREAMS: usize = 256;

fn previous_requests() -> &'static Mutex<HashMap<String, PreviousRequest>> {
    static PREVIOUS: OnceLock<Mutex<HashMap<String, PreviousRequest>>> = OnceLock::new();
    PREVIOUS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 与同一条请求流的上一次请求比较，给出前缀变化的原因。
fn prefix_change(key: String, current: PreviousRequest) -> Value {
    let mut previous = previous_requests()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if previous.len() >= MAX_TRACKED_STREAMS && !previous.contains_key(&key) {
        previous.clear();
    }
    let report = match previous.get(&key) {
        None => json!({"reason": "first_request"}),
        Some(before) => {
            let common = before
                .hashes
                .iter()
                .zip(&current.hashes)
                .take_while(|(a, b)| a == b)
                .count();
            let kept_history = before
                .hashes
                .len()
                .saturating_sub(before.volatile_tail_items);
            // 单条消息、没有工具表的请求（标题、强化等辅助调用）没有追加历史的合同，
            // 内容变化不是前缀被改写。
            let standalone = current.tools_hash.is_none()
                && before.hashes.len() == 1
                && current.hashes.len() == 1;
            let reason = if before.tools_hash != current.tools_hash {
                "tools_changed"
            } else if before.instructions_hash != current.instructions_hash {
                "instructions_changed"
            } else if before.other_hash != current.other_hash {
                "parameters_changed"
            } else if common >= kept_history {
                "append_only"
            } else if standalone {
                "standalone_request"
            } else {
                "history_rewritten"
            };
            json!({
                "reason": reason,
                "previousItems": before.hashes.len(),
                "previousVolatileTailItems": before.volatile_tail_items,
                "commonItems": common,
                "firstDifferentIndex": (reason == "history_rewritten").then_some(common),
                "firstDifferentTag": (reason == "history_rewritten")
                    .then(|| before.tags.get(common).cloned().flatten().or_else(|| current.tags.get(common).cloned().flatten()))
                    .flatten(),
            })
        }
    };
    previous.insert(key, current);
    report
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn value_hash(value: &Value) -> String {
    format!("{:016x}", fnv1a(value.to_string().as_bytes()))
}

/// 系统/开发者片段只暴露 `<magi-developer-fragment id="…">` 这一段标识。
fn fragment_tag(item: &Value) -> Option<String> {
    let role = item["role"].as_str()?;
    if !matches!(role, "system" | "developer") {
        return None;
    }
    let text = item["content"]
        .as_str()
        .or_else(|| item["content"][0]["text"].as_str())?;
    let start = text.find("<magi-developer-fragment id=\"")?;
    let rest = &text[start + "<magi-developer-fragment id=\"".len()..];
    Some(rest.split('"').next()?.to_string())
}

pub(crate) fn record_request_fingerprint(
    model: &str,
    url_path: &str,
    body: &Value,
    volatile_tail_items: usize,
) {
    let Some(dir) = std::env::var_os(FINGERPRINT_DIR_ENV) else {
        return;
    };
    let items = body
        .get("input")
        .or_else(|| body.get("messages"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    json!({
                        "h": value_hash(item),
                        "t": item["type"].as_str().or_else(|| item["role"].as_str()),
                        "role": item["role"].as_str(),
                        "len": item.to_string().len(),
                        "tag": fragment_tag(item),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let tool_names = body["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    tool["name"]
                        .as_str()
                        .or_else(|| tool["function"]["name"].as_str())
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let other_hash = value_hash(&json!({
        "reasoning": body.get("reasoning"),
        "tool_choice": body.get("tool_choice"),
        "temperature": body.get("temperature"),
        "max_output_tokens": body.get("max_output_tokens"),
    }));
    let tools_hash = body.get("tools").map(value_hash);
    let instructions_hash = body
        .get("instructions")
        .or_else(|| body.get("system"))
        .map(value_hash);
    let prefix = prefix_change(
        format!(
            "{}|{model}|{}",
            body.get("prompt_cache_key")
                .and_then(Value::as_str)
                .unwrap_or("-"),
            tools_hash.is_some()
        ),
        PreviousRequest {
            hashes: items
                .iter()
                .map(|item| item["h"].as_str().unwrap_or_default().to_string())
                .collect(),
            tags: items
                .iter()
                .map(|item| item["tag"].as_str().map(str::to_string))
                .collect(),
            volatile_tail_items,
            tools_hash: tools_hash.clone(),
            instructions_hash: instructions_hash.clone(),
            other_hash: other_hash.clone(),
        },
    );
    let record = json!({
        "prefix": prefix,
        "ts": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        "model": model,
        "path": url_path,
        "promptCacheKey": body.get("prompt_cache_key"),
        "toolsHash": tools_hash,
        "toolNames": tool_names,
        "instructionsHash": instructions_hash,
        "otherHash": other_hash,
        "items": items,
    });
    // 仅用于本机排查字节级差异：额外环境变量打开时，把完整请求体写到同目录的 bodies/。
    if std::env::var_os("MAGI_MODEL_REQUEST_FINGERPRINT_FULL").is_some() {
        let bodies = std::path::Path::new(&dir).join("bodies");
        let _ = std::fs::create_dir_all(&bodies);
        let ts = record["ts"].as_u64().unwrap_or(0);
        let _ = std::fs::write(bodies.join(format!("{ts}.json")), body.to_string());
    }
    let path = std::path::Path::new(&dir).join("requests.jsonl");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{record}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(key: &str, items: &[&str], tail: usize) -> (String, PreviousRequest) {
        (
            key.to_string(),
            PreviousRequest {
                hashes: items.iter().map(|item| item.to_string()).collect(),
                tags: items.iter().map(|_| None).collect(),
                volatile_tail_items: tail,
                tools_hash: Some("tools".into()),
                instructions_hash: None,
                other_hash: "other".into(),
            },
        )
    }

    #[test]
    fn prefix_report_separates_append_only_growth_from_rewrites() {
        let key = "test-prefix-report";
        let (_, first) = stream(key, &["h1", "h2", "tail-a"], 1);
        assert_eq!(prefix_change(key.into(), first)["reason"], "first_request");

        // 上一次的易变尾部被替换、历史只追加：不算前缀被改写。
        let (_, appended) = stream(key, &["h1", "h2", "h3", "tail-b"], 1);
        let report = prefix_change(key.into(), appended);
        assert_eq!(report["reason"], "append_only");
        assert_eq!(report["commonItems"], 2);

        // 已发送的历史项内容变了：报告第一个不同的位置。
        let (_, rewritten) = stream(key, &["h1", "CHANGED", "h3", "h4", "tail-c"], 1);
        let report = prefix_change(key.into(), rewritten);
        assert_eq!(report["reason"], "history_rewritten");
        assert_eq!(report["firstDifferentIndex"], 1);

        // 单条消息且没有工具表：独立请求，不报告为历史改写。
        let standalone_key = "test-standalone-report";
        let (_, mut one) = stream(standalone_key, &["h1"], 0);
        one.tools_hash = None;
        assert_eq!(
            prefix_change(standalone_key.into(), one)["reason"],
            "first_request"
        );
        let (_, mut other) = stream(standalone_key, &["h2"], 0);
        other.tools_hash = None;
        assert_eq!(
            prefix_change(standalone_key.into(), other)["reason"],
            "standalone_request"
        );

        let (_, mut tools) = stream(key, &["h1", "CHANGED", "h3", "h4", "h5"], 0);
        tools.tools_hash = Some("other-tools".into());
        assert_eq!(prefix_change(key.into(), tools)["reason"], "tools_changed");
    }
}
