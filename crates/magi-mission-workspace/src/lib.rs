//! Task System v2 L17 — Mission Workspace.
//!
//! C 档 Mission 独占的工作目录。落在
//! `~/.magi/projects/{slug}/missions/{mission_id}/workspace/` 下，包含：
//!
//! - `artifacts/` — Mission 过程中产出的中间/最终交付物
//! - `logs/` — Mission 跨进程的日志归档
//! - `memory.md` — Mission 级 memory（区别于 project 级 ProjectMemory）
//!
//! 与 MissionCharter / Plan 不同，Workspace 本身没有“需要 agent 直接改写”的结构化内容；
//! 它是 Mission 拥有的物理目录布局。本 crate 负责：
//!
//! - 在 Mission 首次进入时建好目录骨架
//! - 提供路径查询接口供其他子系统（Checkpoint、ValidationRunner、KG）落盘
//! - 渲染 prompt 片段，把工作目录暴露给模型，避免 agent 把产物丢到无主目录
//!
//! 不提供独立 `workspace_write` 工具：写文件走通用 `file_write` / `file_patch`，
//! 写 memory 走 `memory_write`。本 crate 只负责目录治理。

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use magi_core::{MissionId, WorkspaceRootPath};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MissionWorkspaceError {
    #[error("HOME 目录不可用，无法定位 ~/.magi/projects/.../missions/.../workspace")]
    HomeDirUnavailable,
    #[error("mission workspace 路径 {path} 读写失败: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MissionWorkspace {
    pub mission_id: MissionId,
    pub root: PathBuf,
    pub artifacts_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub memory_path: PathBuf,
}

pub struct MissionWorkspaceStore {
    root: PathBuf,
}

impl MissionWorkspaceStore {
    pub fn open(workspace_root: &WorkspaceRootPath) -> Result<Self, MissionWorkspaceError> {
        let home = dirs_home()?;
        Self::open_with_home(&home, workspace_root)
    }

    pub fn open_with_home(
        magi_home: &Path,
        workspace_root: &WorkspaceRootPath,
    ) -> Result<Self, MissionWorkspaceError> {
        let slug = workspace_slug(workspace_root.as_str());
        let root = magi_home.join("projects").join(slug).join("missions");
        fs::create_dir_all(&root).map_err(|source| MissionWorkspaceError::Io {
            path: root.clone(),
            source,
        })?;
        Ok(Self { root })
    }

    fn workspace_root_for(&self, mission_id: &MissionId) -> PathBuf {
        self.root.join(mission_id.as_str()).join("workspace")
    }

    fn mission_workspace_for(&self, mission_id: &MissionId) -> MissionWorkspace {
        let root = self.workspace_root_for(mission_id);
        MissionWorkspace {
            mission_id: mission_id.clone(),
            artifacts_dir: root.join("artifacts"),
            logs_dir: root.join("logs"),
            memory_path: root.join("memory.md"),
            root,
        }
    }

    /// 建好 mission 工作目录的骨架（幂等）。每次首次进入 Mission 时调用。
    pub fn ensure(
        &self,
        mission_id: &MissionId,
    ) -> Result<MissionWorkspace, MissionWorkspaceError> {
        let ws = self.mission_workspace_for(mission_id);
        for dir in [&ws.root, &ws.artifacts_dir, &ws.logs_dir] {
            fs::create_dir_all(dir).map_err(|source| MissionWorkspaceError::Io {
                path: dir.clone(),
                source,
            })?;
        }
        Ok(ws)
    }

    /// 不创建任何目录，只返回路径快照；用于查询。
    pub fn locate(&self, mission_id: &MissionId) -> MissionWorkspace {
        self.mission_workspace_for(mission_id)
    }

    /// 渲染 prompt 片段：告知模型 Mission 工作目录位置，引导其把产物落在这里。
    pub fn render_for_prompt(
        &self,
        mission_id: &MissionId,
    ) -> Result<String, MissionWorkspaceError> {
        let ws = self.ensure(mission_id)?;
        let mut out = String::new();
        out.push_str("# Mission Workspace\n\n");
        out.push_str(&format!("- mission_id: {}\n", ws.mission_id.as_str()));
        out.push_str(&format!("- workspace_root: {}\n", ws.root.display()));
        out.push_str(&format!(
            "- artifacts_dir: {}\n",
            ws.artifacts_dir.display()
        ));
        out.push_str(&format!("- logs_dir: {}\n", ws.logs_dir.display()));
        out.push_str(&format!("- memory_path: {}\n\n", ws.memory_path.display()));
        out.push_str(
            "约定：Mission 过程中产生的中间产物、报告、生成代码草稿请写入 artifacts_dir；\
             跨进程日志请写入 logs_dir；Mission 级 memory 走 memory_path。\
             与 Checkpoint / ValidationRunner / KG 共用此目录。\n",
        );
        Ok(out)
    }
}

/// 进程级 MissionWorkspaceStore 缓存，按 workspace_root 聚合。
pub struct MissionWorkspaceRegistry {
    inner: RwLock<HashMap<String, Arc<MissionWorkspaceStore>>>,
    fallback_home: PathBuf,
}

impl Default for MissionWorkspaceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl MissionWorkspaceRegistry {
    pub fn new() -> Self {
        let fallback_home = std::env::temp_dir().join("magi-mission-workspace");
        let _ = fs::create_dir_all(&fallback_home);
        Self {
            inner: RwLock::new(HashMap::new()),
            fallback_home,
        }
    }

    pub fn get_or_open(
        &self,
        workspace_root: &WorkspaceRootPath,
    ) -> Result<Arc<MissionWorkspaceStore>, MissionWorkspaceError> {
        let key = workspace_root.as_str().to_string();
        if let Some(store) = self
            .inner
            .read()
            .expect("mission workspace registry poisoned")
            .get(&key)
        {
            return Ok(store.clone());
        }
        let store = match MissionWorkspaceStore::open(workspace_root) {
            Ok(store) => store,
            Err(MissionWorkspaceError::HomeDirUnavailable) => {
                MissionWorkspaceStore::open_with_home(&self.fallback_home, workspace_root)?
            }
            Err(err) => return Err(err),
        };
        let arc = Arc::new(store);
        self.inner
            .write()
            .expect("mission workspace registry poisoned")
            .insert(key, arc.clone());
        Ok(arc)
    }
}

fn dirs_home() -> Result<PathBuf, MissionWorkspaceError> {
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(MissionWorkspaceError::HomeDirUnavailable)?;
    Ok(base.join(".magi"))
}

fn workspace_slug(workspace_root: &str) -> String {
    let trimmed = workspace_root.trim_start_matches('/').trim_end_matches('/');
    if trimmed.is_empty() {
        "root".to_string()
    } else {
        format!("-{}", trimmed.replace('/', "-"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_workspace_root(path: &Path) -> WorkspaceRootPath {
        WorkspaceRootPath::new(path.to_string_lossy().to_string())
    }

    #[test]
    fn ensure_creates_workspace_skeleton() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws_root = make_workspace_root(tmp.path());
        let store =
            MissionWorkspaceStore::open_with_home(tmp.path(), &ws_root).expect("open store");
        let mission_id = MissionId::new("mission-abc".to_string());

        let ws = store.ensure(&mission_id).expect("ensure ok");
        assert!(ws.root.is_dir(), "workspace root 必须创建");
        assert!(ws.artifacts_dir.is_dir(), "artifacts 必须创建");
        assert!(ws.logs_dir.is_dir(), "logs 必须创建");
        // memory.md 不预先创建，按需写入
        assert!(!ws.memory_path.exists());
    }

    #[test]
    fn ensure_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws_root = make_workspace_root(tmp.path());
        let store =
            MissionWorkspaceStore::open_with_home(tmp.path(), &ws_root).expect("open store");
        let mission_id = MissionId::new("mission-idem".to_string());

        store.ensure(&mission_id).expect("first ensure");
        store.ensure(&mission_id).expect("second ensure 必须幂等");
    }

    #[test]
    fn locate_does_not_create_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws_root = make_workspace_root(tmp.path());
        let store =
            MissionWorkspaceStore::open_with_home(tmp.path(), &ws_root).expect("open store");
        let mission_id = MissionId::new("mission-locate".to_string());

        let ws = store.locate(&mission_id);
        assert!(!ws.root.exists(), "locate 不应触发目录创建");
    }

    #[test]
    fn render_for_prompt_includes_workspace_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws_root = make_workspace_root(tmp.path());
        let store =
            MissionWorkspaceStore::open_with_home(tmp.path(), &ws_root).expect("open store");
        let mission_id = MissionId::new("mission-prompt".to_string());

        let rendered = store.render_for_prompt(&mission_id).expect("render");
        assert!(rendered.contains("Mission Workspace"));
        assert!(rendered.contains("mission-prompt"));
        assert!(rendered.contains("artifacts_dir"));
        assert!(rendered.contains("logs_dir"));
        assert!(rendered.contains("memory_path"));
    }

    #[test]
    fn registry_caches_store_by_workspace_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws_root = make_workspace_root(tmp.path());
        let registry = MissionWorkspaceRegistry::new();
        // 通过外部环境强制走主路径会污染用户家目录；这里直接验证 fallback 路径下的复用。
        // 把 HOME 设为不存在的字符串，让 open() 走 fallback_home。
        let orig_home = std::env::var_os("HOME");
        // SAFETY: 测试单线程串行运行；恢复时再写回。
        unsafe { std::env::remove_var("HOME") };

        let a = registry.get_or_open(&ws_root).expect("first open");
        let b = registry.get_or_open(&ws_root).expect("second open");
        assert!(Arc::ptr_eq(&a, &b), "同 workspace_root 必须共享同一 store");

        // restore HOME
        if let Some(value) = orig_home {
            unsafe { std::env::set_var("HOME", value) };
        }
    }
}
