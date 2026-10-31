//! 模型请求前缀指纹诊断。
//!
//! 仅在设置 `MAGI_MODEL_REQUEST_FINGERPRINT_DIR` 时启用：把每次请求拆成
//! tools / instructions / input 各项的哈希写入 JSONL，用于比对连续请求的第一个
//! 不同位置，定位提示词缓存失效点。只写哈希、角色、长度和系统/开发者片段的
//! 片段标识，不写任何用户或工具内容；`MAGI_MODEL_REQUEST_FINGERPRINT_FULL` 额外打开时
//! 才会写完整请求体，只用于本机字节级比对。

use serde_json::{Value, json};
use std::io::Write;

const FINGERPRINT_DIR_ENV: &str = "MAGI_MODEL_REQUEST_FINGERPRINT_DIR";

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

pub(crate) fn record_request_fingerprint(model: &str, url_path: &str, body: &Value) {
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
    let record = json!({
        "ts": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        "model": model,
        "path": url_path,
        "toolsHash": body.get("tools").map(value_hash),
        "toolNames": tool_names,
        "instructionsHash": body.get("instructions").or_else(|| body.get("system")).map(value_hash),
        "otherHash": value_hash(&json!({
            "reasoning": body.get("reasoning"),
            "tool_choice": body.get("tool_choice"),
            "temperature": body.get("temperature"),
            "max_output_tokens": body.get("max_output_tokens"),
        })),
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
