//! Task System v2 — Tier 4 / L20 Checkpoint：Mission 级别的"可恢复"快照点。
//!
//! 架构定义（参见 docs/task-system-v2/01-architecture.md L20）：
//! 每次进程重启、context 压缩、阶段切换都生成 Checkpoint。Checkpoint 包含 Plan 当前
//! 状态、KG 快照引用、Workspace 工作树指针（git commit）、以及 open Conversations 的
//! mailbox 与 turn 游标。**Checkpoint 让 Mission 跨进程存活**——这是 C 档与 A/B 档
//! 最本质的区别。
//!
//! 本 crate 只承担"记录与查询"职责：
//! - 把一次 Checkpoint 的元数据按时间顺序 append 到 mission 级日志
//! - 渲染 prompt 段落把最近若干 Checkpoint 摊给 Coordinator，让模型理解恢复点
//! - 暴露 `checkpoint_create` 工具入参解析
//!
//! Checkpoint 是**append-only**：每次创建都是新的不可变记录，绝不就地修改历史 Checkpoint
//! ——否则"恢复到 Tn"的语义会被悄悄改写。版本不在单条 record 上递增，而在序号上累积。
//!
//! 物理存储：`~/.magi/projects/{slug}/missions/{mission_id}/checkpoints.md`。
//! 单 mission 单文档，frontmatter 描述元信息，body 用 JSON-lines 记录每个 Checkpoint。
//! 这样既可 grep / diff，又能 round-trip 无损——与同 Tier 的 ValidationReport / KG 同构。

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
// CheckpointKind
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKind {
    /// 进程重启前的兜底快照；恢复时优先从这里读 Plan / KG / Workspace。
    ProcessRestart,
    /// Context 接近上限主动触发；Conversation 即将被压缩。
    ContextCompaction,
    /// 阶段切换边界（Plan 大节点切换）；通常对应一次人审。
    PhaseTransition,
    /// 模型或人显式调用，没有强语义。
    Manual,
}

impl CheckpointKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProcessRestart => "process_restart",
            Self::ContextCompaction => "context_compaction",
            Self::PhaseTransition => "phase_transition",
            Self::Manual => "manual",
        }
    }

    pub fn from_str_lenient(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "process_restart" | "restart" | "shutdown" => Some(Self::ProcessRestart),
            "context_compaction" | "compaction" | "compact" => Some(Self::ContextCompaction),
            "phase_transition" | "phase" | "stage_transition" | "stage" => {
                Some(Self::PhaseTransition)
            }
            "manual" | "user" | "adhoc" | "ad_hoc" => Some(Self::Manual),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ConversationCheckpoint / Checkpoint
// ---------------------------------------------------------------------------

/// 单个 Conversation 在 Checkpoint 时刻的游标。模型不需要看到内部结构，仅作恢复指针。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationCheckpoint {
    pub conversation_id: String,
    /// 最后一个完成的 Turn 序号（None 表示尚未起任何 Turn）。
    #[serde(default)]
    pub turn_cursor: Option<u64>,
    /// 尚未消费的 mailbox 条目数；细节由各自 store 持有，这里只记数便于巡检。
    #[serde(default)]
    pub pending_mailbox: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// mission 内自增的 Checkpoint 序号，从 1 开始。
    pub sequence: u32,
    pub mission_id: MissionId,
    pub kind: CheckpointKind,
    pub created_at: UtcMillis,
    /// 可选的人类可读标签（"完成 UserService 迁移"）。
    #[serde(default)]
    pub label: Option<String>,
    /// 指向 PlanStore 的当前 plan 版本号；None 表示快照时 Plan 尚未落盘。
    #[serde(default)]
    pub plan_version: Option<u32>,
    /// 指向 KnowledgeGraphStore 的当前 fact_count（轻量指针，不复制全图）。
    #[serde(default)]
    pub kg_fact_count: Option<u32>,
    /// 工作目录 git commit SHA。None 表示未受 git 跟踪。
    #[serde(default)]
    pub workspace_commit: Option<String>,
    /// 此 Checkpoint 时活跃的 Conversation 游标列表。
    #[serde(default)]
    pub open_conversations: Vec<ConversationCheckpoint>,
    /// 自由文本备注（限制 1024 字符以内由调用方约束）。
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointLog {
    pub mission_id: MissionId,
    pub checkpoints: Vec<Checkpoint>,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

impl CheckpointLog {
    pub fn new(mission_id: MissionId, now: UtcMillis) -> Self {
        Self {
            mission_id,
            checkpoints: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn latest(&self) -> Option<&Checkpoint> {
        self.checkpoints.last()
    }

    pub fn next_sequence(&self) -> u32 {
        self.checkpoints
            .last()
            .map(|c| c.sequence.saturating_add(1))
            .unwrap_or(1)
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum CheckpointError {
    #[error("无法解析 home 目录")]
    HomeDirUnavailable,
    #[error("checkpoint 数据缺失或非法：{reason}")]
    InvalidRecord { reason: String },
    #[error("checkpoint IO 失败 (path={path}): {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

pub struct CheckpointStore {
    root: PathBuf,
}

impl CheckpointStore {
    pub fn open(workspace_root: &WorkspaceRootPath) -> Result<Self, CheckpointError> {
        let home = dirs_home()?;
        Self::open_with_home(&home, workspace_root)
    }

    pub fn open_with_home(
        magi_home: &Path,
        workspace_root: &WorkspaceRootPath,
    ) -> Result<Self, CheckpointError> {
        let slug = workspace_slug(workspace_root.as_str());
        let root = magi_home.join("projects").join(slug).join("missions");
        fs::create_dir_all(&root).map_err(|source| CheckpointError::Io {
            path: root.clone(),
            source,
        })?;
        Ok(Self { root })
    }

    fn log_path(&self, mission_id: &MissionId) -> PathBuf {
        self.root.join(mission_id.as_str()).join("checkpoints.md")
    }

    pub fn load(
        &self,
        mission_id: &MissionId,
    ) -> Result<Option<CheckpointLog>, CheckpointError> {
        let path = self.log_path(mission_id);
        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(CheckpointError::Io { path, source }),
        };
        parse_log(&raw).map(Some)
    }

    pub fn save(&self, log: &CheckpointLog) -> Result<(), CheckpointError> {
        let path = self.log_path(&log.mission_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| CheckpointError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let rendered = render_log(log);
        fs::write(&path, rendered).map_err(|source| CheckpointError::Io { path, source })
    }

    /// 为 system prompt 渲染最近若干 Checkpoint。空日志返回 None。
    /// 默认只渲染最近 5 个，避免长 mission 把 prompt 撑爆——历史记录在文件里随时可查。
    pub fn render_for_prompt(
        &self,
        mission_id: &MissionId,
    ) -> Result<Option<String>, CheckpointError> {
        let Some(log) = self.load(mission_id)? else {
            return Ok(None);
        };
        if log.checkpoints.is_empty() {
            return Ok(None);
        }
        const PROMPT_TAIL: usize = 5;
        let total = log.checkpoints.len();
        let start = total.saturating_sub(PROMPT_TAIL);
        let recent = &log.checkpoints[start..];

        let mut out = String::new();
        out.push_str("# Mission Checkpoints\n\n");
        out.push_str(&format!("- mission_id: {}\n", log.mission_id.as_str()));
        out.push_str(&format!("- total_checkpoints: {}\n", total));
        out.push_str(&format!("- showing: latest {}\n\n", recent.len()));
        for cp in recent {
            out.push_str(&format!(
                "## #{seq} ({kind}, t={ts})\n",
                seq = cp.sequence,
                kind = cp.kind.as_str(),
                ts = cp.created_at.0,
            ));
            if let Some(label) = &cp.label {
                if !label.is_empty() {
                    out.push_str(&format!("- label: {label}\n"));
                }
            }
            if let Some(pv) = cp.plan_version {
                out.push_str(&format!("- plan_version: {pv}\n"));
            }
            if let Some(kg) = cp.kg_fact_count {
                out.push_str(&format!("- kg_fact_count: {kg}\n"));
            }
            if let Some(commit) = &cp.workspace_commit {
                if !commit.is_empty() {
                    out.push_str(&format!("- workspace_commit: {commit}\n"));
                }
            }
            if !cp.open_conversations.is_empty() {
                out.push_str(&format!(
                    "- open_conversations: {} active\n",
                    cp.open_conversations.len()
                ));
            }
            if let Some(notes) = &cp.notes {
                if !notes.is_empty() {
                    out.push_str(&format!("- notes: {notes}\n"));
                }
            }
            out.push('\n');
        }
        Ok(Some(out))
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// 进程级缓存，按 workspace_root 聚合 CheckpointStore。HOME 不可用时退到
/// `$TMPDIR/magi-checkpoint`，与同 Tier 其它 crate 行为一致。
pub struct CheckpointRegistry {
    inner: RwLock<HashMap<String, Arc<CheckpointStore>>>,
    fallback_home: PathBuf,
}

impl Default for CheckpointRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CheckpointRegistry {
    pub fn new() -> Self {
        let fallback_home = std::env::temp_dir().join("magi-checkpoint");
        let _ = fs::create_dir_all(&fallback_home);
        Self {
            inner: RwLock::new(HashMap::new()),
            fallback_home,
        }
    }

    pub fn get_or_open(
        &self,
        workspace_root: &WorkspaceRootPath,
    ) -> Result<Arc<CheckpointStore>, CheckpointError> {
        let key = workspace_root.as_str().to_string();
        if let Some(store) = self
            .inner
            .read()
            .expect("checkpoint registry poisoned")
            .get(&key)
        {
            return Ok(store.clone());
        }
        let store = match CheckpointStore::open(workspace_root) {
            Ok(store) => store,
            Err(CheckpointError::HomeDirUnavailable) => {
                CheckpointStore::open_with_home(&self.fallback_home, workspace_root)?
            }
            Err(err) => return Err(err),
        };
        let arc = Arc::new(store);
        self.inner
            .write()
            .expect("checkpoint registry poisoned")
            .insert(key, arc.clone());
        Ok(arc)
    }
}

// ---------------------------------------------------------------------------
// Tool argument parsing
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct CheckpointCreateArgs {
    pub kind: CheckpointKind,
    pub label: Option<String>,
    pub plan_version: Option<u32>,
    pub kg_fact_count: Option<u32>,
    pub workspace_commit: Option<String>,
    pub open_conversations: Vec<ConversationCheckpoint>,
    pub notes: Option<String>,
}

pub fn parse_checkpoint_create_arguments(
    raw: &serde_json::Value,
) -> Result<CheckpointCreateArgs, CheckpointError> {
    let obj = raw
        .as_object()
        .ok_or_else(|| CheckpointError::InvalidRecord {
            reason: "arguments 必须为对象".to_string(),
        })?;
    let kind_raw = obj
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| CheckpointError::InvalidRecord {
            reason: "缺少 kind 字段（process_restart/context_compaction/phase_transition/manual）"
                .to_string(),
        })?;
    let kind = CheckpointKind::from_str_lenient(kind_raw).ok_or_else(|| {
        CheckpointError::InvalidRecord {
            reason: format!("kind 非法：{kind_raw}"),
        }
    })?;
    let label = obj
        .get("label")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let plan_version = obj
        .get("plan_version")
        .and_then(|v| v.as_u64())
        .map(|v| u32::try_from(v).unwrap_or(u32::MAX));
    let kg_fact_count = obj
        .get("kg_fact_count")
        .and_then(|v| v.as_u64())
        .map(|v| u32::try_from(v).unwrap_or(u32::MAX));
    let workspace_commit = obj
        .get("workspace_commit")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let notes = obj
        .get("notes")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let mut open_conversations = Vec::new();
    if let Some(arr) = obj.get("open_conversations").and_then(|v| v.as_array()) {
        for item in arr {
            let conv_obj = item
                .as_object()
                .ok_or_else(|| CheckpointError::InvalidRecord {
                    reason: "open_conversations 元素必须为对象".to_string(),
                })?;
            let conversation_id = conv_obj
                .get("conversation_id")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| CheckpointError::InvalidRecord {
                    reason: "open_conversations[].conversation_id 缺失或为空".to_string(),
                })?;
            let turn_cursor = conv_obj.get("turn_cursor").and_then(|v| v.as_u64());
            let pending_mailbox = conv_obj
                .get("pending_mailbox")
                .and_then(|v| v.as_u64())
                .map(|v| u32::try_from(v).unwrap_or(u32::MAX))
                .unwrap_or(0);
            open_conversations.push(ConversationCheckpoint {
                conversation_id,
                turn_cursor,
                pending_mailbox,
            });
        }
    }

    Ok(CheckpointCreateArgs {
        kind,
        label,
        plan_version,
        kg_fact_count,
        workspace_commit,
        open_conversations,
        notes,
    })
}

/// 把一次 Checkpoint 创建参数 append 到 log；序号自动递增。返回新建的 Checkpoint 序号。
pub fn append_checkpoint(
    log: &mut CheckpointLog,
    args: CheckpointCreateArgs,
    now: UtcMillis,
) -> u32 {
    let sequence = log.next_sequence();
    log.checkpoints.push(Checkpoint {
        sequence,
        mission_id: log.mission_id.clone(),
        kind: args.kind,
        created_at: now,
        label: args.label,
        plan_version: args.plan_version,
        kg_fact_count: args.kg_fact_count,
        workspace_commit: args.workspace_commit,
        open_conversations: args.open_conversations,
        notes: args.notes,
    });
    log.updated_at = now;
    sequence
}

// ---------------------------------------------------------------------------
// 序列化 / 反序列化（frontmatter + JSON-lines body）
// ---------------------------------------------------------------------------

fn render_log(log: &CheckpointLog) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("mission_id: {}\n", log.mission_id.as_str()));
    out.push_str(&format!("created_at: {}\n", log.created_at.0));
    out.push_str(&format!("updated_at: {}\n", log.updated_at.0));
    out.push_str(&format!("checkpoint_count: {}\n", log.checkpoints.len()));
    out.push_str("---\n\n");
    out.push_str("## Checkpoints\n");
    for cp in &log.checkpoints {
        let line = serde_json::to_string(cp).expect("Checkpoint 序列化必须成功");
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn parse_log(raw: &str) -> Result<CheckpointLog, CheckpointError> {
    let body_start = raw
        .strip_prefix("---\n")
        .ok_or_else(|| CheckpointError::InvalidRecord {
            reason: "缺少 frontmatter 起始 ---".to_string(),
        })?;
    let (front, body) =
        body_start
            .split_once("\n---\n")
            .ok_or_else(|| CheckpointError::InvalidRecord {
                reason: "缺少 frontmatter 结束 ---".to_string(),
            })?;
    let mut mission_id: Option<MissionId> = None;
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
        let value = value.trim();
        match key.trim() {
            "mission_id" => mission_id = Some(MissionId::new(value.to_string())),
            "created_at" => {
                created_at = Some(value.parse().map_err(|_| CheckpointError::InvalidRecord {
                    reason: format!("created_at 解析失败：{value}"),
                })?)
            }
            "updated_at" => {
                updated_at = Some(value.parse().map_err(|_| CheckpointError::InvalidRecord {
                    reason: format!("updated_at 解析失败：{value}"),
                })?)
            }
            _ => {}
        }
    }
    let mission_id = mission_id.ok_or_else(|| CheckpointError::InvalidRecord {
        reason: "mission_id 缺失".to_string(),
    })?;
    let created_at = UtcMillis(created_at.unwrap_or(0));
    let updated_at = UtcMillis(updated_at.unwrap_or(created_at.0));

    let mut checkpoints = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("##") {
            continue;
        }
        let cp: Checkpoint =
            serde_json::from_str(trimmed).map_err(|err| CheckpointError::InvalidRecord {
                reason: format!("checkpoint 行解析失败：{err} ({trimmed})"),
            })?;
        checkpoints.push(cp);
    }

    Ok(CheckpointLog {
        mission_id,
        checkpoints,
        created_at,
        updated_at,
    })
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn dirs_home() -> Result<PathBuf, CheckpointError> {
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(CheckpointError::HomeDirUnavailable)?;
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn mission() -> MissionId {
        MissionId::new("mission-checkpoint-test".to_string())
    }

    #[test]
    fn append_assigns_monotonic_sequence() {
        let mut log = CheckpointLog::new(mission(), UtcMillis(1000));
        let s1 = append_checkpoint(
            &mut log,
            CheckpointCreateArgs {
                kind: CheckpointKind::PhaseTransition,
                label: Some("phase 1 done".to_string()),
                plan_version: Some(1),
                kg_fact_count: Some(3),
                workspace_commit: Some("abc123".to_string()),
                open_conversations: Vec::new(),
                notes: None,
            },
            UtcMillis(1100),
        );
        let s2 = append_checkpoint(
            &mut log,
            CheckpointCreateArgs {
                kind: CheckpointKind::Manual,
                label: None,
                plan_version: Some(2),
                kg_fact_count: Some(5),
                workspace_commit: None,
                open_conversations: vec![ConversationCheckpoint {
                    conversation_id: "conv-x".to_string(),
                    turn_cursor: Some(7),
                    pending_mailbox: 2,
                }],
                notes: Some("backup".to_string()),
            },
            UtcMillis(1200),
        );
        assert_eq!(s1, 1);
        assert_eq!(s2, 2);
        assert_eq!(log.checkpoints.len(), 2);
        assert_eq!(log.latest().unwrap().sequence, 2);
        assert_eq!(log.updated_at, UtcMillis(1200));
    }

    #[test]
    fn parse_args_validates_required_fields() {
        let err = parse_checkpoint_create_arguments(&serde_json::json!({}))
            .expect_err("缺 kind 必须报错");
        assert!(matches!(err, CheckpointError::InvalidRecord { .. }));

        let err = parse_checkpoint_create_arguments(
            &serde_json::json!({"kind": "wrong"}),
        )
        .expect_err("非法 kind 必须报错");
        assert!(matches!(err, CheckpointError::InvalidRecord { .. }));

        let ok = parse_checkpoint_create_arguments(&serde_json::json!({
            "kind": "compact",
            "label": "before LLM compaction",
            "plan_version": 4,
            "kg_fact_count": 12,
            "workspace_commit": "deadbeef",
            "open_conversations": [{
                "conversation_id": "conv-A",
                "turn_cursor": 9,
                "pending_mailbox": 1
            }],
            "notes": "saved before resume"
        }))
        .expect("合法入参必须解析");
        assert_eq!(ok.kind, CheckpointKind::ContextCompaction);
        assert_eq!(ok.label.as_deref(), Some("before LLM compaction"));
        assert_eq!(ok.plan_version, Some(4));
        assert_eq!(ok.kg_fact_count, Some(12));
        assert_eq!(ok.workspace_commit.as_deref(), Some("deadbeef"));
        assert_eq!(ok.open_conversations.len(), 1);
        assert_eq!(ok.open_conversations[0].conversation_id, "conv-A");
        assert_eq!(ok.open_conversations[0].turn_cursor, Some(9));
        assert_eq!(ok.notes.as_deref(), Some("saved before resume"));
    }

    #[test]
    fn parse_args_rejects_open_conversation_without_id() {
        let err = parse_checkpoint_create_arguments(&serde_json::json!({
            "kind": "manual",
            "open_conversations": [{"turn_cursor": 1}]
        }))
        .expect_err("缺 conversation_id 必须报错");
        assert!(matches!(err, CheckpointError::InvalidRecord { .. }));
    }

    #[test]
    fn render_and_parse_round_trip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws_root = WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store = CheckpointStore::open_with_home(tmp.path(), &ws_root).expect("open store");
        let mut log = CheckpointLog::new(mission(), UtcMillis(1));
        append_checkpoint(
            &mut log,
            CheckpointCreateArgs {
                kind: CheckpointKind::PhaseTransition,
                label: Some("phase 1 done".to_string()),
                plan_version: Some(1),
                kg_fact_count: Some(3),
                workspace_commit: Some("abc123".to_string()),
                open_conversations: vec![ConversationCheckpoint {
                    conversation_id: "conv-1".to_string(),
                    turn_cursor: Some(5),
                    pending_mailbox: 0,
                }],
                notes: None,
            },
            UtcMillis(10),
        );
        append_checkpoint(
            &mut log,
            CheckpointCreateArgs {
                kind: CheckpointKind::ContextCompaction,
                label: None,
                plan_version: Some(1),
                kg_fact_count: Some(5),
                workspace_commit: None,
                open_conversations: Vec::new(),
                notes: Some("compaction before turn 100".to_string()),
            },
            UtcMillis(20),
        );
        store.save(&log).expect("save");
        let reloaded = store
            .load(&mission())
            .expect("load")
            .expect("log saved");
        assert_eq!(reloaded, log);
    }

    #[test]
    fn render_for_prompt_keeps_only_tail_and_returns_none_when_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws_root = WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store = CheckpointStore::open_with_home(tmp.path(), &ws_root).expect("open store");
        // 空 mission 返回 None。
        assert!(
            store
                .render_for_prompt(&mission())
                .expect("render")
                .is_none()
        );

        let mut log = CheckpointLog::new(mission(), UtcMillis(0));
        for i in 0..8 {
            append_checkpoint(
                &mut log,
                CheckpointCreateArgs {
                    kind: CheckpointKind::Manual,
                    label: Some(format!("cp #{i}")),
                    plan_version: Some(i),
                    kg_fact_count: None,
                    workspace_commit: None,
                    open_conversations: Vec::new(),
                    notes: None,
                },
                UtcMillis(i as u64 * 10 + 1),
            );
        }
        store.save(&log).expect("save");
        let rendered = store
            .render_for_prompt(&mission())
            .expect("render")
            .expect("non empty");
        // 默认尾部 5 条，前 3 条不应出现。
        assert!(rendered.contains("total_checkpoints: 8"));
        assert!(rendered.contains("showing: latest 5"));
        assert!(!rendered.contains("cp #0"));
        assert!(!rendered.contains("cp #1"));
        assert!(!rendered.contains("cp #2"));
        assert!(rendered.contains("cp #3"));
        assert!(rendered.contains("cp #7"));
    }

    #[test]
    fn registry_caches_store_by_workspace_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = CheckpointRegistry::new();
        let ws = WorkspaceRootPath::new(format!("{}/sample", tmp.path().display()));
        // 在 sandbox 下 HOME 通常不可写，强制走 fallback。
        unsafe {
            std::env::remove_var("HOME");
        }
        let s1 = registry.get_or_open(&ws).expect("open 1");
        let s2 = registry.get_or_open(&ws).expect("open 2");
        assert!(Arc::ptr_eq(&s1, &s2));
    }
}
