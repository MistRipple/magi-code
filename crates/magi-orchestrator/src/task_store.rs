use magi_core::{
    AccessProfile, AgentContextAccessRecord, AgentContextPackage, AgentContextSupplement,
    AgentRunProjection, DomainError, DomainResult, LeaseId, MissionId, ProgressSummary, Task,
    TaskCompletionAttempt, TaskId, TaskPolicy, TaskRuntimePayload, TaskStatus, TaskTier, UtcMillis,
    WorkerId,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

static LEASE_COUNTER: AtomicU64 = AtomicU64::new(1);
static TASK_PROJECTION_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static TASK_PROJECTION_WRITE_LOCK: Mutex<()> = Mutex::new(());
#[cfg(test)]
type TaskProjectionAfterSnapshotHook = std::sync::Arc<dyn Fn(&Path) + Send + Sync>;

#[cfg(test)]
static TASK_PROJECTION_AFTER_SNAPSHOT_HOOK: Mutex<Option<TaskProjectionAfterSnapshotHook>> =
    Mutex::new(None);

const TASK_PROJECTION_SCHEMA_VERSION: u32 = 2;
const TASK_PROJECTION_MANIFEST_FILE: &str = "manifest.json";
const TASK_PROJECTION_GENERATIONS_DIR: &str = "generations";

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

enum SubtreeTerminalSelection<'a> {
    Audit,
    Execution,
    OrphanedExecution,
    ExecutionIfActive(&'a [TaskId]),
}

#[derive(Debug, Default)]
struct TaskStoreCheckpoint {
    tasks: Vec<Task>,
    leases: Vec<TaskLease>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskProjectionManifest {
    schema_version: u32,
    generation: u64,
    roots: Vec<TaskProjectionManifestRoot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskProjectionManifestRoot {
    root_task_id: TaskId,
    file_name: String,
    /// 该 root 文件实际来自哪个 generation。新的 generation 可以通过硬链接
    /// 复用内容不变的 root，因此它不一定等于 manifest 的 generation。
    generation: u64,
    /// 不包含 generation 字段的 root 业务事实指纹，用于 checkpoint 增量复用。
    content_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskRootProjection {
    schema_version: u32,
    generation: u64,
    root_task_id: TaskId,
    tasks: Vec<Task>,
    leases: Vec<TaskLease>,
}

/// Callback invoked after a successful `update_status` call.
///
/// Receives the task ID, old status, new status, and a snapshot of the task
/// after the status change. Implementations should be lightweight (e.g.
/// publish an event).
pub type StatusChangeCallback = Box<dyn Fn(&TaskId, TaskStatus, TaskStatus, Task) + Send + Sync>;
type SharedStatusChangeCallback = Arc<dyn Fn(&TaskId, TaskStatus, TaskStatus, Task) + Send + Sync>;
/// 待提交的任务事实快照。
///
/// checkpoint 回调只能看到候选快照，不能读取或修改 TaskStore。这样持久化失败时，
/// 内存状态不会先于 durable projection 生效；成功后调用方再一次性提交候选状态。
#[derive(Clone, Debug)]
pub struct TaskStoreSnapshot {
    pub tasks: Vec<Task>,
    pub leases: Vec<TaskLease>,
    /// 本次 checkpoint 之前发生变化的 root。为空表示调用方要求完整快照。
    /// 该字段只用于增量持久化决策，不改变任务事实本身。
    pub changed_root_ids: Vec<TaskId>,
}

pub type CheckpointCallback = Box<dyn Fn(&TaskStoreSnapshot) -> DomainResult<()> + Send + Sync>;

/// 任务调度的内存存储，维护任务、租约及其索引。
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
    on_status_change: Mutex<Option<SharedStatusChangeCallback>>,
    /// Optional callback fired on every successful status change for checkpoint
    /// persistence (design 6.8).
    on_checkpoint: Mutex<Option<CheckpointCallback>>,
    /// Serializes every mutation and every checkpoint snapshot. The three maps
    /// are kept as separate locks for focused read paths, but no committed
    /// snapshot may observe a cross-map intermediate state.
    mutation_lock: Mutex<()>,
    status_change_version: Mutex<u64>,
    runtime_change_notify: tokio::sync::Notify,
    status_change_signal: Condvar,
    /// 生产 daemon 设为 true，把可能触发跨模块 IO 的回调移出 mutation guard；
    /// 默认 false 供同步嵌入测试保持确定的 callback 观察语义。
    status_change_callback_async: AtomicBool,
}

fn default_frozen_policy() -> TaskPolicy {
    TaskPolicy {
        autonomy_level: "Assisted".to_string(),
        access_profile: AccessProfile::Restricted,
        allowed_tools: Vec::new(),
        denied_tools: Vec::new(),
        allowed_paths: Vec::new(),
        denied_paths: Vec::new(),
        read_only_paths: Vec::new(),
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

fn collect_subtree_ids_from_tasks(tasks: &HashMap<TaskId, Task>, root_id: &TaskId) -> Vec<TaskId> {
    collect_subtree_ids_in_scope(tasks, root_id, false)
}

// 审计父子关系永不改写；执行范围在已持久转交的直接子树根处停止。
fn collect_subtree_ids_in_scope(
    tasks: &HashMap<TaskId, Task>,
    root_id: &TaskId,
    execution_only: bool,
) -> Vec<TaskId> {
    let children = child_index_from_tasks(tasks);
    let mut all_ids = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut queue = vec![root_id.clone()];
    while let Some(current) = queue.pop() {
        // 持久化数据损坏时 parent_task_id 可能成环；遍历必须有界。
        if !tasks.contains_key(&current) || !visited.insert(current.clone()) {
            continue;
        }
        if execution_only
            && &current != root_id
            && tasks
                .get(&current)
                .is_some_and(|task| task.agent_run_transfer().is_some())
        {
            continue;
        }
        all_ids.push(current.clone());
        if let Some(child_ids) = children.get(&current) {
            queue.extend(child_ids.iter().cloned());
        }
    }
    all_ids
}

mod notifications;
mod projection;
mod leases;
mod agents;

impl TaskStore {
    pub fn new() -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            leases: RwLock::new(HashMap::new()),
            mission_index: RwLock::new(HashMap::new()),
            on_status_change: Mutex::new(None),
            on_checkpoint: Mutex::new(None),
            mutation_lock: Mutex::new(()),
            status_change_version: Mutex::new(0),
            runtime_change_notify: tokio::sync::Notify::new(),
            status_change_signal: Condvar::new(),
            status_change_callback_async: AtomicBool::new(false),
        }
    }

    fn validate_task_root(
        tasks: &HashMap<TaskId, Task>,
        task_id: &TaskId,
        root_task_id: &TaskId,
    ) -> DomainResult<()> {
        let task = tasks
            .get(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        let root = tasks
            .get(root_task_id)
            .ok_or(DomainError::NotFound { entity: "RootTask" })?;
        if task.root_task_id != *root_task_id {
            return Err(DomainError::InvalidState {
                message: format!(
                    "任务 {} 不属于 root task {}，实际归属为 {}",
                    task_id, root_task_id, task.root_task_id
                ),
            });
        }
        if root.task_id != *root_task_id || root.root_task_id != *root_task_id {
            return Err(DomainError::InvalidState {
                message: format!("{} 不是合法的 root task", root_task_id),
            });
        }
        if task.mission_id != root.mission_id {
            return Err(DomainError::InvalidState {
                message: format!(
                    "任务 {} 与 root task {} 不属于同一 mission",
                    task_id, root_task_id
                ),
            });
        }
        Ok(())
    }

    /// 插入一个任务并更新索引。
    pub fn insert_task(&self, task: Task) -> DomainResult<()> {
        self.insert_task_inner(task, true)
    }

    /// 插入一个任务并更新索引，但把 checkpoint 交给后续状态迁移或 accepted journal。
    ///
    /// 派发提交阶段不能因为已有 task-store 全量快照而阻塞 HTTP accepted；调用方必须
    /// 在返回 accepted 前写入自己的小型恢复记录，随后由后台状态迁移完成完整 checkpoint。
    pub fn insert_task_without_checkpoint(&self, task: Task) -> DomainResult<()> {
        self.insert_task_inner(task, false)
    }

    fn insert_task_inner(&self, task: Task, checkpoint: bool) -> DomainResult<()> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let task_id = task.task_id.clone();
        let root_task_id = task.root_task_id.clone();
        let mission_id = task.mission_id.clone();
        if !checkpoint {
            let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
            if tasks.contains_key(&task_id) {
                return Err(DomainError::InvalidState {
                    message: format!("任务 {task_id} 已存在，禁止覆盖任务事实"),
                });
            }
            Self::validate_agent_activation_insert(&tasks, &task)?;
            tasks.insert(task_id.clone(), task);
            drop(tasks);

            self.mission_index
                .write()
                .expect("mission_index write lock poisoned")
                .entry(mission_id)
                .or_default()
                .push(task_id);
            return Ok(());
        }

        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        let mut mission_index = self
            .mission_index
            .read()
            .expect("mission_index read lock poisoned")
            .clone();
        if tasks.contains_key(&task_id) {
            return Err(DomainError::InvalidState {
                message: format!("任务 {task_id} 已存在，禁止覆盖任务事实"),
            });
        }
        Self::validate_agent_activation_insert(&tasks, &task)?;
        tasks.insert(task_id.clone(), task);
        mission_index
            .entry(mission_id)
            .or_default()
            .push(task_id);
        if checkpoint {
            let leases = self.leases.read().expect("leases read lock poisoned");
            let snapshot = Self::snapshot_from_maps_with_dirty_roots(
                &tasks,
                &leases,
                vec![root_task_id],
            );
            self.fire_checkpoint(&snapshot)?;
        }
        *self.tasks.write().expect("tasks write lock poisoned") = tasks;
        *self
            .mission_index
            .write()
            .expect("mission_index write lock poisoned") = mission_index;
        Ok(())
    }

    /// 通过 ID 获取任务。
    pub fn get_task(&self, task_id: &TaskId) -> Option<Task> {
        self.tasks
            .read()
            .expect("tasks read lock poisoned")
            .get(task_id)
            .cloned()
    }

    /// 返回当前任务快照。用于从持久化 Task.parent_task_id 重建运行期拓扑。
    pub fn all_tasks(&self) -> Vec<Task> {
        self.tasks
            .read()
            .expect("tasks read lock poisoned")
            .values()
            .cloned()
            .collect()
    }

    /// 更新任务状态，带状态迁移合法性校验。
    /// 普通状态更新只允许写入非终态；终态必须使用对应的原子提交接口。
    pub fn update_status_checked(
        &self,
        task_id: &TaskId,
        new_status: TaskStatus,
    ) -> DomainResult<()> {
        if is_terminal_status(new_status) {
            return Err(DomainError::InvalidState {
                message: format!(
                    "任务终态 {:?} 不能通过普通状态更新提交，必须使用 TaskStore 原子终态接口",
                    new_status
                ),
            });
        }
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
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
        if old_status == TaskStatus::Pending
            && new_status == TaskStatus::Pending
            && task.policy_snapshot.is_none()
        {
            task.policy_snapshot = Some(default_frozen_policy());
        }
        task.status = new_status;
        task.updated_at = UtcMillis::now();
        let cloned_task = task.clone();
        let leases = self.leases.read().expect("leases read lock poisoned");
        let snapshot = Self::snapshot_from_maps_with_dirty_roots(
            &tasks,
            &leases,
            vec![cloned_task.root_task_id.clone()],
        );
        self.fire_checkpoint(&snapshot)?;
        *self.tasks.write().expect("tasks write lock poisoned") = tasks;
        drop(_mutation_guard);
        self.emit_status_change(task_id, old_status, new_status, cloned_task);
        Ok(())
    }

    fn transition_recovery_status(
        &self,
        task_id: &TaskId,
        expected_status: TaskStatus,
        new_status: TaskStatus,
        root_only: bool,
    ) -> DomainResult<()> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        let leases = self
            .leases
            .read()
            .expect("leases read lock poisoned")
            .clone();
        let task = tasks
            .get(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        if root_only && (task.root_task_id != *task_id || task.parent_task_id.is_some()) {
            return Err(DomainError::InvalidState {
                message: format!("{} 不是合法的 root task", task_id),
            });
        }
        if task.status != expected_status {
            return Err(DomainError::InvalidState {
                message: format!(
                    "任务 {} 只能从 {:?} 恢复为 {:?}，当前状态为 {:?}",
                    task_id, expected_status, new_status, task.status
                ),
            });
        }
        if leases
            .values()
            .any(|lease| lease.task_id == *task_id && lease.lease_status == TaskLeaseState::Active)
        {
            return Err(DomainError::InvalidState {
                message: format!("任务 {} 仍有活跃租约，不能执行恢复状态迁移", task_id),
            });
        }
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        // 主线 Continue 与独立代理继续使用同一恢复代际。毫秒时间可能相同，
        // 旧终态回调不能仅靠 updated_at 被判定为本次执行的结果。
        if matches!(new_status, TaskStatus::Pending | TaskStatus::Running) {
            task.retry_count =
                task.retry_count
                    .checked_add(1)
                    .ok_or_else(|| DomainError::InvalidState {
                        message: "恢复代际溢出".into(),
                    })?;
        }
        task.status = new_status;
        task.updated_at = UtcMillis::now();
        let cloned_task = task.clone();
        let mission_index = self
            .mission_index
            .read()
            .expect("mission_index read lock poisoned")
            .clone();
        let snapshot = Self::snapshot_from_maps_with_dirty_roots(
            &tasks,
            &leases,
            vec![cloned_task.root_task_id.clone()],
        );
        self.fire_checkpoint(&snapshot)?;
        self.commit_maps(tasks, leases, mission_index);
        drop(_mutation_guard);
        self.emit_status_change(task_id, expected_status, new_status, cloned_task);
        Ok(())
    }

    /// 将失败任务重新置为 Pending，供恢复入口重新派发。
    pub fn reopen_failed_task_for_recovery(&self, task_id: &TaskId) -> DomainResult<()> {
        self.transition_recovery_status(task_id, TaskStatus::Failed, TaskStatus::Pending, false)
    }

    /// 将失败的 root task 置为 Running，供指定 branch 的恢复入口启动执行树。
    pub fn start_failed_root_for_recovery(&self, root_task_id: &TaskId) -> DomainResult<()> {
        self.transition_recovery_status(root_task_id, TaskStatus::Failed, TaskStatus::Running, true)
    }

    fn apply_completion(task: &mut Task, attempt: TaskCompletionAttempt) -> DomainResult<()> {
        if task.status != TaskStatus::Running {
            return Err(DomainError::InvalidState {
                message: format!(
                    "任务 {} 只能从 Running 提交完成，当前状态为 {:?}",
                    task.task_id, task.status
                ),
            });
        }
        task.completion_contract
            .validate(&attempt)
            .map_err(|message| DomainError::InvalidState { message })?;
        task.output_refs = attempt.output_refs;
        task.evidence_refs = attempt
            .evidence
            .into_iter()
            .map(|evidence| {
                serde_json::to_string(&evidence).expect("TaskCompletionEvidence 必须能够序列化")
            })
            .collect();
        task.status = TaskStatus::Completed;
        task.updated_at = UtcMillis::now();
        Ok(())
    }

    fn apply_terminal_status(
        task: &mut Task,
        status: TaskStatus,
        output_refs: Vec<String>,
    ) -> DomainResult<()> {
        if !matches!(status, TaskStatus::Failed | TaskStatus::Killed) {
            return Err(DomainError::InvalidState {
                message: format!("任务终态只能是 Failed 或 Killed，收到 {:?}", status),
            });
        }
        if !matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
            return Err(DomainError::InvalidState {
                message: format!("任务 {} 当前状态 {:?} 不能收口", task.task_id, task.status),
            });
        }
        if !output_refs.is_empty() {
            task.output_refs = output_refs;
        }
        task.status = status;
        task.updated_at = UtcMillis::now();
        Ok(())
    }

    /// 原子验证完成合同并提交任务终态。
    ///
    /// 这是任务系统写入 `Completed` 的唯一入口。工具循环、确定性任务、子代理和恢复
    /// 任务都只能提交 `TaskCompletionAttempt`，不能直接修改状态。
    pub fn complete_task(
        &self,
        task_id: &TaskId,
        attempt: TaskCompletionAttempt,
    ) -> DomainResult<()> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        let leases = self
            .leases
            .read()
            .expect("leases read lock poisoned")
            .clone();
        if leases
            .values()
            .any(|lease| lease.task_id == *task_id && lease.lease_status == TaskLeaseState::Active)
        {
            return Err(DomainError::InvalidState {
                message: format!(
                    "任务 {} 仍有活跃租约，必须通过 TaskStore::complete_lease_and_task 提交完成",
                    task_id
                ),
            });
        }
        let old_status = task.status;
        Self::apply_completion(task, attempt)?;
        let cloned_task = task.clone();
        let snapshot = Self::snapshot_from_maps_with_dirty_roots(
            &tasks,
            &leases,
            vec![cloned_task.root_task_id.clone()],
        );
        self.fire_checkpoint(&snapshot)?;
        *self.tasks.write().expect("tasks write lock poisoned") = tasks;
        drop(_mutation_guard);
        self.emit_status_change(task_id, old_status, TaskStatus::Completed, cloned_task);
        Ok(())
    }

    /// 将异常提前完成的 root task 重新打开为可恢复失败态。
    ///
    /// 这不是普通状态迁移：只有恢复入口在确认仍有可恢复 branch 时才能调用，且必须
    /// 在同一事务中确认 root task 没有活跃租约。这样恢复不会绕过任务投影 checkpoint，
    /// 也不会留下 `Completed + Active lease` 的不一致状态。
    pub fn reopen_completed_root_for_recovery(&self, root_task_id: &TaskId) -> DomainResult<()> {
        self.transition_recovery_status(
            root_task_id,
            TaskStatus::Completed,
            TaskStatus::Failed,
            true,
        )
    }

    /// 原子持久化工作流阶段、检查点、待结算动作和最近结果。动作记录与租约
    /// 同一事务提交，恢复时可以区分“尚未执行”与“已执行但尚未进入下一决策”。
    /// 将工作流交回用户：保存等待动作、释放执行租约并将任务置回 Pending。
    /// 写入用户回答并解除 wait_user 阻塞；任务仍由原 Runner 入口重新派发。
    pub fn update_task_goal(&self, task_id: &TaskId, goal: String) -> DomainResult<()> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        task.goal = goal;
        task.updated_at = UtcMillis::now();
        Ok(())
    }

    /// BFS 收集以 root_id 为根的整棵子树中所有任务 ID。
    pub fn collect_subtree_ids(&self, root_id: &TaskId) -> Vec<TaskId> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        collect_subtree_ids_from_tasks(&tasks, root_id)
    }

    /// 当前拥有方负责的执行子树；显式以转交根查询时包含该转交根及其后代。
    /// 历史、上下文、删除与持久化仍使用 collect_subtree_ids 的完整审计树。
    pub fn collect_execution_subtree_ids(&self, root_id: &TaskId) -> Vec<TaskId> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        collect_subtree_ids_in_scope(&tasks, root_id, true)
    }

    /// 同一读快照中的当前执行范围；转交子树保留在审计树中，不计入原主线。
    pub fn execution_subtree_tasks(&self, root_id: &TaskId) -> Vec<Task> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        collect_subtree_ids_in_scope(&tasks, root_id, true)
            .into_iter()
            .filter_map(|id| tasks.get(&id).cloned())
            .collect()
    }

    /// 调度身份与审计 root 分离；转交不生成新 Task/root/lease 或另一棵任务树。
    pub fn execution_scope_root(&self, task_id: &TaskId) -> DomainResult<TaskId> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        Self::execution_scope_root_in_tasks(&tasks, task_id)
    }

    /// 计划保留历史绑定；是否阻挡目标完成只看同一 TaskStore 快照中的状态和拥有方。
    pub fn goal_has_active_bound_tasks(
        &self,
        completing_root: &TaskId,
        session: &magi_core::SessionId,
        goal: &magi_core::GoalId,
        bound_tasks: &[TaskId],
    ) -> DomainResult<bool> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        Self::validate_task_root(&tasks, completing_root, completing_root)?;
        let mut active = false;
        for id in bound_tasks {
            let task = tasks.get(id).ok_or(DomainError::NotFound {
                entity: "GoalBoundTask",
            })?;
            if id == completing_root
                || !matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
            {
                continue;
            }
            let scope = Self::execution_scope_root_in_tasks(&tasks, id)?;
            if scope == *completing_root {
                active = true;
            } else if let Some((_, transfer)) = Self::agent_run_transfer_in_tasks(&tasks, id)? {
                if transfer.session_id != *session {
                    return Err(DomainError::InvalidState {
                        message: "目标绑定任务的转交会话不匹配".into(),
                    });
                }
                active |= matches!(&transfer.owner, magi_core::AgentRunOwner::Goal { goal_id, .. } if goal_id == goal);
            } else {
                return Err(DomainError::InvalidState {
                    message: "目标绑定的活动任务不属于当前执行范围".into(),
                });
            }
        }
        Ok(active)
    }

    fn execution_scope_root_in_tasks(
        tasks: &HashMap<TaskId, Task>,
        task_id: &TaskId,
    ) -> DomainResult<TaskId> {
        let task = tasks
            .get(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        if let Some((root, transfer)) = Self::agent_run_transfer_in_tasks(tasks, task_id)? {
            let parent = root
                .parent_task_id
                .as_ref()
                .and_then(|id| tasks.get(id))
                .ok_or(DomainError::NotFound {
                    entity: "ParentTask",
                })?;
            Self::validate_agent_transfer_binding(parent, root, transfer)?;
            Ok(root.task_id.clone())
        } else {
            Self::validate_task_root(tasks, task_id, &task.root_task_id)?;
            Ok(task.root_task_id.clone())
        }
    }

    /// 返回任务树是否存在等待用户回答的工作流节点。
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

    /// 获取当前执行范围中处于 Pending 且依赖已满足的任务，已转交子树归新 Runner。
    pub fn get_runnable_leaves(&self, root_task_id: &TaskId) -> Vec<Task> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let children = child_index_from_tasks(&tasks);

        let all_ids = collect_subtree_ids_in_scope(&tasks, root_task_id, true);

        // LocalAgent 的父模型拥有编排循环，恢复时必须先重新派发父任务，
        // 才能让 Pending 子任务获得 Running 祖先。工作流仍等其分派动作的子任务结束。
        let leaves: Vec<&Task> = all_ids
            .iter()
            .filter_map(|id| tasks.get(id))
            .filter(|task| {
                children
                    .get(&task.task_id)
                    .map(|child_ids| {
                        child_ids.iter().all(|cid| {
                            tasks.get(cid).is_some_and(|child| {
                                child.agent_run_transfer().is_some()
                                    || is_terminal_status(child.status)
                                    || task.kind == magi_core::TaskKind::LocalAgent
                                        && child.status == TaskStatus::Pending
                            })
                        })
                    })
                    .unwrap_or(true)
            })
            .collect();

        // runnable：pending 且所有依赖已完成。父子编排约束由 Coordinator
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
        Self::ancestor_chain_is_active(task, tasks, true)
    }

    /// 调度、持有租约及成功提交共用完整父链检查。新派发还要求父任务已经启动；
    /// 已持有租约的执行允许父工作流暂存 Pending，等待其子任务返回。
    fn ancestor_chain_is_active(
        task: &Task,
        tasks: &HashMap<TaskId, Task>,
        require_started: bool,
    ) -> bool {
        let mut current = task;
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(&current.task_id)
                || current.root_task_id != task.root_task_id
                || current.mission_id != task.mission_id
                || is_terminal_status(current.status)
            {
                return false;
            }
            if current.task_id == task.root_task_id {
                return current.parent_task_id.is_none();
            }
            if current.agent_run_transfer().is_some() {
                return Self::execution_scope_root_in_tasks(tasks, &current.task_id)
                    .is_ok_and(|root| root == current.task_id);
            }
            let Some(parent) = current.parent_task_id.as_ref().and_then(|id| tasks.get(id)) else {
                return false;
            };
            if require_started && parent.status != TaskStatus::Running {
                return false;
            }
            current = parent;
        }
    }

    /// 删除单个任务并清理所有关联的索引和租约。
    pub fn remove_task(&self, task_id: &TaskId) -> DomainResult<Option<Task>> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        let mut leases = self
            .leases
            .read()
            .expect("leases read lock poisoned")
            .clone();
        let mut mission_index = self
            .mission_index
            .read()
            .expect("mission_index read lock poisoned")
            .clone();
        if tasks.values().any(|task| {
            task.agent_activation()
                .is_some_and(|binding| binding.previous_task_id.as_ref() == Some(task_id))
                || (task.agent_activation().is_some()
                    && task.parent_task_id.as_ref() == Some(task_id))
        }) {
            return Err(DomainError::InvalidState {
                message: "任务仍被 Agent activation 来源或父子链引用，不能单独删除".into(),
            });
        }
        let removed = tasks.remove(task_id);

        if let Some(ref task) = removed {
            // Remove from mission index
            if let Some(ids) = mission_index.get_mut(&task.mission_id) {
                ids.retain(|id| id != task_id);
                if ids.is_empty() {
                    mission_index.remove(&task.mission_id);
                }
            }
            // 任务已物理删除，关联租约也必须物理删除，不能留下无主租约。
            leases.retain(|_, lease| lease.task_id != *task_id);
            let now = UtcMillis::now();
            for parent in tasks.values_mut() {
                if parent.required_children.iter().any(|id| id == task_id) {
                    parent.required_children.retain(|id| id != task_id);
                    parent.updated_at = now;
                }
            }
        }
        let snapshot = Self::snapshot_from_maps(&tasks, &leases);
        self.fire_checkpoint(&snapshot)?;
        self.commit_maps(tasks, leases, mission_index);
        if removed.is_some() {
            self.notify_status_change();
        }
        Ok(removed)
    }

    /// 删除一个 mission 的全部任务、租约和索引。批量删除只触发一次 checkpoint，
    /// 确保持久化文件不会在任务树半删状态下被观察到。
    pub fn remove_tasks_by_mission(&self, mission_id: &MissionId) -> DomainResult<Vec<Task>> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut mission_index = self
            .mission_index
            .read()
            .expect("mission_index read lock poisoned")
            .clone();
        let task_ids = mission_index.remove(mission_id).unwrap_or_default();
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        let mut leases = self
            .leases
            .read()
            .expect("leases read lock poisoned")
            .clone();
        if task_ids.is_empty() {
            let snapshot = Self::snapshot_from_maps(&tasks, &leases);
            self.fire_checkpoint(&snapshot)?;
            self.commit_maps(tasks, leases, mission_index);
            return Ok(Vec::new());
        }
        let task_id_set = task_ids.iter().cloned().collect::<HashSet<_>>();
        let mut removed = task_ids
            .iter()
            .filter_map(|task_id| tasks.remove(task_id))
            .collect::<Vec<_>>();
        let now = UtcMillis::now();
        for task in tasks.values_mut() {
            let dependency_len = task.dependency_ids.len();
            let required_child_len = task.required_children.len();
            task.dependency_ids
                .retain(|task_id| !task_id_set.contains(task_id));
            task.required_children
                .retain(|task_id| !task_id_set.contains(task_id));
            if dependency_len != task.dependency_ids.len()
                || required_child_len != task.required_children.len()
            {
                task.updated_at = now;
            }
        }
        removed.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.task_id.as_str().cmp(right.task_id.as_str()))
        });
        leases.retain(|_, lease| {
            !task_id_set.contains(&lease.task_id) && !task_id_set.contains(&lease.root_task_id)
        });
        let snapshot = Self::snapshot_from_maps(&tasks, &leases);
        self.fire_checkpoint(&snapshot)?;
        self.commit_maps(tasks, leases, mission_index);
        if !removed.is_empty() {
            self.notify_status_change();
        }
        Ok(removed)
    }

    /// 获取指定任务的所有任务。
    pub fn get_tasks_by_mission(&self, mission_id: &MissionId) -> Vec<Task> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
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
}

impl Default for TaskStore {
    fn default() -> Self {
        Self::new()
    }
}

/// 任务系统 L11 五态迁移表。
pub fn is_valid_transition(from: TaskStatus, to: TaskStatus) -> bool {
    use TaskStatus::*;
    if from == to {
        return true;
    }
    matches!(
        (from, to),
        (Pending, Running) | (Pending, Killed) | (Running, Failed) | (Running, Killed)
    )
}

fn is_terminal_status(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
    )
}

#[cfg(test)]
mod tests;
