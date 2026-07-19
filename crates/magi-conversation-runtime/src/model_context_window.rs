use magi_settings_store::SettingsStore;
use magi_usage_authority::resolve_context_window;
use serde_json::{Map, Value};

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
