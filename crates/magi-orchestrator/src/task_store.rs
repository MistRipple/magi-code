use magi_core::{
    DomainError, DomainResult, LeaseId, MissionId, ProgressSummary, Task, TaskId, TaskKind,
    TaskPolicy, TaskProjection, TaskStatus, TaskTier, UtcMillis, WorkerId,
};
use magi_worker_runtime::WorkerRuntimeDurableSnapshot;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};

static LEASE_COUNTER: AtomicU64 = AtomicU64::new(1);
static CHECKPOINT_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskLease {
    pub lease_id: LeaseId,
    pub task_id: TaskId,
    pub root_task_id: TaskId,
    pub worker_id: WorkerId,
    pub role: String,
    pub granted_at: UtcMillis,
    pub expires_at: UtcMillis,
    pub heartbeat_at: UtcMillis,
    pub lease_status: TaskLeaseState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskLeaseState {
    Active,
    Completed,
    Expired,
    Revoked,
}

/// Callback invoked after a successful `update_status` call.
///
/// Receives the task ID, old status, new status, and a snapshot of the task
/// after the status change. Implementations should be lightweight (e.g.
/// publish an event).
pub type StatusChangeCallback = Box<dyn Fn(&TaskId, TaskStatus, TaskStatus, Task) + Send + Sync>;

/// 任务投影的内存存储，维护任务、租约及其索引。
pub struct TaskStore {
    tasks: RwLock<HashMap<TaskId, Task>>,
    leases: RwLock<HashMap<LeaseId, TaskLease>>,
    /// 索引: mission_id -> task_ids
    mission_index: RwLock<HashMap<MissionId, Vec<TaskId>>>,
    /// Optional callback fired on every successful status change.
    ///
    /// Wrapped in a `Mutex` so that `set_status_change_callback` can replace
    /// the callback through a `&self` reference (needed after restoring from
    /// a checkpoint).
    on_status_change: Mutex<Option<StatusChangeCallback>>,
    /// Optional callback fired on every successful status change for checkpoint
    /// persistence (design 6.8).
    on_checkpoint: Mutex<Option<Box<dyn Fn(&TaskStore) + Send + Sync>>>,
}

fn default_frozen_policy() -> TaskPolicy {
    TaskPolicy {
        autonomy_level: "Assisted".to_string(),
        approval_mode: "Interactive".to_string(),
        allowed_tools: Vec::new(),
        denied_tools: Vec::new(),
        allowed_paths: Vec::new(),
        denied_paths: Vec::new(),
        network_mode: "full".to_string(),
        command_mode: "full".to_string(),
        retry_limit: 1,
        validation_profile: None,
        checkpoint_mode: "turn".to_string(),
        task_tier: TaskTier::ExecutionChain,
        background_allowed: false,
        escalation_conditions: Vec::new(),
    }
}

fn task_requires_delivery_evidence(task: &Task) -> bool {
    task.kind == TaskKind::LocalAgent
        && task.policy_snapshot.as_ref().is_some_and(|policy| {
            policy.task_tier == TaskTier::LongMission && policy.validation_profile.is_some()
        })
}

fn output_ref_is_file_change(output_ref: &str) -> bool {
    let trimmed = output_ref.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('{')
        || trimmed.starts_with('[')
        || trimmed.contains('\n')
        || trimmed.contains('\r')
    {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("evidence://")
        || lower.starts_with("test://")
        || lower.starts_with("tool://")
        || lower.starts_with("http://")
        || lower.starts_with("https://")
    {
        return false;
    }
    if lower.starts_with("file:") || lower.starts_with("diff:") {
        return true;
    }
    let path = Path::new(trimmed);
    (trimmed.contains('/') || trimmed.contains('\\'))
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                let extension = extension.trim();
                !extension.is_empty() && extension.len() <= 16
            })
}

fn child_index_from_tasks(tasks: &HashMap<TaskId, Task>) -> HashMap<TaskId, Vec<TaskId>> {
    let mut children: HashMap<TaskId, Vec<TaskId>> = HashMap::new();
    for task in tasks.values() {
        if let Some(parent_id) = task.parent_task_id.as_ref() {
            children
                .entry(parent_id.clone())
                .or_default()
                .push(task.task_id.clone());
        }
    }
    // 子任务列表必须按 (created_at, task_id) 排序，消除 HashMap 迭代序带来的非确定性。
    // 下游 collect_subtree_ids / get_children 等 BFS / 投影逻辑都依赖这一确定顺序。
    for child_ids in children.values_mut() {
        child_ids.sort_by(|a, b| {
            let ord = tasks
                .get(a)
                .map(|t| t.created_at)
                .cmp(&tasks.get(b).map(|t| t.created_at));
            ord.then_with(|| a.as_str().cmp(b.as_str()))
        });
    }
    children
}

impl TaskStore {
    pub fn new() -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            leases: RwLock::new(HashMap::new()),
            mission_index: RwLock::new(HashMap::new()),
            on_status_change: Mutex::new(None),
            on_checkpoint: Mutex::new(None),
        }
    }

    /// Create a store with a callback that fires after every successful
    /// `update_status` call.
    pub fn with_status_change_callback(callback: StatusChangeCallback) -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            leases: RwLock::new(HashMap::new()),
            mission_index: RwLock::new(HashMap::new()),
            on_status_change: Mutex::new(Some(callback)),
            on_checkpoint: Mutex::new(None),
        }
    }

    /// Set or replace the status-change callback.
    ///
    /// This is useful after restoring from a checkpoint, since callbacks are
    /// not serialized.
    pub fn set_status_change_callback(&self, callback: StatusChangeCallback) {
        let mut guard = self
            .on_status_change
            .lock()
            .expect("on_status_change lock poisoned");
        *guard = Some(callback);
    }

    /// Set or replace the per-transition checkpoint callback (design 6.8).
    pub fn set_checkpoint_callback(&self, callback: Box<dyn Fn(&TaskStore) + Send + Sync>) {
        let mut guard = self
            .on_checkpoint
            .lock()
            .expect("on_checkpoint lock poisoned");
        *guard = Some(callback);
    }

    fn fire_checkpoint(&self) {
        let guard = self
            .on_checkpoint
            .lock()
            .expect("on_checkpoint lock poisoned");
        if let Some(ref cb) = *guard {
            cb(self);
        }
    }

    /// 插入一个任务并更新索引。
    pub fn insert_task(&self, task: Task) {
        let task_id = task.task_id.clone();
        let mission_id = task.mission_id.clone();
        self.tasks
            .write()
            .expect("tasks write lock poisoned")
            .insert(task_id.clone(), task);

        self.mission_index
            .write()
            .expect("mission_index write lock poisoned")
            .entry(mission_id)
            .or_default()
            .push(task_id.clone());
    }

    /// 通过 ID 获取任务。
    pub fn get_task(&self, task_id: &TaskId) -> Option<Task> {
        self.tasks
            .read()
            .expect("tasks read lock poisoned")
            .get(task_id)
            .cloned()
    }

    /// 迁移任务到新的父节点，同时修正 children 索引。
    pub fn reparent_task(&self, task_id: &TaskId, new_parent_id: &TaskId) -> DomainResult<()> {
        let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
        let old_parent = tasks
            .get(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?
            .parent_task_id
            .clone();
        let was_required = old_parent.as_ref().is_some_and(|parent_id| {
            tasks
                .get(parent_id)
                .is_some_and(|parent| parent.required_children.contains(task_id))
        });
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        task.parent_task_id = Some(new_parent_id.clone());
        task.updated_at = UtcMillis::now();
        drop(tasks);

        if let Some(old_parent_id) = old_parent.clone() {
            let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
            if let Some(parent) = tasks.get_mut(&old_parent_id) {
                parent.required_children.retain(|id| id != task_id);
                parent.updated_at = UtcMillis::now();
            }
        }

        if was_required {
            let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
            if let Some(parent) = tasks.get_mut(new_parent_id) {
                if !parent.required_children.iter().any(|id| id == task_id) {
                    parent.required_children.push(task_id.clone());
                    parent.updated_at = UtcMillis::now();
                }
            }
        }

        Ok(())
    }

    /// 更新任务状态（不做迁移合法性校验，兼容内部传播场景）。
    pub fn update_status(&self, task_id: &TaskId, status: TaskStatus) -> DomainResult<()> {
        let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        if status == TaskStatus::Completed
            && task.status != TaskStatus::Completed
            && task_requires_delivery_evidence(task)
            && task.evidence_refs.is_empty()
        {
            return Err(DomainError::Validation {
                message: format!("任务 {task_id} 在完成前必须写入 evidence_refs"),
            });
        }
        let old_status = task.status;
        task.status = status;
        task.updated_at = UtcMillis::now();
        let cloned_task = task.clone();
        drop(tasks);
        let callback = self
            .on_status_change
            .lock()
            .expect("on_status_change lock poisoned");
        if let Some(ref cb) = *callback {
            cb(task_id, old_status, status, cloned_task);
        }
        drop(callback);
        self.fire_checkpoint();
        Ok(())
    }

    /// 更新任务状态，带状态迁移合法性校验。
    /// 拒绝不合法的迁移并返回 `DomainError::InvalidTransition`。
    pub fn update_status_checked(
        &self,
        task_id: &TaskId,
        new_status: TaskStatus,
    ) -> DomainResult<()> {
        let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        let old_status = task.status;
        if !is_valid_transition(old_status, new_status) {
            return Err(DomainError::InvalidState {
                message: format!("非法状态迁移: {:?} -> {:?}", old_status, new_status),
            });
        }
        // G9: Policy freeze — snapshot policy on Draft→Ready transition.
        if old_status == TaskStatus::Pending && new_status == TaskStatus::Pending {
            if task.policy_snapshot.is_none() {
                task.policy_snapshot = Some(default_frozen_policy());
            }
        }
        if new_status == TaskStatus::Completed
            && old_status != TaskStatus::Completed
            && task_requires_delivery_evidence(task)
            && task.evidence_refs.is_empty()
        {
            return Err(DomainError::Validation {
                message: format!("任务 {task_id} 在完成前必须写入 evidence_refs"),
            });
        }
        task.status = new_status;
        task.updated_at = UtcMillis::now();
        let cloned_task = task.clone();
        drop(tasks);
        let callback = self
            .on_status_change
            .lock()
            .expect("on_status_change lock poisoned");
        if let Some(ref cb) = *callback {
            cb(task_id, old_status, new_status, cloned_task);
        }
        drop(callback);
        self.fire_checkpoint();
        Ok(())
    }

    pub fn set_output_refs(&self, task_id: &TaskId, output_refs: Vec<String>) {
        let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
        if let Some(task) = tasks.get_mut(task_id) {
            task.output_refs = output_refs;
            task.updated_at = UtcMillis::now();
        }
    }

    pub fn set_evidence_refs(&self, task_id: &TaskId, evidence_refs: Vec<String>) {
        let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
        if let Some(task) = tasks.get_mut(task_id) {
            task.evidence_refs = evidence_refs;
            task.updated_at = UtcMillis::now();
        }
    }

    pub fn append_input_ref(&self, task_id: &TaskId, input_ref: String) -> DomainResult<()> {
        let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        task.input_refs.push(input_ref);
        task.updated_at = UtcMillis::now();
        Ok(())
    }

    pub fn append_required_child(
        &self,
        task_id: &TaskId,
        child_task_id: &TaskId,
    ) -> DomainResult<()> {
        let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        if !task.required_children.iter().any(|id| id == child_task_id) {
            task.required_children.push(child_task_id.clone());
            task.updated_at = UtcMillis::now();
        }
        Ok(())
    }

    pub fn update_task_goal(&self, task_id: &TaskId, goal: String) -> DomainResult<()> {
        let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        task.goal = goal;
        task.updated_at = UtcMillis::now();
        Ok(())
    }

    /// 递增任务的 retry_count。
    pub fn increment_retry_count(&self, task_id: &TaskId) {
        let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
        if let Some(task) = tasks.get_mut(task_id) {
            task.retry_count += 1;
            task.updated_at = UtcMillis::now();
        }
    }

    /// BFS 收集以 root_id 为根的整棵子树中所有任务 ID。
    pub fn collect_subtree_ids(&self, root_id: &TaskId) -> Vec<TaskId> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let children = child_index_from_tasks(&tasks);
        let mut all_ids: Vec<TaskId> = Vec::new();
        let mut queue: Vec<TaskId> = vec![root_id.clone()];
        while let Some(current) = queue.pop() {
            if !tasks.contains_key(&current) {
                continue;
            }
            all_ids.push(current.clone());
            if let Some(child_ids) = children.get(&current) {
                queue.extend(child_ids.iter().cloned());
            }
        }
        all_ids
    }

    /// 获取某个父任务的所有子任务。
    pub fn get_children(&self, parent_id: &TaskId) -> Vec<Task> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let mut children: Vec<Task> = tasks
            .values()
            .filter(|task| task.parent_task_id.as_ref() == Some(parent_id))
            .cloned()
            .collect();
        // HashMap::values 顺序非确定，统一按 (created_at, task_id) 排序，
        // 保证调用方拿到的子任务序列稳定。
        children.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.task_id.as_str().cmp(b.task_id.as_str()))
        });
        children
    }

    pub fn has_validation_dependent(&self, task_id: &TaskId) -> bool {
        self.tasks
            .read()
            .expect("tasks read lock poisoned")
            .values()
            .any(|task| {
                task.kind == TaskKind::LocalAgent
                    && (task.parent_task_id.as_ref() == Some(task_id)
                        || task.dependency_ids.iter().any(|dep_id| dep_id == task_id))
            })
    }

    /// 获取根任务下所有处于 Pending 状态且依赖已满足的叶子任务。
    pub fn get_runnable_leaves(&self, root_task_id: &TaskId) -> Vec<Task> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let children = child_index_from_tasks(&tasks);

        // Collect all task_ids that belong to this root
        let mut all_ids: Vec<TaskId> = Vec::new();
        let mut queue: Vec<TaskId> = vec![root_task_id.clone()];
        while let Some(current) = queue.pop() {
            all_ids.push(current.clone());
            if !tasks.contains_key(&current) {
                continue;
            }
            if let Some(child_ids) = children.get(&current) {
                queue.extend(child_ids.iter().cloned());
            }
        }

        // A leaf is a task with no children, or whose children are all terminal.
        let leaves: Vec<&Task> = all_ids
            .iter()
            .filter_map(|id| tasks.get(id))
            .filter(|task| {
                children
                    .get(&task.task_id)
                    .map(|child_ids| {
                        child_ids
                            .iter()
                            .all(|cid| tasks.get(cid).is_some_and(|c| is_terminal_status(c.status)))
                    })
                    .unwrap_or(true)
            })
            .collect();

        // v2 runnable：pending 且所有依赖已完成。父子编排约束由 SpawnGraph/Coordinator
        // 管理，TaskStore 只维护最小执行事实。
        leaves
            .into_iter()
            .filter(|task| {
                task.status == TaskStatus::Pending
                    && task.dependency_ids.iter().all(|dep_id| {
                        tasks
                            .get(dep_id)
                            .is_some_and(|dep| dep.status == TaskStatus::Completed)
                    })
                    && Self::ancestor_chain_allows_dispatch_inner(task, &tasks)
            })
            .cloned()
            .collect()
    }

    fn ancestor_chain_allows_dispatch_inner(task: &Task, tasks: &HashMap<TaskId, Task>) -> bool {
        let mut current = task.parent_task_id.as_ref();
        while let Some(pid) = current {
            if let Some(parent) = tasks.get(pid) {
                if parent.status != TaskStatus::Completed {
                    return false;
                }
                if parent.dependency_ids.iter().any(|dependency_id| {
                    !tasks
                        .get(dependency_id)
                        .is_some_and(|dependency| dependency.status == TaskStatus::Completed)
                }) {
                    return false;
                }
                current = parent.parent_task_id.as_ref();
            } else {
                break;
            }
        }
        true
    }

    /// 删除单个任务并清理所有关联的索引和租约。
    pub fn remove_task(&self, task_id: &TaskId) -> Option<Task> {
        let removed = self
            .tasks
            .write()
            .expect("tasks write lock poisoned")
            .remove(task_id);

        if let Some(ref task) = removed {
            // Remove from mission index
            if let Ok(mut mission_index) = self.mission_index.write() {
                if let Some(ids) = mission_index.get_mut(&task.mission_id) {
                    ids.retain(|id| id != task_id);
                }
            }
            // Revoke any active leases for this task
            if let Ok(mut leases) = self.leases.write() {
                for lease in leases.values_mut() {
                    if lease.task_id == *task_id && lease.lease_status == TaskLeaseState::Active {
                        lease.lease_status = TaskLeaseState::Revoked;
                    }
                }
            }
            if let Ok(mut tasks) = self.tasks.write() {
                let now = UtcMillis::now();
                for parent in tasks.values_mut() {
                    if parent.required_children.iter().any(|id| id == task_id) {
                        parent.required_children.retain(|id| id != task_id);
                        parent.updated_at = now;
                    }
                }
            }
        }

        removed
    }

    /// 清除所有任务、租约及索引。
    pub fn clear_all(&self) {
        self.tasks
            .write()
            .expect("tasks write lock poisoned")
            .clear();
        self.leases
            .write()
            .expect("leases write lock poisoned")
            .clear();
        self.mission_index
            .write()
            .expect("mission_index write lock poisoned")
            .clear();
    }

    /// 获取指定任务的所有任务。
    pub fn get_tasks_by_mission(&self, mission_id: &MissionId) -> Vec<Task> {
        let mission_index = self
            .mission_index
            .read()
            .expect("mission_index read lock poisoned");
        let tasks = self.tasks.read().expect("tasks read lock poisoned");

        mission_index
            .get(mission_id)
            .map(|task_ids| {
                task_ids
                    .iter()
                    .filter_map(|id| tasks.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 构建任务投影视图。
    pub fn build_projection(&self, root_task_id: &TaskId) -> Option<TaskProjection> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let children = child_index_from_tasks(&tasks);

        let root_task = tasks.get(root_task_id)?.clone();

        let mut ordered_task_ids: Vec<TaskId> = Vec::new();
        let mut stack: Vec<TaskId> = vec![root_task_id.clone()];
        let mut visited: HashSet<TaskId> = HashSet::new();
        while let Some(current) = stack.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if tasks.contains_key(&current) {
                ordered_task_ids.push(current.clone());
                if let Some(child_ids) = children.get(&current) {
                    for child_id in child_ids.iter().rev() {
                        stack.push(child_id.clone());
                    }
                }
            }
        }
        let all_tasks: Vec<&Task> = ordered_task_ids
            .iter()
            .filter_map(|task_id| tasks.get(task_id))
            .collect();
        let projection_tasks: Vec<Task> = all_tasks.iter().map(|task| (*task).clone()).collect();
        let active_tasks: Vec<&Task> = all_tasks
            .iter()
            .copied()
            .filter(|task| task.status != TaskStatus::Killed)
            .collect();

        let running_tasks: Vec<TaskId> = active_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Running)
            .map(|t| t.task_id.clone())
            .collect();

        let pending_tasks: Vec<TaskId> = active_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Pending)
            .map(|t| t.task_id.clone())
            .collect();

        let completed_task_ids: Vec<TaskId> = active_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Completed)
            .map(|t| t.task_id.clone())
            .collect();

        let failed_task_ids: Vec<TaskId> = active_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Failed)
            .map(|t| t.task_id.clone())
            .collect();

        let killed_task_ids: Vec<TaskId> = all_tasks
            .iter()
            .copied()
            .filter(|t| t.status == TaskStatus::Killed)
            .map(|t| t.task_id.clone())
            .collect();

        let total_tasks = active_tasks.len() as u32;
        let pending_count = pending_tasks.len() as u32;
        let completed_count = completed_task_ids.len() as u32;
        let settled_tasks = active_tasks
            .iter()
            .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Killed))
            .count() as u32;
        let failed_count = failed_task_ids.len() as u32;
        let killed_count = killed_task_ids.len() as u32;
        let running_count = running_tasks.len() as u32;

        let aggregate_status = if root_task.status == TaskStatus::Killed {
            TaskStatus::Killed
        } else if failed_count > 0 {
            TaskStatus::Failed
        } else if running_count > 0 {
            TaskStatus::Running
        } else if total_tasks > 0 && settled_tasks == total_tasks {
            TaskStatus::Completed
        } else {
            root_task.status
        };

        let execution_mode = match root_task
            .policy_snapshot
            .as_ref()
            .map(|policy| policy.task_tier)
            .unwrap_or(TaskTier::ExecutionChain)
        {
            TaskTier::ExecutionChain => "execution_chain".to_string(),
            TaskTier::LongMission => "long_mission".to_string(),
        };
        let runner_status = match aggregate_status {
            TaskStatus::Running => "running".to_string(),
            TaskStatus::Completed => "completed".to_string(),
            TaskStatus::Failed => "error".to_string(),
            TaskStatus::Killed => "killed".to_string(),
            TaskStatus::Pending => "pending".to_string(),
        };
        let display_status = if root_task.status == TaskStatus::Killed {
            "已终止".to_string()
        } else if total_tasks == 0 {
            "待启动".to_string()
        } else {
            let pct = (settled_tasks as f32 / total_tasks as f32 * 100.0).round() as u32;
            if settled_tasks == total_tasks {
                "全部完成".to_string()
            } else if failed_count > 0 {
                format!("{}% 已完成，{} 项失败", pct, failed_count)
            } else if running_count > 0 {
                format!("{}% 已完成，{} 项执行中", pct, running_count)
            } else if pending_count > 0 {
                format!("{}% 已完成，{} 项待执行", pct, pending_count)
            } else {
                format!("{}% 已完成", pct)
            }
        };

        Some(TaskProjection {
            root_task,
            tasks: projection_tasks,
            running_tasks,
            pending_tasks,
            completed_tasks: completed_task_ids,
            failed_tasks: failed_task_ids,
            killed_tasks: killed_task_ids,
            progress_summary: ProgressSummary {
                total_tasks,
                pending_tasks: pending_count,
                running_tasks: running_count,
                completed_tasks: completed_count,
                settled_tasks,
                failed_tasks: failed_count,
                killed_tasks: killed_count,
            },
            aggregate_status,
            display_status,
            execution_mode,
            runner_status,
            has_recoverable_chain: false,
            recoverable_branch_count: 0,
        })
    }

    /// 聚合交付包：从 TaskProjection 和已完成任务中提取
    /// 目标、范围、变更、证据、核验结果等交付摘要。
    pub fn build_delivery_package(&self, root_task_id: &TaskId) -> Option<serde_json::Value> {
        let projection = self.build_projection(root_task_id)?;
        let active_tasks: Vec<&Task> = projection
            .tasks
            .iter()
            .filter(|task| task.status != TaskStatus::Killed)
            .collect();

        let completed_tasks: Vec<&Task> = active_tasks
            .iter()
            .copied()
            .filter(|t| t.status == TaskStatus::Completed)
            .collect();

        let mut file_changes: Vec<String> = Vec::new();
        let mut evidence_list: Vec<String> = Vec::new();
        let mut verification_results: Vec<serde_json::Value> = Vec::new();
        let mut execution_records: Vec<serde_json::Value> = Vec::new();

        for task in &active_tasks {
            for output in &task.output_refs {
                if output_ref_is_file_change(output) && !file_changes.contains(output) {
                    file_changes.push(output.clone());
                }
            }
            for evidence in &task.evidence_refs {
                if !evidence_list.contains(evidence) {
                    evidence_list.push(evidence.clone());
                }
            }
            if task.kind == TaskKind::LocalAgent && task.status == TaskStatus::Completed {
                verification_results.push(serde_json::json!({
                    "task_id": task.task_id.to_string(),
                    "title": task.title,
                    "result": "passed",
                    "evidence": task.evidence_refs,
                }));
            }
            if task.kind == TaskKind::LocalAgent && task.status == TaskStatus::Completed {
                execution_records.push(serde_json::json!({
                    "task_id": task.task_id.to_string(),
                    "title": task.title,
                    "goal": task.goal,
                    "evidence": task.evidence_refs,
                }));
            }
        }

        let remaining_risks: Vec<String> = active_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Failed || t.status == TaskStatus::Pending)
            .map(|t| format!("{}: {:?} - {}", t.task_id, t.status, t.title))
            .collect();

        Some(serde_json::json!({
            "goal": projection.root_task.goal,
            "scope": projection.root_task.workspace_scope,
            "execution_mode": projection.execution_mode,
            "aggregate_status": projection.aggregate_status,
            "progress": {
                "total": projection.progress_summary.total_tasks,
                "completed": projection.progress_summary.settled_tasks,
                "failed": projection.progress_summary.failed_tasks,
                "running": projection.progress_summary.running_tasks,
                "pending": projection.progress_summary.pending_tasks,
                "killed": projection.progress_summary.killed_tasks,
            },
            "file_changes": file_changes,
            "evidence_list": evidence_list,
            "verification_results": verification_results,
            "execution_records": execution_records,
            "remaining_risks": remaining_risks,
            "completed_task_count": completed_tasks.len(),
        }))
    }

    /// 创建执行租约。
    pub fn insert_lease(&self, lease: TaskLease) {
        let lease_id = lease.lease_id.clone();
        self.leases
            .write()
            .expect("leases write lock poisoned")
            .insert(lease_id, lease);
    }

    /// 获取指定任务的活跃租约。
    pub fn get_active_lease(&self, task_id: &TaskId) -> Option<TaskLease> {
        self.leases
            .read()
            .expect("leases read lock poisoned")
            .values()
            .find(|lease| lease.task_id == *task_id && lease.lease_status == TaskLeaseState::Active)
            .cloned()
    }

    /// 为任务授予新的执行租约。如果任务已有活跃租约则返回 None。
    pub fn grant_lease(
        &self,
        task_id: &TaskId,
        root_task_id: &TaskId,
        worker_id: &WorkerId,
        role: &str,
        duration_ms: u64,
    ) -> Option<TaskLease> {
        let mut leases = self.leases.write().expect("leases write lock poisoned");

        // Check if task already has an active lease
        let has_active = leases
            .values()
            .any(|l| l.task_id == *task_id && l.lease_status == TaskLeaseState::Active);
        if has_active {
            return None;
        }

        let now = UtcMillis::now();
        let counter = LEASE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let lease_id = LeaseId::new(format!("lease-{}-{}", now.0, counter));

        let lease = TaskLease {
            lease_id: lease_id.clone(),
            task_id: task_id.clone(),
            root_task_id: root_task_id.clone(),
            worker_id: worker_id.clone(),
            role: role.to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0 + duration_ms),
            heartbeat_at: now,
            lease_status: TaskLeaseState::Active,
        };

        leases.insert(lease_id, lease.clone());
        Some(lease)
    }

    /// 标记活跃租约为已完成。找不到匹配的活跃租约时返回 false。
    pub fn complete_lease(&self, task_id: &TaskId, lease_id: &LeaseId) -> bool {
        let mut leases = self.leases.write().expect("leases write lock poisoned");
        if let Some(lease) = leases.get_mut(lease_id) {
            if lease.task_id == *task_id && lease.lease_status == TaskLeaseState::Active {
                lease.lease_status = TaskLeaseState::Completed;
                return true;
            }
        }
        false
    }

    /// 撤销活跃租约。
    pub fn revoke_lease(&self, task_id: &TaskId, lease_id: &LeaseId) -> bool {
        let mut leases = self.leases.write().expect("leases write lock poisoned");
        if let Some(lease) = leases.get_mut(lease_id) {
            if lease.task_id == *task_id && lease.lease_status == TaskLeaseState::Active {
                lease.lease_status = TaskLeaseState::Revoked;
                return true;
            }
        }
        false
    }

    /// 更新活跃租约的心跳时间。
    pub fn heartbeat_lease(&self, task_id: &TaskId, lease_id: &LeaseId) -> bool {
        let mut leases = self.leases.write().expect("leases write lock poisoned");
        if let Some(lease) = leases.get_mut(lease_id) {
            if lease.task_id == *task_id && lease.lease_status == TaskLeaseState::Active {
                lease.heartbeat_at = UtcMillis::now();
                return true;
            }
        }
        false
    }

    /// 扫描指定 root_task_id 下的租约，返回已过期（Active 且 expires_at < now）的租约。
    /// 不修改其状态，由调用方决定恢复策略后处理。
    pub fn collect_expired_leases(&self, root_task_id: &TaskId) -> Vec<(TaskId, LeaseId)> {
        let now = UtcMillis::now();
        let leases = self.leases.read().expect("leases read lock poisoned");
        leases
            .values()
            .filter(|l| {
                l.lease_status == TaskLeaseState::Active
                    && l.expires_at < now
                    && l.root_task_id == *root_task_id
            })
            .map(|l| (l.task_id.clone(), l.lease_id.clone()))
            .collect()
    }

    /// 获取指定 root_task_id 下所有未过期的活跃租约（Active 且 expires_at >= now）。
    pub fn collect_active_leases(&self, root_task_id: &TaskId) -> Vec<(TaskId, LeaseId)> {
        let now = UtcMillis::now();
        let leases = self.leases.read().expect("leases read lock poisoned");
        leases
            .values()
            .filter(|l| {
                l.lease_status == TaskLeaseState::Active
                    && l.expires_at >= now
                    && l.root_task_id == *root_task_id
            })
            .map(|l| (l.task_id.clone(), l.lease_id.clone()))
            .collect()
    }

    /// 获取所有未过期的活跃租约（不区分 root_task_id）。
    /// 仅在 reconcile / checkpoint restore 等全量收敛场景使用。
    pub fn collect_all_active_leases(&self) -> Vec<(TaskId, LeaseId)> {
        let now = UtcMillis::now();
        let leases = self.leases.read().expect("leases read lock poisoned");
        leases
            .values()
            .filter(|l| l.lease_status == TaskLeaseState::Active && l.expires_at >= now)
            .map(|l| (l.task_id.clone(), l.lease_id.clone()))
            .collect()
    }

    /// 在进程重启后收敛易失执行态。
    ///
    /// 本轮只保证逻辑续接，不恢复进程内瞬时现场，因此 restore 后的 Active lease
    /// 仍然不能被视为真实运行态。这里会结合 `WorkerRuntime` 的最小快照一起决定
    /// 哪些 branch 属于当前可恢复执行树，并把对应子树统一收口到 `Failed`。
    pub fn reconcile_volatile_runtime_after_restore(
        &self,
        worker_snapshot: &WorkerRuntimeDurableSnapshot,
    ) -> (usize, usize) {
        let active_leases = self.collect_all_active_leases();
        let recoverable_branch_task_ids: Vec<_> = worker_snapshot
            .branches
            .iter()
            .filter_map(|branch| {
                let task = self.get_task(&branch.task_id)?;
                if matches!(
                    task.status,
                    TaskStatus::Pending | TaskStatus::Running | TaskStatus::Failed
                ) {
                    Some(branch.task_id.clone())
                } else {
                    None
                }
            })
            .collect();
        if active_leases.is_empty() && recoverable_branch_task_ids.is_empty() {
            return (0, 0);
        }

        let mut affected_roots = HashSet::new();
        for (task_id, lease_id) in &active_leases {
            if let Some(task) = self.get_task(task_id) {
                affected_roots.insert(task.root_task_id);
            }
            self.revoke_lease(task_id, lease_id);
        }
        for task_id in &recoverable_branch_task_ids {
            if let Some(task) = self.get_task(task_id) {
                affected_roots.insert(task.root_task_id);
            }
        }

        let mut failed_count = 0usize;
        for root_task_id in affected_roots {
            for task_id in self.collect_subtree_ids(&root_task_id) {
                let Some(task) = self.get_task(&task_id) else {
                    continue;
                };
                if matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
                    if self.update_status(&task_id, TaskStatus::Failed).is_ok() {
                        failed_count += 1;
                    }
                }
            }
        }

        (active_leases.len(), failed_count)
    }

    /// 获取指定 worker 的所有活跃租约。
    pub fn get_leases_by_worker(&self, worker_id: &WorkerId) -> Vec<TaskLease> {
        let leases = self.leases.read().expect("leases read lock poisoned");
        leases
            .values()
            .filter(|l| l.worker_id == *worker_id && l.lease_status == TaskLeaseState::Active)
            .cloned()
            .collect()
    }

    // ------------------------------------------------------------------
    // Checkpoint / Restore
    // ------------------------------------------------------------------

    /// Serialize all tasks and leases to a JSON value for checkpointing.
    pub fn checkpoint(&self) -> serde_json::Value {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let leases = self.leases.read().expect("leases read lock poisoned");
        serde_json::json!({
            "tasks": tasks.values().cloned().collect::<Vec<Task>>(),
            "leases": leases.values().cloned().collect::<Vec<TaskLease>>(),
        })
    }

    /// Restore a TaskStore from a checkpoint JSON value.
    ///
    /// Rebuilds the mission index from task data.  The `on_status_change`
    /// callback is NOT restored — callers must re-attach it after restore if needed.
    pub fn restore(data: &serde_json::Value) -> Self {
        let tasks: Vec<Task> = data
            .get("tasks")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let leases: Vec<TaskLease> = data
            .get("leases")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let store = Self::new();

        // Re-insert tasks (which rebuilds mission_index).
        for task in tasks {
            store.insert_task(task);
        }

        // Re-insert leases directly.
        {
            let mut lease_map = store.leases.write().expect("leases write lock poisoned");
            for lease in leases {
                lease_map.insert(lease.lease_id.clone(), lease);
            }
        }
        // Advance LEASE_COUNTER past any restored lease IDs to avoid collisions.
        {
            let lease_map = store.leases.read().expect("leases read lock poisoned");
            let max_counter = lease_map
                .keys()
                .filter_map(|id| {
                    let s = id.to_string();
                    // lease IDs have the form "lease-{timestamp}-{counter}"
                    s.rsplit('-')
                        .next()
                        .and_then(|part| part.parse::<u64>().ok())
                })
                .max()
                .unwrap_or(0);
            let current = LEASE_COUNTER.load(Ordering::Relaxed);
            if max_counter >= current {
                LEASE_COUNTER.store(max_counter + 1, Ordering::Relaxed);
            }
        }

        store
    }

    // ------------------------------------------------------------------
    // G8.5: Dynamic dependency management
    // ------------------------------------------------------------------

    /// 运行时添加依赖关系。不允许自依赖或对已完成任务的依赖。
    pub fn add_dependency(&self, task_id: &TaskId, dependency_id: &TaskId) -> DomainResult<()> {
        if task_id == dependency_id {
            return Err(DomainError::InvalidState {
                message: format!("任务 {} 不能依赖自身", task_id),
            });
        }
        let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| DomainError::InvalidState {
                message: format!("任务 {} 不存在", task_id),
            })?;
        if task.dependency_ids.contains(dependency_id) {
            return Ok(());
        }
        task.dependency_ids.push(dependency_id.clone());
        task.updated_at = UtcMillis::now();
        Ok(())
    }

    /// 运行时移除依赖关系。
    pub fn remove_dependency(&self, task_id: &TaskId, dependency_id: &TaskId) -> DomainResult<()> {
        let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| DomainError::InvalidState {
                message: format!("任务 {} 不存在", task_id),
            })?;
        task.dependency_ids.retain(|id| id != dependency_id);
        task.updated_at = UtcMillis::now();
        Ok(())
    }

    /// 获取指定任务的所有依赖。
    pub fn get_dependencies(&self, task_id: &TaskId) -> Vec<TaskId> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        tasks
            .get(task_id)
            .map(|t| t.dependency_ids.clone())
            .unwrap_or_default()
    }

    // ------------------------------------------------------------------
    // G8: Graph structural validation (design 3.2 / 4.x)
    // ------------------------------------------------------------------

    /// Insert a task with structural validation: checks for cycles and
    /// legal parent-child kind relationships.
    pub fn insert_task_validated(&self, task: Task) -> DomainResult<()> {
        // Validate parent-child kind hierarchy.
        if let Some(ref parent_id) = task.parent_task_id {
            if let Some(parent) = self.get_task(parent_id) {
                if !is_valid_parent_child_kind(parent.kind, task.kind) {
                    return Err(DomainError::InvalidState {
                        message: format!(
                            "非法父子关系: {:?} 不能包含 {:?} 子节点",
                            parent.kind, task.kind
                        ),
                    });
                }
            }
        }
        // Check for dependency cycles.
        for dep_id in &task.dependency_ids {
            if *dep_id == task.task_id {
                return Err(DomainError::InvalidState {
                    message: format!("任务 {} 不能依赖自身", task.task_id),
                });
            }
        }
        self.insert_task(task);
        Ok(())
    }

    /// Atomically write a checkpoint to a file (write .tmp, then rename).
    pub fn checkpoint_to_file(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = self.checkpoint();
        let content = serde_json::to_vec_pretty(&data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let temp_path = path.with_extension(format!(
            "json.{}.{}.tmp",
            std::process::id(),
            CHECKPOINT_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&temp_path, content)?;
        if let Err(error) = std::fs::rename(&temp_path, path) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(error);
        }
        Ok(())
    }

    /// Restore a TaskStore from a checkpoint file.
    ///
    /// Returns `Ok(None)` if the file does not exist.
    pub fn restore_from_file(path: &Path) -> std::io::Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(path)?;
        let data: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Some(Self::restore(&data)))
    }
}

impl Default for TaskStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Task System v2 L11 五态迁移表。
pub fn is_valid_transition(from: TaskStatus, to: TaskStatus) -> bool {
    use TaskStatus::*;
    if from == to {
        return true;
    }
    matches!(
        (from, to),
        (Pending, Running)
            | (Pending, Killed)
            | (Running, Completed)
            | (Running, Failed)
            | (Running, Killed)
    )
}

fn is_terminal_status(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
    )
}

/// TaskStore 不再限制固定父子层级，具体编排约束交给 SpawnGraph/Coordinator。
fn is_valid_parent_child_kind(_parent: TaskKind, _child: TaskKind) -> bool {
    true
}
