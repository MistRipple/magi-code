use magi_core::{
    DomainError, DomainResult, LeaseId, MissionId, ProgressSummary, Task, TaskId, TaskKind,
    TaskPolicy, TaskProjection, TaskStatus, UtcMillis, WorkerId,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskContextEntry {
    pub context_ref: String,
    pub task_id: TaskId,
    pub mission_id: MissionId,
    pub content: String,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

/// 任务图的内存存储，维护任务、租约及其索引。
pub struct TaskStore {
    tasks: RwLock<HashMap<TaskId, Task>>,
    leases: RwLock<HashMap<LeaseId, TaskLease>>,
    context_entries: RwLock<HashMap<String, TaskContextEntry>>,
    /// 索引: mission_id -> task_ids
    mission_index: RwLock<HashMap<MissionId, Vec<TaskId>>>,
    /// 索引: parent_task_id -> child task_ids
    children_index: RwLock<HashMap<TaskId, Vec<TaskId>>>,
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
        repair_limit: 1,
        validation_profile: None,
        checkpoint_mode: "turn".to_string(),
        background_allowed: false,
        escalation_conditions: Vec::new(),
    }
}

fn task_requires_delivery_evidence(task: &Task) -> bool {
    matches!(
        task.kind,
        TaskKind::Action | TaskKind::Validation | TaskKind::Repair
    ) && task
        .policy_snapshot
        .as_ref()
        .is_some_and(|policy| policy.validation_profile.is_some() && policy.background_allowed)
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
        || lower.starts_with("repair://")
        || lower.starts_with("tool://")
        || lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("decision_")
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

impl TaskStore {
    pub fn new() -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            leases: RwLock::new(HashMap::new()),
            context_entries: RwLock::new(HashMap::new()),
            mission_index: RwLock::new(HashMap::new()),
            children_index: RwLock::new(HashMap::new()),
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
            context_entries: RwLock::new(HashMap::new()),
            mission_index: RwLock::new(HashMap::new()),
            children_index: RwLock::new(HashMap::new()),
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
        let parent_task_id = task.parent_task_id.clone();

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

        if let Some(parent_id) = parent_task_id {
            self.children_index
                .write()
                .expect("children_index write lock poisoned")
                .entry(parent_id)
                .or_default()
                .push(task_id);
        }
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

        if let Some(old_parent_id) = old_parent {
            let mut children_index = self
                .children_index
                .write()
                .expect("children_index write lock poisoned");
            if let Some(children) = children_index.get_mut(&old_parent_id) {
                children.retain(|id| id != task_id);
                if children.is_empty() {
                    children_index.remove(&old_parent_id);
                }
            }
        }

        self.children_index
            .write()
            .expect("children_index write lock poisoned")
            .entry(new_parent_id.clone())
            .or_default()
            .push(task_id.clone());
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
        if old_status == TaskStatus::Draft && new_status == TaskStatus::Ready {
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

    pub fn append_context_ref(&self, task_id: &TaskId, context_ref: String) -> DomainResult<()> {
        let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        if !task.context_refs.iter().any(|item| item == &context_ref) {
            task.context_refs.push(context_ref);
            task.updated_at = UtcMillis::now();
        }
        Ok(())
    }

    pub fn append_context_entry(
        &self,
        task_id: &TaskId,
        context_ref: String,
        content: String,
    ) -> DomainResult<TaskContextEntry> {
        let trimmed_ref = context_ref.trim();
        if trimmed_ref.is_empty() {
            return Err(DomainError::Validation {
                message: "context_ref 不能为空".to_string(),
            });
        }
        let trimmed_content = content.trim();
        if trimmed_content.is_empty() {
            return Err(DomainError::Validation {
                message: "context content 不能为空".to_string(),
            });
        }

        let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        let now = UtcMillis::now();
        let context_ref = trimmed_ref.to_string();
        if !task.context_refs.iter().any(|item| item == &context_ref) {
            task.context_refs.push(context_ref.clone());
        }
        task.updated_at = now;

        let entry = TaskContextEntry {
            context_ref: context_ref.clone(),
            task_id: task.task_id.clone(),
            mission_id: task.mission_id.clone(),
            content: trimmed_content.to_string(),
            created_at: now,
            updated_at: now,
        };
        self.context_entries
            .write()
            .expect("context_entries write lock poisoned")
            .insert(context_ref, entry.clone());
        drop(tasks);
        self.fire_checkpoint();
        Ok(entry)
    }

    pub fn context_entries_for_refs(&self, refs: &[String]) -> Vec<TaskContextEntry> {
        let entries = self
            .context_entries
            .read()
            .expect("context_entries read lock poisoned");
        refs.iter()
            .filter_map(|context_ref| entries.get(context_ref).cloned())
            .collect()
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

    pub fn resolve_decision(
        &self,
        decision_task_id: &TaskId,
        chosen_option: &str,
        evidence: Option<serde_json::Value>,
    ) -> Result<(), String> {
        let chosen_option = chosen_option.trim();
        if chosen_option.is_empty() {
            return Err("chosen option must not be empty".to_string());
        }
        let decision = self
            .get_task(decision_task_id)
            .ok_or_else(|| format!("decision task {decision_task_id} not found"))?;
        if decision.kind != TaskKind::Decision {
            return Err(format!("{decision_task_id} is not a Decision task"));
        }
        if decision.status != TaskStatus::AwaitingApproval {
            return Err(format!(
                "decision {decision_task_id} is not AwaitingApproval"
            ));
        }
        let payload = decision
            .decision_payload
            .as_ref()
            .ok_or_else(|| format!("decision {decision_task_id} missing decision payload"))?;
        let options = payload
            .get("options")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| format!("decision {decision_task_id} has no options"))?;
        if options.is_empty() {
            return Err(format!("decision {decision_task_id} has no options"));
        }
        if !options.iter().any(|option| {
            option.get("option_id").and_then(serde_json::Value::as_str) == Some(chosen_option)
        }) {
            return Err(format!(
                "option {chosen_option} is not valid for decision {decision_task_id}"
            ));
        }

        self.set_output_refs(
            decision_task_id,
            vec![format!("decision_chosen:{chosen_option}")],
        );
        if let Some(ref ev) = evidence {
            self.set_evidence_refs(
                decision_task_id,
                vec![serde_json::to_string(ev).unwrap_or_default()],
            );
        }
        self.update_status(decision_task_id, TaskStatus::Completed)
            .map_err(|e| format!("failed to complete decision: {e}"))?;
        let target_task_id = payload
            .get("target_task_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(TaskId::new);

        match chosen_option {
            "abort" | "cancel" => {
                self.cancel_open_subtree(&decision.root_task_id, Some(decision_task_id))?;
            }
            "skip" => {
                if let Some(target_task_id) = target_task_id.as_ref() {
                    self.skip_open_subtree(target_task_id, Some(decision_task_id))?;
                } else if let Some(ref parent_id) = decision.parent_task_id {
                    self.skip_open_subtree(parent_id, Some(decision_task_id))?;
                }
            }
            _ => {
                self.release_decision_gate(&decision, decision_task_id, target_task_id.as_ref())?;
            }
        }
        Ok(())
    }

    fn release_decision_gate(
        &self,
        decision: &Task,
        decision_task_id: &TaskId,
        target_task_id: Option<&TaskId>,
    ) -> Result<(), String> {
        if let Some(target_task_id) = target_task_id {
            self.release_open_branch(target_task_id, decision_task_id)?;
        } else if let Some(ref parent_id) = decision.parent_task_id {
            self.release_open_branch(parent_id, decision_task_id)?;
        }

        let all_tasks = self.get_tasks_by_mission(&decision.mission_id);
        for task in all_tasks {
            if task.task_id == *decision_task_id || task.kind == TaskKind::Decision {
                continue;
            }
            if (task.status == TaskStatus::Blocked || task.status == TaskStatus::AwaitingApproval)
                && task
                    .dependency_ids
                    .iter()
                    .any(|dependency_id| dependency_id == decision_task_id)
                && task.dependency_ids.iter().all(|dependency_id| {
                    self.get_task(dependency_id)
                        .is_some_and(|dependency| dependency.status == TaskStatus::Completed)
                })
            {
                self.update_status(&task.task_id, TaskStatus::Ready)
                    .map_err(|e| format!("failed to release decision dependent: {e}"))?;
            }
        }

        Ok(())
    }

    fn release_open_branch(
        &self,
        branch_task_id: &TaskId,
        decision_task_id: &TaskId,
    ) -> Result<(), String> {
        for task_id in self.collect_subtree_ids(branch_task_id) {
            if task_id == *decision_task_id {
                continue;
            }
            if let Some(task) = self.get_task(&task_id) {
                if task.kind != TaskKind::Decision && task.status == TaskStatus::Blocked {
                    self.update_status(&task_id, TaskStatus::Ready)
                        .map_err(|e| {
                            format!("failed to release blocked branch task {task_id}: {e}")
                        })?;
                }
            }
        }
        Ok(())
    }

    fn cancel_open_subtree(
        &self,
        root_task_id: &TaskId,
        keep_completed_task_id: Option<&TaskId>,
    ) -> Result<(), String> {
        for task_id in self.collect_subtree_ids(root_task_id) {
            if keep_completed_task_id.is_some_and(|keep_id| keep_id == &task_id) {
                continue;
            }
            if let Some(task) = self.get_task(&task_id) {
                if !is_terminal_status(task.status) {
                    self.update_status(&task_id, TaskStatus::Cancelled)
                        .map_err(|e| format!("failed to cancel task {task_id}: {e}"))?;
                    if let Some(lease) = self.get_active_lease(&task_id) {
                        self.revoke_lease(&task_id, &lease.lease_id);
                    }
                }
            }
        }
        Ok(())
    }

    fn skip_open_subtree(
        &self,
        root_task_id: &TaskId,
        keep_completed_task_id: Option<&TaskId>,
    ) -> Result<(), String> {
        for task_id in self.collect_subtree_ids(root_task_id) {
            if keep_completed_task_id.is_some_and(|keep_id| keep_id == &task_id) {
                continue;
            }
            if let Some(task) = self.get_task(&task_id) {
                if !is_terminal_status(task.status) {
                    self.update_status(&task_id, TaskStatus::Skipped)
                        .map_err(|e| format!("failed to skip task {task_id}: {e}"))?;
                    if let Some(lease) = self.get_active_lease(&task_id) {
                        self.revoke_lease(&task_id, &lease.lease_id);
                    }
                }
            }
        }
        Ok(())
    }

    /// 递增任务的 repair_count。
    pub fn increment_repair_count(&self, task_id: &TaskId) {
        let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
        if let Some(task) = tasks.get_mut(task_id) {
            task.repair_count += 1;
            task.updated_at = UtcMillis::now();
        }
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
        let children_index = self
            .children_index
            .read()
            .expect("children_index read lock poisoned");
        let mut all_ids: Vec<TaskId> = Vec::new();
        let mut queue: Vec<TaskId> = vec![root_id.clone()];
        while let Some(current) = queue.pop() {
            all_ids.push(current.clone());
            if let Some(child_ids) = children_index.get(&current) {
                queue.extend(child_ids.iter().cloned());
            }
        }
        all_ids
    }

    /// 获取某个父任务的所有子任务。
    pub fn get_children(&self, parent_id: &TaskId) -> Vec<Task> {
        let children_index = self
            .children_index
            .read()
            .expect("children_index read lock poisoned");
        let tasks = self.tasks.read().expect("tasks read lock poisoned");

        children_index
            .get(parent_id)
            .map(|child_ids| {
                child_ids
                    .iter()
                    .filter_map(|id| tasks.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn has_validation_dependent(&self, task_id: &TaskId) -> bool {
        self.tasks
            .read()
            .expect("tasks read lock poisoned")
            .values()
            .any(|task| {
                task.kind == TaskKind::Validation
                    && (task.parent_task_id.as_ref() == Some(task_id)
                        || task.dependency_ids.iter().any(|dep_id| dep_id == task_id))
            })
    }

    /// 获取根任务下所有处于 Ready 状态且依赖已满足的叶子任务。
    pub fn get_runnable_leaves(&self, root_task_id: &TaskId) -> Vec<Task> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let children_index = self
            .children_index
            .read()
            .expect("children_index read lock poisoned");

        // Collect all task_ids that belong to this root
        let mut all_ids: Vec<TaskId> = Vec::new();
        let mut queue: Vec<TaskId> = vec![root_task_id.clone()];
        while let Some(current) = queue.pop() {
            all_ids.push(current.clone());
            if let Some(child_ids) = children_index.get(&current) {
                queue.extend(child_ids.iter().cloned());
            }
        }

        // A leaf is a task with no children, or whose children are all terminal.
        let leaves: Vec<&Task> = all_ids
            .iter()
            .filter_map(|id| tasks.get(id))
            .filter(|task| {
                children_index
                    .get(&task.task_id)
                    .map(|child_ids| {
                        child_ids
                            .iter()
                            .all(|cid| tasks.get(cid).is_some_and(|c| is_terminal_status(c.status)))
                    })
                    .unwrap_or(true)
            })
            .collect();

        // Runnable: status == Ready && all dependencies are Completed
        // && no ancestor in parent chain is Blocked/AwaitingApproval (design 4.1)
        leaves
            .into_iter()
            .filter(|task| {
                task.status == TaskStatus::Ready
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
                if matches!(
                    parent.status,
                    TaskStatus::Draft
                        | TaskStatus::Blocked
                        | TaskStatus::AwaitingApproval
                        | TaskStatus::Failed
                        | TaskStatus::Cancelled
                        | TaskStatus::Skipped
                ) {
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
            // Remove from parent's children index
            if let Some(ref parent_id) = task.parent_task_id {
                if let Ok(mut children_index) = self.children_index.write() {
                    if let Some(ids) = children_index.get_mut(parent_id) {
                        ids.retain(|id| id != task_id);
                    }
                }
            }
            // Remove this task's own children entry (if it was a parent)
            if let Ok(mut children_index) = self.children_index.write() {
                children_index.remove(task_id);
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
        self.children_index
            .write()
            .expect("children_index write lock poisoned")
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
        let children_index = self
            .children_index
            .read()
            .expect("children_index read lock poisoned");

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
                if let Some(child_ids) = children_index.get(&current) {
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
            .filter(|task| task.status != TaskStatus::Cancelled)
            .collect();

        let running_tasks: Vec<TaskId> = active_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Running)
            .map(|t| t.task_id.clone())
            .collect();

        let blocked_tasks: Vec<TaskId> = active_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Blocked)
            .map(|t| t.task_id.clone())
            .collect();

        let pending_decisions: Vec<TaskId> = active_tasks
            .iter()
            .filter(|t| t.kind == TaskKind::Decision && t.status == TaskStatus::AwaitingApproval)
            .map(|t| t.task_id.clone())
            .collect();

        // Current phase: first Running or Ready Phase child of root
        let current_phase = children_index.get(root_task_id).and_then(|child_ids| {
            child_ids.iter().find_map(|cid| {
                tasks.get(cid).and_then(|t| {
                    if t.kind == TaskKind::Phase
                        && (t.status == TaskStatus::Running || t.status == TaskStatus::Ready)
                    {
                        Some(t.title.clone())
                    } else {
                        None
                    }
                })
            })
        });

        let total_tasks = active_tasks.len() as u32;
        let completed_tasks = active_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Completed)
            .count() as u32;
        let settled_tasks = active_tasks
            .iter()
            .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped))
            .count() as u32;
        let failed_tasks = active_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Failed)
            .count() as u32;
        let running_count = running_tasks.len() as u32;
        let blocked_count = blocked_tasks.len() as u32;
        let validation_tasks: Vec<&Task> = active_tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Validation)
            .copied()
            .collect();
        let completed_validations = validation_tasks
            .iter()
            .filter(|task| task.status == TaskStatus::Completed)
            .count();
        let evidence_tasks = active_tasks
            .iter()
            .filter(|task| !task.evidence_refs.is_empty())
            .count();
        let validation_summary = if validation_tasks.is_empty() && evidence_tasks == 0 {
            None
        } else {
            Some(format!(
                "验证 {}/{}，证据 {} 项",
                completed_validations,
                validation_tasks.len(),
                evidence_tasks
            ))
        };

        // Build WorkPackage summaries (design 8.2)
        let workpackage_summaries: Vec<magi_core::WorkPackageSummary> = active_tasks
            .iter()
            .filter(|t| t.kind == magi_core::TaskKind::WorkPackage)
            .filter_map(|wp| {
                let child_ids = children_index.get(&wp.task_id)?;
                let children: Vec<&magi_core::Task> = child_ids
                    .iter()
                    .filter_map(|cid| tasks.get(cid))
                    .filter(|child| child.status != TaskStatus::Cancelled)
                    .collect();
                let total = children.len() as u32;
                if total == 0 {
                    return Some(magi_core::WorkPackageSummary {
                        task_id: wp.task_id.to_string(),
                        title: wp.title.clone(),
                        aggregate_status: wp.status.clone(),
                        display_status: format!("{:?}", wp.status),
                        progress_ratio: 0.0,
                        recent_evidence: Vec::new(),
                        recent_issues: Vec::new(),
                    });
                }
                let terminal = children
                    .iter()
                    .filter(|c| {
                        matches!(
                            c.status,
                            magi_core::TaskStatus::Completed
                                | magi_core::TaskStatus::Failed
                                | magi_core::TaskStatus::Skipped
                        )
                    })
                    .count() as u32;
                let progress_ratio = terminal as f32 / total as f32;
                let recent_evidence: Vec<String> = children
                    .iter()
                    .filter(|c| !c.evidence_refs.is_empty())
                    .flat_map(|c| c.evidence_refs.iter().cloned())
                    .collect::<std::collections::HashSet<String>>()
                    .into_iter()
                    .collect();
                let recent_issues: Vec<String> = children
                    .iter()
                    .filter(|c| {
                        matches!(
                            c.status,
                            magi_core::TaskStatus::Failed | magi_core::TaskStatus::Blocked
                        )
                    })
                    .map(|c| c.title.clone())
                    .collect();
                Some(magi_core::WorkPackageSummary {
                    task_id: wp.task_id.to_string(),
                    title: wp.title.clone(),
                    aggregate_status: wp.status.clone(),
                    display_status: format!("{:?}", wp.status),
                    progress_ratio,
                    recent_evidence,
                    recent_issues,
                })
            })
            .collect();

        let aggregate_status = if root_task.status == TaskStatus::Cancelled {
            TaskStatus::Cancelled
        } else if failed_tasks > 0 {
            TaskStatus::Failed
        } else if running_count > 0 {
            TaskStatus::Running
        } else if blocked_count > 0 {
            TaskStatus::Blocked
        } else if total_tasks > 0 && settled_tasks == total_tasks {
            TaskStatus::Completed
        } else {
            root_task.status
        };

        let execution_mode = if root_task
            .policy_snapshot
            .as_ref()
            .map(|policy| policy.background_allowed)
            .unwrap_or(false)
        {
            "deep".to_string()
        } else {
            "normal".to_string()
        };
        let runner_status = match aggregate_status {
            TaskStatus::Running => "running".to_string(),
            TaskStatus::Blocked | TaskStatus::AwaitingApproval => "blocked".to_string(),
            TaskStatus::Completed => "completed".to_string(),
            TaskStatus::Failed => "error".to_string(),
            _ => "idle".to_string(),
        };
        let display_status = if root_task.status == TaskStatus::Cancelled {
            "已取消".to_string()
        } else if total_tasks == 0 {
            "待启动".to_string()
        } else {
            let pct = (settled_tasks as f32 / total_tasks as f32 * 100.0).round() as u32;
            if settled_tasks == total_tasks {
                "全部完成".to_string()
            } else if failed_tasks > 0 && blocked_count > 0 {
                format!(
                    "{}% 已完成，{} 项失败、{} 项需要处理",
                    pct, failed_tasks, blocked_count
                )
            } else if failed_tasks > 0 {
                format!("{}% 已完成，{} 项失败待修复", pct, failed_tasks)
            } else if blocked_count > 0 {
                format!("{}% 已完成，{} 项需要处理", pct, blocked_count)
            } else if running_count > 0 {
                format!("{}% 已完成，{} 项执行中", pct, running_count)
            } else {
                format!("{}% 已完成", pct)
            }
        };

        Some(TaskProjection {
            root_task,
            tasks: projection_tasks,
            current_phase,
            running_tasks,
            blocked_tasks,
            pending_decisions,
            workpackage_summaries,
            validation_summary,
            progress_summary: ProgressSummary {
                total_tasks,
                completed_tasks,
                settled_tasks,
                failed_tasks,
                running_tasks: running_count,
                blocked_tasks: blocked_count,
            },
            aggregate_status,
            display_status,
            execution_mode,
            runner_status,
        })
    }

    /// 聚合交付包（design 12）：从 TaskProjection 和已完成任务中提取
    /// 目标、范围、变更、证据、验证结果等交付摘要。
    pub fn build_delivery_package(&self, root_task_id: &TaskId) -> Option<serde_json::Value> {
        let projection = self.build_projection(root_task_id)?;
        let active_tasks: Vec<&Task> = projection
            .tasks
            .iter()
            .filter(|task| task.status != TaskStatus::Cancelled)
            .collect();

        let completed_tasks: Vec<&Task> = active_tasks
            .iter()
            .copied()
            .filter(|t| t.status == TaskStatus::Completed)
            .collect();

        let mut file_changes: Vec<String> = Vec::new();
        let mut evidence_list: Vec<String> = Vec::new();
        let mut validation_results: Vec<serde_json::Value> = Vec::new();
        let mut repair_records: Vec<serde_json::Value> = Vec::new();
        let mut key_decisions: Vec<serde_json::Value> = Vec::new();

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
            if task.kind == TaskKind::Validation && task.status == TaskStatus::Completed {
                validation_results.push(serde_json::json!({
                    "task_id": task.task_id.to_string(),
                    "title": task.title,
                    "result": "passed",
                    "evidence": task.evidence_refs,
                }));
            }
            if task.kind == TaskKind::Repair && task.status == TaskStatus::Completed {
                repair_records.push(serde_json::json!({
                    "task_id": task.task_id.to_string(),
                    "title": task.title,
                    "goal": task.goal,
                    "evidence": task.evidence_refs,
                }));
            }
            if task.kind == TaskKind::Decision && task.status == TaskStatus::Completed {
                if let Some(ref payload) = task.decision_payload {
                    key_decisions.push(serde_json::json!({
                        "task_id": task.task_id.to_string(),
                        "context": payload.get("decision_context").and_then(serde_json::Value::as_str),
                        "blocked_reason": payload.get("blocked_reason").and_then(serde_json::Value::as_str),
                        "chosen_option": task.output_refs.first(),
                    }));
                }
            }
        }

        let remaining_risks: Vec<String> = active_tasks
            .iter()
            .filter(|t| {
                t.status == TaskStatus::Failed
                    || t.status == TaskStatus::Blocked
                    || t.status == TaskStatus::AwaitingApproval
            })
            .map(|t| format!("{}: {:?} - {}", t.task_id, t.status, t.title))
            .collect();

        Some(serde_json::json!({
            "goal": projection.root_task.goal,
            "scope": projection.root_task.workspace_scope,
            "execution_mode": projection.execution_mode,
            "aggregate_status": projection.aggregate_status,
            "current_phase": projection.current_phase,
            "progress": {
                "total": projection.progress_summary.total_tasks,
                "completed": projection.progress_summary.settled_tasks,
                "failed": projection.progress_summary.failed_tasks,
                "running": projection.progress_summary.running_tasks,
                "blocked": projection.progress_summary.blocked_tasks,
            },
            "file_changes": file_changes,
            "evidence_list": evidence_list,
            "validation_results": validation_results,
            "repair_records": repair_records,
            "key_decisions": key_decisions,
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
    /// 哪些 branch 属于当前可恢复执行树，并把对应子树统一收口到 `Blocked`。
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
                    TaskStatus::Blocked
                        | TaskStatus::Ready
                        | TaskStatus::Running
                        | TaskStatus::Verifying
                        | TaskStatus::Repairing
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

        let mut blocked_count = 0usize;
        for root_task_id in affected_roots {
            for task_id in self.collect_subtree_ids(&root_task_id) {
                let Some(task) = self.get_task(&task_id) else {
                    continue;
                };
                if matches!(
                    task.status,
                    TaskStatus::Draft
                        | TaskStatus::Ready
                        | TaskStatus::Running
                        | TaskStatus::Verifying
                        | TaskStatus::Repairing
                ) {
                    if self.update_status(&task_id, TaskStatus::Blocked).is_ok() {
                        blocked_count += 1;
                    }
                }
            }
        }

        (active_leases.len(), blocked_count)
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
        let context_entries = self
            .context_entries
            .read()
            .expect("context_entries read lock poisoned");
        serde_json::json!({
            "tasks": tasks.values().cloned().collect::<Vec<Task>>(),
            "leases": leases.values().cloned().collect::<Vec<TaskLease>>(),
            "contextEntries": context_entries.values().cloned().collect::<Vec<TaskContextEntry>>(),
        })
    }

    /// Restore a TaskStore from a checkpoint JSON value.
    ///
    /// Rebuilds the in-memory indexes (mission_index, children_index) from the
    /// task data.  The `on_status_change` callback is NOT restored — callers
    /// must re-attach it after restore if needed.
    pub fn restore(data: &serde_json::Value) -> Self {
        let tasks: Vec<Task> = data
            .get("tasks")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let leases: Vec<TaskLease> = data
            .get("leases")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let context_entries: Vec<TaskContextEntry> = data
            .get("contextEntries")
            .or_else(|| data.get("context_entries"))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let store = Self::new();

        // Re-insert tasks (which rebuilds mission_index and children_index).
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
        {
            let mut entry_map = store
                .context_entries
                .write()
                .expect("context_entries write lock poisoned");
            for entry in context_entries {
                entry_map.insert(entry.context_ref.clone(), entry);
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

/// 根据设计文档 3.3.3 状态迁移表判断一次状态迁移是否合法。
pub fn is_valid_transition(from: TaskStatus, to: TaskStatus) -> bool {
    use TaskStatus::*;
    if from == to {
        return true;
    }
    matches!(
        (from, to),
        // Draft → Ready | Skipped | Cancelled
        (Draft, Ready) | (Draft, Skipped) | (Draft, Cancelled) |
        // Ready → Running | Blocked | AwaitingApproval | Skipped | Cancelled
        (Ready, Running) | (Ready, Blocked) | (Ready, AwaitingApproval) |
        (Ready, Skipped) | (Ready, Cancelled) |
        // Running → Verifying | Completed | Repairing | Failed | Cancelled
        (Running, Verifying) | (Running, Completed) | (Running, Repairing) |
        (Running, Failed) | (Running, Cancelled) |
        // Blocked → Ready | Skipped | Cancelled
        (Blocked, Ready) | (Blocked, Skipped) | (Blocked, Cancelled) |
        // AwaitingApproval → Completed | Cancelled
        (AwaitingApproval, Completed) | (AwaitingApproval, Cancelled) |
        // Verifying → Completed | Repairing | Failed
        (Verifying, Completed) | (Verifying, Repairing) | (Verifying, Failed) |
        // Repairing → Verifying | Failed
        (Repairing, Verifying) | (Repairing, Failed)
    )
}

fn is_terminal_status(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Skipped
    )
}

/// Validate parent-child kind relationships (design 3.2).
fn is_valid_parent_child_kind(parent: TaskKind, child: TaskKind) -> bool {
    use TaskKind::*;
    matches!(
        (parent, child),
        // Objective can contain Phase, WorkPackage, Action, Validation, Decision
        (Objective, Phase)
            | (Objective, WorkPackage)
            | (Objective, Action)
            | (Objective, Validation)
            | (Objective, Decision)
            // Phase can contain WorkPackage, Action, Validation, Decision
            | (Phase, WorkPackage)
            | (Phase, Action)
            | (Phase, Validation)
            | (Phase, Decision)
            // WorkPackage can contain Action, Validation, Decision
            | (WorkPackage, Action)
            | (WorkPackage, Validation)
            | (WorkPackage, Decision)
            // Action can contain Repair, Validation, Decision (runtime children)
            | (Action, Repair)
            | (Action, Validation)
            | (Action, Decision)
            // Repair can contain Validation, Decision (runtime)
            | (Repair, Validation)
            | (Repair, Decision)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::{TaskLease, TaskLeaseState};
    use magi_core::{LeaseId, MissionId, Task, TaskId, TaskKind, TaskStatus, UtcMillis, WorkerId};
    use std::sync::{Arc, Mutex};

    fn make_task(
        task_id: &str,
        mission_id: &str,
        root_task_id: &str,
        parent_task_id: Option<&str>,
        kind: TaskKind,
        status: TaskStatus,
    ) -> Task {
        Task {
            task_id: TaskId::new(task_id),
            mission_id: MissionId::new(mission_id),
            root_task_id: TaskId::new(root_task_id),
            parent_task_id: parent_task_id.map(TaskId::new),
            kind,
            title: format!("Task {task_id}"),
            goal: format!("Goal for {task_id}"),
            status,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            context_refs: Vec::new(),
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            repair_count: 0,
            decision_payload: None,
            variant: magi_core::TaskVariant::default(),
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        }
    }

    fn make_decision_payload(
        decision_context: &str,
        blocked_reason: &str,
        target_task_id: Option<&str>,
        option_id: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "decision_context": decision_context,
            "blocked_reason": blocked_reason,
            "target_task_id": target_task_id,
            "options": [
                {"option_id": option_id, "label": "重试", "description": "重新执行"}
            ],
            "risk_notes": [],
            "recommended_option": option_id,
            "required_user_input": true,
            "decision_evidence": null
        })
    }

    // -----------------------------------------------------------------------
    // Test 1: Simple task graph (Objective -> 2 Actions)
    // -----------------------------------------------------------------------

    #[test]
    fn task_graph_insert_and_get_children() {
        let store = TaskStore::new();

        let objective = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Draft,
        );
        let action1 = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Draft,
        );
        let action2 = make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Draft,
        );

        store.insert_task(objective.clone());
        store.insert_task(action1.clone());
        store.insert_task(action2.clone());

        // Verify retrieval
        let retrieved = store.get_task(&TaskId::new("obj-1"));
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().title, "Task obj-1");

        // Verify children
        let children = store.get_children(&TaskId::new("obj-1"));
        assert_eq!(children.len(), 2);
        let child_ids: Vec<String> = children.iter().map(|c| c.task_id.to_string()).collect();
        assert!(child_ids.contains(&"act-1".to_string()));
        assert!(child_ids.contains(&"act-2".to_string()));

        // Root has no parent, so shouldn't appear as a child of anything
        let root_children = store.get_children(&TaskId::new("nonexistent"));
        assert!(root_children.is_empty());
    }

    #[test]
    fn append_context_ref_updates_existing_task_without_duplication() {
        let store = TaskStore::new();

        let task = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Ready,
        );
        store.insert_task(task);

        let task_id = TaskId::new("obj-1");
        store
            .append_context_ref(&task_id, "ctx-1".to_string())
            .expect("first context ref append should succeed");
        store
            .append_context_ref(&task_id, "ctx-1".to_string())
            .expect("duplicate context ref append should be idempotent");

        let updated = store.get_task(&task_id).unwrap();
        assert_eq!(updated.context_refs, vec!["ctx-1".to_string()]);
        assert_eq!(store.get_tasks_by_mission(&MissionId::new("m-1")).len(), 1);
    }

    #[test]
    fn append_context_entry_persists_content_and_restores_from_checkpoint() {
        let store = TaskStore::new();
        let task_id = TaskId::new("obj-1");
        store.insert_task(make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Ready,
        ));

        let entry = store
            .append_context_entry(
                &task_id,
                "ctx-guide-1".to_string(),
                "优先参考用户补充的真实约束".to_string(),
            )
            .expect("context entry should append");

        assert_eq!(entry.context_ref, "ctx-guide-1");
        assert_eq!(entry.content, "优先参考用户补充的真实约束");
        assert_eq!(
            store.get_task(&task_id).unwrap().context_refs,
            vec!["ctx-guide-1".to_string()]
        );
        assert_eq!(
            store.context_entries_for_refs(&["ctx-guide-1".to_string()]),
            vec![entry.clone()]
        );

        let restored = TaskStore::restore(&store.checkpoint());
        assert_eq!(
            restored.context_entries_for_refs(&["ctx-guide-1".to_string()]),
            vec![entry]
        );
    }

    #[test]
    fn append_required_child_updates_parent_aggregation() {
        let store = TaskStore::new();

        let parent = make_task(
            "parent-1",
            "m-1",
            "parent-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let child = make_task(
            "child-1",
            "m-1",
            "parent-1",
            Some("parent-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        store.insert_task(parent);
        store.insert_task(child);

        let parent_id = TaskId::new("parent-1");
        let child_id = TaskId::new("child-1");
        store
            .append_required_child(&parent_id, &child_id)
            .expect("append required child should succeed");
        store
            .append_required_child(&parent_id, &child_id)
            .expect("duplicate required child append should be idempotent");

        let updated_parent = store.get_task(&parent_id).unwrap();
        assert_eq!(updated_parent.required_children, vec![child_id]);
    }

    #[test]
    fn remove_task_cleans_required_children_references() {
        let store = TaskStore::new();

        let parent = make_task(
            "parent-2",
            "m-1",
            "parent-2",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let child = make_task(
            "child-2",
            "m-1",
            "parent-2",
            Some("parent-2"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        store.insert_task(parent);
        store.insert_task(child);
        store
            .append_required_child(&TaskId::new("parent-2"), &TaskId::new("child-2"))
            .expect("required child append should succeed");

        let removed = store.remove_task(&TaskId::new("child-2"));
        assert!(removed.is_some());

        let updated_parent = store.get_task(&TaskId::new("parent-2")).unwrap();
        assert!(updated_parent.required_children.is_empty());
    }

    #[test]
    fn reparent_task_moves_required_child_marker() {
        let store = TaskStore::new();

        let old_parent = make_task(
            "parent-old",
            "m-1",
            "parent-old",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let new_parent = make_task(
            "parent-new",
            "m-1",
            "parent-old",
            Some("parent-old"),
            TaskKind::Phase,
            TaskStatus::Running,
        );
        let child = make_task(
            "child-3",
            "m-1",
            "parent-old",
            Some("parent-old"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        store.insert_task(old_parent);
        store.insert_task(new_parent);
        store.insert_task(child);
        store
            .append_required_child(&TaskId::new("parent-old"), &TaskId::new("child-3"))
            .expect("required child append should succeed");

        store
            .reparent_task(&TaskId::new("child-3"), &TaskId::new("parent-new"))
            .expect("reparent should succeed");

        let old_parent_updated = store.get_task(&TaskId::new("parent-old")).unwrap();
        let new_parent_updated = store.get_task(&TaskId::new("parent-new")).unwrap();
        assert!(old_parent_updated.required_children.is_empty());
        assert_eq!(
            new_parent_updated.required_children,
            vec![TaskId::new("child-3")]
        );
        let child_updated = store.get_task(&TaskId::new("child-3")).unwrap();
        assert_eq!(
            child_updated.parent_task_id,
            Some(TaskId::new("parent-new"))
        );
    }

    // -----------------------------------------------------------------------
    // Test 2: Status transitions (Draft -> Ready -> Running -> Completed)
    // -----------------------------------------------------------------------

    #[test]
    fn status_transitions() {
        let store = TaskStore::new();

        let task = make_task(
            "t-1",
            "m-1",
            "t-1",
            None,
            TaskKind::Action,
            TaskStatus::Draft,
        );
        store.insert_task(task);

        // Draft -> Ready
        assert!(
            store
                .update_status(&TaskId::new("t-1"), TaskStatus::Ready)
                .is_ok()
        );
        assert_eq!(
            store.get_task(&TaskId::new("t-1")).unwrap().status,
            TaskStatus::Ready
        );

        // Ready -> Running
        assert!(
            store
                .update_status(&TaskId::new("t-1"), TaskStatus::Running)
                .is_ok()
        );
        assert_eq!(
            store.get_task(&TaskId::new("t-1")).unwrap().status,
            TaskStatus::Running
        );

        // Running -> Completed
        assert!(
            store
                .update_status(&TaskId::new("t-1"), TaskStatus::Completed)
                .is_ok()
        );
        assert_eq!(
            store.get_task(&TaskId::new("t-1")).unwrap().status,
            TaskStatus::Completed
        );

        // Updating a nonexistent task should return an error
        let result = store.update_status(&TaskId::new("nonexistent"), TaskStatus::Running);
        assert!(result.is_err());
    }

    #[test]
    fn status_change_callback_receives_old_and_new_status() {
        let transitions = Arc::new(Mutex::new(Vec::new()));
        let transitions_for_callback = Arc::clone(&transitions);
        let store = TaskStore::with_status_change_callback(Box::new(
            move |task_id, old_status, new_status, task| {
                transitions_for_callback
                    .lock()
                    .expect("transitions lock should not be poisoned")
                    .push((task_id.to_string(), old_status, new_status, task.status));
            },
        ));

        store.insert_task(make_task(
            "t-callback",
            "m-callback",
            "t-callback",
            None,
            TaskKind::Action,
            TaskStatus::Ready,
        ));
        store
            .update_status(&TaskId::new("t-callback"), TaskStatus::Running)
            .expect("status update should succeed");

        let recorded = transitions
            .lock()
            .expect("transitions lock should not be poisoned");
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "t-callback");
        assert_eq!(recorded[0].1, TaskStatus::Ready);
        assert_eq!(recorded[0].2, TaskStatus::Running);
        assert_eq!(recorded[0].3, TaskStatus::Running);
    }

    // -----------------------------------------------------------------------
    // Test 3: Runnable leaf detection
    // -----------------------------------------------------------------------

    #[test]
    fn runnable_leaf_detection() {
        let store = TaskStore::new();

        // Objective -> Phase -> 2 Actions (act-1 depends on nothing, act-2 depends on act-1)
        let objective = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let phase = make_task(
            "phase-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Phase,
            TaskStatus::Running,
        );
        let action1 = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("phase-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        let mut action2 = make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("phase-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        action2.dependency_ids = vec![TaskId::new("act-1")];

        store.insert_task(objective);
        store.insert_task(phase);
        store.insert_task(action1);
        store.insert_task(action2);

        // Only act-1 should be runnable (act-2 depends on act-1 which is not Completed)
        let runnable = store.get_runnable_leaves(&TaskId::new("obj-1"));
        assert_eq!(runnable.len(), 1);
        assert_eq!(runnable[0].task_id.to_string(), "act-1");

        // Complete act-1
        store
            .update_status(&TaskId::new("act-1"), TaskStatus::Completed)
            .unwrap();

        // Now act-2 should be runnable
        let runnable = store.get_runnable_leaves(&TaskId::new("obj-1"));
        assert_eq!(runnable.len(), 1);
        assert_eq!(runnable[0].task_id.to_string(), "act-2");

        // Complete act-2 as well
        store
            .update_status(&TaskId::new("act-2"), TaskStatus::Completed)
            .unwrap();

        // No runnable leaves
        let runnable = store.get_runnable_leaves(&TaskId::new("obj-1"));
        assert!(runnable.is_empty());
    }

    #[test]
    fn runnable_leaf_waits_for_ancestor_phase_dependencies() {
        let store = TaskStore::new();

        let objective = make_task(
            "obj-phase-deps",
            "m-phase-deps",
            "obj-phase-deps",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let planning_phase = make_task(
            "phase-planning",
            "m-phase-deps",
            "obj-phase-deps",
            Some("obj-phase-deps"),
            TaskKind::Phase,
            TaskStatus::Ready,
        );
        let planning_action = make_task(
            "act-planning",
            "m-phase-deps",
            "obj-phase-deps",
            Some("phase-planning"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        let mut execution_phase = make_task(
            "phase-execution",
            "m-phase-deps",
            "obj-phase-deps",
            Some("obj-phase-deps"),
            TaskKind::Phase,
            TaskStatus::Ready,
        );
        execution_phase.dependency_ids = vec![TaskId::new("phase-planning")];
        let execution_action = make_task(
            "act-execution",
            "m-phase-deps",
            "obj-phase-deps",
            Some("phase-execution"),
            TaskKind::Action,
            TaskStatus::Ready,
        );

        store.insert_task(objective);
        store.insert_task(planning_phase);
        store.insert_task(planning_action);
        store.insert_task(execution_phase);
        store.insert_task(execution_action);

        let runnable = store.get_runnable_leaves(&TaskId::new("obj-phase-deps"));
        assert_eq!(
            runnable
                .iter()
                .map(|task| task.task_id.to_string())
                .collect::<Vec<_>>(),
            vec!["act-planning".to_string()],
            "执行 action 必须等待父 Phase 的规划依赖完成"
        );

        store
            .update_status(&TaskId::new("act-planning"), TaskStatus::Completed)
            .expect("planning action should complete");
        store
            .update_status(&TaskId::new("phase-planning"), TaskStatus::Completed)
            .expect("planning phase should complete");

        let runnable = store.get_runnable_leaves(&TaskId::new("obj-phase-deps"));
        assert_eq!(
            runnable
                .iter()
                .map(|task| task.task_id.to_string())
                .collect::<Vec<_>>(),
            vec!["act-execution".to_string()]
        );
    }

    // -----------------------------------------------------------------------
    // Test 4: Projection building
    // -----------------------------------------------------------------------

    #[test]
    fn build_projection() {
        let store = TaskStore::new();

        let objective = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let phase = make_task(
            "phase-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Phase,
            TaskStatus::Running,
        );
        let wp = make_task(
            "wp-1",
            "m-1",
            "obj-1",
            Some("phase-1"),
            TaskKind::WorkPackage,
            TaskStatus::Running,
        );
        let act1 = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("wp-1"),
            TaskKind::Action,
            TaskStatus::Completed,
        );
        let act2 = make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("wp-1"),
            TaskKind::Action,
            TaskStatus::Running,
        );
        let blocked = make_task(
            "act-3",
            "m-1",
            "obj-1",
            Some("wp-1"),
            TaskKind::Action,
            TaskStatus::Blocked,
        );

        store.insert_task(objective);
        store.insert_task(phase);
        store.insert_task(wp);
        store.insert_task(act1);
        store.insert_task(act2);
        store.insert_task(blocked);

        let projection = store.build_projection(&TaskId::new("obj-1"));
        assert!(projection.is_some());
        let projection = projection.unwrap();

        assert_eq!(projection.root_task.task_id.to_string(), "obj-1");
        let projection_task_ids: Vec<String> = projection
            .tasks
            .iter()
            .map(|task| task.task_id.to_string())
            .collect();
        assert_eq!(
            projection_task_ids,
            vec!["obj-1", "phase-1", "wp-1", "act-1", "act-2", "act-3"]
        );
        assert_eq!(projection.current_phase, Some("Task phase-1".to_string()));
        assert_eq!(projection.running_tasks.len(), 4); // objective, phase, wp, act-2
        assert_eq!(projection.blocked_tasks.len(), 1);
        assert_eq!(projection.progress_summary.total_tasks, 6);
        assert_eq!(projection.progress_summary.completed_tasks, 1);
        assert_eq!(projection.progress_summary.settled_tasks, 1);
        assert_eq!(projection.progress_summary.failed_tasks, 0);
        assert_eq!(projection.progress_summary.running_tasks, 4);
        assert_eq!(projection.progress_summary.blocked_tasks, 1);
        assert_eq!(projection.aggregate_status, TaskStatus::Running);

        // Nonexistent root returns None
        assert!(
            store
                .build_projection(&TaskId::new("nonexistent"))
                .is_none()
        );
    }

    #[test]
    fn build_projection_excludes_cancelled_history_from_progress() {
        let store = TaskStore::new();

        store.insert_task(make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        ));
        store.insert_task(make_task(
            "phase-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Phase,
            TaskStatus::Running,
        ));
        store.insert_task(make_task(
            "wp-1",
            "m-1",
            "obj-1",
            Some("phase-1"),
            TaskKind::WorkPackage,
            TaskStatus::Running,
        ));
        store.insert_task(make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("wp-1"),
            TaskKind::Action,
            TaskStatus::Completed,
        ));
        store.insert_task(make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("wp-1"),
            TaskKind::Action,
            TaskStatus::Running,
        ));
        store.insert_task(make_task(
            "act-3",
            "m-1",
            "obj-1",
            Some("wp-1"),
            TaskKind::Action,
            TaskStatus::Cancelled,
        ));

        let projection = store.build_projection(&TaskId::new("obj-1")).unwrap();

        assert_eq!(projection.tasks.len(), 6);
        assert_eq!(projection.progress_summary.total_tasks, 5);
        assert_eq!(projection.progress_summary.completed_tasks, 1);
        assert_eq!(projection.progress_summary.settled_tasks, 1);
        assert_eq!(projection.progress_summary.running_tasks, 4);
        assert_eq!(projection.progress_summary.blocked_tasks, 0);
        assert_eq!(projection.display_status, "20% 已完成，4 项执行中");
        assert_eq!(projection.aggregate_status, TaskStatus::Running);

        let wp_summary = projection
            .workpackage_summaries
            .iter()
            .find(|wp| wp.task_id == "wp-1")
            .expect("workpackage summary must exist");
        assert_eq!(wp_summary.progress_ratio, 0.5);
        assert!(wp_summary.recent_issues.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test 5: Lease creation and retrieval
    // -----------------------------------------------------------------------

    #[test]
    fn lease_creation_and_retrieval() {
        let store = TaskStore::new();

        let task = make_task(
            "t-1",
            "m-1",
            "t-1",
            None,
            TaskKind::Action,
            TaskStatus::Running,
        );
        store.insert_task(task);

        let now = UtcMillis::now();
        let lease = TaskLease {
            lease_id: LeaseId::new("lease-1"),
            task_id: TaskId::new("t-1"),
            root_task_id: TaskId::new("t-1"),
            worker_id: WorkerId::new("worker-1"),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0 + 60_000),
            heartbeat_at: now,
            lease_status: TaskLeaseState::Active,
        };

        store.insert_lease(lease);

        // Retrieve active lease
        let active = store.get_active_lease(&TaskId::new("t-1"));
        assert!(active.is_some());
        let active = active.unwrap();
        assert_eq!(active.lease_id.to_string(), "lease-1");
        assert_eq!(active.worker_id.to_string(), "worker-1");
        assert_eq!(active.lease_status, TaskLeaseState::Active);

        // No active lease for another task
        assert!(store.get_active_lease(&TaskId::new("t-2")).is_none());
    }

    // -----------------------------------------------------------------------
    // Test 6: Mission index
    // -----------------------------------------------------------------------

    #[test]
    fn get_tasks_by_mission() {
        let store = TaskStore::new();

        let t1 = make_task(
            "t-1",
            "m-1",
            "t-1",
            None,
            TaskKind::Objective,
            TaskStatus::Draft,
        );
        let t2 = make_task(
            "t-2",
            "m-1",
            "t-1",
            Some("t-1"),
            TaskKind::Action,
            TaskStatus::Draft,
        );
        let t3 = make_task(
            "t-3",
            "m-2",
            "t-3",
            None,
            TaskKind::Objective,
            TaskStatus::Draft,
        );

        store.insert_task(t1);
        store.insert_task(t2);
        store.insert_task(t3);

        let m1_tasks = store.get_tasks_by_mission(&MissionId::new("m-1"));
        assert_eq!(m1_tasks.len(), 2);

        let m2_tasks = store.get_tasks_by_mission(&MissionId::new("m-2"));
        assert_eq!(m2_tasks.len(), 1);

        let empty = store.get_tasks_by_mission(&MissionId::new("m-999"));
        assert!(empty.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test 7: Projection with failed task shows Failed aggregate status
    // -----------------------------------------------------------------------

    #[test]
    fn projection_aggregate_status_reflects_failure() {
        let store = TaskStore::new();

        let objective = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let act1 = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Completed,
        );
        let act2 = make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Failed,
        );

        store.insert_task(objective);
        store.insert_task(act1);
        store.insert_task(act2);

        let projection = store.build_projection(&TaskId::new("obj-1")).unwrap();
        assert_eq!(projection.aggregate_status, TaskStatus::Failed);
        assert_eq!(projection.progress_summary.failed_tasks, 1);
    }

    #[test]
    fn projection_display_status_uses_actionable_copy_for_blocked_tasks() {
        let store = TaskStore::new();

        store.insert_task(make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Blocked,
        ));
        store.insert_task(make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Completed,
        ));

        let projection = store.build_projection(&TaskId::new("obj-1")).unwrap();

        assert_eq!(projection.aggregate_status, TaskStatus::Blocked);
        assert_eq!(projection.progress_summary.blocked_tasks, 1);
        assert_eq!(projection.display_status, "50% 已完成，1 项需要处理");
    }

    // -----------------------------------------------------------------------
    // Test 8: Decision tasks appear in pending_decisions
    // -----------------------------------------------------------------------

    #[test]
    fn pending_decisions_in_projection() {
        let store = TaskStore::new();

        let obj = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let decision = make_task(
            "dec-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Decision,
            TaskStatus::AwaitingApproval,
        );
        let act = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Blocked,
        );

        store.insert_task(obj);
        store.insert_task(decision);
        store.insert_task(act);

        let projection = store.build_projection(&TaskId::new("obj-1")).unwrap();
        assert_eq!(projection.pending_decisions.len(), 1);
        assert_eq!(projection.pending_decisions[0].to_string(), "dec-1");
    }

    #[test]
    fn resolve_decision_releases_blocked_siblings() {
        let store = TaskStore::new();

        let parent = make_task(
            "parent-1",
            "m-1",
            "root-1",
            Some("root-1"),
            TaskKind::Action,
            TaskStatus::Blocked,
        );
        let decision = make_task(
            "decision-1",
            "m-1",
            "root-1",
            Some("parent-1"),
            TaskKind::Decision,
            TaskStatus::AwaitingApproval,
        );
        let mut decision = decision;
        decision.decision_payload = Some(make_decision_payload(
            "选择修复路径",
            "需要用户确认",
            Some("parent-1"),
            "retry",
        ));

        store.insert_task(make_task(
            "root-1",
            "m-1",
            "root-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        ));
        let sibling = make_task(
            "sibling-1",
            "m-1",
            "root-1",
            Some("parent-1"),
            TaskKind::Action,
            TaskStatus::Blocked,
        );

        store.insert_task(parent);
        store.insert_task(decision);
        store.insert_task(sibling);

        store
            .resolve_decision(&TaskId::new("decision-1"), "retry", None)
            .expect("decision should resolve");

        assert_eq!(
            store.get_task(&TaskId::new("decision-1")).unwrap().status,
            TaskStatus::Completed
        );
        assert_eq!(
            store.get_task(&TaskId::new("sibling-1")).unwrap().status,
            TaskStatus::Ready
        );
        // Decision 释放后 parent 从 Blocked 变为 Ready（由 Runner 再调度为 Running）
        assert_eq!(
            store.get_task(&TaskId::new("parent-1")).unwrap().status,
            TaskStatus::Ready
        );
        assert_eq!(
            store
                .get_task(&TaskId::new("decision-1"))
                .unwrap()
                .output_refs,
            vec!["decision_chosen:retry".to_string()]
        );
    }

    // -----------------------------------------------------------------------
    // Test 9: grant_lease succeeds on fresh task
    // -----------------------------------------------------------------------

    #[test]
    fn grant_lease_succeeds_on_fresh_task() {
        let store = TaskStore::new();
        let task = make_task(
            "t-1",
            "m-1",
            "t-1",
            None,
            TaskKind::Action,
            TaskStatus::Running,
        );
        store.insert_task(task);

        let worker_id = WorkerId::new("worker-1");
        let lease = store.grant_lease(
            &TaskId::new("t-1"),
            &TaskId::new("t-1"),
            &worker_id,
            "executor",
            60_000,
        );
        assert!(lease.is_some());

        let lease = lease.unwrap();
        assert_eq!(lease.task_id.to_string(), "t-1");
        assert_eq!(lease.worker_id.to_string(), "worker-1");
        assert_eq!(lease.role, "executor");
        assert_eq!(lease.lease_status, TaskLeaseState::Active);
        assert!(lease.expires_at.0 > lease.granted_at.0);
        assert_eq!(lease.heartbeat_at, lease.granted_at);
    }

    // -----------------------------------------------------------------------
    // Test 10: grant_lease fails when active lease exists
    // -----------------------------------------------------------------------

    #[test]
    fn grant_lease_fails_when_active_lease_exists() {
        let store = TaskStore::new();
        let task = make_task(
            "t-1",
            "m-1",
            "t-1",
            None,
            TaskKind::Action,
            TaskStatus::Running,
        );
        store.insert_task(task);

        let worker_id = WorkerId::new("worker-1");
        let first = store.grant_lease(
            &TaskId::new("t-1"),
            &TaskId::new("t-1"),
            &worker_id,
            "executor",
            60_000,
        );
        assert!(first.is_some());

        // Second grant should fail
        let worker_id2 = WorkerId::new("worker-2");
        let second = store.grant_lease(
            &TaskId::new("t-1"),
            &TaskId::new("t-1"),
            &worker_id2,
            "executor",
            60_000,
        );
        assert!(second.is_none());
    }

    // -----------------------------------------------------------------------
    // Test 11: complete_lease marks lease as Completed
    // -----------------------------------------------------------------------

    #[test]
    fn complete_lease_marks_lease_completed() {
        let store = TaskStore::new();
        let task = make_task(
            "t-1",
            "m-1",
            "t-1",
            None,
            TaskKind::Action,
            TaskStatus::Running,
        );
        store.insert_task(task);

        let worker_id = WorkerId::new("worker-1");
        let lease = store
            .grant_lease(
                &TaskId::new("t-1"),
                &TaskId::new("t-1"),
                &worker_id,
                "executor",
                60_000,
            )
            .unwrap();

        let result = store.complete_lease(&TaskId::new("t-1"), &lease.lease_id);
        assert!(result);

        // Active lease should no longer exist
        assert!(store.get_active_lease(&TaskId::new("t-1")).is_none());

        // Completing again should fail (no longer active)
        let result2 = store.complete_lease(&TaskId::new("t-1"), &lease.lease_id);
        assert!(!result2);

        // After completing, a new lease can be granted
        let new_lease = store.grant_lease(
            &TaskId::new("t-1"),
            &TaskId::new("t-1"),
            &worker_id,
            "executor",
            60_000,
        );
        assert!(new_lease.is_some());
    }

    // -----------------------------------------------------------------------
    // Test 12: revoke_lease marks lease as Revoked
    // -----------------------------------------------------------------------

    #[test]
    fn revoke_lease_marks_lease_revoked() {
        let store = TaskStore::new();
        let task = make_task(
            "t-1",
            "m-1",
            "t-1",
            None,
            TaskKind::Action,
            TaskStatus::Running,
        );
        store.insert_task(task);

        let worker_id = WorkerId::new("worker-1");
        let lease = store
            .grant_lease(
                &TaskId::new("t-1"),
                &TaskId::new("t-1"),
                &worker_id,
                "executor",
                60_000,
            )
            .unwrap();

        let result = store.revoke_lease(&TaskId::new("t-1"), &lease.lease_id);
        assert!(result);

        // Active lease should no longer exist
        assert!(store.get_active_lease(&TaskId::new("t-1")).is_none());

        // Revoking again should fail
        let result2 = store.revoke_lease(&TaskId::new("t-1"), &lease.lease_id);
        assert!(!result2);

        // A new lease can be granted after revocation
        let new_lease = store.grant_lease(
            &TaskId::new("t-1"),
            &TaskId::new("t-1"),
            &worker_id,
            "executor",
            60_000,
        );
        assert!(new_lease.is_some());
    }

    // -----------------------------------------------------------------------
    // Test 13: heartbeat_lease updates heartbeat_at
    // -----------------------------------------------------------------------

    #[test]
    fn heartbeat_lease_updates_heartbeat_at() {
        let store = TaskStore::new();
        let task = make_task(
            "t-1",
            "m-1",
            "t-1",
            None,
            TaskKind::Action,
            TaskStatus::Running,
        );
        store.insert_task(task);

        let worker_id = WorkerId::new("worker-1");
        let lease = store
            .grant_lease(
                &TaskId::new("t-1"),
                &TaskId::new("t-1"),
                &worker_id,
                "executor",
                60_000,
            )
            .unwrap();
        let original_heartbeat = lease.heartbeat_at;

        // Small busy-wait to ensure time advances (UtcMillis is ms-resolution)
        std::thread::sleep(std::time::Duration::from_millis(5));

        let result = store.heartbeat_lease(&TaskId::new("t-1"), &lease.lease_id);
        assert!(result);

        let updated = store.get_active_lease(&TaskId::new("t-1")).unwrap();
        assert!(updated.heartbeat_at.0 >= original_heartbeat.0);

        // Heartbeat on nonexistent lease should fail
        let bad = store.heartbeat_lease(&TaskId::new("t-1"), &LeaseId::new("nonexistent"));
        assert!(!bad);
    }

    // -----------------------------------------------------------------------
    // Test 14: collect_expired_leases finds expired ones
    // -----------------------------------------------------------------------

    #[test]
    fn collect_expired_leases_finds_expired() {
        let store = TaskStore::new();
        let task1 = make_task(
            "t-1",
            "m-1",
            "t-1",
            None,
            TaskKind::Action,
            TaskStatus::Running,
        );
        let task2 = make_task(
            "t-2",
            "m-1",
            "t-2",
            None,
            TaskKind::Action,
            TaskStatus::Running,
        );
        store.insert_task(task1);
        store.insert_task(task2);

        // Insert a lease that is already expired (expires_at in the past)
        let now = UtcMillis::now();
        let expired_lease = TaskLease {
            lease_id: LeaseId::new("lease-expired"),
            task_id: TaskId::new("t-1"),
            root_task_id: TaskId::new("t-1"),
            worker_id: WorkerId::new("worker-1"),
            role: "executor".to_string(),
            granted_at: UtcMillis(now.0.saturating_sub(120_000)),
            expires_at: UtcMillis(now.0.saturating_sub(60_000)), // expired 60s ago
            heartbeat_at: UtcMillis(now.0.saturating_sub(120_000)),
            lease_status: TaskLeaseState::Active,
        };
        store.insert_lease(expired_lease);

        // Insert a lease that is NOT expired (far future)
        let worker_id = WorkerId::new("worker-2");
        let _valid = store.grant_lease(
            &TaskId::new("t-2"),
            &TaskId::new("t-2"),
            &worker_id,
            "executor",
            600_000,
        );

        let expired = store.collect_expired_leases(&TaskId::new("t-1"));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0.to_string(), "t-1");
        assert_eq!(expired[0].1.to_string(), "lease-expired");
    }

    // -----------------------------------------------------------------------
    // Test 15: get_leases_by_worker filters correctly
    // -----------------------------------------------------------------------

    #[test]
    fn get_leases_by_worker_filters_correctly() {
        let store = TaskStore::new();
        let task1 = make_task(
            "t-1",
            "m-1",
            "t-1",
            None,
            TaskKind::Action,
            TaskStatus::Running,
        );
        let task2 = make_task(
            "t-2",
            "m-1",
            "t-2",
            None,
            TaskKind::Action,
            TaskStatus::Running,
        );
        let task3 = make_task(
            "t-3",
            "m-1",
            "t-3",
            None,
            TaskKind::Action,
            TaskStatus::Running,
        );
        store.insert_task(task1);
        store.insert_task(task2);
        store.insert_task(task3);

        let worker1 = WorkerId::new("worker-1");
        let worker2 = WorkerId::new("worker-2");

        // Worker 1 gets two leases
        store.grant_lease(
            &TaskId::new("t-1"),
            &TaskId::new("t-1"),
            &worker1,
            "executor",
            60_000,
        );
        store.grant_lease(
            &TaskId::new("t-2"),
            &TaskId::new("t-2"),
            &worker1,
            "validator",
            60_000,
        );

        // Worker 2 gets one lease
        store.grant_lease(
            &TaskId::new("t-3"),
            &TaskId::new("t-3"),
            &worker2,
            "executor",
            60_000,
        );

        let w1_leases = store.get_leases_by_worker(&worker1);
        assert_eq!(w1_leases.len(), 2);
        assert!(
            w1_leases
                .iter()
                .all(|l| l.worker_id.to_string() == "worker-1")
        );

        let w2_leases = store.get_leases_by_worker(&worker2);
        assert_eq!(w2_leases.len(), 1);
        assert_eq!(w2_leases[0].task_id.to_string(), "t-3");

        // Complete one of worker-1's leases — it should no longer appear
        let lease_id = w1_leases[0].lease_id.clone();
        let task_id = w1_leases[0].task_id.clone();
        store.complete_lease(&task_id, &lease_id);

        let w1_leases_after = store.get_leases_by_worker(&worker1);
        assert_eq!(w1_leases_after.len(), 1);

        // Unknown worker returns empty
        let empty = store.get_leases_by_worker(&WorkerId::new("worker-unknown"));
        assert!(empty.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test 16: checkpoint and restore round-trip preserves tasks and leases
    // -----------------------------------------------------------------------

    #[test]
    fn checkpoint_restore_round_trip() {
        let store = TaskStore::new();

        let obj = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let act1 = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Completed,
        );
        let act2 = make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Running,
        );

        store.insert_task(obj);
        store.insert_task(act1);
        store.insert_task(act2);

        // Grant a lease
        let worker_id = WorkerId::new("worker-1");
        let lease = store
            .grant_lease(
                &TaskId::new("act-2"),
                &TaskId::new("obj-1"),
                &worker_id,
                "executor",
                60_000,
            )
            .unwrap();

        // Checkpoint
        let data = store.checkpoint();

        // Restore into a new store
        let restored = TaskStore::restore(&data);

        // Verify tasks
        assert!(restored.get_task(&TaskId::new("obj-1")).is_some());
        assert!(restored.get_task(&TaskId::new("act-1")).is_some());
        assert!(restored.get_task(&TaskId::new("act-2")).is_some());
        assert_eq!(
            restored.get_task(&TaskId::new("obj-1")).unwrap().status,
            TaskStatus::Running
        );
        assert_eq!(
            restored.get_task(&TaskId::new("act-1")).unwrap().status,
            TaskStatus::Completed
        );

        // Verify children index was rebuilt
        let children = restored.get_children(&TaskId::new("obj-1"));
        assert_eq!(children.len(), 2);

        // Verify mission index was rebuilt
        let mission_tasks = restored.get_tasks_by_mission(&MissionId::new("m-1"));
        assert_eq!(mission_tasks.len(), 3);

        // Verify lease was restored
        let active_lease = restored.get_active_lease(&TaskId::new("act-2"));
        assert!(active_lease.is_some());
        assert_eq!(active_lease.unwrap().lease_id, lease.lease_id);
    }

    // -----------------------------------------------------------------------
    // Test 17: checkpoint_to_file and restore_from_file round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn checkpoint_file_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "magi-task-store-checkpoint-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("task-store.json");

        let store = TaskStore::new();

        let obj = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let act = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        store.insert_task(obj);
        store.insert_task(act);

        let worker_id = WorkerId::new("worker-1");
        store.grant_lease(
            &TaskId::new("act-1"),
            &TaskId::new("act-1"),
            &worker_id,
            "executor",
            60_000,
        );

        // Write checkpoint
        store
            .checkpoint_to_file(&path)
            .expect("checkpoint should write");
        assert!(path.exists());

        // Restore from file
        let restored = TaskStore::restore_from_file(&path)
            .expect("restore should succeed")
            .expect("file should exist");

        assert_eq!(
            restored.get_task(&TaskId::new("obj-1")).unwrap().status,
            TaskStatus::Running
        );
        assert_eq!(
            restored.get_task(&TaskId::new("act-1")).unwrap().status,
            TaskStatus::Ready
        );
        assert!(restored.get_active_lease(&TaskId::new("act-1")).is_some());

        let children = restored.get_children(&TaskId::new("obj-1"));
        assert_eq!(children.len(), 1);
    }

    // -----------------------------------------------------------------------
    // Test 18: restore_from_file returns None when file does not exist
    // -----------------------------------------------------------------------

    #[test]
    fn restore_from_nonexistent_file_returns_none() {
        let path = std::env::temp_dir().join("magi-task-store-nonexistent-test-file.json");
        let result = TaskStore::restore_from_file(&path).expect("restore should not fail");
        assert!(result.is_none());
    }

    // -----------------------------------------------------------------------
    // Test 19: is_valid_transition covers design doc 3.3.3
    // -----------------------------------------------------------------------

    #[test]
    fn valid_transitions_accepted() {
        use TaskStatus::*;
        let valid = [
            (Draft, Ready),
            (Draft, Skipped),
            (Draft, Cancelled),
            (Ready, Running),
            (Ready, Blocked),
            (Ready, AwaitingApproval),
            (Ready, Skipped),
            (Ready, Cancelled),
            (Running, Verifying),
            (Running, Completed),
            (Running, Repairing),
            (Running, Failed),
            (Running, Cancelled),
            (Blocked, Ready),
            (Blocked, Skipped),
            (Blocked, Cancelled),
            (AwaitingApproval, Completed),
            (AwaitingApproval, Cancelled),
            (Verifying, Completed),
            (Verifying, Repairing),
            (Verifying, Failed),
            (Repairing, Verifying),
            (Repairing, Failed),
        ];
        for (from, to) in valid {
            assert!(
                is_valid_transition(from, to),
                "expected valid: {:?} -> {:?}",
                from,
                to
            );
        }
        // Self-transitions are always valid.
        for s in [
            Draft,
            Ready,
            Running,
            Blocked,
            AwaitingApproval,
            Verifying,
            Repairing,
            Completed,
            Failed,
            Cancelled,
            Skipped,
        ] {
            assert!(
                is_valid_transition(s, s),
                "self-transition {:?} should be valid",
                s
            );
        }
    }

    #[test]
    fn invalid_transitions_rejected() {
        use TaskStatus::*;
        let invalid = [
            (Draft, Running),
            (Draft, Completed),
            (Draft, Failed),
            (Ready, Completed),
            (Ready, Verifying),
            (Running, Ready),
            (Running, Blocked),
            (Running, Draft),
            (Blocked, Running),
            (Blocked, Completed),
            (Completed, Running),
            (Completed, Failed),
            (Completed, Ready),
            (Failed, Running),
            (Failed, Ready),
            (Failed, Completed),
            (Cancelled, Running),
            (Cancelled, Ready),
            (Skipped, Running),
            (Skipped, Ready),
            (AwaitingApproval, Running),
            (AwaitingApproval, Failed),
            (Repairing, Completed),
            (Repairing, Running),
        ];
        for (from, to) in invalid {
            assert!(
                !is_valid_transition(from, to),
                "expected invalid: {:?} -> {:?}",
                from,
                to
            );
        }
    }

    // -----------------------------------------------------------------------
    // Test 20: update_status_checked enforces transitions
    // -----------------------------------------------------------------------

    #[test]
    fn update_status_checked_accepts_valid_rejects_invalid() {
        let store = TaskStore::new();
        let task = make_task(
            "t-1",
            "m-1",
            "t-1",
            None,
            TaskKind::Action,
            TaskStatus::Draft,
        );
        store.insert_task(task);

        // Draft -> Ready: valid
        assert!(
            store
                .update_status_checked(&TaskId::new("t-1"), TaskStatus::Ready)
                .is_ok()
        );
        assert_eq!(
            store.get_task(&TaskId::new("t-1")).unwrap().status,
            TaskStatus::Ready
        );

        // Ready -> Completed: invalid (must go through Running first)
        let err = store.update_status_checked(&TaskId::new("t-1"), TaskStatus::Completed);
        assert!(err.is_err());
        assert_eq!(
            store.get_task(&TaskId::new("t-1")).unwrap().status,
            TaskStatus::Ready
        );
    }

    // -----------------------------------------------------------------------
    // Test 21: increment_repair_count / increment_retry_count
    // -----------------------------------------------------------------------

    #[test]
    fn increment_counts() {
        let store = TaskStore::new();
        let task = make_task(
            "t-1",
            "m-1",
            "t-1",
            None,
            TaskKind::Action,
            TaskStatus::Running,
        );
        store.insert_task(task);

        assert_eq!(store.get_task(&TaskId::new("t-1")).unwrap().repair_count, 0);
        store.increment_repair_count(&TaskId::new("t-1"));
        assert_eq!(store.get_task(&TaskId::new("t-1")).unwrap().repair_count, 1);
        store.increment_repair_count(&TaskId::new("t-1"));
        assert_eq!(store.get_task(&TaskId::new("t-1")).unwrap().repair_count, 2);

        assert_eq!(store.get_task(&TaskId::new("t-1")).unwrap().retry_count, 0);
        store.increment_retry_count(&TaskId::new("t-1"));
        assert_eq!(store.get_task(&TaskId::new("t-1")).unwrap().retry_count, 1);
    }

    // -----------------------------------------------------------------------
    // Test 22: checkpoint restore preserves projection
    // -----------------------------------------------------------------------

    #[test]
    fn checkpoint_restore_preserves_projection() {
        let store = TaskStore::new();

        let obj = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let phase = make_task(
            "phase-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Phase,
            TaskStatus::Running,
        );
        let wp = make_task(
            "wp-1",
            "m-1",
            "obj-1",
            Some("phase-1"),
            TaskKind::WorkPackage,
            TaskStatus::Running,
        );
        let act1 = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("wp-1"),
            TaskKind::Action,
            TaskStatus::Completed,
        );
        let act2 = make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("wp-1"),
            TaskKind::Action,
            TaskStatus::Running,
        );

        store.insert_task(obj);
        store.insert_task(phase);
        store.insert_task(wp);
        store.insert_task(act1);
        store.insert_task(act2);

        let original_projection = store.build_projection(&TaskId::new("obj-1")).unwrap();

        // Checkpoint and restore
        let data = store.checkpoint();
        let restored = TaskStore::restore(&data);

        let restored_projection = restored.build_projection(&TaskId::new("obj-1")).unwrap();

        assert_eq!(
            original_projection.progress_summary.total_tasks,
            restored_projection.progress_summary.total_tasks
        );
        assert_eq!(
            original_projection.progress_summary.completed_tasks,
            restored_projection.progress_summary.completed_tasks
        );
        assert_eq!(
            original_projection.progress_summary.running_tasks,
            restored_projection.progress_summary.running_tasks
        );
        assert_eq!(
            original_projection.aggregate_status,
            restored_projection.aggregate_status
        );
    }

    #[test]
    fn reconcile_volatile_runtime_after_restore_revokes_active_leases_and_blocks_running_subtree() {
        let store = TaskStore::new();

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let phase = make_task(
            "phase-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Phase,
            TaskStatus::Running,
        );
        let wp = make_task(
            "wp-1",
            "m-1",
            "obj-1",
            Some("phase-1"),
            TaskKind::WorkPackage,
            TaskStatus::Running,
        );
        let act = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("wp-1"),
            TaskKind::Action,
            TaskStatus::Running,
        );

        store.insert_task(root);
        store.insert_task(phase);
        store.insert_task(wp);
        store.insert_task(act);

        let lease = store
            .grant_lease(
                &TaskId::new("act-1"),
                &TaskId::new("obj-1"),
                &WorkerId::new("worker-1"),
                "executor",
                60_000,
            )
            .expect("running task should receive active lease");
        assert_eq!(lease.lease_status, TaskLeaseState::Active);

        let (revoked_leases, blocked_tasks) = store
            .reconcile_volatile_runtime_after_restore(&WorkerRuntimeDurableSnapshot::default());
        assert_eq!(revoked_leases, 1);
        assert_eq!(blocked_tasks, 4);
        assert!(store.get_active_lease(&TaskId::new("act-1")).is_none());

        for task_id in ["obj-1", "phase-1", "wp-1", "act-1"] {
            let task = store
                .get_task(&TaskId::new(task_id))
                .expect("task should exist after reconciliation");
            assert_eq!(
                task.status,
                TaskStatus::Blocked,
                "{task_id} should become Blocked"
            );
        }
    }

    #[test]
    fn reconcile_volatile_runtime_after_restore_blocks_snapshot_tracked_branch_without_active_lease()
     {
        let store = TaskStore::new();

        let root = make_task(
            "obj-2",
            "m-2",
            "obj-2",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let phase = make_task(
            "phase-2",
            "m-2",
            "obj-2",
            Some("obj-2"),
            TaskKind::Phase,
            TaskStatus::Running,
        );
        let wp = make_task(
            "wp-2",
            "m-2",
            "obj-2",
            Some("phase-2"),
            TaskKind::WorkPackage,
            TaskStatus::Running,
        );
        let act = make_task(
            "act-2",
            "m-2",
            "obj-2",
            Some("wp-2"),
            TaskKind::Action,
            TaskStatus::Running,
        );

        store.insert_task(root);
        store.insert_task(phase);
        store.insert_task(wp);
        store.insert_task(act);

        let (revoked_leases, blocked_tasks) =
            store.reconcile_volatile_runtime_after_restore(&WorkerRuntimeDurableSnapshot {
                branches: vec![magi_worker_runtime::WorkerRuntimeBranchSnapshot {
                    task_id: TaskId::new("act-2"),
                    worker_id: WorkerId::new("worker-2"),
                    stage: magi_worker_runtime::WorkerStage::Execute,
                    lease_id: Some("snapshot-lease-2".to_string()),
                    execution_intent_ref: Some("worker-intent-act-2".to_string()),
                    binding_lifecycle: None,
                    checkpoint_cursor: Some(magi_worker_runtime::WorkerExecutionCheckpointCursor {
                        checkpoint_stage: magi_worker_runtime::WorkerStage::Execute,
                        next_step_index: 1,
                        checkpoint_at: UtcMillis::now(),
                        resume_mode:
                            magi_worker_runtime::WorkerCheckpointResumeMode::StepCheckpoint,
                        resume_token: None,
                    }),
                }],
            });
        assert_eq!(revoked_leases, 0);
        assert_eq!(blocked_tasks, 4);

        for task_id in ["obj-2", "phase-2", "wp-2", "act-2"] {
            let task = store
                .get_task(&TaskId::new(task_id))
                .expect("task should exist after snapshot reconciliation");
            assert_eq!(
                task.status,
                TaskStatus::Blocked,
                "{task_id} should become Blocked"
            );
        }
    }

    #[test]
    fn reconcile_volatile_runtime_after_restore_ignores_completed_branch_snapshots() {
        let store = TaskStore::new();

        let root = make_task(
            "obj-3",
            "m-3",
            "obj-3",
            None,
            TaskKind::Objective,
            TaskStatus::Completed,
        );
        let phase = make_task(
            "phase-3",
            "m-3",
            "obj-3",
            Some("obj-3"),
            TaskKind::Phase,
            TaskStatus::Completed,
        );
        let act = make_task(
            "act-3",
            "m-3",
            "obj-3",
            Some("phase-3"),
            TaskKind::Action,
            TaskStatus::Completed,
        );

        store.insert_task(root);
        store.insert_task(phase);
        store.insert_task(act);

        let (revoked_leases, blocked_tasks) =
            store.reconcile_volatile_runtime_after_restore(&WorkerRuntimeDurableSnapshot {
                branches: vec![magi_worker_runtime::WorkerRuntimeBranchSnapshot {
                    task_id: TaskId::new("act-3"),
                    worker_id: WorkerId::new("worker-3"),
                    stage: magi_worker_runtime::WorkerStage::Finish,
                    lease_id: None,
                    execution_intent_ref: Some("worker-intent-act-3".to_string()),
                    binding_lifecycle: None,
                    checkpoint_cursor: None,
                }],
            });
        assert_eq!(revoked_leases, 0);
        assert_eq!(blocked_tasks, 0);

        for task_id in ["obj-3", "phase-3", "act-3"] {
            let task = store
                .get_task(&TaskId::new(task_id))
                .expect("completed task should remain in store");
            assert_eq!(task.status, TaskStatus::Completed);
        }
    }

    // -----------------------------------------------------------------------
    // Test: resolve_decision abort 取消整棵根任务子树
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_decision_abort_cancels_root_subtree() {
        let store = TaskStore::new();

        // root -> phase -> [action, decision]
        store.insert_task(make_task(
            "root-1",
            "m-1",
            "root-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        ));
        store.insert_task(make_task(
            "phase-1",
            "m-1",
            "root-1",
            Some("root-1"),
            TaskKind::Phase,
            TaskStatus::Running,
        ));
        store.insert_task(make_task(
            "action-1",
            "m-1",
            "root-1",
            Some("phase-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        ));

        let mut decision = make_task(
            "dec-1",
            "m-1",
            "root-1",
            Some("phase-1"),
            TaskKind::Decision,
            TaskStatus::AwaitingApproval,
        );
        decision.decision_payload = Some(make_decision_payload(
            "需要中止",
            "任务失败",
            Some("action-1"),
            "abort",
        ));
        store.insert_task(decision);

        store
            .resolve_decision(&TaskId::new("dec-1"), "abort", None)
            .expect("abort should resolve");

        // Decision 本身应已 Completed
        assert_eq!(
            store.get_task(&TaskId::new("dec-1")).unwrap().status,
            TaskStatus::Completed,
        );
        // 其余非终态节点全部 Cancelled
        assert_eq!(
            store.get_task(&TaskId::new("root-1")).unwrap().status,
            TaskStatus::Cancelled,
        );
        assert_eq!(
            store.get_task(&TaskId::new("phase-1")).unwrap().status,
            TaskStatus::Cancelled,
        );
        assert_eq!(
            store.get_task(&TaskId::new("action-1")).unwrap().status,
            TaskStatus::Cancelled,
        );
    }

    // -----------------------------------------------------------------------
    // Test: resolve_decision skip 跳过目标子树
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_decision_skip_skips_target_subtree() {
        let store = TaskStore::new();

        // root -> [action-1(Ready), action-2(Running), decision]
        store.insert_task(make_task(
            "root-1",
            "m-1",
            "root-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        ));
        store.insert_task(make_task(
            "action-1",
            "m-1",
            "root-1",
            Some("root-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        ));
        store.insert_task(make_task(
            "action-2",
            "m-1",
            "root-1",
            Some("root-1"),
            TaskKind::Action,
            TaskStatus::Running,
        ));

        let mut decision = make_task(
            "dec-1",
            "m-1",
            "root-1",
            Some("root-1"),
            TaskKind::Decision,
            TaskStatus::AwaitingApproval,
        );
        decision.decision_payload = Some(make_decision_payload(
            "跳过失败动作",
            "action-1 可跳过",
            Some("action-1"),
            "skip",
        ));
        store.insert_task(decision);

        store
            .resolve_decision(&TaskId::new("dec-1"), "skip", None)
            .expect("skip should resolve");

        // Decision 完成
        assert_eq!(
            store.get_task(&TaskId::new("dec-1")).unwrap().status,
            TaskStatus::Completed,
        );
        // 目标 action-1 被跳过
        assert_eq!(
            store.get_task(&TaskId::new("action-1")).unwrap().status,
            TaskStatus::Skipped,
        );
        // 不相关的 action-2 不受影响
        assert_eq!(
            store.get_task(&TaskId::new("action-2")).unwrap().status,
            TaskStatus::Running,
        );
        // root 不受影响
        assert_eq!(
            store.get_task(&TaskId::new("root-1")).unwrap().status,
            TaskStatus::Running,
        );
    }

    // -----------------------------------------------------------------------
    // Test: resolve_decision 普通选项释放直接依赖 Decision 的下游任务
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_decision_releases_direct_dependents() {
        let store = TaskStore::new();

        // root -> [decision, downstream(Blocked, depends on decision)]
        store.insert_task(make_task(
            "root-1",
            "m-1",
            "root-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        ));

        let mut decision = make_task(
            "dec-1",
            "m-1",
            "root-1",
            Some("root-1"),
            TaskKind::Decision,
            TaskStatus::AwaitingApproval,
        );
        decision.decision_payload = Some(make_decision_payload(
            "选择方案",
            "需要确认",
            None,
            "continue",
        ));
        store.insert_task(decision);

        let mut downstream = make_task(
            "downstream-1",
            "m-1",
            "root-1",
            Some("root-1"),
            TaskKind::Action,
            TaskStatus::Blocked,
        );
        downstream.dependency_ids = vec![TaskId::new("dec-1")];
        store.insert_task(downstream);

        store
            .resolve_decision(&TaskId::new("dec-1"), "continue", None)
            .expect("continue should resolve");

        // Decision 完成
        assert_eq!(
            store.get_task(&TaskId::new("dec-1")).unwrap().status,
            TaskStatus::Completed,
        );
        // 下游依赖 Decision 且所有依赖均 Completed → 释放为 Ready
        assert_eq!(
            store.get_task(&TaskId::new("downstream-1")).unwrap().status,
            TaskStatus::Ready,
        );
    }

    // -----------------------------------------------------------------------
    // Test: default_frozen_policy 返回保守默认值
    // -----------------------------------------------------------------------

    #[test]
    fn default_frozen_policy_values() {
        let policy = default_frozen_policy();
        assert_eq!(policy.autonomy_level, "Assisted");
        assert_eq!(policy.approval_mode, "Interactive");
        assert!(!policy.background_allowed);
        assert_eq!(policy.retry_limit, 1);
        assert_eq!(policy.repair_limit, 1);
        assert!(policy.validation_profile.is_none());
        assert!(policy.escalation_conditions.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test: build_projection 正确填充 execution_mode 和 runner_status
    // -----------------------------------------------------------------------

    #[test]
    fn build_projection_execution_mode_and_runner_status() {
        let store = TaskStore::new();

        let mut root = make_task(
            "root-1",
            "m-1",
            "root-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        root.policy_snapshot = Some(TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 3,
            repair_limit: 3,
            validation_profile: Some("Required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            background_allowed: true,
            escalation_conditions: vec!["on_failure".to_string()],
        });
        store.insert_task(root);

        let projection = store.build_projection(&TaskId::new("root-1")).unwrap();
        assert_eq!(
            projection.execution_mode, "deep",
            "background_allowed=true 时应为 deep"
        );
        assert_eq!(
            projection.runner_status, "running",
            "有 Running 任务时应为 running"
        );

        // 改为普通模式
        let mut root2 = make_task(
            "root-2",
            "m-1",
            "root-2",
            None,
            TaskKind::Objective,
            TaskStatus::Blocked,
        );
        root2.policy_snapshot = Some(TaskPolicy {
            autonomy_level: "Assisted".to_string(),
            approval_mode: "Interactive".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 1,
            repair_limit: 1,
            validation_profile: None,
            checkpoint_mode: "turn".to_string(),
            background_allowed: false,
            escalation_conditions: Vec::new(),
        });
        store.insert_task(root2);

        let projection2 = store.build_projection(&TaskId::new("root-2")).unwrap();
        assert_eq!(
            projection2.execution_mode, "normal",
            "background_allowed=false 时应为 normal"
        );
        assert_eq!(
            projection2.runner_status, "blocked",
            "Blocked 状态时应为 blocked"
        );
    }

    // -----------------------------------------------------------------------
    // Test: build_delivery_package 聚合交付包字段
    // -----------------------------------------------------------------------

    #[test]
    fn build_delivery_package_aggregates_correctly() {
        let store = TaskStore::new();

        let mut root = make_task(
            "root-1",
            "m-1",
            "root-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        root.goal = "实现用户登录功能".to_string();
        root.workspace_scope = Some("src/auth".to_string());
        root.policy_snapshot = Some(TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 3,
            repair_limit: 3,
            validation_profile: Some("Required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            background_allowed: true,
            escalation_conditions: vec!["on_failure".to_string()],
        });
        store.insert_task(root);

        let mut action = make_task(
            "act-1",
            "m-1",
            "root-1",
            Some("root-1"),
            TaskKind::Action,
            TaskStatus::Completed,
        );
        action.output_refs = vec![
            "src/auth/login.rs".to_string(),
            "{\"blocks\":[{\"content\":\"只读验收完成\",\"type\":\"text\"}]}".to_string(),
            "已完成 /Users/xie/code/TEST 只读检查".to_string(),
        ];
        action.evidence_refs = vec!["test://login-passed".to_string()];
        store.insert_task(action);

        let mut validation = make_task(
            "val-1",
            "m-1",
            "root-1",
            Some("root-1"),
            TaskKind::Validation,
            TaskStatus::Completed,
        );
        validation.title = "验证登录模块".to_string();
        validation.evidence_refs = vec!["test://login-validation-passed".to_string()];
        store.insert_task(validation);

        let mut decision = make_task(
            "dec-1",
            "m-1",
            "root-1",
            Some("root-1"),
            TaskKind::Decision,
            TaskStatus::Completed,
        );
        decision.decision_payload = Some(make_decision_payload(
            "是否引入第三方库",
            "需要确认依赖方案",
            None,
            "use-oauth",
        ));
        decision.output_refs = vec!["use-oauth".to_string()];
        store.insert_task(decision);

        let package = store
            .build_delivery_package(&TaskId::new("root-1"))
            .unwrap();
        let pkg = package.as_object().unwrap();

        assert_eq!(
            pkg.get("goal").and_then(|v| v.as_str()),
            Some("实现用户登录功能")
        );
        assert_eq!(pkg.get("scope").and_then(|v| v.as_str()), Some("src/auth"));
        assert_eq!(
            pkg.get("execution_mode").and_then(|v| v.as_str()),
            Some("deep")
        );
        assert_eq!(
            pkg.get("aggregate_status").and_then(|v| v.as_str()),
            Some("Running")
        );

        let file_changes = pkg.get("file_changes").unwrap().as_array().unwrap();
        assert!(
            file_changes
                .iter()
                .any(|v| v.as_str() == Some("src/auth/login.rs"))
        );
        assert!(
            !file_changes.iter().any(|v| v
                .as_str()
                .is_some_and(|value| value.contains("\"blocks\"") || value.contains("只读检查"))),
            "交付包文件变更不应暴露 assistant 输出 JSON 或普通说明文本"
        );

        let evidence_list = pkg.get("evidence_list").unwrap().as_array().unwrap();
        assert!(
            evidence_list
                .iter()
                .any(|v| v.as_str() == Some("test://login-passed"))
        );
        assert!(
            evidence_list
                .iter()
                .any(|v| v.as_str() == Some("test://login-validation-passed"))
        );

        let validation_results = pkg.get("validation_results").unwrap().as_array().unwrap();
        assert_eq!(validation_results.len(), 1);
        assert_eq!(
            validation_results[0]
                .get("task_id")
                .and_then(|v| v.as_str()),
            Some("val-1")
        );

        let key_decisions = pkg.get("key_decisions").unwrap().as_array().unwrap();
        assert_eq!(key_decisions.len(), 1);
        assert_eq!(
            key_decisions[0]
                .get("chosen_option")
                .and_then(|v| v.as_str()),
            Some("use-oauth")
        );

        let progress = pkg.get("progress").unwrap().as_object().unwrap();
        assert_eq!(progress.get("total").and_then(|v| v.as_u64()), Some(4));
        assert_eq!(progress.get("completed").and_then(|v| v.as_u64()), Some(3));
    }
}
