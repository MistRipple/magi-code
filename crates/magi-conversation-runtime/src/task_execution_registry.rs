//! 任务系统 — 任务派发计划与注册中心。
//!
//! - [`TaskExecutionPlan`]：dispatch_submission 接受后挂在 task_execution_registry
//!   上的派发载体；当前派发链路只保留 Dispatch 一支。
//! - [`TaskExecutionRegistry`]：线程安全的 `TaskId → TaskExecutionPlan` 索引，
//!   `LlmTaskDispatcher` 与 `Runner` 通过它取出已接受派发计划。
//!
//! magi-api 不再实现这两个类型，改为 `pub use` 重导出；本模块是任务派发链路的
//! 唯一所有者。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use magi_bridge_client::ModelBridgeClient;
use magi_core::{
    ExecutionOwnership, PlanItemId, SessionId, Task, TaskExecutionTarget, TaskId, ThreadId,
    UtcMillis, WorkerId, WorkspaceId,
};
use magi_orchestrator::{ExecutionWritebackPlans, task_store::TaskStore};
use magi_session_store::{ActiveExecutionBranch, ExecutionThread, SessionPlan, SessionStore};
use magi_settings_store::SettingsStore;
use magi_spawn_graph::SpawnGraph;

use crate::execution_admission::ExecutionAdmissionController;
use crate::{session_images::SessionTurnImage, session_thread};

pub const DEFAULT_MAX_ACTIVE_AGENTS_PER_ROLE: usize = 5;

/// `agent_spawn` 创建前预检需要的进程级运行时事实。
///
/// 该配置只保存可共享的句柄，不在预检阶段执行任何持久化或 Git mutation。测试和
/// 轻量运行时可以不注入它，此时预检仍会执行参数、角色、作用域和准入检查；daemon
/// 生产装配时必须注入真实 settings、模型客户端和 Git/workspace 事实源。
#[derive(Clone, Default)]
pub struct AgentSpawnPreflightRuntime {
    pub settings_store: Option<Arc<SettingsStore>>,
    pub default_model_client: Option<Arc<dyn ModelBridgeClient>>,
    pub workspace_registry: Option<Arc<magi_workspace::WorkspaceStore>>,
    pub session_code_contexts: Option<magi_git::SessionCodeContextRegistry>,
    pub git_service_configured: bool,
}

#[derive(Clone, Debug)]
pub enum TaskExecutionPlan {
    Dispatch {
        target: TaskExecutionTarget,
        worker_id: WorkerId,
        /// task 绑定的 thread，由 `session_thread::ensure_thread_for_role` 为当前 task
        /// 独立创建，是 task 详情归属与当前 task 恢复记录的路由键。
        thread_id: ThreadId,
        is_primary: bool,
        session_id: SessionId,
        /// 该任务所属的会话 Turn。所有异步写回都必须使用它做来源校验，不能
        /// 在写回时重新读取 session 当前指针。
        turn_id: String,
        workspace_id: Option<WorkspaceId>,
        /// 此任务实际执行的根目录。个人会话使用 Magi 管理的目录，项目会话使用项目根目录。
        /// 它与 `workspace_id` 分离，避免把“是否属于项目”和“工具 cwd”混为同一概念。
        execution_root: Option<PathBuf>,
        ownership: ExecutionOwnership,
        writebacks: ExecutionWritebackPlans,
        use_tools: bool,
        skill_name: Option<String>,
        images: Vec<SessionTurnImage>,
        execution_settings_snapshot: Option<Arc<SettingsStore>>,
    },
}

impl TaskExecutionPlan {
    pub fn execution_settings_snapshot(&self) -> Option<Arc<SettingsStore>> {
        match self {
            Self::Dispatch {
                execution_settings_snapshot,
                ..
            } => execution_settings_snapshot.clone(),
        }
    }
}

pub struct SpawnedChildExecutionRequest<'a> {
    pub task_store: &'a TaskStore,
    pub spawn_graph: &'a Mutex<SpawnGraph>,
    pub session_store: &'a SessionStore,
    pub child_task: &'a Task,
    pub session_id: &'a SessionId,
    pub workspace_id: &'a Option<WorkspaceId>,
    pub role: &'a str,
    /// 角色定义中的并发上限。`None` 表示该角色不设置角色级上限；全局和会话级
    /// 执行准入仍然继续生效。调用方必须从同一份 AgentRoleRegistry 读取该值。
    pub role_parallelism_limit: Option<u32>,
    /// 可选的当前会话计划。传入时，子任务与 `plan_item_id` 的绑定和回滚都在
    /// 同一个注册事务内完成；预检阶段已经确认该 item 存在且处于可执行状态。
    pub plan_store: Option<&'a magi_plan::PlanStore>,
    pub plan_item_id: Option<PlanItemId>,
    /// 可选的已通过预检的执行目录。为空时继承父任务计划中的目录。
    pub execution_root: Option<PathBuf>,
    pub now: UtcMillis,
}

#[derive(Debug)]
pub struct SpawnedChildExecution {
    pub worker_id: WorkerId,
    pub thread_id: ThreadId,
    pub execution_chain_ref: String,
    /// 计划绑定在同一注册事务内完成后的新快照。调用方只负责发布事件，不再重复绑定。
    pub plan: Option<SessionPlan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpawnedChildExecutionError {
    RoleCapacityExceeded {
        role: String,
        active: usize,
        limit: usize,
    },
    InvalidState(String),
}

impl std::fmt::Display for SpawnedChildExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RoleCapacityExceeded {
                role,
                active,
                limit,
            } => write!(
                f,
                "角色 {role} 已达到代理实例上限：最多 {limit} 个活跃实例，当前 {active} 个"
            ),
            Self::InvalidState(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for SpawnedChildExecutionError {}

#[derive(Clone)]
pub struct TaskExecutionRegistry {
    plans: Arc<RwLock<HashMap<TaskId, TaskExecutionPlan>>>,
    execution_admission: Arc<ExecutionAdmissionController>,
    agent_spawn_preflight: Arc<RwLock<AgentSpawnPreflightRuntime>>,
    /// 序列化跨 `TaskStore`、`SpawnGraph`、`SessionStore` 和执行计划的子任务注册。
    ///
    /// 预检发生在该锁之外，因此注册入口必须再次检查所有依赖唯一性的事实；
    /// 这样并发的两个 `agent_spawn` 即使同时通过预检，也不会产生重复的
    /// canonical task name 或交错的 active execution chain。
    registration_lock: Arc<Mutex<()>>,
}

impl Default for TaskExecutionRegistry {
    fn default() -> Self {
        Self {
            plans: Arc::new(RwLock::new(HashMap::new())),
            execution_admission: Arc::new(ExecutionAdmissionController::default()),
            agent_spawn_preflight: Arc::new(RwLock::new(AgentSpawnPreflightRuntime::default())),
            registration_lock: Arc::new(Mutex::new(())),
        }
    }
}

impl TaskExecutionRegistry {
    /// 返回与 Dispatcher/Runner 共用的执行准入控制器。
    pub fn execution_admission(&self) -> Arc<ExecutionAdmissionController> {
        Arc::clone(&self.execution_admission)
    }

    pub fn with_execution_admission(
        mut self,
        execution_admission: Arc<ExecutionAdmissionController>,
    ) -> Self {
        self.execution_admission = execution_admission;
        self
    }

    /// 注入 `agent_spawn` 唯一预检使用的运行时事实源。
    pub fn with_agent_spawn_preflight_runtime(self, runtime: AgentSpawnPreflightRuntime) -> Self {
        *self
            .agent_spawn_preflight
            .write()
            .expect("agent spawn preflight runtime lock poisoned") = runtime;
        self
    }

    pub fn agent_spawn_preflight_runtime(&self) -> AgentSpawnPreflightRuntime {
        self.agent_spawn_preflight
            .read()
            .expect("agent spawn preflight runtime lock poisoned")
            .clone()
    }

    pub fn admission_preview(&self, session_id: Option<&SessionId>, role: &str) -> Option<String> {
        self.execution_admission.preview(session_id, role)
    }

    pub fn insert(
        &self,
        task_id: TaskId,
        plan: TaskExecutionPlan,
    ) -> Result<(), Box<TaskExecutionPlan>> {
        let mut plans = self
            .plans
            .write()
            .expect("task execution registry write lock poisoned");
        if plans.contains_key(&task_id) {
            return Err(Box::new(plan));
        }
        plans.insert(task_id, plan);
        Ok(())
    }

    pub fn remove(&self, task_id: &TaskId) -> Option<TaskExecutionPlan> {
        self.plans
            .write()
            .expect("task execution registry write lock poisoned")
            .remove(task_id)
    }

    /// 仅删除仍属于指定 Turn 的执行计划。
    ///
    /// 一个任务在恢复时可能先结束旧租约、再注册新 Turn。旧 dispatcher 的 Drop
    /// 不能无条件删除同一 task id 的新计划，否则会把恢复后的执行链再次变成无计划
    /// 执行。Turn 是计划的生命周期代际，因此这里把代际校验和删除放在同一把写锁内。
    pub fn remove_if_turn_matches(
        &self,
        task_id: &TaskId,
        expected_turn_id: &str,
    ) -> Option<TaskExecutionPlan> {
        let mut plans = self
            .plans
            .write()
            .expect("task execution registry write lock poisoned");
        let matches = plans.get(task_id).is_some_and(|plan| match plan {
            TaskExecutionPlan::Dispatch { turn_id, .. } => turn_id == expected_turn_id,
        });
        matches.then(|| plans.remove(task_id)).flatten()
    }

    pub fn get(&self, task_id: &TaskId) -> Option<TaskExecutionPlan> {
        self.plans
            .read()
            .expect("task execution registry read lock poisoned")
            .get(task_id)
            .cloned()
    }

    pub fn turn_id(&self, task_id: &TaskId) -> Option<String> {
        self.plans
            .read()
            .expect("task execution registry read lock poisoned")
            .get(task_id)
            .map(|plan| match plan {
                TaskExecutionPlan::Dispatch { turn_id, .. } => turn_id.clone(),
            })
    }

    /// 删除一个 session 拥有的全部执行计划，并返回被删除的 TaskId，供上层同步
    /// 清理 TaskStore 与 SpawnGraph。
    pub fn remove_session(&self, session_id: &SessionId) -> Vec<TaskId> {
        let mut plans = self
            .plans
            .write()
            .expect("task execution registry write lock poisoned");
        let removed = plans
            .iter()
            .filter_map(|(task_id, plan)| match plan {
                TaskExecutionPlan::Dispatch {
                    session_id: candidate,
                    ..
                } if candidate == session_id => Some(task_id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        for task_id in &removed {
            plans.remove(task_id);
        }
        removed
    }

    pub fn update_active_skill(
        &self,
        task_id: &TaskId,
        session_store: &SessionStore,
        session_id: &SessionId,
        skill_id: String,
    ) -> Result<(), String> {
        if self.get(task_id).is_none() {
            return Err(format!("任务 {task_id} 缺少执行计划，无法记录 Skill 激活"));
        }
        let mut chain = session_store
            .active_execution_chain(session_id)
            .ok_or_else(|| format!("会话 {session_id} 缺少活跃执行链"))?;
        let branch = chain
            .branches
            .iter_mut()
            .find(|branch| &branch.task_id == task_id)
            .ok_or_else(|| format!("任务 {task_id} 不在当前执行链分支中"))?;
        branch.skill_name = Some(skill_id.clone());
        if &chain.root_task_id == task_id {
            chain.dispatch_context.skill_name = Some(skill_id.clone());
        }
        chain.normalize();
        session_store
            .upsert_active_execution_chain(session_id.clone(), chain)
            .map_err(|error| error.to_string())?;

        let mut plans = self
            .plans
            .write()
            .expect("task execution registry write lock poisoned");
        let plan = plans
            .get_mut(task_id)
            .ok_or_else(|| format!("任务 {task_id} 执行计划在更新期间消失"))?;
        match plan {
            TaskExecutionPlan::Dispatch { skill_name, .. } => {
                *skill_name = Some(skill_id);
            }
        }
        Ok(())
    }

    pub fn register_spawned_local_agent_child(
        &self,
        request: SpawnedChildExecutionRequest<'_>,
    ) -> Result<SpawnedChildExecution, SpawnedChildExecutionError> {
        let SpawnedChildExecutionRequest {
            task_store,
            spawn_graph,
            session_store,
            child_task,
            session_id,
            workspace_id,
            role,
            role_parallelism_limit,
            plan_store,
            plan_item_id,
            execution_root,
            now,
        } = request;
        let _registration_guard = self.registration_lock.lock().map_err(|error| {
            SpawnedChildExecutionError::InvalidState(format!("agent_spawn 注册锁不可用: {error}"))
        })?;
        let mut chain = session_store
            .active_execution_chain(session_id)
            .ok_or_else(|| {
                SpawnedChildExecutionError::InvalidState(
                    "agent_spawn 需要当前会话存在活跃执行链".to_string(),
                )
            })?;
        let original_chain = chain.clone();
        if chain.mission_id != child_task.mission_id
            || chain.root_task_id != child_task.root_task_id
        {
            return Err(SpawnedChildExecutionError::InvalidState(format!(
                "agent_spawn 子任务不属于当前执行链: mission/root {}:{} != {}:{}",
                child_task.mission_id,
                child_task.root_task_id,
                chain.mission_id,
                chain.root_task_id
            )));
        }
        let parent_task_id = child_task.parent_task_id.clone().ok_or_else(|| {
            SpawnedChildExecutionError::InvalidState(format!(
                "agent_spawn 子任务 {} 缺少 parent_task_id",
                child_task.task_id
            ))
        })?;
        if task_store.get_task(&child_task.task_id).is_some() {
            return Err(SpawnedChildExecutionError::InvalidState(format!(
                "agent_spawn 子任务 {} 已存在于 TaskStore，拒绝重复注册",
                child_task.task_id
            )));
        }
        if self.get(&child_task.task_id).is_some() {
            return Err(SpawnedChildExecutionError::InvalidState(format!(
                "agent_spawn 子任务 {} 已存在于执行注册表，拒绝重复注册",
                child_task.task_id
            )));
        }

        // `preflight_agent_spawn` 已经检查过一次 task_name，但它与原子注册之间
        // 可能存在并发窗口。这里用注册锁下的最新 TaskStore 快照再做一次唯一性
        // 校验，确保同一父任务不会出现两个相同的 canonical task name。
        if let Some(canonical_task_name) = child_task.canonical_task_name()
            && task_store
                .get_children(&parent_task_id)
                .iter()
                .any(|child| child.canonical_task_name() == Some(canonical_task_name))
        {
            return Err(SpawnedChildExecutionError::InvalidState(format!(
                "同一父任务下 canonical task name 已存在: {canonical_task_name}"
            )));
        }

        if let Some(limit) = role_parallelism_limit {
            let limit = limit as usize;
            let active_role_agent_count = active_execution_agent_count_for_role(
                task_store,
                session_store,
                session_id,
                &chain,
                role,
            );
            if active_role_agent_count >= limit {
                return Err(SpawnedChildExecutionError::RoleCapacityExceeded {
                    role: role.to_string(),
                    active: active_role_agent_count,
                    limit,
                });
            }
        }

        let worker_id = WorkerId::new(format!("worker-spawn-{}", child_task.task_id.as_str()));
        let parent_plan = self.get(&parent_task_id);
        let inherited_turn_id = parent_plan
            .as_ref()
            .map(|plan| match plan {
                TaskExecutionPlan::Dispatch { turn_id, .. } => turn_id.clone(),
            })
            .or_else(|| chain.current_turn.as_ref().map(|turn| turn.turn_id.clone()))
            .ok_or_else(|| {
                SpawnedChildExecutionError::InvalidState(
                    "agent_spawn 父任务缺少所属 Turn，无法注册子任务".to_string(),
                )
            })?;
        // Thread ID 只在这里生成一次，并把同一个对象同时写入 active branch、
        // execution plan 和 SessionStore，避免“计划指向 A、注册表指向 B”的分裂。
        let thread = session_thread::build_thread_for_role(
            session_id,
            &chain.mission_id,
            role,
            &worker_id,
            &child_task.task_id,
            now,
        );
        let thread_id = thread.thread_id.clone();
        let inherited_skill_name = parent_plan.as_ref().and_then(|plan| match plan {
            TaskExecutionPlan::Dispatch { skill_name, .. } => skill_name.clone(),
        });
        let inherited_execution_root = execution_root.or_else(|| {
            parent_plan.as_ref().and_then(|plan| match plan {
                TaskExecutionPlan::Dispatch { execution_root, .. } => execution_root.clone(),
            })
        });
        let branch = ActiveExecutionBranch {
            task_id: child_task.task_id.clone(),
            worker_id: worker_id.clone(),
            stage: "execute".to_string(),
            lease_id: None,
            execution_intent_ref: None,
            binding_lifecycle: None,
            checkpoint_stage: Some("execute".to_string()),
            next_step_index: Some(0),
            checkpoint_at: Some(now),
            resume_mode: Some("stage-restart".to_string()),
            resume_token: None,
            use_tools: true,
            skill_name: inherited_skill_name.clone(),
            is_primary: false,
            thread_id: thread_id.clone(),
        };
        chain
            .branches
            .retain(|entry| entry.task_id != child_task.task_id);
        chain.branches.push(branch);
        chain.active_branch_task_ids = chain
            .branches
            .iter()
            .map(|entry| entry.task_id.clone())
            .collect();
        chain.active_worker_bindings = chain
            .branches
            .iter()
            .map(|entry| entry.worker_id.clone())
            .collect();
        if let Some(turn) = chain.current_turn.as_mut() {
            turn.normalize();
        }
        let execution_chain_ref = chain.execution_chain_ref.clone();
        chain.normalize();

        let execution_settings_snapshot = parent_plan
            .as_ref()
            .and_then(TaskExecutionPlan::execution_settings_snapshot);

        let plan = TaskExecutionPlan::Dispatch {
            target: TaskExecutionTarget {
                mission_id: child_task.mission_id.clone(),
                root_task_id: child_task.root_task_id.clone(),
                task_id: child_task.task_id.clone(),
                requested_worker_id: Some(worker_id.clone()),
                recovery_id: None,
                execution_chain_ref: Some(execution_chain_ref.clone()),
            },
            worker_id: worker_id.clone(),
            thread_id: thread_id.clone(),
            is_primary: false,
            session_id: session_id.clone(),
            turn_id: inherited_turn_id,
            workspace_id: workspace_id.clone(),
            execution_root: inherited_execution_root,
            ownership: ExecutionOwnership {
                session_id: Some(session_id.clone()),
                workspace_id: workspace_id.clone(),
                mission_id: Some(child_task.mission_id.clone()),
                task_id: Some(child_task.task_id.clone()),
                worker_id: Some(worker_id.clone()),
                execution_chain_ref: Some(execution_chain_ref.clone()),
            },
            writebacks: ExecutionWritebackPlans::default(),
            use_tools: true,
            skill_name: inherited_skill_name,
            images: Vec::new(),
            execution_settings_snapshot,
        };

        if let Err(error) = task_store.insert_task(child_task.clone()) {
            return Err(SpawnedChildExecutionError::InvalidState(error.to_string()));
        }
        let task_inserted = true;

        let graph_added = match spawn_graph.lock().map_err(|err| {
            SpawnedChildExecutionError::InvalidState(format!("SpawnGraph mutex poisoned: {err}"))
        }) {
            Ok(mut graph) => graph
                .add_edge(
                    parent_task_id.clone(),
                    child_task.task_id.clone(),
                    child_task.kind,
                    std::time::SystemTime::now(),
                )
                .map(|_| true)
                .map_err(|error| {
                    SpawnedChildExecutionError::InvalidState(format!(
                        "agent_spawn 注册 SpawnGraph 边失败: {error}"
                    ))
                }),
            Err(error) => Err(error),
        };
        if let Err(error) = graph_added {
            return Err(rollback_spawned_local_agent_child(
                self,
                task_store,
                spawn_graph,
                session_store,
                session_id,
                &child_task.task_id,
                &original_chain,
                task_inserted,
                false,
                false,
                false,
                None,
                None,
                error,
            ));
        }

        if let Err(error) =
            session_store.upsert_active_execution_chain(session_id.clone(), chain.clone())
        {
            return Err(rollback_spawned_local_agent_child(
                self,
                task_store,
                spawn_graph,
                session_store,
                session_id,
                &child_task.task_id,
                &original_chain,
                task_inserted,
                true,
                false,
                false,
                None,
                None,
                SpawnedChildExecutionError::InvalidState(error.to_string()),
            ));
        }

        if let Err(_plan) = self.insert(child_task.task_id.clone(), plan) {
            return Err(rollback_spawned_local_agent_child(
                self,
                task_store,
                spawn_graph,
                session_store,
                session_id,
                &child_task.task_id,
                &original_chain,
                task_inserted,
                true,
                true,
                false,
                None,
                None,
                SpawnedChildExecutionError::InvalidState(format!(
                    "agent_spawn 子任务 {} 已存在于执行注册表，拒绝重复注册",
                    child_task.task_id
                )),
            ));
        }

        if let Err(error) = session_store.register_thread(thread.clone()) {
            return Err(rollback_spawned_local_agent_child(
                self,
                task_store,
                spawn_graph,
                session_store,
                session_id,
                &child_task.task_id,
                &original_chain,
                task_inserted,
                true,
                true,
                true,
                None,
                None,
                SpawnedChildExecutionError::InvalidState(format!(
                    "agent_spawn 注册执行 thread 失败: {error}"
                )),
            ));
        }

        let plan_binding = match (plan_store, plan_item_id) {
            (Some(plan_store), Some(plan_item_id)) => match plan_store
                .bind_task_for_materialization(child_task.task_id.clone(), plan_item_id)
            {
                Ok(binding) => binding,
                Err(error) => {
                    return Err(rollback_spawned_local_agent_child(
                        self,
                        task_store,
                        spawn_graph,
                        session_store,
                        session_id,
                        &child_task.task_id,
                        &original_chain,
                        task_inserted,
                        true,
                        true,
                        true,
                        Some(&thread),
                        None,
                        SpawnedChildExecutionError::InvalidState(format!(
                            "agent_spawn 计划绑定失败: {error}"
                        )),
                    ));
                }
            },
            (None, None) | (Some(_), None) => None,
            (None, Some(_)) => {
                return Err(rollback_spawned_local_agent_child(
                    self,
                    task_store,
                    spawn_graph,
                    session_store,
                    session_id,
                    &child_task.task_id,
                    &original_chain,
                    task_inserted,
                    true,
                    true,
                    true,
                    Some(&thread),
                    None,
                    SpawnedChildExecutionError::InvalidState(
                        "agent_spawn 计划绑定上下文不完整".to_string(),
                    ),
                ));
            }
        };

        Ok(SpawnedChildExecution {
            worker_id,
            thread_id,
            execution_chain_ref,
            plan: plan_binding.map(|(_, updated)| updated),
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn rollback_spawned_local_agent_child(
    registry: &TaskExecutionRegistry,
    task_store: &TaskStore,
    spawn_graph: &Mutex<SpawnGraph>,
    session_store: &SessionStore,
    session_id: &SessionId,
    child_task_id: &TaskId,
    original_chain: &magi_session_store::ActiveExecutionChain,
    task_inserted: bool,
    graph_added: bool,
    session_chain_updated: bool,
    registry_inserted: bool,
    registered_thread: Option<&ExecutionThread>,
    plan_rollback: Option<(&SessionPlan, u64)>,
    primary_error: SpawnedChildExecutionError,
) -> SpawnedChildExecutionError {
    let mut rollback_errors = Vec::new();
    if let Some(thread) = registered_thread
        && let Err(error) =
            session_store.remove_thread_if_current(session_id, &thread.thread_id, thread)
    {
        rollback_errors.push(format!("执行 thread 回滚失败: {error}"));
    }
    if let Some((original_plan, expected_revision)) = plan_rollback
        && let Err(error) = session_store.restore_plan_if_current(
            session_id,
            expected_revision,
            Some(original_plan.clone()),
        )
    {
        rollback_errors.push(format!("计划绑定回滚失败: {error}"));
    }
    if registry_inserted && registry.remove(child_task_id).is_none() {
        rollback_errors.push("执行注册表回滚时未找到子任务".to_string());
    }
    if graph_added {
        let mut task_ids = HashSet::new();
        task_ids.insert(child_task_id.clone());
        match spawn_graph.lock() {
            Ok(mut graph) => {
                graph.remove_tasks(&task_ids);
            }
            Err(error) => rollback_errors.push(format!("SpawnGraph 回滚失败: {error}")),
        }
    }
    if session_chain_updated
        && let Err(error) =
            session_store.upsert_active_execution_chain(session_id.clone(), original_chain.clone())
    {
        rollback_errors.push(format!("session active chain 回滚失败: {error}"));
    }
    if task_inserted {
        match task_store.remove_task(child_task_id) {
            Ok(Some(_)) => {}
            Ok(None) => rollback_errors.push("TaskStore 回滚时未找到子任务".to_string()),
            Err(error) => rollback_errors.push(format!("TaskStore 回滚失败: {error}")),
        }
    }

    if rollback_errors.is_empty() {
        primary_error
    } else {
        SpawnedChildExecutionError::InvalidState(format!(
            "{primary_error}；回滚失败: {}",
            rollback_errors.join("；")
        ))
    }
}

fn active_execution_agent_count_for_role(
    task_store: &TaskStore,
    session_store: &SessionStore,
    session_id: &SessionId,
    chain: &magi_session_store::ActiveExecutionChain,
    role: &str,
) -> usize {
    let threads = session_store.thread_registry_snapshot(session_id);
    chain
        .branches
        .iter()
        .filter(|branch| {
            let is_active = task_store
                .get_task(&branch.task_id)
                .map(|task| {
                    matches!(
                        task.status,
                        magi_core::TaskStatus::Pending | magi_core::TaskStatus::Running
                    )
                })
                .unwrap_or(true);
            is_active
                && threads
                    .iter()
                    .any(|thread| thread.thread_id == branch.thread_id && thread.role_id == role)
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{MissionId, Task, TaskKind, TaskRuntimePayload, TaskStatus, UtcMillis};
    use magi_session_store::{
        ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
        ActiveExecutionTurn, SessionStore,
    };

    fn test_task(task_id: &str, root_task_id: &str, mission_id: &MissionId) -> Task {
        let now = UtcMillis(1_000);
        Task {
            task_id: TaskId::new(task_id),
            mission_id: mission_id.clone(),
            root_task_id: TaskId::new(root_task_id),
            parent_task_id: Some(TaskId::new(root_task_id)),
            kind: TaskKind::LocalAgent,
            title: format!("task {task_id}"),
            goal: format!("run task {task_id}"),
            status: TaskStatus::Pending,
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

    #[test]
    fn spawned_local_agent_child_registration_is_atomic_runtime_source() {
        use magi_settings_store::SettingsStore;

        let task_store = TaskStore::new();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_store = SessionStore::new();
        let registry = TaskExecutionRegistry::default();
        let session_id = SessionId::new("session-atomic-spawn");
        let workspace_id = Some(WorkspaceId::new("workspace-atomic-spawn"));
        let mission_id = MissionId::new("mission-atomic-spawn");
        let root_task_id = TaskId::new("task-root");
        let parent_worker_id = WorkerId::new("worker-parent");
        let now = UtcMillis(10_000);
        let _ = session_store.ensure_session_mission(&session_id, now, || mission_id.clone());
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-atomic-spawn".to_string(),
                    workspace_id: workspace_id.clone(),
                    active_branch_task_ids: vec![root_task_id.clone()],
                    active_worker_bindings: vec![parent_worker_id.clone()],
                    branches: vec![ActiveExecutionBranch {
                        task_id: root_task_id.clone(),
                        worker_id: parent_worker_id,
                        stage: "execute".to_string(),
                        lease_id: None,
                        execution_intent_ref: None,
                        binding_lifecycle: None,
                        checkpoint_stage: Some("execute".to_string()),
                        next_step_index: Some(0),
                        checkpoint_at: Some(now),
                        resume_mode: Some("stage-restart".to_string()),
                        resume_token: None,
                        use_tools: true,
                        skill_name: None,
                        is_primary: true,
                        thread_id: ThreadId::new("thread-atomic-spawn-parent"),
                    }],
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-atomic-spawn".to_string(),
                        trimmed_text: Some("spawn child".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-atomic-spawn".to_string(),
                        turn_seq: 1,
                        accepted_at: now,
                        completed_at: None,
                        status: "running".to_string(),
                        user_message: Some("spawn child".to_string()),
                        items: Vec::new(),
                    }),
                },
            )
            .expect("active chain should be accepted");

        let parent_settings = SettingsStore::new();
        let parent_settings_snapshot = Arc::new(parent_settings.execution_snapshot());
        registry
            .insert(
                root_task_id.clone(),
                TaskExecutionPlan::Dispatch {
                    target: magi_core::TaskExecutionTarget {
                        mission_id: mission_id.clone(),
                        root_task_id: root_task_id.clone(),
                        task_id: root_task_id.clone(),
                        requested_worker_id: None,
                        recovery_id: None,
                        execution_chain_ref: Some("chain-atomic-spawn".to_string()),
                    },
                    worker_id: WorkerId::new("worker-parent"),
                    thread_id: ThreadId::new("thread-atomic-spawn-parent"),
                    is_primary: true,
                    session_id: session_id.clone(),
                    turn_id: "turn-atomic-spawn".to_string(),
                    workspace_id: workspace_id.clone(),
                    execution_root: None,
                    ownership: ExecutionOwnership::default(),
                    writebacks: ExecutionWritebackPlans::default(),
                    use_tools: true,
                    skill_name: None,
                    images: Vec::new(),
                    execution_settings_snapshot: Some(parent_settings_snapshot.clone()),
                },
            )
            .expect("父任务执行计划应严格插入注册表");
        registry
            .update_active_skill(
                &root_task_id,
                &session_store,
                &session_id,
                "code-review".to_string(),
            )
            .expect("dynamic parent skill activation should update runtime ownership");

        let child = test_task("task-child", root_task_id.as_str(), &mission_id);
        let registered = registry
            .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                task_store: &task_store,
                spawn_graph: &spawn_graph,
                session_store: &session_store,
                child_task: &child,
                session_id: &session_id,
                workspace_id: &workspace_id,
                role: "executor",
                role_parallelism_limit: None,
                plan_store: None,
                plan_item_id: None,
                execution_root: None,
                now,
            })
            .expect("spawned child runtime registration should succeed");

        assert_eq!(registered.execution_chain_ref, "chain-atomic-spawn");
        assert!(
            task_store.get_task(&child.task_id).is_some(),
            "child task should be inserted by the atomic runtime registration entry"
        );
        assert_eq!(
            spawn_graph
                .lock()
                .expect("spawn graph lock should be available")
                .parent_of(&child.task_id),
            Some(&root_task_id)
        );

        let plan = registry
            .get(&child.task_id)
            .expect("child execution plan should be registered atomically");
        match plan {
            TaskExecutionPlan::Dispatch {
                thread_id,
                session_id: plan_session_id,
                workspace_id: plan_workspace_id,
                is_primary,
                use_tools,
                skill_name,
                execution_settings_snapshot,
                ..
            } => {
                assert_eq!(thread_id, registered.thread_id);
                assert_eq!(plan_session_id, session_id);
                assert_eq!(plan_workspace_id, workspace_id);
                assert!(!is_primary);
                assert!(use_tools);
                assert_eq!(skill_name.as_deref(), Some("code-review"));
                assert!(
                    execution_settings_snapshot
                        .as_ref()
                        .is_some_and(|snapshot| Arc::ptr_eq(snapshot, &parent_settings_snapshot)),
                    "agent_spawn 子任务必须继承父任务执行快照"
                );
            }
        }

        let chain = session_store
            .active_execution_chain(&session_id)
            .expect("active execution chain should remain available");
        let child_branch = chain
            .branches
            .iter()
            .find(|branch| {
                branch.task_id == child.task_id && branch.worker_id == registered.worker_id
            })
            .expect("child branch should exist");
        assert_eq!(child_branch.thread_id, registered.thread_id);
        assert_eq!(child_branch.skill_name.as_deref(), Some("code-review"));
        assert!(chain.active_branch_task_ids.contains(&child.task_id));
        assert!(chain.active_worker_bindings.contains(&registered.worker_id));

        let task_state_before = format!("{:?}", task_store.get_task(&child.task_id));
        let plan_state_before = format!("{:?}", registry.get(&child.task_id));
        let chain_state_before = format!("{:?}", session_store.active_execution_chain(&session_id));
        let threads_state_before =
            format!("{:?}", session_store.thread_registry_snapshot(&session_id));
        let graph_state_before = format!(
            "{:?}",
            spawn_graph
                .lock()
                .expect("spawn graph lock should be available")
                .all_edges()
        );

        let duplicate_plan = registry
            .get(&child.task_id)
            .expect("重复插入测试需要保留原执行计划");
        assert!(
            registry
                .insert(child.task_id.clone(), duplicate_plan)
                .is_err(),
            "执行注册表不得覆盖已有执行计划"
        );
        let duplicate_error = registry
            .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                task_store: &task_store,
                spawn_graph: &spawn_graph,
                session_store: &session_store,
                child_task: &child,
                session_id: &session_id,
                workspace_id: &workspace_id,
                role: "executor",
                role_parallelism_limit: None,
                plan_store: None,
                plan_item_id: None,
                execution_root: None,
                now,
            })
            .expect_err("重复注册子任务必须被拒绝");
        assert!(matches!(
            duplicate_error,
            SpawnedChildExecutionError::InvalidState(message)
                if message.contains("已存在")
        ));
        assert_eq!(
            format!("{:?}", task_store.get_task(&child.task_id)),
            task_state_before,
            "重复注册不得改变 TaskStore"
        );
        assert_eq!(
            format!("{:?}", registry.get(&child.task_id)),
            plan_state_before,
            "重复注册不得改变执行注册表"
        );
        assert_eq!(
            format!("{:?}", session_store.active_execution_chain(&session_id)),
            chain_state_before,
            "重复注册不得改变 session active chain"
        );
        assert_eq!(
            format!("{:?}", session_store.thread_registry_snapshot(&session_id)),
            threads_state_before,
            "重复注册不得改变 session thread registry"
        );
        assert_eq!(
            format!(
                "{:?}",
                spawn_graph
                    .lock()
                    .expect("spawn graph lock should be available")
                    .all_edges()
            ),
            graph_state_before,
            "重复注册不得改变 SpawnGraph"
        );

        task_store
            .remove_task(&child.task_id)
            .expect("移除测试任务应成功")
            .expect("测试任务应存在");
        let task_state_before = format!("{:?}", task_store.get_task(&child.task_id));
        let plan_state_before = format!("{:?}", registry.get(&child.task_id));
        let chain_state_before = format!("{:?}", session_store.active_execution_chain(&session_id));
        let threads_state_before =
            format!("{:?}", session_store.thread_registry_snapshot(&session_id));
        let graph_state_before = format!(
            "{:?}",
            spawn_graph
                .lock()
                .expect("spawn graph lock should be available")
                .all_edges()
        );
        let registry_duplicate_error = registry
            .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                task_store: &task_store,
                spawn_graph: &spawn_graph,
                session_store: &session_store,
                child_task: &child,
                session_id: &session_id,
                workspace_id: &workspace_id,
                role: "executor",
                role_parallelism_limit: None,
                plan_store: None,
                plan_item_id: None,
                execution_root: None,
                now,
            })
            .expect_err("仅执行注册表中已有的任务也必须被拒绝");
        assert!(matches!(
            registry_duplicate_error,
            SpawnedChildExecutionError::InvalidState(message)
                if message.contains("执行注册表")
        ));
        assert_eq!(
            format!("{:?}", task_store.get_task(&child.task_id)),
            task_state_before,
            "执行注册表重复注册不得写入 TaskStore"
        );
        assert_eq!(
            format!("{:?}", registry.get(&child.task_id)),
            plan_state_before,
            "执行注册表重复注册不得改变执行计划"
        );
        assert_eq!(
            format!("{:?}", session_store.active_execution_chain(&session_id)),
            chain_state_before,
            "执行注册表重复注册不得改变 session active chain"
        );
        assert_eq!(
            format!("{:?}", session_store.thread_registry_snapshot(&session_id)),
            threads_state_before,
            "执行注册表重复注册不得改变 session thread registry"
        );
        assert_eq!(
            format!(
                "{:?}",
                spawn_graph
                    .lock()
                    .expect("spawn graph lock should be available")
                    .all_edges()
            ),
            graph_state_before,
            "执行注册表重复注册不得改变 SpawnGraph"
        );

        let graph_conflict_child = test_task(
            "task-child-graph-conflict",
            root_task_id.as_str(),
            &mission_id,
        );
        spawn_graph
            .lock()
            .expect("spawn graph lock should be available")
            .add_edge(
                root_task_id.clone(),
                graph_conflict_child.task_id.clone(),
                graph_conflict_child.kind,
                std::time::SystemTime::now(),
            )
            .expect("应先建立用于验证回滚的冲突边");
        let task_state_before = format!("{:?}", task_store.get_task(&graph_conflict_child.task_id));
        let plan_state_before = format!("{:?}", registry.get(&graph_conflict_child.task_id));
        let chain_state_before = format!("{:?}", session_store.active_execution_chain(&session_id));
        let threads_state_before =
            format!("{:?}", session_store.thread_registry_snapshot(&session_id));
        let graph_state_before = format!(
            "{:?}",
            spawn_graph
                .lock()
                .expect("spawn graph lock should be available")
                .all_edges()
        );
        let graph_error = registry
            .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                task_store: &task_store,
                spawn_graph: &spawn_graph,
                session_store: &session_store,
                child_task: &graph_conflict_child,
                session_id: &session_id,
                workspace_id: &workspace_id,
                role: "executor",
                role_parallelism_limit: None,
                plan_store: None,
                plan_item_id: None,
                execution_root: None,
                now,
            })
            .expect_err("SpawnGraph 冲突必须拒绝注册");
        assert!(matches!(
            graph_error,
            SpawnedChildExecutionError::InvalidState(message)
                if message.contains("SpawnGraph")
        ));
        assert_eq!(
            format!("{:?}", task_store.get_task(&graph_conflict_child.task_id)),
            task_state_before,
            "SpawnGraph 失败后必须回滚 TaskStore"
        );
        assert_eq!(
            format!("{:?}", registry.get(&graph_conflict_child.task_id)),
            plan_state_before,
            "SpawnGraph 失败后不得写入执行注册表"
        );
        assert_eq!(
            format!("{:?}", session_store.active_execution_chain(&session_id)),
            chain_state_before,
            "SpawnGraph 失败后必须回滚 session active chain"
        );
        assert_eq!(
            format!("{:?}", session_store.thread_registry_snapshot(&session_id)),
            threads_state_before,
            "SpawnGraph 失败后不得创建 session thread"
        );
        assert_eq!(
            format!(
                "{:?}",
                spawn_graph
                    .lock()
                    .expect("spawn graph lock should be available")
                    .all_edges()
            ),
            graph_state_before,
            "SpawnGraph 失败后必须保留原有拓扑"
        );
    }

    #[test]
    fn remove_session_drops_every_execution_plan_owned_by_session() {
        let registry = TaskExecutionRegistry::default();
        let session_a = SessionId::new("session-a");
        let session_b = SessionId::new("session-b");
        let mission_id = MissionId::new("mission-registry-cleanup");
        for (task_id, session_id) in [
            (TaskId::new("task-a-root"), session_a.clone()),
            (TaskId::new("task-a-child"), session_a.clone()),
            (TaskId::new("task-b-root"), session_b.clone()),
        ] {
            registry
                .insert(
                    task_id.clone(),
                    TaskExecutionPlan::Dispatch {
                        target: magi_core::TaskExecutionTarget {
                            mission_id: mission_id.clone(),
                            root_task_id: task_id.clone(),
                            task_id,
                            requested_worker_id: None,
                            recovery_id: None,
                            execution_chain_ref: None,
                        },
                        worker_id: WorkerId::new("worker-registry-cleanup"),
                        thread_id: ThreadId::new("thread-registry-cleanup"),
                        is_primary: true,
                        session_id,
                        turn_id: "turn-registry-cleanup".to_string(),
                        workspace_id: None,
                        execution_root: None,
                        ownership: ExecutionOwnership::default(),
                        writebacks: ExecutionWritebackPlans::default(),
                        use_tools: true,
                        skill_name: None,
                        images: Vec::new(),
                        execution_settings_snapshot: None,
                    },
                )
                .expect("清理测试执行计划应严格插入注册表");
        }

        let removed = registry.remove_session(&session_a);

        assert_eq!(removed.len(), 2);
        assert!(registry.get(&TaskId::new("task-a-root")).is_none());
        assert!(registry.get(&TaskId::new("task-a-child")).is_none());
        assert!(registry.get(&TaskId::new("task-b-root")).is_some());
    }

    #[test]
    fn spawned_local_agent_child_registration_allows_five_agents_per_role() {
        let task_store = TaskStore::new();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_store = SessionStore::new();
        let registry = TaskExecutionRegistry::default();
        let session_id = SessionId::new("session-agent-capacity");
        let workspace_id = Some(WorkspaceId::new("workspace-agent-capacity"));
        let mission_id = MissionId::new("mission-agent-capacity");
        let root_task_id = TaskId::new("task-root-capacity");
        let parent_worker_id = WorkerId::new("worker-parent-capacity");
        let now = UtcMillis(20_000);
        let _ = session_store.ensure_session_mission(&session_id, now, || mission_id.clone());
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-agent-capacity".to_string(),
                    workspace_id: workspace_id.clone(),
                    active_branch_task_ids: vec![root_task_id.clone()],
                    active_worker_bindings: vec![parent_worker_id.clone()],
                    branches: vec![ActiveExecutionBranch {
                        task_id: root_task_id.clone(),
                        worker_id: parent_worker_id,
                        stage: "execute".to_string(),
                        lease_id: None,
                        execution_intent_ref: None,
                        binding_lifecycle: None,
                        checkpoint_stage: Some("execute".to_string()),
                        next_step_index: Some(0),
                        checkpoint_at: Some(now),
                        resume_mode: Some("stage-restart".to_string()),
                        resume_token: None,
                        use_tools: true,
                        skill_name: None,
                        is_primary: true,
                        thread_id: ThreadId::new("thread-agent-capacity-parent"),
                    }],
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-agent-capacity".to_string(),
                        trimmed_text: Some("spawn children".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-agent-capacity".to_string(),
                        turn_seq: 1,
                        accepted_at: now,
                        completed_at: None,
                        status: "running".to_string(),
                        user_message: Some("spawn children".to_string()),
                        items: Vec::new(),
                    }),
                },
            )
            .expect("active chain should be accepted");

        for (role_index, role) in ["executor", "reviewer"].into_iter().enumerate() {
            for instance_index in 0..5 {
                let child = test_task(
                    &format!("task-child-capacity-{role}-{instance_index}"),
                    root_task_id.as_str(),
                    &mission_id,
                );
                registry
                    .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                        task_store: &task_store,
                        spawn_graph: &spawn_graph,
                        session_store: &session_store,
                        child_task: &child,
                        session_id: &session_id,
                        workspace_id: &workspace_id,
                        role,
                        role_parallelism_limit: Some(DEFAULT_MAX_ACTIVE_AGENTS_PER_ROLE as u32),
                        plan_store: None,
                        plan_item_id: None,
                        execution_root: None,
                        now: UtcMillis(now.0 + (role_index * 5 + instance_index) as u64 + 1),
                    })
                    .expect("默认容量应允许每个角色同时运行五个代理实例");
            }
        }

        let overflow_child = test_task(
            "task-child-capacity-executor-overflow",
            root_task_id.as_str(),
            &mission_id,
        );
        let error = registry
            .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                task_store: &task_store,
                spawn_graph: &spawn_graph,
                session_store: &session_store,
                child_task: &overflow_child,
                session_id: &session_id,
                workspace_id: &workspace_id,
                role: "executor",
                role_parallelism_limit: Some(DEFAULT_MAX_ACTIVE_AGENTS_PER_ROLE as u32),
                plan_store: None,
                plan_item_id: None,
                execution_root: None,
                now: UtcMillis(now.0 + 10),
            })
            .expect_err("同一角色的第六个并发代理应被角色实例上限拒绝");

        assert_eq!(
            error,
            SpawnedChildExecutionError::RoleCapacityExceeded {
                role: "executor".to_string(),
                active: DEFAULT_MAX_ACTIVE_AGENTS_PER_ROLE,
                limit: DEFAULT_MAX_ACTIVE_AGENTS_PER_ROLE,
            }
        );
        assert!(
            task_store.get_task(&overflow_child.task_id).is_none(),
            "被容量拒绝的子代理不能写入 task_store"
        );
        assert!(
            spawn_graph
                .lock()
                .expect("spawn graph lock should be available")
                .parent_of(&overflow_child.task_id)
                .is_none(),
            "被容量拒绝的子代理不能写入 spawn_graph"
        );

        let completed_task_id = TaskId::new("task-child-capacity-executor-0");
        task_store
            .update_status_checked(&completed_task_id, TaskStatus::Running)
            .expect("executor 代理应进入运行态");
        task_store
            .complete_task(
                &completed_task_id,
                magi_core::TaskCompletionAttempt {
                    output_refs: vec!["executor done".to_string()],
                    final_response: Some("executor done".to_string()),
                    evidence: Vec::new(),
                },
            )
            .expect("完成一个 executor 代理后应释放角色容量");
        registry
            .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                task_store: &task_store,
                spawn_graph: &spawn_graph,
                session_store: &session_store,
                child_task: &overflow_child,
                session_id: &session_id,
                workspace_id: &workspace_id,
                role: "executor",
                role_parallelism_limit: Some(DEFAULT_MAX_ACTIVE_AGENTS_PER_ROLE as u32),
                plan_store: None,
                plan_item_id: None,
                execution_root: None,
                now: UtcMillis(now.0 + 11),
            })
            .expect("同角色已有实例完成后，第六个代理应能占用释放的名额");
    }

    #[test]
    fn spawned_local_agent_child_registration_rolls_back_when_plan_binding_fails() {
        let (
            task_store,
            spawn_graph,
            session_store,
            registry,
            session_id,
            workspace_id,
            mission_id,
            root_task_id,
            now,
        ) = spawn_fixture("plan-binding-rollback");
        let plan_store = magi_plan::PlanStore::from_store(&session_store, session_id.clone());
        session_store
            .create_session(session_id.clone(), "计划绑定回滚测试")
            .expect("计划绑定测试会话应创建");
        session_store
            .upsert_plan(
                &session_id,
                magi_session_store::SessionPlan {
                    plan_id: magi_core::PlanId::new("plan-binding-rollback"),
                    session_id: session_id.clone(),
                    goal_id: None,
                    revision: 1,
                    language: "zh-CN".to_string(),
                    state: magi_core::PlanState::Active,
                    items: vec![magi_core::PlanItem::new(
                        magi_core::PlanItemId::new("active-item"),
                        "绑定代理",
                        magi_core::PlanItemStatus::InProgress,
                    )],
                    task_bindings: HashMap::new(),
                    task_statuses: HashMap::new(),
                    updated_at: now,
                },
                Some(0),
            )
            .expect("测试计划应创建");

        let child = test_task(
            "task-child-plan-binding-rollback",
            root_task_id.as_str(),
            &mission_id,
        );
        let chain_before = session_store
            .active_execution_chain(&session_id)
            .expect("测试执行链应存在");
        let threads_before = session_store.thread_registry_snapshot(&session_id);
        let graph_before = spawn_graph.lock().expect("SpawnGraph 锁应可用").all_edges();
        let plan_before = plan_store.snapshot().expect("测试计划应存在");

        let error = registry
            .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                task_store: &task_store,
                spawn_graph: &spawn_graph,
                session_store: &session_store,
                child_task: &child,
                session_id: &session_id,
                workspace_id: &workspace_id,
                role: "executor",
                role_parallelism_limit: None,
                plan_store: Some(&plan_store),
                plan_item_id: Some(magi_core::PlanItemId::new("missing-item")),
                execution_root: None,
                now,
            })
            .expect_err("计划绑定失败时必须回滚整次注册");

        assert!(
            matches!(error, SpawnedChildExecutionError::InvalidState(message) if message.contains("计划绑定失败"))
        );
        assert!(task_store.get_task(&child.task_id).is_none());
        assert!(registry.get(&child.task_id).is_none());
        assert_eq!(
            format!(
                "{:?}",
                session_store
                    .active_execution_chain(&session_id)
                    .expect("执行链应保留")
            ),
            format!("{:?}", chain_before),
            "计划绑定失败后必须恢复完整 active execution chain"
        );
        assert_eq!(
            session_store.thread_registry_snapshot(&session_id),
            threads_before,
            "计划绑定失败后不得残留执行 thread"
        );
        assert_eq!(
            format!(
                "{:?}",
                spawn_graph.lock().expect("SpawnGraph 锁应可用").all_edges()
            ),
            format!("{:?}", graph_before),
            "计划绑定失败后不得残留 SpawnGraph 边"
        );
        let plan_after = plan_store.snapshot().expect("计划应保留");
        assert_eq!(plan_after.task_bindings, plan_before.task_bindings);
        assert_eq!(plan_after.task_statuses, plan_before.task_statuses);
    }

    #[test]
    fn spawned_local_agent_child_registration_rejects_duplicate_canonical_name() {
        let (
            task_store,
            spawn_graph,
            session_store,
            registry,
            session_id,
            workspace_id,
            mission_id,
            root_task_id,
            now,
        ) = spawn_fixture("canonical-name");
        let canonical_name = "/root/canonical-name";
        let mut first = test_task(
            "task-child-canonical-first",
            root_task_id.as_str(),
            &mission_id,
        );
        first.executor_binding = Some(
            magi_core::TaskExecutorBinding::for_role("executor")
                .with_canonical_task_name(canonical_name),
        );
        registry
            .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                task_store: &task_store,
                spawn_graph: &spawn_graph,
                session_store: &session_store,
                child_task: &first,
                session_id: &session_id,
                workspace_id: &workspace_id,
                role: "executor",
                role_parallelism_limit: None,
                plan_store: None,
                plan_item_id: None,
                execution_root: None,
                now,
            })
            .expect("第一个 canonical task name 应注册成功");

        let mut duplicate = test_task(
            "task-child-canonical-duplicate",
            root_task_id.as_str(),
            &mission_id,
        );
        duplicate.executor_binding = Some(
            magi_core::TaskExecutorBinding::for_role("executor")
                .with_canonical_task_name(canonical_name),
        );
        let error = registry
            .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                task_store: &task_store,
                spawn_graph: &spawn_graph,
                session_store: &session_store,
                child_task: &duplicate,
                session_id: &session_id,
                workspace_id: &workspace_id,
                role: "executor",
                role_parallelism_limit: None,
                plan_store: None,
                plan_item_id: None,
                execution_root: None,
                now: UtcMillis(now.0 + 1),
            })
            .expect_err("重复 canonical task name 必须被拒绝");

        assert!(
            matches!(error, SpawnedChildExecutionError::InvalidState(message) if message.contains("canonical task name"))
        );
        assert!(task_store.get_task(&duplicate.task_id).is_none());
        assert!(registry.get(&duplicate.task_id).is_none());
    }

    fn spawn_fixture(
        label: &str,
    ) -> (
        TaskStore,
        Mutex<SpawnGraph>,
        SessionStore,
        TaskExecutionRegistry,
        SessionId,
        Option<WorkspaceId>,
        MissionId,
        TaskId,
        UtcMillis,
    ) {
        let task_store = TaskStore::new();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_store = SessionStore::new();
        let registry = TaskExecutionRegistry::default();
        let session_id = SessionId::new(format!("session-{label}"));
        let workspace_id = Some(WorkspaceId::new(format!("workspace-{label}")));
        let mission_id = MissionId::new(format!("mission-{label}"));
        let root_task_id = TaskId::new(format!("task-root-{label}"));
        let parent_worker_id = WorkerId::new(format!("worker-parent-{label}"));
        let now = UtcMillis(30_000);
        let _ = session_store.ensure_session_mission(&session_id, now, || mission_id.clone());
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: format!("chain-{label}"),
                    workspace_id: workspace_id.clone(),
                    active_branch_task_ids: vec![root_task_id.clone()],
                    active_worker_bindings: vec![parent_worker_id.clone()],
                    branches: vec![ActiveExecutionBranch {
                        task_id: root_task_id.clone(),
                        worker_id: parent_worker_id,
                        stage: "execute".to_string(),
                        lease_id: None,
                        execution_intent_ref: None,
                        binding_lifecycle: None,
                        checkpoint_stage: Some("execute".to_string()),
                        next_step_index: Some(0),
                        checkpoint_at: Some(now),
                        resume_mode: Some("stage-restart".to_string()),
                        resume_token: None,
                        use_tools: true,
                        skill_name: None,
                        is_primary: true,
                        thread_id: ThreadId::new(format!("thread-{label}-parent")),
                    }],
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: format!("timeline-{label}"),
                        trimmed_text: Some("spawn children".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: format!("turn-{label}"),
                        turn_seq: 1,
                        accepted_at: now,
                        completed_at: None,
                        status: "running".to_string(),
                        user_message: Some("spawn children".to_string()),
                        items: Vec::new(),
                    }),
                },
            )
            .expect("测试执行链应创建");
        (
            task_store,
            spawn_graph,
            session_store,
            registry,
            session_id,
            workspace_id,
            mission_id,
            root_task_id,
            now,
        )
    }

    #[test]
    fn spawned_local_agent_child_registration_uses_role_parallelism_limit() {
        let (
            task_store,
            spawn_graph,
            session_store,
            registry,
            session_id,
            workspace_id,
            mission_id,
            root_task_id,
            now,
        ) = spawn_fixture("role-limit");

        let limited_first = test_task(
            "task-child-role-limit-0",
            root_task_id.as_str(),
            &mission_id,
        );
        registry
            .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                task_store: &task_store,
                spawn_graph: &spawn_graph,
                session_store: &session_store,
                child_task: &limited_first,
                session_id: &session_id,
                workspace_id: &workspace_id,
                role: "limited-role",
                role_parallelism_limit: Some(1),
                plan_store: None,
                plan_item_id: None,
                execution_root: None,
                now: UtcMillis(now.0 + 1),
            })
            .expect("并发上限为 1 时应允许第一个实例");

        let limited_second = test_task(
            "task-child-role-limit-1",
            root_task_id.as_str(),
            &mission_id,
        );
        let limited_error = registry
            .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                task_store: &task_store,
                spawn_graph: &spawn_graph,
                session_store: &session_store,
                child_task: &limited_second,
                session_id: &session_id,
                workspace_id: &workspace_id,
                role: "limited-role",
                role_parallelism_limit: Some(1),
                plan_store: None,
                plan_item_id: None,
                execution_root: None,
                now: UtcMillis(now.0 + 2),
            })
            .expect_err("并发上限为 1 时不应允许第二个活跃实例");
        assert_eq!(
            limited_error,
            SpawnedChildExecutionError::RoleCapacityExceeded {
                role: "limited-role".to_string(),
                active: 1,
                limit: 1,
            }
        );

        task_store
            .update_status_checked(&limited_first.task_id, TaskStatus::Running)
            .expect("受限角色实例应进入运行态");
        task_store
            .complete_task(
                &limited_first.task_id,
                magi_core::TaskCompletionAttempt {
                    output_refs: vec!["limited done".to_string()],
                    final_response: Some("limited done".to_string()),
                    evidence: Vec::new(),
                },
            )
            .expect("完成受限角色实例后应释放容量");
        registry
            .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                task_store: &task_store,
                spawn_graph: &spawn_graph,
                session_store: &session_store,
                child_task: &limited_second,
                session_id: &session_id,
                workspace_id: &workspace_id,
                role: "limited-role",
                role_parallelism_limit: Some(1),
                plan_store: None,
                plan_item_id: None,
                execution_root: None,
                now: UtcMillis(now.0 + 3),
            })
            .expect("前一个受限实例完成后应允许下一个实例");

        for index in 0..=DEFAULT_MAX_ACTIVE_AGENTS_PER_ROLE {
            let unlimited_child = test_task(
                &format!("task-child-role-unlimited-{index}"),
                root_task_id.as_str(),
                &mission_id,
            );
            registry
                .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                    task_store: &task_store,
                    spawn_graph: &spawn_graph,
                    session_store: &session_store,
                    child_task: &unlimited_child,
                    session_id: &session_id,
                    workspace_id: &workspace_id,
                    role: "unlimited-role",
                    role_parallelism_limit: None,
                    plan_store: None,
                    plan_item_id: None,
                    execution_root: None,
                    now: UtcMillis(now.0 + 4 + index as u64),
                })
                .expect("None 应表示不设置角色级并发上限");
        }
    }
}
