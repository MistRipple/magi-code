//! 任务系统 — 任务派发计划与注册中心。
//!
//! - [`TaskExecutionPlan`]：dispatch_submission 接受后挂在 task_execution_registry
//!   上的派发载体；当前派发链路只保留 Dispatch 一支。
//! - [`TaskExecutionRegistry`]：线程安全的 `TaskId → TaskExecutionPlan` 索引，
//!   `LlmTaskDispatcher` 与 `Runner` 通过它取出已接受派发计划。
//!
//! magi-api 不再实现这两个类型，改为 `pub use` 重导出；本模块是任务派发链路的
//! 唯一所有者。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use magi_bridge_client::ModelBridgeClient;
use magi_core::{
    PlanItemId, SessionId, Task, TaskExecutionTarget, TaskId, ThreadId, UtcMillis, WorkerId,
    WorkspaceId,
};
use magi_orchestrator::{ExecutionWritebackPlans, task_store::TaskStore};
use magi_session_store::{ActiveExecutionBranch, ExecutionThread, SessionPlan, SessionStore};
use magi_settings_store::SettingsStore;

use crate::execution_admission::ExecutionAdmissionController;

/// 同一父任务下同时未结束的直接子代理上限。
pub const MAX_OPEN_CHILDREN_PER_PARENT: usize = 16;
use crate::{session_images::SessionTurnImage, session_thread};

pub const DEFAULT_MAX_ACTIVE_AGENTS_PER_ROLE: usize = 5;

/// 宿主在原 Task 终态提交后结算这一组任务的资源与执行线程。
/// 只保存进程内能力句柄，不拥有另一份 Task 或资源状态。
type AgentSubtreeSettler = dyn Fn(&SessionId, &[TaskId]) -> Result<(), String> + Send + Sync;

/// `agent_spawn` 创建前预检需要的进程级运行时事实。
///
/// 该配置只保存可共享的句柄，不在预检阶段执行任何持久化或 Git mutation。
/// 创建代理必须提供有效模型配置或显式注入的客户端；测试也遵循同一预检合同。
/// daemon 生产装配注入真实 settings、模型客户端和 Git/workspace 事实源。
#[derive(Clone, Default)]
pub struct AgentSpawnPreflightRuntime {
    pub settings_store: Option<Arc<SettingsStore>>,
    pub default_model_client: Option<Arc<dyn ModelBridgeClient>>,
    pub workspace_registry: Option<Arc<magi_workspace::WorkspaceStore>>,
    pub session_code_contexts: Option<magi_git::SessionCodeContextRegistry>,
    pub git_service_configured: bool,
    pub skill_runtime: Option<Arc<magi_skill_runtime::SkillRuntime>>,
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
    pub session_store: &'a SessionStore,
    pub child_task: &'a Task,
    pub session_id: &'a SessionId,
    pub workspace_id: &'a Option<WorkspaceId>,
    pub role: &'a str,
    /// 当前工具调用已校验的委派目的；只对自主并行/上下文隔离拦截完全相同的活跃工作包。
    pub delegation_purpose: magi_tool_runtime::AgentDelegationPurpose,
    /// 角色定义中的并发上限。`None` 表示该角色不设置角色级上限；全局和会话级
    /// 执行准入仍然继续生效。调用方必须从同一份 AgentRoleRegistry 读取该值。
    pub role_parallelism_limit: Option<u32>,
    /// 可选的当前会话计划。传入时，子任务与 `plan_item_id` 的绑定和回滚都在
    /// 同一个注册事务内完成；预检阶段已经确认该 item 存在且处于可执行状态。
    pub plan_store: Option<&'a magi_plan::PlanStore>,
    pub plan_item_id: Option<PlanItemId>,
    /// 可选的已通过预检的执行目录。为空时继承父任务计划中的目录。
    pub execution_root: Option<PathBuf>,
    /// 子代理是否沿用父任务当前激活的 Skill。
    pub inherit_skill: bool,
    pub now: UtcMillis,
}

#[derive(Debug)]
pub struct SpawnedChildExecution {
    pub worker_id: WorkerId,
    pub thread_id: ThreadId,
    pub execution_chain_ref: String,
    /// 计划绑定在同一注册事务内完成后的新快照。调用方只负责发布事件，不再重复绑定。
    pub plan: Option<SessionPlan>,
    /// 注册时预占执行名额失败的原因；为空表示已预占、会立即开始。
    pub queue_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpawnedChildExecutionError {
    GoalPaused(String),
    DuplicateTaskName {
        existing_task_id: TaskId,
    },
    DuplicateAgentWork {
        existing_task_id: TaskId,
    },
    RoleCapacityExceeded {
        role: String,
        active: usize,
        limit: usize,
    },
    SharedWorkspaceWriterActive,
    InvalidState(String),
}

impl std::fmt::Display for SpawnedChildExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GoalPaused(message) => f.write_str(message),
            Self::DuplicateTaskName { existing_task_id } => {
                write!(
                    f,
                    "同一父任务下 canonical task name 已存在，原任务: {existing_task_id}"
                )
            }
            Self::DuplicateAgentWork { existing_task_id } => {
                write!(f, "相同工作包已有活跃代理，原任务: {existing_task_id}")
            }
            Self::RoleCapacityExceeded {
                role,
                active,
                limit,
            } => write!(
                f,
                "角色 {role} 已达到代理实例上限：最多 {limit} 个活跃实例，当前 {active} 个"
            ),
            Self::SharedWorkspaceWriterActive => f.write_str(
                "当前工作区没有 Git 隔离，已有一个具有写入权限的代理未结束；请等待它结束后再派发",
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
    agent_subtree_settler: Arc<RwLock<Option<Arc<AgentSubtreeSettler>>>>,
    /// 序列化跨 `TaskStore`、`SessionStore` 和执行计划的子任务注册。
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
            agent_subtree_settler: Arc::new(RwLock::new(None)),
            registration_lock: Arc::new(Mutex::new(())),
        }
    }
}

impl TaskExecutionRegistry {
    /// 只从已有拥有方和 Goal 控制版本派生撤销范围，不保存另一份授权状态。
    pub fn revoked_goal_agent_roots(
        task_store: &TaskStore,
        session_store: &SessionStore,
        session_id: &SessionId,
    ) -> Vec<Task> {
        let goals = session_store.goals_for_session(session_id);
        task_store
            .all_tasks()
            .into_iter()
            .filter(|task| {
                task.agent_run_transfer().is_some_and(|transfer| {
                    &transfer.session_id == session_id
                        && match &transfer.owner {
                            magi_core::AgentRunOwner::Session => false,
                            magi_core::AgentRunOwner::Goal {
                                goal_id,
                                control_revision,
                            } => !goals.iter().any(|goal| {
                                &goal.goal_id == goal_id
                                    && goal.status == magi_session_store::GoalStatus::Active
                                    && goal.control_revision == *control_revision
                            }),
                        }
                })
            })
            .collect()
    }

    /// 模型工具和宿主控制共用同一个 Task 收口与资源结算入口。
    /// 调用者不能属于被结算范围；当前仅主线可以修改共享 Goal。
    pub fn settle_revoked_goal_agent_run(
        &self,
        task_store: &TaskStore,
        session_id: &SessionId,
        root: &TaskId,
    ) -> Result<bool, String> {
        let terminal = crate::task_runner::settle_subtree_in_store(
            task_store,
            &self.execution_admission,
            root,
            magi_core::TaskStatus::Killed,
            Vec::new(),
            true,
        );
        let ids = task_store.collect_execution_subtree_ids(root);
        // 写盘失败仍应取消实际资源；但不能把失败的终态提交报告为已结算。
        let resources = self.settle_agent_subtree(session_id, &ids);
        terminal?;
        resources
    }

    pub fn settle_revoked_goal_agent_runs(
        &self,
        task_store: &TaskStore,
        session_store: &SessionStore,
        session_id: &SessionId,
    ) -> Result<usize, String> {
        let roots = Self::revoked_goal_agent_roots(task_store, session_store, session_id);
        let mut errors = Vec::new();
        for root in &roots {
            match self.settle_revoked_goal_agent_run(task_store, session_id, &root.task_id) {
                Ok(true) => {}
                Ok(false) => errors.push(format!("{} 未装配资源结算能力", root.task_id)),
                Err(error) => errors.push(format!("{}: {error}", root.task_id)),
            }
        }
        if errors.is_empty() {
            Ok(roots.len())
        } else {
            Err(errors.join("；"))
        }
    }

    pub fn set_agent_subtree_settler(
        &self,
        settle: impl Fn(&SessionId, &[TaskId]) -> Result<(), String> + Send + Sync + 'static,
    ) {
        *self
            .agent_subtree_settler
            .write()
            .expect("agent settler lock poisoned") = Some(Arc::new(settle));
    }

    /// false 表示轻量装配未提供结算能力，不能声称资源已退出。
    pub(crate) fn settle_agent_subtree(
        &self,
        session: &SessionId,
        tasks: &[TaskId],
    ) -> Result<bool, String> {
        let settle = self
            .agent_subtree_settler
            .read()
            .expect("agent settler lock poisoned")
            .clone();
        match settle {
            Some(settle) => settle(session, tasks).map(|()| true),
            None => Ok(false),
        }
    }

    /// 工作流说明可选择不继承；显式封闭工具范围不能通过创建子代理移除。
    pub(crate) fn validate_spawn_skill_inheritance(
        &self,
        parent: &TaskId,
        inherit: bool,
    ) -> Result<(), String> {
        if inherit {
            return Ok(());
        }
        let skill = self.get(parent).and_then(|plan| match plan {
            TaskExecutionPlan::Dispatch { skill_name, .. } => skill_name,
        });
        let runtime = self.agent_spawn_preflight_runtime();
        let policy = crate::active_skill_tool_execution_policy(
            magi_core::AccessProfile::Restricted,
            runtime.skill_runtime.as_deref(),
            skill.as_deref(),
        );
        if policy.allowed_tool_names.is_none() {
            Ok(())
        } else {
            Err("父任务 Skill 明确限制了工具范围；必须使用 inherit_skill=true，不能通过子代理移除该范围".into())
        }
    }
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

    /// Recheck the frozen directory authorization at each tool boundary, including
    /// after an approval wait or process restart.
    pub(crate) fn validate_project_directories(
        &self,
        task: &magi_core::Task,
        workspace_id: &Option<magi_core::WorkspaceId>,
    ) -> Result<(), String> {
        let Some(binding) = task
            .executor_binding
            .as_ref()
            .and_then(|executor| executor.project_directories.as_ref())
        else {
            return Ok(());
        };
        if workspace_id.as_ref() != Some(&binding.workspace_id) {
            return Err("Task 源文件夹授权与当前项目不匹配".into());
        }
        self.agent_spawn_preflight_runtime()
            .workspace_registry
            .ok_or_else(|| "缺少项目目录权威，不能执行已绑定任务".to_string())?
            .validate_directory_binding(binding)
            .map_err(|error| error.to_string())
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
    /// 清理 TaskStore。
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

    pub(crate) fn bind_agent_turn(
        &self,
        task: &TaskId,
        source: &str,
        owned: &magi_session_store::CanonicalTurn,
    ) -> Result<(), String> {
        let mut plans = self
            .plans
            .write()
            .expect("execution registry lock poisoned");
        let Some(TaskExecutionPlan::Dispatch {
            turn_id,
            session_id,
            thread_id,
            skill_name,
            ..
        }) = plans.get_mut(task)
        else {
            return Err("接纳 Agent Turn 后 Task 执行计划消失".into());
        };
        if turn_id != source
            || &owned.session_id != session_id
            || owned
                .agent_turn_binding()
                .map_err(|error| error.to_string())?
                .is_none_or(|binding| &binding.task_id != task || &binding.thread_id != thread_id)
            || owned.status.is_terminal()
        {
            return Err("Agent Turn 执行计划来源变化，拒绝改绑".into());
        }
        *turn_id = owned.turn_id.clone();
        if let Some(skill) = owned
            .agent_active_skill_id()
            .map_err(|error| error.to_string())?
        {
            *skill_name = Some(skill);
        }
        Ok(())
    }

    pub fn update_active_skill(
        &self,
        task_id: &TaskId,
        session_store: &SessionStore,
        session_id: &SessionId,
        expected_turn_id: &str,
        skill_id: String,
    ) -> Result<(), String> {
        let plan = self
            .get(task_id)
            .ok_or_else(|| format!("任务 {task_id} 缺少执行计划，无法记录 Skill 激活"))?;
        let TaskExecutionPlan::Dispatch {
            turn_id,
            session_id: plan_session,
            ..
        } = &plan;
        if plan_session != session_id || turn_id != expected_turn_id {
            return Err("Skill 激活的 Session/Turn 来源已变化".into());
        }
        let owned = session_store
            .canonical_turn_for_session_turn_id(session_id, turn_id)
            .map(|turn| turn.agent_turn_binding())
            .transpose()
            .map_err(|error| error.to_string())?
            .flatten();
        if let Some(binding) = owned {
            if &binding.task_id != task_id {
                return Err("Skill 激活 Task 与 Agent Turn 不匹配".into());
            }
            session_store
                .update_agent_turn_active_skill(session_id, task_id, turn_id, &skill_id)
                .map_err(|error| error.to_string())?;
        } else {
            session_store
                .update_execution_branch_active_skill(session_id, task_id, turn_id, &skill_id)
                .map_err(|error| error.to_string())?;
        }

        let mut plans = self
            .plans
            .write()
            .expect("task execution registry write lock poisoned");
        let plan = plans
            .get_mut(task_id)
            .ok_or_else(|| format!("任务 {task_id} 执行计划在更新期间消失"))?;
        match plan {
            TaskExecutionPlan::Dispatch {
                skill_name,
                turn_id: actual_turn,
                session_id: actual_session,
                ..
            } => {
                if actual_turn != turn_id || actual_session != session_id {
                    return Err("Skill 激活期间执行计划的 Turn/Session 已变化".into());
                }
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
            session_store,
            child_task,
            session_id,
            workspace_id,
            role,
            delegation_purpose,
            role_parallelism_limit,
            plan_store,
            plan_item_id,
            execution_root,
            inherit_skill,
            now,
        } = request;
        let _registration_guard = self.registration_lock.lock().map_err(|error| {
            SpawnedChildExecutionError::InvalidState(format!("agent_spawn 注册锁不可用: {error}"))
        })?;
        let chain = session_store
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
        let parent = task_store.get_task(&parent_task_id).ok_or_else(|| {
            SpawnedChildExecutionError::InvalidState("agent_spawn 父任务不存在".into())
        })?;
        self.validate_spawn_skill_inheritance(&parent_task_id, inherit_skill)
            .map_err(SpawnedChildExecutionError::InvalidState)?;
        task_store
            .validate_agent_delegation(&parent)
            .map_err(|error| SpawnedChildExecutionError::InvalidState(error.to_string()))?;
        if parent.parent_task_id.is_some() && plan_item_id.is_some() {
            return Err(SpawnedChildExecutionError::InvalidState(
                "嵌套任务不能绑定主线共享计划".into(),
            ));
        }
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

        if let Some(goal) = chain
            .current_turn_id
            .as_deref()
            .and_then(|turn_id| session_store.model_step_goal(session_id, turn_id))
            && goal.status != magi_session_store::GoalStatus::Active
        {
            return Err(SpawnedChildExecutionError::GoalPaused(
                "当前 Goal 未处于执行状态，不能创建新的子代理".into(),
            ));
        }

        if let Some(activation) = child_task.agent_activation() {
            if activation.session_id != *session_id {
                return Err(SpawnedChildExecutionError::InvalidState(
                    "Agent activation 会话归属不匹配".into(),
                ));
            }
            if let magi_core::AgentLifetime::Goal { goal_id } = &activation.lifetime
                && chain
                    .current_turn_id
                    .as_deref()
                    .and_then(|turn_id| session_store.model_step_goal(session_id, turn_id))
                    .is_none_or(|goal| {
                        goal.goal_id != *goal_id
                            || goal.status != magi_session_store::GoalStatus::Active
                    })
            {
                return Err(SpawnedChildExecutionError::GoalPaused(
                    "Agent 所属 Goal 已关闭或不属于当前 Turn".into(),
                ));
            }
        }

        // `preflight_agent_spawn` 已经检查过一次 task_name，但它与原子注册之间
        // 可能存在并发窗口。这里用注册锁下的最新 TaskStore 快照再做一次唯一性
        // 校验，确保同一父任务不会出现两个相同的 canonical task name。
        if let Some(canonical_task_name) = child_task.canonical_task_name()
            && let Some(existing) = task_store
                .get_children(&parent_task_id)
                .iter()
                .find(|child| child.canonical_task_name() == Some(canonical_task_name))
        {
            return Err(SpawnedChildExecutionError::DuplicateTaskName {
                existing_task_id: existing.task_id.clone(),
            });
        }

        let parent_plan = self.get(&parent_task_id);
        let inherited_skill_name =
            parent_plan
                .as_ref()
                .filter(|_| inherit_skill)
                .and_then(|plan| match plan {
                    TaskExecutionPlan::Dispatch { skill_name, .. } => skill_name.clone(),
                });
        let inherited_execution_root = execution_root.or_else(|| {
            parent_plan.as_ref().and_then(|plan| match plan {
                TaskExecutionPlan::Dispatch { execution_root, .. } => execution_root.clone(),
            })
        });
        // 同名检查不足以拦住换名复制；在同一注册锁内读取原 Task/执行计划，
        // 不新增指纹表。明确要求的多个代理和独立复核可以有意处理相同输入。
        if matches!(
            delegation_purpose,
            magi_tool_runtime::AgentDelegationPurpose::ParallelWork
                | magi_tool_runtime::AgentDelegationPurpose::ContextIsolation
        ) && let Some(existing) =
            task_store
                .get_children(&parent_task_id)
                .iter()
                .find(|existing| {
                    matches!(
                        existing.status,
                        magi_core::TaskStatus::Pending | magi_core::TaskStatus::Running
                    ) && same_agent_work_package(existing, child_task)
                        && self.get(&existing.task_id).is_some_and(|plan| match plan {
                            TaskExecutionPlan::Dispatch {
                                execution_root,
                                skill_name,
                                ..
                            } => {
                                execution_root == inherited_execution_root
                                    && skill_name == inherited_skill_name
                            }
                        })
                })
        {
            return Err(SpawnedChildExecutionError::DuplicateAgentWork {
                existing_task_id: existing.task_id.clone(),
            });
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

        // 父子关系只以 TaskStore 为准：同一父任务下仍未结束的直接子代理数量受上限约束，
        // 已结束的子代理不再占用名额。
        let open_children = task_store
            .get_children(&parent_task_id)
            .iter()
            .filter(|child| {
                matches!(
                    child.status,
                    magi_core::TaskStatus::Pending | magi_core::TaskStatus::Running
                )
            })
            .count();
        if open_children >= MAX_OPEN_CHILDREN_PER_PARENT {
            return Err(SpawnedChildExecutionError::InvalidState(format!(
                "父任务 {parent_task_id} 已有 {open_children} 个未结束的子代理，达到上限 {MAX_OPEN_CHILDREN_PER_PARENT}；请先等待或取消已有代理"
            )));
        }

        // 非 Git 工作区没有隔离 worktree，多个可写代理会直接在同一目录互相覆盖；
        // 同一时间只允许一个可写代理。
        let writes = |task: &magi_core::Task| {
            task.policy_snapshot
                .as_ref()
                .map(magi_core::TaskPolicy::effective_access_profile)
                .unwrap_or_default()
                != magi_core::AccessProfile::ReadOnly
        };
        let isolated_by_git = self
            .agent_spawn_preflight_runtime()
            .session_code_contexts
            .as_ref()
            .is_some_and(|registry| {
                if let Some(binding) = child_task
                    .executor_binding
                    .as_ref()
                    .and_then(|executor| executor.project_directories.as_ref())
                {
                    !binding.source_folders.is_empty()
                        && binding.source_folders.iter().all(|grant| {
                            magi_core::HostPath::from_path_ref(&grant.source_path_ref)
                                .ok()
                                .and_then(|source| {
                                    registry.get_for_path(session_id.as_str(), source.as_path())
                                })
                                .is_some()
                        })
                } else {
                    match inherited_execution_root.as_deref() {
                        Some(root) => registry.get_for_path(session_id.as_str(), root),
                        None => registry.get(session_id.as_str()),
                    }
                    .is_some()
                }
            });
        if !isolated_by_git
            && writes(child_task)
            && task_store
                .collect_subtree_ids(&parent.root_task_id)
                .iter()
                .filter_map(|id| task_store.get_task(id))
                .any(|child| {
                    matches!(
                        child.status,
                        magi_core::TaskStatus::Pending | magi_core::TaskStatus::Running
                    ) && child.task_id != parent.root_task_id
                        && writes(&child)
                })
        {
            return Err(SpawnedChildExecutionError::SharedWorkspaceWriterActive);
        }

        let worker_id = WorkerId::new(format!("worker-spawn-{}", child_task.task_id.as_str()));
        let inherited_turn_id = parent_plan
            .as_ref()
            .map(|plan| match plan {
                TaskExecutionPlan::Dispatch { turn_id, .. } => turn_id.clone(),
            })
            .or_else(|| chain.current_turn_id.clone())
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
        let branch = ActiveExecutionBranch {
            task_id: child_task.task_id.clone(),
            worker_id: worker_id.clone(),
            use_tools: true,
            skill_name: inherited_skill_name.clone(),
            is_primary: false,
            thread_id: thread_id.clone(),
        };
        let execution_chain_ref = chain.execution_chain_ref.clone();

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
            writebacks: ExecutionWritebackPlans::default(),
            use_tools: true,
            skill_name: inherited_skill_name,
            images: Vec::new(),
            execution_settings_snapshot,
        };

        if let Err(error) =
            session_store.insert_execution_branch(session_id, &chain, branch.clone())
        {
            return Err(rollback_spawned_local_agent_child(
                self,
                session_store,
                session_id,
                &child_task.task_id,
                &original_chain,
                &branch,
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
                session_store,
                session_id,
                &child_task.task_id,
                &original_chain,
                &branch,
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
                session_store,
                session_id,
                &child_task.task_id,
                &original_chain,
                &branch,
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
                        session_store,
                        session_id,
                        &child_task.task_id,
                        &original_chain,
                        &branch,
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
                    session_store,
                    session_id,
                    &child_task.task_id,
                    &original_chain,
                    &branch,
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

        // Runner 只以 TaskStore 判断可调度性。完整准备描述与准入后才发布 Task；
        // 提前插入 Pending Task 会让并发 Runner 在计划/Thread 尚未存在时派发。
        let queue_reason = self.execution_admission.reserve(
            child_task.task_id.clone(),
            Some(session_id.clone()),
            role,
        );
        if let Err(error) = task_store.insert_task(child_task.clone()) {
            self.execution_admission
                .release_pending(&child_task.task_id);
            return Err(rollback_spawned_local_agent_child(
                self,
                session_store,
                session_id,
                &child_task.task_id,
                &original_chain,
                &branch,
                true,
                true,
                Some(&thread),
                plan_binding
                    .as_ref()
                    .map(|(original, updated)| (original, updated.revision)),
                SpawnedChildExecutionError::InvalidState(error.to_string()),
            ));
        }
        Ok(SpawnedChildExecution {
            worker_id,
            thread_id,
            execution_chain_ref,
            plan: plan_binding.map(|(_, updated)| updated),
            queue_reason,
        })
    }
}

/// 只比较明确相同的工作包，不用目标相似度或关键词推测任务意图。
fn same_agent_work_package(existing: &Task, candidate: &Task) -> bool {
    let (Some(left), Some(right)) = (
        existing.agent_context_package(),
        candidate.agent_context_package(),
    ) else {
        return false;
    };
    existing.goal.trim() == candidate.goal.trim()
        && existing.executor_binding_target_role() == candidate.executor_binding_target_role()
        && existing.executor_binding_capability_ids() == candidate.executor_binding_capability_ids()
        && existing.plan_item_id() == candidate.plan_item_id()
        && left.summary == right.summary
        && left.constraints == right.constraints
        && left.expected_output == right.expected_output
        && left.fork == right.fork
        && left.references.len() == right.references.len()
        && left
            .references
            .iter()
            .zip(&right.references)
            .all(|(left, right)| {
                left.kind == right.kind
                    && left.source_ref == right.source_ref
                    && left.preview == right.preview
            })
}

#[allow(clippy::too_many_arguments)]
fn rollback_spawned_local_agent_child(
    registry: &TaskExecutionRegistry,
    session_store: &SessionStore,
    session_id: &SessionId,
    child_task_id: &TaskId,
    original_chain: &magi_session_store::ActiveExecutionChain,
    branch: &ActiveExecutionBranch,
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
    if session_chain_updated
        && let Err(error) =
            session_store.remove_execution_branch_if_current(session_id, original_chain, branch)
    {
        rollback_errors.push(format!("session active chain 回滚失败: {error}"));
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
mod tests;
