use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::RwLock;
use tracing::warn;

#[derive(Debug)]
pub struct SettingsStore {
    sections: RwLock<HashMap<String, Value>>,
    /// 持久化文件路径，为 None 时仅内存模式（兼容已有测试）
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
                let mut sections = self.sections.write().unwrap();
                *sections = data;
            }
            Err(error) => {
                warn!(
                    path = %path.display(),
                    error = %error,
                    "设置文件解析失败，使用空默认值"
                );
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
        let content = serde_json::to_vec_pretty(&*sections)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let temp_path = temp_path_for(path);
        fs::write(&temp_path, content)?;
        fs::rename(temp_path, path)?;
        Ok(())
    }

    /// 自动持久化：写操作后静默保存，失败仅打印警告
    fn auto_persist(&self) {
        if self.persistence_path.is_some() {
            if let Err(error) = self.save_to_disk() {
                warn!(error = %error, "设置自动持久化失败");
            }
        }
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        self.sections.read().unwrap().get(key).cloned()
    }

    pub fn set(&self, key: &str, value: Value) {
        self.sections
            .write()
            .unwrap()
            .insert(key.to_string(), value);
        self.auto_persist();
    }

    pub fn get_section(&self, section: &str) -> Value {
        self.sections
            .read()
            .unwrap()
            .get(section)
            .cloned()
            .unwrap_or(Value::Null)
    }

    pub fn set_section(&self, section: &str, value: Value) {
        self.sections
            .write()
            .unwrap()
            .insert(section.to_string(), value);
        self.auto_persist();
    }

    pub fn remove_section_entry(&self, section: &str, key: &str) {
        let mut sections = self.sections.write().unwrap();
        if let Some(Value::Object(map)) = sections.get_mut(section) {
            map.remove(key);
        }
        drop(sections);
        self.auto_persist();
    }

    pub fn upsert_array_entry(&self, section: &str, id_field: &str, entry: &Value) {
        let mut sections = self.sections.write().unwrap();
        let arr = sections
            .entry(section.to_string())
            .or_insert_with(|| Value::Array(vec![]));
        if let Value::Array(items) = arr {
            let id_str = Self::extract_id_str(entry, id_field);
            if let Some(id_val) = id_str {
                if let Some(pos) = items.iter().position(|item| Self::extract_id_str(item, id_field) == Some(id_val)) {
                    items[pos] = entry.clone();
                    drop(sections);
                    self.auto_persist();
                    return;
                }
            }
            items.push(entry.clone());
        }
        drop(sections);
        self.auto_persist();
    }

    pub fn remove_array_entry(&self, section: &str, id_field: &str, id_value: &str) {
        let mut sections = self.sections.write().unwrap();
        if let Some(Value::Array(items)) = sections.get_mut(section) {
            items.retain(|item| {
                Self::extract_id_str(item, id_field).map(|v| v != id_value).unwrap_or(true)
            });
        }
        drop(sections);
        self.auto_persist();
    }

    fn extract_id_str<'a>(item: &'a Value, primary_field: &str) -> Option<&'a str> {
        if let Some(s) = item.get(primary_field).and_then(|v| v.as_str()) { return Some(s); }
        if let Some(s) = item.get("id").and_then(|v| v.as_str()) { return Some(s); }
        if let Some(s) = item.get("serverId").and_then(|v| v.as_str()) { return Some(s); }
        if let Some(s) = item.get("repositoryId").and_then(|v| v.as_str()) { return Some(s); }
        if let Some(s) = item.get("engineId").and_then(|v| v.as_str()) { return Some(s); }
        
        if let Some(server) = item.get("server") {
            if let Some(s) = server.get(primary_field).and_then(|v| v.as_str()) { return Some(s); }
            if let Some(s) = server.get("id").and_then(|v| v.as_str()) { return Some(s); }
            if let Some(s) = server.get("serverId").and_then(|v| v.as_str()) { return Some(s); }
        }
        if let Some(engine) = item.get("engine") {
            if let Some(s) = engine.get(primary_field).and_then(|v| v.as_str()) { return Some(s); }
            if let Some(s) = engine.get("id").and_then(|v| v.as_str()) { return Some(s); }
            if let Some(s) = engine.get("engineId").and_then(|v| v.as_str()) { return Some(s); }
        }
        if let Some(agent) = item.get("agent") {
            if let Some(s) = agent.get(primary_field).and_then(|v| v.as_str()) { return Some(s); }
            if let Some(s) = agent.get("id").and_then(|v| v.as_str()) { return Some(s); }
        }
        if let Some(llm) = item.get("llm") {
            if let Some(s) = llm.get(primary_field).and_then(|v| v.as_str()) { return Some(s); }
            if let Some(s) = llm.get("id").and_then(|v| v.as_str()) { return Some(s); }
        }
        
        None
    }

    pub fn snapshot(&self) -> HashMap<String, Value> {
        self.sections.read().unwrap().clone()
    }
}

/// 生成临时文件路径，用于原子写入
fn temp_path_for(path: &PathBuf) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "settings.json".to_string());
    file_name.push_str(".tmp");
    path.with_file_name(file_name)
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
        store.set("theme", json!("dark"));
        store.set_section("workers", json!({"primary": "gpu-0"}));
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
    fn pure_memory_mode_does_not_write_files() {
        let store = SettingsStore::new();
        store.set("key", json!("value"));
        // 纯内存模式不应产生任何磁盘操作
        assert_eq!(store.get("key"), Some(json!("value")));
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
        store.upsert_array_entry("engines", "engineId", &json!({"engineId": "e1", "name": "test"}));
        store.remove_array_entry("engines", "engineId", "e1");
        store.set_section("config", json!({"a": 1}));
        store.remove_section_entry("config", "a");

        // 验证最终状态被持久化
        let store2 = SettingsStore::with_persistence_path(path);
        store2.load_from_disk().unwrap();
        assert_eq!(store2.get_section("engines"), json!([]));
        assert_eq!(store2.get_section("config"), json!({}));
    }
}
