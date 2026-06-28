//! Mission 维度记账 sidecar：累计 turn 数、token 与 wall-clock。
//!
//! 设计目标：
//! - **单一写点**：在 `conversation-runtime` turn 结束时调用 `record_turn` 累加；
//!   不允许多个组件并发追加（mission 内顺序串行轮次，天然单写者）。
//! - **原子写**：`tmp + rename` 避免半写文件；schema_version 字段防止
//!   未来格式漂移导致静默错读。
//! - **存在性即可选**：metrics.md 在第一次记账前不存在，`load` 返回 `Ok(None)`；
//!   `record_turn` 自动 create-or-update。
//! - **反孤儿**：[`magi_mission::MissionAggregate::metrics`] 通过本 crate
//!   读取，保证读端在 §1.4 聚合根上有显式调用方。
//!
//! 文件格式（v1）——YAML 风格 frontmatter + 空 body：
//! ```text
//! ---
//! schema_version: 1
//! mission_id: M-xyz
//! turn_count: 12
//! total_prompt_tokens: 4523
//! total_completion_tokens: 1024
//! total_tokens: 5547
//! first_turn_started_at: 1715000000000
//! last_turn_finished_at: 1715000540000
//! wall_clock_millis: 540000
//! last_lifecycle_phase: executing
//! ---
//! ```
//!
//! 反向兼容：`schema_version` ≠ 1 视为格式错误，调用方应升级而非静默丢弃。

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use magi_core::{MissionId, MissionLifecyclePhase, UtcMillis, WorkspaceRootPath};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 当前 schema 版本号，写入并校验该字段。
pub const SCHEMA_VERSION: u32 = 1;

/// mission 累计指标（持久化结构）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionMetrics {
    pub schema_version: u32,
    pub mission_id: MissionId,
    pub turn_count: u64,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_tokens: u64,
    pub first_turn_started_at: Option<UtcMillis>,
    pub last_turn_finished_at: Option<UtcMillis>,
    pub wall_clock_millis: u64,
    pub last_lifecycle_phase: Option<MissionLifecyclePhase>,
}

impl MissionMetrics {
    pub fn new(mission_id: MissionId) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            mission_id,
            turn_count: 0,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            total_tokens: 0,
            first_turn_started_at: None,
            last_turn_finished_at: None,
            wall_clock_millis: 0,
            last_lifecycle_phase: None,
        }
    }

    /// 在累加器上叠加一次 turn 用量。
    pub fn apply_turn(&mut self, usage: &TurnUsage) {
        self.turn_count = self.turn_count.saturating_add(1);
        self.total_prompt_tokens = self.total_prompt_tokens.saturating_add(usage.prompt_tokens);
        self.total_completion_tokens = self
            .total_completion_tokens
            .saturating_add(usage.completion_tokens);
        self.total_tokens = self
            .total_tokens
            .saturating_add(usage.prompt_tokens)
            .saturating_add(usage.completion_tokens);
        if self.first_turn_started_at.is_none() {
            self.first_turn_started_at = Some(usage.started_at);
        }
        self.last_turn_finished_at = Some(usage.finished_at);
        let delta = usage.finished_at.0.saturating_sub(usage.started_at.0);
        self.wall_clock_millis = self.wall_clock_millis.saturating_add(delta);
        self.last_lifecycle_phase = usage.phase;
    }
}

/// 一次 turn 的用量切片，由 conversation-runtime 收集后传入。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub started_at: UtcMillis,
    pub finished_at: UtcMillis,
    pub phase: Option<MissionLifecyclePhase>,
}

#[derive(Debug, Error)]
pub enum MissionMetricsError {
    #[error("metrics IO 失败 path={path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("metrics 文件 schema_version 不匹配：期望 {expected}，实际 {actual}（path={path}）")]
    SchemaMismatch {
        path: PathBuf,
        expected: u32,
        actual: u32,
    },
    #[error("metrics 解析失败 path={path}: {reason}")]
    ParseError { path: PathBuf, reason: String },
}

pub struct MissionMetricsStore {
    root: PathBuf,
}

impl MissionMetricsStore {
    pub fn open_with_home(
        magi_home: &Path,
        workspace_root: &WorkspaceRootPath,
    ) -> Result<Self, MissionMetricsError> {
        let root = magi_core::paths::missions_root(magi_home, workspace_root);
        fs::create_dir_all(&root).map_err(|source| MissionMetricsError::Io {
            path: root.clone(),
            source,
        })?;
        Ok(Self { root })
    }

    fn file_path(&self, mission_id: &MissionId) -> PathBuf {
        self.root.join(mission_id.as_str()).join("metrics.md")
    }

    pub fn load(
        &self,
        mission_id: &MissionId,
    ) -> Result<Option<MissionMetrics>, MissionMetricsError> {
        let path = self.file_path(mission_id);
        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(MissionMetricsError::Io { path, source }),
        };
        let metrics = parse_metrics(&path, &raw)?;
        if metrics.schema_version != SCHEMA_VERSION {
            return Err(MissionMetricsError::SchemaMismatch {
                path,
                expected: SCHEMA_VERSION,
                actual: metrics.schema_version,
            });
        }
        Ok(Some(metrics))
    }

    pub fn record_turn(
        &self,
        mission_id: &MissionId,
        usage: TurnUsage,
    ) -> Result<MissionMetrics, MissionMetricsError> {
        let mut metrics = self
            .load(mission_id)?
            .unwrap_or_else(|| MissionMetrics::new(mission_id.clone()));
        metrics.apply_turn(&usage);
        self.save(&metrics)?;
        Ok(metrics)
    }

    fn save(&self, metrics: &MissionMetrics) -> Result<(), MissionMetricsError> {
        let path = self.file_path(&metrics.mission_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| MissionMetricsError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let rendered = render_metrics(metrics);
        magi_core::fs_atomic::write_atomic(&path, rendered)
            .map_err(|source| MissionMetricsError::Io { path, source })
    }
}

fn render_metrics(metrics: &MissionMetrics) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("schema_version: {}\n", metrics.schema_version));
    out.push_str(&format!("mission_id: {}\n", metrics.mission_id.as_str()));
    out.push_str(&format!("turn_count: {}\n", metrics.turn_count));
    out.push_str(&format!(
        "total_prompt_tokens: {}\n",
        metrics.total_prompt_tokens
    ));
    out.push_str(&format!(
        "total_completion_tokens: {}\n",
        metrics.total_completion_tokens
    ));
    out.push_str(&format!("total_tokens: {}\n", metrics.total_tokens));
    out.push_str(&format!(
        "first_turn_started_at: {}\n",
        metrics
            .first_turn_started_at
            .map(|m| m.0.to_string())
            .unwrap_or_else(|| "null".to_string())
    ));
    out.push_str(&format!(
        "last_turn_finished_at: {}\n",
        metrics
            .last_turn_finished_at
            .map(|m| m.0.to_string())
            .unwrap_or_else(|| "null".to_string())
    ));
    out.push_str(&format!(
        "wall_clock_millis: {}\n",
        metrics.wall_clock_millis
    ));
    out.push_str(&format!(
        "last_lifecycle_phase: {}\n",
        metrics
            .last_lifecycle_phase
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| "null".to_string())
    ));
    out.push_str("---\n");
    out
}

fn parse_metrics(path: &Path, raw: &str) -> Result<MissionMetrics, MissionMetricsError> {
    let trimmed = raw.trim();
    let inner = trimmed
        .strip_prefix("---")
        .and_then(|s| s.trim_start_matches('\n').strip_suffix("---"))
        .or_else(|| {
            trimmed
                .strip_prefix("---")
                .map(|s| s.trim_start_matches('\n'))
        })
        .ok_or_else(|| MissionMetricsError::ParseError {
            path: path.to_path_buf(),
            reason: "缺少 `---` frontmatter 包围".to_string(),
        })?
        .trim();

    let mut schema_version: Option<u32> = None;
    let mut mission_id: Option<String> = None;
    let mut turn_count: u64 = 0;
    let mut total_prompt_tokens: u64 = 0;
    let mut total_completion_tokens: u64 = 0;
    let mut total_tokens: u64 = 0;
    let mut first_turn_started_at: Option<UtcMillis> = None;
    let mut last_turn_finished_at: Option<UtcMillis> = None;
    let mut wall_clock_millis: u64 = 0;
    let mut last_lifecycle_phase: Option<MissionLifecyclePhase> = None;

    for line in inner.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| MissionMetricsError::ParseError {
                path: path.to_path_buf(),
                reason: format!("无法解析行 `{line}`"),
            })?;
        let key = key.trim();
        let value = value.trim();
        match key {
            "schema_version" => {
                schema_version =
                    Some(value.parse().map_err(|_| MissionMetricsError::ParseError {
                        path: path.to_path_buf(),
                        reason: format!("schema_version 非数字: {value}"),
                    })?);
            }
            "mission_id" => mission_id = Some(value.to_string()),
            "turn_count" => turn_count = parse_u64(path, key, value)?,
            "total_prompt_tokens" => total_prompt_tokens = parse_u64(path, key, value)?,
            "total_completion_tokens" => total_completion_tokens = parse_u64(path, key, value)?,
            "total_tokens" => total_tokens = parse_u64(path, key, value)?,
            "first_turn_started_at" => {
                first_turn_started_at = parse_opt_millis(path, key, value)?;
            }
            "last_turn_finished_at" => {
                last_turn_finished_at = parse_opt_millis(path, key, value)?;
            }
            "wall_clock_millis" => wall_clock_millis = parse_u64(path, key, value)?,
            "last_lifecycle_phase" => {
                last_lifecycle_phase = parse_opt_phase(path, value)?;
            }
            _ => {}
        }
    }

    let schema_version = schema_version.ok_or_else(|| MissionMetricsError::ParseError {
        path: path.to_path_buf(),
        reason: "缺少 schema_version".to_string(),
    })?;
    let mission_id = mission_id.ok_or_else(|| MissionMetricsError::ParseError {
        path: path.to_path_buf(),
        reason: "缺少 mission_id".to_string(),
    })?;

    Ok(MissionMetrics {
        schema_version,
        mission_id: MissionId::new(mission_id),
        turn_count,
        total_prompt_tokens,
        total_completion_tokens,
        total_tokens,
        first_turn_started_at,
        last_turn_finished_at,
        wall_clock_millis,
        last_lifecycle_phase,
    })
}

fn parse_u64(path: &Path, key: &str, value: &str) -> Result<u64, MissionMetricsError> {
    value.parse().map_err(|_| MissionMetricsError::ParseError {
        path: path.to_path_buf(),
        reason: format!("{key} 非整数: {value}"),
    })
}

fn parse_opt_millis(
    path: &Path,
    key: &str,
    value: &str,
) -> Result<Option<UtcMillis>, MissionMetricsError> {
    if value == "null" {
        return Ok(None);
    }
    Ok(Some(UtcMillis(parse_u64(path, key, value)?)))
}

fn parse_opt_phase(
    path: &Path,
    value: &str,
) -> Result<Option<MissionLifecyclePhase>, MissionMetricsError> {
    Ok(match value {
        "null" => None,
        "charter_draft" => Some(MissionLifecyclePhase::CharterDraft),
        "awaiting_human_checkpoint" => Some(MissionLifecyclePhase::AwaitingHumanCheckpoint),
        "plan_ready" => Some(MissionLifecyclePhase::PlanReady),
        "executing" => Some(MissionLifecyclePhase::Executing),
        "all_steps_completed" => Some(MissionLifecyclePhase::AllStepsCompleted),
        other => {
            return Err(MissionMetricsError::ParseError {
                path: path.to_path_buf(),
                reason: format!("未知 last_lifecycle_phase: {other}"),
            });
        }
    })
}

/// 跨 workspace 复用 `MissionMetricsStore` 的轻量缓存。
///
/// 与 `MissionCharterRegistry` 等同构：daemon bootstrap 构造一次后通过
/// builder 注入 dispatcher，dispatch 路径用 `get_or_open` 拿到 per-workspace
/// 的 store 写入。
pub struct MissionMetricsRegistry {
    inner: RwLock<HashMap<String, Arc<MissionMetricsStore>>>,
    home: PathBuf,
}

impl MissionMetricsRegistry {
    pub fn new() -> Self {
        Self::with_home(dirs_home().expect("HOME 目录不可用，无法定位 Magi 状态根"))
    }

    pub fn with_home(home: PathBuf) -> Self {
        let _ = fs::create_dir_all(&home);
        Self {
            inner: RwLock::new(HashMap::new()),
            home,
        }
    }

    pub fn get_or_open(
        &self,
        workspace_root: &WorkspaceRootPath,
    ) -> Result<Arc<MissionMetricsStore>, MissionMetricsError> {
        let key = workspace_root.as_str().to_string();
        if let Some(found) = self
            .inner
            .read()
            .expect("metrics registry read lock")
            .get(&key)
            .cloned()
        {
            return Ok(found);
        }
        let store = MissionMetricsStore::open_with_home(&self.home, workspace_root)?;
        let arc = Arc::new(store);
        self.inner
            .write()
            .expect("metrics registry write lock")
            .insert(key, arc.clone());
        Ok(arc)
    }
}

impl Default for MissionMetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".magi"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn harness() -> (TempDir, MissionMetricsStore) {
        let tmp = TempDir::new().unwrap();
        let ws = WorkspaceRootPath::from("/Users/test/proj");
        let store = MissionMetricsStore::open_with_home(tmp.path(), &ws).unwrap();
        (tmp, store)
    }

    #[test]
    fn load_returns_none_when_metrics_absent() {
        let (_tmp, store) = harness();
        let mid = MissionId::new("M-1");
        assert!(store.load(&mid).unwrap().is_none());
    }

    #[test]
    fn record_turn_creates_and_accumulates() {
        let (_tmp, store) = harness();
        let mid = MissionId::new("M-2");
        let first = store
            .record_turn(
                &mid,
                TurnUsage {
                    prompt_tokens: 100,
                    completion_tokens: 30,
                    started_at: UtcMillis(1_000),
                    finished_at: UtcMillis(1_500),
                    phase: Some(MissionLifecyclePhase::Executing),
                },
            )
            .unwrap();
        assert_eq!(first.turn_count, 1);
        assert_eq!(first.total_tokens, 130);
        assert_eq!(first.wall_clock_millis, 500);
        assert_eq!(first.first_turn_started_at, Some(UtcMillis(1_000)));

        let second = store
            .record_turn(
                &mid,
                TurnUsage {
                    prompt_tokens: 50,
                    completion_tokens: 20,
                    started_at: UtcMillis(2_000),
                    finished_at: UtcMillis(2_300),
                    phase: Some(MissionLifecyclePhase::AllStepsCompleted),
                },
            )
            .unwrap();
        assert_eq!(second.turn_count, 2);
        assert_eq!(second.total_prompt_tokens, 150);
        assert_eq!(second.total_completion_tokens, 50);
        assert_eq!(second.total_tokens, 200);
        assert_eq!(second.wall_clock_millis, 800);
        assert_eq!(second.first_turn_started_at, Some(UtcMillis(1_000)));
        assert_eq!(second.last_turn_finished_at, Some(UtcMillis(2_300)));
        assert_eq!(
            second.last_lifecycle_phase,
            Some(MissionLifecyclePhase::AllStepsCompleted)
        );

        let reloaded = store.load(&mid).unwrap().unwrap();
        assert_eq!(reloaded, second);
    }

    #[test]
    fn load_rejects_when_schema_version_mismatched() {
        let (_tmp, store) = harness();
        let mid = MissionId::new("M-3");
        let path = store.file_path(&mid);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let raw = "---\nschema_version: 999\nmission_id: M-3\nturn_count: 0\ntotal_prompt_tokens: 0\ntotal_completion_tokens: 0\ntotal_tokens: 0\nfirst_turn_started_at: null\nlast_turn_finished_at: null\nwall_clock_millis: 0\nlast_lifecycle_phase: null\n---\n";
        std::fs::write(&path, raw).unwrap();
        let err = store.load(&mid).unwrap_err();
        assert!(matches!(
            err,
            MissionMetricsError::SchemaMismatch {
                expected: 1,
                actual: 999,
                ..
            }
        ));
    }

    #[test]
    fn record_turn_writes_via_tmp_rename_atomic() {
        let (_tmp, store) = harness();
        let mid = MissionId::new("M-4");
        store
            .record_turn(
                &mid,
                TurnUsage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    started_at: UtcMillis(10),
                    finished_at: UtcMillis(20),
                    phase: None,
                },
            )
            .unwrap();
        let parent = store.file_path(&mid).parent().unwrap().to_path_buf();
        let leftover_tmp = std::fs::read_dir(&parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some("tmp"));
        assert!(!leftover_tmp, "tmp 文件应已 rename，不应残留");
    }

    #[test]
    fn registry_reuses_store_per_workspace() {
        let tmp = TempDir::new().unwrap();
        let registry = MissionMetricsRegistry::with_home(tmp.path().to_path_buf());
        let ws = WorkspaceRootPath::from("/Users/test/proj");
        let a = registry.get_or_open(&ws).expect("open");
        let b = registry.get_or_open(&ws).expect("open again");
        assert!(
            Arc::ptr_eq(&a, &b),
            "同 workspace 第二次开 store 应命中缓存"
        );
    }
}
