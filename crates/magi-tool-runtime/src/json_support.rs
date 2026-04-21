use serde_json::Value;
use std::path::PathBuf;

pub(crate) fn parse_json_object(input: &str) -> Option<serde_json::Map<String, Value>> {
    serde_json::from_str::<Value>(input)
        .ok()
        .and_then(|value| value.as_object().cloned())
}

pub(crate) fn field_string(
    object: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .map(|value| value.to_string())
    })
}

pub(crate) fn field_usize(
    object: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<usize> {
    keys.iter().find_map(|key| {
        object.get(*key).and_then(|value| {
            value.as_u64().map(|value| value as usize).or_else(|| {
                value
                    .as_str()
                    .and_then(|value| value.parse::<usize>().ok())
            })
        })
    })
}

pub(crate) fn field_bool(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|key| {
        object.get(*key).and_then(|value| {
            value
                .as_bool()
                .or_else(|| value.as_str().and_then(|value| value.parse::<bool>().ok()))
        })
    })
}

pub(crate) fn resolve_path(input: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(input);
    if path.is_absolute() {
        Ok(path)
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|error| format!("无法解析当前目录: {error}"))
    }
}
