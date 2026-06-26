//! Checkpoint：Mission 级别的"可恢复"快照点。
//!
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

use magi_core::{MissionId, SessionId, UtcMillis, WorkspaceRootPath};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use thiserror::Error;
// --- CheckpointKind

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

    /// 是否要求携带"最小恢复集"。`Manual` 是纯标签型快照，不强制；其它三种都是
    /// 跨进程或跨上下文边界，必须把恢复路径写明，否则未来根本无法 resume。
    /// 跨进程或跨上下文边界必须携带恢复指针，否则恢复入口无法定位执行链。
    pub fn requires_recovery_set(self) -> bool {
        matches!(
            self,
            Self::ProcessRestart | Self::ContextCompaction | Self::PhaseTransition
        )
    }
}
// --- ConversationCheckpoint / Checkpoint

/// 单个 Conversation 在 Checkpoint 时刻的恢复指针。模型只需要看到 session_id /
/// recovery_ref 这两个真正用于"重启时找回 active chain"的字段。
///
/// `recovery_ref` 与 `execution_chain_ref` 至少要存在一个——否则进程重启后
/// 没有任何线索能定位到 SessionRuntimeSidecar，符合 §1.4「恢复集不完整时必须显式失败」。
/// `turn_cursor` 仅用于巡检（最后一个完成的 Turn 序号），不是恢复必需。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationCheckpoint {
    /// 被快照的 Session（= Conversation 标识）。
    pub session_id: SessionId,
    /// 指向 WorkspaceStore 中那条可 resume 的 recovery_id（来自 SessionRuntimeSidecar）。
    /// 在 §1.4 中用于重建 active execution chain，使恢复后能继续推进未完成任务。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_ref: Option<String>,
    /// 指向 ActiveExecutionChain 的 execution_chain_ref；与 recovery_ref 至少存其一。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_chain_ref: Option<String>,
    /// 最后一个完成的 Turn 序号（None 表示尚未起任何 Turn）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_cursor: Option<u64>,
    /// 截至快照时尚未消费的 mailbox 条目数；用于巡检和事件审计，不参与恢复主流程。
    #[serde(default)]
    pub pending_mailbox: u32,
}

impl ConversationCheckpoint {
    /// 该条 conversation 是否携带了至少一个可定位 SessionRuntimeSidecar 的指针。
    pub fn has_recovery_pointer(&self) -> bool {
        let has_recovery = self
            .recovery_ref
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let has_chain = self
            .execution_chain_ref
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        has_recovery || has_chain
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// mission 内自增的 Checkpoint 序号，从 1 开始。
    pub sequence: u32,
    pub mission_id: MissionId,
    pub kind: CheckpointKind,
    pub created_at: UtcMillis,
    /// 可选的人类可读标签（"完成 UserService 迁移"）。仅展示，不参与恢复。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// 指向 PlanStore 的当前 plan 版本号。属于「最小恢复集」，对于
    /// `ProcessRestart / ContextCompaction / PhaseTransition` 必填。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_version: Option<u32>,
    /// 指向 KnowledgeGraphStore 的当前 fact_count（轻量指针，不复制全图）。
    /// 当前作为审计指标保留，未列入强制恢复集。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kg_fact_count: Option<u32>,
    /// 工作目录 git commit SHA。属于「最小恢复集」：恢复时需要把工作树
    /// 重新固定到这个 commit 才能保证后续 Plan step 引用的文件路径一致。
    /// `Manual` kind 不强制；其它三种 kind 必须非空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_commit: Option<String>,
    /// 此 Checkpoint 时活跃的 Conversation 游标列表。空列表表示"快照时无活跃
    /// execution chain"——对恢复相关 kind 仍是合法的（mission 可能正卡在
    /// HumanCheckpoint 上），但任何**存在**的 conversation 都必须携带至少一个
    /// recovery_ref / execution_chain_ref 指针，否则视为恢复集不完整。
    #[serde(default)]
    pub open_conversations: Vec<ConversationCheckpoint>,
    /// 自由文本备注（限制 1024 字符以内由调用方约束）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl Checkpoint {
    /// 按 §1.4 规则校验"最小恢复集"完整性。返回 `Ok(())` 表示该 Checkpoint
    /// 真正具备 resume 能力；返回 `Err(reason)` 时调用方应拒绝落盘。
    ///
    /// 规则：
    /// - `Manual` kind 不要求恢复集，恒返回 `Ok(())`。
    /// - 其它 kind：
    ///   - `workspace_commit` 必须非空（恢复时需要把工作树固定到这个 commit）。
    ///   - 任何**存在**的 `open_conversations` 元素必须携带 `recovery_ref` 或
    ///     `execution_chain_ref` 至少其一；空 `open_conversations` 视为"快照时
    ///     mission 无活跃 chain"，合法。
    pub fn recovery_set_status(&self) -> Result<(), MissingRecoverySetReason> {
        if !self.kind.requires_recovery_set() {
            return Ok(());
        }
        match self.workspace_commit.as_deref().map(str::trim) {
            Some(s) if !s.is_empty() => {}
            _ => return Err(MissingRecoverySetReason::WorkspaceCommitMissing),
        }
        for conv in &self.open_conversations {
            if !conv.has_recovery_pointer() {
                return Err(MissingRecoverySetReason::ConversationPointerMissing {
                    session_id: conv.session_id.as_str().to_string(),
                });
            }
        }
        Ok(())
    }
}

/// 描述「最小恢复集」缺哪一块——错误信息要可以直接交给上游展示，不依赖外部上下文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MissingRecoverySetReason {
    /// 没有 workspace_commit 指针；恢复时无法固定工作树状态。
    WorkspaceCommitMissing,
    /// 某条 open_conversations 既没有 recovery_ref 也没有 execution_chain_ref。
    ConversationPointerMissing { session_id: String },
}

impl std::fmt::Display for MissingRecoverySetReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WorkspaceCommitMissing => {
                write!(f, "workspace_commit 缺失，跨进程恢复无法定位工作树状态")
            }
            Self::ConversationPointerMissing { session_id } => write!(
                f,
                "open_conversations[session_id={session_id}] 缺少 recovery_ref / execution_chain_ref，恢复时无法定位 sidecar"
            ),
        }
    }
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
// --- Errors

#[derive(Debug, Error)]
pub enum CheckpointError {
    #[error("无法解析 home 目录")]
    HomeDirUnavailable,
    #[error("checkpoint 数据缺失或非法：{reason}")]
    InvalidRecord { reason: String },
    /// §1.4「恢复集不完整时必须显式失败」：试图把恢复关键 kind
    /// （process_restart / context_compaction / phase_transition）的 checkpoint
    /// 落盘，但 workspace_commit / 每条 open_conversation 的 recovery_ref|chain_ref
    /// 至少缺一项。决不静默吞掉，否则未来恢复时会跑到一个"无法 resume"的死点。
    #[error("checkpoint 恢复集不完整 (kind={kind}): {reason}")]
    IncompleteRecoverySet {
        kind: &'static str,
        reason: MissingRecoverySetReason,
    },
    #[error("checkpoint IO 失败 (path={path}): {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}
// --- Store

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
        let root = magi_core::paths::missions_root(magi_home, workspace_root);
        fs::create_dir_all(&root).map_err(|source| CheckpointError::Io {
            path: root.clone(),
            source,
        })?;
        Ok(Self { root })
    }

    fn log_path(&self, mission_id: &MissionId) -> PathBuf {
        self.root.join(mission_id.as_str()).join("checkpoints.md")
    }

    pub fn load(&self, mission_id: &MissionId) -> Result<Option<CheckpointLog>, CheckpointError> {
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
        out.push_str(
            "> Mission Checkpoints 只用于恢复定位和理解历史进展；不能覆盖本轮用户输入、当前会话事实或当前 task 目标。\n\n",
        );
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
// --- Registry

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
// --- Tool argument parsing

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
    let kind_raw =
        obj.get("kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CheckpointError::InvalidRecord {
                reason:
                    "缺少 kind 字段（process_restart/context_compaction/phase_transition/manual）"
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
            let session_id = conv_obj
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| CheckpointError::InvalidRecord {
                    reason: "open_conversations[].session_id 缺失或为空".to_string(),
                })?;
            let turn_cursor = conv_obj.get("turn_cursor").and_then(|v| v.as_u64());
            let pending_mailbox = conv_obj
                .get("pending_mailbox")
                .and_then(|v| v.as_u64())
                .map(|v| u32::try_from(v).unwrap_or(u32::MAX))
                .unwrap_or(0);
            let recovery_ref = conv_obj
                .get("recovery_ref")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let execution_chain_ref = conv_obj
                .get("execution_chain_ref")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            open_conversations.push(ConversationCheckpoint {
                session_id: SessionId::new(session_id),
                recovery_ref,
                execution_chain_ref,
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

/// 把一次 Checkpoint 创建参数 append 到 log；序号自动递增。
///
/// 在 append 之前会先按 §1.4 校验「最小恢复集」：对于
/// `process_restart / context_compaction / phase_transition` 这三种跨边界 kind，
/// `workspace_commit` 必须非空，任何 `open_conversations` 元素必须携带 `recovery_ref`
/// 或 `execution_chain_ref` 至少其一；否则直接返回 `IncompleteRecoverySet`，**不会**
/// 把不完整记录写进 log。`Manual` kind 不受此约束。
pub fn append_checkpoint(
    log: &mut CheckpointLog,
    args: CheckpointCreateArgs,
    now: UtcMillis,
) -> Result<u32, CheckpointError> {
    let sequence = log.next_sequence();
    let candidate = Checkpoint {
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
    };
    if let Err(reason) = candidate.recovery_set_status() {
        return Err(CheckpointError::IncompleteRecoverySet {
            kind: candidate.kind.as_str(),
            reason,
        });
    }
    log.checkpoints.push(candidate);
    log.updated_at = now;
    Ok(sequence)
}
// --- 序列化 / 反序列化（frontmatter + JSON-lines body）

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
// --- helpers

fn dirs_home() -> Result<PathBuf, CheckpointError> {
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(CheckpointError::HomeDirUnavailable)?;
    Ok(base.join(".magi"))
}
// --- Orchestration tool entry：checkpoint_create

/// S16：`checkpoint_create` 工具实现。append-only 写入 mission 维度的
/// `checkpoints.md`，并通过 `task.checkpoint.appended` 域事件广播给观察者。
/// 入参与 `&magi_core::Task` 解耦，仅依赖 `task_id`/`mission_id`，
/// 为后续删除 `magi-core::Task` 做准备。
pub fn execute_checkpoint_create_tool(
    event_bus: &magi_event_bus::InMemoryEventBus,
    store: Option<&CheckpointStore>,
    session_id: &magi_core::SessionId,
    workspace_id: Option<&magi_core::WorkspaceId>,
    task_id: &magi_core::TaskId,
    mission_id: &magi_core::MissionId,
    arguments: &str,
) -> (String, magi_core::ExecutionResultStatus) {
    use magi_core::{EventId, ExecutionResultStatus, UtcMillis};
    use magi_event_bus::{EventContext, EventEnvelope};

    let Some(store) = store else {
        return (
            serde_json::json!({
                "tool": "checkpoint_create",
                "status": "failed",
                "error": "当前 task 未绑定 workspace，无法定位 mission checkpoint store",
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    };
    let args_value: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "checkpoint_create",
                    "status": "failed",
                    "error": format!("arguments 非合法 JSON：{err}"),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let args = match parse_checkpoint_create_arguments(&args_value) {
        Ok(parsed) => parsed,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "checkpoint_create",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let now = UtcMillis::now();
    let mut log = match store.load(mission_id) {
        Ok(Some(existing)) => existing,
        Ok(None) => CheckpointLog::new(mission_id.clone(), now),
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "checkpoint_create",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let kind = args.kind;
    let label_snapshot = args.label.clone();
    let sequence = match append_checkpoint(&mut log, args, now) {
        Ok(seq) => seq,
        Err(err @ CheckpointError::IncompleteRecoverySet { .. }) => {
            return (
                serde_json::json!({
                    "tool": "checkpoint_create",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "checkpoint_create",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    if let Err(err) = store.save(&log) {
        return (
            serde_json::json!({
                "tool": "checkpoint_create",
                "status": "failed",
                "error": err.to_string(),
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    }
    let payload = serde_json::json!({
        "tool": "checkpoint_create",
        "status": "succeeded",
        "mission_id": log.mission_id.to_string(),
        "sequence": sequence,
        "kind": kind.as_str(),
        "label": label_snapshot,
        "checkpoint_count": log.checkpoints.len(),
    });
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-checkpoint-appended-{}", UtcMillis::now().0)),
            "task.checkpoint.appended",
            serde_json::json!({
                "task_id": task_id.to_string(),
                "mission_id": log.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.map(ToString::to_string),
                "sequence": sequence,
                "kind": kind.as_str(),
                "label": label_snapshot,
                "checkpoint_count": log.checkpoints.len(),
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.cloned(),
            session_id: Some(session_id.clone()),
            mission_id: Some(mission_id.clone()),
            task_id: Some(task_id.clone()),
            ..EventContext::default()
        }),
    );
    (payload.to_string(), ExecutionResultStatus::Succeeded)
}
// --- Tests

#[cfg(test)]
mod tests {
    use super::*;

    fn mission() -> MissionId {
        MissionId::new("mission-checkpoint-test".to_string())
    }

    fn full_recovery_args(kind: CheckpointKind) -> CheckpointCreateArgs {
        CheckpointCreateArgs {
            kind,
            label: Some("phase 1 done".to_string()),
            plan_version: Some(1),
            kg_fact_count: Some(3),
            workspace_commit: Some("abc123".to_string()),
            open_conversations: vec![ConversationCheckpoint {
                session_id: SessionId::new("session-A"),
                recovery_ref: Some("rec-A".to_string()),
                execution_chain_ref: Some("chain-A".to_string()),
                turn_cursor: Some(7),
                pending_mailbox: 2,
            }],
            notes: None,
        }
    }

    #[test]
    fn append_assigns_monotonic_sequence_when_recovery_set_complete() {
        let mut log = CheckpointLog::new(mission(), UtcMillis(1000));
        let s1 = append_checkpoint(
            &mut log,
            full_recovery_args(CheckpointKind::PhaseTransition),
            UtcMillis(1100),
        )
        .expect("完整恢复集应通过");
        let s2 = append_checkpoint(
            &mut log,
            CheckpointCreateArgs {
                kind: CheckpointKind::Manual,
                label: None,
                plan_version: Some(2),
                kg_fact_count: Some(5),
                workspace_commit: None,
                open_conversations: vec![ConversationCheckpoint {
                    // Manual kind 不强制 recovery_ref/execution_chain_ref。
                    session_id: SessionId::new("conv-x"),
                    recovery_ref: None,
                    execution_chain_ref: None,
                    turn_cursor: Some(7),
                    pending_mailbox: 2,
                }],
                notes: Some("backup".to_string()),
            },
            UtcMillis(1200),
        )
        .expect("Manual kind 无恢复集约束");
        assert_eq!(s1, 1);
        assert_eq!(s2, 2);
        assert_eq!(log.checkpoints.len(), 2);
        assert_eq!(log.latest().unwrap().sequence, 2);
        assert_eq!(log.updated_at, UtcMillis(1200));
    }

    #[test]
    fn append_rejects_recovery_kind_without_workspace_commit() {
        let mut log = CheckpointLog::new(mission(), UtcMillis(0));
        let err = append_checkpoint(
            &mut log,
            CheckpointCreateArgs {
                kind: CheckpointKind::ProcessRestart,
                label: None,
                plan_version: None,
                kg_fact_count: None,
                workspace_commit: None, // 这里缺失，必然失败
                open_conversations: Vec::new(),
                notes: None,
            },
            UtcMillis(1),
        )
        .expect_err("workspace_commit 缺失必须显式失败");
        match err {
            CheckpointError::IncompleteRecoverySet { kind, reason } => {
                assert_eq!(kind, "process_restart");
                assert_eq!(reason, MissingRecoverySetReason::WorkspaceCommitMissing);
            }
            other => panic!("意外错误：{other}"),
        }
        assert!(log.checkpoints.is_empty(), "失败时不得污染 log");
    }

    #[test]
    fn append_rejects_conversation_without_recovery_pointer() {
        let mut log = CheckpointLog::new(mission(), UtcMillis(0));
        let err = append_checkpoint(
            &mut log,
            CheckpointCreateArgs {
                kind: CheckpointKind::ContextCompaction,
                label: None,
                plan_version: Some(3),
                kg_fact_count: None,
                workspace_commit: Some("commit".to_string()),
                open_conversations: vec![ConversationCheckpoint {
                    session_id: SessionId::new("s1"),
                    recovery_ref: None,
                    execution_chain_ref: None,
                    turn_cursor: None,
                    pending_mailbox: 0,
                }],
                notes: None,
            },
            UtcMillis(1),
        )
        .expect_err("缺 recovery_ref + execution_chain_ref 必须显式失败");
        match err {
            CheckpointError::IncompleteRecoverySet {
                reason: MissingRecoverySetReason::ConversationPointerMissing { session_id },
                ..
            } => assert_eq!(session_id, "s1"),
            other => panic!("意外错误：{other}"),
        }
        assert!(log.checkpoints.is_empty());
    }

    #[test]
    fn append_recovery_kind_with_empty_conversations_is_allowed_when_commit_present() {
        let mut log = CheckpointLog::new(mission(), UtcMillis(0));
        // mission 可能正卡在 HumanCheckpoint 上：没有活跃 chain，但需要快照工作树状态。
        let seq = append_checkpoint(
            &mut log,
            CheckpointCreateArgs {
                kind: CheckpointKind::PhaseTransition,
                label: None,
                plan_version: Some(1),
                kg_fact_count: None,
                workspace_commit: Some("sha".to_string()),
                open_conversations: Vec::new(),
                notes: None,
            },
            UtcMillis(1),
        )
        .expect("空 open_conversations 仅在有 workspace_commit 时合法");
        assert_eq!(seq, 1);
    }

    #[test]
    fn parse_args_validates_required_fields_and_pointers() {
        let err = parse_checkpoint_create_arguments(&serde_json::json!({}))
            .expect_err("缺 kind 必须报错");
        assert!(matches!(err, CheckpointError::InvalidRecord { .. }));

        let err = parse_checkpoint_create_arguments(&serde_json::json!({"kind": "wrong"}))
            .expect_err("非法 kind 必须报错");
        assert!(matches!(err, CheckpointError::InvalidRecord { .. }));

        let ok = parse_checkpoint_create_arguments(&serde_json::json!({
            "kind": "compact",
            "label": "before LLM compaction",
            "plan_version": 4,
            "kg_fact_count": 12,
            "workspace_commit": "deadbeef",
            "open_conversations": [{
                "session_id": "session-A",
                "recovery_ref": "rec-X",
                "execution_chain_ref": "chain-Y",
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
        assert_eq!(ok.open_conversations[0].session_id.as_str(), "session-A");
        assert_eq!(
            ok.open_conversations[0].recovery_ref.as_deref(),
            Some("rec-X")
        );
        assert_eq!(
            ok.open_conversations[0].execution_chain_ref.as_deref(),
            Some("chain-Y")
        );
        assert_eq!(ok.open_conversations[0].turn_cursor, Some(9));
        assert_eq!(ok.notes.as_deref(), Some("saved before resume"));
    }

    #[test]
    fn parse_args_rejects_open_conversation_without_session_id() {
        let err = parse_checkpoint_create_arguments(&serde_json::json!({
            "kind": "manual",
            "open_conversations": [{"turn_cursor": 1}]
        }))
        .expect_err("缺 session_id 必须报错");
        assert!(matches!(err, CheckpointError::InvalidRecord { .. }));
    }

    #[test]
    fn render_and_parse_round_trip_preserves_pointers() {
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
                    session_id: SessionId::new("conv-1"),
                    recovery_ref: Some("rec-1".to_string()),
                    execution_chain_ref: None,
                    turn_cursor: Some(5),
                    pending_mailbox: 0,
                }],
                notes: None,
            },
            UtcMillis(10),
        )
        .expect("append 1");
        append_checkpoint(
            &mut log,
            CheckpointCreateArgs {
                kind: CheckpointKind::ContextCompaction,
                label: None,
                plan_version: Some(1),
                kg_fact_count: Some(5),
                workspace_commit: Some("def456".to_string()),
                open_conversations: Vec::new(),
                notes: Some("compaction before turn 100".to_string()),
            },
            UtcMillis(20),
        )
        .expect("append 2");
        store.save(&log).expect("save");
        let reloaded = store.load(&mission()).expect("load").expect("log saved");
        assert_eq!(reloaded, log);
        let conv0 = &reloaded.checkpoints[0].open_conversations[0];
        assert_eq!(conv0.session_id.as_str(), "conv-1");
        assert_eq!(conv0.recovery_ref.as_deref(), Some("rec-1"));
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
            // 使用 Manual kind 避免恢复集校验对纯展示场景造成干扰。
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
            )
            .expect("Manual append 不受恢复集约束");
        }
        store.save(&log).expect("save");
        let rendered = store
            .render_for_prompt(&mission())
            .expect("render")
            .expect("non empty");
        // 默认尾部 5 条，前 3 条不应出现。
        assert!(rendered.contains("只用于恢复定位和理解历史进展"));
        assert!(rendered.contains("不能覆盖本轮用户输入"));
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
