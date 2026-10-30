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
    inner: Arc<RwLock<HashMap<(String, PathBuf), SessionIsolation>>>,
}

fn source_key(source: &Path) -> PathBuf {
    std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf())
}

impl SessionIsolationRegistry {
    /// 旧单目录消费者只允许读取唯一映射；多目录时不能选择任意首项。
    pub fn get(&self, session_id: &str) -> Option<SessionIsolation> {
        let inner = self
            .inner
            .read()
            .expect("session isolation registry read lock poisoned");
        let mut matches = inner.values().filter(|item| item.session_id == session_id);
        let only = matches.next()?.clone();
        matches.next().is_none().then_some(only)
    }

    pub fn for_session(&self, session_id: &str) -> Vec<SessionIsolation> {
        self.all()
            .into_iter()
            .filter(|item| item.session_id == session_id)
            .collect()
    }

    pub fn get_for_source(&self, session_id: &str, source: &Path) -> Option<SessionIsolation> {
        self.inner
            .read()
            .expect("session isolation registry read lock poisoned")
            .get(&(session_id.to_string(), source_key(source)))
            .cloned()
    }

    /// 允许调用方已经持有实际执行路径；仍按同一登记解析，不创建第二条绑定。
    pub fn get_for_root(&self, session_id: &str, root: &Path) -> Option<SessionIsolation> {
        let root = source_key(root);
        let mut matches = self
            .for_session(session_id)
            .into_iter()
            .filter(|item| source_key(&item.source_root) == root || source_key(&item.root) == root);
        let only = matches.next()?;
        matches.next().is_none().then_some(only)
    }

    pub fn contains(&self, session_id: &str) -> bool {
        self.inner
            .read()
            .expect("session isolation registry read lock poisoned")
            .values()
            .any(|item| item.session_id == session_id)
    }

    pub fn all(&self) -> Vec<SessionIsolation> {
        let mut all = self
            .inner
            .read()
            .expect("session isolation registry read lock poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        all.sort_by(|left, right| {
            left.session_id
                .cmp(&right.session_id)
                .then_with(|| left.source_root.cmp(&right.source_root))
        });
        all
    }

    pub fn insert(&self, isolation: SessionIsolation) {
        self.inner
            .write()
            .expect("session isolation registry write lock poisoned")
            .insert(
                (
                    isolation.session_id.clone(),
                    source_key(&isolation.source_root),
                ),
                isolation,
            );
    }

    /// 整组映射先持久化，再发布。持有原注册表写锁，避免不同会话相互覆盖持久状态。
    pub fn insert_many_with_persistence<E>(
        &self,
        additions: &[SessionIsolation],
        persist: impl FnOnce(&[SessionIsolation]) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut inner = self
            .inner
            .write()
            .expect("session isolation registry write lock poisoned");
        let mut next = inner.clone();
        for isolation in additions {
            next.insert(
                (
                    isolation.session_id.clone(),
                    source_key(&isolation.source_root),
                ),
                isolation.clone(),
            );
        }
        let mut records = next.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| {
            left.session_id
                .cmp(&right.session_id)
                .then_with(|| left.source_root.cmp(&right.source_root))
        });
        persist(&records)?;
        *inner = next;
        Ok(())
    }

    /// 持久化期间保持原注册表读锁，避免快照取出后被另一会话的新登记反向覆盖。
    pub fn persist_with<E>(
        &self,
        persist: impl FnOnce(&[SessionIsolation]) -> Result<(), E>,
    ) -> Result<(), E> {
        let inner = self
            .inner
            .read()
            .expect("session isolation registry read lock poisoned");
        let records = inner.values().cloned().collect::<Vec<_>>();
        persist(&records)
    }

    /// 旧单目录删除不能删除多根中的任意一项或整组。
    pub fn remove(&self, session_id: &str) -> Option<SessionIsolation> {
        let mut inner = self
            .inner
            .write()
            .expect("session isolation registry write lock poisoned");
        let mut matches = inner.keys().filter(|(id, _)| id == session_id);
        let key = matches.next()?.clone();
        if matches.next().is_some() {
            return None;
        }
        inner.remove(&key)
    }

    pub fn remove_for_source(&self, session_id: &str, source: &Path) -> Option<SessionIsolation> {
        self.inner
            .write()
            .expect("session isolation registry write lock poisoned")
            .remove(&(session_id.to_string(), source_key(source)))
    }

    pub fn remove_session(&self, session_id: &str) -> Vec<SessionIsolation> {
        let mut inner = self
            .inner
            .write()
            .expect("session isolation registry write lock poisoned");
        let removed = inner
            .values()
            .filter(|item| item.session_id == session_id)
            .cloned()
            .collect();
        inner.retain(|(id, _), _| id != session_id);
        removed
    }

    /// 保留原映射，包括暂时不可用的副本。丢弃映射会使重启后的请求回落到
    /// 原目录，既掩盖成果丢失，也可能把后续写入落到用户目录。
    pub fn restore(&self, isolations: Vec<SessionIsolation>) -> usize {
        let mut target = self
            .inner
            .write()
            .expect("session isolation registry write lock poisoned");
        target.clear();
        for isolation in isolations {
            target.insert(
                (
                    isolation.session_id.clone(),
                    source_key(&isolation.source_root),
                ),
                isolation,
            );
        }
        target.len()
    }

    /// 指定源目录下仍在使用隔离副本的会话；符号链接别名不分配第二份副本。
    pub fn sessions_for_source(&self, source_root: &Path) -> Vec<SessionIsolation> {
        let key = source_key(source_root);
        self.all()
            .into_iter()
            .filter(|item| source_key(&item.source_root) == key)
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
    fn restore_keeps_missing_copy_binding_to_prevent_source_fallback() {
        let kept = tempfile::tempdir().unwrap();
        let registry = SessionIsolationRegistry::default();
        let restored = registry.restore(vec![
            sample("alive", kept.path().to_path_buf()),
            sample("gone", PathBuf::from("/definitely/not/here")),
        ]);
        assert_eq!(restored, 2);
        assert!(registry.contains("alive"));
        assert!(registry.contains("gone"));
        assert_eq!(
            registry.get("gone").unwrap().root,
            PathBuf::from("/definitely/not/here")
        );
    }

    #[test]
    fn multiple_sources_keep_independent_mappings_after_restart_and_removal() {
        let registry = SessionIsolationRegistry::default();
        let a = sample("same-session", PathBuf::from("/copy-a"));
        let b = SessionIsolation {
            source_root: PathBuf::from("/src-b"),
            root: PathBuf::from("/missing-copy-b"),
            ..a.clone()
        };
        registry.insert(a.clone());
        registry.insert(b.clone());
        assert!(registry.contains("same-session"));
        assert!(
            registry.get("same-session").is_none(),
            "旧入口不得选任意首项"
        );
        assert!(registry.remove("same-session").is_none());
        assert_eq!(
            registry.get_for_source("same-session", Path::new("/src")),
            Some(a.clone())
        );
        assert_eq!(
            registry.get_for_root("same-session", Path::new("/missing-copy-b")),
            Some(b.clone())
        );
        let durable = serde_json::to_vec(&registry.all()).unwrap();
        let restarted = SessionIsolationRegistry::default();
        assert_eq!(
            restarted.restore(serde_json::from_slice(&durable).unwrap()),
            2
        );
        restarted.remove_for_source("same-session", Path::new("/src"));
        assert_eq!(restarted.get("same-session"), Some(b));
        assert_eq!(restarted.remove_session("same-session").len(), 1);
        assert!(!restarted.contains("same-session"));
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
