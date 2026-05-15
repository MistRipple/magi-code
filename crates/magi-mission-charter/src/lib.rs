//! Task System v2 — Tier 4 / L11 MissionCharter：mission 的"宪章"契约。
//!
//! 每个 Mission 在 `ensure_session_mission` 首次创建时同步落一份 charter，
//! 把"为什么做、做到什么程度算完、有什么硬约束"沉淀为可读 markdown 文件，
//! 后续每轮 Turn 自动注入 orchestrator system prompt，避免长对话偏题。
//!
//! 物理存储：`~/.magi/projects/{slug}/missions/{mission_id}/charter.md`。
//! - 与 ProjectMemory 共用同一个 slug 派生策略（绝对路径 `/` → `-`）。
//! - 与 mission 同生命周期，mission 结束后保留作为"已交付契约"档案。
//!
//! 字段结构（frontmatter + body）：
//! ```yaml
//! ---
//! mission_id: mission-xxxx
//! title: 短标题
//! created_at: 2026-05-15T00:00:00Z
//! updated_at: 2026-05-15T00:00:00Z
//! ---
//! ## Goal
//! <用户原始诉求 + 推理后的目标陈述>
//!
//! ## Success Criteria
//! - 验证点 1
//! - 验证点 2
//!
//! ## Constraints
//! - 时间/技术/范围硬约束
//!
//! ## Stakeholders
//! - 角色 1
//! ```
//!
//! charter 是"长效契约"，TodoLedger 是"过程笔记"，二者不重合：前者跨 session、
//! 后者 in-session。`mission_charter_write` 工具允许 orchestrator 在澄清后增量更新。

use magi_core::{MissionId, UtcMillis, WorkspaceRootPath};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use thiserror::Error;

// ---------------------------------------------------------------------------
// MissionCharter
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionCharter {
    pub mission_id: MissionId,
    pub title: String,
    pub goal: String,
    pub success_criteria: Vec<String>,
    pub constraints: Vec<String>,
    pub stakeholders: Vec<String>,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

impl MissionCharter {
    pub fn new(mission_id: MissionId, title: impl Into<String>, goal: impl Into<String>, now: UtcMillis) -> Self {
        Self {
            mission_id,
            title: title.into(),
            goal: goal.into(),
            success_criteria: Vec::new(),
            constraints: Vec::new(),
            stakeholders: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum MissionCharterError {
    #[error("无法解析 home 目录")]
    HomeDirUnavailable,
    #[error("charter 数据缺失或非法：{reason}")]
    InvalidCharter { reason: String },
    #[error("charter IO 失败 (path={path}): {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// 单 workspace 范围的 mission charter 存储：负责 `~/.magi/projects/{slug}/missions/` 下
/// 所有 mission 的 charter.md 读写。
pub struct MissionCharterStore {
    root: PathBuf,
}

impl MissionCharterStore {
    pub fn open(workspace_root: &WorkspaceRootPath) -> Result<Self, MissionCharterError> {
        let home = dirs_home()?;
        Self::open_with_home(&home, workspace_root)
    }

    pub fn open_with_home(
        magi_home: &Path,
        workspace_root: &WorkspaceRootPath,
    ) -> Result<Self, MissionCharterError> {
        let slug = workspace_slug(workspace_root.as_str());
        let root = magi_home.join("projects").join(slug).join("missions");
        fs::create_dir_all(&root).map_err(|source| MissionCharterError::Io {
            path: root.clone(),
            source,
        })?;
        Ok(Self { root })
    }

    fn charter_path(&self, mission_id: &MissionId) -> PathBuf {
        self.root.join(mission_id.as_str()).join("charter.md")
    }

    pub fn load(&self, mission_id: &MissionId) -> Result<Option<MissionCharter>, MissionCharterError> {
        let path = self.charter_path(mission_id);
        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(MissionCharterError::Io { path, source }),
        };
        parse_charter(&raw).map(Some)
    }

    pub fn save(&self, charter: &MissionCharter) -> Result<(), MissionCharterError> {
        let path = self.charter_path(&charter.mission_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| MissionCharterError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let rendered = render_charter(charter);
        fs::write(&path, rendered).map_err(|source| MissionCharterError::Io { path, source })
    }

    /// 为 system prompt 渲染 charter 段落。返回 None 表示尚未建立 charter。
    pub fn render_for_prompt(
        &self,
        mission_id: &MissionId,
    ) -> Result<Option<String>, MissionCharterError> {
        let Some(charter) = self.load(mission_id)? else {
            return Ok(None);
        };
        let mut out = String::new();
        out.push_str("# Mission Charter\n\n");
        out.push_str(&format!("- mission_id: {}\n", charter.mission_id.as_str()));
        out.push_str(&format!("- title: {}\n\n", charter.title));
        out.push_str("## Goal\n");
        out.push_str(charter.goal.trim());
        out.push_str("\n\n");
        if !charter.success_criteria.is_empty() {
            out.push_str("## Success Criteria\n");
            for c in &charter.success_criteria {
                out.push_str(&format!("- {}\n", c));
            }
            out.push('\n');
        }
        if !charter.constraints.is_empty() {
            out.push_str("## Constraints\n");
            for c in &charter.constraints {
                out.push_str(&format!("- {}\n", c));
            }
            out.push('\n');
        }
        if !charter.stakeholders.is_empty() {
            out.push_str("## Stakeholders\n");
            for s in &charter.stakeholders {
                out.push_str(&format!("- {}\n", s));
            }
            out.push('\n');
        }
        Ok(Some(out))
    }
}

// ---------------------------------------------------------------------------
// Registry: 按 workspace_root 缓存 store
// ---------------------------------------------------------------------------

pub struct MissionCharterRegistry {
    inner: RwLock<HashMap<String, Arc<MissionCharterStore>>>,
    home: PathBuf,
}

impl MissionCharterRegistry {
    /// 不可失败：home 解析失败时回退到 `$TMPDIR/magi-mission-charter`，
    /// 保证 dispatcher 构造不被 IO 状态阻塞。
    pub fn new() -> Self {
        let home = dirs_home().unwrap_or_else(|_| {
            std::env::temp_dir().join("magi-mission-charter")
        });
        Self {
            inner: RwLock::new(HashMap::new()),
            home,
        }
    }

    pub fn with_home(home: PathBuf) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            home,
        }
    }

    pub fn get_or_open(
        &self,
        workspace_root: &WorkspaceRootPath,
    ) -> Result<Arc<MissionCharterStore>, MissionCharterError> {
        let key = workspace_root.as_str().to_string();
        if let Some(found) = self.inner.read().expect("registry read lock").get(&key).cloned() {
            return Ok(found);
        }
        let store = MissionCharterStore::open_with_home(&self.home, workspace_root)?;
        let arc = Arc::new(store);
        self.inner
            .write()
            .expect("registry write lock")
            .insert(key, arc.clone());
        Ok(arc)
    }
}

impl Default for MissionCharterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tool argument parsing
// ---------------------------------------------------------------------------

/// `mission_charter_write` 工具入参形态。
#[derive(Debug)]
pub struct MissionCharterWriteArgs {
    pub title: Option<String>,
    pub goal: Option<String>,
    pub success_criteria: Option<Vec<String>>,
    pub constraints: Option<Vec<String>>,
    pub stakeholders: Option<Vec<String>>,
}

pub fn parse_mission_charter_write_arguments(
    raw: &serde_json::Value,
) -> Result<MissionCharterWriteArgs, MissionCharterError> {
    let obj = raw.as_object().ok_or_else(|| MissionCharterError::InvalidCharter {
        reason: "arguments 必须为对象".to_string(),
    })?;
    let title = obj
        .get("title")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let goal = obj
        .get("goal")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let success_criteria = parse_string_list(obj.get("success_criteria"))?;
    let constraints = parse_string_list(obj.get("constraints"))?;
    let stakeholders = parse_string_list(obj.get("stakeholders"))?;
    if title.is_none()
        && goal.is_none()
        && success_criteria.is_none()
        && constraints.is_none()
        && stakeholders.is_none()
    {
        return Err(MissionCharterError::InvalidCharter {
            reason: "至少需要提供 title/goal/success_criteria/constraints/stakeholders 中一个字段".to_string(),
        });
    }
    Ok(MissionCharterWriteArgs {
        title,
        goal,
        success_criteria,
        constraints,
        stakeholders,
    })
}

fn parse_string_list(
    value: Option<&serde_json::Value>,
) -> Result<Option<Vec<String>>, MissionCharterError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let arr = value.as_array().ok_or_else(|| MissionCharterError::InvalidCharter {
        reason: "字段必须为字符串数组".to_string(),
    })?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item.as_str().ok_or_else(|| MissionCharterError::InvalidCharter {
            reason: "数组项必须为字符串".to_string(),
        })?;
        out.push(s.to_string());
    }
    Ok(Some(out))
}

/// 把入参应用到已有 charter 上（增量更新）。返回是否实际产生了变更。
pub fn apply_charter_update(
    charter: &mut MissionCharter,
    args: MissionCharterWriteArgs,
    now: UtcMillis,
) -> bool {
    let mut changed = false;
    if let Some(title) = args.title {
        if charter.title != title {
            charter.title = title;
            changed = true;
        }
    }
    if let Some(goal) = args.goal {
        if charter.goal != goal {
            charter.goal = goal;
            changed = true;
        }
    }
    if let Some(success) = args.success_criteria {
        if charter.success_criteria != success {
            charter.success_criteria = success;
            changed = true;
        }
    }
    if let Some(constraints) = args.constraints {
        if charter.constraints != constraints {
            charter.constraints = constraints;
            changed = true;
        }
    }
    if let Some(stakeholders) = args.stakeholders {
        if charter.stakeholders != stakeholders {
            charter.stakeholders = stakeholders;
            changed = true;
        }
    }
    if changed {
        charter.updated_at = now;
    }
    changed
}

// ---------------------------------------------------------------------------
// 序列化 / 反序列化（frontmatter + markdown body）
// ---------------------------------------------------------------------------

fn render_charter(charter: &MissionCharter) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("mission_id: {}\n", charter.mission_id.as_str()));
    out.push_str(&format!("title: {}\n", charter.title));
    out.push_str(&format!("created_at: {}\n", charter.created_at.0));
    out.push_str(&format!("updated_at: {}\n", charter.updated_at.0));
    out.push_str("---\n\n");
    out.push_str("## Goal\n");
    out.push_str(charter.goal.trim());
    out.push_str("\n\n");
    out.push_str("## Success Criteria\n");
    if charter.success_criteria.is_empty() {
        out.push_str("(待补充)\n");
    } else {
        for c in &charter.success_criteria {
            out.push_str(&format!("- {}\n", c));
        }
    }
    out.push('\n');
    out.push_str("## Constraints\n");
    if charter.constraints.is_empty() {
        out.push_str("(待补充)\n");
    } else {
        for c in &charter.constraints {
            out.push_str(&format!("- {}\n", c));
        }
    }
    out.push('\n');
    out.push_str("## Stakeholders\n");
    if charter.stakeholders.is_empty() {
        out.push_str("(待补充)\n");
    } else {
        for s in &charter.stakeholders {
            out.push_str(&format!("- {}\n", s));
        }
    }
    out.push('\n');
    out
}

fn parse_charter(raw: &str) -> Result<MissionCharter, MissionCharterError> {
    let (front, body) = split_frontmatter(raw).ok_or_else(|| MissionCharterError::InvalidCharter {
        reason: "缺少 frontmatter".to_string(),
    })?;
    let mut mission_id: Option<String> = None;
    let mut title: Option<String> = None;
    let mut created_at: Option<u64> = None;
    let mut updated_at: Option<u64> = None;
    for line in front.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().to_string();
        match key {
            "mission_id" => mission_id = Some(value),
            "title" => title = Some(value),
            "created_at" => created_at = value.parse::<u64>().ok(),
            "updated_at" => updated_at = value.parse::<u64>().ok(),
            _ => {}
        }
    }
    let mission_id = mission_id.ok_or_else(|| MissionCharterError::InvalidCharter {
        reason: "frontmatter 缺少 mission_id".to_string(),
    })?;
    let title = title.ok_or_else(|| MissionCharterError::InvalidCharter {
        reason: "frontmatter 缺少 title".to_string(),
    })?;
    let created_at = created_at.ok_or_else(|| MissionCharterError::InvalidCharter {
        reason: "frontmatter 缺少 created_at".to_string(),
    })?;
    let updated_at = updated_at.unwrap_or(created_at);
    let sections = parse_sections(body);
    let goal = sections.get("Goal").cloned().unwrap_or_default();
    let success_criteria = sections
        .get("Success Criteria")
        .map(|s| parse_bullets(s))
        .unwrap_or_default();
    let constraints = sections
        .get("Constraints")
        .map(|s| parse_bullets(s))
        .unwrap_or_default();
    let stakeholders = sections
        .get("Stakeholders")
        .map(|s| parse_bullets(s))
        .unwrap_or_default();
    Ok(MissionCharter {
        mission_id: MissionId::new(mission_id),
        title,
        goal: goal.trim().to_string(),
        success_criteria,
        constraints,
        stakeholders,
        created_at: UtcMillis(created_at),
        updated_at: UtcMillis(updated_at),
    })
}

fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let stripped = raw.strip_prefix("---\n")?;
    let end = stripped.find("\n---\n")?;
    let front = &stripped[..end];
    let body = &stripped[end + "\n---\n".len()..];
    Some((front, body))
}

fn parse_sections(body: &str) -> HashMap<String, String> {
    let mut sections: HashMap<String, String> = HashMap::new();
    let mut current: Option<(String, String)> = None;
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            if let Some((name, content)) = current.take() {
                sections.insert(name, content);
            }
            current = Some((rest.trim().to_string(), String::new()));
        } else if let Some((_, content)) = current.as_mut() {
            content.push_str(line);
            content.push('\n');
        }
    }
    if let Some((name, content)) = current.take() {
        sections.insert(name, content);
    }
    sections
}

fn parse_bullets(section: &str) -> Vec<String> {
    section
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("- ") {
                let s = rest.trim();
                if s.is_empty() || s == "(待补充)" {
                    None
                } else {
                    Some(s.to_string())
                }
            } else {
                None
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

fn workspace_slug(absolute_path: &str) -> String {
    absolute_path.replace('/', "-")
}

fn dirs_home() -> Result<PathBuf, MissionCharterError> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or(MissionCharterError::HomeDirUnavailable)?;
    Ok(PathBuf::from(home).join(".magi"))
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmp_workspace() -> (TempDir, WorkspaceRootPath) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        (tmp, ws)
    }

    #[test]
    fn workspace_slug_replaces_slash_with_dash() {
        assert_eq!(workspace_slug("/Users/x/proj"), "-Users-x-proj");
    }

    #[test]
    fn save_then_load_round_trips_all_fields() {
        let (home, ws) = tmp_workspace();
        let store = MissionCharterStore::open_with_home(home.path(), &ws).expect("open");
        let charter = MissionCharter {
            mission_id: MissionId::new("mission-1"),
            title: "迁移 Task System v2".to_string(),
            goal: "把 v1 状态推进彻底替换为 v2 4-Tier 21-Layer".to_string(),
            success_criteria: vec!["S1-S18 全部完成".to_string(), "cargo test 通过".to_string()],
            constraints: vec!["不保留 v1 兼容路径".to_string()],
            stakeholders: vec!["用户（架构师）".to_string()],
            created_at: UtcMillis(1_700_000_000_000),
            updated_at: UtcMillis(1_700_000_000_000),
        };
        store.save(&charter).expect("save");
        let loaded = store.load(&charter.mission_id).expect("load").expect("present");
        assert_eq!(loaded, charter);
    }

    #[test]
    fn load_returns_none_when_missing() {
        let (home, ws) = tmp_workspace();
        let store = MissionCharterStore::open_with_home(home.path(), &ws).expect("open");
        assert!(store.load(&MissionId::new("missing")).expect("load").is_none());
    }

    #[test]
    fn render_for_prompt_emits_sections_only_when_populated() {
        let (home, ws) = tmp_workspace();
        let store = MissionCharterStore::open_with_home(home.path(), &ws).expect("open");
        let charter = MissionCharter {
            mission_id: MissionId::new("m"),
            title: "T".to_string(),
            goal: "G".to_string(),
            success_criteria: Vec::new(),
            constraints: vec!["C1".to_string()],
            stakeholders: Vec::new(),
            created_at: UtcMillis(0),
            updated_at: UtcMillis(0),
        };
        store.save(&charter).expect("save");
        let rendered = store
            .render_for_prompt(&charter.mission_id)
            .expect("render")
            .expect("present");
        assert!(rendered.contains("# Mission Charter"));
        assert!(rendered.contains("## Goal"));
        assert!(rendered.contains("## Constraints"));
        assert!(!rendered.contains("## Success Criteria"));
        assert!(!rendered.contains("## Stakeholders"));
    }

    #[test]
    fn parse_mission_charter_write_requires_some_field() {
        let err = parse_mission_charter_write_arguments(&serde_json::json!({}))
            .expect_err("empty must error");
        match err {
            MissionCharterError::InvalidCharter { .. } => {}
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn parse_mission_charter_write_accepts_subset() {
        let args = parse_mission_charter_write_arguments(&serde_json::json!({
            "title": "new",
            "success_criteria": ["a", "b"],
        }))
        .expect("parse");
        assert_eq!(args.title.as_deref(), Some("new"));
        assert!(args.goal.is_none());
        assert_eq!(args.success_criteria.as_deref().unwrap(), &["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn apply_charter_update_bumps_timestamp_when_changed() {
        let mut charter = MissionCharter::new(
            MissionId::new("m"),
            "old".to_string(),
            "g".to_string(),
            UtcMillis(100),
        );
        let args = MissionCharterWriteArgs {
            title: Some("new".to_string()),
            goal: None,
            success_criteria: None,
            constraints: None,
            stakeholders: None,
        };
        let changed = apply_charter_update(&mut charter, args, UtcMillis(200));
        assert!(changed);
        assert_eq!(charter.title, "new");
        assert_eq!(charter.updated_at.0, 200);
    }

    #[test]
    fn apply_charter_update_no_op_keeps_timestamp() {
        let mut charter = MissionCharter::new(
            MissionId::new("m"),
            "same".to_string(),
            "g".to_string(),
            UtcMillis(100),
        );
        let args = MissionCharterWriteArgs {
            title: Some("same".to_string()),
            goal: None,
            success_criteria: None,
            constraints: None,
            stakeholders: None,
        };
        let changed = apply_charter_update(&mut charter, args, UtcMillis(200));
        assert!(!changed);
        assert_eq!(charter.updated_at.0, 100);
    }

    #[test]
    fn registry_caches_per_workspace() {
        let home = tempfile::tempdir().expect("tempdir");
        let registry = MissionCharterRegistry::with_home(home.path().to_path_buf());
        let ws = WorkspaceRootPath::new("/Users/x/proj");
        let a = registry.get_or_open(&ws).expect("open");
        let b = registry.get_or_open(&ws).expect("open again");
        assert!(Arc::ptr_eq(&a, &b));
    }
}
