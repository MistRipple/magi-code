//! Task System v2 §1.4 读端：Mission 聚合恢复入口。
//!
//! 写端契约由 `magi-checkpoint` 落地：`Checkpoint::recovery_set_status` 和
//! `append_checkpoint` 拒绝缺料写入；但读端长期是孤儿——`CheckpointLog::latest`
//! 没有调用方，七个 Tier 4 store 散在 7 个 crate 里，没有"一次性聚合并校验恢复
//! 集"的 chokepoint。本 crate 把这个口子补齐：
//!
//! - `MissionAggregate` 是聚合根，构造时已通过 head Checkpoint 的恢复集校验，
//!   保证拿到 aggregate 的调用方可以直接拿来恢复 active execution chain；
//! - Charter / Plan / 最近 Checkpoint 是恢复必需的，缺一即拒绝；
//! - KG / Validation / HumanCheckpoint 是 Mission 演进中按需产生的，缺失合法，
//!   通过 lazy 方法按需 load，返回 `Option`；
//! - `enumerate_resumable_missions` 用于 daemon bootstrap 扫描（Phase B 使用）。
//!
//! 依赖方向：`magi-mission` → 7 个 store，store 不反向依赖本 crate。这保证
//! aggregate 是上层视图，不污染底层契约。

use std::path::{Path, PathBuf};

use magi_checkpoint::{Checkpoint, CheckpointLog, CheckpointStore, MissingRecoverySetReason};
use magi_core::{MissionId, MissionLifecyclePhase, WorkspaceRootPath};
use magi_human_checkpoint::{HumanCheckpointLog, HumanCheckpointStore};
use magi_knowledge_graph::{KnowledgeGraph, KnowledgeGraphStore};
use magi_mission_charter::{MissionCharter, MissionCharterStore};
use magi_mission_metrics::{MissionMetrics, MissionMetricsError, MissionMetricsStore};
use magi_mission_workspace::{MissionWorkspace, MissionWorkspaceStore};
use magi_plan::{Plan, PlanStepStatus, PlanStore};
use magi_validation_runner::{ValidationReport, ValidationStore};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreKind {
    Charter,
    Plan,
    KnowledgeGraph,
    Validation,
    Workspace,
    Checkpoint,
    HumanCheckpoint,
    Metrics,
}

impl std::fmt::Display for StoreKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Charter => "charter",
            Self::Plan => "plan",
            Self::KnowledgeGraph => "knowledge_graph",
            Self::Validation => "validation",
            Self::Workspace => "workspace",
            Self::Checkpoint => "checkpoint",
            Self::HumanCheckpoint => "human_checkpoint",
            Self::Metrics => "metrics",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Error)]
pub enum MissionResumeError {
    #[error("mission {mission_id} 缺少 charter.md，无法恢复（业务契约必需）")]
    CharterMissing { mission_id: MissionId },
    #[error("mission {mission_id} 缺少 plan.md，无法恢复（推进锚点必需）")]
    PlanMissing { mission_id: MissionId },
    #[error("mission {mission_id} 缺少 checkpoints.md，无法定位恢复点")]
    CheckpointLogMissing { mission_id: MissionId },
    #[error("mission {mission_id} 最近 Checkpoint 恢复集不完整：{reason}")]
    LatestCheckpointIncomplete {
        mission_id: MissionId,
        reason: MissingRecoverySetReason,
    },
    #[error("store {which} 读取失败：{source}")]
    StoreError {
        which: StoreKind,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("I/O 错误 {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl MissionResumeError {
    fn from_store<E>(which: StoreKind, err: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::StoreError {
            which,
            source: Box::new(err),
        }
    }

    fn from_metrics(err: MissionMetricsError) -> Self {
        Self::StoreError {
            which: StoreKind::Metrics,
            source: Box::new(err),
        }
    }
}

/// 聚合根：构造完成时已通过 head Checkpoint 恢复集校验。
///
/// 体量小、必需的恢复 head（Charter / Plan / 最近 Checkpoint）一次性持有；
/// 体量大或可空的二级数据（CheckpointLog / KG / Validation / HumanCheckpoint）
/// 按需 lazy 加载，避免内存尖峰。
#[derive(Debug)]
pub struct MissionAggregate {
    mission_id: MissionId,
    workspace_root: WorkspaceRootPath,
    magi_home: PathBuf,
    head_checkpoint: Checkpoint,
    charter_head: MissionCharter,
    plan_head: Plan,
}

impl MissionAggregate {
    pub fn mission_id(&self) -> &MissionId {
        &self.mission_id
    }

    pub fn workspace_root(&self) -> &WorkspaceRootPath {
        &self.workspace_root
    }

    pub fn magi_home(&self) -> &Path {
        &self.magi_home
    }

    /// 已通过 `recovery_set_status` 校验的最近 Checkpoint。
    pub fn head_checkpoint(&self) -> &Checkpoint {
        &self.head_checkpoint
    }

    pub fn charter_head(&self) -> &MissionCharter {
        &self.charter_head
    }

    pub fn plan_head(&self) -> &Plan {
        &self.plan_head
    }

    /// 重新读全量 CheckpointLog（用于回放/审计）。head 已经被 resume 加载过，
    /// 这里再 load 一次以拿到完整序列；不缓存，避免与底层文件状态偏离。
    pub fn checkpoint_log(&self) -> Result<CheckpointLog, MissionResumeError> {
        let store = CheckpointStore::open_with_home(&self.magi_home, &self.workspace_root)
            .map_err(|e| MissionResumeError::from_store(StoreKind::Checkpoint, e))?;
        match store
            .load(&self.mission_id)
            .map_err(|e| MissionResumeError::from_store(StoreKind::Checkpoint, e))?
        {
            Some(log) => Ok(log),
            None => Err(MissionResumeError::CheckpointLogMissing {
                mission_id: self.mission_id.clone(),
            }),
        }
    }

    /// 当前 KG 快照；Mission 早期未跑过 KG 则返回 None（合法状态）。
    pub fn knowledge(&self) -> Result<Option<KnowledgeGraph>, MissionResumeError> {
        let store = KnowledgeGraphStore::open_with_home(&self.magi_home, &self.workspace_root)
            .map_err(|e| MissionResumeError::from_store(StoreKind::KnowledgeGraph, e))?;
        store
            .load(&self.mission_id)
            .map_err(|e| MissionResumeError::from_store(StoreKind::KnowledgeGraph, e))
    }

    /// 当前 ValidationReport；Mission 未跑过 validation 则返回 None（合法状态）。
    pub fn validation(&self) -> Result<Option<ValidationReport>, MissionResumeError> {
        let store = ValidationStore::open_with_home(&self.magi_home, &self.workspace_root)
            .map_err(|e| MissionResumeError::from_store(StoreKind::Validation, e))?;
        store
            .load(&self.mission_id)
            .map_err(|e| MissionResumeError::from_store(StoreKind::Validation, e))
    }

    /// HumanCheckpoint 日志；Mission 没经过人审则返回 None（合法状态）。
    pub fn human_checkpoint_log(&self) -> Result<Option<HumanCheckpointLog>, MissionResumeError> {
        let store = HumanCheckpointStore::open_with_home(&self.magi_home, &self.workspace_root)
            .map_err(|e| MissionResumeError::from_store(StoreKind::HumanCheckpoint, e))?;
        store
            .load(&self.mission_id)
            .map_err(|e| MissionResumeError::from_store(StoreKind::HumanCheckpoint, e))
    }

    /// 物理 workspace 句柄（artifacts / logs / memory.md 路径）。
    pub fn workspace(&self) -> Result<MissionWorkspace, MissionResumeError> {
        let store = MissionWorkspaceStore::open_with_home(&self.magi_home, &self.workspace_root)
            .map_err(|e| MissionResumeError::from_store(StoreKind::Workspace, e))?;
        Ok(store.locate(&self.mission_id))
    }

    /// mission 累计 metrics（turn 数 / token / wall-clock）。
    /// 第一次记账前文件不存在，返回 `Ok(None)`，由调用方决定如何展示。
    pub fn metrics(&self) -> Result<Option<MissionMetrics>, MissionResumeError> {
        let store = MissionMetricsStore::open_with_home(&self.magi_home, &self.workspace_root)
            .map_err(MissionResumeError::from_metrics)?;
        store
            .load(&self.mission_id)
            .map_err(MissionResumeError::from_metrics)
    }

    /// 派生当前 mission 所处的生命周期阶段。
    ///
    /// 仅基于已加载/可懒加载的状态推导，不写盘：
    /// - charter 未冻结 → `CharterDraft`
    /// - 有 Pending 人审 → `AwaitingHumanCheckpoint`
    /// - plan.steps 为空 → `PlanReady`
    /// - 全部步骤为 Completed/Cancelled → `AllStepsCompleted`
    /// - 至少有一个 InProgress 或部分 Completed → `Executing`
    /// - 否则（全 Pending/混合 Pending+Cancelled）→ `PlanReady`
    ///
    /// 优先级：CharterDraft > AwaitingHumanCheckpoint > AllStepsCompleted > Executing > PlanReady。
    pub fn lifecycle_phase(&self) -> Result<MissionLifecyclePhase, MissionResumeError> {
        if !self.charter_head.state.is_frozen() {
            return Ok(MissionLifecyclePhase::CharterDraft);
        }
        if let Some(log) = self.human_checkpoint_log()? {
            if log.has_pending() {
                return Ok(MissionLifecyclePhase::AwaitingHumanCheckpoint);
            }
        }
        let steps = &self.plan_head.steps;
        if steps.is_empty() {
            return Ok(MissionLifecyclePhase::PlanReady);
        }
        let all_done = steps.iter().all(|s| {
            matches!(
                s.status,
                PlanStepStatus::Completed | PlanStepStatus::Cancelled
            )
        });
        if all_done {
            return Ok(MissionLifecyclePhase::AllStepsCompleted);
        }
        let any_started = steps.iter().any(|s| {
            matches!(
                s.status,
                PlanStepStatus::InProgress | PlanStepStatus::Completed
            )
        });
        if any_started {
            Ok(MissionLifecyclePhase::Executing)
        } else {
            Ok(MissionLifecyclePhase::PlanReady)
        }
    }
}

/// 读取并校验单个 Mission 的恢复集，返回聚合根。
///
/// 失败模式：
/// - Charter / Plan / CheckpointLog 缺失 → `*Missing`；
/// - 最近 Checkpoint 缺料 → `LatestCheckpointIncomplete`（**读端对称写端**）；
/// - I/O 或 store 内部解析失败 → `Io` / `StoreError`。
pub fn resume_mission(
    mission_id: &MissionId,
    workspace_root: &WorkspaceRootPath,
    magi_home: &Path,
) -> Result<MissionAggregate, MissionResumeError> {
    let charter_store = MissionCharterStore::open_with_home(magi_home, workspace_root)
        .map_err(|e| MissionResumeError::from_store(StoreKind::Charter, e))?;
    let charter_head = charter_store
        .load(mission_id)
        .map_err(|e| MissionResumeError::from_store(StoreKind::Charter, e))?
        .ok_or_else(|| MissionResumeError::CharterMissing {
            mission_id: mission_id.clone(),
        })?;

    let plan_store = PlanStore::open_with_home(magi_home, workspace_root)
        .map_err(|e| MissionResumeError::from_store(StoreKind::Plan, e))?;
    let plan_head = plan_store
        .load(mission_id)
        .map_err(|e| MissionResumeError::from_store(StoreKind::Plan, e))?
        .ok_or_else(|| MissionResumeError::PlanMissing {
            mission_id: mission_id.clone(),
        })?;

    let checkpoint_store = CheckpointStore::open_with_home(magi_home, workspace_root)
        .map_err(|e| MissionResumeError::from_store(StoreKind::Checkpoint, e))?;
    let checkpoint_log = checkpoint_store
        .load(mission_id)
        .map_err(|e| MissionResumeError::from_store(StoreKind::Checkpoint, e))?
        .ok_or_else(|| MissionResumeError::CheckpointLogMissing {
            mission_id: mission_id.clone(),
        })?;

    let head = checkpoint_log
        .latest()
        .ok_or_else(|| MissionResumeError::CheckpointLogMissing {
            mission_id: mission_id.clone(),
        })?;
    if let Err(reason) = head.recovery_set_status() {
        return Err(MissionResumeError::LatestCheckpointIncomplete {
            mission_id: mission_id.clone(),
            reason,
        });
    }
    let head_checkpoint = head.clone();

    Ok(MissionAggregate {
        mission_id: mission_id.clone(),
        workspace_root: workspace_root.clone(),
        magi_home: magi_home.to_path_buf(),
        head_checkpoint,
        charter_head,
        plan_head,
    })
}

/// 扫描 `<magi_home>/projects/<slug>/missions/` 下所有子目录，返回带 charter.md
/// 的 mission_id 列表（升序）。空目录或不带 charter 的目录视为"已废弃 / 未真正
/// 开始的 mission"，跳过。
///
/// 不会触发 `resume_mission`——枚举只看目录是否存在，调用方需要逐个 resume
/// 才能拿到校验过的聚合根。这样设计是为了让 daemon bootstrap 能批量发现 mission
/// 后再决定哪些要全量恢复、哪些只展示元数据。
pub fn enumerate_resumable_missions(
    workspace_root: &WorkspaceRootPath,
    magi_home: &Path,
) -> Result<Vec<MissionId>, MissionResumeError> {
    let root = magi_core::paths::missions_root(magi_home, workspace_root);
    let read_dir = match std::fs::read_dir(&root) {
        Ok(rd) => rd,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(MissionResumeError::Io { path: root, source }),
    };
    let mut ids = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(|source| MissionResumeError::Io {
            path: root.clone(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| MissionResumeError::Io {
            path: entry.path(),
            source,
        })?;
        if !file_type.is_dir() {
            continue;
        }
        let path = entry.path();
        if !path.join("charter.md").is_file() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            ids.push(MissionId::new(name.to_string()));
        }
    }
    ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    Ok(ids)
}

#[cfg(test)]
mod tests;
