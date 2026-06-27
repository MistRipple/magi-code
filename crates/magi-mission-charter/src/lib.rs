//! 任务系统 — Tier 4 / L15 MissionCharter：mission 的"宪章"契约。
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
// --- MissionCharter

/// Charter 生命周期状态。
///
/// `Draft`：澄清阶段，可以自由更新。`Frozen`：契约已固化，所有写入必须绑定一条
/// 已 Approved 的 HumanCheckpoint，对应 §1.6 / 02-migration-plan.md P4 验收条件。
///
/// 持久化时 frontmatter 的 `state` 字段必须显式存在，缺失直接报错——不提供"缺省
/// 视为 Draft"的回退路径，避免把数据格式问题降级成静默行为。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CharterState {
    Draft,
    Frozen,
}

impl CharterState {
    pub fn as_str(self) -> &'static str {
        match self {
            CharterState::Draft => "draft",
            CharterState::Frozen => "frozen",
        }
    }

    pub fn is_frozen(self) -> bool {
        matches!(self, CharterState::Frozen)
    }
}

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
    /// 生命周期状态。frontmatter 必须显式提供 `state` 字段；持久化层不接受缺省回退。
    pub state: CharterState,
    /// 最近一次被消费过的 HumanCheckpoint 序号；用于强制 frozen 阶段每次修改
    /// 都必须引用一条新的 approval，杜绝同一 approval 反复授权多次修改。
    /// `None` 表示尚未消费任何 approval，是 Draft 与刚被 freeze 后的合法初始状态。
    pub last_approval_sequence: Option<u32>,
}

impl MissionCharter {
    pub fn new(
        mission_id: MissionId,
        title: impl Into<String>,
        goal: impl Into<String>,
        now: UtcMillis,
    ) -> Self {
        Self {
            mission_id,
            title: title.into(),
            goal: goal.into(),
            success_criteria: Vec::new(),
            constraints: Vec::new(),
            stakeholders: Vec::new(),
            created_at: now,
            updated_at: now,
            state: CharterState::Draft,
            last_approval_sequence: None,
        }
    }
}
// --- Errors

#[derive(Debug, Error)]
pub enum MissionCharterError {
    #[error("无法解析 home 目录")]
    HomeDirUnavailable,
    #[error("charter 数据缺失或非法：{reason}")]
    InvalidCharter { reason: String },
    /// frozen 后写入未绑定 approval 或 approval 不可用。
    #[error("charter 已 frozen，禁止直接修改：{reason}")]
    FrozenRejected { reason: String },
    #[error("charter goal 长度非法：trim 后 {actual} 字符，必须 ∈ [{min}, {max}]")]
    GoalLengthOutOfRange {
        actual: usize,
        min: usize,
        max: usize,
    },
    #[error("frozen charter 至少需要 1 条 success_criteria")]
    SuccessCriteriaEmpty,
    #[error("success_criteria[{index}] 长度非法：trim 后 {actual} 字符，必须 ∈ [{min}, {max}]")]
    SuccessCriterionLengthOutOfRange {
        index: usize,
        actual: usize,
        min: usize,
        max: usize,
    },
    #[error("constraints[{index}] 长度非法：trim 后 {actual} 字符，必须 ∈ [{min}, {max}]")]
    ConstraintLengthOutOfRange {
        index: usize,
        actual: usize,
        min: usize,
        max: usize,
    },
    #[error("charter IO 失败 (path={path}): {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// charter 内容字段的长度上下界（trim 后按 Unicode `char` 计数）。
///
/// 设计原则：
/// - 单一校验关口 [`validate_charter`]，所有 mutation 入口先 apply 再 validate；
/// - 上下界写成模块级常量，便于 doc/test 引用，避免硬编码散落多处；
/// - frozen 阶段对 success_criteria 做"非空"硬约束（draft 阶段允许逐步补齐）。
pub const CHARTER_GOAL_MIN_LEN: usize = 10;
pub const CHARTER_GOAL_MAX_LEN: usize = 4096;
pub const CHARTER_ITEM_MIN_LEN: usize = 3;
pub const CHARTER_ITEM_MAX_LEN: usize = 512;

/// 在所有 mutation 入口的最后调用：把 charter 的合法性约束收敛在此一处。
///
/// 调用顺序：[`apply_charter_update`] 完成字段写入与 frozen/approval 自洽检查后，
/// 紧接着调用本函数；任何不通过的写入都不应到达 [`MissionCharterStore::save`]。
///
/// 不变式：
/// - `goal.trim().chars().count() ∈ [GOAL_MIN_LEN, GOAL_MAX_LEN]`；
/// - 当 `state == Frozen` 时，`success_criteria.len() ≥ 1`；
/// - `success_criteria[i].trim().chars().count() ∈ [ITEM_MIN_LEN, ITEM_MAX_LEN]`；
/// - `constraints[i].trim().chars().count() ∈ [ITEM_MIN_LEN, ITEM_MAX_LEN]`。
pub fn validate_charter(charter: &MissionCharter) -> Result<(), MissionCharterError> {
    let goal_len = charter.goal.trim().chars().count();
    if goal_len < CHARTER_GOAL_MIN_LEN || goal_len > CHARTER_GOAL_MAX_LEN {
        return Err(MissionCharterError::GoalLengthOutOfRange {
            actual: goal_len,
            min: CHARTER_GOAL_MIN_LEN,
            max: CHARTER_GOAL_MAX_LEN,
        });
    }
    if charter.state.is_frozen() && charter.success_criteria.is_empty() {
        return Err(MissionCharterError::SuccessCriteriaEmpty);
    }
    for (index, item) in charter.success_criteria.iter().enumerate() {
        let len = item.trim().chars().count();
        if len < CHARTER_ITEM_MIN_LEN || len > CHARTER_ITEM_MAX_LEN {
            return Err(MissionCharterError::SuccessCriterionLengthOutOfRange {
                index,
                actual: len,
                min: CHARTER_ITEM_MIN_LEN,
                max: CHARTER_ITEM_MAX_LEN,
            });
        }
    }
    for (index, item) in charter.constraints.iter().enumerate() {
        let len = item.trim().chars().count();
        if len < CHARTER_ITEM_MIN_LEN || len > CHARTER_ITEM_MAX_LEN {
            return Err(MissionCharterError::ConstraintLengthOutOfRange {
                index,
                actual: len,
                min: CHARTER_ITEM_MIN_LEN,
                max: CHARTER_ITEM_MAX_LEN,
            });
        }
    }
    Ok(())
}
// --- Store

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
        let root = magi_core::paths::missions_root(magi_home, workspace_root);
        fs::create_dir_all(&root).map_err(|source| MissionCharterError::Io {
            path: root.clone(),
            source,
        })?;
        Ok(Self { root })
    }

    fn charter_path(&self, mission_id: &MissionId) -> PathBuf {
        self.root.join(mission_id.as_str()).join("charter.md")
    }

    pub fn load(
        &self,
        mission_id: &MissionId,
    ) -> Result<Option<MissionCharter>, MissionCharterError> {
        let path = self.charter_path(mission_id);
        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(MissionCharterError::Io { path, source }),
        };
        parse_charter(&raw).map(Some)
    }

    /// 写入 charter 前先经 [`validate_charter`] 校验：作为持久化关口，
    /// 任何来源的非法 charter 都不允许落盘——即便上游绕过 [`apply_charter_update`]
    /// 直接构造 [`MissionCharter::new`] 也无法写出违例内容。
    pub fn save(&self, charter: &MissionCharter) -> Result<(), MissionCharterError> {
        validate_charter(charter)?;
        let path = self.charter_path(&charter.mission_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| MissionCharterError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let rendered = render_charter(charter);
        magi_core::fs_atomic::write_atomic(&path, rendered)
            .map_err(|source| MissionCharterError::Io { path, source })
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
        out.push_str(
            "> Mission Charter 是当前 mission 的长期目标与约束；执行时应结合本轮用户输入和当前 task 目标。如本轮明确调整目标或约束，不要用旧 charter 自行覆盖，应按审批/写入流程更新 charter。\n\n",
        );
        out.push_str(&format!("- mission_id: {}\n", charter.mission_id.as_str()));
        out.push_str(&format!("- title: {}\n", charter.title));
        out.push_str(&format!("- state: {}\n", charter.state.as_str()));
        if charter.state.is_frozen() {
            out.push_str(
                "  > charter 已 frozen：任何字段修改必须先经 HumanCheckpoint 审批，再以 approval_sequence 引用。\n",
            );
        }
        out.push('\n');
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
// --- Registry: 按 workspace_root 缓存 store

pub struct MissionCharterRegistry {
    inner: RwLock<HashMap<String, Arc<MissionCharterStore>>>,
    home: PathBuf,
}

impl MissionCharterRegistry {
    /// 不可失败：home 解析失败时回退到 `$TMPDIR/magi-mission-charter`，
    /// 保证 dispatcher 构造不被 IO 状态阻塞。
    pub fn new() -> Self {
        let home =
            dirs_home().unwrap_or_else(|_| std::env::temp_dir().join("magi-mission-charter"));
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
        if let Some(found) = self
            .inner
            .read()
            .expect("registry read lock")
            .get(&key)
            .cloned()
        {
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
// --- Tool argument parsing

/// `mission_charter_write` 工具入参形态。
#[derive(Debug)]
pub struct MissionCharterWriteArgs {
    pub title: Option<String>,
    pub goal: Option<String>,
    pub success_criteria: Option<Vec<String>>,
    pub constraints: Option<Vec<String>>,
    pub stakeholders: Option<Vec<String>>,
    /// `true` 表示在本次写入应用完成后把 charter 状态由 Draft 切到 Frozen。
    /// 已经 Frozen 的 charter 上传 `freeze=true` 视为幂等（保持 frozen）。
    pub freeze: Option<bool>,
    /// frozen 阶段必填：引用一条已 Approved 的 HumanCheckpoint 序号。
    /// 必须严格 > `charter.last_approval_sequence`，保证一次 approval 只授权一次修改。
    pub approval_sequence: Option<u32>,
}

pub fn parse_mission_charter_write_arguments(
    raw: &serde_json::Value,
) -> Result<MissionCharterWriteArgs, MissionCharterError> {
    let obj = raw
        .as_object()
        .ok_or_else(|| MissionCharterError::InvalidCharter {
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
    let freeze = obj.get("freeze").and_then(|v| v.as_bool());
    let approval_sequence = match obj.get("approval_sequence") {
        None => None,
        Some(v) if v.is_null() => None,
        Some(v) => {
            let n = v
                .as_u64()
                .ok_or_else(|| MissionCharterError::InvalidCharter {
                    reason: "approval_sequence 必须为非负整数".to_string(),
                })?;
            if n == 0 || n > u32::MAX as u64 {
                return Err(MissionCharterError::InvalidCharter {
                    reason: format!("approval_sequence 超出范围：{n}"),
                });
            }
            Some(n as u32)
        }
    };
    let has_content_field = title.is_some()
        || goal.is_some()
        || success_criteria.is_some()
        || constraints.is_some()
        || stakeholders.is_some();
    let has_freeze_request = matches!(freeze, Some(true));
    if !has_content_field && !has_freeze_request {
        return Err(MissionCharterError::InvalidCharter {
            reason: "至少需要提供 title/goal/success_criteria/constraints/stakeholders/freeze \
                     中一个字段"
                .to_string(),
        });
    }
    Ok(MissionCharterWriteArgs {
        title,
        goal,
        success_criteria,
        constraints,
        stakeholders,
        freeze,
        approval_sequence,
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
    let arr = value
        .as_array()
        .ok_or_else(|| MissionCharterError::InvalidCharter {
            reason: "字段必须为字符串数组".to_string(),
        })?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item
            .as_str()
            .ok_or_else(|| MissionCharterError::InvalidCharter {
                reason: "数组项必须为字符串".to_string(),
            })?;
        out.push(s.to_string());
    }
    Ok(Some(out))
}

/// 把入参应用到已有 charter 上（增量更新）。返回是否实际产生了变更。
///
/// 不变式：
/// - frozen charter 收到任何**内容**字段（title/goal/success_criteria/constraints/stakeholders）
///   时必须同时携带 `approval_sequence`，且必须严格大于 `charter.last_approval_sequence`。
/// - frozen charter 上的 `freeze=true` 为幂等无操作。
/// - draft charter 上的 `freeze=true` 在内容更新（如有）之后转换为 frozen。
///
/// 注意：`approval_sequence` 是否真的指向一条已 Approved 的 HumanCheckpoint 由调用方
/// （`execute_mission_charter_write_tool`）在调用本函数之前完成核实——本函数只承担
/// charter 自身一致性（序号递增、frozen 强制约束）。
pub fn apply_charter_update(
    charter: &mut MissionCharter,
    args: MissionCharterWriteArgs,
    now: UtcMillis,
) -> Result<bool, MissionCharterError> {
    let want_content_update = args.title.is_some()
        || args.goal.is_some()
        || args.success_criteria.is_some()
        || args.constraints.is_some()
        || args.stakeholders.is_some();

    // 在快照 `next` 上完成所有变更（含 approval_sequence 推进），
    // validate 通过后再 commit 回 charter；任何校验失败都不能在原 charter 上留下"半改"。
    let mut next = charter.clone();

    if next.state.is_frozen() && want_content_update {
        let approval =
            args.approval_sequence
                .ok_or_else(|| MissionCharterError::FrozenRejected {
                    reason: "frozen charter 修改必须提供 approval_sequence 引用已 Approved 的 \
                         HumanCheckpoint"
                        .to_string(),
                })?;
        if let Some(prev) = next.last_approval_sequence {
            if approval <= prev {
                return Err(MissionCharterError::FrozenRejected {
                    reason: format!(
                        "approval_sequence={approval} 必须严格大于已消费的 last_approval_sequence={prev}"
                    ),
                });
            }
        }
        next.last_approval_sequence = Some(approval);
    }

    let mut changed = false;
    if let Some(title) = args.title {
        if next.title != title {
            next.title = title;
            changed = true;
        }
    }
    if let Some(goal) = args.goal {
        if next.goal != goal {
            next.goal = goal;
            changed = true;
        }
    }
    if let Some(success) = args.success_criteria {
        if next.success_criteria != success {
            next.success_criteria = success;
            changed = true;
        }
    }
    if let Some(constraints) = args.constraints {
        if next.constraints != constraints {
            next.constraints = constraints;
            changed = true;
        }
    }
    if let Some(stakeholders) = args.stakeholders {
        if next.stakeholders != stakeholders {
            next.stakeholders = stakeholders;
            changed = true;
        }
    }
    if matches!(args.freeze, Some(true)) && next.state != CharterState::Frozen {
        next.state = CharterState::Frozen;
        changed = true;
    }
    if changed {
        next.updated_at = now;
    }
    validate_charter(&next)?;
    *charter = next;
    Ok(changed)
}
// --- 序列化 / 反序列化（frontmatter + markdown body）

fn render_charter(charter: &MissionCharter) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("mission_id: {}\n", charter.mission_id.as_str()));
    out.push_str(&format!("title: {}\n", charter.title));
    out.push_str(&format!("created_at: {}\n", charter.created_at.0));
    out.push_str(&format!("updated_at: {}\n", charter.updated_at.0));
    out.push_str(&format!("state: {}\n", charter.state.as_str()));
    if let Some(seq) = charter.last_approval_sequence {
        out.push_str(&format!("last_approval_sequence: {seq}\n"));
    }
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
    let (front, body) =
        split_frontmatter(raw).ok_or_else(|| MissionCharterError::InvalidCharter {
            reason: "缺少 frontmatter".to_string(),
        })?;
    let mut mission_id: Option<String> = None;
    let mut title: Option<String> = None;
    let mut created_at: Option<u64> = None;
    let mut updated_at: Option<u64> = None;
    let mut state: Option<CharterState> = None;
    let mut last_approval_sequence: Option<u32> = None;
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
            "state" => {
                state = Some(match value.as_str() {
                    "draft" => CharterState::Draft,
                    "frozen" => CharterState::Frozen,
                    other => {
                        return Err(MissionCharterError::InvalidCharter {
                            reason: format!("frontmatter state 取值非法：{other}"),
                        });
                    }
                });
            }
            "last_approval_sequence" => {
                last_approval_sequence = Some(value.parse::<u32>().map_err(|_| {
                    MissionCharterError::InvalidCharter {
                        reason: format!("last_approval_sequence 解析失败：{value}"),
                    }
                })?);
            }
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
    // state 是 charter 生命周期的根字段，不允许"缺省回退"——缺失即报错，迫使写入方
    // 显式声明 Draft/Frozen，避免静默把陈旧或异常数据当作 Draft 接收。
    let state = state.ok_or_else(|| MissionCharterError::InvalidCharter {
        reason: "frontmatter 缺少 state（必填：draft|frozen）".to_string(),
    })?;
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
        state,
        last_approval_sequence,
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
// --- 工具

fn dirs_home() -> Result<PathBuf, MissionCharterError> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or(MissionCharterError::HomeDirUnavailable)?;
    Ok(PathBuf::from(home).join(".magi"))
}
// --- Tool entry：`mission_charter_write` 工具执行体

/// S11 工具下沉：把 `mission_charter_write` 完整执行体收口在本 crate。
/// `store: None` 表示当前 task 未绑定 workspace，直接失败。首次写入必须同时
/// 提供 title + goal，否则拒绝（避免半成品契约落盘）。
///
/// frozen 阶段规则：
/// - charter 已 frozen 且本次入参带有任意内容字段（title/goal/success_criteria/...）
///   时，必须同时提供 `approval_sequence`；该序号必须指向一条 status==Approved 且
///   严格大于 `charter.last_approval_sequence` 的 HumanCheckpoint。
/// - 仅传 `freeze: true` 的幂等请求在 frozen 状态下视为无操作。
pub fn execute_mission_charter_write_tool(
    event_bus: &magi_event_bus::InMemoryEventBus,
    store: Option<&MissionCharterStore>,
    human_checkpoint_store: Option<&magi_human_checkpoint::HumanCheckpointStore>,
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
                "tool": "mission_charter_write",
                "status": "failed",
                "error": "当前 task 未绑定 workspace，无法定位 mission charter 目录",
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
                    "tool": "mission_charter_write",
                    "status": "failed",
                    "error": format!("arguments 非合法 JSON：{err}"),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let args = match parse_mission_charter_write_arguments(&args_value) {
        Ok(parsed) => parsed,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "mission_charter_write",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let now = UtcMillis::now();
    let mut charter = match store.load(mission_id) {
        Ok(Some(existing)) => existing,
        Ok(None) => {
            let (Some(title), Some(goal)) = (args.title.clone(), args.goal.clone()) else {
                return (
                    serde_json::json!({
                        "tool": "mission_charter_write",
                        "status": "failed",
                        "error": "首次创建 charter 必须同时提供 title 与 goal",
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            };
            MissionCharter::new(mission_id.clone(), title, goal, now)
        }
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "mission_charter_write",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };

    // frozen 阶段且本次涉及内容修改时，必须先确认 approval_sequence 指向一条
    // Approved 的 HumanCheckpoint（apply_charter_update 内部只做序号递增的自洽检查）。
    let want_content_update = args.title.is_some()
        || args.goal.is_some()
        || args.success_criteria.is_some()
        || args.constraints.is_some()
        || args.stakeholders.is_some();
    if charter.state.is_frozen() && want_content_update {
        let Some(approval_sequence) = args.approval_sequence else {
            return (
                serde_json::json!({
                    "tool": "mission_charter_write",
                    "status": "failed",
                    "error": "charter 已 frozen，修改必须提供 approval_sequence 引用 \
                              已 Approved 的 HumanCheckpoint",
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        };
        let Some(hc_store) = human_checkpoint_store else {
            return (
                serde_json::json!({
                    "tool": "mission_charter_write",
                    "status": "failed",
                    "error": "frozen charter 修改需要 HumanCheckpoint store，但运行时未提供",
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        };
        let log = match hc_store.load(mission_id) {
            Ok(Some(log)) => log,
            Ok(None) => {
                return (
                    serde_json::json!({
                        "tool": "mission_charter_write",
                        "status": "failed",
                        "error": format!(
                            "approval_sequence={approval_sequence} 不存在：mission 没有 \
                             HumanCheckpoint 记录"
                        ),
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            Err(err) => {
                return (
                    serde_json::json!({
                        "tool": "mission_charter_write",
                        "status": "failed",
                        "error": format!("加载 HumanCheckpoint 失败：{err}"),
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
        };
        let Some(entry) = log.entries.iter().find(|e| e.sequence == approval_sequence) else {
            return (
                serde_json::json!({
                    "tool": "mission_charter_write",
                    "status": "failed",
                    "error": format!("approval_sequence={approval_sequence} 不存在"),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        };
        if !matches!(
            entry.status,
            magi_human_checkpoint::HumanCheckpointStatus::Approved
        ) {
            return (
                serde_json::json!({
                    "tool": "mission_charter_write",
                    "status": "failed",
                    "error": format!(
                        "approval_sequence={approval_sequence} 当前状态为 {}，必须为 approved",
                        entry.status.as_str()
                    ),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    }

    let changed = match apply_charter_update(&mut charter, args, now) {
        Ok(changed) => changed,
        Err(err @ MissionCharterError::FrozenRejected { .. }) => {
            return (
                serde_json::json!({
                    "tool": "mission_charter_write",
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
                    "tool": "mission_charter_write",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    if let Err(err) = store.save(&charter) {
        return (
            serde_json::json!({
                "tool": "mission_charter_write",
                "status": "failed",
                "error": err.to_string(),
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    }
    let payload = serde_json::json!({
        "tool": "mission_charter_write",
        "status": "succeeded",
        "mission_id": charter.mission_id.to_string(),
        "title": charter.title,
        "state": charter.state.as_str(),
        "last_approval_sequence": charter.last_approval_sequence,
        "changed": changed,
    });
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!(
                "event-mission-charter-updated-{}",
                UtcMillis::now().0
            )),
            "task.mission_charter.updated",
            serde_json::json!({
                "task_id": task_id.to_string(),
                "mission_id": charter.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.map(ToString::to_string),
                "changed": changed,
                "title": charter.title,
                "state": charter.state.as_str(),
                "last_approval_sequence": charter.last_approval_sequence,
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
// --- tests

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
    fn save_then_load_round_trips_all_fields() {
        let (home, ws) = tmp_workspace();
        let store = MissionCharterStore::open_with_home(home.path(), &ws).expect("open");
        let charter = MissionCharter {
            mission_id: MissionId::new("mission-1"),
            title: "迁移 任务系统".to_string(),
            goal: "收敛 任务系统 4-Tier 21-Layer 完成态".to_string(),
            success_criteria: vec!["S1-S18 全部完成".to_string(), "cargo test 通过".to_string()],
            constraints: vec!["不保留旧任务系统兼容路径".to_string()],
            stakeholders: vec!["用户（架构师）".to_string()],
            created_at: UtcMillis(1_700_000_000_000),
            updated_at: UtcMillis(1_700_000_000_000),
            state: CharterState::Draft,
            last_approval_sequence: None,
        };
        store.save(&charter).expect("save");
        let loaded = store
            .load(&charter.mission_id)
            .expect("load")
            .expect("present");
        assert_eq!(loaded, charter);
    }

    #[test]
    fn load_returns_none_when_missing() {
        let (home, ws) = tmp_workspace();
        let store = MissionCharterStore::open_with_home(home.path(), &ws).expect("open");
        assert!(
            store
                .load(&MissionId::new("missing"))
                .expect("load")
                .is_none()
        );
    }

    #[test]
    fn render_for_prompt_emits_sections_only_when_populated() {
        let (home, ws) = tmp_workspace();
        let store = MissionCharterStore::open_with_home(home.path(), &ws).expect("open");
        let charter = MissionCharter {
            mission_id: MissionId::new("m"),
            title: "T".to_string(),
            goal: "render charter draft prompt section".to_string(),
            success_criteria: Vec::new(),
            constraints: vec!["constraint-one".to_string()],
            stakeholders: Vec::new(),
            created_at: UtcMillis(0),
            updated_at: UtcMillis(0),
            state: CharterState::Draft,
            last_approval_sequence: None,
        };
        store.save(&charter).expect("save");
        let rendered = store
            .render_for_prompt(&charter.mission_id)
            .expect("render")
            .expect("present");
        assert!(rendered.contains("# Mission Charter"));
        assert!(rendered.contains("长期目标与约束"));
        assert!(rendered.contains("不要用旧 charter 自行覆盖"));
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
        assert_eq!(
            args.success_criteria.as_deref().unwrap(),
            &["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn apply_charter_update_bumps_timestamp_when_changed() {
        let mut charter = MissionCharter::new(
            MissionId::new("m"),
            "old".to_string(),
            "实施 任务系统 §1.4 验收闭环".to_string(),
            UtcMillis(100),
        );
        let args = MissionCharterWriteArgs {
            title: Some("new".to_string()),
            goal: None,
            success_criteria: None,
            constraints: None,
            stakeholders: None,
            freeze: None,
            approval_sequence: None,
        };
        let changed =
            apply_charter_update(&mut charter, args, UtcMillis(200)).expect("draft apply 不应失败");
        assert!(changed);
        assert_eq!(charter.title, "new");
        assert_eq!(charter.updated_at.0, 200);
        assert_eq!(charter.state, CharterState::Draft);
    }

    #[test]
    fn apply_charter_update_no_op_keeps_timestamp() {
        let mut charter = MissionCharter::new(
            MissionId::new("m"),
            "same".to_string(),
            "实施 任务系统 §1.4 验收闭环".to_string(),
            UtcMillis(100),
        );
        let args = MissionCharterWriteArgs {
            title: Some("same".to_string()),
            goal: None,
            success_criteria: None,
            constraints: None,
            stakeholders: None,
            freeze: None,
            approval_sequence: None,
        };
        let changed =
            apply_charter_update(&mut charter, args, UtcMillis(200)).expect("no-op apply 不应失败");
        assert!(!changed);
        assert_eq!(charter.updated_at.0, 100);
    }

    // ---- §1.6 生命周期不变式 --------------------------------------------

    #[test]
    fn apply_charter_update_freeze_transitions_draft_to_frozen() {
        let mut charter = MissionCharter::new(
            MissionId::new("m"),
            "t".to_string(),
            "实施 任务系统 §1.4 验收闭环".to_string(),
            UtcMillis(0),
        );
        let args = MissionCharterWriteArgs {
            title: None,
            goal: None,
            success_criteria: Some(vec!["criterion".to_string()]),
            constraints: None,
            stakeholders: None,
            freeze: Some(true),
            approval_sequence: None,
        };
        let changed = apply_charter_update(&mut charter, args, UtcMillis(10)).expect("freeze ok");
        assert!(changed);
        assert_eq!(charter.state, CharterState::Frozen);
        assert!(charter.last_approval_sequence.is_none());
    }

    #[test]
    fn apply_charter_update_frozen_content_rejects_without_approval() {
        let mut charter = MissionCharter::new(
            MissionId::new("m"),
            "t".to_string(),
            "实施 任务系统 §1.4 验收闭环".to_string(),
            UtcMillis(0),
        );
        charter.state = CharterState::Frozen;
        charter.success_criteria = vec!["criterion".to_string()];
        let args = MissionCharterWriteArgs {
            title: Some("new".to_string()),
            goal: None,
            success_criteria: None,
            constraints: None,
            stakeholders: None,
            freeze: None,
            approval_sequence: None,
        };
        let err = apply_charter_update(&mut charter, args, UtcMillis(10))
            .expect_err("frozen 状态修改应被拒");
        assert!(matches!(err, MissionCharterError::FrozenRejected { .. }));
        assert_eq!(charter.title, "t", "拒绝后原值保留");
    }

    #[test]
    fn apply_charter_update_frozen_requires_strictly_increasing_approval() {
        let mut charter = MissionCharter::new(
            MissionId::new("m"),
            "t".to_string(),
            "实施 任务系统 §1.4 验收闭环".to_string(),
            UtcMillis(0),
        );
        charter.state = CharterState::Frozen;
        charter.success_criteria = vec!["criterion".to_string()];
        charter.last_approval_sequence = Some(5);
        // 同序号必须被拒绝
        let same = apply_charter_update(
            &mut charter,
            MissionCharterWriteArgs {
                title: Some("new".to_string()),
                goal: None,
                success_criteria: None,
                constraints: None,
                stakeholders: None,
                freeze: None,
                approval_sequence: Some(5),
            },
            UtcMillis(10),
        )
        .expect_err("approval 不递增必须被拒");
        assert!(matches!(same, MissionCharterError::FrozenRejected { .. }));
        assert_eq!(
            charter.last_approval_sequence,
            Some(5),
            "被拒绝后 approval 不应推进"
        );
        // 严格递增可通过
        let changed = apply_charter_update(
            &mut charter,
            MissionCharterWriteArgs {
                title: Some("new".to_string()),
                goal: None,
                success_criteria: None,
                constraints: None,
                stakeholders: None,
                freeze: None,
                approval_sequence: Some(6),
            },
            UtcMillis(20),
        )
        .expect("递增 approval 应放行");
        assert!(changed);
        assert_eq!(charter.last_approval_sequence, Some(6));
        assert_eq!(charter.title, "new");
        assert_eq!(charter.state, CharterState::Frozen, "修改后仍保持 frozen");
    }

    #[test]
    fn apply_charter_update_frozen_idempotent_freeze_is_noop() {
        let mut charter = MissionCharter::new(
            MissionId::new("m"),
            "t".to_string(),
            "实施 任务系统 §1.4 验收闭环".to_string(),
            UtcMillis(0),
        );
        charter.state = CharterState::Frozen;
        charter.success_criteria = vec!["criterion".to_string()];
        // 仅 freeze=true 且无内容变更：视为幂等 no-op，不需要 approval。
        let changed = apply_charter_update(
            &mut charter,
            MissionCharterWriteArgs {
                title: None,
                goal: None,
                success_criteria: None,
                constraints: None,
                stakeholders: None,
                freeze: Some(true),
                approval_sequence: None,
            },
            UtcMillis(10),
        )
        .expect("frozen 上幂等 freeze 不应报错");
        assert!(!changed);
        assert_eq!(charter.updated_at.0, 0);
    }

    #[test]
    fn parse_mission_charter_write_accepts_freeze_only() {
        let args = parse_mission_charter_write_arguments(&serde_json::json!({
            "freeze": true,
        }))
        .expect("仅 freeze=true 应当合法");
        assert_eq!(args.freeze, Some(true));
        assert!(args.title.is_none());
    }

    #[test]
    fn parse_mission_charter_write_rejects_invalid_approval_sequence() {
        let err = parse_mission_charter_write_arguments(&serde_json::json!({
            "title": "x",
            "approval_sequence": 0,
        }))
        .expect_err("approval_sequence=0 必须报错");
        assert!(matches!(err, MissionCharterError::InvalidCharter { .. }));
    }

    #[test]
    fn save_then_load_round_trips_lifecycle_fields() {
        let (home, ws) = tmp_workspace();
        let store = MissionCharterStore::open_with_home(home.path(), &ws).expect("open");
        let charter = MissionCharter {
            mission_id: MissionId::new("mf"),
            title: "frozen-title".to_string(),
            goal: "deliver the frozen lifecycle round-trip".to_string(),
            success_criteria: vec!["criterion".to_string()],
            constraints: Vec::new(),
            stakeholders: Vec::new(),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(2),
            state: CharterState::Frozen,
            last_approval_sequence: Some(7),
        };
        store.save(&charter).expect("save");
        let loaded = store
            .load(&charter.mission_id)
            .expect("load")
            .expect("present");
        assert_eq!(loaded.state, CharterState::Frozen);
        assert_eq!(loaded.last_approval_sequence, Some(7));
    }

    #[test]
    fn parse_charter_rejects_missing_state() {
        let (home, ws) = tmp_workspace();
        let store = MissionCharterStore::open_with_home(home.path(), &ws).expect("open");
        let mission_id = MissionId::new("legacy");
        let path = store.charter_path(&mission_id);
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
        // frontmatter 不带 state：必须直接报 InvalidCharter，禁止"静默回退 Draft"。
        let raw = "---\nmission_id: legacy\ntitle: t\ncreated_at: 1\nupdated_at: 1\n---\n\n## Goal\ng\n\n";
        std::fs::write(&path, raw).expect("write");
        let err = store.load(&mission_id).expect_err("缺 state 必须报错");
        assert!(
            matches!(err, MissionCharterError::InvalidCharter { ref reason } if reason.contains("state")),
            "期望 InvalidCharter 且 reason 提及 state，实际：{err}"
        );
    }

    #[test]
    fn render_for_prompt_marks_frozen_state() {
        let (home, ws) = tmp_workspace();
        let store = MissionCharterStore::open_with_home(home.path(), &ws).expect("open");
        let charter = MissionCharter {
            mission_id: MissionId::new("m"),
            title: "T".to_string(),
            goal: "freeze the charter prompt for rendering".to_string(),
            success_criteria: vec!["criterion".to_string()],
            constraints: Vec::new(),
            stakeholders: Vec::new(),
            created_at: UtcMillis(0),
            updated_at: UtcMillis(0),
            state: CharterState::Frozen,
            last_approval_sequence: Some(3),
        };
        store.save(&charter).expect("save");
        let rendered = store
            .render_for_prompt(&charter.mission_id)
            .expect("render")
            .expect("present");
        assert!(rendered.contains("state: frozen"));
        assert!(rendered.contains("HumanCheckpoint 审批"));
    }

    // ---- execute_mission_charter_write_tool 端到端 ----------------------

    fn make_hc_log(
        hc_store: &magi_human_checkpoint::HumanCheckpointStore,
        mission: &MissionId,
        approved_seq: u32,
    ) {
        let mut log = magi_human_checkpoint::HumanCheckpointLog::new(mission.clone(), UtcMillis(0));
        for i in 1..=approved_seq {
            magi_human_checkpoint::append_human_checkpoint_request(
                &mut log,
                magi_human_checkpoint::HumanCheckpointRequestArgs {
                    plan_step_id: format!("s{i}"),
                    prompt_to_human: format!("review {i}"),
                    label: Some(format!("lbl-{i}")),
                    context: None,
                },
                UtcMillis(i as u64),
            );
        }
        hc_store.save(&log).expect("save log");
        let bus = magi_event_bus::InMemoryEventBus::new(16);
        hc_store
            .resolve_request(
                &bus,
                mission,
                approved_seq,
                magi_human_checkpoint::HumanCheckpointDecision::Approve,
                "ops".to_string(),
                None,
                UtcMillis(1_000 + approved_seq as u64),
            )
            .expect("approve");
    }

    #[test]
    fn execute_tool_end_to_end_lifecycle() {
        use magi_core::{ExecutionResultStatus, SessionId, TaskId, WorkspaceId};
        let home = tempfile::tempdir().expect("tempdir");
        let ws_root = WorkspaceRootPath::new(home.path().to_string_lossy().to_string());
        let charter_store =
            MissionCharterStore::open_with_home(home.path(), &ws_root).expect("open");
        let hc_store =
            magi_human_checkpoint::HumanCheckpointStore::open_with_home(home.path(), &ws_root)
                .expect("open hc");
        let bus = magi_event_bus::InMemoryEventBus::new(64);
        let session_id = SessionId::new("session-1");
        let task_id = TaskId::new("task-1");
        let mission_id = MissionId::new("mission-end2end");
        let workspace_id = WorkspaceId::new("ws-1");

        // 1) draft 阶段创建 + 增量更新（无 approval）。
        let (out, status) = execute_mission_charter_write_tool(
            &bus,
            Some(&charter_store),
            Some(&hc_store),
            &session_id,
            Some(&workspace_id),
            &task_id,
            &mission_id,
            r#"{"title":"T","goal":"实施 任务系统 §1.4 验收闭环"}"#,
        );
        assert_eq!(
            status,
            ExecutionResultStatus::Succeeded,
            "draft 创建应通过：{out}"
        );
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["state"], "draft");

        // 2) draft 阶段 freeze=true 转为 frozen（必须同时提供 success_criteria，
        //    满足 frozen 不变式 success_criteria.len() ≥ 1）。
        let (out, status) = execute_mission_charter_write_tool(
            &bus,
            Some(&charter_store),
            Some(&hc_store),
            &session_id,
            Some(&workspace_id),
            &task_id,
            &mission_id,
            r#"{"freeze":true,"success_criteria":["端到端测试通过"]}"#,
        );
        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["state"], "frozen");

        // 3) frozen 后直接修改（无 approval_sequence）必须失败。
        let (out, status) = execute_mission_charter_write_tool(
            &bus,
            Some(&charter_store),
            Some(&hc_store),
            &session_id,
            Some(&workspace_id),
            &task_id,
            &mission_id,
            r#"{"title":"NEW"}"#,
        );
        assert_eq!(status, ExecutionResultStatus::Failed);
        assert!(out.contains("frozen"), "拒绝原因应说明 frozen：{out}");

        // 4) 引用未 approved 的序号必须失败。
        // 先 append 一条 pending（不 resolve）。
        let mut log =
            magi_human_checkpoint::HumanCheckpointLog::new(mission_id.clone(), UtcMillis(0));
        magi_human_checkpoint::append_human_checkpoint_request(
            &mut log,
            magi_human_checkpoint::HumanCheckpointRequestArgs {
                plan_step_id: "s1".to_string(),
                prompt_to_human: "review".to_string(),
                label: Some("lbl".to_string()),
                context: None,
            },
            UtcMillis(1),
        );
        hc_store.save(&log).expect("save log");
        let (out, status) = execute_mission_charter_write_tool(
            &bus,
            Some(&charter_store),
            Some(&hc_store),
            &session_id,
            Some(&workspace_id),
            &task_id,
            &mission_id,
            r#"{"title":"NEW","approval_sequence":1}"#,
        );
        assert_eq!(status, ExecutionResultStatus::Failed);
        assert!(
            out.contains("approved"),
            "拒绝原因应提示状态非 approved：{out}"
        );

        // 5) 把 #1 resolve approved，再写入应通过。
        hc_store
            .resolve_request(
                &bus,
                &mission_id,
                1,
                magi_human_checkpoint::HumanCheckpointDecision::Approve,
                "ops".to_string(),
                None,
                UtcMillis(500),
            )
            .expect("approve");
        let (out, status) = execute_mission_charter_write_tool(
            &bus,
            Some(&charter_store),
            Some(&hc_store),
            &session_id,
            Some(&workspace_id),
            &task_id,
            &mission_id,
            r#"{"title":"NEW","approval_sequence":1}"#,
        );
        assert_eq!(
            status,
            ExecutionResultStatus::Succeeded,
            "已批准 approval 应放行：{out}"
        );
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["state"], "frozen");
        assert_eq!(parsed["last_approval_sequence"], 1);

        // 6) 重复使用同一个 approval_sequence 必须被拒。
        let (out, status) = execute_mission_charter_write_tool(
            &bus,
            Some(&charter_store),
            Some(&hc_store),
            &session_id,
            Some(&workspace_id),
            &task_id,
            &mission_id,
            r#"{"title":"NEWER","approval_sequence":1}"#,
        );
        assert_eq!(status, ExecutionResultStatus::Failed);
        assert!(
            out.contains("last_approval_sequence") || out.contains("严格大于"),
            "拒绝原因应说明序号不递增：{out}"
        );
        // 防止 unused：保留 make_hc_log 作为便捷构造器（在更复杂的扩展测试时使用）。
        let _ = make_hc_log;
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

    // ---- validate_charter 关口 -----------------------------------------

    fn valid_draft(mission_id: &str) -> MissionCharter {
        MissionCharter::new(
            MissionId::new(mission_id),
            "title".to_string(),
            "实施 任务系统 §1.4 验收闭环".to_string(),
            UtcMillis(0),
        )
    }

    #[test]
    fn validate_charter_rejects_too_short_goal() {
        let mut charter = valid_draft("v1");
        charter.goal = "短目标".to_string();
        let err = validate_charter(&charter).expect_err("过短 goal 必须被拒");
        assert!(matches!(
            err,
            MissionCharterError::GoalLengthOutOfRange {
                actual: 3,
                min: 10,
                max: 4096
            }
        ));
    }

    #[test]
    fn validate_charter_rejects_too_long_goal() {
        let mut charter = valid_draft("v2");
        charter.goal = "a".repeat(CHARTER_GOAL_MAX_LEN + 1);
        let err = validate_charter(&charter).expect_err("超长 goal 必须被拒");
        assert!(matches!(
            err,
            MissionCharterError::GoalLengthOutOfRange { actual, .. }
                if actual == CHARTER_GOAL_MAX_LEN + 1
        ));
    }

    #[test]
    fn validate_charter_rejects_frozen_without_success_criteria() {
        let mut charter = valid_draft("v3");
        charter.state = CharterState::Frozen;
        // success_criteria 留空：frozen 状态下必须 ≥1，否则拒绝。
        let err = validate_charter(&charter).expect_err("frozen 缺 criteria 必须被拒");
        assert!(matches!(err, MissionCharterError::SuccessCriteriaEmpty));
    }

    #[test]
    fn validate_charter_rejects_too_short_success_criterion() {
        let mut charter = valid_draft("v4");
        charter.success_criteria = vec!["ok".to_string()];
        let err = validate_charter(&charter).expect_err("过短 criterion 必须被拒");
        assert!(matches!(
            err,
            MissionCharterError::SuccessCriterionLengthOutOfRange {
                index: 0,
                actual: 2,
                min: 3,
                max: 512,
            }
        ));
    }

    #[test]
    fn validate_charter_rejects_too_short_constraint() {
        let mut charter = valid_draft("v5");
        charter.constraints = vec!["合法约束".to_string(), "x".to_string()];
        let err = validate_charter(&charter).expect_err("过短 constraint 必须被拒");
        assert!(matches!(
            err,
            MissionCharterError::ConstraintLengthOutOfRange {
                index: 1,
                actual: 1,
                ..
            }
        ));
    }

    #[test]
    fn validate_charter_allows_unicode_goal_at_min_boundary() {
        let mut charter = valid_draft("v6");
        // 10 个汉字 = 10 个 char，恰好命中下界。
        charter.goal = "一二三四五六七八九十".to_string();
        validate_charter(&charter).expect("边界值合法");
    }

    #[test]
    fn apply_charter_update_rolls_back_state_on_validation_failure() {
        let mut charter = valid_draft("rb");
        charter.state = CharterState::Frozen;
        charter.success_criteria = vec!["criterion".to_string()];
        charter.last_approval_sequence = Some(2);
        // 通过 approval 检查，但 success_criteria 写入会让某条非法：
        let err = apply_charter_update(
            &mut charter,
            MissionCharterWriteArgs {
                title: None,
                goal: None,
                success_criteria: Some(vec!["ok".to_string()]),
                constraints: None,
                stakeholders: None,
                freeze: None,
                approval_sequence: Some(3),
            },
            UtcMillis(1),
        )
        .expect_err("过短 criterion 必须被拒");
        assert!(matches!(
            err,
            MissionCharterError::SuccessCriterionLengthOutOfRange { .. }
        ));
        assert_eq!(
            charter.last_approval_sequence,
            Some(2),
            "校验失败时 approval 不能在原 charter 上推进"
        );
        assert_eq!(
            charter.success_criteria,
            vec!["criterion".to_string()],
            "校验失败时 success_criteria 不能在原 charter 上落入新值"
        );
    }
}
