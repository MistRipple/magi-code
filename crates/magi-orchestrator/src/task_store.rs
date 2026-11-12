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
            runtime_change_notify: tokio::sync::Notify::new(),
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
        self.runtime_change_notify.notify_waiters();
    }

    pub fn status_change_version(&self) -> u64 {
        *self
            .status_change_version
            .lock()
            .expect("status_change_version lock poisoned")
    }

    /// 异步宿主复用相同的提交后通知；先取 future 再读事实，避免查询期间丢失唤醒。
    /// 不复制 Task/转交状态，也不需要轮询或为每个等待者启动阻塞线程。
    pub fn runtime_change_notification(&self) -> impl std::future::Future<Output = ()> + Send + '_ {
        self.runtime_change_notify.notified()
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

    /// 根据 Task 中的不可变 activation 来源读取会话拥有的代理，不另存身份注册表。
    pub fn agent_heads_for_session(&self, session_id: &magi_core::SessionId) -> Vec<Task> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let mut heads = std::collections::BTreeMap::<String, &Task>::new();
        for task in tasks.values() {
            let Some(activation) = task
                .agent_activation()
                .filter(|binding| &binding.session_id == session_id)
            else {
                continue;
            };
            let entry = heads.entry(activation.agent_id.to_string()).or_insert(task);
            if activation.generation > entry.agent_activation().expect("agent task").generation {
                *entry = task;
            }
        }
        heads.into_values().cloned().collect()
    }

    /// 两层代理试点以真实父链校验，不从展示名称推断深度。
    pub fn validate_agent_delegation(&self, parent: &Task) -> DomainResult<()> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        Self::validate_agent_delegation_inner(&tasks, parent)
    }

    /// 根代理可随会话进入新的主线 Turn；嵌套代理只能由同一父 Agent 的新 activation 继续。
    pub fn agent_resume_parent_matches(&self, source: &Task, parent: &Task) -> bool {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        Self::same_agent_owner(
            source.parent_task_id.as_ref().and_then(|id| tasks.get(id)),
            parent,
        )
    }

    fn same_agent_owner(previous_parent: Option<&Task>, parent: &Task) -> bool {
        let Some(previous) = previous_parent else {
            return false;
        };
        if previous.parent_task_id.is_none() || parent.parent_task_id.is_none() {
            return previous.parent_task_id.is_none() && parent.parent_task_id.is_none();
        }
        match (previous.agent_activation(), parent.agent_activation()) {
            (Some(previous), Some(current)) => {
                previous.agent_id == current.agent_id && previous.session_id == current.session_id
            }
            _ => false,
        }
    }

    fn validate_agent_delegation_inner(
        tasks: &HashMap<TaskId, Task>,
        parent: &Task,
    ) -> DomainResult<()> {
        let latest = tasks
            .get(&parent.task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        if !matches!(latest.status, TaskStatus::Pending | TaskStatus::Running)
            || latest.parent_task_id != parent.parent_task_id
            || latest.root_task_id != parent.root_task_id
            || latest.mission_id != parent.mission_id
            || latest.delegation_allowed() != parent.delegation_allowed()
        {
            return Err(DomainError::InvalidState {
                message: "派发父任务已失效或委派授权已变化".into(),
            });
        }
        if parent.parent_task_id.is_none() && parent.root_task_id == parent.task_id {
            return Ok(());
        }
        let root = tasks
            .get(&parent.root_task_id)
            .ok_or(DomainError::NotFound { entity: "RootTask" })?;
        if !parent.delegation_allowed()
            || parent.parent_task_id.as_ref() != Some(&root.task_id)
            || root.parent_task_id.is_some()
            || root.task_id != root.root_task_id
            || root.mission_id != parent.mission_id
            || (root.status != TaskStatus::Running && latest.agent_run_transfer().is_none())
            || root.executor_binding_target_role() != Some("coordinator")
            || !Self::ancestor_chain_is_active(latest, tasks, false)
        {
            return Err(DomainError::InvalidState {
                message: "嵌套派发需要主线显式委派，且最多两层代理".into(),
            });
        }
        Ok(())
    }

    fn validate_agent_activation_insert(
        tasks: &HashMap<TaskId, Task>,
        task: &Task,
    ) -> DomainResult<()> {
        Self::validate_project_directory_inheritance(
            task,
            task.parent_task_id.as_ref().and_then(|id| tasks.get(id)),
        )?;
        if task.agent_run_transfer().is_some() {
            return Err(DomainError::InvalidState {
                message: "创建任务不能携带已转交状态，必须从显式转交事务接纳".into(),
            });
        }
        let owner = task
            .parent_task_id
            .as_ref()
            .map(|parent| Self::agent_run_transfer_in_tasks(tasks, parent))
            .transpose()?
            .flatten()
            .map(|(root, _)| &root.task_id);
        // 转交后创建的新任务同样不能重新建立跨拥有方依赖。
        for dependency in task.dependency_ids.iter().chain(&task.required_children) {
            let dependency_owner = Self::agent_run_transfer_in_tasks(tasks, dependency)?
                .map(|(root, _)| &root.task_id);
            if owner != dependency_owner {
                return Err(DomainError::InvalidState {
                    message: "新任务不能建立跨转交子树的调度或完成依赖".into(),
                });
            }
        }
        for candidate in tasks.values() {
            if candidate
                .dependency_ids
                .iter()
                .chain(&candidate.required_children)
                .any(|id| id == &task.task_id)
                && Self::agent_run_transfer_in_tasks(tasks, &candidate.task_id)?
                    .map(|(root, _)| &root.task_id)
                    != owner
            {
                return Err(DomainError::InvalidState {
                    message: "任务身份被另一拥有方预留为调度或完成依赖".into(),
                });
            }
        }
        let Some(binding) = task.agent_activation() else {
            if task.delegation_allowed() || owner.is_some() {
                return Err(DomainError::InvalidState {
                    message: "受委派或已转交子树中的代理必须有持久 activation 身份".into(),
                });
            }
            return Ok(());
        };
        binding.validate(&task.task_id)?;
        let parent = task
            .parent_task_id
            .as_ref()
            .and_then(|id| tasks.get(id))
            .ok_or_else(|| DomainError::InvalidState {
                message: "Agent activation 缺少父任务".into(),
            })?;
        if !matches!(parent.status, TaskStatus::Pending | TaskStatus::Running)
            || parent.root_task_id != task.root_task_id
            || parent.mission_id != task.mission_id
        {
            return Err(DomainError::InvalidState {
                message: "Agent activation 父任务已失效或作用域不匹配".into(),
            });
        }
        if parent.parent_task_id.is_some() || task.delegation_allowed() {
            Self::validate_agent_delegation_inner(tasks, parent)?;
            if (task.delegation_allowed()
                && (parent.parent_task_id.is_some()
                    || parent.executor_binding_target_role() != Some("coordinator")))
                || task.workspace_scope != parent.workspace_scope
                || task.write_scope != parent.write_scope
                || task.policy_snapshot != parent.policy_snapshot
            {
                return Err(DomainError::InvalidState {
                    message: "嵌套任务不能增加委派深度或改变父任务授权边界".into(),
                });
            }
        }
        if let Some((root, transfer)) = Self::agent_run_transfer_in_tasks(tasks, &parent.task_id)? {
            Self::validate_agent_transfer_subtree(
                tasks.values().chain(std::iter::once(task)),
                root,
                transfer,
            )?;
        }
        let prior = tasks
            .values()
            .filter(|candidate| {
                candidate
                    .agent_activation()
                    .is_some_and(|source| source.agent_id == binding.agent_id)
            })
            .max_by_key(|candidate| candidate.agent_activation().expect("agent task").generation);
        match (prior, &binding.previous_task_id) {
            (None, None) if binding.generation == 1 => Ok(()),
            (Some(prior), Some(previous)) => {
                let source = prior.agent_activation().expect("agent task");
                if prior.task_id != *previous
                    || prior.status != TaskStatus::Completed
                    || source.generation.checked_add(1) != Some(binding.generation)
                    || source.session_id != binding.session_id
                    || source.lifetime != binding.lifetime
                    || matches!(source.lifetime, magi_core::AgentLifetime::Turn)
                    || (task.delegation_allowed() && !prior.delegation_allowed())
                    || prior.mission_id != task.mission_id
                    || !Self::same_agent_owner(
                        prior.parent_task_id.as_ref().and_then(|id| tasks.get(id)),
                        parent,
                    )
                    || prior.workspace_scope != task.workspace_scope
                    || prior.write_scope != task.write_scope
                    || prior.executor_binding_target_role() != task.executor_binding_target_role()
                    || task
                        .executor_binding_capability_ids()
                        .iter()
                        .any(|capability| {
                            !prior.executor_binding_capability_ids().contains(capability)
                        })
                {
                    return Err(DomainError::InvalidState {
                        message: "Agent 已有更新/活动 activation 或来源作用域冲突".into(),
                    });
                }
                Ok(())
            }
            _ => Err(DomainError::InvalidState {
                message: "Agent activation 来源缺失或代际冲突".into(),
            }),
        }
    }

    fn validate_project_directory_inheritance(
        task: &Task,
        parent: Option<&Task>,
    ) -> DomainResult<()> {
        let binding = task
            .executor_binding
            .as_ref()
            .and_then(|binding| binding.project_directories.as_ref());
        let parent_binding = parent
            .and_then(|task| task.executor_binding.as_ref())
            .and_then(|binding| binding.project_directories.as_ref());
        if let Some(binding) = binding {
            binding.validate()?;
        }
        match (binding, parent_binding, task.parent_task_id.as_ref()) {
            (Some(child), Some(parent), Some(_)) => child.validate_inherited_from(parent),
            (None, None, _) | (Some(_), None, None) => Ok(()),
            _ => Err(DomainError::InvalidState {
                message: "子任务缺少父目录授权或试图自行扩大项目范围".into(),
            }),
        }
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

    /// 显式转交只改变直接子树根的拥有方，不迁移 Task/lease 或复制后代状态。
    /// API 还须在会话提交锁下校验来源 Turn、Goal 控制版本及资源可独立运行。
    pub fn transfer_agent_subtree(
        &self,
        parent_id: &TaskId,
        child_id: &TaskId,
        transfer: magi_core::AgentRunTransfer,
    ) -> DomainResult<bool> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        let parent = tasks.get(parent_id).ok_or(DomainError::NotFound {
            entity: "ParentTask",
        })?;
        let child = tasks.get(child_id).ok_or(DomainError::NotFound {
            entity: "AgentTask",
        })?;
        Self::validate_agent_transfer_binding(parent, child, &transfer)?;
        if let Some(existing) = child.agent_run_transfer() {
            if existing == &transfer {
                return Ok(false);
            }
            return Err(DomainError::InvalidState {
                message: "子树已经转交，不能改写原拥有方或来源".into(),
            });
        }
        if parent.status != TaskStatus::Running
            || !matches!(child.status, TaskStatus::Pending | TaskStatus::Running)
            || transfer.transferred_at < child.created_at
        {
            return Err(DomainError::InvalidState {
                message: "转交需要父任务仍在运行、子树根尚未结束".into(),
            });
        }
        if tasks.values().any(|candidate| {
            candidate
                .agent_run_transfer()
                .is_some_and(|existing| existing.operation_id == transfer.operation_id)
        }) {
            return Err(DomainError::InvalidState {
                message: "转交 operation_id 已用于其他子树".into(),
            });
        }
        Self::validate_agent_transfer_subtree(tasks.values(), child, &transfer)?;
        let root_id = parent.root_task_id.clone();
        let child = tasks.get_mut(child_id).expect("child checked");
        child
            .executor_binding
            .as_mut()
            .expect("activation binding checked")
            .agent_run_transfer = Some(transfer);
        child.updated_at = UtcMillis::now();
        let leases = self
            .leases
            .read()
            .expect("leases read lock poisoned")
            .clone();
        let index = self
            .mission_index
            .read()
            .expect("mission index read lock poisoned")
            .clone();
        let snapshot = Self::snapshot_from_maps_with_dirty_roots(&tasks, &leases, vec![root_id]);
        self.fire_checkpoint(&snapshot)?;
        self.commit_maps(tasks, leases, index);
        self.notify_status_change();
        Ok(true)
    }

    fn validate_agent_transfer_binding(
        parent: &Task,
        child: &Task,
        transfer: &magi_core::AgentRunTransfer,
    ) -> DomainResult<()> {
        let activation = child
            .agent_activation()
            .ok_or_else(|| DomainError::InvalidState {
                message: "转交子树需要持久 Agent 身份".into(),
            })?;
        transfer.validate(activation)?;
        if parent.parent_task_id.is_some()
            || parent.root_task_id != parent.task_id
            || parent.executor_binding_target_role() != Some("coordinator")
            || child.parent_task_id.as_ref() != Some(&parent.task_id)
            || child.root_task_id != parent.root_task_id
            || child.mission_id != parent.mission_id
            || transfer.transferred_by != parent.task_id
            || child.kind != magi_core::TaskKind::LocalAgent
        {
            return Err(DomainError::InvalidState {
                message: "只有主线能转交自己的直接子代理，不能改变任务树归属".into(),
            });
        }
        Ok(())
    }

    /// 创建与重载读取相同的转交合同。共享计划和跨拥有方的调度依赖不能随子树留在后台。
    fn validate_agent_transfer_subtree<'a>(
        tasks: impl Iterator<Item = &'a Task>,
        child: &Task,
        transfer: &magi_core::AgentRunTransfer,
    ) -> DomainResult<()> {
        let tasks = tasks.collect::<Vec<_>>();
        let mut children = HashMap::<&TaskId, Vec<&Task>>::new();
        for task in &tasks {
            if let Some(parent) = &task.parent_task_id {
                children.entry(parent).or_default().push(task);
            }
        }
        let mut subtree = vec![child];
        let mut ids = HashSet::new();
        let mut cursor = 0;
        while cursor < subtree.len() {
            let task = subtree[cursor];
            if !ids.insert(task.task_id.clone())
                || task.root_task_id != child.root_task_id
                || task.mission_id != child.mission_id
                || task.kind != magi_core::TaskKind::LocalAgent
                || task.workspace_scope != child.workspace_scope
                || task.write_scope != child.write_scope
                || task
                    .agent_activation()
                    .is_none_or(|activation| activation.session_id != transfer.session_id)
                || task
                    .executor_binding
                    .as_ref()
                    .is_some_and(|binding| binding.plan_item_id.is_some())
                || (task.task_id != child.task_id && task.agent_run_transfer().is_some())
            {
                return Err(DomainError::InvalidState {
                    message: "子树存在循环、异域任务、共享计划或重复转交，不能独立运行".into(),
                });
            }
            if let Some(descendants) = children.get(&task.task_id) {
                subtree.extend(descendants.iter().copied());
            }
            cursor += 1;
        }
        if transfer.transferred_at < child.created_at
            || tasks.iter().any(|task| {
                task.dependency_ids
                    .iter()
                    .chain(&task.required_children)
                    .any(|dependency| ids.contains(&task.task_id) != ids.contains(dependency))
            })
        {
            return Err(DomainError::InvalidState {
                message: "子树转交时间无效或存在跨拥有方的调度/完成依赖，必须先结算依赖".into(),
            });
        }
        Ok(())
    }

    /// 只读返回现有父链上的转交根 Task，包含原 activation 与唯一转交记录。
    /// 无转交与缺失/损坏的查询来源分开返回，调用方不得将查询失败当作原父轮授权。
    pub fn agent_run_transfer_for_task(&self, task_id: &TaskId) -> DomainResult<Option<Task>> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        if !tasks.contains_key(task_id) {
            return Err(DomainError::NotFound {
                entity: "AgentTask",
            });
        }
        Self::agent_run_transfer_in_tasks(&tasks, task_id)
            .map(|boundary| boundary.map(|(root, _)| root.clone()))
    }

    fn agent_run_transfer_in_tasks<'a>(
        tasks: &'a HashMap<TaskId, Task>,
        task_id: &TaskId,
    ) -> DomainResult<Option<(&'a Task, &'a magi_core::AgentRunTransfer)>> {
        // 未创建的依赖项可以没有拥有方；公开执行查询另外要求来源 Task 存在。
        let Some(original) = tasks.get(task_id) else {
            return Ok(None);
        };
        let mut current = original;
        let mut seen = HashSet::new();
        loop {
            if current.root_task_id != original.root_task_id
                || current.mission_id != original.mission_id
                || !seen.insert(current.task_id.clone())
            {
                return Err(DomainError::InvalidState {
                    message: "Agent 转交查询的父链循环或作用域不匹配".into(),
                });
            }
            if let Some(transfer) = current.agent_run_transfer() {
                return Ok(Some((current, transfer)));
            }
            let Some(parent) = &current.parent_task_id else {
                return Ok(None);
            };
            current = tasks.get(parent).ok_or_else(|| DomainError::InvalidState {
                message: "Agent 转交查询的来源父任务缺失".into(),
            })?;
        }
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

    /// 显式继续同一转交范围；只重开调用方已核对的中断任务，不改写原父链或完成结果。
    pub fn reopen_transferred_agent_scope(
        &self,
        root: &TaskId,
        expected: &[Task],
    ) -> DomainResult<()> {
        let guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks lock poisoned").clone();
        let leases = self.leases.read().expect("leases lock poisoned").clone();
        if expected.first().is_none_or(|task| &task.task_id != root)
            || tasks
                .get(root)
                .is_none_or(|task| task.agent_run_transfer().is_none())
        {
            return Err(DomainError::InvalidState {
                message: "独立继续需要原转交子树根".into(),
            });
        }
        let ids = collect_subtree_ids_in_scope(&tasks, root, true);
        if ids.iter().any(|id| {
            tasks.get(id).is_some_and(|task| {
                matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
            }) || leases
                .values()
                .any(|lease| &lease.task_id == id && lease.lease_status == TaskLeaseState::Active)
        }) {
            return Err(DomainError::InvalidState {
                message: "代理子树仍有活动任务或租约，不能继续".into(),
            });
        }
        let mut changed = Vec::new();
        for source in expected {
            let task = tasks.get(&source.task_id).ok_or(DomainError::NotFound {
                entity: "AgentTask",
            })?;
            if !ids.contains(&source.task_id)
                || task.status != TaskStatus::Failed
                || task.updated_at != source.updated_at
                || task.retry_count != source.retry_count
                || task.agent_activation() != source.agent_activation()
            {
                return Err(DomainError::InvalidState {
                    message: "代理恢复来源已变化".into(),
                });
            }
            let task = tasks.get_mut(&source.task_id).expect("validated task");
            task.retry_count =
                task.retry_count
                    .checked_add(1)
                    .ok_or_else(|| DomainError::InvalidState {
                        message: "恢复代际溢出".into(),
                    })?;
            task.status = TaskStatus::Pending;
            task.updated_at = UtcMillis(
                UtcMillis::now()
                    .0
                    .max(source.updated_at.0.saturating_add(1)),
            );
            changed.push(task.clone());
        }
        let index = self
            .mission_index
            .read()
            .expect("mission index lock poisoned")
            .clone();
        self.fire_checkpoint(&Self::snapshot_from_maps_with_dirty_roots(
            &tasks,
            &leases,
            vec![expected[0].root_task_id.clone()],
        ))?;
        self.commit_maps(tasks, leases, index);
        drop(guard);
        for task in changed {
            self.emit_status_change(
                &task.task_id.clone(),
                TaskStatus::Failed,
                TaskStatus::Pending,
                task,
            );
        }
        Ok(())
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

    /// 原子提交补充、访问回执与 revision，并在同一锁内复查接纳状态。
    /// Pending -> Running 与关闭边界不能把“已接纳”补充遗落在旧启动快照里。
    pub fn deliver_agent_context_supplement(
        &self,
        task_id: &TaskId,
        expected_status: TaskStatus,
        supplement: AgentContextSupplement,
        access: AgentContextAccessRecord,
    ) -> DomainResult<AgentContextPackage> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        let task = tasks
            .get(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        let parent = tasks
            .get(&supplement.author_task_id)
            .ok_or(DomainError::NotFound {
                entity: "parent Task",
            })?;
        if !matches!(expected_status, TaskStatus::Pending | TaskStatus::Running)
            || task.status != expected_status
            || !matches!(parent.status, TaskStatus::Pending | TaskStatus::Running)
            || task.parent_task_id.as_ref() != Some(&parent.task_id)
            || task.root_task_id != parent.root_task_id
            || task.mission_id != parent.mission_id
            || task.workspace_scope != parent.workspace_scope
            || task.write_scope != parent.write_scope
        {
            return Err(DomainError::InvalidState {
                message: "代理接纳状态或父任务作用域已变化，请刷新后重新投递".into(),
            });
        }
        let task = tasks.get_mut(task_id).expect("validated task exists");
        let dirty_root_id = task.root_task_id.clone();
        let TaskRuntimePayload::AgentContext { package, accesses } = &mut task.runtime_payload
        else {
            return Err(DomainError::InvalidState {
                message: format!("任务 {task_id} 没有 agent context package"),
            });
        };
        if package.parent_task_id != supplement.author_task_id
            || access.operation != magi_core::AgentContextAccessOperation::Send
            || access.query.as_deref() != Some(supplement.message.as_str())
            || package
                .supplements
                .iter()
                .any(|entry| entry.supplement_id == supplement.supplement_id)
        {
            return Err(DomainError::InvalidState {
                message: "代理补充身份或发送回执冲突".into(),
            });
        }
        package.revision =
            package
                .revision
                .checked_add(1)
                .ok_or_else(|| DomainError::InvalidState {
                    message: "代理上下文 revision 溢出".into(),
                })?;
        package.updated_at = supplement.created_at;
        package.supplements.push(supplement);
        accesses.push(access);
        task.updated_at = package.updated_at;
        let package = package.as_ref().clone();
        let leases = self.leases.read().expect("leases read lock poisoned");
        let snapshot =
            Self::snapshot_from_maps_with_dirty_roots(&tasks, &leases, vec![dirty_root_id]);
        self.fire_checkpoint(&snapshot)?;
        *self.tasks.write().expect("tasks write lock poisoned") = tasks;
        Ok(package)
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

    /// 执行资格同时属于本次租约和仍活跃的父链。父任务终态的级联清理可能尚未
    /// 运行，不能仅凭子任务仍持有 Active lease 继续执行。已转交子树只检查到
    /// 已验证的转交根；Session/Goal 与 canonical Turn 准入继续由运行时检查。
    pub fn execution_lease_is_current(&self, task_id: &TaskId, lease_id: &LeaseId) -> bool {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let leases = self.leases.read().expect("leases read lock poisoned");
        let Some(lease) = leases.get(lease_id).filter(|lease| {
            &lease.task_id == task_id && lease.lease_status == TaskLeaseState::Active
        }) else {
            return false;
        };
        let Some(task) = tasks.get(task_id).filter(|task| {
            task.status == TaskStatus::Running && task.root_task_id == lease.root_task_id
        }) else {
            return false;
        };
        Self::ancestor_chain_is_active(task, &tasks, false)
    }

    /// 获取租约历史，用于恢复审计和终态断言。
    pub fn get_lease(&self, lease_id: &LeaseId) -> Option<TaskLease> {
        self.leases
            .read()
            .expect("leases read lock poisoned")
            .get(lease_id)
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
    /// 返回 `Ok(None)` 表示扫描后任务已被领取、已结束或父链已不可调度。
    /// 这些竞争不应使其他可运行分支失败；归属错误或 checkpoint 错误仍返回错误。
    pub fn grant_lease_and_start_task(
        &self,
        task_id: &TaskId,
        root_task_id: &TaskId,
        worker_id: &WorkerId,
        role: &str,
        duration_ms: u64,
    ) -> DomainResult<Option<TaskLease>> {
        self.grant_lease_and_start_task_inner(
            task_id,
            root_task_id,
            None,
            worker_id,
            role,
            duration_ms,
        )
    }

    /// Runner 按执行拥有方领取，扫描与转交/取消竞争时在同一 mutation 锁下复核。
    /// Lease 的 root_task_id 始终保留原审计 root，不能改成当前执行拥有方。
    pub fn claim_task_for_execution_scope(
        &self,
        task_id: &TaskId,
        scope_root: &TaskId,
        worker_id: &WorkerId,
        role: &str,
        duration_ms: u64,
    ) -> DomainResult<Option<TaskLease>> {
        let task = self
            .get_task(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        self.grant_lease_and_start_task_inner(
            task_id,
            &task.root_task_id,
            Some(scope_root),
            worker_id,
            role,
            duration_ms,
        )
    }

    fn grant_lease_and_start_task_inner(
        &self,
        task_id: &TaskId,
        root_task_id: &TaskId,
        scope_root: Option<&TaskId>,
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
        if let Some(scope) = scope_root
            && Self::execution_scope_root_in_tasks(&tasks, task_id)? != *scope
        {
            return Ok(None);
        }
        let task = tasks
            .get(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        if task.status != TaskStatus::Pending {
            return Ok(None);
        }
        // Runner 扫描与这里的事务之间父任务可能已结束，必须在授予前重新检查。
        if !Self::ancestor_chain_allows_dispatch_inner(task, &tasks) {
            return Ok(None);
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
    /// 不存在、已经失效的租约或已关闭的父链返回 `Ok(false)`，用于丢弃迟到结果。合同校验
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
        // 执行循环的检查与结果提交不是同一事务；父任务可能在结果返回途中结束。
        // 原 Task/lease 留给既有级联终止入口结算，不能把迟到成功写成新的终态。
        if tasks
            .get(task_id)
            .is_none_or(|task| !Self::ancestor_chain_is_active(task, &tasks, false))
        {
            return Ok(false);
        }
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
        self.revoke_lease_and_set_task_terminal_inner(
            task_id,
            root_task_id,
            lease_id,
            status,
            output_refs,
            false,
            None,
        )
    }

    /// 用户取消语义：在 TaskStore mutation lock 内读取并撤销当前活跃租约，
    /// 避免 Runner 在调用方“先读 lease、再写终态”的窗口里刚好授予新 lease。
    pub fn revoke_active_lease_and_set_task_terminal(
        &self,
        task_id: &TaskId,
        root_task_id: &TaskId,
        status: TaskStatus,
        output_refs: Vec<String>,
    ) -> DomainResult<bool> {
        self.revoke_lease_and_set_task_terminal_inner(
            task_id,
            root_task_id,
            None,
            status,
            output_refs,
            true,
            None,
        )
    }

    /// 过期扫描与转交/续租竞争时，只有仍属于本执行范围且确实过期的原租约可失败。
    pub fn expire_lease_for_execution_scope(
        &self,
        scope_root: &TaskId,
        task_id: &TaskId,
        lease_id: &LeaseId,
    ) -> DomainResult<bool> {
        let task = self
            .get_task(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        self.revoke_lease_and_set_task_terminal_inner(
            task_id,
            &task.root_task_id,
            Some(lease_id),
            TaskStatus::Failed,
            vec![format!("任务 {task_id} 租约已过期")],
            false,
            Some(scope_root),
        )
    }

    /// 在一次 TaskStore mutation 内收口整棵任务树。子任务列表、租约和终态使用同一
    /// 快照，避免调用方先扫描再逐项取消期间新任务/租约穿过取消屏障。损坏的单个
    /// 节点会被报告，但不会阻止其余合法节点在同一 checkpoint 中结算。
    pub fn revoke_active_leases_and_set_subtree_terminal(
        &self,
        root_task_id: &TaskId,
        status: TaskStatus,
        output_refs: Vec<String>,
    ) -> DomainResult<bool> {
        self.set_subtree_terminal(
            root_task_id,
            status,
            output_refs,
            SubtreeTerminalSelection::Audit,
        )
    }

    /// 终止当前拥有方的执行范围，不跨越已提交的转交边界。会话关闭等整体销毁
    /// 仍使用完整子树入口，以免留下已转交执行。
    pub fn revoke_active_leases_and_set_execution_subtree_terminal(
        &self,
        root_task_id: &TaskId,
        status: TaskStatus,
        output_refs: Vec<String>,
    ) -> DomainResult<bool> {
        self.set_subtree_terminal(
            root_task_id,
            status,
            output_refs,
            SubtreeTerminalSelection::Execution,
        )
    }

    /// 父终态、执行拥有方和后代结算在同一 mutation 中检查；不使用 Runner 的旧扫描
    /// 结果决定取消范围，也不跨过已提交的转交边界。
    pub fn revoke_orphaned_execution_descendants(&self, scope_root: &TaskId) -> DomainResult<bool> {
        self.set_subtree_terminal(
            scope_root,
            TaskStatus::Killed,
            Vec::new(),
            SubtreeTerminalSelection::OrphanedExecution,
        )
    }

    /// Runner 报告不可运行后，候选可能已完成或转交。只有仍属于该拥有方的活跃
    /// 且尚未取得执行租约的候选才能触发失败；检查与终态提交不能分开，否则会
    /// 误停仍正常执行的主线。
    pub fn fail_unrunnable_execution_scope(
        &self,
        scope_root: &TaskId,
        candidates: &[TaskId],
        output_refs: Vec<String>,
    ) -> DomainResult<bool> {
        self.set_subtree_terminal(
            scope_root,
            TaskStatus::Failed,
            output_refs,
            SubtreeTerminalSelection::ExecutionIfActive(candidates),
        )
    }

    fn set_subtree_terminal(
        &self,
        root_task_id: &TaskId,
        status: TaskStatus,
        output_refs: Vec<String>,
        selection: SubtreeTerminalSelection<'_>,
    ) -> DomainResult<bool> {
        if !matches!(status, TaskStatus::Failed | TaskStatus::Killed) {
            return Err(DomainError::InvalidState {
                message: "子树批量结算只能写入 Failed 或 Killed 终态".into(),
            });
        }
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let audit_root_id = tasks
            .get(root_task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?
            .root_task_id
            .clone();
        let execution_only = !matches!(selection, SubtreeTerminalSelection::Audit);
        let mut task_ids = collect_subtree_ids_in_scope(&tasks, root_task_id, execution_only);
        let leases = self.leases.read().expect("leases read lock poisoned");
        match selection {
            SubtreeTerminalSelection::ExecutionIfActive(candidates) => {
                if !task_ids.iter().any(|id| {
                    candidates.contains(id)
                        && tasks
                            .get(id)
                            .is_some_and(|task| !is_terminal_status(task.status))
                        && !leases.values().any(|lease| {
                            lease.task_id == *id && lease.lease_status == TaskLeaseState::Active
                        })
                }) {
                    return Ok(false);
                }
            }
            SubtreeTerminalSelection::OrphanedExecution => {
                // 遍历保证父节点先于后代；一次扫描传播终态祖先，不反复扫描每棵子树。
                // 已转交节点及其后代不在当前 scope 中，不能被原 Runner 清理。
                let mut closed_branches = HashSet::new();
                task_ids.retain(|id| {
                    let task = &tasks[id];
                    let orphaned = task
                        .parent_task_id
                        .as_ref()
                        .is_some_and(|parent| closed_branches.contains(parent));
                    if orphaned || is_terminal_status(task.status) {
                        closed_branches.insert(id.clone());
                    }
                    orphaned
                });
            }
            SubtreeTerminalSelection::Audit | SubtreeTerminalSelection::Execution => {}
        }
        if task_ids.is_empty() {
            return Ok(false);
        }
        let candidate_tasks = tasks.clone();
        drop(tasks);
        let mut tasks = candidate_tasks;
        let candidate_leases = leases.clone();
        drop(leases);
        let mut leases = candidate_leases;
        let mut errors = Vec::new();
        let mut changes = Vec::new();
        for task_id in task_ids {
            match Self::prepare_task_terminal_in_maps(
                &mut tasks,
                &mut leases,
                &task_id,
                &audit_root_id,
                None,
                status,
                output_refs.clone(),
                true,
            ) {
                Ok(Some((old_status, task))) => changes.push((task_id, old_status, task)),
                Ok(None) => {}
                Err(error) => errors.push(format!("任务 {task_id}: {error}")),
            }
        }
        if !changes.is_empty() {
            let mission_index = self
                .mission_index
                .read()
                .expect("mission index lock poisoned")
                .clone();
            let snapshot =
                Self::snapshot_from_maps_with_dirty_roots(&tasks, &leases, vec![audit_root_id]);
            self.fire_checkpoint(&snapshot)?;
            self.commit_maps(tasks, leases, mission_index);
        }
        drop(_mutation_guard);
        for (task_id, old_status, task) in &changes {
            self.emit_status_change(task_id, *old_status, status, task.clone());
        }
        if errors.is_empty() {
            Ok(!changes.is_empty())
        } else {
            Err(DomainError::InvalidState {
                message: errors.join("；"),
            })
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn revoke_lease_and_set_task_terminal_inner(
        &self,
        task_id: &TaskId,
        root_task_id: &TaskId,
        lease_id: Option<&LeaseId>,
        status: TaskStatus,
        output_refs: Vec<String>,
        discover_active_lease: bool,
        expiry_scope: Option<&TaskId>,
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
        if let Some(scope) = expiry_scope
            && (Self::execution_scope_root_in_tasks(&tasks, task_id)? != *scope
                || lease_id.and_then(|id| leases.get(id)).is_none_or(|lease| {
                    lease.lease_status != TaskLeaseState::Active
                        || lease.expires_at >= UtcMillis::now()
                }))
        {
            return Ok(false);
        }
        let Some((old_status, cloned_task)) = Self::prepare_task_terminal_in_maps(
            &mut tasks,
            &mut leases,
            task_id,
            root_task_id,
            lease_id,
            status,
            output_refs,
            discover_active_lease,
        )?
        else {
            return Ok(false);
        };
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

    /// 只在候选 maps 中准备终态；单任务与整子树取消共用同一租约/状态校验。
    #[allow(clippy::too_many_arguments)]
    fn prepare_task_terminal_in_maps(
        tasks: &mut HashMap<TaskId, Task>,
        leases: &mut HashMap<LeaseId, TaskLease>,
        task_id: &TaskId,
        root_task_id: &TaskId,
        lease_id: Option<&LeaseId>,
        status: TaskStatus,
        output_refs: Vec<String>,
        discover_active_lease: bool,
    ) -> DomainResult<Option<(TaskStatus, Task)>> {
        Self::validate_task_root(tasks, task_id, root_task_id)?;
        let active_leases = leases
            .values()
            .filter(|lease| {
                lease.task_id == *task_id && lease.lease_status == TaskLeaseState::Active
            })
            .cloned()
            .collect::<Vec<_>>();
        for active_lease in &active_leases {
            Self::validate_lease_contract(tasks, active_lease)?;
            if active_lease.root_task_id != *root_task_id {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "活跃租约 {} 不属于 root task {}",
                        active_lease.lease_id, root_task_id
                    ),
                });
            }
        }
        if discover_active_lease && active_leases.len() > 1 {
            return Err(DomainError::InvalidState {
                message: format!("任务 {} 同时拥有多个活跃租约，不能安全取消", task_id),
            });
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
            return Ok(None);
        }
        let requested_lease = if discover_active_lease {
            active_leases.first().cloned()
        } else {
            match lease_id {
                Some(expected) => {
                    let Some(lease) = leases.get(expected).cloned() else {
                        return Ok(None);
                    };
                    if lease.task_id != *task_id {
                        return Err(DomainError::InvalidState {
                            message: format!("租约 {} 不属于任务 {}", expected, task_id),
                        });
                    }
                    Self::validate_lease_contract(tasks, &lease)?;
                    if lease.root_task_id != *root_task_id {
                        return Err(DomainError::InvalidState {
                            message: format!("租约 {} 不属于 root task {}", expected, root_task_id),
                        });
                    }
                    if lease.lease_status != TaskLeaseState::Active {
                        return Ok(None);
                    }
                    Some(lease)
                }
                None => None,
            }
        };
        let active_lease_id = active_leases.first().map(|lease| lease.lease_id.clone());
        if let Some(active_lease) = active_lease_id
            .as_ref()
            .and_then(|active_id| leases.get(active_id))
        {
            Self::validate_lease_contract(tasks, active_lease)?;
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
                return Ok(None);
            }
            (Some(_), None) => {
                return Ok(None);
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
        Ok(Some((old_status, cloned_task)))
    }

    /// 更新当前执行作用域内活跃租约的心跳时间。
    pub fn heartbeat_lease_for_execution_scope(
        &self,
        scope_root: &TaskId,
        task_id: &TaskId,
        lease_id: &LeaseId,
    ) -> bool {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        if !Self::execution_scope_root_in_tasks(&tasks, task_id)
            .is_ok_and(|root| root == *scope_root)
        {
            return false;
        }
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

    /// 扫描当前执行范围中已过期的活跃租约，不修改状态。
    pub fn collect_expired_leases(&self, scope_root: &TaskId) -> Vec<(TaskId, LeaseId)> {
        self.collect_leases_for_execution_scope(scope_root, true)
    }

    /// 获取当前执行范围中未过期的活跃租约。
    pub fn collect_active_leases(&self, scope_root: &TaskId) -> Vec<(TaskId, LeaseId)> {
        self.collect_leases_for_execution_scope(scope_root, false)
    }

    /// 仅为当前调度拥有方收集租约，原 root 不能再为已转交执行续租或报告过期。
    fn collect_leases_for_execution_scope(
        &self,
        scope_root: &TaskId,
        expired: bool,
    ) -> Vec<(TaskId, LeaseId)> {
        let now = UtcMillis::now();
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let ids = collect_subtree_ids_in_scope(&tasks, scope_root, true)
            .into_iter()
            .collect::<HashSet<_>>();
        let leases = self.leases.read().expect("leases read lock poisoned");
        leases
            .values()
            .filter(|lease| {
                lease.lease_status == TaskLeaseState::Active
                    && (lease.expires_at < now) == expired
                    && ids.contains(&lease.task_id)
            })
            .map(|lease| (lease.task_id.clone(), lease.lease_id.clone()))
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
    /// 仍然不能被视为真实运行态。TaskStore 自身的 Pending/Running 与租约事实
    /// 决定受影响的 execution scope，不依赖 sidecar 或旧 Worker 阶段快照。
    pub fn reconcile_volatile_runtime_after_restore(&self) -> DomainResult<(usize, usize)> {
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
        let interrupted_task_ids = tasks
            .values()
            .filter(|task| matches!(task.status, TaskStatus::Pending | TaskStatus::Running))
            .map(|task| task.task_id.clone())
            .collect::<Vec<_>>();
        if active_leases.is_empty() && interrupted_task_ids.is_empty() {
            return Ok((0, 0));
        }

        let mut affected_roots = HashSet::new();
        let mut affected_scopes = HashSet::new();
        for task_id in active_leases
            .iter()
            .map(|(task_id, _)| task_id)
            .chain(interrupted_task_ids.iter())
        {
            let scope = Self::execution_scope_root_in_tasks(&tasks, task_id)?;
            affected_roots.insert(tasks[task_id].root_task_id.clone());
            affected_scopes.insert(scope);
        }
        for (task_id, lease_id) in &active_leases {
            if let Some(lease) = leases.get_mut(lease_id)
                && lease.task_id == *task_id
                && lease.lease_status == TaskLeaseState::Active
            {
                lease.lease_status = TaskLeaseState::Revoked;
            }
        }
        let now = UtcMillis::now();
        let mut failed_count = 0usize;
        for scope in &affected_scopes {
            for task_id in collect_subtree_ids_in_scope(&tasks, scope, true) {
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

    // ------------------------------------------------------------------
    // Checkpoint / Restore
    // ------------------------------------------------------------------

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

    // ------------------------------------------------------------------
    // G8.5: Dynamic dependency management
    // ------------------------------------------------------------------

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
        previous_root: Option<&TaskProjectionManifestRoot>,
        content_hash: &str,
    ) -> io::Result<Option<(u64, String)>> {
        let (Some(source), Some(previous_root)) = (source, previous_root) else {
            return Ok(None);
        };
        if previous_root.content_hash != content_hash || fs::hard_link(source, destination).is_err()
        {
            return Ok(None);
        }
        Ok(Some((previous_root.generation, content_hash.to_string())))
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
            let content_hash_matches =
                Self::projection_content_hash(&projection)? == root.content_hash;
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
        Self::validate_agent_history(&checkpoint)?;
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

    fn deserialize_projection_strict(content: &[u8]) -> io::Result<TaskRootProjection> {
        serde_json::from_slice(content).map_err(|error| {
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
        // 全量 checkpoint 的公共表示始终按稳定 ID 排序，确保 checkpoint 比较和后续
        // 重启恢复比较的是同一份 canonical 表示，而不是 HashMap/分组遍历顺序。
        checkpoint
            .tasks
            .sort_by(|left, right| left.task_id.as_str().cmp(right.task_id.as_str()));
        checkpoint
            .leases
            .sort_by(|left, right| left.lease_id.as_str().cmp(right.lease_id.as_str()));
        Self::validate_checkpoint(&checkpoint)?;
        Self::validate_agent_history(&checkpoint)?;
        Ok(checkpoint)
    }

    /// Activation 可跨 root；只在完整恢复集合上校验来源，不能用单 root 增量快照裁决。
    fn validate_agent_history(checkpoint: &TaskStoreCheckpoint) -> io::Result<()> {
        let mut generations = HashSet::new();
        let mut transfer_operations = HashSet::new();
        let all = checkpoint
            .tasks
            .iter()
            .map(|task| (&task.task_id, task))
            .collect::<HashMap<_, _>>();
        for task in &checkpoint.tasks {
            Self::validate_project_directory_inheritance(
                task,
                task.parent_task_id
                    .as_ref()
                    .and_then(|id| all.get(id).copied()),
            )
            .map_err(|error| Self::invalid_projection(error.to_string()))?;
            if task.delegation_allowed() && task.agent_activation().is_none() {
                return Err(Self::invalid_projection(
                    "受委派代理缺少持久 activation 身份",
                ));
            }
            let Some(binding) = task.agent_activation() else {
                if task.agent_run_transfer().is_some() {
                    return Err(Self::invalid_projection("转交子树缺少 Agent activation"));
                }
                continue;
            };
            if let Some(transfer) = task.agent_run_transfer() {
                let parent = task
                    .parent_task_id
                    .as_ref()
                    .and_then(|id| all.get(id))
                    .ok_or_else(|| Self::invalid_projection("转交子树缺少原父任务"))?;
                Self::validate_agent_transfer_binding(parent, task, transfer)
                    .map_err(|error| Self::invalid_projection(error.to_string()))?;
                Self::validate_agent_transfer_subtree(checkpoint.tasks.iter(), task, transfer)
                    .map_err(|error| Self::invalid_projection(error.to_string()))?;
                if !transfer_operations.insert(transfer.operation_id.clone()) {
                    return Err(Self::invalid_projection("子树转交 operation_id 重复"));
                }
            }
            if task
                .parent_task_id
                .as_ref()
                .and_then(|id| all.get(id))
                .is_none_or(|parent| {
                    parent.root_task_id != task.root_task_id || parent.mission_id != task.mission_id
                })
            {
                return Err(Self::invalid_projection(
                    "Agent activation 父任务归属不匹配",
                ));
            }
            if let Some(parent) = task.parent_task_id.as_ref().and_then(|id| all.get(id)) {
                if task.delegation_allowed()
                    && (parent.parent_task_id.is_some()
                        || parent.executor_binding_target_role() != Some("coordinator")
                        || parent.root_task_id != parent.task_id
                        || task.policy_snapshot != parent.policy_snapshot
                        || task.workspace_scope != parent.workspace_scope
                        || task.write_scope != parent.write_scope)
                {
                    return Err(Self::invalid_projection("代理委派来源无效"));
                }
                if parent.parent_task_id.is_some() {
                    let root = all
                        .get(&task.root_task_id)
                        .ok_or_else(|| Self::invalid_projection("嵌套代理 root 缺失"))?;
                    if !parent.delegation_allowed()
                        || task.delegation_allowed()
                        || parent.parent_task_id.as_ref() != Some(&root.task_id)
                        || root.parent_task_id.is_some()
                        || root.root_task_id != root.task_id
                        || root.executor_binding_target_role() != Some("coordinator")
                        || task.policy_snapshot != parent.policy_snapshot
                        || task.workspace_scope != parent.workspace_scope
                        || task.write_scope != parent.write_scope
                    {
                        return Err(Self::invalid_projection("嵌套代理授权或深度无效"));
                    }
                }
            }
            if !generations.insert((binding.agent_id.clone(), binding.generation)) {
                return Err(Self::invalid_projection("Agent activation generation 重复"));
            }
            if let Some(previous_id) = &binding.previous_task_id {
                let previous = all
                    .get(previous_id)
                    .ok_or_else(|| Self::invalid_projection("Agent activation 来源缺失"))?;
                let source = previous
                    .agent_activation()
                    .ok_or_else(|| Self::invalid_projection("Agent activation 来源无身份"))?;
                if source.agent_id != binding.agent_id
                    || source.session_id != binding.session_id
                    || source.lifetime != binding.lifetime
                    || source.generation.checked_add(1) != Some(binding.generation)
                    || (task.delegation_allowed() && !previous.delegation_allowed())
                    || previous.mission_id != task.mission_id
                    || !Self::same_agent_owner(
                        previous
                            .parent_task_id
                            .as_ref()
                            .and_then(|id| all.get(id))
                            .copied(),
                        all.get(task.parent_task_id.as_ref().expect("validated parent"))
                            .expect("validated parent"),
                    )
                    || previous.status != TaskStatus::Completed
                    || matches!(source.lifetime, magi_core::AgentLifetime::Turn)
                    || previous.workspace_scope != task.workspace_scope
                    || previous.write_scope != task.write_scope
                    || previous.executor_binding_target_role()
                        != task.executor_binding_target_role()
                    || task
                        .executor_binding_capability_ids()
                        .iter()
                        .any(|capability| {
                            !previous
                                .executor_binding_capability_ids()
                                .contains(capability)
                        })
                {
                    return Err(Self::invalid_projection("Agent activation 来源不匹配"));
                }
            }
        }
        Ok(())
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
            if let Some(binding) = task.agent_activation() {
                binding
                    .validate(&task.task_id)
                    .map_err(|error| Self::invalid_projection(error.to_string()))?;
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
        fs::File::open(path)?.sync_all()
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

#[cfg(test)]
mod tests;
