use magi_core::fs_atomic::write_atomic;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, fs, path::PathBuf};

const STATE_VERSION: u16 = 2;

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
    pub fn read(&self, plugin_id: &str, scope: &str, resource_id: &str) -> Option<PluginResource> {
        self.state
            .values
            .get(&key(plugin_id, scope, resource_id))
            .cloned()
    }
    pub fn write(
        &mut self,
        plugin_id: &str,
        scope: &str,
        resource_id: &str,
        expected_version: u64,
        value: Value,
    ) -> Result<PluginResource, ResourceError> {
        if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > 4 * 1024 * 1024) {
            return Err(ResourceError::Invalid("资源快照超限".into()));
        }
        let key = key(plugin_id, scope, resource_id);
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
        self.state.values.insert(key, resource.clone());
        let bytes = serde_json::to_vec_pretty(&self.state)
            .map_err(|error| ResourceError::Storage(error.to_string()))?;
        write_atomic(&self.path, bytes)
            .map_err(|error| ResourceError::Storage(error.to_string()))?;
        Ok(resource)
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

fn key(plugin_id: &str, scope: &str, resource_id: &str) -> String {
    format!("{plugin_id}\u{1f}{scope}\u{1f}{resource_id}")
}
