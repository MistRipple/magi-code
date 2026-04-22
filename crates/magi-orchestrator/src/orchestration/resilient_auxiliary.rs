use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClassification {
    AuthOrQuota,
    Connection,
    Model,
    Config,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryDecision {
    Retry { delay_ms: u64 },
    Fallback,
    Fail,
}

pub fn classify_error(status: Option<u16>, error_code: &str, message: &str) -> ErrorClassification {
    if matches!(status, Some(401) | Some(403) | Some(429)) {
        return ErrorClassification::AuthOrQuota;
    }
    let lower = message.to_lowercase();
    if lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("invalid api key")
        || lower.contains("quota")
        || lower.contains("rate limit")
        || lower.contains("billing")
        || lower.contains("insufficient")
    {
        return ErrorClassification::AuthOrQuota;
    }

    if matches!(status, Some(408) | Some(502) | Some(503) | Some(504)) {
        return ErrorClassification::Connection;
    }
    let connection_codes = [
        "ETIMEDOUT",
        "ECONNRESET",
        "ECONNREFUSED",
        "ENOTFOUND",
        "EAI_AGAIN",
    ];
    if connection_codes.iter().any(|c| error_code == *c) {
        return ErrorClassification::Connection;
    }
    if lower.contains("timeout")
        || lower.contains("connection")
        || lower.contains("network")
        || lower.contains("fetch failed")
        || lower.contains("socket hang up")
    {
        return ErrorClassification::Connection;
    }

    if lower.contains("model")
        && (lower.contains("not found")
            || lower.contains("unknown")
            || lower.contains("invalid")
            || lower.contains("unsupported"))
    {
        return ErrorClassification::Model;
    }

    if lower.contains("not configured")
        || lower.contains("invalid configuration")
        || lower.contains("disabled in config")
    {
        return ErrorClassification::Config;
    }

    ErrorClassification::Unknown
}

const RETRY_DELAYS: &[u64] = &[10_000, 20_000, 30_000];

pub fn decide_retry(
    classification: ErrorClassification,
    attempt: usize,
) -> RetryDecision {
    match classification {
        ErrorClassification::AuthOrQuota => RetryDecision::Fallback,
        ErrorClassification::Connection => {
            if attempt < RETRY_DELAYS.len() {
                RetryDecision::Retry {
                    delay_ms: RETRY_DELAYS[attempt],
                }
            } else {
                RetryDecision::Fallback
            }
        }
        ErrorClassification::Model | ErrorClassification::Config => RetryDecision::Fallback,
        ErrorClassification::Unknown => RetryDecision::Fail,
    }
}

pub fn should_fallback_to_orchestrator(classification: ErrorClassification) -> bool {
    matches!(
        classification,
        ErrorClassification::AuthOrQuota
            | ErrorClassification::Connection
            | ErrorClassification::Model
            | ErrorClassification::Config
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelLabel {
    Auxiliary,
    Orchestrator,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuxiliaryResponse {
    pub content: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub model_label: ModelLabel,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_401_as_auth() {
        assert_eq!(
            classify_error(Some(401), "", ""),
            ErrorClassification::AuthOrQuota
        );
    }

    #[test]
    fn classify_429_as_auth() {
        assert_eq!(
            classify_error(Some(429), "", ""),
            ErrorClassification::AuthOrQuota
        );
    }

    #[test]
    fn classify_quota_message() {
        assert_eq!(
            classify_error(None, "", "Quota exceeded for this billing period"),
            ErrorClassification::AuthOrQuota
        );
    }

    #[test]
    fn classify_502_as_connection() {
        assert_eq!(
            classify_error(Some(502), "", ""),
            ErrorClassification::Connection
        );
    }

    #[test]
    fn classify_etimedout_code() {
        assert_eq!(
            classify_error(None, "ETIMEDOUT", ""),
            ErrorClassification::Connection
        );
    }

    #[test]
    fn classify_timeout_message() {
        assert_eq!(
            classify_error(None, "", "Connection timed out"),
            ErrorClassification::Connection
        );
    }

    #[test]
    fn classify_model_not_found() {
        assert_eq!(
            classify_error(None, "", "Model not found: gpt-5"),
            ErrorClassification::Model
        );
    }

    #[test]
    fn classify_config_error() {
        assert_eq!(
            classify_error(None, "", "disabled in config"),
            ErrorClassification::Config
        );
    }

    #[test]
    fn classify_unknown() {
        assert_eq!(
            classify_error(None, "", "something weird happened"),
            ErrorClassification::Unknown
        );
    }

    #[test]
    fn retry_connection_first_attempt() {
        assert_eq!(
            decide_retry(ErrorClassification::Connection, 0),
            RetryDecision::Retry { delay_ms: 10_000 }
        );
    }

    #[test]
    fn retry_connection_third_attempt() {
        assert_eq!(
            decide_retry(ErrorClassification::Connection, 2),
            RetryDecision::Retry { delay_ms: 30_000 }
        );
    }

    #[test]
    fn retry_connection_exhausted() {
        assert_eq!(
            decide_retry(ErrorClassification::Connection, 3),
            RetryDecision::Fallback
        );
    }

    #[test]
    fn auth_always_fallback() {
        assert_eq!(
            decide_retry(ErrorClassification::AuthOrQuota, 0),
            RetryDecision::Fallback
        );
    }

    #[test]
    fn unknown_always_fail() {
        assert_eq!(
            decide_retry(ErrorClassification::Unknown, 0),
            RetryDecision::Fail
        );
    }

    #[test]
    fn fallback_decisions() {
        assert!(should_fallback_to_orchestrator(ErrorClassification::AuthOrQuota));
        assert!(should_fallback_to_orchestrator(ErrorClassification::Connection));
        assert!(should_fallback_to_orchestrator(ErrorClassification::Model));
        assert!(!should_fallback_to_orchestrator(ErrorClassification::Unknown));
    }
}
