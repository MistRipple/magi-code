use magi_core::{
    AccessProfile, AgentContextAccessRecord, AgentContextPackage, AgentContextSupplement,
    AgentRunProjection, DomainError, DomainResult, LeaseId, MissionId, ProgressSummary, Task,
    TaskCompletionAttempt, TaskId, TaskKind, TaskPolicy, TaskRuntimePayload, TaskStatus, TaskTier,
    UtcMillis, WorkerId,
};
use magi_worker_runtime::WorkerRuntimeDurableSnapshot;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
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

#[derive(Debug, Default, Deserialize)]
struct TaskStoreCheckpoint {
    #[serde(default)]
    tasks: Vec<Task>,
    #[serde(default)]
    leases: Vec<TaskLease>,
}

/// 旧版 v2 在引入 generation manifest 前写入的单 root 投影。
///
/// 该类型只允许在 state layout 一次性迁移边界使用；正常恢复必须经过 manifest。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnmarkedTaskRootProjection {
    #[serde(default)]
    tasks: Vec<Task>,
    #[serde(default)]
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
    #[serde(default)]
    generation: u64,
    /// 不包含 generation 字段的 root 业务事实指纹，用于 checkpoint 增量复用。
    #[serde(default)]
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
    status_change_signal: Condvar,
    /// 生产 daemon 设为 true，把可能触发跨模块 IO 的回调移出 mutation guard；
    /// 默认 false 供同步嵌入测试保持确定的 callback 观察语义。
    status_change_callback_async: AtomicBool,
}

fn default_frozen_policy() -> TaskPolicy {
    TaskPolicy {
        autonomy_level: "Assisted".to_string(),
        access_profile: AccessProfile::Restricted,
        collaboration_mode: Default::default(),
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
    let children = child_index_from_tasks(tasks);
    let mut all_ids = Vec::new();
    let mut queue = vec![root_id.clone()];
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
            status_change_signal: Condvar::new(),
            status_change_callback_async: AtomicBool::new(false),
        }
    }

    /// Create a store with a callback that fires after every successful
    /// `update_status` call.
    pub fn with_status_change_callback(callback: StatusChangeCallback) -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            leases: RwLock::new(HashMap::new()),
            mission_index: RwLock::new(HashMap::new()),
            on_status_change: Mutex::new(Some(Arc::from(callback))),
            on_checkpoint: Mutex::new(None),
            mutation_lock: Mutex::new(()),
            status_change_version: Mutex::new(0),
            status_change_signal: Condvar::new(),
            status_change_callback_async: AtomicBool::new(false),
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
        *guard = Some(Arc::from(callback));
    }

    /// 生产路径启用异步状态通知，保证 callback 不会在 TaskStore mutation guard
    /// 生命周期内执行。同步模式只用于不涉及外部 IO 的嵌入测试。
    pub fn set_status_change_callback_async(&self, enabled: bool) {
        self.status_change_callback_async
            .store(enabled, Ordering::Release);
    }

    /// Set or replace the per-transition checkpoint callback (design 6.8).
    pub fn set_checkpoint_callback(&self, callback: CheckpointCallback) {
        let mut guard = self
            .on_checkpoint
            .lock()
            .expect("on_checkpoint lock poisoned");
        *guard = Some(callback);
    }

    /// 判断状态变更是否已经接入 durable checkpoint 回调。
    ///
    /// Runner 的调度循环会收到“状态已变化”的信号，但生产 TaskStore 在每次
    /// 变更提交前已经通过该回调完成增量 checkpoint。暴露这个只读事实让 Runner
    /// 避免在终态再次写一份全量快照；未配置回调的轻量测试/嵌入场景仍保留原有
    /// `with_checkpoint_persist` 兜底。
    pub fn has_checkpoint_callback(&self) -> bool {
        self.on_checkpoint
            .lock()
            .expect("on_checkpoint lock poisoned")
            .is_some()
    }

    fn fire_checkpoint(&self, snapshot: &TaskStoreSnapshot) -> DomainResult<()> {
        let guard = self
            .on_checkpoint
            .lock()
            .expect("on_checkpoint lock poisoned");
        if let Some(ref cb) = *guard {
            cb(snapshot)?;
        }
        Ok(())
    }

    fn snapshot_from_maps(
        tasks: &HashMap<TaskId, Task>,
        leases: &HashMap<LeaseId, TaskLease>,
    ) -> TaskStoreSnapshot {
        let mut tasks = tasks.values().cloned().collect::<Vec<_>>();
        let mut leases = leases.values().cloned().collect::<Vec<_>>();
        tasks.sort_by(|left, right| left.task_id.as_str().cmp(right.task_id.as_str()));
        leases.sort_by(|left, right| left.lease_id.as_str().cmp(right.lease_id.as_str()));
        TaskStoreSnapshot {
            tasks,
            leases,
            changed_root_ids: Vec::new(),
        }
    }

    fn snapshot_from_maps_with_dirty_roots(
        tasks: &HashMap<TaskId, Task>,
        leases: &HashMap<LeaseId, TaskLease>,
        dirty_roots: impl IntoIterator<Item = TaskId>,
    ) -> TaskStoreSnapshot {
        let mut snapshot = Self::snapshot_from_maps(tasks, leases);
        snapshot.changed_root_ids = dirty_roots.into_iter().collect();
        snapshot
            .changed_root_ids
            .sort_by(|left, right| left.as_str().cmp(right.as_str()));
        snapshot.changed_root_ids.dedup();
        snapshot
    }

    fn snapshot_locked(&self) -> TaskStoreSnapshot {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let leases = self.leases.read().expect("leases read lock poisoned");
        // 空 changed_root_ids 明确表示完整快照，供周期性 checkpoint 和恢复写入使用。
        Self::snapshot_from_maps(&tasks, &leases)
    }

    fn commit_maps(
        &self,
        tasks: HashMap<TaskId, Task>,
        leases: HashMap<LeaseId, TaskLease>,
        mission_index: HashMap<MissionId, Vec<TaskId>>,
    ) {
        *self.tasks.write().expect("tasks write lock poisoned") = tasks;
        *self.leases.write().expect("leases write lock poisoned") = leases;
        *self
            .mission_index
            .write()
            .expect("mission_index write lock poisoned") = mission_index;
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

    fn validate_lease_owner(tasks: &HashMap<TaskId, Task>, lease: &TaskLease) -> DomainResult<()> {
        Self::validate_task_root(tasks, &lease.task_id, &lease.root_task_id)
    }

    fn validate_lease_contract(
        tasks: &HashMap<TaskId, Task>,
        lease: &TaskLease,
    ) -> DomainResult<()> {
        Self::validate_lease_owner(tasks, lease)?;
        if lease.lease_status == TaskLeaseState::Active {
            let task = tasks
                .get(&lease.task_id)
                .ok_or(DomainError::NotFound { entity: "Task" })?;
            if task.status != TaskStatus::Running {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "活跃租约 {} 的任务 {} 必须处于 Running，当前为 {:?}",
                        lease.lease_id, lease.task_id, task.status
                    ),
                });
            }
        }
        Ok(())
    }

    fn build_lease(
        task_id: &TaskId,
        root_task_id: &TaskId,
        worker_id: &WorkerId,
        role: &str,
        duration_ms: u64,
    ) -> TaskLease {
        let now = UtcMillis::now();
        let counter = LEASE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let lease_id = LeaseId::new(format!("lease-{}-{}", now.0, counter));
        TaskLease {
            lease_id,
            task_id: task_id.clone(),
            root_task_id: root_task_id.clone(),
            worker_id: worker_id.clone(),
            role: role.to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0.saturating_add(duration_ms)),
            heartbeat_at: now,
            lease_status: TaskLeaseState::Active,
        }
    }

    fn emit_status_change(
        &self,
        task_id: &TaskId,
        old_status: TaskStatus,
        new_status: TaskStatus,
        task: Task,
    ) {
        // 状态事实已经通过 checkpoint 和 map 提交完成后才进入这里。回调可能触发
        // SessionStore、事件总线或 Runner 收口，不能在 mutation_lock 生命周期内执行；
        // 复制轻量句柄并异步投递，确保任务状态提交不会被跨模块 IO 反向阻塞。
        let callback = self
            .on_status_change
            .lock()
            .expect("on_status_change lock poisoned")
            .clone();
        if let Some(callback) = callback {
            if self.status_change_callback_async.load(Ordering::Acquire) {
                let callback_task_id = task_id.clone();
                let log_task_id = callback_task_id.clone();
                if let Err(error) = std::thread::Builder::new()
                    .name("magi-task-status-notify".to_string())
                    .spawn(move || callback(&callback_task_id, old_status, new_status, task))
                {
                    tracing::warn!(?error, %log_task_id, "异步任务状态通知线程启动失败");
                }
            } else {
                callback(task_id, old_status, new_status, task);
            }
        }
        self.notify_status_change();
    }

    fn notify_status_change(&self) {
        let mut version = self
            .status_change_version
            .lock()
            .expect("status_change_version lock poisoned");
        *version = version.saturating_add(1);
        self.status_change_signal.notify_all();
    }

    pub fn status_change_version(&self) -> u64 {
        *self
            .status_change_version
            .lock()
            .expect("status_change_version lock poisoned")
    }

    /// 唤醒等待任务运行态变化的协调器。用于非状态字段但会改变编排决策的事件，
    /// 例如子代理发起上下文请求。
    pub fn notify_runtime_change(&self) {
        self.notify_status_change();
    }

    pub fn wait_for_status_change_since(&self, observed_version: u64, timeout: Duration) -> u64 {
        let version = self
            .status_change_version
            .lock()
            .expect("status_change_version lock poisoned");
        if *version != observed_version || timeout.is_zero() {
            return *version;
        }
        let (version, _) = self
            .status_change_signal
            .wait_timeout_while(version, timeout, |current| *current == observed_version)
            .expect("status_change_signal wait poisoned");
        *version
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
        tasks.insert(task_id.clone(), task);
        mission_index
            .entry(mission_id)
            .or_default()
            .push(task_id.clone());
        if checkpoint {
            let leases = self.leases.read().expect("leases read lock poisoned");
            let snapshot = Self::snapshot_from_maps_with_dirty_roots(
                &tasks,
                &leases,
                vec![root_task_id.clone()],
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

    /// 迁移任务到新的父节点，同时修正 children 索引。
    pub fn reparent_task(&self, task_id: &TaskId, new_parent_id: &TaskId) -> DomainResult<()> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
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
        if let Some(old_parent_id) = old_parent.clone()
            && let Some(parent) = tasks.get_mut(&old_parent_id)
        {
            parent.required_children.retain(|id| id != task_id);
            parent.updated_at = UtcMillis::now();
        }

        if was_required
            && let Some(parent) = tasks.get_mut(new_parent_id)
            && !parent.required_children.iter().any(|id| id == task_id)
        {
            parent.required_children.push(task_id.clone());
            parent.updated_at = UtcMillis::now();
        }
        *self.tasks.write().expect("tasks write lock poisoned") = tasks;
        Ok(())
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

    pub fn append_input_ref(&self, task_id: &TaskId, input_ref: String) -> DomainResult<()> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        task.input_refs.push(input_ref);
        task.updated_at = UtcMillis::now();
        Ok(())
    }

    /// 为子代理写入唯一的结构化上下文包。该操作覆盖旧包但保留已有访问审计，适用于
    /// 创建阶段的原子注册与显式重建；普通运行期补充必须使用
    /// `append_agent_context_supplement` 递增 revision。
    pub fn set_agent_context_package(
        &self,
        task_id: &TaskId,
        package: AgentContextPackage,
    ) -> DomainResult<()> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        let dirty_root_id = task.root_task_id.clone();
        let accesses = match &mut task.runtime_payload {
            TaskRuntimePayload::AgentContext { accesses, .. } => std::mem::take(accesses),
            TaskRuntimePayload::None | TaskRuntimePayload::BrowserAnnotations { .. } => Vec::new(),
        };
        task.runtime_payload = TaskRuntimePayload::AgentContext {
            package: Box::new(package),
            accesses,
        };
        task.updated_at = UtcMillis::now();
        let leases = self.leases.read().expect("leases read lock poisoned");
        let snapshot =
            Self::snapshot_from_maps_with_dirty_roots(&tasks, &leases, vec![dirty_root_id]);
        self.fire_checkpoint(&snapshot)?;
        *self.tasks.write().expect("tasks write lock poisoned") = tasks;
        self.notify_status_change();
        Ok(())
    }

    pub fn append_agent_context_access(
        &self,
        task_id: &TaskId,
        record: AgentContextAccessRecord,
    ) -> DomainResult<()> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        let dirty_root_id = task.root_task_id.clone();
        let TaskRuntimePayload::AgentContext { accesses, .. } = &mut task.runtime_payload else {
            return Err(DomainError::InvalidState {
                message: format!("任务 {task_id} 没有 agent context package"),
            });
        };
        accesses.push(record);
        task.updated_at = UtcMillis::now();
        let leases = self.leases.read().expect("leases read lock poisoned");
        let snapshot =
            Self::snapshot_from_maps_with_dirty_roots(&tasks, &leases, vec![dirty_root_id]);
        self.fire_checkpoint(&snapshot)?;
        *self.tasks.write().expect("tasks write lock poisoned") = tasks;
        Ok(())
    }

    /// 把主线补充写回目标代理的权威上下文包并递增 revision。
    pub fn append_agent_context_supplement(
        &self,
        task_id: &TaskId,
        supplement: AgentContextSupplement,
    ) -> DomainResult<AgentContextPackage> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        let dirty_root_id = task.root_task_id.clone();
        let TaskRuntimePayload::AgentContext { package, .. } = &mut task.runtime_payload else {
            return Err(DomainError::InvalidState {
                message: format!("任务 {task_id} 没有 agent context package"),
            });
        };
        package.revision = package.revision.saturating_add(1);
        package.updated_at = supplement.created_at;
        package.supplements.push(supplement);
        task.updated_at = package.updated_at;
        let package = package.as_ref().clone();
        let leases = self.leases.read().expect("leases read lock poisoned");
        let snapshot =
            Self::snapshot_from_maps_with_dirty_roots(&tasks, &leases, vec![dirty_root_id]);
        self.fire_checkpoint(&snapshot)?;
        *self.tasks.write().expect("tasks write lock poisoned") = tasks;
        Ok(package)
    }

    pub fn append_required_child(
        &self,
        task_id: &TaskId,
        child_task_id: &TaskId,
    ) -> DomainResult<()> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
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

    /// 递增任务的 retry_count。
    pub fn increment_retry_count(&self, task_id: &TaskId) {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.write().expect("tasks write lock poisoned");
        if let Some(task) = tasks.get_mut(task_id) {
            task.retry_count += 1;
            task.updated_at = UtcMillis::now();
        }
    }

    /// BFS 收集以 root_id 为根的整棵子树中所有任务 ID。
    pub fn collect_subtree_ids(&self, root_id: &TaskId) -> Vec<TaskId> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        collect_subtree_ids_from_tasks(&tasks, root_id)
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

        // runnable：pending 且所有依赖已完成。父子编排约束由 SpawnGraph/Coordinator
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
        // 新执行模型下，父任务调用 agent_spawn 后进入 Running，子任务由后台 runner
        // 独立推进，因此祖先链允许出现 Running 节点；只有 Pending 祖先
        // （还没开始执行）才说明该子任务尚不该被调度。父任务的依赖在父任务自身
        // 进入 Running 前已被 dispatcher 校验过，这里不再重复校验。
        let mut current = task.parent_task_id.as_ref();
        while let Some(pid) = current {
            if let Some(parent) = tasks.get(pid) {
                if parent.status == TaskStatus::Pending {
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

    /// 清除所有任务、租约及索引。
    pub fn clear_all(&self) {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
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

    /// 构建代理运行视图。
    pub fn build_agent_run_projection(&self, root_task_id: &TaskId) -> Option<AgentRunProjection> {
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
            .filter(|t| {
                matches!(
                    t.status,
                    TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
                )
            })
            .count() as u32;
        let failed_count = failed_task_ids.len() as u32;
        let killed_count = killed_task_ids.len() as u32;
        let running_count = running_tasks.len() as u32;

        let aggregate_status = root_task.status;

        let execution_mode = "execution_chain".to_string();
        let runner_status = match aggregate_status {
            TaskStatus::Running => "running".to_string(),
            TaskStatus::Completed => "completed".to_string(),
            TaskStatus::Failed => "error".to_string(),
            TaskStatus::Killed => "killed".to_string(),
            TaskStatus::Pending => "pending".to_string(),
        };
        let display_status = match root_task.status {
            TaskStatus::Killed => "已终止".to_string(),
            TaskStatus::Failed => "主线任务执行失败，等待用户处理".to_string(),
            TaskStatus::Completed if failed_count > 0 => {
                format!("主线已完成，{} 项子任务已降级处理", failed_count)
            }
            TaskStatus::Completed => "全部完成".to_string(),
            TaskStatus::Pending => "待启动".to_string(),
            TaskStatus::Running if failed_count > 0 => {
                format!("主线仍在执行，正在接管 {} 项失败子任务", failed_count)
            }
            TaskStatus::Running => {
                let pct = (settled_tasks as f32 / total_tasks.max(1) as f32 * 100.0).round() as u32;
                if running_count > 0 {
                    format!("{}% 已完成，{} 项执行中", pct, running_count)
                } else if pending_count > 0 {
                    format!("{}% 已完成，{} 项待执行", pct, pending_count)
                } else {
                    format!("{}% 已完成，主线汇总中", pct)
                }
            }
        };

        Some(AgentRunProjection {
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

    /// 插入已有租约事实。
    ///
    /// 这是恢复/测试建 fixture 的底层入口，不负责推进任务状态。运行时必须使用
    /// `grant_lease_and_start_task`，避免把租约和任务状态拆成两个事实提交。
    pub fn insert_lease(&self, lease: TaskLease) -> DomainResult<()> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        Self::validate_lease_contract(&tasks, &lease)?;
        let leases = self.leases.read().expect("leases read lock poisoned");
        if leases.contains_key(&lease.lease_id) {
            return Err(DomainError::AlreadyExists { entity: "Lease" });
        }
        if lease.lease_status == TaskLeaseState::Active
            && leases.values().any(|existing| {
                existing.task_id == lease.task_id && existing.lease_status == TaskLeaseState::Active
            })
        {
            return Err(DomainError::InvalidState {
                message: format!("任务 {} 已有活跃租约，禁止重复持有", lease.task_id),
            });
        }
        drop(leases);
        let lease_id = lease.lease_id.clone();
        self.leases
            .write()
            .expect("leases write lock poisoned")
            .insert(lease_id, lease);
        Ok(())
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

    /// 获取租约历史，用于恢复审计和终态断言。
    pub fn get_lease(&self, lease_id: &LeaseId) -> Option<TaskLease> {
        self.leases
            .read()
            .expect("leases read lock poisoned")
            .get(lease_id)
            .cloned()
    }

    /// 校验 worker report 继续使用的执行租约仍是同一轮、同一任务的活跃租约。
    ///
    /// `false` 表示租约已不存在或已经失效，调用方必须丢弃迟到 report；合同字段
    /// 不一致则返回错误，避免把另一轮执行的事实误认成当前执行结果。
    #[cfg(test)]
    pub(crate) fn validate_active_lease_contract(
        &self,
        task_id: &TaskId,
        root_task_id: &TaskId,
        lease_id: &LeaseId,
        worker_id: &WorkerId,
    ) -> DomainResult<bool> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let Some(lease) = self
            .leases
            .read()
            .expect("leases read lock poisoned")
            .get(lease_id)
            .cloned()
        else {
            return Ok(false);
        };
        if lease.lease_status != TaskLeaseState::Active {
            return Ok(false);
        }
        if lease.task_id != *task_id {
            return Err(DomainError::InvalidState {
                message: format!("租约 {} 不属于任务 {}", lease_id, task_id),
            });
        }
        if lease.root_task_id != *root_task_id {
            return Err(DomainError::InvalidState {
                message: format!("租约 {} 不属于 root task {}", lease_id, root_task_id),
            });
        }
        if lease.worker_id != *worker_id {
            return Err(DomainError::InvalidState {
                message: format!(
                    "租约 {} 属于 worker {}，不能由 worker {} 提交结果",
                    lease_id, lease.worker_id, worker_id
                ),
            });
        }
        Self::validate_lease_contract(&tasks, &lease)?;
        Ok(true)
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
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        if Self::validate_task_root(&tasks, task_id, root_task_id).is_err() {
            return None;
        }
        if tasks
            .get(task_id)
            .is_none_or(|task| task.status != TaskStatus::Running)
        {
            return None;
        }
        let mut leases = self.leases.write().expect("leases write lock poisoned");

        if let Some(active_lease) = leases
            .values()
            .find(|lease| lease.task_id == *task_id && lease.lease_status == TaskLeaseState::Active)
        {
            if active_lease.root_task_id != *root_task_id {
                return None;
            }
            if Self::validate_lease_contract(&tasks, active_lease).is_err() {
                return None;
            }
            return None;
        }

        let lease = Self::build_lease(task_id, root_task_id, worker_id, role, duration_ms);
        let lease_id = lease.lease_id.clone();

        leases.insert(lease_id, lease.clone());
        Some(lease)
    }

    /// 在一个 TaskStore 事务中授予租约并把任务推进到 `Running`。
    ///
    /// 返回 `Ok(None)` 表示任务已经被其他执行轮持有活跃租约；任何归属错误或
    /// checkpoint 错误都会返回错误，并且任务与租约都保持原状。
    pub fn grant_lease_and_start_task(
        &self,
        task_id: &TaskId,
        root_task_id: &TaskId,
        worker_id: &WorkerId,
        role: &str,
        duration_ms: u64,
    ) -> DomainResult<Option<TaskLease>> {
        let transaction_started_at = Instant::now();
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        Self::validate_task_root(&tasks, task_id, root_task_id)?;
        let task = tasks
            .get(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        if task.status != TaskStatus::Pending {
            return Err(DomainError::InvalidState {
                message: format!(
                    "任务 {} 只能从 Pending 获得执行租约，当前状态为 {:?}",
                    task_id, task.status
                ),
            });
        }

        let mut leases = self
            .leases
            .read()
            .expect("leases read lock poisoned")
            .clone();
        if let Some(active_lease) = leases
            .values()
            .find(|lease| lease.task_id == *task_id && lease.lease_status == TaskLeaseState::Active)
            .cloned()
        {
            Self::validate_lease_owner(&tasks, &active_lease)?;
            if active_lease.root_task_id != *root_task_id {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "任务 {} 的活跃租约 {} 不属于 root task {}",
                        task_id, active_lease.lease_id, root_task_id
                    ),
                });
            }
            return Ok(None);
        }

        let lease = Self::build_lease(task_id, root_task_id, worker_id, role, duration_ms);
        leases.insert(lease.lease_id.clone(), lease.clone());
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        let old_status = task.status;
        task.status = TaskStatus::Running;
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
        tracing::info!(
            target: "magi.performance",
            task_id = %task_id,
            root_task_id = %root_task_id,
            elapsed_ms = transaction_started_at.elapsed().as_millis() as u64,
            stage = "task_lease_checkpoint_returned",
            "conversation response timing"
        );
        self.commit_maps(tasks, leases, mission_index);
        drop(_mutation_guard);
        self.emit_status_change(task_id, old_status, TaskStatus::Running, cloned_task);
        tracing::info!(
            target: "magi.performance",
            task_id = %task_id,
            root_task_id = %root_task_id,
            elapsed_ms = transaction_started_at.elapsed().as_millis() as u64,
            stage = "task_lease_transaction_completed",
            "conversation response timing"
        );
        Ok(Some(lease))
    }

    /// 标记活跃租约为已完成。找不到匹配的活跃租约时返回 false。
    pub fn complete_lease(&self, task_id: &TaskId, lease_id: &LeaseId) -> bool {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let mut leases = self.leases.write().expect("leases write lock poisoned");
        let Some(lease) = leases.get_mut(lease_id) else {
            return false;
        };
        if lease.task_id != *task_id
            || lease.lease_status != TaskLeaseState::Active
            || Self::validate_lease_contract(&tasks, lease).is_err()
        {
            return false;
        }
        lease.lease_status = TaskLeaseState::Completed;
        true
    }

    /// 撤销活跃租约。
    pub fn revoke_lease(&self, task_id: &TaskId, lease_id: &LeaseId) -> bool {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let mut leases = self.leases.write().expect("leases write lock poisoned");
        let Some(lease) = leases.get_mut(lease_id) else {
            return false;
        };
        if lease.task_id != *task_id
            || lease.lease_status != TaskLeaseState::Active
            || Self::validate_lease_contract(&tasks, lease).is_err()
        {
            return false;
        }
        lease.lease_status = TaskLeaseState::Revoked;
        true
    }

    /// 在一个 TaskStore 事务中完成指定租约和任务。
    ///
    /// 只有不存在或已经失效的租约会返回 `Ok(false)`，用于丢弃迟到结果。合同校验
    /// 与 checkpoint 错误会原样返回，绝不能通过读取任务内存状态猜测错误类型。
    pub fn complete_lease_and_task(
        &self,
        task_id: &TaskId,
        root_task_id: &TaskId,
        lease_id: &LeaseId,
        attempt: TaskCompletionAttempt,
    ) -> DomainResult<bool> {
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
        let Some(lease) = leases.get(lease_id).cloned() else {
            return Ok(false);
        };
        if lease.lease_status != TaskLeaseState::Active {
            return Ok(false);
        }
        if lease.task_id != *task_id {
            return Err(DomainError::InvalidState {
                message: format!("租约 {} 不属于任务 {}", lease_id, task_id),
            });
        }
        if lease.root_task_id != *root_task_id {
            return Err(DomainError::InvalidState {
                message: format!("租约 {} 不属于 root task {}", lease_id, root_task_id),
            });
        }
        Self::validate_task_root(&tasks, task_id, root_task_id)?;
        Self::validate_lease_contract(&tasks, &lease)?;
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        let old_status = task.status;
        Self::apply_completion(task, attempt)?;
        let cloned_task = task.clone();
        leases
            .get_mut(lease_id)
            .ok_or(DomainError::NotFound { entity: "Lease" })?
            .lease_status = TaskLeaseState::Completed;
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
        self.emit_status_change(task_id, old_status, TaskStatus::Completed, cloned_task);
        Ok(true)
    }

    /// 原子撤销指定租约并把任务推进到 `Failed` 或 `Killed`。
    ///
    /// `lease_id` 为 `None` 只允许没有活跃租约的 Pending 任务执行，适用于尚未派发
    /// 的任务被终止；Running 任务必须携带调用方观察到的租约 ID，避免误收口新执行轮。
    pub fn revoke_lease_and_set_task_terminal(
        &self,
        task_id: &TaskId,
        root_task_id: &TaskId,
        lease_id: Option<&LeaseId>,
        status: TaskStatus,
        output_refs: Vec<String>,
    ) -> DomainResult<bool> {
        if !matches!(status, TaskStatus::Failed | TaskStatus::Killed) {
            return Err(DomainError::InvalidState {
                message: format!("任务终态只能是 Failed 或 Killed，收到 {:?}", status),
            });
        }
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
        Self::validate_task_root(&tasks, task_id, root_task_id)?;
        let active_leases = leases
            .values()
            .filter(|lease| {
                lease.task_id == *task_id && lease.lease_status == TaskLeaseState::Active
            })
            .cloned()
            .collect::<Vec<_>>();
        for active_lease in &active_leases {
            Self::validate_lease_contract(&tasks, active_lease)?;
            if active_lease.root_task_id != *root_task_id {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "活跃租约 {} 不属于 root task {}",
                        active_lease.lease_id, root_task_id
                    ),
                });
            }
        }
        let task = tasks
            .get(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        if is_terminal_status(task.status) {
            if !active_leases.is_empty() {
                return Err(DomainError::InvalidState {
                    message: format!("终态任务 {} 不能拥有活跃租约，必须先修复任务投影", task_id),
                });
            }
            return Ok(false);
        }
        let requested_lease = match lease_id {
            Some(expected) => {
                let Some(lease) = leases.get(expected).cloned() else {
                    return Ok(false);
                };
                if lease.task_id != *task_id {
                    return Err(DomainError::InvalidState {
                        message: format!("租约 {} 不属于任务 {}", expected, task_id),
                    });
                }
                Self::validate_lease_contract(&tasks, &lease)?;
                if lease.root_task_id != *root_task_id {
                    return Err(DomainError::InvalidState {
                        message: format!("租约 {} 不属于 root task {}", expected, root_task_id),
                    });
                }
                if lease.lease_status != TaskLeaseState::Active {
                    return Ok(false);
                }
                Some(lease)
            }
            None => None,
        };
        let active_lease_id = active_leases.first().map(|lease| lease.lease_id.clone());
        if let Some(active_lease) = active_lease_id
            .as_ref()
            .and_then(|active_id| leases.get(active_id))
        {
            Self::validate_lease_contract(&tasks, active_lease)?;
            if active_lease.root_task_id != *root_task_id {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "活跃租约 {} 不属于 root task {}",
                        active_lease.lease_id, root_task_id
                    ),
                });
            }
        }
        match (requested_lease.as_ref(), active_lease_id.as_ref()) {
            (Some(expected), Some(actual)) if expected.lease_id != *actual => {
                return Ok(false);
            }
            (Some(_), None) => {
                return Ok(false);
            }
            (None, Some(_)) => {
                return Err(DomainError::InvalidState {
                    message: format!("任务 {} 仍有活跃租约，必须指定租约 ID", task_id),
                });
            }
            // 没有活跃租约的 Running 任务是重启或 runner 崩溃后的陈旧执行态。
            // 用户终止和恢复收敛必须能够把它写成终态；此时不存在需要撤销的
            // 新租约，因此不会误伤另一轮执行。
            (None, None) if task.status == TaskStatus::Running => {}
            _ => {}
        }

        if requested_lease.is_some() && task.status != TaskStatus::Running {
            return Err(DomainError::InvalidState {
                message: format!("带租约的任务 {} 必须处于 Running", task_id),
            });
        }

        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        let old_status = task.status;
        Self::apply_terminal_status(task, status, output_refs)?;
        let cloned_task = task.clone();
        if let Some(expected) = requested_lease.as_ref() {
            leases
                .get_mut(&expected.lease_id)
                .ok_or(DomainError::NotFound { entity: "Lease" })?
                .lease_status = TaskLeaseState::Revoked;
        }
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
        self.emit_status_change(task_id, old_status, status, cloned_task);
        Ok(true)
    }

    /// 更新活跃租约的心跳时间。
    pub fn heartbeat_lease(&self, task_id: &TaskId, lease_id: &LeaseId) -> bool {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let Some(lease) = self
            .leases
            .read()
            .expect("leases read lock poisoned")
            .get(lease_id)
            .cloned()
        else {
            return false;
        };
        if lease.task_id != *task_id
            || lease.lease_status != TaskLeaseState::Active
            || Self::validate_lease_contract(&tasks, &lease).is_err()
        {
            return false;
        }
        drop(tasks);
        let mut leases = self.leases.write().expect("leases write lock poisoned");
        let Some(lease) = leases.get_mut(lease_id) else {
            return false;
        };
        if lease.task_id != *task_id || lease.lease_status != TaskLeaseState::Active {
            return false;
        }
        let now = UtcMillis::now();
        let lease_ttl_ms = lease.expires_at.0.saturating_sub(lease.granted_at.0);
        lease.heartbeat_at = now;
        lease.expires_at = UtcMillis(now.0.saturating_add(lease_ttl_ms));
        true
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

    /// 获取所有活跃租约（不区分 root_task_id）。
    /// 仅在 reconcile / checkpoint restore 等全量收敛场景使用。
    pub fn collect_all_active_leases(&self) -> Vec<(TaskId, LeaseId)> {
        let leases = self.leases.read().expect("leases read lock poisoned");
        leases
            .values()
            .filter(|l| l.lease_status == TaskLeaseState::Active)
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
    ) -> DomainResult<(usize, usize)> {
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
        for lease in leases.values() {
            Self::validate_lease_contract(&tasks, lease)?;
        }
        let active_leases = leases
            .values()
            .filter(|lease| lease.lease_status == TaskLeaseState::Active)
            .map(|lease| (lease.task_id.clone(), lease.lease_id.clone()))
            .collect::<Vec<_>>();
        let recoverable_branch_task_ids = worker_snapshot
            .branches
            .iter()
            .filter_map(|branch| {
                let task = tasks.get(&branch.task_id)?;
                matches!(
                    task.status,
                    TaskStatus::Pending | TaskStatus::Running | TaskStatus::Failed
                )
                .then(|| branch.task_id.clone())
            })
            .collect::<Vec<_>>();
        if active_leases.is_empty() && recoverable_branch_task_ids.is_empty() {
            return Ok((0, 0));
        }

        let mut affected_roots = HashSet::new();
        for (task_id, lease_id) in &active_leases {
            if let Some(task) = tasks.get(task_id) {
                affected_roots.insert(task.root_task_id.clone());
            }
            if let Some(lease) = leases.get_mut(lease_id)
                && lease.task_id == *task_id
                && lease.lease_status == TaskLeaseState::Active
            {
                lease.lease_status = TaskLeaseState::Revoked;
            }
        }
        for task_id in &recoverable_branch_task_ids {
            if let Some(task) = tasks.get(task_id) {
                affected_roots.insert(task.root_task_id.clone());
            }
        }

        let now = UtcMillis::now();
        let mut failed_count = 0usize;
        for root_task_id in &affected_roots {
            for task_id in collect_subtree_ids_from_tasks(&tasks, &root_task_id) {
                if let Some(task) = tasks.get_mut(&task_id)
                    && matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
                {
                    Self::apply_terminal_status(task, TaskStatus::Failed, Vec::new())?;
                    task.updated_at = now;
                    failed_count += 1;
                }
            }
        }

        let mission_index = self
            .mission_index
            .read()
            .expect("mission_index read lock poisoned")
            .clone();
        let snapshot = Self::snapshot_from_maps_with_dirty_roots(&tasks, &leases, affected_roots);
        self.fire_checkpoint(&snapshot)?;
        self.commit_maps(tasks, leases, mission_index);
        self.notify_status_change();
        Ok((active_leases.len(), failed_count))
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
        let snapshot = self.snapshot_locked_with_mutation_guard();
        Self::validate_snapshot(&snapshot)
            .expect("TaskStore checkpoint cannot serialize an invalid lease/task contract");
        serde_json::json!({
            "tasks": snapshot.tasks,
            "leases": snapshot.leases,
        })
    }

    fn snapshot_locked_with_mutation_guard(&self) -> TaskStoreSnapshot {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        self.snapshot_locked()
    }

    pub fn snapshot(&self) -> TaskStoreSnapshot {
        self.snapshot_locked_with_mutation_guard()
    }

    /// 仅供 v1 -> v2 一次性布局迁移读取旧的全量 checkpoint。
    ///
    /// v2 正常恢复不得调用此入口，避免旧字段默认值和迁移逻辑进入正常启动路径。
    pub fn restore_legacy_checkpoint(data: &serde_json::Value) -> io::Result<Self> {
        let mut checkpoint: TaskStoreCheckpoint = serde_json::from_value(data.clone())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        for task in &mut checkpoint.tasks {
            task.migrate_persisted_completion_contract();
            task.migrate_persisted_goal_mode();
        }
        Self::normalize_legacy_checkpoint(&mut checkpoint)?;
        Self::restore_checked(checkpoint)
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
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
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
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
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
        if let Some(ref parent_id) = task.parent_task_id
            && let Some(parent) = self.get_task(parent_id)
            && !is_valid_parent_child_kind(parent.kind, task.kind)
        {
            return Err(DomainError::InvalidState {
                message: format!(
                    "非法父子关系: {:?} 不能包含 {:?} 子节点",
                    parent.kind, task.kind
                ),
            });
        }
        // Check for dependency cycles.
        for dep_id in &task.dependency_ids {
            if *dep_id == task.task_id {
                return Err(DomainError::InvalidState {
                    message: format!("任务 {} 不能依赖自身", task.task_id),
                });
            }
        }
        self.insert_task(task)
    }

    /// 将同一内存快照写入一个不可变 generation，最后以 manifest 原子切换作为唯一提交点。
    pub fn checkpoint_to_projection_directory(&self, dir: &Path) -> io::Result<usize> {
        let snapshot = {
            let _mutation_guard = self
                .mutation_lock
                .lock()
                .map_err(|_| io::Error::other("task mutation lock poisoned"))?;
            self.snapshot_locked()
        };
        Self::checkpoint_snapshot_to_projection_directory(&snapshot, dir)
    }

    /// 将候选任务事实快照写入 projection。供 checkpoint 回调使用，避免回调重新读取
    /// 可能已经变化的 TaskStore。
    pub fn checkpoint_snapshot_to_projection_directory(
        snapshot: &TaskStoreSnapshot,
        dir: &Path,
    ) -> io::Result<usize> {
        let checkpoint_started_at = Instant::now();
        let changed_root_count = snapshot.changed_root_ids.len();
        Self::validate_snapshot(snapshot)?;
        let _writer = TASK_PROJECTION_WRITE_LOCK
            .lock()
            .map_err(|_| io::Error::other("task projection writer lock poisoned"))?;
        fs::create_dir_all(dir)?;
        let generations_dir = dir.join(TASK_PROJECTION_GENERATIONS_DIR);
        fs::create_dir_all(&generations_dir)?;

        let previous_manifest = Self::read_projection_manifest_if_present(dir)?;
        let mut generation = previous_manifest
            .as_ref()
            .map(|manifest| manifest.generation)
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| io::Error::other("task projection generation 已耗尽"))?;
        while generations_dir
            .join(Self::generation_directory_name(generation))
            .exists()
        {
            generation = generation
                .checked_add(1)
                .ok_or_else(|| io::Error::other("task projection generation 已耗尽"))?;
        }

        let mut roots = HashMap::<TaskId, (Vec<Task>, Vec<TaskLease>)>::new();
        for task in &snapshot.tasks {
            roots
                .entry(task.root_task_id.clone())
                .or_default()
                .0
                .push(task.clone());
        }
        for lease in &snapshot.leases {
            roots
                .entry(lease.root_task_id.clone())
                .or_default()
                .1
                .push(lease.clone());
        }
        for (tasks, leases) in roots.values_mut() {
            tasks.sort_by(|left, right| {
                left.created_at
                    .0
                    .cmp(&right.created_at.0)
                    .then_with(|| left.task_id.as_str().cmp(right.task_id.as_str()))
            });
            leases.sort_by(|left, right| left.lease_id.as_str().cmp(right.lease_id.as_str()));
        }
        #[cfg(test)]
        if let Some(hook) = TASK_PROJECTION_AFTER_SNAPSHOT_HOOK
            .lock()
            .expect("task projection test hook lock poisoned")
            .clone()
        {
            hook(dir);
        }

        let mut projections = roots
            .into_iter()
            .map(|(root_task_id, (tasks, leases))| TaskRootProjection {
                schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                generation,
                root_task_id,
                tasks,
                leases,
            })
            .collect::<Vec<_>>();
        projections
            .sort_by(|left, right| left.root_task_id.as_str().cmp(right.root_task_id.as_str()));
        let dirty_roots = (!snapshot.changed_root_ids.is_empty()).then(|| {
            snapshot
                .changed_root_ids
                .iter()
                .cloned()
                .collect::<HashSet<_>>()
        });
        if let Some(dirty_roots) = dirty_roots.as_ref() {
            let dirty_projections = projections
                .iter()
                .filter(|projection| dirty_roots.contains(&projection.root_task_id))
                .cloned()
                .collect::<Vec<_>>();
            Self::validate_projection_set(&dirty_projections)?;
        } else {
            Self::validate_projection_set(&projections)?;
        }

        let sequence = TASK_PROJECTION_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary_generation_dir = generations_dir.join(format!(
            ".generation-{generation:020}-{}-{sequence}.tmp",
            std::process::id()
        ));
        fs::create_dir(&temporary_generation_dir)?;
        // 每次任务状态变更都会产生一个新 generation，但绝大多数 root projection
        // 并没有变化。复用上一 generation 的不变文件，只为变化的 root 重新序列化并
        // 写入新文件；最后仍通过 manifest 原子切换，保留崩溃恢复和完整快照语义。
        let previous_generation_dir = previous_manifest.as_ref().and_then(|manifest| {
            let path = generations_dir.join(Self::generation_directory_name(manifest.generation));
            path.is_dir().then_some(path)
        });
        let write_result = (|| -> io::Result<Vec<TaskProjectionManifestRoot>> {
            let mut manifest_roots = Vec::with_capacity(projections.len());
            for projection in &projections {
                let file_name = Self::projection_file_name(projection.root_task_id.as_str());
                let destination = temporary_generation_dir.join(&file_name);
                let source = previous_generation_dir
                    .as_ref()
                    .map(|directory| directory.join(&file_name));
                let previous_root = previous_manifest.as_ref().and_then(|manifest| {
                    manifest
                        .roots
                        .iter()
                        .find(|root| root.file_name == file_name)
                });
                let is_dirty = dirty_roots
                    .as_ref()
                    .is_none_or(|roots| roots.contains(&projection.root_task_id));
                if !is_dirty
                    && let Some(previous_root) = previous_root
                    && let Some(source) = source.as_deref()
                    && fs::hard_link(source, &destination).is_ok()
                {
                    manifest_roots.push(TaskProjectionManifestRoot {
                        root_task_id: projection.root_task_id.clone(),
                        file_name,
                        generation: previous_root.generation,
                        content_hash: previous_root.content_hash.clone(),
                    });
                    continue;
                }

                let content = serde_json::to_vec_pretty(projection).map_err(io::Error::other)?;
                let content_hash = Self::projection_content_hash(projection)?;
                let source_identity = Self::reuse_projection_file_if_unchanged(
                    source.as_deref(),
                    &destination,
                    projection,
                    previous_root,
                    &content_hash,
                )?;
                if source_identity.is_none() {
                    Self::write_new_file_synced(&destination, &content)?;
                }
                let (root_generation, root_hash) =
                    source_identity.unwrap_or((generation, content_hash));
                manifest_roots.push(TaskProjectionManifestRoot {
                    root_task_id: projection.root_task_id.clone(),
                    file_name,
                    generation: root_generation,
                    content_hash: root_hash,
                });
            }
            Self::sync_directory(&temporary_generation_dir)?;
            Ok(manifest_roots)
        })();
        let manifest_roots = match write_result {
            Ok(roots) => roots,
            Err(error) => {
                Self::remove_stale_generation(&temporary_generation_dir, &generations_dir)?;
                return Err(error);
            }
        };

        let generation_dir = generations_dir.join(Self::generation_directory_name(generation));
        fs::rename(&temporary_generation_dir, &generation_dir)?;
        Self::sync_directory(&generations_dir)?;

        let manifest = TaskProjectionManifest {
            schema_version: TASK_PROJECTION_SCHEMA_VERSION,
            generation,
            roots: manifest_roots,
        };
        let manifest_content = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
        magi_core::fs_atomic::write_atomic(
            &dir.join(TASK_PROJECTION_MANIFEST_FILE),
            manifest_content,
        )?;
        Self::sync_directory(dir)?;

        Self::cleanup_stale_generations(&generations_dir, &generation_dir)?;
        if changed_root_count > 0 {
            tracing::info!(
                target: "magi.performance",
                changed_root_count,
                task_count = snapshot.tasks.len(),
                lease_count = snapshot.leases.len(),
                projection_count = projections.len(),
                elapsed_ms = checkpoint_started_at.elapsed().as_millis() as u64,
                stage = "task_projection_checkpoint_completed",
                "conversation response timing"
            );
        }
        Ok(projections.len())
    }

    /// 复用上一 generation 中内容完全相同的 root projection。
    ///
    /// generation 目录只读且最终会整体重命名，因此可以安全建立硬链接；硬链接不
    /// 可用时退回到普通写入，由调用方继续执行原子目录提交。返回 root 文件的来源
    /// generation 和业务事实指纹，供 manifest 精确记录。
    fn reuse_projection_file_if_unchanged(
        source: Option<&Path>,
        destination: &Path,
        projection: &TaskRootProjection,
        previous_root: Option<&TaskProjectionManifestRoot>,
        content_hash: &str,
    ) -> io::Result<Option<(u64, String)>> {
        let Some(source) = source else {
            return Ok(None);
        };
        let expected_hash = previous_root
            .map(|root| root.content_hash.trim())
            .filter(|hash| !hash.is_empty());
        if expected_hash == Some(content_hash) {
            if fs::hard_link(source, destination).is_ok() {
                return Ok(Some((
                    previous_root
                        .map(|root| root.generation)
                        .unwrap_or(projection.generation),
                    content_hash.to_string(),
                )));
            }
            return Ok(None);
        }

        // 旧 manifest 尚未带 content_hash 时只做一次兼容性比较；后续 checkpoint
        // 会写入指纹，避免每次状态变更都重新解析所有历史 root 文件。
        let existing = match fs::read(source) {
            Ok(existing) => existing,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let previous: TaskRootProjection = Self::deserialize_projection_strict(&existing)?;
        let same_content = previous.schema_version == projection.schema_version
            && previous.root_task_id == projection.root_task_id
            && serde_json::to_value(&previous.tasks).map_err(io::Error::other)?
                == serde_json::to_value(&projection.tasks).map_err(io::Error::other)?
            && serde_json::to_value(&previous.leases).map_err(io::Error::other)?
                == serde_json::to_value(&projection.leases).map_err(io::Error::other)?;
        if !same_content || fs::hard_link(source, destination).is_err() {
            return Ok(None);
        }
        Ok(Some((previous.generation, content_hash.to_string())))
    }

    fn projection_content_hash(projection: &TaskRootProjection) -> io::Result<String> {
        let mut normalized = projection.clone();
        normalized.generation = 0;
        let content = serde_json::to_vec(&normalized).map_err(io::Error::other)?;
        let hash = content.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
        Ok(format!("{hash:016x}"))
    }

    /// Restore a TaskStore from a root-task projection directory.
    ///
    /// Returns `Ok(None)` when the directory has not been created.
    pub fn restore_from_projection_directory(dir: &Path) -> io::Result<Option<Self>> {
        if !dir.exists() {
            return Ok(None);
        }
        let Some(manifest) = Self::read_projection_manifest_if_present(dir)? else {
            Self::discard_uncommitted_projection_directory(dir)?;
            return Ok(None);
        };
        let generation_dir = dir
            .join(TASK_PROJECTION_GENERATIONS_DIR)
            .join(Self::generation_directory_name(manifest.generation));
        if !generation_dir.is_dir() {
            return Err(Self::invalid_projection(format!(
                "manifest 指向的 generation 不存在: {}",
                manifest.generation
            )));
        }

        let mut expected_files = HashSet::new();
        let mut expected_roots = HashSet::new();
        let mut projections = Vec::with_capacity(manifest.roots.len());
        for root in &manifest.roots {
            if root.root_task_id.as_str().is_empty()
                || !expected_roots.insert(root.root_task_id.clone())
            {
                return Err(Self::invalid_projection("manifest 包含空或重复 rootTaskId"));
            }
            let expected_file_name = Self::projection_file_name(root.root_task_id.as_str());
            if root.file_name != expected_file_name
                || !expected_files.insert(root.file_name.clone())
            {
                return Err(Self::invalid_projection(format!(
                    "manifest 文件名与 rootTaskId 不一致: {}",
                    root.root_task_id
                )));
            }
            let content = fs::read(generation_dir.join(&root.file_name))?;
            let projection = Self::deserialize_projection_strict(&content)?;
            let content_hash_matches = root.content_hash.trim().is_empty()
                || Self::projection_content_hash(&projection)? == root.content_hash;
            if projection.schema_version != TASK_PROJECTION_SCHEMA_VERSION
                || root.generation == 0
                || projection.generation != root.generation
                || projection.root_task_id != root.root_task_id
                || !content_hash_matches
            {
                return Err(Self::invalid_projection(format!(
                    "task projection 元数据与 manifest 不一致: {}",
                    root.root_task_id
                )));
            }
            projections.push(projection);
        }
        let actual_files = fs::read_dir(&generation_dir)?
            .map(|entry| {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    return Err(Self::invalid_projection("generation 中包含非文件条目"));
                }
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| Self::invalid_projection("generation 文件名不是 UTF-8"))
            })
            .collect::<io::Result<HashSet<_>>>()?;
        if actual_files != expected_files {
            return Err(Self::invalid_projection(
                "generation 文件集合与 manifest 不一致",
            ));
        }

        let checkpoint = Self::validate_projection_set(&projections)?;
        Ok(Some(Self::restore_checked(checkpoint)?))
    }

    /// 读取 generation manifest 之前的旧版 root 投影，仅供一次性 state layout 迁移使用。
    ///
    /// 旧目录中的每个 JSON 文件都是一个 root 的完整 `{tasks, leases}` 快照。相同
    /// task/lease 在多个文件中重复出现时必须字节级一致；任何冲突都拒绝迁移，避免
    /// 在两个不确定快照之间静默选错。
    pub fn restore_unmarked_projection_directory_for_migration(
        dir: &Path,
    ) -> io::Result<Option<Self>> {
        if !dir.exists() {
            return Ok(None);
        }
        if dir.join(TASK_PROJECTION_MANIFEST_FILE).exists() {
            return Self::restore_from_projection_directory(dir);
        }

        let mut tasks = HashMap::<TaskId, Task>::new();
        let mut leases = HashMap::<LeaseId, TaskLease>::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_file()
                || path.extension().and_then(|extension| extension.to_str()) != Some("json")
            {
                continue;
            }
            let content = fs::read(&path)?;
            let projection: UnmarkedTaskRootProjection =
                serde_json::from_slice(&content).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "解析未标记 task projection 失败 {}: {error}",
                            path.display()
                        ),
                    )
                })?;
            for task in projection.tasks {
                let task_id = task.task_id.clone();
                if let Some(existing) = tasks.insert(task_id.clone(), task.clone())
                    && serde_json::to_value(&existing)? != serde_json::to_value(&task)?
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("未标记 task projection 包含冲突 task: {task_id}"),
                    ));
                }
            }
            for lease in projection.leases {
                let lease_id = lease.lease_id.clone();
                if let Some(existing) = leases.insert(lease_id.clone(), lease.clone())
                    && serde_json::to_value(&existing)? != serde_json::to_value(&lease)?
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("未标记 task projection 包含冲突 lease: {lease_id}"),
                    ));
                }
            }
        }

        let checkpoint = TaskStoreCheckpoint {
            tasks: tasks.into_values().collect(),
            leases: leases.into_values().collect(),
        };
        let mut checkpoint = checkpoint;
        Self::normalize_legacy_checkpoint(&mut checkpoint)?;
        Ok(Some(Self::restore_checked(checkpoint)?))
    }

    /// 返回 manifest 已提交的 generation。accepted WAL 用它判断某条接纳记录是否已经
    /// 被后续完整 task checkpoint 覆盖。
    pub fn committed_projection_generation(dir: &Path) -> io::Result<Option<u64>> {
        if !dir.exists() {
            return Ok(None);
        }
        if !dir.join(TASK_PROJECTION_MANIFEST_FILE).exists() {
            Self::discard_uncommitted_projection_directory(dir)?;
            return Ok(None);
        }
        Self::read_projection_manifest(dir).map(|manifest| Some(manifest.generation))
    }

    /// 判断已提交 manifest 是否包含指定根任务。该判断只依据 manifest 提交事实，
    /// 不读取当前内存 TaskStore，也不以 task 是否存在推断 accepted WAL 状态。
    pub fn committed_projection_contains_root(
        dir: &Path,
        root_task_id: &TaskId,
    ) -> io::Result<bool> {
        if !dir.exists() {
            return Ok(false);
        }
        let Some(manifest) = Self::read_projection_manifest_if_present(dir)? else {
            Self::discard_uncommitted_projection_directory(dir)?;
            return Ok(false);
        };
        Ok(manifest
            .roots
            .iter()
            .any(|root| root.root_task_id == *root_task_id))
    }

    fn restore_checked(checkpoint: TaskStoreCheckpoint) -> io::Result<Self> {
        Self::validate_checkpoint(&checkpoint)?;
        let store = Self::new();
        {
            let mut tasks = store.tasks.write().expect("tasks write lock poisoned");
            let mut mission_index = store
                .mission_index
                .write()
                .expect("mission_index write lock poisoned");
            for task in checkpoint.tasks {
                mission_index
                    .entry(task.mission_id.clone())
                    .or_default()
                    .push(task.task_id.clone());
                tasks.insert(task.task_id.clone(), task);
            }
        }
        let mut lease_map = store.leases.write().expect("leases write lock poisoned");
        for lease in checkpoint.leases {
            lease_map.insert(lease.lease_id.clone(), lease);
        }
        let max_counter = lease_map
            .keys()
            .filter_map(|id| id.as_str().rsplit('-').next())
            .filter_map(|part| part.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        drop(lease_map);
        let current = LEASE_COUNTER.load(Ordering::Relaxed);
        if max_counter >= current {
            LEASE_COUNTER.store(max_counter + 1, Ordering::Relaxed);
        }
        Ok(store)
    }

    /// 将旧布局中的租约收敛到当前任务图合同后再恢复。
    ///
    /// 旧版删除任务时可能只删除了任务事实，保留了已经结束的历史租约。这样的
    /// 租约不再具备恢复价值，也无法重建已经不存在的任务，因此在一次性迁移边界
    /// 丢弃。活跃租约仍然必须能完整验证，避免把可能尚未完成的执行静默变成历史。
    fn normalize_legacy_checkpoint(checkpoint: &mut TaskStoreCheckpoint) -> io::Result<()> {
        let tasks_by_id = checkpoint
            .tasks
            .iter()
            .map(|task| (task.task_id.clone(), task))
            .collect::<HashMap<_, _>>();
        let mut invalid_active_lease = None;
        checkpoint.leases.retain(|lease| {
            let owner_is_verifiable = tasks_by_id
                .get(&lease.task_id)
                .zip(tasks_by_id.get(&lease.root_task_id))
                .is_some_and(|(task, root)| {
                    root.task_id == root.root_task_id
                        && task.root_task_id == lease.root_task_id
                        && task.mission_id == root.mission_id
                        && (lease.lease_status != TaskLeaseState::Active
                            || task.status == TaskStatus::Running)
                });
            if owner_is_verifiable {
                return true;
            }
            if lease.lease_status == TaskLeaseState::Active {
                invalid_active_lease = Some(lease.lease_id.clone());
                return true;
            }
            false
        });
        if let Some(lease_id) = invalid_active_lease {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("旧 checkpoint 包含无法验证的活跃租约，拒绝迁移: {lease_id}"),
            ));
        }
        Ok(())
    }

    fn deserialize_projection_strict(content: &[u8]) -> io::Result<TaskRootProjection> {
        let value: serde_json::Value = serde_json::from_slice(content).map_err(|error| {
            Self::invalid_projection(format!("解析 task projection 失败: {error}"))
        })?;
        let tasks = value
            .as_object()
            .and_then(|projection| projection.get("tasks"))
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| Self::invalid_projection("task projection 缺少 tasks 数组"))?;
        for task in tasks {
            let task = task
                .as_object()
                .ok_or_else(|| Self::invalid_projection("task projection 包含非对象 task"))?;
            for field in [
                "completion_contract",
                "recovery_checkpoint",
                "runtime_payload",
            ] {
                if !task.contains_key(field) {
                    return Err(Self::invalid_projection(format!(
                        "task projection 的 task 缺少 v2 必填字段: {field}"
                    )));
                }
            }
        }
        serde_json::from_value(value).map_err(|error| {
            Self::invalid_projection(format!("解析 task projection 失败: {error}"))
        })
    }

    fn validate_projection_set(
        projections: &[TaskRootProjection],
    ) -> io::Result<TaskStoreCheckpoint> {
        let mut checkpoint = TaskStoreCheckpoint::default();
        let mut roots = HashSet::new();
        for projection in projections {
            if projection.schema_version != TASK_PROJECTION_SCHEMA_VERSION
                || projection.generation == 0
            {
                return Err(Self::invalid_projection(
                    "task projection schema 或 generation 不合法",
                ));
            }
            if projection.root_task_id.as_str().is_empty()
                || !roots.insert(projection.root_task_id.clone())
            {
                return Err(Self::invalid_projection(
                    "task projection 的 rootTaskId 为空或重复",
                ));
            }
            if projection.tasks.is_empty() {
                return Err(Self::invalid_projection(format!(
                    "root projection 不得为空: {}",
                    projection.root_task_id
                )));
            }
            if !projection.tasks.iter().any(|task| {
                task.task_id == projection.root_task_id
                    && task.root_task_id == projection.root_task_id
            }) {
                return Err(Self::invalid_projection(format!(
                    "root projection 缺少 root task: {}",
                    projection.root_task_id
                )));
            }
            if projection
                .tasks
                .iter()
                .any(|task| task.root_task_id != projection.root_task_id)
                || projection
                    .leases
                    .iter()
                    .any(|lease| lease.root_task_id != projection.root_task_id)
            {
                return Err(Self::invalid_projection(format!(
                    "task 或 lease 归属错误: {}",
                    projection.root_task_id
                )));
            }
            checkpoint.tasks.extend(projection.tasks.iter().cloned());
            checkpoint.leases.extend(projection.leases.iter().cloned());
        }
        // Projection 文件按 root 分组存储，恢复时不能依赖 manifest 的 root 顺序。
        // 全量 checkpoint 的公共表示始终按稳定 ID 排序，确保迁移校验和后续
        // 重启恢复比较的是同一份 canonical 表示，而不是 HashMap/分组遍历顺序。
        checkpoint
            .tasks
            .sort_by(|left, right| left.task_id.as_str().cmp(right.task_id.as_str()));
        checkpoint
            .leases
            .sort_by(|left, right| left.lease_id.as_str().cmp(right.lease_id.as_str()));
        Self::validate_checkpoint(&checkpoint)?;
        Ok(checkpoint)
    }

    fn validate_checkpoint(checkpoint: &TaskStoreCheckpoint) -> io::Result<()> {
        let mut task_ids = HashSet::new();
        let mut tasks_by_id = HashMap::new();
        for task in &checkpoint.tasks {
            if task.task_id.as_str().is_empty()
                || task.root_task_id.as_str().is_empty()
                || !task_ids.insert(task.task_id.clone())
            {
                return Err(Self::invalid_projection(
                    "taskId/rootTaskId 为空或 taskId 重复",
                ));
            }
            tasks_by_id.insert(task.task_id.clone(), task);
        }
        for task in &checkpoint.tasks {
            let Some(root) = tasks_by_id.get(&task.root_task_id) else {
                return Err(Self::invalid_projection(format!(
                    "task 引用不存在的 rootTaskId: {}",
                    task.root_task_id
                )));
            };
            if root.task_id != root.root_task_id {
                return Err(Self::invalid_projection(format!(
                    "root task 自身归属不合法: {}",
                    root.task_id
                )));
            }
        }

        let mut lease_ids = HashSet::new();
        let mut active_task_ids = HashSet::new();
        for lease in &checkpoint.leases {
            if lease.lease_id.as_str().is_empty() || !lease_ids.insert(lease.lease_id.clone()) {
                return Err(Self::invalid_projection("leaseId 为空或重复"));
            }
            let Some(task) = tasks_by_id.get(&lease.task_id) else {
                return Err(Self::invalid_projection(format!(
                    "lease 引用不存在的 taskId: {}",
                    lease.task_id
                )));
            };
            let Some(root) = tasks_by_id.get(&lease.root_task_id) else {
                return Err(Self::invalid_projection(format!(
                    "lease 引用不存在的 rootTaskId: {}",
                    lease.root_task_id
                )));
            };
            if root.task_id != root.root_task_id
                || task.root_task_id != lease.root_task_id
                || task.mission_id != root.mission_id
            {
                return Err(Self::invalid_projection(format!(
                    "lease 与 task/root 的归属不一致: {}",
                    lease.lease_id
                )));
            }
            if lease.lease_status == TaskLeaseState::Active {
                if !active_task_ids.insert(lease.task_id.clone()) {
                    return Err(Self::invalid_projection(format!(
                        "任务 {} 存在多个活跃租约",
                        lease.task_id
                    )));
                }
                if task.status != TaskStatus::Running {
                    return Err(Self::invalid_projection(format!(
                        "活跃租约 {} 的任务 {} 必须处于 Running，当前为 {:?}",
                        lease.lease_id, lease.task_id, task.status
                    )));
                }
            }
        }
        Ok(())
    }

    fn validate_snapshot(snapshot: &TaskStoreSnapshot) -> io::Result<()> {
        if snapshot.changed_root_ids.is_empty() {
            return Self::validate_checkpoint(&TaskStoreCheckpoint {
                tasks: snapshot.tasks.clone(),
                leases: snapshot.leases.clone(),
            });
        }
        // 状态变更已经在 TaskStore mutation lock 下完成结构校验；增量 checkpoint
        // 只需验证受影响 root 的完整子树和租约，不能因为无关历史任务数量增长而
        // 阻塞当前 Turn。恢复入口仍会对完整 projection 做全量校验。
        let dirty_roots = snapshot.changed_root_ids.iter().collect::<HashSet<_>>();
        Self::validate_checkpoint(&TaskStoreCheckpoint {
            tasks: snapshot
                .tasks
                .iter()
                .filter(|task| dirty_roots.contains(&task.root_task_id))
                .cloned()
                .collect(),
            leases: snapshot
                .leases
                .iter()
                .filter(|lease| dirty_roots.contains(&lease.root_task_id))
                .cloned()
                .collect(),
        })
    }

    fn read_projection_manifest_if_present(
        dir: &Path,
    ) -> io::Result<Option<TaskProjectionManifest>> {
        let path = dir.join(TASK_PROJECTION_MANIFEST_FILE);
        if !path.exists() {
            return Ok(None);
        }
        Self::read_projection_manifest(dir).map(Some)
    }

    fn discard_uncommitted_projection_directory(dir: &Path) -> io::Result<()> {
        let generations_dir = dir.join(TASK_PROJECTION_GENERATIONS_DIR);
        if generations_dir.exists() {
            fs::remove_dir_all(&generations_dir)?;
        }
        let has_other_entries = fs::read_dir(dir)?.next().transpose()?.is_some();
        if !has_other_entries {
            let _ = fs::remove_dir(dir);
        }
        Ok(())
    }

    fn read_projection_manifest(dir: &Path) -> io::Result<TaskProjectionManifest> {
        let content = fs::read(dir.join(TASK_PROJECTION_MANIFEST_FILE))?;
        let manifest: TaskProjectionManifest =
            serde_json::from_slice(&content).map_err(|error| {
                Self::invalid_projection(format!("解析 task projection manifest 失败: {error}"))
            })?;
        if manifest.schema_version != TASK_PROJECTION_SCHEMA_VERSION || manifest.generation == 0 {
            return Err(Self::invalid_projection(
                "task projection manifest 版本或 generation 不合法",
            ));
        }
        let mut manifest = manifest;
        // 兼容 generation manifest 引入前已经写入的 root 条目：旧 manifest 没有
        // root generation，旧文件本身使用顶层 generation。
        for root in &mut manifest.roots {
            if root.generation == 0 {
                root.generation = manifest.generation;
            }
        }
        Ok(manifest)
    }

    fn generation_directory_name(generation: u64) -> String {
        format!("generation-{generation:020}")
    }

    fn write_new_file_synced(path: &Path, content: &[u8]) -> io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(content)?;
        file.sync_all()
    }

    fn cleanup_stale_generations(generations_dir: &Path, current: &Path) -> io::Result<()> {
        let mut removed = false;
        for entry in fs::read_dir(generations_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path == current {
                continue;
            }
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
            removed = true;
        }
        if removed {
            Self::sync_directory(generations_dir)?;
        }
        Ok(())
    }

    fn remove_stale_generation(path: &Path, generations_dir: &Path) -> io::Result<()> {
        if path.exists() {
            fs::remove_dir_all(path)?;
            Self::sync_directory(generations_dir)?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn sync_directory(path: &Path) -> io::Result<()> {
        File::open(path)?.sync_all()
    }

    #[cfg(not(unix))]
    fn sync_directory(_path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn invalid_projection(message: impl Into<String>) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, message.into())
    }

    fn projection_file_name(value: &str) -> String {
        let mut encoded = String::new();
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
                encoded.push(byte as char);
            } else {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
        format!("{encoded}.json")
    }
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

/// TaskStore 不再限制固定父子层级，具体编排约束交给 SpawnGraph/Coordinator。
fn is_valid_parent_child_kind(_parent: TaskKind, _child: TaskKind) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{TaskCompletionEvidence, TaskEvidenceRequirement, TaskExecutorBinding};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn task(task_id: &str, status: TaskStatus) -> Task {
        let now = UtcMillis::now();
        Task {
            task_id: TaskId::new(task_id),
            mission_id: MissionId::new("mission-task-store-tests"),
            root_task_id: TaskId::new("root-task-store-tests"),
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: task_id.to_string(),
            goal: task_id.to_string(),
            status,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        }
    }

    fn successful_attempt() -> TaskCompletionAttempt {
        TaskCompletionAttempt {
            output_refs: vec!["output://task-store".to_string()],
            final_response: Some("任务已完成".to_string()),
            evidence: Vec::new(),
        }
    }

    #[test]
    fn grant_lease_and_start_task_commits_task_and_lease_together() {
        let store = TaskStore::new();
        let root = rooted_task("root-grant-atomic", "root-grant-atomic");
        let mut child = rooted_task("root-grant-atomic", "task-grant-atomic");
        child.status = TaskStatus::Pending;
        store.insert_task(root).expect("root should insert");
        store
            .insert_task(child.clone())
            .expect("child should insert");

        let snapshots = Arc::new(Mutex::new(Vec::<TaskStoreSnapshot>::new()));
        let observed_snapshots = Arc::clone(&snapshots);
        store.set_checkpoint_callback(Box::new(move |snapshot| {
            observed_snapshots
                .lock()
                .expect("snapshot lock should not poison")
                .push(snapshot.clone());
            Ok(())
        }));

        let lease = store
            .grant_lease_and_start_task(
                &child.task_id,
                &child.root_task_id,
                &WorkerId::new("worker-grant-atomic"),
                "executor",
                60_000,
            )
            .expect("grant should succeed")
            .expect("task should receive a lease");

        assert_eq!(
            store
                .get_task(&child.task_id)
                .expect("child should exist")
                .status,
            TaskStatus::Running
        );
        assert_eq!(
            store
                .get_active_lease(&child.task_id)
                .expect("lease should be active")
                .lease_id,
            lease.lease_id
        );
        let snapshots = snapshots.lock().expect("snapshot lock should not poison");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(
            snapshots[0]
                .tasks
                .iter()
                .find(|task| task.task_id == child.task_id)
                .expect("snapshot should contain child")
                .status,
            TaskStatus::Running
        );
        assert_eq!(
            snapshots[0]
                .leases
                .iter()
                .find(|snapshot_lease| snapshot_lease.lease_id == lease.lease_id)
                .expect("snapshot should contain lease")
                .lease_status,
            TaskLeaseState::Active
        );
    }

    #[test]
    fn grant_lease_and_start_task_rejects_wrong_root_and_rolls_back_on_checkpoint_failure() {
        let store = TaskStore::new();
        let root = rooted_task("root-grant-owner", "root-grant-owner");
        let other_root = rooted_task("root-grant-other", "root-grant-other");
        let mut child = rooted_task("root-grant-owner", "task-grant-owner");
        child.status = TaskStatus::Pending;
        store.insert_task(root).expect("root should insert");
        store
            .insert_task(other_root.clone())
            .expect("other root should insert");
        store
            .insert_task(child.clone())
            .expect("child should insert");

        let wrong_root_error = store
            .grant_lease_and_start_task(
                &child.task_id,
                &other_root.task_id,
                &WorkerId::new("worker-grant-owner"),
                "executor",
                60_000,
            )
            .expect_err("wrong root must be rejected");
        assert!(matches!(wrong_root_error, DomainError::InvalidState { .. }));
        assert_eq!(
            store
                .get_task(&child.task_id)
                .expect("child should exist")
                .status,
            TaskStatus::Pending
        );
        assert!(store.get_active_lease(&child.task_id).is_none());

        let now = UtcMillis::now();
        let wrong_owner_lease = TaskLease {
            lease_id: LeaseId::new("lease-wrong-owner"),
            task_id: child.task_id.clone(),
            root_task_id: other_root.task_id.clone(),
            worker_id: WorkerId::new("worker-grant-owner"),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0.saturating_add(60_000)),
            heartbeat_at: now,
            lease_status: TaskLeaseState::Active,
        };
        let wrong_owner_error = store
            .insert_lease(wrong_owner_lease)
            .expect_err("lease belonging to another root must be rejected");
        assert!(matches!(
            wrong_owner_error,
            DomainError::InvalidState { .. }
        ));

        store.set_checkpoint_callback(Box::new(|_| {
            Err(DomainError::Persistence {
                message: "grant checkpoint unavailable".to_string(),
            })
        }));
        let persistence_error = store
            .grant_lease_and_start_task(
                &child.task_id,
                &child.root_task_id,
                &WorkerId::new("worker-grant-owner"),
                "executor",
                60_000,
            )
            .expect_err("checkpoint failure must reject grant");
        assert!(matches!(persistence_error, DomainError::Persistence { .. }));
        assert_eq!(
            store
                .get_task(&child.task_id)
                .expect("child should exist")
                .status,
            TaskStatus::Pending
        );
        assert!(store.get_active_lease(&child.task_id).is_none());
    }

    #[test]
    fn complete_lease_and_task_commits_both_terminal_facts_or_neither() {
        let store = TaskStore::new();
        let root = rooted_task("root-complete-atomic", "root-complete-atomic");
        let mut child = rooted_task("root-complete-atomic", "task-complete-atomic");
        child.status = TaskStatus::Pending;
        store.insert_task(root).expect("root should insert");
        store
            .insert_task(child.clone())
            .expect("child should insert");
        let lease = store
            .grant_lease_and_start_task(
                &child.task_id,
                &child.root_task_id,
                &WorkerId::new("worker-complete-atomic"),
                "executor",
                60_000,
            )
            .expect("grant should succeed")
            .expect("lease should be created");

        let wrong_root = rooted_task("root-complete-wrong", "root-complete-wrong");
        store
            .insert_task(wrong_root.clone())
            .expect("wrong root should insert");
        let ownership_error = store
            .complete_lease_and_task(
                &child.task_id,
                &wrong_root.task_id,
                &lease.lease_id,
                successful_attempt(),
            )
            .expect_err("completion with the wrong root must be rejected");
        assert!(matches!(ownership_error, DomainError::InvalidState { .. }));
        assert_eq!(
            store
                .get_task(&child.task_id)
                .expect("child should remain running")
                .status,
            TaskStatus::Running
        );
        assert_eq!(
            store
                .get_lease(&lease.lease_id)
                .expect("lease should remain active")
                .lease_status,
            TaskLeaseState::Active
        );

        let snapshots = Arc::new(Mutex::new(Vec::<TaskStoreSnapshot>::new()));
        let observed_snapshots = Arc::clone(&snapshots);
        store.set_checkpoint_callback(Box::new(move |snapshot| {
            observed_snapshots
                .lock()
                .expect("snapshot lock should not poison")
                .push(snapshot.clone());
            Ok(())
        }));
        assert!(
            store
                .complete_lease_and_task(
                    &child.task_id,
                    &child.root_task_id,
                    &lease.lease_id,
                    successful_attempt(),
                )
                .expect("completion should succeed")
        );
        assert_eq!(
            store
                .get_task(&child.task_id)
                .expect("child should exist")
                .status,
            TaskStatus::Completed
        );
        assert_eq!(
            store
                .get_lease(&lease.lease_id)
                .expect("completed lease should remain as history")
                .lease_status,
            TaskLeaseState::Completed
        );
        let snapshots = snapshots.lock().expect("snapshot lock should not poison");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(
            snapshots[0]
                .tasks
                .iter()
                .find(|task| task.task_id == child.task_id)
                .expect("snapshot should contain child")
                .status,
            TaskStatus::Completed
        );
        assert_eq!(
            snapshots[0]
                .leases
                .iter()
                .find(|snapshot_lease| snapshot_lease.lease_id == lease.lease_id)
                .expect("snapshot should contain lease")
                .lease_status,
            TaskLeaseState::Completed
        );

        let failed_store = TaskStore::new();
        let root = rooted_task("root-complete-persistence", "root-complete-persistence");
        let mut child = rooted_task("root-complete-persistence", "task-complete-persistence");
        child.status = TaskStatus::Pending;
        failed_store.insert_task(root).expect("root should insert");
        failed_store
            .insert_task(child.clone())
            .expect("child should insert");
        let lease = failed_store
            .grant_lease_and_start_task(
                &child.task_id,
                &child.root_task_id,
                &WorkerId::new("worker-complete-persistence"),
                "executor",
                60_000,
            )
            .expect("grant should succeed")
            .expect("lease should be created");
        failed_store.set_checkpoint_callback(Box::new(|_| {
            Err(DomainError::Persistence {
                message: "complete checkpoint unavailable".to_string(),
            })
        }));
        let error = failed_store
            .complete_lease_and_task(
                &child.task_id,
                &child.root_task_id,
                &lease.lease_id,
                successful_attempt(),
            )
            .expect_err("checkpoint failure must reject completion");
        assert!(matches!(error, DomainError::Persistence { .. }));
        assert_eq!(
            failed_store
                .get_task(&child.task_id)
                .expect("child should exist")
                .status,
            TaskStatus::Running
        );
        assert_eq!(
            failed_store
                .get_active_lease(&child.task_id)
                .expect("lease should remain active")
                .lease_id,
            lease.lease_id
        );
    }

    #[test]
    fn revoke_lease_and_set_task_terminal_is_atomic_for_failed_and_killed() {
        let store = TaskStore::new();
        let root = rooted_task("root-terminal-atomic", "root-terminal-atomic");
        let mut child = rooted_task("root-terminal-atomic", "task-terminal-failed");
        child.status = TaskStatus::Pending;
        store.insert_task(root).expect("root should insert");
        store
            .insert_task(child.clone())
            .expect("child should insert");
        let lease = store
            .grant_lease_and_start_task(
                &child.task_id,
                &child.root_task_id,
                &WorkerId::new("worker-terminal-atomic"),
                "executor",
                60_000,
            )
            .expect("grant should succeed")
            .expect("lease should be created");
        assert!(
            store
                .revoke_lease_and_set_task_terminal(
                    &child.task_id,
                    &child.root_task_id,
                    Some(&lease.lease_id),
                    TaskStatus::Failed,
                    vec!["失败原因".to_string()],
                )
                .expect("failure transition should succeed")
        );
        assert_eq!(
            store
                .get_task(&child.task_id)
                .expect("child should exist")
                .status,
            TaskStatus::Failed
        );
        assert_eq!(
            store
                .get_lease(&lease.lease_id)
                .expect("failed lease should remain as history")
                .lease_status,
            TaskLeaseState::Revoked
        );

        let failed_store = TaskStore::new();
        let root = rooted_task("root-terminal-persistence", "root-terminal-persistence");
        let mut child = rooted_task("root-terminal-persistence", "task-terminal-persistence");
        child.status = TaskStatus::Pending;
        failed_store.insert_task(root).expect("root should insert");
        failed_store
            .insert_task(child.clone())
            .expect("child should insert");
        let lease = failed_store
            .grant_lease_and_start_task(
                &child.task_id,
                &child.root_task_id,
                &WorkerId::new("worker-terminal-persistence"),
                "executor",
                60_000,
            )
            .expect("grant should succeed")
            .expect("lease should be created");
        failed_store.set_checkpoint_callback(Box::new(|_| {
            Err(DomainError::Persistence {
                message: "terminal checkpoint unavailable".to_string(),
            })
        }));
        let error = failed_store
            .revoke_lease_and_set_task_terminal(
                &child.task_id,
                &child.root_task_id,
                Some(&lease.lease_id),
                TaskStatus::Failed,
                vec!["失败原因".to_string()],
            )
            .expect_err("checkpoint failure must reject terminal transition");
        assert!(matches!(error, DomainError::Persistence { .. }));
        assert_eq!(
            failed_store
                .get_task(&child.task_id)
                .expect("child should exist")
                .status,
            TaskStatus::Running
        );
        assert_eq!(
            failed_store
                .get_active_lease(&child.task_id)
                .expect("lease should remain active")
                .lease_id,
            lease.lease_id
        );

        let kill_store = TaskStore::new();
        let root = rooted_task("root-terminal-killed", "root-terminal-killed");
        let mut pending = rooted_task("root-terminal-killed", "task-terminal-killed");
        pending.status = TaskStatus::Pending;
        kill_store.insert_task(root).expect("root should insert");
        kill_store
            .insert_task(pending.clone())
            .expect("pending task should insert");
        assert!(
            kill_store
                .revoke_lease_and_set_task_terminal(
                    &pending.task_id,
                    &pending.root_task_id,
                    None,
                    TaskStatus::Killed,
                    Vec::new(),
                )
                .expect("pending task should be killable without lease")
        );
        assert_eq!(
            kill_store
                .get_task(&pending.task_id)
                .expect("pending task should exist")
                .status,
            TaskStatus::Killed
        );
        assert!(kill_store.get_active_lease(&pending.task_id).is_none());
    }

    #[test]
    fn completed_can_only_be_written_through_completion_gate() {
        let store = TaskStore::new();
        let task_id = TaskId::new("task-status-gate");
        store
            .insert_task(task(task_id.as_str(), TaskStatus::Running))
            .expect("任务应插入");

        let error = store
            .update_status_checked(&task_id, TaskStatus::Completed)
            .expect_err("直接写入 Completed 必须被拒绝");

        assert!(matches!(error, DomainError::InvalidState { .. }));
        assert_eq!(
            store.get_task(&task_id).expect("任务应保留").status,
            TaskStatus::Running
        );
    }

    #[test]
    fn completion_gate_rejects_active_lease_without_mutating_either_fact() {
        let store = TaskStore::new();
        let root = rooted_task("root-completion-lease-gate", "root-completion-lease-gate");
        let mut child = rooted_task("root-completion-lease-gate", "task-completion-lease-gate");
        child.status = TaskStatus::Pending;
        store.insert_task(root).expect("root should insert");
        store
            .insert_task(child.clone())
            .expect("child should insert");
        let lease = store
            .grant_lease_and_start_task(
                &child.task_id,
                &child.root_task_id,
                &WorkerId::new("worker-completion-lease-gate"),
                "executor",
                60_000,
            )
            .expect("lease should be granted")
            .expect("task should receive a lease");

        let error = store
            .complete_task(&child.task_id, successful_attempt())
            .expect_err("active lease must use the atomic lease completion path");
        assert!(matches!(error, DomainError::InvalidState { .. }));
        assert_eq!(
            store
                .get_task(&child.task_id)
                .expect("child should exist")
                .status,
            TaskStatus::Running
        );
        assert_eq!(
            store
                .get_lease(&lease.lease_id)
                .expect("lease should remain")
                .lease_status,
            TaskLeaseState::Active
        );
    }

    #[test]
    fn completed_root_reopen_for_recovery_is_checkpointed_atomically() {
        let store = TaskStore::new();
        let root_id = TaskId::new("root-reopen-recovery");
        let mut root = rooted_task(root_id.as_str(), root_id.as_str());
        root.status = TaskStatus::Completed;
        store.insert_task(root).expect("root should insert");

        let snapshots = Arc::new(Mutex::new(Vec::<TaskStoreSnapshot>::new()));
        let observed_snapshots = Arc::clone(&snapshots);
        store.set_checkpoint_callback(Box::new(move |snapshot| {
            observed_snapshots
                .lock()
                .expect("snapshot lock should not poison")
                .push(snapshot.clone());
            Ok(())
        }));

        store
            .reopen_completed_root_for_recovery(&root_id)
            .expect("completed root should reopen for recovery");
        assert_eq!(
            store.get_task(&root_id).expect("root should exist").status,
            TaskStatus::Failed
        );
        assert_eq!(
            snapshots
                .lock()
                .expect("snapshot lock should not poison")
                .last()
                .and_then(|snapshot| snapshot.tasks.first())
                .map(|task| task.status),
            Some(TaskStatus::Failed)
        );
    }

    #[test]
    fn completion_gate_rejects_missing_final_response_without_mutating_task() {
        let store = TaskStore::new();
        let task_id = TaskId::new("task-final-response-required");
        store
            .insert_task(task(task_id.as_str(), TaskStatus::Running))
            .expect("任务应插入");

        let error = store
            .complete_task(
                &task_id,
                TaskCompletionAttempt {
                    final_response: Some("  ".to_string()),
                    ..successful_attempt()
                },
            )
            .expect_err("空最终回复不能提交完成");

        assert!(matches!(error, DomainError::InvalidState { .. }));
        let persisted = store.get_task(&task_id).expect("任务应保留");
        assert_eq!(persisted.status, TaskStatus::Running);
        assert!(persisted.output_refs.is_empty());
        assert!(persisted.evidence_refs.is_empty());
    }

    #[test]
    fn completion_gate_requires_matching_tool_evidence_and_persists_success() {
        let store = TaskStore::new();
        let task_id = TaskId::new("task-evidence-gate");
        let mut task = task(task_id.as_str(), TaskStatus::Running);
        task.completion_contract = magi_core::TaskCompletionContract::default()
            .with_evidence_requirements(vec![TaskEvidenceRequirement::SuccessfulToolCall {
                tool_name: "diagram_render".to_string(),
                minimum_successes: 1,
                argument_equals: [("/format".to_string(), json!("svg"))]
                    .into_iter()
                    .collect(),
                result_contains: vec!["rendered".to_string()],
            }]);
        store.insert_task(task).expect("任务应插入");

        let missing_error = store
            .complete_task(&task_id, successful_attempt())
            .expect_err("缺少工具成功证据不能完成");
        assert!(matches!(missing_error, DomainError::InvalidState { .. }));

        let attempt = TaskCompletionAttempt {
            evidence: vec![TaskCompletionEvidence::SuccessfulToolCall {
                call_id: "call-diagram".to_string(),
                tool_name: "diagram_render".to_string(),
                arguments: json!({"format": "svg"}),
                result: "rendered successfully".to_string(),
            }],
            ..successful_attempt()
        };
        store
            .complete_task(&task_id, attempt)
            .expect("满足合同后应提交完成");

        let persisted = store.get_task(&task_id).expect("任务应保留");
        assert_eq!(persisted.status, TaskStatus::Completed);
        assert_eq!(persisted.output_refs, ["output://task-store"]);
        assert_eq!(persisted.evidence_refs.len(), 1);
    }

    #[test]
    fn restore_migrates_legacy_completion_requirements_and_rejects_corrupt_tasks() {
        let mut task_value =
            serde_json::to_value(task("task-legacy", TaskStatus::Running)).expect("任务应可序列化");
        task_value["root_task_id"] = json!("task-legacy");
        task_value["executor_binding"] = json!({
            "target_role": "coordinator",
            "required_evidence_tools": ["diagram_render"],
            "resumes_turn_id": "turn-legacy",
            "required_tool_chain": ["get_goal", "update_plan"]
        });

        let restored = TaskStore::restore_legacy_checkpoint(&json!({
            "tasks": [task_value],
            "leases": []
        }))
        .expect("旧 checkpoint 应完成一次性迁移");
        let restored_task = restored
            .get_task(&TaskId::new("task-legacy"))
            .expect("迁移后的任务应存在");
        assert_eq!(
            restored_task.completion_contract.evidence_requirements,
            vec![TaskEvidenceRequirement::successful_tool_call(
                "diagram_render"
            )]
        );
        assert_eq!(
            restored_task.executor_binding,
            Some(
                TaskExecutorBinding::for_role("coordinator")
                    .with_required_tool_chain(vec![
                        "get_goal".to_string(),
                        "update_plan".to_string()
                    ])
                    .with_goal_mode(true)
            )
        );

        let error = match TaskStore::restore_legacy_checkpoint(&json!({
            "tasks": [{"task_id": 123}],
            "leases": []
        })) {
            Ok(_) => panic!("损坏 checkpoint 不能静默恢复为空任务列表"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn restore_legacy_checkpoint_discards_terminal_orphan_leases() {
        let root = rooted_task("root-legacy-lease", "root-legacy-lease");
        let now = UtcMillis::now();
        let valid_lease = TaskLease {
            lease_id: LeaseId::new("lease-valid-history"),
            task_id: root.task_id.clone(),
            root_task_id: root.root_task_id.clone(),
            worker_id: WorkerId::new("worker-history"),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: now,
            heartbeat_at: now,
            lease_status: TaskLeaseState::Completed,
        };
        let orphan_lease = TaskLease {
            lease_id: LeaseId::new("lease-orphan-history"),
            task_id: TaskId::new("task-deleted-by-legacy-store"),
            root_task_id: TaskId::new("root-deleted-by-legacy-store"),
            worker_id: WorkerId::new("worker-history"),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: now,
            heartbeat_at: now,
            lease_status: TaskLeaseState::Revoked,
        };

        let restored = TaskStore::restore_legacy_checkpoint(&json!({
            "tasks": [root],
            "leases": [valid_lease, orphan_lease]
        }))
        .expect("旧布局中的终态孤儿租约应在迁移边界被清理");

        let snapshot = restored.snapshot();
        assert_eq!(snapshot.tasks.len(), 1);
        assert_eq!(snapshot.leases.len(), 1);
        assert_eq!(
            snapshot.leases[0].lease_id,
            LeaseId::new("lease-valid-history")
        );
    }

    #[test]
    fn restore_legacy_checkpoint_rejects_unverifiable_active_leases() {
        let now = UtcMillis::now();
        let active_orphan = TaskLease {
            lease_id: LeaseId::new("lease-active-orphan"),
            task_id: TaskId::new("task-missing-during-migration"),
            root_task_id: TaskId::new("root-missing-during-migration"),
            worker_id: WorkerId::new("worker-history"),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: now,
            heartbeat_at: now,
            lease_status: TaskLeaseState::Active,
        };

        let error = match TaskStore::restore_legacy_checkpoint(&json!({
            "tasks": [],
            "leases": [active_orphan]
        })) {
            Ok(_) => panic!("无法验证的活跃租约不能在迁移时静默丢弃"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    fn rooted_task(root_task_id: &str, task_id: &str) -> Task {
        let mut value = task(task_id, TaskStatus::Running);
        value.root_task_id = TaskId::new(root_task_id);
        value.parent_task_id = (task_id != root_task_id).then(|| TaskId::new(root_task_id));
        value
    }

    fn projection_fixture(dir: &Path, generation: u64, projections: &[TaskRootProjection]) {
        let generation_dir = dir
            .join(TASK_PROJECTION_GENERATIONS_DIR)
            .join(TaskStore::generation_directory_name(generation));
        fs::create_dir_all(&generation_dir).expect("fixture generation should create");
        let roots = projections
            .iter()
            .map(|projection| {
                let file_name = TaskStore::projection_file_name(projection.root_task_id.as_str());
                fs::write(
                    generation_dir.join(&file_name),
                    serde_json::to_vec_pretty(projection)
                        .expect("fixture projection should encode"),
                )
                .expect("fixture projection should write");
                TaskProjectionManifestRoot {
                    root_task_id: projection.root_task_id.clone(),
                    file_name,
                    generation,
                    content_hash: TaskStore::projection_content_hash(projection)
                        .expect("fixture projection hash should compute"),
                }
            })
            .collect();
        fs::create_dir_all(dir).expect("fixture root should create");
        fs::write(
            dir.join(TASK_PROJECTION_MANIFEST_FILE),
            serde_json::to_vec_pretty(&TaskProjectionManifest {
                schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                generation,
                roots,
            })
            .expect("fixture manifest should encode"),
        )
        .expect("fixture manifest should write");
    }

    #[test]
    fn projection_restore_ignores_uncommitted_multi_root_generation() {
        let dir = tempfile::tempdir().expect("projection root");
        let projection_dir = dir.path().join("tasks");
        let store = TaskStore::new();
        store
            .insert_task(rooted_task("root-a", "root-a"))
            .expect("root a should insert");
        store
            .insert_task(rooted_task("root-b", "root-b"))
            .expect("root b should insert");
        store
            .checkpoint_to_projection_directory(&projection_dir)
            .expect("first generation should commit");

        let uncommitted = projection_dir
            .join(TASK_PROJECTION_GENERATIONS_DIR)
            .join(TaskStore::generation_directory_name(2));
        fs::create_dir_all(&uncommitted).expect("partial generation should create");
        fs::write(uncommitted.join("root-a.json"), b"{\"partial\":true}")
            .expect("partial root should write");

        let restored = TaskStore::restore_from_projection_directory(&projection_dir)
            .expect("committed generation should restore")
            .expect("manifest represents an initialized store");
        assert!(restored.get_task(&TaskId::new("root-a")).is_some());
        assert!(restored.get_task(&TaskId::new("root-b")).is_some());
    }

    #[test]
    fn projection_restore_uses_manifest_even_when_stale_generation_remains() {
        let dir = tempfile::tempdir().expect("projection root");
        let projection_dir = dir.path().join("tasks");
        let store = TaskStore::new();
        store
            .insert_task(rooted_task("root-current", "root-current"))
            .expect("root should insert");
        store
            .checkpoint_to_projection_directory(&projection_dir)
            .expect("generation should commit");

        let stale = projection_dir
            .join(TASK_PROJECTION_GENERATIONS_DIR)
            .join(TaskStore::generation_directory_name(99));
        fs::create_dir_all(&stale).expect("stale generation should create");
        fs::write(stale.join("invalid.json"), b"not-json").expect("stale projection should write");

        let restored = TaskStore::restore_from_projection_directory(&projection_dir)
            .expect("manifest generation should restore")
            .expect("store should exist");
        assert!(restored.get_task(&TaskId::new("root-current")).is_some());
    }

    #[test]
    fn projection_restore_preserves_canonical_checkpoint_order() {
        let dir = tempfile::tempdir().expect("projection root");
        let projection_dir = dir.path().join("tasks");
        let store = TaskStore::new();

        // The child IDs intentionally sort across root boundaries. A projection
        // restore must produce the same canonical order as a full checkpoint,
        // regardless of root grouping in the manifest.
        store
            .insert_task(rooted_task("root-z", "root-z"))
            .expect("root z should insert");
        store
            .insert_task(rooted_task("root-z", "a-child"))
            .expect("child a should insert");
        store
            .insert_task(rooted_task("root-a", "root-a"))
            .expect("root a should insert");
        store
            .insert_task(rooted_task("root-a", "z-child"))
            .expect("child z should insert");

        let expected = store.checkpoint();
        store
            .checkpoint_to_projection_directory(&projection_dir)
            .expect("projection checkpoint should commit");

        let restored = TaskStore::restore_from_projection_directory(&projection_dir)
            .expect("projection should restore")
            .expect("projection should contain a store");
        assert_eq!(restored.checkpoint(), expected);
    }

    #[test]
    fn concurrent_checkpoints_cannot_commit_an_older_snapshot_last() {
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().expect("projection root");
        let projection_dir = dir.path().join("tasks");
        let store = Arc::new(TaskStore::new());
        store
            .insert_task(rooted_task("root-race", "root-race"))
            .expect("root should insert");
        store
            .checkpoint_to_projection_directory(&projection_dir)
            .expect("initial generation should commit");

        let barrier = Arc::new(Barrier::new(2));
        let first_hook_call = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let hooked_projection_dir = projection_dir.clone();
        *TASK_PROJECTION_AFTER_SNAPSHOT_HOOK
            .lock()
            .expect("test hook should lock") = Some(Arc::new({
            let barrier = barrier.clone();
            let first_hook_call = first_hook_call.clone();
            move |path| {
                if path != hooked_projection_dir {
                    return;
                }
                if first_hook_call.swap(false, Ordering::SeqCst) {
                    barrier.wait();
                    barrier.wait();
                }
            }
        }));

        let old_store = store.clone();
        let old_path = projection_dir.clone();
        let old_writer =
            std::thread::spawn(move || old_store.checkpoint_to_projection_directory(&old_path));
        barrier.wait();
        store
            .update_task_goal(&TaskId::new("root-race"), "new snapshot".to_string())
            .expect("goal should update");
        let new_store = store.clone();
        let new_path = projection_dir.clone();
        let new_writer =
            std::thread::spawn(move || new_store.checkpoint_to_projection_directory(&new_path));
        barrier.wait();
        old_writer
            .join()
            .expect("old writer should join")
            .expect("old generation should commit first");
        new_writer
            .join()
            .expect("new writer should join")
            .expect("new generation should commit last");
        *TASK_PROJECTION_AFTER_SNAPSHOT_HOOK
            .lock()
            .expect("test hook should lock") = None;

        let restored = TaskStore::restore_from_projection_directory(&projection_dir)
            .expect("latest generation should restore")
            .expect("store should exist");
        assert_eq!(
            restored
                .get_task(&TaskId::new("root-race"))
                .expect("root should restore")
                .goal,
            "new snapshot"
        );
    }

    #[test]
    fn checkpoint_removes_every_stale_generation_after_manifest_commit() {
        let dir = tempfile::tempdir().expect("projection root");
        let projection_dir = dir.path().join("tasks");
        let store = TaskStore::new();
        store
            .insert_task(rooted_task("root-cleanup", "root-cleanup"))
            .expect("root should insert");
        store
            .checkpoint_to_projection_directory(&projection_dir)
            .expect("first generation should commit");
        let stale = projection_dir
            .join(TASK_PROJECTION_GENERATIONS_DIR)
            .join(TaskStore::generation_directory_name(77));
        fs::create_dir_all(&stale).expect("stale generation should create");
        store
            .update_task_goal(&TaskId::new("root-cleanup"), "updated".to_string())
            .expect("goal should update");
        store
            .checkpoint_to_projection_directory(&projection_dir)
            .expect("second generation should commit and clean stale directories");

        let generations = fs::read_dir(projection_dir.join(TASK_PROJECTION_GENERATIONS_DIR))
            .expect("generations should read")
            .collect::<Result<Vec<_>, _>>()
            .expect("generation entries should read");
        assert_eq!(generations.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn checkpoint_reuses_unchanged_root_projection_files() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().expect("projection root");
        let projection_dir = dir.path().join("tasks");
        let store = TaskStore::new();
        store
            .insert_task(rooted_task("root-a", "root-a"))
            .expect("root a should insert");
        store
            .insert_task(rooted_task("root-b", "root-b"))
            .expect("root b should insert");
        store
            .checkpoint_to_projection_directory(&projection_dir)
            .expect("first generation should commit");

        let first_manifest = TaskStore::read_projection_manifest_if_present(&projection_dir)
            .expect("manifest should read")
            .expect("first manifest should exist");
        let first_generation_dir = projection_dir.join(TASK_PROJECTION_GENERATIONS_DIR).join(
            TaskStore::generation_directory_name(first_manifest.generation),
        );
        let root_a_file = first_generation_dir.join(TaskStore::projection_file_name("root-a"));
        let root_b_file = first_generation_dir.join(TaskStore::projection_file_name("root-b"));
        let root_a_inode = fs::metadata(&root_a_file)
            .expect("root a projection should exist")
            .ino();
        let root_b_inode = fs::metadata(&root_b_file)
            .expect("root b projection should exist")
            .ino();

        store
            .update_task_goal(&TaskId::new("root-a"), "updated root a".to_string())
            .expect("root a goal should update");
        let mut snapshot = store.snapshot();
        snapshot.changed_root_ids = vec![TaskId::new("root-a")];
        TaskStore::checkpoint_snapshot_to_projection_directory(&snapshot, &projection_dir)
            .expect("second generation should commit");

        let second_manifest = TaskStore::read_projection_manifest_if_present(&projection_dir)
            .expect("manifest should read")
            .expect("second manifest should exist");
        let second_generation_dir = projection_dir.join(TASK_PROJECTION_GENERATIONS_DIR).join(
            TaskStore::generation_directory_name(second_manifest.generation),
        );
        let second_root_a_inode =
            fs::metadata(second_generation_dir.join(TaskStore::projection_file_name("root-a")))
                .expect("updated root a projection should exist")
                .ino();
        let second_root_b_inode =
            fs::metadata(second_generation_dir.join(TaskStore::projection_file_name("root-b")))
                .expect("unchanged root b projection should exist")
                .ino();

        assert_ne!(second_root_a_inode, root_a_inode);
        assert_eq!(second_root_b_inode, root_b_inode);
    }

    #[test]
    fn projection_restore_rejects_duplicate_tasks_and_invalid_leases() {
        let duplicate_dir = tempfile::tempdir().expect("duplicate fixture");
        let shared_a = rooted_task("root-a", "shared-task");
        let shared_b = rooted_task("root-b", "shared-task");
        projection_fixture(
            duplicate_dir.path(),
            1,
            &[
                TaskRootProjection {
                    schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                    generation: 1,
                    root_task_id: TaskId::new("root-a"),
                    tasks: vec![rooted_task("root-a", "root-a"), shared_a],
                    leases: Vec::new(),
                },
                TaskRootProjection {
                    schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                    generation: 1,
                    root_task_id: TaskId::new("root-b"),
                    tasks: vec![rooted_task("root-b", "root-b"), shared_b],
                    leases: Vec::new(),
                },
            ],
        );
        assert_eq!(
            TaskStore::restore_from_projection_directory(duplicate_dir.path())
                .err()
                .expect("duplicate task IDs must fail")
                .kind(),
            io::ErrorKind::InvalidData
        );

        let lease_dir = tempfile::tempdir().expect("lease fixture");
        let now = UtcMillis::now();
        projection_fixture(
            lease_dir.path(),
            1,
            &[TaskRootProjection {
                schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                generation: 1,
                root_task_id: TaskId::new("root-lease"),
                tasks: vec![rooted_task("root-lease", "root-lease")],
                leases: vec![TaskLease {
                    lease_id: LeaseId::new("lease-orphan"),
                    task_id: TaskId::new("missing-task"),
                    root_task_id: TaskId::new("root-lease"),
                    worker_id: WorkerId::new("worker-lease"),
                    role: "executor".to_string(),
                    granted_at: now,
                    expires_at: now,
                    heartbeat_at: now,
                    lease_status: TaskLeaseState::Active,
                }],
            }],
        );
        assert_eq!(
            TaskStore::restore_from_projection_directory(lease_dir.path())
                .err()
                .expect("orphan lease must fail")
                .kind(),
            io::ErrorKind::InvalidData
        );

        let duplicate_lease_dir = tempfile::tempdir().expect("duplicate lease fixture");
        let duplicate_lease_id = LeaseId::new("lease-duplicate");
        projection_fixture(
            duplicate_lease_dir.path(),
            1,
            &[
                TaskRootProjection {
                    schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                    generation: 1,
                    root_task_id: TaskId::new("root-lease-a"),
                    tasks: vec![rooted_task("root-lease-a", "root-lease-a")],
                    leases: vec![TaskLease {
                        lease_id: duplicate_lease_id.clone(),
                        task_id: TaskId::new("root-lease-a"),
                        root_task_id: TaskId::new("root-lease-a"),
                        worker_id: WorkerId::new("worker-a"),
                        role: "executor".to_string(),
                        granted_at: now,
                        expires_at: now,
                        heartbeat_at: now,
                        lease_status: TaskLeaseState::Active,
                    }],
                },
                TaskRootProjection {
                    schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                    generation: 1,
                    root_task_id: TaskId::new("root-lease-b"),
                    tasks: vec![rooted_task("root-lease-b", "root-lease-b")],
                    leases: vec![TaskLease {
                        lease_id: duplicate_lease_id,
                        task_id: TaskId::new("root-lease-b"),
                        root_task_id: TaskId::new("root-lease-b"),
                        worker_id: WorkerId::new("worker-b"),
                        role: "executor".to_string(),
                        granted_at: now,
                        expires_at: now,
                        heartbeat_at: now,
                        lease_status: TaskLeaseState::Active,
                    }],
                },
            ],
        );
        assert_eq!(
            TaskStore::restore_from_projection_directory(duplicate_lease_dir.path())
                .err()
                .expect("duplicate lease IDs must fail")
                .kind(),
            io::ErrorKind::InvalidData
        );

        let wrong_lease_owner_dir = tempfile::tempdir().expect("wrong lease owner fixture");
        projection_fixture(
            wrong_lease_owner_dir.path(),
            1,
            &[
                TaskRootProjection {
                    schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                    generation: 1,
                    root_task_id: TaskId::new("root-owner-a"),
                    tasks: vec![rooted_task("root-owner-a", "root-owner-a")],
                    leases: vec![TaskLease {
                        lease_id: LeaseId::new("lease-wrong-owner"),
                        task_id: TaskId::new("root-owner-b"),
                        root_task_id: TaskId::new("root-owner-a"),
                        worker_id: WorkerId::new("worker-owner"),
                        role: "executor".to_string(),
                        granted_at: now,
                        expires_at: now,
                        heartbeat_at: now,
                        lease_status: TaskLeaseState::Active,
                    }],
                },
                TaskRootProjection {
                    schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                    generation: 1,
                    root_task_id: TaskId::new("root-owner-b"),
                    tasks: vec![rooted_task("root-owner-b", "root-owner-b")],
                    leases: Vec::new(),
                },
            ],
        );
        assert_eq!(
            TaskStore::restore_from_projection_directory(wrong_lease_owner_dir.path())
                .err()
                .expect("lease assigned to another root must fail")
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn checkpoint_and_restore_reject_terminal_task_with_active_lease() {
        for (label, status) in [
            ("completed", TaskStatus::Completed),
            ("failed", TaskStatus::Failed),
            ("killed", TaskStatus::Killed),
        ] {
            let root_task_id = TaskId::new(format!("root-invalid-active-{label}"));
            let mut task = rooted_task(root_task_id.as_str(), root_task_id.as_str());
            task.status = status;
            let now = UtcMillis::now();
            let snapshot = TaskStoreSnapshot {
                tasks: vec![task],
                leases: vec![TaskLease {
                    lease_id: LeaseId::new(format!("lease-invalid-active-{label}")),
                    task_id: root_task_id.clone(),
                    root_task_id: root_task_id.clone(),
                    worker_id: WorkerId::new(format!("worker-invalid-active-{label}")),
                    role: "executor".to_string(),
                    granted_at: now,
                    expires_at: UtcMillis(now.0.saturating_add(60_000)),
                    heartbeat_at: now,
                    lease_status: TaskLeaseState::Active,
                }],
                changed_root_ids: Vec::new(),
            };
            let checkpoint_dir = tempfile::tempdir().expect("checkpoint directory");
            let projection_dir = checkpoint_dir.path().join("projection");
            assert_eq!(
                TaskStore::checkpoint_snapshot_to_projection_directory(&snapshot, &projection_dir)
                    .expect_err("invalid active lease must reject checkpoint")
                    .kind(),
                io::ErrorKind::InvalidData
            );

            let restore_dir = tempfile::tempdir().expect("restore directory");
            projection_fixture(
                restore_dir.path(),
                1,
                &[TaskRootProjection {
                    schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                    generation: 1,
                    root_task_id: root_task_id.clone(),
                    tasks: snapshot.tasks.clone(),
                    leases: snapshot.leases.clone(),
                }],
            );
            assert_eq!(
                TaskStore::restore_from_projection_directory(restore_dir.path())
                    .err()
                    .expect("invalid active lease must reject restore")
                    .kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn checkpoint_and_restore_reject_duplicate_active_leases_for_one_task() {
        let root_task_id = TaskId::new("root-duplicate-active-leases");
        let mut task = rooted_task(root_task_id.as_str(), root_task_id.as_str());
        task.status = TaskStatus::Running;
        let now = UtcMillis::now();
        let leases = vec![
            TaskLease {
                lease_id: LeaseId::new("lease-duplicate-active-a"),
                task_id: root_task_id.clone(),
                root_task_id: root_task_id.clone(),
                worker_id: WorkerId::new("worker-duplicate-active-a"),
                role: "executor".to_string(),
                granted_at: now,
                expires_at: UtcMillis(now.0.saturating_add(60_000)),
                heartbeat_at: now,
                lease_status: TaskLeaseState::Active,
            },
            TaskLease {
                lease_id: LeaseId::new("lease-duplicate-active-b"),
                task_id: root_task_id.clone(),
                root_task_id: root_task_id.clone(),
                worker_id: WorkerId::new("worker-duplicate-active-b"),
                role: "executor".to_string(),
                granted_at: now,
                expires_at: UtcMillis(now.0.saturating_add(60_000)),
                heartbeat_at: now,
                lease_status: TaskLeaseState::Active,
            },
        ];
        let snapshot = TaskStoreSnapshot {
            tasks: vec![task],
            leases,
            changed_root_ids: Vec::new(),
        };
        let checkpoint_dir = tempfile::tempdir().expect("checkpoint directory");
        let projection_dir = checkpoint_dir.path().join("projection");
        assert_eq!(
            TaskStore::checkpoint_snapshot_to_projection_directory(&snapshot, &projection_dir)
                .expect_err("duplicate active leases must reject checkpoint")
                .kind(),
            io::ErrorKind::InvalidData
        );

        let restore_dir = tempfile::tempdir().expect("restore directory");
        projection_fixture(
            restore_dir.path(),
            1,
            &[TaskRootProjection {
                schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                generation: 1,
                root_task_id,
                tasks: snapshot.tasks,
                leases: snapshot.leases,
            }],
        );
        assert_eq!(
            TaskStore::restore_from_projection_directory(restore_dir.path())
                .err()
                .expect("duplicate active leases must reject restore")
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn lease_creation_rejects_non_running_active_lease_contract() {
        for (label, status) in [
            ("completed", TaskStatus::Completed),
            ("failed", TaskStatus::Failed),
            ("killed", TaskStatus::Killed),
        ] {
            let root_task_id = TaskId::new(format!("root-lease-contract-{label}"));
            let mut task = rooted_task(root_task_id.as_str(), root_task_id.as_str());
            task.status = status;
            let store = TaskStore::new();
            store.insert_task(task.clone()).expect("task should insert");
            let now = UtcMillis::now();
            let lease = TaskLease {
                lease_id: LeaseId::new(format!("lease-contract-{label}")),
                task_id: task.task_id.clone(),
                root_task_id: task.root_task_id.clone(),
                worker_id: WorkerId::new(format!("worker-contract-{label}")),
                role: "executor".to_string(),
                granted_at: now,
                expires_at: UtcMillis(now.0.saturating_add(60_000)),
                heartbeat_at: now,
                lease_status: TaskLeaseState::Active,
            };
            assert!(matches!(
                store.insert_lease(lease),
                Err(DomainError::InvalidState { .. })
            ));
            assert!(
                store
                    .grant_lease(
                        &task.task_id,
                        &task.root_task_id,
                        &WorkerId::new(format!("worker-grant-contract-{label}")),
                        "executor",
                        60_000,
                    )
                    .is_none()
            );
            assert!(store.get_active_lease(&task.task_id).is_none());
        }
    }

    #[test]
    fn projection_restore_rejects_wrong_filename_empty_and_missing_schema() {
        let wrong_name_dir = tempfile::tempdir().expect("wrong name fixture");
        projection_fixture(
            wrong_name_dir.path(),
            1,
            &[TaskRootProjection {
                schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                generation: 1,
                root_task_id: TaskId::new("root-name"),
                tasks: vec![rooted_task("root-name", "root-name")],
                leases: Vec::new(),
            }],
        );
        let mut manifest = TaskStore::read_projection_manifest(wrong_name_dir.path())
            .expect("fixture manifest should read");
        manifest.roots[0].file_name = "another-root.json".to_string();
        fs::write(
            wrong_name_dir.path().join(TASK_PROJECTION_MANIFEST_FILE),
            serde_json::to_vec(&manifest).expect("manifest should encode"),
        )
        .expect("manifest should rewrite");
        assert!(TaskStore::restore_from_projection_directory(wrong_name_dir.path()).is_err());

        let empty_dir = tempfile::tempdir().expect("empty fixture");
        projection_fixture(
            empty_dir.path(),
            1,
            &[TaskRootProjection {
                schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                generation: 1,
                root_task_id: TaskId::new("root-empty"),
                tasks: Vec::new(),
                leases: Vec::new(),
            }],
        );
        assert!(TaskStore::restore_from_projection_directory(empty_dir.path()).is_err());

        let schema_dir = tempfile::tempdir().expect("schema fixture");
        let generation_dir = schema_dir
            .path()
            .join(TASK_PROJECTION_GENERATIONS_DIR)
            .join(TaskStore::generation_directory_name(1));
        fs::create_dir_all(&generation_dir).expect("generation should create");
        fs::write(generation_dir.join("root-schema.json"), b"{}")
            .expect("invalid projection should write");
        fs::write(
            schema_dir.path().join(TASK_PROJECTION_MANIFEST_FILE),
            serde_json::to_vec(&TaskProjectionManifest {
                schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                generation: 1,
                roots: vec![TaskProjectionManifestRoot {
                    root_task_id: TaskId::new("root-schema"),
                    file_name: "root-schema.json".to_string(),
                    generation: 1,
                    content_hash: String::new(),
                }],
            })
            .expect("manifest should encode"),
        )
        .expect("manifest should write");
        assert!(TaskStore::restore_from_projection_directory(schema_dir.path()).is_err());

        let missing_task_field_dir = tempfile::tempdir().expect("missing task field fixture");
        projection_fixture(
            missing_task_field_dir.path(),
            1,
            &[TaskRootProjection {
                schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                generation: 1,
                root_task_id: TaskId::new("root-required-field"),
                tasks: vec![rooted_task("root-required-field", "root-required-field")],
                leases: Vec::new(),
            }],
        );
        let projection_path = missing_task_field_dir
            .path()
            .join(TASK_PROJECTION_GENERATIONS_DIR)
            .join(TaskStore::generation_directory_name(1))
            .join("root-required-field.json");
        let mut projection: serde_json::Value = serde_json::from_slice(
            &fs::read(&projection_path).expect("projection fixture should read"),
        )
        .expect("projection fixture should parse");
        projection["tasks"][0]
            .as_object_mut()
            .expect("task fixture should be an object")
            .remove("runtime_payload");
        fs::write(
            projection_path,
            serde_json::to_vec_pretty(&projection).expect("projection fixture should encode"),
        )
        .expect("projection fixture should rewrite");
        assert_eq!(
            TaskStore::restore_from_projection_directory(missing_task_field_dir.path())
                .err()
                .expect("missing v2 task fields must fail")
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn checkpoint_callback_failure_is_returned_for_terminal_and_delete_mutations() {
        fn failing_callback(_: &TaskStoreSnapshot) -> DomainResult<()> {
            Err(DomainError::Persistence {
                message: "checkpoint unavailable".to_string(),
            })
        }

        let failed_store = TaskStore::new();
        failed_store
            .insert_task(rooted_task("root-failed", "root-failed"))
            .expect("root should insert before callback");
        failed_store.set_checkpoint_callback(Box::new(failing_callback));
        assert!(
            failed_store
                .revoke_lease_and_set_task_terminal(
                    &TaskId::new("root-failed"),
                    &TaskId::new("root-failed"),
                    None,
                    TaskStatus::Failed,
                    Vec::new(),
                )
                .is_err()
        );

        let completed_store = TaskStore::new();
        completed_store
            .insert_task(rooted_task("root-completed", "root-completed"))
            .expect("root should insert before callback");
        completed_store.set_checkpoint_callback(Box::new(failing_callback));
        assert!(
            completed_store
                .complete_task(&TaskId::new("root-completed"), successful_attempt())
                .is_err()
        );
        assert_eq!(
            completed_store
                .get_task(&TaskId::new("root-completed"))
                .expect("failed completion must leave task in memory")
                .status,
            TaskStatus::Running
        );

        let delete_store = TaskStore::new();
        delete_store
            .insert_task(rooted_task("root-delete", "root-delete"))
            .expect("root should insert before callback");
        delete_store.set_checkpoint_callback(Box::new(failing_callback));
        assert!(
            delete_store
                .remove_task(&TaskId::new("root-delete"))
                .is_err()
        );
        assert!(delete_store.get_task(&TaskId::new("root-delete")).is_some());
        let retried_checkpoint_count = std::sync::Arc::new(AtomicU64::new(0));
        let observed_retry_count = retried_checkpoint_count.clone();
        delete_store.set_checkpoint_callback(Box::new(move |_| {
            observed_retry_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        assert!(
            delete_store
                .remove_task(&TaskId::new("root-delete"))
                .expect("retry should checkpoint")
                .is_some()
        );
        assert_eq!(retried_checkpoint_count.load(Ordering::SeqCst), 1);
    }
}
