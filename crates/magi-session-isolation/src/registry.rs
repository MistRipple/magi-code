//! 会话到隔离副本的登记表。
use crate::clone::CloneStrategy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// 会话为什么运行在隔离副本里。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum IsolationOrigin {
    /// 用户主动选择。
    Manual,
    /// 提交时主工作区正被另一个会话使用，自动隔离以免互相等待。
    Contention { blocking_session_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIsolation {
    pub session_id: String,
    pub workspace_id: String,
    /// 主工作区根目录：合并的目标，也是副本的来源。
    pub source_root: PathBuf,
    /// 隔离副本根目录：会话读写的实际位置。
    pub root: PathBuf,
    pub origin: IsolationOrigin,
    pub strategy: CloneStrategy,
    /// 普通复制时被替换为符号链接的目录。
    #[serde(default)]
    pub linked_dirs: Vec<String>,
    #[serde(default)]
    pub git_available: bool,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, Default)]
pub struct SessionIsolationRegistry {
    inner: Arc<RwLock<HashMap<String, SessionIsolation>>>,
}

impl SessionIsolationRegistry {
    pub fn get(&self, session_id: &str) -> Option<SessionIsolation> {
        self.inner
            .read()
            .expect("session isolation registry read lock poisoned")
            .get(session_id)
            .cloned()
    }

    pub fn contains(&self, session_id: &str) -> bool {
        self.inner
            .read()
            .expect("session isolation registry read lock poisoned")
            .contains_key(session_id)
    }

    pub fn all(&self) -> Vec<SessionIsolation> {
        let mut all = self
            .inner
            .read()
            .expect("session isolation registry read lock poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        all.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        all
    }

    pub fn insert(&self, isolation: SessionIsolation) {
        self.inner
            .write()
            .expect("session isolation registry write lock poisoned")
            .insert(isolation.session_id.clone(), isolation);
    }

    pub fn remove(&self, session_id: &str) -> Option<SessionIsolation> {
        self.inner
            .write()
            .expect("session isolation registry write lock poisoned")
            .remove(session_id)
    }

    /// daemon 启动时载入持久化的登记；只保留副本目录仍然存在的条目。
    pub fn restore(&self, isolations: Vec<SessionIsolation>) -> usize {
        let mut target = self
            .inner
            .write()
            .expect("session isolation registry write lock poisoned");
        target.clear();
        for isolation in isolations {
            if isolation.root.is_dir() {
                target.insert(isolation.session_id.clone(), isolation);
            }
        }
        target.len()
    }

    /// 指定主工作区下仍在使用隔离副本的会话。
    pub fn sessions_for_source(&self, source_root: &Path) -> Vec<SessionIsolation> {
        self.all()
            .into_iter()
            .filter(|isolation| isolation.source_root == source_root)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(session_id: &str, root: PathBuf) -> SessionIsolation {
        SessionIsolation {
            session_id: session_id.to_string(),
            workspace_id: "ws".to_string(),
            source_root: PathBuf::from("/src"),
            root,
            origin: IsolationOrigin::Manual,
            strategy: CloneStrategy::Clone,
            linked_dirs: Vec::new(),
            git_available: true,
            created_at_ms: 1,
        }
    }

    #[test]
    fn restore_drops_isolations_whose_copy_directory_is_gone() {
        let kept = tempfile::tempdir().unwrap();
        let registry = SessionIsolationRegistry::default();
        let restored = registry.restore(vec![
            sample("alive", kept.path().to_path_buf()),
            sample("gone", PathBuf::from("/definitely/not/here")),
        ]);
        assert_eq!(restored, 1);
        assert!(registry.contains("alive"));
        assert!(!registry.contains("gone"));
    }

    #[test]
    fn registry_round_trips_through_json() {
        let isolation = SessionIsolation {
            origin: IsolationOrigin::Contention {
                blocking_session_id: "other".to_string(),
            },
            ..sample("s", PathBuf::from("/copy"))
        };
        let json = serde_json::to_string(&isolation).unwrap();
        let parsed: SessionIsolation = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, isolation);
    }
}
