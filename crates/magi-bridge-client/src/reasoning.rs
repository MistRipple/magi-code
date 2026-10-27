//! Model reasoning capabilities shared by daemon projection and HTTP execution.
use magi_usage_authority::ReasoningEffort;
use serde_json::Value;

pub use magi_core::model_reasoning::{EFFORTS, ReasoningCapability, ReasoningTransport};

pub fn effort_label(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::None => "none",
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
        ReasoningEffort::Max => "max",
        ReasoningEffort::Ultra => "ultra",
    }
}

/// Exact family boundaries: do not decode proxy aliases or infer future versions.
pub fn builtin_capability(model: &str, anthropic: bool) -> ReasoningCapability {
    use ReasoningEffort::*;
    let model = model.trim().to_ascii_lowercase();
    let family = |name: &str| {
        model == name
            || model
                .strip_prefix(name)
                .and_then(|tail| tail.strip_prefix('-'))
                .is_some_and(|snapshot| snapshot.as_bytes().first().is_some_and(u8::is_ascii_digit))
    };
    let (efforts, default, transport): (&[ReasoningEffort], Option<ReasoningEffort>, _) =
        if anthropic {
            let profile = crate::protocol::resolve_capability_profile(&model);
            match profile.thinking_kind {
                crate::protocol::ThinkingKind::BudgetTokens => (
                    &[Low, Medium, High, Xhigh],
                    Some(Medium),
                    ReasoningTransport::AnthropicBudget,
                ),
                crate::protocol::ThinkingKind::Effort if model.starts_with("claude-opus-4-7") => (
                    &[Low, Medium, High, Xhigh],
                    Some(High),
                    ReasoningTransport::AnthropicEffort,
                ),
                _ => (&[], Option::None, ReasoningTransport::AnthropicEffort),
            }
        } else if family("gpt-5.1-codex") || family("gpt-5-codex") {
            (
                &[Low, Medium, High],
                Some(Medium),
                ReasoningTransport::Openai,
            )
        } else if family("gpt-5.2-codex") || family("gpt-5.3-codex") {
            (
                &[Low, Medium, High, Xhigh],
                Some(Medium),
                ReasoningTransport::Openai,
            )
        } else if family("gpt-6.1-sol") || family("gpt-6-astra") {
            (
                &[Low, Medium, High, Xhigh, Max],
                Some(Medium),
                ReasoningTransport::Openai,
            )
        } else if model == "gpt-5.6"
            || [
                "gpt-5.6-sol",
                "gpt-5.6-luna",
                "gpt-5.6-terra",
                "gpt-6-sol",
                "gpt-6-luna",
            ]
            .iter()
            .any(|name| family(name))
        {
            (
                &[None, Low, Medium, High, Xhigh, Max],
                Some(Medium),
                ReasoningTransport::Openai,
            )
        } else if family("gpt-5.1") {
            (
                &[None, Low, Medium, High],
                Some(None),
                ReasoningTransport::Openai,
            )
        } else if family("gpt-5.2") {
            (
                &[None, Low, Medium, High, Xhigh],
                Some(None),
                ReasoningTransport::Openai,
            )
        } else if ["gpt-5", "gpt-5-mini", "gpt-5-nano"]
            .iter()
            .any(|name| family(name))
            && !model.starts_with("gpt-5.")
        {
            (
                &[Minimal, Low, Medium, High],
                Some(Medium),
                ReasoningTransport::Openai,
            )
        } else if ["o3", "o3-mini", "o4-mini"].iter().any(|name| family(name)) {
            (
                &[Low, Medium, High],
                Some(Medium),
                ReasoningTransport::Openai,
            )
        } else {
            (&[], Option::None, ReasoningTransport::Openai)
        };
    ReasoningCapability {
        supported_efforts: efforts.to_vec(),
        default_effort: default,
        transport,
        source: if !efforts.is_empty()
            || ["gpt-4o", "gpt-4.1", "gpt-3.5"]
                .iter()
                .any(|name| family(name))
        {
            "builtin"
        } else {
            "unknown"
        }
        .into(),
    }
}

/// Accept explicit effort arrays, never infer capability from a generic `reasoning: true` flag.
pub fn provider_capability(entry: &Value, anthropic: bool) -> Option<ReasoningCapability> {
    let descriptor = entry.get("reasoningCapabilities").unwrap_or(entry);
    let values = descriptor
        .get("supportedEfforts")
        .or_else(|| descriptor.get("supportedReasoningEfforts"))
        .or_else(|| descriptor.get("supported_reasoning_efforts"))
        .or_else(|| descriptor.get("supported_reasoning_levels"))?
        .as_array()?;
    let mut efforts = Vec::new();
    for value in values {
        let value = value
            .get("reasoningEffort")
            .or_else(|| value.get("reasoning_effort"))
            .or_else(|| value.get("effort"))
            .unwrap_or(value);
        efforts.push(serde_json::from_value(value.clone()).ok()?);
    }
    let unsupported = efforts.is_empty();
    let mut capability = ReasoningCapability {
        supported_efforts: efforts,
        default_effort: descriptor
            .get("defaultEffort")
            .or_else(|| descriptor.get("defaultReasoningEffort"))
            .or_else(|| descriptor.get("default_reasoning_effort"))
            .or_else(|| descriptor.get("default_reasoning_level"))
            .filter(|value| !unsupported && !value.is_null())
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .ok()?,
        transport: descriptor
            .get("transport")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .ok()?
            .unwrap_or(if anthropic {
                ReasoningTransport::AnthropicEffort
            } else {
                ReasoningTransport::Openai
            }),
        source: "provider".into(),
    };
    if anthropic == (capability.transport == ReasoningTransport::Openai) {
        return None;
    }
    capability.validate().ok()?;
    Some(capability)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn catalog_has_model_specific_defaults_and_unknown_aliases_are_conservative() {
        assert_eq!(
            builtin_capability("gpt-5.6-sol", false)
                .supported_efforts
                .len(),
            6
        );
        assert_eq!(
            builtin_capability("gpt-6.1-sol", false)
                .supported_efforts
                .len(),
            5
        );
        assert_eq!(
            builtin_capability("o3-mini", false).supported_efforts.len(),
            3
        );
        assert!(
            builtin_capability("proxy-gpt-5.6", false)
                .supported_efforts
                .is_empty()
        );
        assert!(
            builtin_capability("claude-proxy-reversed", false)
                .supported_efforts
                .is_empty()
        );
        assert_eq!(
            builtin_capability("gpt-5.1", false).default_effort,
            Some(ReasoningEffort::None)
        );
    }
    #[test]
    fn metadata_preserves_sparse_levels_and_rejects_invalid_defaults() {
        let cap = provider_capability(&json!({"supportedReasoningEfforts":[{"reasoningEffort":"max"},{"reasoningEffort":"low"},{"reasoningEffort":"high"}],"defaultReasoningEffort":"high"}), false).unwrap();
        assert_eq!(
            cap.supported_efforts,
            vec![
                ReasoningEffort::Low,
                ReasoningEffort::High,
                ReasoningEffort::Max
            ]
        );
        assert_eq!(
            cap.reconcile(Some(ReasoningEffort::Medium)),
            Some(ReasoningEffort::High)
        );
        assert!(
            provider_capability(
                &json!({"supportedEfforts":["low"],"defaultEffort":"max"}),
                false
            )
            .is_none()
        );
        assert!(provider_capability(&json!({"reasoning":true}), false).is_none());
        assert!(
            provider_capability(&json!({"supportedEfforts":["future-value"]}), false).is_none()
        );
        assert!(
            provider_capability(&json!({"supportedEfforts":[]}), false)
                .unwrap()
                .supported_efforts
                .is_empty()
        );
        assert!(
            provider_capability(
                &json!({"supportedEfforts":["high"],"defaultEffort":"future-value"}),
                false
            )
            .is_none()
        );
        assert!(
            provider_capability(
                &json!({"supportedEfforts":["high"],"transport":"anthropic_effort"}),
                false
            )
            .is_none()
        );
        let cap = provider_capability(&json!({"reasoningCapabilities":{"supported_reasoning_efforts":[{"reasoning_effort":"high"}],"default_reasoning_effort":"high","transport":"anthropic_budget"}}), true).unwrap();
        assert_eq!(cap.transport, ReasoningTransport::AnthropicBudget);
        assert_eq!(cap.default_effort, Some(ReasoningEffort::High));
    }

    #[test]
    fn codex_catalog_preserves_real_effort_levels_and_empty_support() {
        let entry = json!({"slug":"glm-5.3", "supported_reasoning_levels":[{"effort":"low","description":"light"},{"effort":"high"},{"effort":"max"}],"default_reasoning_level":"max"});
        for anthropic in [false, true] {
            let capability = provider_capability(&entry, anthropic).unwrap();
            assert_eq!(
                capability.supported_efforts,
                vec![
                    ReasoningEffort::Low,
                    ReasoningEffort::High,
                    ReasoningEffort::Max
                ]
            );
            assert_eq!(capability.default_effort, Some(ReasoningEffort::Max));
        }
        let disabled = provider_capability(&json!({"slug":"kiro-claude-haiku-4-5", "supported_reasoning_levels":[],"default_reasoning_level":"medium"}), true).unwrap();
        assert!(disabled.supported_efforts.is_empty());
        assert_eq!(disabled.default_effort, None);
        assert!(
            provider_capability(
                &json!({"supported_reasoning_levels":[{"effort":"invented"}]}),
                false
            )
            .is_none()
        );
    }

    #[test]
    fn unlisted_variants_do_not_inherit_a_parent_models_efforts() {
        for model in [
            "gpt-5-pro",
            "gpt-5.2-pro",
            "gpt-5.6-sol-custom",
            "gpt-6.1-sol-proxy",
        ] {
            assert!(
                builtin_capability(model, false)
                    .supported_efforts
                    .is_empty(),
                "{model}"
            );
        }
        assert_eq!(
            builtin_capability("gpt-5.6-sol-2026-03-05", false)
                .supported_efforts
                .len(),
            6
        );
        assert_eq!(
            builtin_capability("gpt-6-sol", false)
                .supported_efforts
                .len(),
            6
        );
    }
}
