//! Worker 分支检查点存储。
//!
//! 记录每个执行分支最近一次的 stage、租约、绑定生命周期与检查点游标，
//! 由 daemon 持久化为 durable snapshot，并在会话续跑时回灌。

use magi_core::{TaskId, UtcMillis, WorkerId};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerStage {
    Execute,
    Review,
    Verify,
    Repair,
    Finish,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerCheckpointResumeMode {
    StepCheckpoint,
    StageRestart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WorkerExecutionBindingLifecycle {
    #[default]
    None,
    Requested,
    Bound,
    Released,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerExecutionCheckpointCursor {
    pub checkpoint_stage: WorkerStage,
    pub next_step_index: usize,
    pub checkpoint_at: UtcMillis,
    pub resume_mode: WorkerCheckpointResumeMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_token: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerRuntimeBranchSnapshot {
    pub task_id: TaskId,
    pub worker_id: WorkerId,
    pub stage: WorkerStage,
    pub lease_id: Option<String>,
    pub execution_intent_ref: Option<String>,
    pub binding_lifecycle: Option<WorkerExecutionBindingLifecycle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_cursor: Option<WorkerExecutionCheckpointCursor>,
}

#[derive(Clone, Debug, Default)]
pub struct WorkerBranchCheckpointState {
    pub lease_id: Option<String>,
    pub execution_intent_ref: Option<String>,
    pub binding_lifecycle: Option<WorkerExecutionBindingLifecycle>,
    pub checkpoint_cursor: Option<WorkerExecutionCheckpointCursor>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerRuntimeDurableSnapshot {
    pub branches: Vec<WorkerRuntimeBranchSnapshot>,
}

#[derive(Clone, Default)]
pub struct WorkerRuntime {
    branch_snapshots: Arc<RwLock<HashMap<TaskId, WorkerRuntimeBranchSnapshot>>>,
    durable_snapshot_version: Arc<AtomicU64>,
    flushed_durable_snapshot_version: Arc<AtomicU64>,
}

impl WorkerRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    /// 合并写入分支检查点：未提供的字段沿用已有快照；进入 Finish 时清除游标。
    pub fn record_branch_checkpoint(
        &self,
        task_id: &TaskId,
        worker_id: &WorkerId,
        stage: WorkerStage,
        state: WorkerBranchCheckpointState,
    ) -> WorkerRuntimeBranchSnapshot {
        let WorkerBranchCheckpointState {
            lease_id,
            execution_intent_ref,
            binding_lifecycle,
            checkpoint_cursor,
        } = state;
        let snapshot = {
            let mut snapshots = self
                .branch_snapshots
                .write()
                .expect("worker branch snapshot write lock poisoned");
            let existing = snapshots.get(task_id);
            let next = WorkerRuntimeBranchSnapshot {
                task_id: task_id.clone(),
                worker_id: worker_id.clone(),
                stage,
                lease_id: lease_id
                    .or_else(|| existing.and_then(|snapshot| snapshot.lease_id.clone())),
                execution_intent_ref: execution_intent_ref.or_else(|| {
                    existing.and_then(|snapshot| snapshot.execution_intent_ref.clone())
                }),
                binding_lifecycle: binding_lifecycle
                    .or_else(|| existing.and_then(|snapshot| snapshot.binding_lifecycle)),
                checkpoint_cursor: if matches!(stage, WorkerStage::Finish) {
                    None
                } else {
                    checkpoint_cursor.or_else(|| {
                        existing.and_then(|snapshot| snapshot.checkpoint_cursor.clone())
                    })
                },
            };
            snapshots.insert(task_id.clone(), next.clone());
            next
        };
        self.durable_snapshot_version
            .fetch_add(1, Ordering::Relaxed);
        snapshot
    }

    pub fn branch_snapshot_for_task(
        &self,
        task_id: &TaskId,
    ) -> Option<WorkerRuntimeBranchSnapshot> {
        self.branch_snapshots
            .read()
            .expect("worker branch snapshot read lock poisoned")
            .get(task_id)
            .cloned()
    }

    pub fn durable_snapshot(&self) -> WorkerRuntimeDurableSnapshot {
        let mut branches = self
            .branch_snapshots
            .read()
            .expect("worker branch snapshot read lock poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        branches.sort_by(|left, right| {
            left.task_id
                .as_str()
                .cmp(right.task_id.as_str())
                .then_with(|| left.worker_id.as_str().cmp(right.worker_id.as_str()))
        });
        WorkerRuntimeDurableSnapshot { branches }
    }

    pub fn durable_snapshot_dirty(&self) -> bool {
        self.durable_snapshot_version.load(Ordering::Relaxed)
            != self
                .flushed_durable_snapshot_version
                .load(Ordering::Relaxed)
    }

    pub fn restore_durable_snapshot(&self, snapshot: WorkerRuntimeDurableSnapshot) {
        let mut branch_map = self
            .branch_snapshots
            .write()
            .expect("worker branch snapshot write lock poisoned");
        branch_map.clear();
        for branch in snapshot.branches {
            branch_map.insert(branch.task_id.clone(), branch);
        }
        drop(branch_map);
        self.durable_snapshot_version.store(0, Ordering::Relaxed);
        self.flushed_durable_snapshot_version
            .store(0, Ordering::Relaxed);
    }

    pub fn flush_durable_snapshot_with<E, F>(&self, persist: F) -> Result<bool, E>
    where
        F: FnOnce(&WorkerRuntimeDurableSnapshot) -> Result<(), E>,
    {
        let current_version = self.durable_snapshot_version.load(Ordering::Relaxed);
        let flushed_version = self
            .flushed_durable_snapshot_version
            .load(Ordering::Relaxed);
        if current_version == flushed_version {
            return Ok(false);
        }
        let snapshot = self.durable_snapshot();
        persist(&snapshot)?;
        self.flushed_durable_snapshot_version
            .store(current_version, Ordering::Relaxed);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(stage: WorkerStage, next_step_index: usize) -> WorkerExecutionCheckpointCursor {
        WorkerExecutionCheckpointCursor {
            checkpoint_stage: stage,
            next_step_index,
            checkpoint_at: UtcMillis(1),
            resume_mode: WorkerCheckpointResumeMode::StepCheckpoint,
            resume_token: None,
        }
    }

    #[test]
    fn record_branch_checkpoint_merges_missing_fields_and_clears_cursor_on_finish() {
        let runtime = WorkerRuntime::new();
        let task_id = TaskId::new("task-1");
        let worker_id = WorkerId::new("worker-1");
        runtime.record_branch_checkpoint(
            &task_id,
            &worker_id,
            WorkerStage::Execute,
            WorkerBranchCheckpointState {
                lease_id: Some("lease-1".to_string()),
                execution_intent_ref: Some("intent-1".to_string()),
                binding_lifecycle: Some(WorkerExecutionBindingLifecycle::Bound),
                checkpoint_cursor: Some(cursor(WorkerStage::Execute, 2)),
            },
        );

        let verify = runtime.record_branch_checkpoint(
            &task_id,
            &worker_id,
            WorkerStage::Verify,
            WorkerBranchCheckpointState::default(),
        );
        assert_eq!(verify.lease_id.as_deref(), Some("lease-1"));
        assert_eq!(verify.execution_intent_ref.as_deref(), Some("intent-1"));
        assert_eq!(
            verify.binding_lifecycle,
            Some(WorkerExecutionBindingLifecycle::Bound)
        );
        assert_eq!(
            verify.checkpoint_cursor,
            Some(cursor(WorkerStage::Execute, 2))
        );

        let finish = runtime.record_branch_checkpoint(
            &task_id,
            &worker_id,
            WorkerStage::Finish,
            WorkerBranchCheckpointState {
                checkpoint_cursor: Some(cursor(WorkerStage::Finish, 3)),
                ..WorkerBranchCheckpointState::default()
            },
        );
        assert_eq!(finish.checkpoint_cursor, None);
        assert_eq!(runtime.branch_snapshot_for_task(&task_id), Some(finish));
    }

    #[test]
    fn durable_snapshot_flush_tracks_dirty_versions_and_restore_resets_them() {
        let runtime = WorkerRuntime::new();
        assert!(!runtime.durable_snapshot_dirty());
        runtime.record_branch_checkpoint(
            &TaskId::new("task-b"),
            &WorkerId::new("worker-1"),
            WorkerStage::Execute,
            WorkerBranchCheckpointState::default(),
        );
        runtime.record_branch_checkpoint(
            &TaskId::new("task-a"),
            &WorkerId::new("worker-1"),
            WorkerStage::Review,
            WorkerBranchCheckpointState::default(),
        );
        assert!(runtime.durable_snapshot_dirty());

        let mut persisted = None;
        let flushed = runtime
            .flush_durable_snapshot_with(|snapshot| {
                persisted = Some(snapshot.clone());
                Ok::<(), ()>(())
            })
            .expect("flush should succeed");
        assert!(flushed);
        assert!(!runtime.durable_snapshot_dirty());
        let persisted = persisted.expect("snapshot should be persisted");
        let task_ids = persisted
            .branches
            .iter()
            .map(|branch| branch.task_id.as_str().to_string())
            .collect::<Vec<_>>();
        assert_eq!(task_ids, vec!["task-a", "task-b"]);
        assert_eq!(
            runtime.flush_durable_snapshot_with(|_| Err::<(), ()>(())),
            Ok(false)
        );

        let restored = WorkerRuntime::new();
        restored.restore_durable_snapshot(persisted.clone());
        assert!(!restored.durable_snapshot_dirty());
        assert_eq!(restored.durable_snapshot(), persisted);
    }
}
