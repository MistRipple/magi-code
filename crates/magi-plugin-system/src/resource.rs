use magi_core::fs_atomic::write_atomic;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, fs, path::PathBuf};

const STATE_VERSION: u16 = 3;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginResource {
    pub version: u64,
    pub value: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceState {
    schema_version: u16,
    values: BTreeMap<String, PluginResource>,
}

/// 插件资源的唯一事实源。UI 只读快照并携带版本提交变更，冲突不会被最后写入覆盖。
pub struct PluginResourceStore {
    path: PathBuf,
    state: ResourceState,
}

impl PluginResourceStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, std::io::Error> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let state = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ResourceState {
                schema_version: STATE_VERSION,
                values: BTreeMap::new(),
            },
            Err(error) => return Err(error),
        };
        if state.schema_version != STATE_VERSION {
            return Err(std::io::Error::other("插件资源状态版本不受支持"));
        }
        Ok(Self { path, state })
    }
    pub fn read(&self, plugin_id: &str, resource_id: &str) -> Option<PluginResource> {
        self.state.values.get(&key(plugin_id, resource_id)).cloned()
    }
    /// 该插件是否已有持久数据（含设置）。
    pub fn has_data(&self, plugin_id: &str) -> bool {
        let prefix = format!("{plugin_id}\u{1f}");
        self.state.values.keys().any(|key| key.starts_with(&prefix))
    }
    /// 该插件的全部持久条目（含设置），按资源 ID 排序，供升级事务读取。
    pub fn entries(&self, plugin_id: &str) -> Vec<(String, PluginResource)> {
        let prefix = format!("{plugin_id}\u{1f}");
        self.state
            .values
            .iter()
            .filter_map(|(key, resource)| {
                let rest = key.strip_prefix(&prefix)?;
                Some((rest.to_owned(), resource.clone()))
            })
            .collect()
    }

    pub(crate) fn reserved_entries(&self, resource_prefix: &str) -> Vec<PluginResource> {
        self.state
            .values
            .iter()
            .filter_map(|(key, value)| {
                key.split_once('\u{1f}')
                    .filter(|(_, id)| id.starts_with(resource_prefix))
                    .map(|_| value.clone())
            })
            .collect()
    }

    pub(crate) fn remove(&mut self, plugin: &str, resource: &str) -> Result<bool, ResourceError> {
        let key = key(plugin, resource);
        let Some(previous) = self.state.values.remove(&key) else {
            return Ok(false);
        };
        if let Err(error) = self.persist() {
            self.state.values.insert(key, previous);
            return Err(error);
        }
        Ok(true)
    }

    /// 用转换后的完整条目整体替换该插件的持久数据，只落盘一次。幂等：
    /// 以相同条目重复调用结果不变，崩溃恢复可以安全重放。
    pub fn replace_plugin(
        &mut self,
        plugin_id: &str,
        entries: &[(String, PluginResource)],
    ) -> Result<(), ResourceError> {
        let prefix = format!("{plugin_id}\u{1f}");
        let previous = self.state.values.clone();
        self.state.values.retain(|key, _| !key.starts_with(&prefix));
        for (resource_id, resource) in entries {
            self.state
                .values
                .insert(key(plugin_id, resource_id), resource.clone());
        }
        if let Err(error) = self.persist() {
            self.state.values = previous;
            return Err(error);
        }
        Ok(())
    }

    pub fn write(
        &mut self,
        plugin_id: &str,
        resource_id: &str,
        expected_version: u64,
        value: Value,
    ) -> Result<PluginResource, ResourceError> {
        if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > 4 * 1024 * 1024) {
            return Err(ResourceError::Invalid("资源快照超限".into()));
        }
        let key = key(plugin_id, resource_id);
        let current = self
            .state
            .values
            .get(&key)
            .map(|resource| resource.version)
            .unwrap_or(0);
        if current != expected_version {
            return Err(ResourceError::Conflict {
                expected: expected_version,
                actual: current,
            });
        }
        let resource = PluginResource {
            version: current.saturating_add(1),
            value,
        };
        let previous = self.state.values.insert(key.clone(), resource.clone());
        if let Err(error) = self.persist() {
            // 内存快照必须与磁盘一致：落盘失败时撤回本次变更，版本号不前进。
            match previous {
                Some(previous) => self.state.values.insert(key, previous),
                None => self.state.values.remove(&key),
            };
            return Err(error);
        }
        Ok(resource)
    }

    fn persist(&self) -> Result<(), ResourceError> {
        let bytes = serde_json::to_vec_pretty(&self.state)
            .map_err(|error| ResourceError::Storage(error.to_string()))?;
        write_atomic(&self.path, bytes).map_err(|error| ResourceError::Storage(error.to_string()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ResourceError {
    #[error("资源版本冲突：expected={expected}, actual={actual}")]
    Conflict { expected: u64, actual: u64 },
    #[error("资源无效：{0}")]
    Invalid(String),
    #[error("资源存储失败：{0}")]
    Storage(String),
}

fn key(plugin_id: &str, resource_id: &str) -> String {
    format!("{plugin_id}\u{1f}{resource_id}")
}
