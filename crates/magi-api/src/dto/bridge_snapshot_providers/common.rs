use magi_bridge_client::{BridgeServerKind, BridgeTransport};
use magi_core::public_runtime_excerpt;
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct BridgeTransportBinding {
    pub(super) server_kind: BridgeServerKind,
    pub(super) transport: Arc<dyn BridgeTransport>,
}

pub(super) fn excerpt(value: &str) -> String {
    public_runtime_excerpt(value, 120)
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
