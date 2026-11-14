//! 插件状态文件的版本迁移。当前版本只有一个生命周期状态 `enabled`。
use super::*;

fn corrupt(error: impl ToString) -> PluginError {
    PluginError::CorruptState(error.to_string())
}

/// 读取状态文件；版本 2 的“启用 + 激活 + 授权”三层状态折叠为单一 `enabled` 后落盘。
/// 折叠规则与用户看到的结果一致：原来处于激活（即实际在运行）的插件保持启用，
/// 只启用未激活或已停用的插件保持停用；授权记录丢弃，权限随清单全部授予。
pub(super) fn read_state(root: &Path, bytes: &[u8]) -> Result<PluginManagerState, PluginError> {
    let mut value: Value = serde_json::from_slice(bytes).map_err(corrupt)?;
    match value.get("schema_version").and_then(Value::as_u64) {
        Some(version) if version == u64::from(STATE_VERSION) => {
            serde_json::from_value(value).map_err(corrupt)
        }
        Some(2) => {
            write_atomic(
                &root.join(format!("{STATE_FILE}.schema-v2-{}", UtcMillis::now().0)),
                bytes,
            )?;
            migrate_v2(&mut value);
            let state: PluginManagerState = serde_json::from_value(value).map_err(corrupt)?;
            write_atomic(
                &root.join(STATE_FILE),
                serde_json::to_vec_pretty(&state).map_err(corrupt)?,
            )?;
            Ok(state)
        }
        _ => Err(PluginError::CorruptState("插件状态版本不受支持".into())),
    }
}

fn migrate_v2(value: &mut Value) {
    if let Some(plugins) = value.get_mut("plugins").and_then(Value::as_object_mut) {
        for plugin in plugins.values_mut() {
            fold_installed(plugin);
        }
    }
    if let Some(removals) = value.get_mut("removals").and_then(Value::as_object_mut) {
        for removal in removals.values_mut() {
            if let Some(installed) = removal.get_mut("installed") {
                fold_installed(installed);
            }
        }
    }
    value["schema_version"] = Value::from(STATE_VERSION);
}

fn fold_installed(installed: &mut Value) {
    let Some(object) = installed.as_object_mut() else {
        return;
    };
    let enabled = object
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && object
            .get("active")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    object.remove("active");
    object.remove("grants");
    object.insert("enabled".into(), Value::from(enabled));
}
