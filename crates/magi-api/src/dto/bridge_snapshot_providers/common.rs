use magi_bridge_client::{BridgeServerKind, BridgeTransport};
use serde_json::Value;
use std::sync::Arc;

const REDACTED_VALUE: &str = "[redacted]";
const REDACTED_PATH: &str = "[path]";

#[derive(Clone)]
pub(super) struct BridgeTransportBinding {
    pub(super) server_kind: BridgeServerKind,
    pub(super) transport: Arc<dyn BridgeTransport>,
}

pub(super) fn excerpt(value: &str) -> String {
    let public_value = public_bridge_excerpt_source(value);
    let mut chars = public_value.chars();
    let excerpt: String = chars.by_ref().take(120).collect();
    if chars.next().is_some() {
        format!("{excerpt}...")
    } else {
        excerpt
    }
}

fn public_bridge_excerpt_source(value: &str) -> String {
    match serde_json::from_str::<Value>(value) {
        Ok(mut parsed) => {
            redact_json_value(&mut parsed);
            serde_json::to_string(&parsed).unwrap_or_else(|_| redact_sensitive_text(value))
        }
        Err(_) => redact_sensitive_text(value),
    }
}

fn redact_json_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if is_sensitive_key(key) {
                    *value = Value::String(REDACTED_VALUE.to_string());
                } else {
                    redact_json_value(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_json_value(item);
            }
        }
        Value::String(text) => {
            *text = redact_sensitive_text(text);
        }
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "api_key"
            | "apikey"
            | "authorization"
            | "access_token"
            | "refresh_token"
            | "token"
            | "secret"
            | "client_secret"
            | "password"
    )
}

fn redact_sensitive_text(value: &str) -> String {
    let value = redact_bearer_tokens(value);
    let value = redact_prefixed_tokens(&value);
    redact_absolute_paths(&value)
}

fn redact_bearer_tokens(value: &str) -> String {
    let mut redacted = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(pos) = remaining.find("Bearer ") {
        redacted.push_str(&remaining[..pos + "Bearer ".len()]);
        redacted.push_str(REDACTED_VALUE);
        let after = &remaining[pos + "Bearer ".len()..];
        let token_end = after
            .char_indices()
            .find(|(_, ch)| is_value_delimiter(*ch))
            .map(|(idx, _)| idx)
            .unwrap_or(after.len());
        remaining = &after[token_end..];
    }
    redacted.push_str(remaining);
    redacted
}

fn redact_prefixed_tokens(value: &str) -> String {
    let mut redacted = String::with_capacity(value.len());
    let mut index = 0usize;
    while let Some(relative_pos) = value[index..].find("sk-") {
        let start = index + relative_pos;
        redacted.push_str(&value[index..start]);
        let token_end = value[start..]
            .char_indices()
            .find(|(_, ch)| !is_token_char(*ch))
            .map(|(idx, _)| start + idx)
            .unwrap_or(value.len());
        if token_end > start + 3 {
            redacted.push_str("sk-");
            redacted.push_str(REDACTED_VALUE);
            index = token_end;
        } else {
            redacted.push_str("sk-");
            index = start + 3;
        }
    }
    redacted.push_str(&value[index..]);
    redacted
}

fn redact_absolute_paths(value: &str) -> String {
    let mut redacted = String::with_capacity(value.len());
    let mut index = 0usize;
    while index < value.len() {
        let Some(ch) = value[index..].chars().next() else {
            break;
        };
        if ch == '/' && starts_sensitive_absolute_path(&value[index..]) {
            redacted.push_str(REDACTED_PATH);
            let path_end = value[index..]
                .char_indices()
                .find(|(_, ch)| is_path_delimiter(*ch))
                .map(|(idx, _)| index + idx)
                .unwrap_or(value.len());
            index = path_end;
            continue;
        }
        redacted.push(ch);
        index += ch.len_utf8();
    }
    redacted
}

fn starts_sensitive_absolute_path(value: &str) -> bool {
    value.starts_with("/Users/")
        || value.starts_with("/private/")
        || value.starts_with("/var/folders/")
        || value.starts_with("/tmp/")
        || value.starts_with("/Volumes/")
}

fn is_value_delimiter(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '"' | '\'' | ',' | ';' | ')' | ']' | '}')
}

fn is_path_delimiter(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '"' | '\'' | ',' | ';' | ')' | ']' | '}')
}

fn is_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_')
}

#[cfg(test)]
mod tests {
    use super::excerpt;

    #[test]
    fn excerpt_redacts_sensitive_json_fields_paths_and_tokens() {
        let excerpt = excerpt(
            r#"{"api_key":"sk-live-secret","message":"failed at /Users/xie/.magi/token with Bearer abcdef","nested":{"password":"hidden"}}"#,
        );

        assert!(excerpt.contains(r#""api_key":"[redacted]""#));
        assert!(excerpt.contains(r#""password":"[redacted]""#));
        assert!(excerpt.contains("[path]"));
        assert!(excerpt.contains("Bearer [redacted]"));
        assert!(!excerpt.contains("sk-live-secret"));
        assert!(!excerpt.contains("/Users/xie"));
        assert!(!excerpt.contains("abcdef"));
        assert!(!excerpt.contains("hidden"));
    }

    #[test]
    fn excerpt_redacts_plain_text_paths_and_tokens_before_truncating() {
        let excerpt = excerpt(
            "provider rejected token sk-test-secret at /private/tmp/magi/config with Authorization: Bearer abcdef",
        );

        assert!(excerpt.contains("sk-[redacted]"));
        assert!(excerpt.contains("[path]"));
        assert!(excerpt.contains("Bearer [redacted]"));
        assert!(!excerpt.contains("sk-test-secret"));
        assert!(!excerpt.contains("/private/tmp"));
        assert!(!excerpt.contains("abcdef"));
    }
}
