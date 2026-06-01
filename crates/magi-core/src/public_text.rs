use serde_json::Value;

pub const PUBLIC_REDACTED_VALUE: &str = "[redacted]";
pub const PUBLIC_REDACTED_PATH: &str = "[path]";
pub const PUBLIC_RUNTIME_SUMMARY_MAX_CHARS: usize = 240;

pub fn public_runtime_text(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }

    match serde_json::from_str::<Value>(value) {
        Ok(mut parsed) => {
            redact_json_value(&mut parsed);
            serde_json::to_string(&parsed).unwrap_or_else(|_| redact_sensitive_text(value))
        }
        Err(_) => redact_sensitive_text(value),
    }
}

pub fn public_runtime_summary(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }

    let public = public_runtime_text(value);
    let normalized = public.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        None
    } else {
        Some(truncate_chars(
            &normalized,
            PUBLIC_RUNTIME_SUMMARY_MAX_CHARS,
        ))
    }
}

pub fn public_runtime_excerpt(value: &str, max_chars: usize) -> String {
    truncate_chars(&public_runtime_text(value), max_chars)
}

fn redact_json_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if is_sensitive_key(key) {
                    *value = Value::String(PUBLIC_REDACTED_VALUE.to_string());
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
        redacted.push_str(PUBLIC_REDACTED_VALUE);
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
            redacted.push_str(PUBLIC_REDACTED_VALUE);
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
            redacted.push_str(PUBLIC_REDACTED_PATH);
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

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let mut chars = value.chars();
    let excerpt: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{excerpt}...")
    } else {
        excerpt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_runtime_text_redacts_sensitive_json_fields_paths_and_tokens() {
        let public = public_runtime_text(
            r#"{"api_key":"sk-live-secret","message":"failed at /Users/xie/.magi/token with Bearer abcdef","nested":{"password":"hidden"}}"#,
        );

        assert!(public.contains(r#""api_key":"[redacted]""#));
        assert!(public.contains(r#""password":"[redacted]""#));
        assert!(public.contains(PUBLIC_REDACTED_PATH));
        assert!(public.contains("Bearer [redacted]"));
        assert!(!public.contains("sk-live-secret"));
        assert!(!public.contains("/Users/xie"));
        assert!(!public.contains("abcdef"));
        assert!(!public.contains("hidden"));
    }

    #[test]
    fn public_runtime_summary_redacts_and_normalizes_plain_text() {
        let public = public_runtime_summary(Some(
            "provider rejected token sk-test-secret\nat /private/tmp/magi/config with Bearer abcdef",
        ))
        .expect("summary should be public");

        assert!(public.contains("sk-[redacted]"));
        assert!(public.contains(PUBLIC_REDACTED_PATH));
        assert!(public.contains("Bearer [redacted]"));
        assert!(!public.contains("sk-test-secret"));
        assert!(!public.contains("/private/tmp"));
        assert!(!public.contains("abcdef"));
        assert!(!public.contains('\n'));
    }

    #[test]
    fn public_runtime_excerpt_truncates_after_redaction() {
        let public = public_runtime_excerpt(
            "failed at /Users/xie/.magi/config because sk-secret-value was rejected",
            24,
        );

        assert!(public.len() > 24);
        assert!(public.ends_with("..."));
        assert!(!public.contains("/Users/xie"));
        assert!(!public.contains("sk-secret-value"));
    }
}
