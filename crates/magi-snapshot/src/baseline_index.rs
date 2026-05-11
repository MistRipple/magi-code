use crate::blob_store::write_atomic;
use crate::error::{SnapshotError, SnapshotResult};
use crate::types::FileMeta;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// 持久化在 `.magi/snapshots/index/{session_id}/baseline.json` 的 baseline 索引。
///
/// 单一字段 `entries`：path → FileMeta。原子写。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineIndex {
    pub entries: BTreeMap<String, FileMeta>,
}

impl BaselineIndex {
    pub fn load(path: &Path) -> SnapshotResult<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let payload = fs::read(path).map_err(|e| SnapshotError::io(path, e))?;
        if payload.is_empty() {
            return Ok(Self::default());
        }
        let parsed: BaselineIndex = serde_json::from_slice(&payload)?;
        Ok(parsed)
    }

    pub fn save(&self, path: &Path) -> SnapshotResult<()> {
        let payload = serde_json::to_vec_pretty(self)?;
        write_atomic(path, &payload)
    }

    pub fn get(&self, path: &str) -> Option<&FileMeta> {
        self.entries.get(path)
    }

    pub fn upsert(&mut self, meta: FileMeta) {
        self.entries.insert(meta.path.clone(), meta);
    }

    pub fn remove(&mut self, path: &str) -> Option<FileMeta> {
        self.entries.remove(path)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &FileMeta)> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// `refs.json`：approve 后被推进的 baseline 子集。
/// 与 `BaselineIndex` 同 schema，但只保留 approve 过的 path/hash 记录。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefsIndex {
    pub entries: BTreeMap<String, FileMeta>,
}

impl RefsIndex {
    pub fn load(path: &Path) -> SnapshotResult<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let payload = fs::read(path).map_err(|e| SnapshotError::io(path, e))?;
        if payload.is_empty() {
            return Ok(Self::default());
        }
        let parsed: RefsIndex = serde_json::from_slice(&payload)?;
        Ok(parsed)
    }

    pub fn save(&self, path: &Path) -> SnapshotResult<()> {
        let payload = serde_json::to_vec_pretty(self)?;
        write_atomic(path, &payload)
    }

    pub fn upsert(&mut self, meta: FileMeta) {
        self.entries.insert(meta.path.clone(), meta);
    }

    pub fn get(&self, path: &str) -> Option<&FileMeta> {
        self.entries.get(path)
    }
}

pub fn baseline_path(session_dir: &Path) -> PathBuf {
    session_dir.join("baseline.json")
}

pub fn refs_path(session_dir: &Path) -> PathBuf {
    session_dir.join("refs.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ContentKind;
    use tempfile::tempdir;

    fn meta(path: &str) -> FileMeta {
        FileMeta {
            path: path.into(),
            content_kind: ContentKind::Text,
            size: 12,
            mime: None,
            blob_hash: Some("deadbeef".into()),
            mtime_ms: Some(0),
            symlink: None,
            error: None,
        }
    }

    #[test]
    fn baseline_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("baseline.json");
        let mut idx = BaselineIndex::default();
        idx.upsert(meta("a.txt"));
        idx.upsert(meta("b.txt"));
        idx.save(&path).unwrap();
        let back = BaselineIndex::load(&path).unwrap();
        assert_eq!(back.len(), 2);
        assert!(back.get("a.txt").is_some());
    }

    #[test]
    fn baseline_load_missing_returns_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.json");
        let idx = BaselineIndex::load(&path).unwrap();
        assert!(idx.is_empty());
    }
}
