//! 任务系统 — Tier 4 / L16 Plan：mission 的"执行计划"工件。
//!
//! Charter（L15）回答"为什么做、做到什么程度算完"；
//! Plan（L16）回答"怎么走，每一步是什么状态"；
//! TodoLedger（L13）回答"当前 session 内还要做哪些手头杂事"。
//!
//! 三者层级不同，互不重叠：
//! - Charter 是宪章，跨 session 长存，几乎不变；
//! - Plan 是计划，按澄清/复盘节奏变更，跨 session 共享；
//! - TodoLedger 是临时清单，session 内可任意改写。
//!
//! 物理存储：`~/.magi/projects/{slug}/missions/{mission_id}/plan.md`，
//! 与 Charter 在同一目录，共享同一 workspace slug 派生策略。
//!
//! 落盘格式（frontmatter + body）：
//! ```yaml
//! ---
//! mission_id: mission-xxxx
//! created_at: 2026-05-15T00:00:00Z
//! updated_at: 2026-05-15T00:00:00Z
//! ---
//! ## Steps
//! - [ ] step-1 (pending) — 拆 v1
//!   notes: 先盘 dispatch
//! - [x] step-2 (completed) — 切新链路
//!   depends_on: step-1
//! ```
//!
//! 单 mission 单 plan 文档，`plan_write` 工具整体替换 steps 列表（按用户给的最新版本快照写入）。
//! 不做"局部 patch"——因为模型每次都能传完整列表，patch 反而增加歧义和漂移。

use magi_core::{MissionId, UtcMillis, WorkspaceRootPath};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use thiserror::Error;
// --- Plan / PlanStep

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl PlanStepStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_str_lenient(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "pending" | "todo" => Some(Self::Pending),
            "in_progress" | "doing" | "active" => Some(Self::InProgress),
            "completed" | "done" => Some(Self::Completed),
            "cancelled" | "canceled" | "abandoned" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: String,
    pub content: String,
    pub status: PlanStepStatus,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub mission_id: MissionId,
    pub steps: Vec<PlanStep>,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

impl Plan {
    pub fn new(mission_id: MissionId, now: UtcMillis) -> Self {
        Self {
            mission_id,
            steps: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }
}
// --- Errors

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("无法解析 home 目录")]
    HomeDirUnavailable,
    #[error("plan 数据缺失或非法：{reason}")]
    InvalidPlan { reason: String },
    #[error("plan IO 失败 (path={path}): {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// 不变式 7：step 标记为 completed 但 ValidationReport 没有对应 step 的 pass 记录。
    /// 让模型必须先 `validation_record` 登记证据，再回头改 plan，避免“嘴说做完”。
    #[error(
        "step {step_id} 标记为 completed 但 validation report 没有 pass 记录；\
        请先用 validation_record 工具登记验证证据后再标记完成"
    )]
    ValidationEvidenceMissing { step_id: String },
}
// --- Store

pub struct PlanStore {
    root: PathBuf,
}

impl PlanStore {
    pub fn open(workspace_root: &WorkspaceRootPath) -> Result<Self, PlanError> {
        let home = dirs_home()?;
        Self::open_with_home(&home, workspace_root)
    }

    pub fn open_with_home(
        magi_home: &Path,
        workspace_root: &WorkspaceRootPath,
    ) -> Result<Self, PlanError> {
        let root = magi_core::paths::missions_root(magi_home, workspace_root);
        fs::create_dir_all(&root).map_err(|source| PlanError::Io {
            path: root.clone(),
            source,
        })?;
        Ok(Self { root })
    }

    fn plan_path(&self, mission_id: &MissionId) -> PathBuf {
        self.root.join(mission_id.as_str()).join("plan.md")
    }

    pub fn load(&self, mission_id: &MissionId) -> Result<Option<Plan>, PlanError> {
        let path = self.plan_path(mission_id);
        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(PlanError::Io { path, source }),
        };
        parse_plan(&raw).map(Some)
    }

    pub fn save(&self, plan: &Plan) -> Result<(), PlanError> {
        let path = self.plan_path(&plan.mission_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| PlanError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let rendered = render_plan(plan);
        magi_core::fs_atomic::write_atomic(&path, rendered)
            .map_err(|source| PlanError::Io { path, source })
    }

    pub fn render_for_prompt(&self, mission_id: &MissionId) -> Result<Option<String>, PlanError> {
        let Some(plan) = self.load(mission_id)? else {
            return Ok(None);
        };
        if plan.steps.is_empty() {
            return Ok(None);
        }
        let mut out = String::new();
        out.push_str("# Mission Plan\n\n");
        out.push_str(
            "> Mission Plan 是当前 mission 的执行状态参考；不得覆盖本轮用户输入、当前会话事实或当前 task 目标。如二者冲突，应以本轮任务事实为准并通过 plan_write 更新计划。\n\n",
        );
        out.push_str(&format!("- mission_id: {}\n", plan.mission_id.as_str()));
        out.push_str(&format!("- total_steps: {}\n\n", plan.steps.len()));
        out.push_str("## Steps\n");
        for step in &plan.steps {
            let marker = match step.status {
                PlanStepStatus::Completed => "[x]",
                PlanStepStatus::InProgress => "[~]",
                PlanStepStatus::Cancelled => "[-]",
                PlanStepStatus::Pending => "[ ]",
            };
            out.push_str(&format!(
                "- {} {} ({}) — {}\n",
                marker,
                step.id,
                step.status.as_str(),
                step.content
            ));
            if !step.depends_on.is_empty() {
                out.push_str(&format!("  depends_on: {}\n", step.depends_on.join(", ")));
            }
            if let Some(notes) = &step.notes {
                if !notes.is_empty() {
                    out.push_str(&format!("  notes: {notes}\n"));
                }
            }
        }
        Ok(Some(out))
    }
}
// --- Registry

/// 进程级缓存，按 workspace_root 聚合 PlanStore。失败时回退到 `$TMPDIR/magi-plan`，
/// 避免 home 目录不可写导致整路径不可用。
pub struct PlanRegistry {
    inner: RwLock<HashMap<String, Arc<PlanStore>>>,
    fallback_home: PathBuf,
}

impl Default for PlanRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PlanRegistry {
    pub fn new() -> Self {
        let fallback_home = std::env::temp_dir().join("magi-plan");
        let _ = fs::create_dir_all(&fallback_home);
        Self {
            inner: RwLock::new(HashMap::new()),
            fallback_home,
        }
    }

    pub fn get_or_open(
        &self,
        workspace_root: &WorkspaceRootPath,
    ) -> Result<Arc<PlanStore>, PlanError> {
        let key = workspace_root.as_str().to_string();
        if let Some(store) = self.inner.read().expect("plan registry poisoned").get(&key) {
            return Ok(store.clone());
        }
        let store = match PlanStore::open(workspace_root) {
            Ok(store) => store,
            Err(PlanError::HomeDirUnavailable) => {
                PlanStore::open_with_home(&self.fallback_home, workspace_root)?
            }
            Err(err) => return Err(err),
        };
        let arc = Arc::new(store);
        self.inner
            .write()
            .expect("plan registry poisoned")
            .insert(key, arc.clone());
        Ok(arc)
    }
}
// --- Tool argument parsing

#[derive(Debug)]
pub struct PlanWriteArgs {
    pub steps: Vec<PlanStepInput>,
}

#[derive(Debug)]
pub struct PlanStepInput {
    pub id: String,
    pub content: String,
    pub status: PlanStepStatus,
    pub depends_on: Vec<String>,
    pub notes: Option<String>,
}

pub fn parse_plan_write_arguments(raw: &serde_json::Value) -> Result<PlanWriteArgs, PlanError> {
    let obj = raw.as_object().ok_or_else(|| PlanError::InvalidPlan {
        reason: "arguments 必须为对象".to_string(),
    })?;
    let steps_value = obj.get("steps").ok_or_else(|| PlanError::InvalidPlan {
        reason: "缺少 steps 字段".to_string(),
    })?;
    let arr = steps_value
        .as_array()
        .ok_or_else(|| PlanError::InvalidPlan {
            reason: "steps 必须为数组".to_string(),
        })?;
    if arr.is_empty() {
        return Err(PlanError::InvalidPlan {
            reason: "steps 至少需要一项；plan_write 总是整表替换当前计划，不支持空计划。若某步骤已废弃，请保留它并将 status 置为 cancelled，而不是清空 steps。"
                .to_string(),
        });
    }
    let mut steps = Vec::with_capacity(arr.len());
    for (idx, raw_step) in arr.iter().enumerate() {
        let step_obj = raw_step.as_object().ok_or_else(|| PlanError::InvalidPlan {
            reason: format!("steps[{idx}] 必须为对象"),
        })?;
        let id = step_obj
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| PlanError::InvalidPlan {
                reason: format!("steps[{idx}].id 缺失"),
            })?
            .trim()
            .to_string();
        if id.is_empty() {
            return Err(PlanError::InvalidPlan {
                reason: format!("steps[{idx}].id 不能为空字符串"),
            });
        }
        let content = step_obj
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| PlanError::InvalidPlan {
                reason: format!("steps[{idx}].content 缺失"),
            })?
            .trim()
            .to_string();
        if content.is_empty() {
            return Err(PlanError::InvalidPlan {
                reason: format!("steps[{idx}].content 不能为空"),
            });
        }
        let status = match step_obj.get("status").and_then(|v| v.as_str()) {
            Some(s) => {
                PlanStepStatus::from_str_lenient(s).ok_or_else(|| PlanError::InvalidPlan {
                    reason: format!("steps[{idx}].status 非法：{s}"),
                })?
            }
            None => PlanStepStatus::Pending,
        };
        let depends_on = match step_obj.get("depends_on") {
            Some(serde_json::Value::Null) | None => Vec::new(),
            Some(serde_json::Value::Array(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for (j, item) in items.iter().enumerate() {
                    let dep = item.as_str().ok_or_else(|| PlanError::InvalidPlan {
                        reason: format!("steps[{idx}].depends_on[{j}] 必须为字符串"),
                    })?;
                    out.push(dep.to_string());
                }
                out
            }
            Some(_) => {
                return Err(PlanError::InvalidPlan {
                    reason: format!("steps[{idx}].depends_on 必须为字符串数组"),
                });
            }
        };
        let notes = step_obj
            .get("notes")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        steps.push(PlanStepInput {
            id,
            content,
            status,
            depends_on,
            notes,
        });
    }

    // 校验 depends_on 全部指向 plan 内部已知 id（依赖图必须封闭）。
    let known_ids: std::collections::HashSet<&str> = steps.iter().map(|s| s.id.as_str()).collect();
    for step in &steps {
        for dep in &step.depends_on {
            if !known_ids.contains(dep.as_str()) {
                return Err(PlanError::InvalidPlan {
                    reason: format!(
                        "step {} 的依赖 {} 不在 plan 内，依赖必须指向已声明的 step.id",
                        step.id, dep
                    ),
                });
            }
            if dep == &step.id {
                return Err(PlanError::InvalidPlan {
                    reason: format!("step {} 不能依赖自身", step.id),
                });
            }
        }
    }

    Ok(PlanWriteArgs { steps })
}

/// `apply_plan_update` 的返回值，把"是否变化"与"新近完成的 step 列表"一并返出。
/// 后者用于 `execute_plan_write_tool` 发布 `mission.plan_step.completed` 事件。
#[derive(Debug, Default)]
pub struct PlanUpdateOutcome {
    pub changed: bool,
    pub newly_completed_steps: Vec<PlanStep>,
}

/// 用 `plan_write` 入参整体替换 plan.steps。返回 `PlanUpdateOutcome`。
///
/// 不变式 7（"Plan completion 需要证据"）的运行时关口在此实现：
/// - 当某个 step 在新版本里被首次标记为 `Completed`（旧版本不是 Completed）时，
///   必须存在 `ValidationReport.step_is_passing(step.id) == true`，否则整笔
///   plan 写入被拒绝，并返回 `PlanError::ValidationEvidenceMissing`。
/// - 旧版本已经是 Completed 的步骤即使 `validation_report` 缺失也允许沿用，
///   避免一次写入失败导致后续无法编辑同 plan 的其他字段。
/// - `validation_report = None` 表示当前 mission 还没有任何验证记录，
///   等同于"所有 step 都未通过验证"——所以本轮 plan_write 不能新增 Completed。
pub fn apply_plan_update(
    plan: &mut Plan,
    args: PlanWriteArgs,
    now: UtcMillis,
    validation_report: Option<&magi_validation_runner::ValidationReport>,
) -> Result<PlanUpdateOutcome, PlanError> {
    // 旧版本快照：用于判断"newly Completed"。
    let prior_status: HashMap<String, PlanStepStatus> = plan
        .steps
        .iter()
        .map(|s| (s.id.clone(), s.status))
        .collect();

    let mut newly_completed_ids: Vec<String> = Vec::new();
    for step in &args.steps {
        if step.status != PlanStepStatus::Completed {
            continue;
        }
        let was_completed = matches!(prior_status.get(&step.id), Some(PlanStepStatus::Completed));
        if was_completed {
            continue;
        }
        let passing = validation_report
            .map(|report| report.step_is_passing(&step.id))
            .unwrap_or(false);
        if !passing {
            return Err(PlanError::ValidationEvidenceMissing {
                step_id: step.id.clone(),
            });
        }
        newly_completed_ids.push(step.id.clone());
    }

    let new_steps: Vec<PlanStep> = args
        .steps
        .into_iter()
        .map(|s| PlanStep {
            id: s.id,
            content: s.content,
            status: s.status,
            depends_on: s.depends_on,
            notes: s.notes,
        })
        .collect();
    if plan.steps == new_steps {
        return Ok(PlanUpdateOutcome {
            changed: false,
            newly_completed_steps: Vec::new(),
        });
    }
    plan.steps = new_steps;
    plan.updated_at = now;

    let newly_completed_steps: Vec<PlanStep> = newly_completed_ids
        .iter()
        .filter_map(|id| plan.steps.iter().find(|s| &s.id == id).cloned())
        .collect();
    Ok(PlanUpdateOutcome {
        changed: true,
        newly_completed_steps,
    })
}
// --- 序列化 / 反序列化（frontmatter + markdown body）

fn render_plan(plan: &Plan) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("mission_id: {}\n", plan.mission_id.as_str()));
    out.push_str(&format!("created_at: {}\n", plan.created_at.0));
    out.push_str(&format!("updated_at: {}\n", plan.updated_at.0));
    out.push_str("---\n\n");
    out.push_str("## Steps\n");
    for step in &plan.steps {
        let checkbox = match step.status {
            PlanStepStatus::Completed => "[x]",
            PlanStepStatus::InProgress => "[~]",
            PlanStepStatus::Cancelled => "[-]",
            PlanStepStatus::Pending => "[ ]",
        };
        out.push_str(&format!(
            "- {} {} ({}) — {}\n",
            checkbox,
            step.id,
            step.status.as_str(),
            step.content
        ));
        if !step.depends_on.is_empty() {
            out.push_str(&format!("  depends_on: {}\n", step.depends_on.join(", ")));
        }
        if let Some(notes) = &step.notes {
            if !notes.is_empty() {
                out.push_str(&format!("  notes: {notes}\n"));
            }
        }
    }
    out
}

fn parse_plan(raw: &str) -> Result<Plan, PlanError> {
    let body_start = raw
        .strip_prefix("---\n")
        .ok_or_else(|| PlanError::InvalidPlan {
            reason: "缺少 frontmatter 起始 ---".to_string(),
        })?;
    let (front, body) = body_start
        .split_once("\n---\n")
        .ok_or_else(|| PlanError::InvalidPlan {
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
        let (key, value) = line.split_once(':').ok_or_else(|| PlanError::InvalidPlan {
            reason: format!("frontmatter 行非法：{line}"),
        })?;
        let value = value.trim();
        match key.trim() {
            "mission_id" => mission_id = Some(MissionId::new(value.to_string())),
            "created_at" => {
                created_at = Some(value.parse().map_err(|_| PlanError::InvalidPlan {
                    reason: format!("created_at 解析失败：{value}"),
                })?)
            }
            "updated_at" => {
                updated_at = Some(value.parse().map_err(|_| PlanError::InvalidPlan {
                    reason: format!("updated_at 解析失败：{value}"),
                })?)
            }
            _ => {}
        }
    }
    let mission_id = mission_id.ok_or_else(|| PlanError::InvalidPlan {
        reason: "mission_id 缺失".to_string(),
    })?;
    let created_at = UtcMillis(created_at.unwrap_or(0));
    let updated_at = UtcMillis(updated_at.unwrap_or(created_at.0));

    let mut steps: Vec<PlanStep> = Vec::new();
    let mut current: Option<PlanStep> = None;
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("- ") {
            if let Some(prev) = current.take() {
                steps.push(prev);
            }
            // 形如：[x] step-1 (completed) — content
            let rest = rest.trim_start();
            let rest = rest
                .strip_prefix("[x] ")
                .or_else(|| rest.strip_prefix("[~] "))
                .or_else(|| rest.strip_prefix("[-] "))
                .or_else(|| rest.strip_prefix("[ ] "))
                .unwrap_or(rest);
            let (id, after_id) = rest.split_once(' ').ok_or_else(|| PlanError::InvalidPlan {
                reason: format!("step 行缺 id：{line}"),
            })?;
            let after_id = after_id.trim_start();
            let status = if let Some(end) = after_id.find(')') {
                let inner = &after_id[1..end];
                PlanStepStatus::from_str_lenient(inner).ok_or_else(|| PlanError::InvalidPlan {
                    reason: format!("step 状态非法：{inner}"),
                })?
            } else {
                return Err(PlanError::InvalidPlan {
                    reason: format!("step 行缺状态括号：{line}"),
                });
            };
            let content = after_id
                .split_once("— ")
                .map(|(_, c)| c.to_string())
                .unwrap_or_default();
            current = Some(PlanStep {
                id: id.to_string(),
                content,
                status,
                depends_on: Vec::new(),
                notes: None,
            });
        } else if let Some(rest) = line.strip_prefix("  depends_on:") {
            if let Some(step) = current.as_mut() {
                step.depends_on = rest
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        } else if let Some(rest) = line.strip_prefix("  notes:") {
            if let Some(step) = current.as_mut() {
                step.notes = Some(rest.trim().to_string());
            }
        }
    }
    if let Some(prev) = current.take() {
        steps.push(prev);
    }

    Ok(Plan {
        mission_id,
        steps,
        created_at,
        updated_at,
    })
}
// --- helpers

fn dirs_home() -> Result<PathBuf, PlanError> {
    // 与 ProjectMemory / MissionCharter 共用 ~/.magi 作为 magi 主目录根。
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(PlanError::HomeDirUnavailable)?;
    Ok(base.join(".magi"))
}
// --- Tool entry：`plan_write` 工具执行体

/// S12 工具下沉：`plan_write` 完整执行体收口在本 crate。`store: None` 表示当前
/// task 未绑定 workspace，直接失败。空 plan 自动创建后再 apply 更新。
///
/// 不变式 7 关口：`validation_store` 用于加载同 mission 的 `ValidationReport`，
/// 任何把 step 首次标为 completed 的写入都要满足 `step_is_passing`，否则失败。
/// `validation_store = None` 与 `store = None` 同样代表运行环境缺位，直接失败。
pub fn execute_plan_write_tool(
    event_bus: &magi_event_bus::InMemoryEventBus,
    store: Option<&PlanStore>,
    validation_store: Option<&magi_validation_runner::ValidationStore>,
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
                "tool": "plan_write",
                "status": "failed",
                "error": "当前 task 未绑定 workspace，无法定位 mission plan 目录",
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    };
    let Some(validation_store) = validation_store else {
        return (
            serde_json::json!({
                "tool": "plan_write",
                "status": "failed",
                "error": "当前 task 未绑定 workspace，无法定位 mission validation runner",
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
                    "tool": "plan_write",
                    "status": "failed",
                    "error": format!("arguments 非合法 JSON：{err}"),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let args = match parse_plan_write_arguments(&args_value) {
        Ok(parsed) => parsed,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "plan_write",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let now = UtcMillis::now();
    let mut plan_doc = match store.load(mission_id) {
        Ok(Some(existing)) => existing,
        Ok(None) => Plan::new(mission_id.clone(), now),
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "plan_write",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let validation_report = match validation_store.load(mission_id) {
        Ok(report) => report,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "plan_write",
                    "status": "failed",
                    "error": format!("加载 validation report 失败：{err}"),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let outcome = match apply_plan_update(&mut plan_doc, args, now, validation_report.as_ref()) {
        Ok(outcome) => outcome,
        Err(err @ PlanError::ValidationEvidenceMissing { .. }) => {
            // 这是不变式 7 的"软失败"——给模型清晰反馈，让它先去补 validation_record。
            return (
                serde_json::json!({
                    "tool": "plan_write",
                    "status": "failed",
                    "error": err.to_string(),
                    "hint": "先用 validation_record 工具登记 outcome=pass 后，再用 plan_write 把对应 step 标为 completed",
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "plan_write",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    if let Err(err) = store.save(&plan_doc) {
        return (
            serde_json::json!({
                "tool": "plan_write",
                "status": "failed",
                "error": err.to_string(),
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    }
    let total_steps = plan_doc.steps.len();
    let completed_steps = plan_doc
        .steps
        .iter()
        .filter(|s| s.status == PlanStepStatus::Completed)
        .count();
    let payload = serde_json::json!({
        "tool": "plan_write",
        "status": "succeeded",
        "mission_id": plan_doc.mission_id.to_string(),
        "step_count": total_steps,
        "changed": outcome.changed,
    });
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-plan-updated-{}", UtcMillis::now().0)),
            "task.plan.updated",
            serde_json::json!({
                "task_id": task_id.to_string(),
                "mission_id": plan_doc.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.map(ToString::to_string),
                "changed": outcome.changed,
                "step_count": total_steps,
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
    // 为每个本轮首次进入 completed 的 step 发布 mission.plan_step.completed,
    // 让 lifecycle-notice 订阅器据此摆出下轮 prompt 的"生命周期通知"段。
    for step in &outcome.newly_completed_steps {
        let envelope = magi_event_bus::task_events::mission_plan_step_completed_event(
            plan_doc.mission_id.as_str(),
            &step.id,
            &step.content,
            total_steps,
            completed_steps,
        )
        .with_context(EventContext {
            workspace_id: workspace_id.cloned(),
            session_id: Some(session_id.clone()),
            mission_id: Some(mission_id.clone()),
            task_id: Some(task_id.clone()),
            ..EventContext::default()
        });
        let _ = event_bus.publish(envelope);
    }
    (payload.to_string(), ExecutionResultStatus::Succeeded)
}
// --- Tests

#[cfg(test)]
mod tests {
    use super::*;

    fn mission() -> MissionId {
        MissionId::new("mission-plan-test".to_string())
    }

    #[test]
    fn save_and_load_plan_roundtrip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = WorkspaceRootPath::new("/tmp/ws-plan".to_string());
        let store = PlanStore::open_with_home(tmp.path(), &ws).expect("open");
        let plan = Plan {
            mission_id: mission(),
            steps: vec![
                PlanStep {
                    id: "s1".to_string(),
                    content: "拆 v1".to_string(),
                    status: PlanStepStatus::Completed,
                    depends_on: Vec::new(),
                    notes: Some("先盘 dispatch".to_string()),
                },
                PlanStep {
                    id: "s2".to_string(),
                    content: "切新链路".to_string(),
                    status: PlanStepStatus::InProgress,
                    depends_on: vec!["s1".to_string()],
                    notes: None,
                },
            ],
            created_at: UtcMillis(100),
            updated_at: UtcMillis(200),
        };
        store.save(&plan).expect("save");
        let loaded = store.load(&mission()).expect("load").expect("plan present");
        assert_eq!(loaded.mission_id, plan.mission_id);
        assert_eq!(loaded.steps.len(), 2);
        assert_eq!(loaded.steps[0].id, "s1");
        assert_eq!(loaded.steps[0].status, PlanStepStatus::Completed);
        assert_eq!(loaded.steps[0].notes.as_deref(), Some("先盘 dispatch"));
        assert_eq!(loaded.steps[1].depends_on, vec!["s1".to_string()]);
    }

    #[test]
    fn load_returns_none_when_plan_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = WorkspaceRootPath::new("/tmp/ws-plan-missing".to_string());
        let store = PlanStore::open_with_home(tmp.path(), &ws).expect("open");
        assert!(store.load(&mission()).expect("load").is_none());
    }

    #[test]
    fn parse_plan_write_arguments_requires_steps_array() {
        let err = parse_plan_write_arguments(&serde_json::json!({})).unwrap_err();
        match err {
            PlanError::InvalidPlan { reason } => assert!(reason.contains("steps")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parse_plan_write_arguments_rejects_empty_steps() {
        let err = parse_plan_write_arguments(&serde_json::json!({ "steps": [] })).unwrap_err();
        match err {
            PlanError::InvalidPlan { reason } => {
                assert!(reason.contains("至少需要一项"));
                // 文案不得再引用不存在的 plan_clear 工具
                assert!(!reason.contains("plan_clear"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parse_plan_write_arguments_rejects_unknown_dependency() {
        let err = parse_plan_write_arguments(&serde_json::json!({
            "steps": [{
                "id": "s1",
                "content": "x",
                "status": "pending",
                "depends_on": ["s99"]
            }]
        }))
        .unwrap_err();
        match err {
            PlanError::InvalidPlan { reason } => {
                assert!(reason.contains("s99"));
                assert!(reason.contains("不在 plan 内"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parse_plan_write_arguments_rejects_self_dependency() {
        let err = parse_plan_write_arguments(&serde_json::json!({
            "steps": [{
                "id": "s1",
                "content": "x",
                "depends_on": ["s1"]
            }]
        }))
        .unwrap_err();
        match err {
            PlanError::InvalidPlan { reason } => assert!(reason.contains("依赖自身")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parse_plan_write_arguments_defaults_status_to_pending() {
        let args = parse_plan_write_arguments(&serde_json::json!({
            "steps": [{ "id": "s1", "content": "x" }]
        }))
        .expect("parse");
        assert_eq!(args.steps[0].status, PlanStepStatus::Pending);
    }

    #[test]
    fn apply_plan_update_replaces_and_bumps_timestamp() {
        let mut plan = Plan::new(mission(), UtcMillis(100));
        let args = parse_plan_write_arguments(&serde_json::json!({
            "steps": [
                { "id": "s1", "content": "first", "status": "in_progress" }
            ]
        }))
        .expect("parse");
        let outcome = apply_plan_update(&mut plan, args, UtcMillis(200), None)
            .expect("非 completed 转换不应走 validation 关口");
        assert!(outcome.changed);
        assert!(outcome.newly_completed_steps.is_empty());
        assert_eq!(plan.updated_at.0, 200);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].status, PlanStepStatus::InProgress);
    }

    #[test]
    fn apply_plan_update_no_op_when_steps_identical() {
        let mut plan = Plan {
            mission_id: mission(),
            steps: vec![PlanStep {
                id: "s1".to_string(),
                content: "x".to_string(),
                status: PlanStepStatus::Pending,
                depends_on: Vec::new(),
                notes: None,
            }],
            created_at: UtcMillis(100),
            updated_at: UtcMillis(100),
        };
        let args = parse_plan_write_arguments(&serde_json::json!({
            "steps": [{ "id": "s1", "content": "x", "status": "pending" }]
        }))
        .expect("parse");
        let outcome = apply_plan_update(&mut plan, args, UtcMillis(200), None)
            .expect("nop 路径不应触发 validation 关口");
        assert!(!outcome.changed);
        assert!(outcome.newly_completed_steps.is_empty());
        assert_eq!(plan.updated_at.0, 100);
    }

    #[test]
    fn apply_plan_update_rejects_newly_completed_without_validation_evidence() {
        use magi_validation_runner::ValidationReport;
        let mut plan = Plan::new(mission(), UtcMillis(100));
        let args = parse_plan_write_arguments(&serde_json::json!({
            "steps": [{ "id": "s1", "content": "x", "status": "completed" }]
        }))
        .expect("parse");
        // 没有 validation report：等同于无证据，必须拒绝把 s1 直接标 completed。
        let err =
            apply_plan_update(&mut plan, args, UtcMillis(200), None).expect_err("缺证据必须拒写");
        match err {
            PlanError::ValidationEvidenceMissing { step_id } => assert_eq!(step_id, "s1"),
            other => panic!("expected ValidationEvidenceMissing, got {other:?}"),
        }
        // plan 不应被修改。
        assert!(plan.steps.is_empty());
        assert_eq!(plan.updated_at.0, 100);

        // 提供一份 report，但 s1 只有 fail 记录 → 仍要拒。
        let mut report = ValidationReport::new(mission(), UtcMillis(150));
        magi_validation_runner::apply_validation_record(
            &mut report,
            magi_validation_runner::ValidationRecordArgs {
                plan_step_id: "s1".to_string(),
                kind: magi_validation_runner::ValidationKind::TestSuite,
                outcome: magi_validation_runner::ValidationOutcome::Fail,
                command: None,
                evidence: Some("regression".to_string()),
            },
            UtcMillis(160),
        );
        let args = parse_plan_write_arguments(&serde_json::json!({
            "steps": [{ "id": "s1", "content": "x", "status": "completed" }]
        }))
        .expect("parse");
        let err = apply_plan_update(&mut plan, args, UtcMillis(200), Some(&report))
            .expect_err("仅 fail 记录不构成证据");
        assert!(matches!(err, PlanError::ValidationEvidenceMissing { .. }));
    }

    #[test]
    fn apply_plan_update_accepts_completion_with_passing_validation() {
        use magi_validation_runner::ValidationReport;
        let mut plan = Plan::new(mission(), UtcMillis(100));
        let mut report = ValidationReport::new(mission(), UtcMillis(150));
        magi_validation_runner::apply_validation_record(
            &mut report,
            magi_validation_runner::ValidationRecordArgs {
                plan_step_id: "s1".to_string(),
                kind: magi_validation_runner::ValidationKind::TestSuite,
                outcome: magi_validation_runner::ValidationOutcome::Pass,
                command: Some("cargo test".to_string()),
                evidence: Some("16 passed".to_string()),
            },
            UtcMillis(160),
        );
        let args = parse_plan_write_arguments(&serde_json::json!({
            "steps": [{ "id": "s1", "content": "x", "status": "completed" }]
        }))
        .expect("parse");
        let outcome = apply_plan_update(&mut plan, args, UtcMillis(200), Some(&report))
            .expect("有 pass 记录必须放行");
        assert!(outcome.changed);
        assert_eq!(outcome.newly_completed_steps.len(), 1);
        assert_eq!(outcome.newly_completed_steps[0].id, "s1");
        assert_eq!(plan.steps[0].status, PlanStepStatus::Completed);
    }

    #[test]
    fn apply_plan_update_allows_keeping_existing_completed_without_revalidation() {
        // 已经处于 completed 的 step 在后续 plan_write 里继续保持 completed，
        // 不需要重新读 validation——避免一次写入失败连带封死整张 plan。
        let mut plan = Plan {
            mission_id: mission(),
            steps: vec![PlanStep {
                id: "s1".to_string(),
                content: "x".to_string(),
                status: PlanStepStatus::Completed,
                depends_on: Vec::new(),
                notes: None,
            }],
            created_at: UtcMillis(100),
            updated_at: UtcMillis(100),
        };
        let args = parse_plan_write_arguments(&serde_json::json!({
            "steps": [
                { "id": "s1", "content": "x", "status": "completed" },
                { "id": "s2", "content": "y", "status": "in_progress" }
            ]
        }))
        .expect("parse");
        let outcome = apply_plan_update(&mut plan, args, UtcMillis(200), None)
            .expect("沿用既有 completed 不应触发 validation 关口");
        assert!(outcome.changed);
        // s1 之前就是 completed，不属于"本次新转 completed"。
        assert!(outcome.newly_completed_steps.is_empty());
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].status, PlanStepStatus::Completed);
        assert_eq!(plan.steps[1].status, PlanStepStatus::InProgress);
    }

    #[test]
    fn render_for_prompt_returns_none_when_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = WorkspaceRootPath::new("/tmp/ws-plan-empty".to_string());
        let store = PlanStore::open_with_home(tmp.path(), &ws).expect("open");
        // 未落盘
        assert!(
            store
                .render_for_prompt(&mission())
                .expect("render")
                .is_none()
        );
        // 空 steps
        store
            .save(&Plan::new(mission(), UtcMillis(0)))
            .expect("save");
        assert!(
            store
                .render_for_prompt(&mission())
                .expect("render")
                .is_none()
        );
    }

    #[test]
    fn render_for_prompt_lists_step_metadata() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = WorkspaceRootPath::new("/tmp/ws-plan-render".to_string());
        let store = PlanStore::open_with_home(tmp.path(), &ws).expect("open");
        let plan = Plan {
            mission_id: mission(),
            steps: vec![PlanStep {
                id: "s1".to_string(),
                content: "拆 v1".to_string(),
                status: PlanStepStatus::Completed,
                depends_on: vec!["s0".to_string()],
                notes: Some("ok".to_string()),
            }],
            created_at: UtcMillis(0),
            updated_at: UtcMillis(0),
        };
        store.save(&plan).expect("save");
        let rendered = store
            .render_for_prompt(&mission())
            .expect("render")
            .expect("present");
        assert!(rendered.contains("# Mission Plan"));
        assert!(rendered.contains("不得覆盖本轮用户输入"));
        assert!(rendered.contains("通过 plan_write 更新计划"));
        assert!(rendered.contains("s1"));
        assert!(rendered.contains("(completed)"));
        assert!(rendered.contains("depends_on: s0"));
        assert!(rendered.contains("notes: ok"));
    }
}
