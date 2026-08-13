use magi_core::SessionId;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

const SESSION_SECTION_PREFIX: &str = "__session__:";
pub const ORCHESTRATOR_SESSION_DEFAULTS_SECTION: &str = "orchestratorSessionDefaults";
const PUBLIC_RESPONSE_ALIAS_SECTIONS: &[&str] = &[
    "workerConfigs",
    "orchestratorConfig",
    "auxiliaryConfig",
    "imageGenerationConfig",
    "userRulesConfig",
    "registryEngines",
    "registryAgents",
];
pub const DEPRECATED_MODEL_CONFIG_FIELDS: &[&str] =
    &["provider", "openaiProtocol", "protocolEndpoint"];

#[derive(Debug)]
pub struct SettingsStore {
    sections: RwLock<HashMap<String, Value>>,
    /// 持久化文件路径，为 None 时仅内存模式。
    persistence_path: Option<PathBuf>,
}

impl Default for SettingsStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for SettingsStore {
    fn clone(&self) -> Self {
        let sections = self.sections.read().unwrap().clone();
        Self {
            sections: RwLock::new(sections),
            persistence_path: self.persistence_path.clone(),
        }
    }
}

impl SettingsStore {
    pub fn new() -> Self {
        Self {
            sections: RwLock::new(HashMap::new()),
            persistence_path: None,
        }
    }

    /// 创建带持久化路径的 SettingsStore，启动后需调用 load_from_disk 恢复数据
    pub fn with_persistence_path(path: PathBuf) -> Self {
        Self {
            sections: RwLock::new(HashMap::new()),
            persistence_path: Some(path),
        }
    }

    /// 为一次执行创建只读配置快照。
    ///
    /// 快照复制当前所有 section，但不携带持久化路径，避免执行期工具或模型解析路径误写
    /// 用户全局设置。运行中用户修改模型配置时，已接受的 turn / 任务树继续读取这份快照。
    pub fn execution_snapshot(&self) -> Self {
        let sections = self.sections.read().unwrap().clone();
        Self {
            sections: RwLock::new(sections),
            persistence_path: None,
        }
    }

    /// 从磁盘 JSON 文件加载设置，文件不存在时使用空默认值
    pub fn load_from_disk(&self) -> Result<(), std::io::Error> {
        let path = match &self.persistence_path {
            Some(p) => p,
            None => return Ok(()),
        };
        if !path.exists() {
            return Ok(());
        }
        let content = fs::read_to_string(path)?;
        match serde_json::from_str::<HashMap<String, Value>>(&content) {
            Ok(mut data) => {
                let changed = canonicalize_settings_sections(&mut data);
                if changed {
                    Self::save_sections(path, &data)?;
                }
                *self.sections.write().unwrap() = data;
            }
            Err(error) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("设置文件解析失败 {}: {error}", path.display()),
                ));
            }
        }
        Ok(())
    }

    /// 将当前设置保存到磁盘 JSON 文件（原子写入）
    pub fn save_to_disk(&self) -> Result<(), std::io::Error> {
        let path = match &self.persistence_path {
            Some(p) => p,
            None => return Ok(()),
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let sections = self.sections.read().unwrap();
        Self::save_sections(path, &sections)
    }

    fn save_sections(path: &Path, sections: &HashMap<String, Value>) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_vec_pretty(sections).map_err(std::io::Error::other)?;
        magi_core::fs_atomic::write_atomic(path, content)
    }

    fn mutate<R>(
        &self,
        mutation: impl FnOnce(&mut HashMap<String, Value>) -> R,
    ) -> Result<R, std::io::Error> {
        let mut sections = self.sections.write().unwrap();
        let previous = self.persistence_path.as_ref().map(|_| sections.clone());
        let result = mutation(&mut sections);
        if let Some(path) = self.persistence_path.as_ref()
            && let Err(error) = Self::save_sections(path, &sections)
        {
            if let Some(previous) = previous {
                *sections = previous;
            }
            return Err(error);
        }
        Ok(result)
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        self.sections.read().unwrap().get(key).cloned()
    }

    pub fn set(&self, key: &str, value: Value) -> Result<(), std::io::Error> {
        if is_public_response_alias_section(key) {
            return self.remove_section(key);
        }
        let mut value = value;
        canonicalize_settings_section_value(key, &mut value);
        self.mutate(|sections| {
            sections.insert(key.to_string(), value);
        })
    }

    pub fn get_section(&self, section: &str) -> Value {
        self.sections
            .read()
            .unwrap()
            .get(section)
            .cloned()
            .unwrap_or(Value::Null)
    }

    pub fn set_section(&self, section: &str, value: Value) -> Result<(), std::io::Error> {
        if is_public_response_alias_section(section) {
            return self.remove_section(section);
        }
        let mut value = value;
        canonicalize_settings_section_value(section, &mut value);
        self.mutate(|sections| {
            sections.insert(section.to_string(), value);
        })
    }

    pub fn remove_section(&self, section: &str) -> Result<(), std::io::Error> {
        self.mutate(|sections| {
            sections.remove(section);
        })
    }

    pub fn apply_section_changes(
        &self,
        updates: impl IntoIterator<Item = (String, Value)>,
        removals: impl IntoIterator<Item = String>,
    ) -> Result<(), std::io::Error> {
        let updates = updates
            .into_iter()
            .map(|(section, mut value)| {
                canonicalize_settings_section_value(&section, &mut value);
                (section, value)
            })
            .collect::<Vec<_>>();
        let removals = removals.into_iter().collect::<Vec<_>>();
        self.mutate(|sections| {
            for section in removals {
                sections.remove(&section);
            }
            for (section, value) in updates {
                if is_public_response_alias_section(&section) {
                    sections.remove(&section);
                } else {
                    sections.insert(section, value);
                }
            }
        })
    }

    pub fn get_session_section(&self, session_id: &SessionId, section: &str) -> Value {
        self.get_section(&session_section_key(session_id, section))
    }

    pub fn set_session_section(
        &self,
        session_id: &SessionId,
        section: &str,
        value: Value,
    ) -> Result<(), std::io::Error> {
        self.set_section(&session_section_key(session_id, section), value)
    }

    /// 原子更新一个会话 section 与一个全局 section。
    ///
    /// 会话模型选择同时承担“当前会话配置”和“后续新会话默认值”两项职责，
    /// 两者必须在同一次持久化事务中提交，避免写入失败时产生相互矛盾的状态。
    pub fn set_session_and_global_sections(
        &self,
        session_id: &SessionId,
        session_section: &str,
        session_value: Value,
        global_section: &str,
        global_value: Value,
    ) -> Result<(), std::io::Error> {
        let session_key = session_section_key(session_id, session_section);
        let mut session_value = session_value;
        let mut global_value = global_value;
        canonicalize_settings_section_value(&session_key, &mut session_value);
        canonicalize_settings_section_value(global_section, &mut global_value);
        self.mutate(|sections| {
            sections.insert(session_key, session_value);
            sections.insert(global_section.to_string(), global_value);
        })
    }

    /// 在持久化写锁内读取并更新单个 section。
    ///
    /// 用于“仅当尚未初始化时写入默认值”这类读改写操作，防止并发请求
    /// 分别读取旧值后互相覆盖。
    pub fn update_section<R>(
        &self,
        section: &str,
        update: impl FnOnce(&mut Value) -> R,
    ) -> Result<R, std::io::Error> {
        let section = section.to_string();
        self.mutate(|sections| {
            let value = sections.entry(section.clone()).or_insert(Value::Null);
            let result = update(value);
            canonicalize_settings_section_value(&section, value);
            result
        })
    }

    pub fn remove_session_section(
        &self,
        session_id: &SessionId,
        section: &str,
    ) -> Result<(), std::io::Error> {
        self.remove_section(&session_section_key(session_id, section))
    }

    /// 删除一个 session 拥有的全部设置 section。会话级模型与推理强度属于会话，
    /// 不能在会话删除后继续留在全局 settings 文件中。
    pub fn remove_session(&self, session_id: &SessionId) -> Result<usize, std::io::Error> {
        let prefix = format!("{SESSION_SECTION_PREFIX}{}:", session_id.as_str());
        self.mutate(|sections| {
            let before = sections.len();
            sections.retain(|key, _| !key.starts_with(&prefix));
            before.saturating_sub(sections.len())
        })
    }

    pub fn remove_section_entry(&self, section: &str, key: &str) -> Result<(), std::io::Error> {
        self.mutate(|sections| {
            if let Some(Value::Object(map)) = sections.get_mut(section) {
                map.remove(key);
            }
        })
    }

    pub fn upsert_array_entry(
        &self,
        section: &str,
        id_field: &str,
        entry: &Value,
    ) -> Result<(), std::io::Error> {
        let Some(id_val) = Self::extract_id_str(entry, id_field).map(ToOwned::to_owned) else {
            return Ok(());
        };
        self.mutate(|sections| {
            let arr = sections
                .entry(section.to_string())
                .or_insert_with(|| Value::Array(vec![]));
            let Value::Array(items) = arr else {
                return;
            };
            if let Some(pos) = items
                .iter()
                .position(|item| Self::extract_id_str(item, id_field) == Some(id_val.as_str()))
            {
                items[pos] = entry.clone();
            } else {
                items.push(entry.clone());
            }
            canonicalize_settings_section_value(section, arr);
        })
    }

    pub fn remove_array_entry(
        &self,
        section: &str,
        id_field: &str,
        id_value: &str,
    ) -> Result<(), std::io::Error> {
        self.mutate(|sections| {
            if let Some(Value::Array(items)) = sections.get_mut(section) {
                items.retain(|item| {
                    Self::extract_id_str(item, id_field)
                        .map(|v| v != id_value)
                        .unwrap_or(true)
                });
            }
        })
    }

    fn extract_id_str<'a>(item: &'a Value, primary_field: &str) -> Option<&'a str> {
        if let Some(s) = item.get(primary_field).and_then(|v| v.as_str()) {
            return Some(s);
        }
        if let Some(s) = item.get("id").and_then(|v| v.as_str()) {
            return Some(s);
        }
        if let Some(s) = item.get("serverId").and_then(|v| v.as_str()) {
            return Some(s);
        }
        if let Some(s) = item.get("repositoryId").and_then(|v| v.as_str()) {
            return Some(s);
        }
        if let Some(s) = item.get("engineId").and_then(|v| v.as_str()) {
            return Some(s);
        }

        None
    }

    pub fn snapshot(&self) -> HashMap<String, Value> {
        self.sections.read().unwrap().clone()
    }

    pub fn public_snapshot(&self) -> HashMap<String, Value> {
        self.sections
            .read()
            .unwrap()
            .iter()
            .filter(|(key, _)| !is_session_section_key(key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

fn canonicalize_settings_sections(sections: &mut HashMap<String, Value>) -> bool {
    let mut changed = false;
    for section in PUBLIC_RESPONSE_ALIAS_SECTIONS {
        changed |= sections.remove(*section).is_some();
    }
    for (section, value) in sections.iter_mut() {
        changed |= canonicalize_settings_section_value(section, value);
    }
    changed
}

fn is_public_response_alias_section(section: &str) -> bool {
    PUBLIC_RESPONSE_ALIAS_SECTIONS.contains(&section)
}

fn canonicalize_settings_section_value(section: &str, value: &mut Value) -> bool {
    if section == "orchestrator" {
        return canonicalize_global_orchestrator_section(value);
    }
    if section == ORCHESTRATOR_SESSION_DEFAULTS_SECTION
        || (is_session_section_key(section) && section.ends_with(":orchestrator"))
    {
        return canonicalize_session_orchestrator_section(value);
    }
    if section == "auxiliary"
        || section == "imageGeneration"
        || (is_session_section_key(section) && section.ends_with(":auxiliary"))
    {
        return canonicalize_model_config(value);
    }
    match section {
        "workers" => normalize_workers_section(value),
        "engines" => normalize_engines_section(value),
        _ => false,
    }
}

fn canonicalize_global_orchestrator_section(value: &mut Value) -> bool {
    let Some(object) = value.as_object_mut() else {
        return false;
    };
    let mut changed = object.remove("model").is_some();
    changed |= object.remove("reasoningEffort").is_some();
    changed |= canonicalize_model_config(value);
    changed
}

fn canonicalize_session_orchestrator_section(value: &mut Value) -> bool {
    let Some(object) = value.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    object.retain(|key, _| {
        let keep = matches!(key.as_str(), "model" | "reasoningEffort");
        if !keep {
            changed = true;
        }
        keep
    });
    changed
}

fn normalize_workers_section(value: &mut Value) -> bool {
    let Some(workers) = value.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    for config in workers.values_mut() {
        changed |= canonicalize_model_config(config);
    }
    changed
}

fn normalize_engines_section(value: &mut Value) -> bool {
    let Some(engines) = value.as_array_mut() else {
        return false;
    };
    let mut changed = false;
    for engine in engines {
        if let Some(llm) = engine.get_mut("llm") {
            changed |= canonicalize_model_config(llm);
        }
    }
    changed
}

fn canonicalize_model_config(value: &mut Value) -> bool {
    let Some(object) = value.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    let has_connection_fields = [
        "baseUrl",
        "apiKey",
        "model",
        "urlMode",
        "provider",
        "openaiProtocol",
        "protocolEndpoint",
    ]
    .iter()
    .any(|field| object.contains_key(*field));
    if has_connection_fields && !object.contains_key("apiProtocol") {
        let legacy_provider_is_anthropic = object
            .get("provider")
            .and_then(Value::as_str)
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("anthropic"));
        let endpoint_is_anthropic = object
            .get("baseUrl")
            .and_then(Value::as_str)
            .is_some_and(is_explicit_anthropic_endpoint)
            || object
                .get("protocolEndpoint")
                .and_then(Value::as_str)
                .is_some_and(is_explicit_anthropic_endpoint);
        object.insert(
            "apiProtocol".to_string(),
            Value::String(
                if legacy_provider_is_anthropic || endpoint_is_anthropic {
                    "anthropic_messages"
                } else {
                    "openai_chat"
                }
                .to_string(),
            ),
        );
        changed = true;
    }
    changed | remove_deprecated_model_fields(value)
}

fn is_explicit_anthropic_endpoint(value: &str) -> bool {
    let normalized = value.trim().trim_end_matches('/').to_ascii_lowercase();
    normalized.contains("api.anthropic.com")
        || normalized.ends_with("/anthropic")
        || normalized.ends_with("/messages")
}

fn remove_deprecated_model_fields(value: &mut Value) -> bool {
    let Some(object) = value.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    for field in DEPRECATED_MODEL_CONFIG_FIELDS {
        changed |= object.remove(*field).is_some();
    }
    changed
}

fn session_section_key(session_id: &SessionId, section: &str) -> String {
    format!("{SESSION_SECTION_PREFIX}{}:{section}", session_id.as_str())
}

fn is_session_section_key(section: &str) -> bool {
    section.starts_with(SESSION_SECTION_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn persistence_round_trip_saves_and_loads_settings() {
        let dir = std::env::temp_dir().join(format!(
            "magi-settings-test-round-trip-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");

        let store = SettingsStore::with_persistence_path(path.clone());
        store.set("theme", json!("dark")).unwrap();
        store
            .set_section("workers", json!({"primary": "gpu-0"}))
            .unwrap();
        assert!(path.exists(), "设置文件应已被自动创建");

        // 用新实例从磁盘加载
        let store2 = SettingsStore::with_persistence_path(path);
        store2.load_from_disk().unwrap();
        assert_eq!(store2.get("theme"), Some(json!("dark")));
        assert_eq!(store2.get_section("workers"), json!({"primary": "gpu-0"}));
    }

    #[test]
    fn load_from_disk_tolerates_missing_file() {
        let path = std::env::temp_dir().join("magi-settings-test-missing-file-never-exists.json");
        let store = SettingsStore::with_persistence_path(path);
        assert!(store.load_from_disk().is_ok());
        assert!(store.snapshot().is_empty());
    }

    #[test]
    fn load_from_disk_rejects_corrupt_settings_without_clearing_memory() {
        let dir = std::env::temp_dir().join(format!(
            "magi-settings-test-corrupt-load-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let store = SettingsStore::with_persistence_path(path.clone());
        store.set("theme", json!("dark")).unwrap();
        std::fs::write(&path, b"{not-json").unwrap();

        let error = store
            .load_from_disk()
            .expect_err("损坏的设置文件必须显式失败");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(store.get("theme"), Some(json!("dark")));
        assert_eq!(std::fs::read(&path).unwrap(), b"{not-json");
    }

    #[test]
    fn failed_persistence_does_not_publish_uncommitted_setting_to_memory() {
        let root = std::env::temp_dir().join(format!(
            "magi-settings-test-persist-failure-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::write(&root, b"parent-is-a-file").unwrap();
        let store = SettingsStore::with_persistence_path(root.join("settings.json"));

        let result = store.set("theme", json!("dark"));

        assert!(result.is_err());
        assert_eq!(store.get("theme"), None);
    }

    #[test]
    fn apply_section_changes_persists_updates_and_removals_as_one_snapshot() {
        let dir = std::env::temp_dir().join(format!(
            "magi-settings-test-batch-mutation-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let store = SettingsStore::with_persistence_path(path.clone());
        store
            .set_section("legacy", json!({"enabled": true}))
            .unwrap();

        store
            .apply_section_changes(
                [("skillsConfig".to_string(), json!({"customTools": []}))],
                ["legacy".to_string()],
            )
            .unwrap();

        let reloaded = SettingsStore::with_persistence_path(path);
        reloaded.load_from_disk().unwrap();
        assert_eq!(
            reloaded.get_section("skillsConfig"),
            json!({"customTools": []})
        );
        assert_eq!(reloaded.get_section("legacy"), Value::Null);
    }

    #[test]
    fn pure_memory_mode_does_not_write_files() {
        let store = SettingsStore::new();
        store.set("key", json!("value")).unwrap();
        // 纯内存模式不应产生任何磁盘操作
        assert_eq!(store.get("key"), Some(json!("value")));
    }

    #[test]
    fn execution_snapshot_is_detached_from_later_mutations() {
        let store = SettingsStore::new();
        store
            .set_section(
                "orchestrator",
                json!({
                    "baseUrl": "https://old.example.com/v1",
                    "model": "model-old",
                }),
            )
            .unwrap();

        let snapshot = store.execution_snapshot();
        store
            .set_section(
                "orchestrator",
                json!({
                    "baseUrl": "https://new.example.com/v1",
                    "model": "model-new",
                }),
            )
            .unwrap();

        assert_eq!(
            snapshot.get_section("orchestrator"),
            json!({
                "baseUrl": "https://old.example.com/v1",
                "apiProtocol": "openai_chat",
            })
        );
        assert_eq!(
            store.get_section("orchestrator"),
            json!({
                "baseUrl": "https://new.example.com/v1",
                "apiProtocol": "openai_chat",
            })
        );
    }

    #[test]
    fn auto_persist_on_array_and_section_mutations() {
        let dir = std::env::temp_dir().join(format!(
            "magi-settings-test-auto-persist-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");

        let store = SettingsStore::with_persistence_path(path.clone());
        store
            .upsert_array_entry(
                "engines",
                "engineId",
                &json!({"engineId": "e1", "name": "test"}),
            )
            .unwrap();
        store
            .remove_array_entry("engines", "engineId", "e1")
            .unwrap();
        store.set_section("config", json!({"a": 1})).unwrap();
        store.remove_section_entry("config", "a").unwrap();

        // 验证最终状态被持久化
        let store2 = SettingsStore::with_persistence_path(path);
        store2.load_from_disk().unwrap();
        assert_eq!(store2.get_section("engines"), json!([]));
        assert_eq!(store2.get_section("config"), json!({}));
    }

    #[test]
    fn load_from_disk_canonicalizes_model_sections_and_public_aliases() {
        let dir = std::env::temp_dir().join(format!(
            "magi-settings-test-canonicalize-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "orchestrator": {
                    "provider": "anthropic",
                    "openaiProtocol": "responses",
                    "protocolEndpoint": "/v1/responses",
                    "baseUrl": "https://api.example.com",
                    "model": "main",
                    "reasoningEffort": "high"
                },
                "workers": {
                    "reviewer": {
                        "provider": "openai",
                        "baseUrl": "https://api.example.com",
                        "model": "worker"
                    }
                },
                "engines": [{
                    "id": "reviewer",
                    "llm": {
                        "provider": "openai",
                        "baseUrl": "https://api.example.com",
                        "model": "worker"
                    }
                }],
                "orchestratorConfig": { "model": "alias-main" },
                "workerConfigs": { "alias-worker": { "model": "alias" } },
                "registryEngines": []
            }))
            .unwrap(),
        )
        .unwrap();

        let store = SettingsStore::with_persistence_path(path.clone());
        store.load_from_disk().unwrap();

        assert!(store.get_section("orchestrator").get("provider").is_none());
        assert_eq!(
            store.get_section("orchestrator")["apiProtocol"],
            json!("anthropic_messages")
        );
        assert!(
            store
                .get_section("orchestrator")
                .get("openaiProtocol")
                .is_none()
        );
        assert!(
            store.get_section("workers")["reviewer"]
                .get("provider")
                .is_none()
        );
        assert_eq!(
            store.get_section("workers")["reviewer"]["apiProtocol"],
            json!("openai_chat")
        );
        assert!(
            store.get_section("engines")[0]["llm"]
                .get("provider")
                .is_none()
        );
        assert_eq!(
            store.get_section("engines")[0]["llm"]["apiProtocol"],
            json!("openai_chat")
        );
        assert_eq!(store.get_section("orchestratorConfig"), Value::Null);
        assert_eq!(store.get_section("workerConfigs"), Value::Null);
        assert_eq!(store.get_section("registryEngines"), Value::Null);

        let persisted: HashMap<String, Value> =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(!persisted.contains_key("orchestratorConfig"));
        assert!(!persisted.contains_key("workerConfigs"));
        assert!(!persisted.contains_key("registryEngines"));
        assert!(
            persisted["orchestrator"]
                .as_object()
                .unwrap()
                .get("provider")
                .is_none()
        );
        assert_eq!(
            persisted["orchestrator"]["apiProtocol"],
            json!("anthropic_messages")
        );
        assert!(
            persisted["orchestrator"]
                .as_object()
                .unwrap()
                .get("model")
                .is_none()
        );
        assert!(
            persisted["orchestrator"]
                .as_object()
                .unwrap()
                .get("reasoningEffort")
                .is_none()
        );
    }

    #[test]
    fn session_orchestrator_section_keeps_only_session_owned_fields() {
        let store = SettingsStore::new();
        let session_id = SessionId::new("session-main-model");
        store
            .set_session_section(
                &session_id,
                "orchestrator",
                json!({
                    "baseUrl": "https://api.example.com/v1",
                    "apiKey": "sk-session-should-not-own",
                    "urlMode": "standard",
                    "model": "session-main",
                    "reasoningEffort": "xhigh",
                    "previousModel": "legacy-main",
                    "modelSwitchPending": true,
                    "provider": "openai"
                }),
            )
            .unwrap();

        let section = store.get_session_section(&session_id, "orchestrator");
        assert_eq!(section["model"], json!("session-main"));
        assert_eq!(section["reasoningEffort"], json!("xhigh"));
        assert!(section.get("baseUrl").is_none());
        assert!(section.get("apiKey").is_none());
        assert!(section.get("urlMode").is_none());
        assert!(section.get("previousModel").is_none());
        assert!(section.get("modelSwitchPending").is_none());
        assert!(section.get("provider").is_none());
    }

    #[test]
    fn orchestrator_session_defaults_keep_only_session_owned_fields() {
        let store = SettingsStore::new();
        store
            .set_section(
                ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
                json!({
                    "model": "model-default",
                    "reasoningEffort": "high",
                    "apiKey": "must-not-persist",
                    "baseUrl": "https://must-not-persist.example.com"
                }),
            )
            .unwrap();

        assert_eq!(
            store.get_section(ORCHESTRATOR_SESSION_DEFAULTS_SECTION),
            json!({
                "model": "model-default",
                "reasoningEffort": "high"
            })
        );
    }

    #[test]
    fn session_and_default_model_are_persisted_together() {
        let store = SettingsStore::new();
        let session_id = SessionId::new("session-model-default-transaction");
        let config = json!({
            "model": "model-current",
            "reasoningEffort": "xhigh",
            "apiKey": "must-not-persist"
        });

        store
            .set_session_and_global_sections(
                &session_id,
                "orchestrator",
                config.clone(),
                ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
                config,
            )
            .unwrap();

        let expected = json!({
            "model": "model-current",
            "reasoningEffort": "xhigh"
        });
        assert_eq!(
            store.get_session_section(&session_id, "orchestrator"),
            expected
        );
        assert_eq!(
            store.get_section(ORCHESTRATOR_SESSION_DEFAULTS_SECTION),
            expected
        );
    }

    #[test]
    fn array_upsert_uses_only_top_level_canonical_ids() {
        let store = SettingsStore::new();
        store
            .upsert_array_entry(
                "engines",
                "id",
                &json!({
                    "id": "reviewer",
                    "llm": {
                        "provider": "openai",
                        "model": "old-worker"
                    }
                }),
            )
            .unwrap();
        store
            .upsert_array_entry(
                "engines",
                "id",
                &json!({
                    "engine": {
                        "id": "reviewer"
                    },
                    "llm": {
                        "model": "wrapped-worker"
                    }
                }),
            )
            .unwrap();
        store
            .upsert_array_entry(
                "engines",
                "id",
                &json!({
                    "id": "reviewer",
                    "llm": {
                        "model": "current-worker"
                    }
                }),
            )
            .unwrap();

        let engines = store.get_section("engines");
        let engines = engines.as_array().expect("engines should be array");
        assert_eq!(engines.len(), 1);
        assert_eq!(engines[0]["llm"]["model"], json!("current-worker"));
        assert_eq!(engines[0]["llm"]["apiProtocol"], json!("openai_chat"));
        assert!(engines[0]["llm"].get("provider").is_none());
    }

    #[test]
    fn migration_does_not_infer_protocol_from_model_name() {
        let mut sections = HashMap::from([(
            "auxiliary".to_string(),
            json!({
                "baseUrl": "https://gateway.example.com/v1",
                "model": "claude-sonnet",
                "urlMode": "standard"
            }),
        )]);

        assert!(canonicalize_settings_sections(&mut sections));
        assert_eq!(sections["auxiliary"]["apiProtocol"], json!("openai_chat"));
    }

    #[test]
    fn session_scoped_sections_do_not_leak_into_public_snapshot() {
        let store = SettingsStore::new();
        let session_a = SessionId::new("session-a");
        let session_b = SessionId::new("session-b");

        store
            .set_section("workers", json!({"primary": "gpu-0"}))
            .unwrap();
        store
            .set_session_section(&session_a, "userRules", json!({"userRules": "A"}))
            .unwrap();
        store
            .set_session_section(&session_b, "userRules", json!({"userRules": "B"}))
            .unwrap();

        assert_eq!(
            store.get_session_section(&session_a, "userRules"),
            json!({"userRules": "A"})
        );
        assert_eq!(
            store.get_session_section(&session_b, "userRules"),
            json!({"userRules": "B"})
        );

        let snapshot = store.public_snapshot();
        assert_eq!(snapshot.get("workers"), Some(&json!({"primary": "gpu-0"})));
        assert!(!snapshot.contains_key("__session__:session-a:userRules"));
        assert!(!snapshot.contains_key("__session__:session-b:userRules"));
    }

    #[test]
    fn remove_session_drops_all_session_owned_sections() {
        let store = SettingsStore::new();
        let session_a = SessionId::new("session-a");
        let session_b = SessionId::new("session-b");
        store
            .set_session_section(&session_a, "orchestrator", json!({"model": "model-a"}))
            .unwrap();
        store
            .set_session_section(&session_a, "userRules", json!({"userRules": "A"}))
            .unwrap();
        store
            .set_session_section(&session_b, "orchestrator", json!({"model": "model-b"}))
            .unwrap();

        assert_eq!(store.remove_session(&session_a).unwrap(), 2);
        assert_eq!(
            store.get_session_section(&session_a, "orchestrator"),
            Value::Null
        );
        assert_eq!(
            store.get_session_section(&session_a, "userRules"),
            Value::Null
        );
        assert_eq!(
            store.get_session_section(&session_b, "orchestrator")["model"],
            json!("model-b")
        );
    }
}
