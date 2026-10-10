use magi_core::SessionId;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

const SESSION_SECTION_PREFIX: &str = "__session__:";
pub const ORCHESTRATOR_SESSION_DEFAULTS_SECTION: &str = "orchestratorSessionDefaults";
/// 设置响应 DTO 的投影字段名，不是持久化 section；写入这些名字会被丢弃，
/// 防止响应别名反向成为第二份设置事实。
const PUBLIC_RESPONSE_ALIAS_SECTIONS: &[&str] = &[
    "workerConfigs",
    "orchestratorConfig",
    "auxiliaryConfig",
    "visionConfig",
    "imageGenerationConfig",
    "userRulesConfig",
    "registryEngines",
    "registryAgents",
];

#[derive(Debug)]
pub struct SettingsStore {
    sections: RwLock<HashMap<String, Value>>,
    /// 持久化文件路径，为 None 时仅内存模式。
    persistence_path: Option<PathBuf>,
    /// 配置事实源代际。执行期缓存只允许绑定这个代际，设置变化后自然失效。
    revision: Arc<AtomicU64>,
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
            revision: Arc::new(AtomicU64::new(self.revision())),
        }
    }
}

impl SettingsStore {
    pub fn new() -> Self {
        Self {
            sections: RwLock::new(HashMap::new()),
            persistence_path: None,
            revision: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 创建带持久化路径的 SettingsStore，启动后需调用 load_from_disk 恢复数据
    pub fn with_persistence_path(path: PathBuf) -> Self {
        Self {
            sections: RwLock::new(HashMap::new()),
            persistence_path: Some(path),
            revision: Arc::new(AtomicU64::new(0)),
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
            revision: Arc::new(AtomicU64::new(self.revision())),
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
            Ok(data) => {
                *self.sections.write().unwrap() = data;
                self.revision.fetch_add(1, Ordering::AcqRel);
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
        self.revision.fetch_add(1, Ordering::AcqRel);
        Ok(result)
    }

    /// 返回设置事实源的单调代际，用于失效执行期模型、工具和安全策略缓存。
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
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

    fn extract_id_str<'a>(item: &'a Value, id_field: &str) -> Option<&'a str> {
        item.get(id_field).and_then(Value::as_str)
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

fn is_public_response_alias_section(section: &str) -> bool {
    PUBLIC_RESPONSE_ALIAS_SECTIONS.contains(&section)
}

fn canonicalize_settings_section_value(section: &str, value: &mut Value) {
    if section == ORCHESTRATOR_SESSION_DEFAULTS_SECTION {
        canonicalize_session_orchestrator_section(value, false);
    } else if is_session_section_key(section) && section.ends_with(":orchestrator") {
        canonicalize_session_orchestrator_section(value, true);
    }
}

/// 会话级编排模型覆盖。
///
/// `engineId` 是会话级主模型绑定的引擎选择（A22）：它决定该会话是否由内置浏览器
/// 里的 GPT Web 引擎承载。所有会话 section 写入路径都会经过这里，若在这里被剥离，
/// 用户选中的 Web 引擎会在持久化时静默消失、重启即失效且不报错。
///
/// `allow_engine_binding` 区分两种 section：会话级覆盖允许引擎绑定；
/// `ORCHESTRATOR_SESSION_DEFAULTS_SECTION`（新会话默认值）不允许——引擎绑定是
/// 会话级事实，不得成为跨会话默认值。
fn canonicalize_session_orchestrator_section(value: &mut Value, allow_engine_binding: bool) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    object.retain(|key, _| {
        matches!(key.as_str(), "model" | "reasoningEffort")
            || (allow_engine_binding && key == "engineId")
    });
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
                    "apiProtocol": "openai_chat",
                }),
            )
            .unwrap();

        let snapshot = store.execution_snapshot();
        store
            .set_section(
                "orchestrator",
                json!({
                    "baseUrl": "https://new.example.com/v1",
                    "apiProtocol": "openai_chat",
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
    fn revision_invalidates_execution_caches_without_mutating_old_snapshot() {
        let store = SettingsStore::new();
        assert_eq!(store.revision(), 0);

        store.set("cacheFlag", json!("a")).unwrap();
        assert_eq!(store.revision(), 1);

        let snapshot = store.execution_snapshot();
        assert_eq!(snapshot.revision(), 1);

        store.set("cacheFlag", json!("b")).unwrap();
        assert_eq!(store.revision(), 2);
        assert_eq!(snapshot.revision(), 1);
        assert_eq!(snapshot.get("cacheFlag"), Some(json!("a")));
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
    #[test]
    fn session_orchestrator_section_keeps_engine_binding() {
        // A22：会话级主模型覆盖的引擎绑定决定该会话是否由 GPT Web 引擎承载。
        // 所有会话 section 写入路径都经过这里，剥离它会让用户选中的引擎静默失效。
        let store = SettingsStore::new();
        let session = SessionId::new("session-engine");
        store
            .set_session_section(
                &session,
                "orchestrator",
                json!({"model": "gpt-5", "reasoningEffort": "high", "engineId": "plugin/openai.chatgpt-web/gpt-5"}),
            )
            .unwrap();
        let stored = store.get_session_section(&session, "orchestrator");
        assert_eq!(stored["engineId"], json!("plugin/openai.chatgpt-web/gpt-5"));
        assert_eq!(stored["model"], json!("gpt-5"));
        assert_eq!(stored["reasoningEffort"], json!("high"));
    }

    #[test]
    fn session_orchestrator_section_drops_unknown_keys() {
        let store = SettingsStore::new();
        let session = SessionId::new("session-unknown");
        store
            .set_session_section(
                &session,
                "orchestrator",
                json!({"model": "gpt-5", "engineId": "plugin/openai.chatgpt-web/gpt-5", "baseUrl": "https://x"}),
            )
            .unwrap();
        let stored = store.get_session_section(&session, "orchestrator");
        assert_eq!(stored["model"], json!("gpt-5"));
        assert_eq!(stored["engineId"], json!("plugin/openai.chatgpt-web/gpt-5"));
        assert!(stored.get("baseUrl").is_none());
    }

    #[test]
    fn orchestrator_session_defaults_never_keep_engine_binding() {
        // 引擎绑定是会话级事实，不得成为新会话的跨会话默认值。
        let store = SettingsStore::new();
        store
            .set_section(
                ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
                json!({"model": "gpt-5", "reasoningEffort": "high", "engineId": "plugin/openai.chatgpt-web/gpt-5"}),
            )
            .unwrap();
        let stored = store.get_section(ORCHESTRATOR_SESSION_DEFAULTS_SECTION);
        assert_eq!(stored["model"], json!("gpt-5"));
        assert_eq!(stored["reasoningEffort"], json!("high"));
        assert!(stored.get("engineId").is_none());
    }
}
