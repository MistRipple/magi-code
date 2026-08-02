use magi_core::{EventId, UtcMillis};
use magi_event_bus::{EventEnvelope, InMemoryEventBus};
use magi_settings_store::SettingsStore;
use magi_usage_authority::resolve_context_window;
use serde_json::{Map, Value};
use std::sync::Arc;

pub const MODEL_CONTEXT_WINDOWS_SECTION: &str = "modelContextWindows";
pub const MIN_MODEL_CONTEXT_WINDOW: u64 = 16_000;
pub const MAX_MODEL_CONTEXT_WINDOW: u64 = 10_000_000;

pub fn normalize_model_context_key(model: &str) -> String {
    model.trim().to_ascii_lowercase()
}

pub fn configured_model_context_window(
    settings_store: Option<&SettingsStore>,
    model: &str,
) -> Option<u64> {
    let key = normalize_model_context_key(model);
    if key.is_empty() {
        return None;
    }
    settings_store?
        .get_section(MODEL_CONTEXT_WINDOWS_SECTION)
        .as_object()?
        .get(&key)?
        .as_u64()
        .filter(|value| (MIN_MODEL_CONTEXT_WINDOW..=MAX_MODEL_CONTEXT_WINDOW).contains(value))
}

pub fn resolve_model_context_window(settings_store: Option<&SettingsStore>, model: &str) -> u64 {
    configured_model_context_window(settings_store, model)
        .unwrap_or_else(|| resolve_context_window(model).max(0) as u64)
}

pub fn set_model_context_window(
    settings_store: &SettingsStore,
    model: &str,
    context_window_tokens: u64,
) -> Result<Map<String, Value>, String> {
    let key = normalize_model_context_key(model);
    if key.is_empty() {
        return Err("模型名称不能为空".to_string());
    }
    if !(MIN_MODEL_CONTEXT_WINDOW..=MAX_MODEL_CONTEXT_WINDOW).contains(&context_window_tokens) {
        return Err(format!(
            "上下文窗口必须在 {MIN_MODEL_CONTEXT_WINDOW} 到 {MAX_MODEL_CONTEXT_WINDOW} token 之间"
        ));
    }

    let mut entries = settings_store
        .get_section(MODEL_CONTEXT_WINDOWS_SECTION)
        .as_object()
        .cloned()
        .unwrap_or_default();
    entries.insert(key, Value::from(context_window_tokens));
    settings_store
        .set_section(
            MODEL_CONTEXT_WINDOWS_SECTION,
            Value::Object(entries.clone()),
        )
        .map_err(|error| format!("保存模型上下文窗口失败：{error}"))?;
    Ok(entries)
}

pub(crate) fn apply_reported_context_limit(
    event_bus: &InMemoryEventBus,
    execution_settings_store: Option<&Arc<SettingsStore>>,
    live_settings_store: Option<&Arc<SettingsStore>>,
    model: &str,
    context_limit: u64,
) -> bool {
    let Some(primary_store) = live_settings_store.or(execution_settings_store) else {
        return false;
    };
    let entries = match set_model_context_window(primary_store.as_ref(), model, context_limit) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                model,
                context_limit,
                %error,
                "保存上游返回的模型上下文窗口失败"
            );
            return false;
        }
    };
    if let Some(store) = execution_settings_store {
        let _ = set_model_context_window(store.as_ref(), model, context_limit);
    }
    let updated_at = UtcMillis::now();
    let _ = event_bus.publish(EventEnvelope::domain(
        EventId::new(format!(
            "event-model-context-window-updated-{}",
            updated_at.0
        )),
        "model.context_window.updated",
        serde_json::json!({
            "model": model,
            "contextWindowTokens": context_limit,
            "modelContextWindows": entries,
            "source": "provider_error",
            "updatedAt": updated_at.0,
        }),
    ));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_window_is_shared_by_normalized_model_name() {
        let store = SettingsStore::new();
        set_model_context_window(&store, " GPT-5.6 ", 1_000_000).unwrap();
        assert_eq!(
            configured_model_context_window(Some(&store), "gpt-5.6"),
            Some(1_000_000)
        );
        assert_eq!(
            resolve_model_context_window(Some(&store), "GPT-5.6"),
            1_000_000
        );
    }

    #[test]
    fn resolver_uses_builtin_window_without_user_configuration() {
        assert_eq!(resolve_model_context_window(None, "gpt-4.1"), 1_000_000);
    }
}
