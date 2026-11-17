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
            if let Ok(mut nested) = serde_json::from_str::<Value>(text)
                && (nested.is_object() || nested.is_array())
            {
                redact_json_value(&mut nested);
                *text = nested.to_string();
            } else {
                *text = redact_sensitive_text(text);
            }
        }
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    if ["_token", "_secret", "_password", "_api_key"]
        .iter()
        .any(|suffix| key.ends_with(suffix))
    {
        return true;
    }
    matches!(
        key.as_str(),
        "api_key"
            | "apikey"
            | "accesstoken"
            | "refreshtoken"
            | "clientsecret"
            | "authorization"
            | "access_token"
            | "auth_token"
            | "refresh_token"
            | "id_token"
            | "session_token"
            | "token"
            | "secret"
            | "client_secret"
            | "private_key"
            | "password"
            | "credential"
            | "credentials"
            | "x-api-key"
            | "api-key"
            | "cookie"
            | "set-cookie"
    )
}

fn redact_sensitive_text(value: &str) -> String {
    let value = redact_bearer_tokens(value);
    let value = redact_prefixed_tokens(&value);
    let value = redact_named_credentials(&value);
    let value = redact_url_userinfo(&value);
    redact_absolute_paths(&value)
}

/// Errors often embed headers, URL queries or JSON in otherwise plain text.
/// Redact their named values as well as structured JSON fields.
fn redact_named_credentials(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut index = 0;
    let mut copied = 0;
    while index < value.len() {
        let byte = value.as_bytes()[index];
        if !byte.is_ascii_alphabetic() {
            index += 1;
            continue;
        }
        let start = index;
        while index < value.len()
            && (value.as_bytes()[index].is_ascii_alphanumeric()
                || matches!(value.as_bytes()[index], b'_' | b'-'))
        {
            index += 1;
        }
        if !is_sensitive_key(&value[start..index]) {
            continue;
        }
        let mut cursor = index;
        if matches!(value.as_bytes().get(cursor), Some(b'"' | b'\'')) {
            cursor += 1;
        }
        while value
            .as_bytes()
            .get(cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            cursor += 1;
        }
        if !matches!(value.as_bytes().get(cursor), Some(b':' | b'=')) {
            continue;
        }
        cursor += 1;
        while value
            .as_bytes()
            .get(cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            cursor += 1;
        }
        let quote = value
            .as_bytes()
            .get(cursor)
            .copied()
            .filter(|b| matches!(b, b'"' | b'\''));
        if quote.is_some() {
            cursor += 1;
        }
        let value_start = cursor;
        while cursor < value.len() {
            let ch = value.as_bytes()[cursor];
            if quote.is_some_and(|quote| ch == quote) {
                break;
            }
            if quote.is_none()
                && (ch.is_ascii_whitespace()
                    || matches!(
                        ch,
                        b'&' | b'#' | b',' | b';' | b')' | b']' | b'}' | b'"' | b'\''
                    ))
            {
                break;
            }
            if quote.is_some() && ch == b'\\' && cursor + 1 < value.len() {
                cursor += 1;
            }
            cursor += 1;
        }
        output.push_str(&value[copied..value_start]);
        output.push_str(PUBLIC_REDACTED_VALUE);
        copied = cursor;
        index = cursor;
    }
    output.push_str(&value[copied..]);
    output
}

fn redact_url_userinfo(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(scheme_end) = remaining.find("://") {
        let authority_start = scheme_end + 3;
        let authority_end = remaining[authority_start..]
            .find(|ch: char| ch.is_whitespace() || matches!(ch, '/' | '?' | '#' | '"' | '\''))
            .map(|offset| authority_start + offset)
            .unwrap_or(remaining.len());
        if let Some(at) = remaining[authority_start..authority_end].rfind('@') {
            output.push_str(&remaining[..authority_start]);
            output.push_str(PUBLIC_REDACTED_VALUE);
            remaining = &remaining[authority_start + at..];
        } else {
            output.push_str(&remaining[..authority_end]);
            remaining = &remaining[authority_end..];
        }
    }
    output.push_str(remaining);
    output
}

fn redact_bearer_tokens(value: &str) -> String {
    const BEARER_PREFIX: &str = "bearer ";
    let mut redacted = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(pos) = find_ascii_case_insensitive(remaining, BEARER_PREFIX) {
        redacted.push_str(&remaining[..pos + BEARER_PREFIX.len()]);
        redacted.push_str(PUBLIC_REDACTED_VALUE);
        let after = &remaining[pos + BEARER_PREFIX.len()..];
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

fn find_ascii_case_insensitive(value: &str, needle: &str) -> Option<usize> {
    value
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
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
        let token_body_len = token_end.saturating_sub(start + 3);
        if token_body_len >= 8 && is_token_start_boundary(value, start) {
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

fn is_token_start_boundary(value: &str, start: usize) -> bool {
    if start == 0 {
        return true;
    }
    value[..start]
        .chars()
        .next_back()
        .is_none_or(|ch| !is_token_char(ch))
}

fn redact_absolute_paths(value: &str) -> String {
    let mut redacted = String::with_capacity(value.len());
    let mut index = 0usize;
    while index < value.len() {
        let Some(ch) = value[index..].chars().next() else {
            break;
        };
        if starts_sensitive_absolute_path(&value[index..]) {
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
        || value.starts_with("/home/")
        || value.starts_with("/root/")
        || value.starts_with("/private/")
        || value.starts_with("/var/folders/")
        || value.starts_with("/tmp/")
        || value.starts_with("/Volumes/")
        || starts_windows_absolute_path(value)
}

fn starts_windows_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
        && bytes.get(3).is_none_or(|next| *next != bytes[2])
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
            r#"{"api_key":"sk-live-secret","x-api-key":"raw-key","private_key":"raw-private","message":"failed at /Users/xie/.magi/token with Bearer abcdef","nested":{"password":"hidden"}}"#,
        );

        assert!(public.contains(r#""api_key":"[redacted]""#));
        assert!(public.contains(r#""x-api-key":"[redacted]""#));
        assert!(public.contains(r#""private_key":"[redacted]""#));
        assert!(public.contains(r#""password":"[redacted]""#));
        assert!(public.contains(PUBLIC_REDACTED_PATH));
        assert!(public.contains("Bearer [redacted]"));
        assert!(!public.contains("sk-live-secret"));
        assert!(!public.contains("/Users/xie"));
        assert!(!public.contains("abcdef"));
        assert!(!public.contains("hidden"));
        assert!(!public.contains("raw-key"));
        assert!(!public.contains("raw-private"));
    }

    #[test]
    fn public_runtime_text_preserves_task_ids_with_task_spawn_prefix() {
        let public = public_runtime_text(
            r#"{"child_task_id":"task-spawn-task-local-agent-1783524657851-1783524669760-0","worker_id":"worker-spawn-task-local-agent-1783524657851"}"#,
        );

        assert!(public.contains("task-spawn-task-local-agent-1783524657851-1783524669760-0"));
        assert!(public.contains("worker-spawn-task-local-agent-1783524657851"));
        assert!(!public.contains("task-[redacted]"));
        assert!(!public.contains("worker-spawn-task-[redacted]"));
    }

    #[test]
    fn public_runtime_summary_redacts_and_normalizes_plain_text() {
        let public = public_runtime_summary(Some(
            "provider rejected token sk-test-secret\nat /private/tmp/magi/config and /home/xie/.magi with bearer abcdef",
        ))
        .expect("summary should be public");

        assert!(public.contains("sk-[redacted]"));
        assert!(public.contains(PUBLIC_REDACTED_PATH));
        assert!(public.contains("bearer [redacted]"));
        assert!(!public.contains("sk-test-secret"));
        assert!(!public.contains("/private/tmp"));
        assert!(!public.contains("/home/xie"));
        assert!(!public.contains("abcdef"));
        assert!(!public.contains('\n'));
    }

    #[test]
    fn public_runtime_text_redacts_windows_absolute_paths() {
        let public = public_runtime_text(r#"{"message":"failed at C:\\Users\\xie\\.magi\\token"}"#);

        assert!(public.contains(PUBLIC_REDACTED_PATH));
        assert!(!public.contains("C:\\Users"));
        assert!(!public.contains(".magi"));
    }

    #[test]
    fn public_runtime_text_preserves_http_urls() {
        let public = public_runtime_text(
            r#"{"http":"http://127.0.0.1:38123/web.html","https":"https://cn.bing.com/search?q=magi-code"}"#,
        );

        assert!(public.contains(r#""http":"http://127.0.0.1:38123/web.html""#));
        assert!(public.contains(r#""https":"https://cn.bing.com/search?q=magi-code""#));
        assert!(!public.contains(PUBLIC_REDACTED_PATH));
    }

    #[test]
    fn public_runtime_text_redacts_embedded_diagnostic_credentials() {
        let raw = serde_json::json!({
            "error": "capture target is unavailable; api_key=raw-secret; https://user:pass@example.com/mcp?access_token=query-secret",
            "content": "{\"receipt\":{\"status\":\"failed\",\"retry\":\"after_recheck\"},\"authorization\":\"header-secret\"}",
            "structuredContent": {"receipt": {"dispatch": "not_applicable"}, "cookie": "cookie-secret"}
        }).to_string();
        let public = public_runtime_text(&raw);
        for secret in [
            "raw-secret",
            "user:pass",
            "query-secret",
            "header-secret",
            "cookie-secret",
        ] {
            assert!(!public.contains(secret), "leaked {secret}: {public}");
        }
        for diagnostic in [
            "capture target is unavailable",
            "after_recheck",
            "not_applicable",
            "example.com",
        ] {
            assert!(public.contains(diagnostic));
        }
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
