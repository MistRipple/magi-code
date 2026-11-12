//! 任务系统 — 派发提交载体。
//!
//! 这两个 DTO 与 ApiState / ApiError 无运行期耦合，是 dispatch 流程的
//! "请求 → 接受" 一次性数据载体。magi-api 通过 `pub use` 重导出维持外部
//! import 路径不变。

use std::path::Path;
use std::sync::Arc;

use magi_agent_role::AgentRoleRegistry;
use magi_bridge_client::ModelBridgeClient;
use magi_core::{
    AccessProfile, DomainError, DomainResult, GoalId, MissionId, PlanItemId, SessionId,
    TaskCompletionContract, TaskExecutionTarget, TaskExecutorBinding, TaskId, TaskKind,
    TaskRecoveryCheckpoint, TaskStatus, TaskTier, ThreadId, UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::{EventContext, InMemoryEventBus, task_events};
use magi_orchestrator::{
    DispatchMemoryExtractionInput, ExecutionWritebackPlans, task_store::TaskStore,
};
use magi_session_store::{
    ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
    ActiveExecutionTurn, ActiveExecutionTurnItem, CanonicalTurn, CanonicalTurnItemKind,
    ExecutionThread, SessionPlan, SessionRuntimeSidecar, SessionStore, TimelineEntryInput,
    TimelineEntryKind,
};
use serde::{Deserialize, Serialize};

use crate::session_thread;
use crate::session_writeback::CanonicalTurnEventSink;

use crate::context_reference::{
    SessionContextReference, browser_annotation_artifact_paths,
    browser_annotation_reference_input_refs, browser_annotation_references_metadata,
    browser_node_selection_input_refs, browser_node_selections_metadata,
    session_context_reference_input_refs, session_context_reference_paths,
    session_context_references_metadata,
};
use crate::session_images::SessionTurnImage;
use crate::task_execution_registry::{TaskExecutionPlan, TaskExecutionRegistry};
use magi_settings_store::SettingsStore;

pub struct DispatchSubmissionGraph {
    pub root_task_id: TaskId,
    pub action_task_id: TaskId,
    pub active_execution_chain: ActiveExecutionChain,
    pub turn: ActiveExecutionTurn,
    pub(crate) materialize_rollback: Option<MaterializeRollback>,
}

impl DispatchSubmissionGraph {
    pub fn new(
        root_task_id: TaskId,
        action_task_id: TaskId,
        active_execution_chain: ActiveExecutionChain,
        turn: ActiveExecutionTurn,
    ) -> Self {
        Self {
            root_task_id,
            action_task_id,
            active_execution_chain,
            turn,
            materialize_rollback: None,
        }
    }
}

struct PlanMaterialization {
    original_plan: Option<SessionPlan>,
    current_revision: u64,
}

/// materialize 阶段跨 SessionStore、PlanStore 和执行注册表的补偿记录。
///
/// 这些 store 没有共享事务锁，因此提交顺序必须由本记录管理：任何一个步骤失败
/// 都按“只回收本次写入、遇到并发修改则拒绝覆盖”的规则补偿。直接 runtime 入口
/// 在交给 accepted 流程前也持有该记录，避免接受失败留下半装配状态。
pub(crate) struct MaterializeRollback {
    session_store: SessionStore,
    execution_registry: TaskExecutionRegistry,
    session_id: SessionId,
    task_id: TaskId,
    turn_id: String,
    created_threads: Vec<ExecutionThread>,
    coordinator_thread_before: Option<ExecutionThread>,
    coordinator_thread_after: Option<ExecutionThread>,
    plan_materialization: Option<PlanMaterialization>,
    registry_inserted: bool,
    committed: bool,
}

impl MaterializeRollback {
    fn new(
        session_store: &SessionStore,
        execution_registry: &TaskExecutionRegistry,
        session_id: SessionId,
        task_id: TaskId,
        turn_id: String,
    ) -> Self {
        Self {
            session_store: session_store.clone(),
            execution_registry: execution_registry.clone(),
            session_id,
            task_id,
            turn_id,
            created_threads: Vec::new(),
            coordinator_thread_before: None,
            coordinator_thread_after: None,
            plan_materialization: None,
            registry_inserted: false,
            committed: false,
        }
    }

    fn record_created_thread(&mut self, thread: ExecutionThread) {
        self.created_threads.push(thread);
    }

    fn refresh_created_thread(&mut self, thread_id: &ThreadId) -> Result<(), String> {
        let thread = self
            .session_store
            .thread_registry_snapshot(&self.session_id)
            .into_iter()
            .find(|thread| &thread.thread_id == thread_id)
            .ok_or_else(|| format!("回滚记录缺少已创建 thread {thread_id}"))?;
        let Some(created) = self
            .created_threads
            .iter_mut()
            .find(|created| &created.thread_id == thread_id)
        else {
            return Err(format!("thread {thread_id} 不属于本次 materialize"));
        };
        *created = thread;
        Ok(())
    }

    fn record_coordinator_before(&mut self, thread: ExecutionThread) {
        self.coordinator_thread_before = Some(thread);
    }

    fn record_coordinator_after(&mut self, thread: ExecutionThread) {
        self.coordinator_thread_after = Some(thread);
    }

    fn record_plan_materialization(&mut self, materialization: PlanMaterialization) {
        self.plan_materialization = Some(materialization);
    }

    fn record_registry_inserted(&mut self) {
        self.registry_inserted = true;
    }

    fn rollback_resources(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.registry_inserted {
            if self
                .execution_registry
                .remove_if_turn_matches(&self.task_id, &self.turn_id)
                .is_none()
            {
                errors.push(format!(
                    "执行注册表中未找到属于 Turn {} 的任务 {}",
                    self.turn_id, self.task_id
                ));
            }
            self.registry_inserted = false;
        }
        if let Some(materialization) = self.plan_materialization.take()
            && let Err(error) = self.session_store.restore_plan_if_current(
                &self.session_id,
                materialization.current_revision,
                materialization.original_plan,
            )
        {
            errors.push(format!("计划回滚失败: {error}"));
        }
        if let Some(original) = self.coordinator_thread_before.take() {
            let thread_id = original.thread_id.clone();
            let Some(expected_current) = self.coordinator_thread_after.take() else {
                errors.push(format!(
                    "coordinator thread {} 缺少 materialize 后快照，拒绝回滚",
                    thread_id
                ));
                return errors;
            };
            if let Err(error) = self.session_store.restore_thread_after_materialization(
                &self.session_id,
                &thread_id,
                &expected_current,
                original,
            ) {
                errors.push(format!("coordinator thread 回滚失败: {error}"));
            }
        }
        for expected in self.created_threads.drain(..).rev() {
            if let Err(error) = self.session_store.remove_thread_if_current(
                &self.session_id,
                &expected.thread_id,
                &expected,
            ) {
                errors.push(format!("thread {} 回收失败: {error}", expected.thread_id));
            }
        }
        errors
    }

    fn rollback_now(mut self) -> Vec<String> {
        let errors = self.rollback_resources();
        self.committed = true;
        errors
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for MaterializeRollback {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let errors = self.rollback_resources();
        if !errors.is_empty() {
            tracing::error!(errors = ?errors, "materialize 事务自动回滚不完整");
        }
    }
}

struct MaterializeExecutionAttempt {
    rollback: MaterializeRollback,
}

impl MaterializeExecutionAttempt {
    fn new(
        session_store: &SessionStore,
        execution_registry: &TaskExecutionRegistry,
        session_id: SessionId,
        task_id: TaskId,
        turn_id: String,
    ) -> Self {
        Self {
            rollback: MaterializeRollback::new(
                session_store,
                execution_registry,
                session_id,
                task_id,
                turn_id,
            ),
        }
    }

    fn fail(
        self,
        error: DispatchSubmissionRunError,
    ) -> Result<DispatchSubmissionGraph, DispatchSubmissionRunError> {
        let rollback = self.rollback;
        let rollback_errors = rollback.rollback_now();
        if rollback_errors.is_empty() {
            Err(error)
        } else {
            Err(DispatchSubmissionRunError::Internal(format!(
                "{}；materialize 回滚失败: {}",
                error.into_message(),
                rollback_errors.join("；")
            )))
        }
    }

    fn into_rollback(self) -> MaterializeRollback {
        self.rollback
    }

    fn commit(self) {
        self.rollback.commit();
    }
}

/// Root coordinator Turn 的来源。
///
/// 用户输入和 Goal 自动续跑都使用同一条 ExecutionChain；区别只体现在持久化的
/// 时间线及 canonical item，可见性不能再由另一套 session runner 决定。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispatchTurnOrigin {
    User,
    GoalContinuation(GoalId),
}

impl DispatchTurnOrigin {
    fn timeline_kind(&self) -> TimelineEntryKind {
        match self {
            Self::User => TimelineEntryKind::UserMessage,
            Self::GoalContinuation(_) => TimelineEntryKind::NotificationPublished,
        }
    }

    fn creates_user_message_item(&self) -> bool {
        matches!(self, Self::User)
    }

    fn continuation_goal_id(&self) -> Option<&GoalId> {
        match self {
            Self::User => None,
            Self::GoalContinuation(goal_id) => Some(goal_id),
        }
    }
}

fn default_use_tools() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DispatchSubmissionRequest {
    pub accepted_at: UtcMillis,
    pub session_id: SessionId,
    pub workspace_id: Option<WorkspaceId>,
    /// 接纳时冻结的目录授权；它随 accepted input 持久化，不能由当前项目选择漂移。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_directories: Option<magi_core::ProjectDirectoryBinding>,
    /// 任务执行 cwd。项目会话使用项目根目录；个人会话使用 Magi 管理的私有目录。
    pub execution_root: Option<std::path::PathBuf>,
    /// 新会话首次执行时使用的模型配置。它是 accepted 请求事实的一部分，实际写入
    /// settings store 必须延后到后台 materialize 阶段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator_session_config: Option<serde_json::Value>,
    pub entry_id: String,
    pub timeline_message: String,
    /// 图片只随用户消息条目的 `images` 元数据持久化一份；pendingDispatch 不再重复
    /// 保存 base64，重启恢复时从该元数据重建。
    #[serde(skip)]
    pub images: Vec<SessionTurnImage>,
    pub context_references: Vec<SessionContextReference>,
    /// 已由 Magi API 的 BrowserAuthority 解析并校验的页面标记引用。
    pub browser_annotation_refs: Vec<serde_json::Value>,
    /// 已由 Magi API 严格校验的当前 Browser Surface DOM 节点观察结果。
    pub browser_node_selections: Vec<serde_json::Value>,
    pub created_session: bool,
    pub mission_title: String,
    pub task_title: String,
    pub trimmed_text: Option<String>,
    pub execution_goal: Option<String>,
    pub task_tier: TaskTier,
    #[serde(default)]
    pub access_profile: AccessProfile,
    pub skill_name: Option<String>,
    pub goal_mode: bool,
    /// 是否向本轮模型暴露工具面。普通 Chat Turn 默认关闭，避免把完整工具
    /// Schema 重复发送给只需要文本回答的模型；历史 accepted 请求缺省为 true。
    #[serde(default = "default_use_tools")]
    pub use_tools: bool,
    pub target_role: Option<String>,
    pub request_id: Option<String>,
    pub user_message_id: Option<String>,
    pub placeholder_message_id: Option<String>,
    pub replace_turn_id: Option<String>,
    pub required_tool_chain: Vec<String>,
    pub completion_contract: TaskCompletionContract,
    pub recovery_checkpoint: Option<TaskRecoveryCheckpoint>,
    pub denied_tools: Vec<String>,
    /// 需要随 user message 原子持久化的 API 语义元数据。
    pub user_message_metadata: std::collections::HashMap<String, serde_json::Value>,
    pub turn_origin: DispatchTurnOrigin,
}

#[derive(Clone, Debug)]
pub struct DispatchSubmissionAccepted {
    /// accepted 后后台 materialize 需要的完整请求。请求本身不直接出现在对外事件中，
    /// 但会随 current turn 的 pendingDispatch 元数据进入 accepted journal，供重启恢复。
    pub request: DispatchSubmissionRequest,
    pub session_id: SessionId,
    pub entry_id: String,
    pub accepted_at: UtcMillis,
    pub created_session: bool,
    pub root_task_id: TaskId,
    pub action_task_id: TaskId,
    pub turn_id: String,
    pub user_message_item_id: Option<String>,
    /// accepted 持久化时已经构建出的 canonical turn，供事件和 HTTP 响应直接复用。
    pub accepted_canonical_turn: Option<CanonicalTurn>,
    pub runner_started: bool,
    pub superseded_turn: Option<CanonicalTurn>,
}

/// accepted 前只写入一个 root task 和一条最小 Turn，供直接调用 runtime 的内部入口使用。
pub struct PendingDispatchSubmission {
    pub task: magi_core::Task,
    pub active_execution_chain: ActiveExecutionChain,
    pub turn: ActiveExecutionTurn,
    pub user_message_item_id: Option<String>,
}

/// 从 accepted journal 中恢复后台 materialize 所需的完整请求。
///
/// pending dispatch 是控制面 accepted 事实的一部分，不能依赖进程内的 future 或
/// runner 闭包；重启后唯一可信来源是 current turn item 的持久化元数据。
pub fn recover_dispatch_submission_request(
    sidecar: &SessionRuntimeSidecar,
) -> Result<DispatchSubmissionRequest, String> {
    let item = sidecar
        .current_turn
        .as_ref()
        .and_then(|turn| {
            turn.items
                .iter()
                .find(|item| item.metadata.contains_key("pendingDispatch"))
        })
        .ok_or_else(|| "accepted Turn 缺少 pendingDispatch 请求事实".to_string())?;
    let mut request: DispatchSubmissionRequest =
        serde_json::from_value(item.metadata["pendingDispatch"].clone())
            .map_err(|error| format!("解析 accepted Turn 的 pendingDispatch 请求失败: {error}"))?;
    request.images = crate::session_images::session_turn_images_from_metadata(&item.metadata);
    Ok(request)
}

pub struct DispatchSubmissionRuntime<'a> {
    pub session_store: &'a SessionStore,
    pub task_store: &'a TaskStore,
    pub execution_registry: &'a TaskExecutionRegistry,
    pub event_bus: &'a InMemoryEventBus,
    pub agent_role_registry: &'a AgentRoleRegistry,
    pub model_bridge_client: Option<&'a Arc<dyn ModelBridgeClient>>,
    pub settings_store: Option<&'a Arc<SettingsStore>>,
    pub workspace_root_path: Option<&'a Path>,
}

#[derive(Debug)]
pub enum DispatchSubmissionRunError {
    InvalidInput(String),
    Internal(String),
}

impl DispatchSubmissionRunError {
    pub fn into_message(self) -> String {
        match self {
            Self::InvalidInput(message) | Self::Internal(message) => message,
        }
    }
}

#[derive(Debug)]
pub enum DispatchSubmissionAcceptError {
    Conflict { message: String },
    Internal { message: String },
}

impl DispatchSubmissionAcceptError {
    pub fn from_store_error(error: DomainError) -> Self {
        match error {
            DomainError::CurrentTurnConflict {
                session_id,
                active_turn_id,
            } => Self::Conflict {
                message: format!("会话 {session_id} 已有活动轮次 {active_turn_id}"),
            },
            DomainError::InvalidState { message }
                if message.contains("最近轮次")
                    || message.contains("最后一轮")
                    || message.contains("不是已停止")
                    || message.contains("不是用户主动停止")
                    || message.contains("only an active goal can start continuation")
                    || message.contains("goal continuation is already running") =>
            {
                Self::Conflict { message }
            }
            other => Self::Internal {
                message: other.to_string(),
            },
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Conflict { message } | Self::Internal { message } => message,
        }
    }
}

pub fn ensure_dispatch_submission_acceptance_available(
    session_store: &SessionStore,
    request: &DispatchSubmissionRequest,
) -> Result<(), DispatchSubmissionAcceptError> {
    session_store
        .ensure_current_turn_acceptance_available(&request.session_id)
        .map_err(DispatchSubmissionAcceptError::from_store_error)
}

pub fn cleanup_rejected_dispatch(
    task_store: Option<&TaskStore>,
    execution_registry: &TaskExecutionRegistry,
    mut graph: DispatchSubmissionGraph,
) -> DomainResult<()> {
    if let Some(rollback) = graph.materialize_rollback.take() {
        let rollback_errors = rollback.rollback_now();
        if !rollback_errors.is_empty() {
            return Err(DomainError::InvalidState {
                message: format!("materialize 回滚失败: {}", rollback_errors.join("；")),
            });
        }
    }
    let chain = &graph.active_execution_chain;
    if let Some(turn_id) = chain.current_turn_id.as_deref() {
        for branch in &chain.branches {
            let _ = execution_registry.remove_if_turn_matches(&branch.task_id, turn_id);
        }
    }
    if let Some(task_store) = task_store {
        task_store.remove_task(&graph.root_task_id)?;
    }
    Ok(())
}

/// 清理已 accepted、但 runner 尚未启动就失败的执行资源。
///
/// accepted Turn 和 root task 是用户动作事实，不能删除；这里只按当前 Turn
/// 身份回收执行注册、一次性 worker thread 和计划绑定。主线 coordinator thread
/// 是 session 的长期上下文，只收口为 idle，不会随单个失败任务删除。
pub fn cleanup_materialized_dispatch_submission_if_not_started(
    session_store: &SessionStore,
    execution_registry: &TaskExecutionRegistry,
    session_id: &SessionId,
    task_id: &TaskId,
    turn_id: &str,
) -> DomainResult<()> {
    let sidecar = session_store
        .runtime_sidecar(session_id)
        .ok_or(DomainError::NotFound {
            entity: "session runtime",
        })?;
    let current_turn = sidecar.current_turn.as_ref().ok_or(DomainError::NotFound {
        entity: "current turn",
    })?;
    if current_turn.turn_id != turn_id {
        return Err(DomainError::InvalidState {
            message: format!("当前 Turn 已变化，拒绝清理任务 {} 的执行资源", task_id),
        });
    }
    let chain = sidecar
        .active_execution_chain
        .as_ref()
        .filter(|chain| &chain.root_task_id == task_id)
        .ok_or(DomainError::InvalidState {
            message: format!("当前执行链不属于任务 {}，拒绝清理执行资源", task_id),
        })?;
    let branch = chain
        .branches
        .iter()
        .find(|branch| &branch.task_id == task_id)
        .ok_or(DomainError::InvalidState {
            message: format!("当前执行链缺少任务 {} 的主分支", task_id),
        })?;
    // 清理入口必须幂等：accepted 事实可能在 materialize 已经回滚后才收到
    // 启动失败通知，此时注册表为空，但仍应继续校验并回收其他残留资源。
    let _ = execution_registry.remove_if_turn_matches(task_id, turn_id);
    let now = UtcMillis::now();
    let is_orchestrator = session_store
        .orchestrator_thread_for_session(session_id)
        .is_some_and(|thread| thread.thread_id == branch.thread_id);
    if is_orchestrator {
        session_store.mark_task_threads_idle(task_id, now);
    } else {
        session_store.remove_recovery_thread_if_owned(
            session_id,
            &branch.thread_id,
            &chain.mission_id,
            task_id,
            &branch.worker_id,
        )?;
    }
    let plan_store = magi_plan::PlanStore::from_store(session_store, session_id.clone());
    plan_store
        .unbind_task(task_id)
        .map_err(|error| DomainError::InvalidState {
            message: format!("清理任务 {} 的计划绑定失败: {error}", task_id),
        })?;
    Ok(())
}

/// `allowed_paths` 只承载用户显式限定的范围，派发不写入任何默认值：
/// 非完全访问任务的默认工作区范围由工具权限层按执行根目录推导（只约束写入），
/// 写进策略会被当作显式范围，连带把读取也锁在工作区内，子代理也会原样继承。
/// 上下文引用和浏览器标注产物只登记为只读路径。
fn build_task_policy(
    task_tier: TaskTier,
    access_profile: AccessProfile,
    context_references: &[SessionContextReference],
    browser_annotation_refs: &[serde_json::Value],
    denied_tools: Vec<String>,
) -> magi_core::TaskPolicy {
    let mut read_only_paths = session_context_reference_paths(context_references);
    for artifact_path in browser_annotation_artifact_paths(browser_annotation_refs) {
        if !read_only_paths.contains(&artifact_path) {
            read_only_paths.push(artifact_path);
        }
    }
    magi_core::TaskPolicy {
        autonomy_level: "Autonomous".to_string(),
        access_profile,
        allowed_tools: Vec::new(),
        denied_tools,
        allowed_paths: Vec::new(),
        denied_paths: Vec::new(),
        read_only_paths,
        network_mode: "full".to_string(),
        command_mode: "full".to_string(),
        retry_limit: 1,
        validation_profile: None,
        checkpoint_mode: "turn".to_string(),
        task_tier,
        background_allowed: false,
        escalation_conditions: Vec::new(),
    }
}

struct DispatchTaskInput<'a> {
    task_id: TaskId,
    mission_id: MissionId,
    title: String,
    goal: String,
    now: UtcMillis,
    target_role: &'a str,
    active_skill_id: Option<&'a str>,
    task_tier: TaskTier,
    access_profile: AccessProfile,
    context_references: &'a [SessionContextReference],
    required_tool_chain: Vec<String>,
    goal_mode: bool,
    completion_contract: TaskCompletionContract,
    recovery_checkpoint: Option<TaskRecoveryCheckpoint>,
    denied_tools: Vec<String>,
    plan_item_id: Option<PlanItemId>,
    browser_annotation_refs: &'a [serde_json::Value],
    browser_node_selections: &'a [serde_json::Value],
}

fn make_dispatch_task(input: DispatchTaskInput<'_>) -> magi_core::Task {
    let DispatchTaskInput {
        task_id,
        mission_id,
        title,
        goal,
        now,
        target_role,
        active_skill_id,
        task_tier,
        access_profile,
        context_references,
        required_tool_chain,
        goal_mode,
        completion_contract,
        recovery_checkpoint,
        denied_tools,
        plan_item_id,
        browser_annotation_refs,
        browser_node_selections,
    } = input;
    let executor_binding = TaskExecutorBinding::for_role(target_role)
        .with_active_skill_id(active_skill_id.map(str::to_string))
        .with_required_tool_chain(required_tool_chain)
        .with_goal_mode(goal_mode)
        .with_plan_item_id(plan_item_id);
    let mut input_refs = session_context_reference_input_refs(context_references);
    input_refs.extend(browser_annotation_reference_input_refs(
        browser_annotation_refs,
    ));
    input_refs.extend(browser_node_selection_input_refs(browser_node_selections));

    magi_core::Task {
        task_id: task_id.clone(),
        mission_id,
        root_task_id: task_id,
        parent_task_id: None,
        kind: TaskKind::LocalAgent,
        title,
        goal,
        status: TaskStatus::Pending,
        dependency_ids: Vec::new(),
        required_children: Vec::new(),
        policy_snapshot: Some(build_task_policy(
            task_tier,
            access_profile,
            context_references,
            browser_annotation_refs,
            denied_tools,
        )),
        executor_binding: Some(executor_binding),
        completion_contract,
        recovery_checkpoint,
        knowledge_refs: Vec::new(),
        workspace_scope: None,
        write_scope: None,
        input_refs,
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        runtime_payload: if browser_annotation_refs.is_empty() && browser_node_selections.is_empty()
        {
            magi_core::TaskRuntimePayload::None
        } else {
            magi_core::TaskRuntimePayload::ContextAnchors {
                references: browser_annotation_refs.to_vec(),
                node_selections: browser_node_selections.to_vec(),
            }
        },
        created_at: now,
        updated_at: now,
    }
}

struct PreparedDispatchSubmission {
    task: magi_core::Task,
    mission_id: MissionId,
    orchestrator_thread_id: ThreadId,
    worker_id: WorkerId,
    worker_thread_id: ThreadId,
    action_task_id: TaskId,
    turn_id: String,
    execution_chain_ref: String,
    plan_item_id: Option<PlanItemId>,
    target_role: String,
    now: UtcMillis,
}

fn map_acceptance_error(error: DispatchSubmissionAcceptError) -> DispatchSubmissionRunError {
    match error {
        DispatchSubmissionAcceptError::Conflict { message }
        | DispatchSubmissionAcceptError::Internal { message } => {
            DispatchSubmissionRunError::Internal(message)
        }
    }
}

fn resolve_session_mission(
    session_store: &SessionStore,
    session_id: &SessionId,
    accepted_at: UtcMillis,
) -> (MissionId, ThreadId) {
    if let Some(thread) = session_store.orchestrator_thread_for_session(session_id) {
        return (thread.mission_id, thread.thread_id);
    }
    if let Some(mission_id) = session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.ownership.mission_id)
    {
        return (
            mission_id,
            ThreadId::new(format!("thread-orchestrator-{session_id}")),
        );
    }
    (
        MissionId::new(format!("mission-session-action-{}", accepted_at.0)),
        ThreadId::new(format!("thread-orchestrator-{session_id}")),
    )
}

/// 根据 accepted 时间和来源生成稳定的 Task Turn 身份。
///
/// 该身份在 Coordinator、accepted journal、恢复请求和执行注册表之间共享；不能在
/// 任一层重新随机生成，否则迟到回调无法证明自己属于原始 Turn。
pub fn dispatch_turn_id(request: &DispatchSubmissionRequest) -> String {
    match &request.turn_origin {
        DispatchTurnOrigin::User => format!("turn-session-action-{}", request.accepted_at.0),
        DispatchTurnOrigin::GoalContinuation(_) => {
            format!(
                "turn-goal-continuation-{}-{}",
                request.session_id, request.accepted_at.0
            )
        }
    }
}

fn validate_dispatch_request(
    runtime: &DispatchSubmissionRuntime<'_>,
    request: &DispatchSubmissionRequest,
    check_current_turn: bool,
) -> Result<PreparedDispatchSubmission, DispatchSubmissionRunError> {
    if check_current_turn {
        ensure_dispatch_submission_acceptance_available(runtime.session_store, request)
            .map_err(map_acceptance_error)?;
    }
    let execution_goal = request
        .execution_goal
        .as_deref()
        .map(str::trim)
        .filter(|goal| !goal.is_empty())
        .ok_or_else(|| {
            DispatchSubmissionRunError::InvalidInput(
                "任务派发必须提供非空 execution_goal".to_string(),
            )
        })?;
    if let Some(checkpoint) = request.recovery_checkpoint.as_ref() {
        validate_interrupted_turn_checkpoint(runtime.session_store, checkpoint)?;
    }
    // Skill 是本轮方法上下文，不是角色路由信号。主线任务必须保留 coordinator 权限面。
    let target_role = request.target_role.as_deref().unwrap_or("coordinator");
    if request.goal_mode && target_role != "coordinator" {
        return Err(DispatchSubmissionRunError::InvalidInput(
            "目标模式只能绑定主线 coordinator，不能下发到 worker 或 sidechain".to_string(),
        ));
    }
    let task_kind = TaskKind::LocalAgent;
    if !runtime
        .agent_role_registry
        .role_supports_task_kind(target_role, task_kind)
    {
        return Err(DispatchSubmissionRunError::InvalidInput(format!(
            "role {target_role} 不支持 {task_kind:?} 任务"
        )));
    }

    let accepted_at = request.accepted_at;
    let (mission_id, orchestrator_thread_id) =
        resolve_session_mission(runtime.session_store, &request.session_id, accepted_at);
    let worker_id = WorkerId::new(format!("worker-session-action-{}", accepted_at.0));
    let action_task_id = TaskId::new(format!("task-local-agent-{}", accepted_at.0));
    let plan_item_id =
        magi_plan::PlanStore::from_store(runtime.session_store, request.session_id.clone())
            .active_item_id();
    let now = UtcMillis::now();
    let mut task = make_dispatch_task(DispatchTaskInput {
        task_id: action_task_id.clone(),
        mission_id: mission_id.clone(),
        title: request.task_title.clone(),
        goal: execution_goal.to_string(),
        now,
        target_role,
        active_skill_id: request.skill_name.as_deref(),
        task_tier: request.task_tier,
        access_profile: request.access_profile,
        context_references: &request.context_references,
        browser_annotation_refs: &request.browser_annotation_refs,
        browser_node_selections: &request.browser_node_selections,
        required_tool_chain: request.required_tool_chain.clone(),
        goal_mode: request.goal_mode,
        completion_contract: request.completion_contract.clone(),
        recovery_checkpoint: request.recovery_checkpoint.clone(),
        denied_tools: request.denied_tools.clone(),
        plan_item_id: plan_item_id.clone(),
    });
    if let Some(binding) = task.executor_binding.as_mut() {
        binding.project_directories = request.project_directories.clone();
    }
    let worker_thread_id = if target_role == "coordinator" && request.recovery_checkpoint.is_none()
    {
        orchestrator_thread_id.clone()
    } else {
        ThreadId::new(format!(
            "thread-{target_role}-{}-{}",
            action_task_id, accepted_at.0
        ))
    };
    let turn_id = dispatch_turn_id(request);
    Ok(PreparedDispatchSubmission {
        task,
        mission_id,
        orchestrator_thread_id,
        worker_id,
        worker_thread_id,
        action_task_id,
        turn_id,
        execution_chain_ref: format!("session-action-chain-{}", accepted_at.0),
        plan_item_id,
        target_role: target_role.to_string(),
        now,
    })
}

fn build_active_execution_chain(
    prepared: &PreparedDispatchSubmission,
    request: &DispatchSubmissionRequest,
) -> Result<(ActiveExecutionChain, ActiveExecutionTurn, Option<String>), DispatchSubmissionRunError>
{
    let branch = ActiveExecutionBranch {
        task_id: prepared.action_task_id.clone(),
        worker_id: prepared.worker_id.clone(),
        use_tools: request.use_tools,
        skill_name: request.skill_name.clone(),
        is_primary: true,
        thread_id: prepared.worker_thread_id.clone(),
    };
    let user_message_id = request.user_message_id.clone();
    let user_message_item_id = request.turn_origin.creates_user_message_item().then(|| {
        user_message_id
            .clone()
            .unwrap_or_else(|| format!("turn-item-user-{}", request.accepted_at.0))
    });
    let pending_dispatch = serde_json::to_value(request).map_err(|error| {
        DispatchSubmissionRunError::Internal(format!("保存 pending dispatch 请求失败: {error}"))
    })?;
    let mut current_turn = ActiveExecutionTurn {
        turn_id: prepared.turn_id.clone(),
        turn_seq: request.accepted_at.0,
        accepted_at: request.accepted_at,
        status: "accepted".to_string(),
        completed_at: None,
        user_message: request
            .turn_origin
            .creates_user_message_item()
            .then(|| request.timeline_message.clone()),
        items: user_message_item_id
            .clone()
            .into_iter()
            .map(|item_id| {
                let mut metadata =
                    crate::session_images::session_turn_images_metadata(&request.images);
                metadata.extend(session_context_references_metadata(
                    &request.context_references,
                ));
                metadata.extend(browser_annotation_references_metadata(
                    &request.browser_annotation_refs,
                ));
                metadata.extend(browser_node_selections_metadata(
                    &request.browser_node_selections,
                ));
                if let Some(replace_turn_id) = request.replace_turn_id.as_ref() {
                    metadata.insert(
                        "replacesTurnId".to_string(),
                        serde_json::Value::String(replace_turn_id.clone()),
                    );
                }
                if let Some(skill_name) = request.skill_name.as_deref() {
                    metadata.insert(
                        "skillName".to_string(),
                        serde_json::Value::String(skill_name.to_string()),
                    );
                }
                metadata.extend(request.user_message_metadata.clone());
                metadata.insert(
                    "goalMode".to_string(),
                    serde_json::Value::Bool(request.goal_mode),
                );
                metadata.insert("pendingDispatch".to_string(), pending_dispatch.clone());
                Ok(ActiveExecutionTurnItem {
                    item_id,
                    item_seq: 1,
                    kind: "user_message".to_string(),
                    status: "completed".to_string(),
                    source: "user".to_string(),
                    title: None,
                    content: Some(request.timeline_message.clone()),
                    task_id: Some(prepared.action_task_id.clone()),
                    worker_id: None,
                    role_id: None,
                    tool_call_id: None,
                    tool_name: None,
                    tool_status: None,
                    tool_arguments: None,
                    tool_result: None,
                    tool_error: None,
                    request_id: request.request_id.clone(),
                    user_message_id: user_message_id.clone(),
                    placeholder_message_id: request.placeholder_message_id.clone(),
                    metadata,
                    timeline_entry_id: Some(request.entry_id.clone()),
                    source_thread_id: prepared.orchestrator_thread_id.clone(),
                })
            })
            .collect::<Result<Vec<_>, DispatchSubmissionRunError>>()?,
    };
    if !request.turn_origin.creates_user_message_item() {
        let mut metadata = request.user_message_metadata.clone();
        metadata.insert("pendingDispatch".to_string(), pending_dispatch);
        metadata.insert("renderable".to_string(), serde_json::Value::Bool(false));
        current_turn.items.push(ActiveExecutionTurnItem {
            item_id: format!("turn-item-dispatch-{}", request.accepted_at.0),
            item_seq: 1,
            kind: "assistant_phase".to_string(),
            status: "completed".to_string(),
            source: "system".to_string(),
            title: None,
            content: None,
            task_id: Some(prepared.action_task_id.clone()),
            worker_id: None,
            role_id: None,
            tool_call_id: None,
            tool_name: None,
            tool_status: None,
            tool_arguments: None,
            tool_result: None,
            tool_error: None,
            request_id: request.request_id.clone(),
            user_message_id: None,
            placeholder_message_id: None,
            metadata,
            timeline_entry_id: Some(request.entry_id.clone()),
            source_thread_id: prepared.orchestrator_thread_id.clone(),
        });
    }
    current_turn.normalize();
    let branches = vec![branch];
    Ok((
        ActiveExecutionChain {
            session_id: request.session_id.clone(),
            mission_id: prepared.mission_id.clone(),
            root_task_id: prepared.action_task_id.clone(),
            execution_chain_ref: prepared.execution_chain_ref.clone(),
            workspace_id: request.workspace_id.clone(),
            branches,
            dispatch_context: ActiveExecutionDispatchContext {
                accepted_at: request.accepted_at,
                entry_id: request.entry_id.clone(),
                trimmed_text: request.trimmed_text.clone(),
                skill_name: request.skill_name.clone(),
            },
            current_turn_id: Some(current_turn.turn_id.clone()),
        },
        current_turn,
        user_message_item_id,
    ))
}

fn bind_dispatch_task_to_plan(
    runtime: &DispatchSubmissionRuntime<'_>,
    session_id: &SessionId,
    prepared: &PreparedDispatchSubmission,
) -> Result<Option<PlanMaterialization>, DispatchSubmissionRunError> {
    let Some(plan_item_id) = prepared.plan_item_id.clone() else {
        return Ok(None);
    };
    let plan_store = magi_plan::PlanStore::from_store(runtime.session_store, session_id.clone());
    plan_store
        .bind_task_for_materialization(prepared.action_task_id.clone(), plan_item_id)
        .map(|mutation| {
            mutation.map(|(original_plan, updated_plan)| PlanMaterialization {
                original_plan: Some(original_plan),
                current_revision: updated_plan.revision,
            })
        })
        .map_err(|error| {
            DispatchSubmissionRunError::Internal(format!(
                "将主线任务绑定到当前计划阶段失败: {error}"
            ))
        })
}

fn materialize_execution(
    runtime: &DispatchSubmissionRuntime<'_>,
    request: &DispatchSubmissionRequest,
    prepared: &PreparedDispatchSubmission,
    pending_chain: Option<ActiveExecutionChain>,
) -> Result<DispatchSubmissionGraph, DispatchSubmissionRunError> {
    let accepted_materialize = pending_chain.is_some();
    let (active_execution_chain, turn) = match pending_chain {
        Some(chain) => {
            let turn = runtime
                .session_store
                .runtime_sidecar(&request.session_id)
                .and_then(|sidecar| sidecar.current_turn)
                .ok_or_else(|| {
                    DispatchSubmissionRunError::Internal("已接纳任务缺少当前 Turn".into())
                })?;
            (chain, turn)
        }
        None => {
            let (chain, turn, _) = build_active_execution_chain(prepared, request)?;
            (chain, turn)
        }
    };
    let mut attempt = MaterializeExecutionAttempt::new(
        runtime.session_store,
        runtime.execution_registry,
        request.session_id.clone(),
        prepared.action_task_id.clone(),
        prepared.turn_id.clone(),
    );
    let (mission_id, orchestrator_thread_id, coordinator_created) = runtime
        .session_store
        .ensure_session_mission_with_created(&request.session_id, request.accepted_at, || {
            prepared.mission_id.clone()
        });
    if coordinator_created {
        let coordinator = runtime
            .session_store
            .thread_registry_snapshot(&request.session_id)
            .into_iter()
            .find(|thread| thread.thread_id == orchestrator_thread_id)
            .ok_or_else(|| {
                DispatchSubmissionRunError::Internal(
                    "创建 coordinator thread 后无法读取其快照".to_string(),
                )
            });
        let coordinator = match coordinator {
            Ok(coordinator) => coordinator,
            Err(error) => return attempt.fail(error),
        };
        attempt.rollback.record_created_thread(coordinator);
    }
    if mission_id != prepared.mission_id
        || orchestrator_thread_id != prepared.orchestrator_thread_id
    {
        return attempt.fail(DispatchSubmissionRunError::Internal(
            "materialize 后 session mission/thread 身份发生变化".to_string(),
        ));
    }
    let worker_thread_id =
        if prepared.target_role == "coordinator" && request.recovery_checkpoint.is_none() {
            prepared.orchestrator_thread_id.clone()
        } else {
            let new_thread = session_thread::build_thread_for_role(
                &request.session_id,
                &prepared.mission_id,
                &prepared.target_role,
                &prepared.worker_id,
                &prepared.action_task_id,
                request.accepted_at,
            );
            let thread_id = new_thread.thread_id.clone();
            if let Err(error) = runtime.session_store.register_thread(new_thread.clone()) {
                return attempt.fail(DispatchSubmissionRunError::Internal(format!(
                    "为任务 {} 注册执行 thread 失败: {error}",
                    prepared.action_task_id
                )));
            }
            attempt.rollback.record_created_thread(new_thread);
            thread_id
        };
    if worker_thread_id != prepared.worker_thread_id {
        return attempt.fail(DispatchSubmissionRunError::Internal(
            "materialize 后 worker thread 身份发生变化".to_string(),
        ));
    }
    let coordinator_reused = worker_thread_id == orchestrator_thread_id && !coordinator_created;
    if coordinator_reused {
        let coordinator = runtime
            .session_store
            .thread_registry_snapshot(&request.session_id)
            .into_iter()
            .find(|thread| thread.thread_id == orchestrator_thread_id)
            .or_else(|| {
                runtime
                    .session_store
                    .orchestrator_thread_for_session(&request.session_id)
            });
        let Some(coordinator) = coordinator else {
            return attempt.fail(DispatchSubmissionRunError::Internal(
                "激活 coordinator thread 前无法读取其原始快照".to_string(),
            ));
        };
        attempt.rollback.record_coordinator_before(coordinator);
    }
    let activated_thread = match runtime.session_store.activate_thread_checked(
        &worker_thread_id,
        &prepared.action_task_id,
        prepared.now,
    ) {
        Ok(thread) => thread,
        Err(error) => {
            return attempt.fail(DispatchSubmissionRunError::Internal(format!(
                "激活任务 {} 的执行 thread 失败: {error}",
                prepared.action_task_id
            )));
        }
    };
    if coordinator_reused {
        attempt.rollback.record_coordinator_after(activated_thread);
    }
    if let Some(checkpoint) = request.recovery_checkpoint.as_ref()
        && let Err(error) = restore_interrupted_turn_context(
            runtime.session_store,
            &checkpoint.source_thread_id,
            &worker_thread_id,
            UtcMillis::now(),
        )
    {
        return attempt.fail(error);
    }
    if let Some(created_thread) = attempt
        .rollback
        .created_threads
        .iter()
        .find(|thread| thread.thread_id == worker_thread_id)
    {
        let thread_id = created_thread.thread_id.clone();
        if let Err(error) = attempt.rollback.refresh_created_thread(&thread_id) {
            return attempt.fail(DispatchSubmissionRunError::Internal(error));
        }
    }
    let plan_materialization =
        match bind_dispatch_task_to_plan(runtime, &request.session_id, prepared) {
            Ok(materialization) => materialization,
            Err(error) => return attempt.fail(error),
        };
    if let Some(materialization) = plan_materialization {
        attempt
            .rollback
            .record_plan_materialization(materialization);
    }
    let execution_plan = TaskExecutionPlan::Dispatch {
        target: TaskExecutionTarget {
            mission_id: prepared.mission_id.clone(),
            root_task_id: prepared.action_task_id.clone(),
            task_id: prepared.action_task_id.clone(),
            requested_worker_id: Some(prepared.worker_id.clone()),
            recovery_id: None,
            execution_chain_ref: Some(prepared.execution_chain_ref.clone()),
        },
        worker_id: prepared.worker_id.clone(),
        thread_id: worker_thread_id,
        is_primary: true,
        session_id: request.session_id.clone(),
        turn_id: prepared.turn_id.clone(),
        workspace_id: request.workspace_id.clone(),
        execution_root: request.execution_root.clone(),
        // 用户命令的正文是业务参数，不触发聊天记忆提取或隐式辅助模型请求。
        writebacks: if request
            .user_message_metadata
            .contains_key("pluginCommandId")
            || request.user_message_metadata.contains_key("pluginMessage")
        {
            ExecutionWritebackPlans::default()
        } else {
            ExecutionWritebackPlans::from_session_action_input(DispatchMemoryExtractionInput {
                accepted_at: request.accepted_at,
                session_id: &request.session_id,
                timeline_entry_id: &request.entry_id,
                text: request.trimmed_text.as_deref(),
                skill_name: request.skill_name.as_deref(),
            })
        },
        use_tools: request.use_tools,
        skill_name: request.skill_name.clone(),
        images: request.images.clone(),
        execution_settings_snapshot: runtime
            .settings_store
            .map(|store| Arc::new(store.execution_snapshot())),
    };
    if runtime
        .execution_registry
        .insert(prepared.action_task_id.clone(), execution_plan)
        .is_err()
    {
        return attempt.fail(DispatchSubmissionRunError::Internal(format!(
            "主线任务 {} 已存在执行计划，拒绝重复注册",
            prepared.action_task_id
        )));
    }
    attempt.rollback.record_registry_inserted();
    if accepted_materialize
        && let Err(error) = runtime
            .session_store
            .validate_execution_chain(&request.session_id, &active_execution_chain)
    {
        return attempt.fail(DispatchSubmissionRunError::Internal(format!(
            "校验 materialize execution chain 失败: {error}"
        )));
    }
    let materialize_rollback = if accepted_materialize {
        attempt.commit();
        None
    } else {
        Some(attempt.into_rollback())
    };
    let event = task_events::task_submission_created_event(
        prepared.mission_id.as_str(),
        prepared.action_task_id.as_str(),
        1,
    )
    .with_context(EventContext {
        mission_id: Some(prepared.mission_id.clone()),
        task_id: Some(prepared.action_task_id.clone()),
        ..EventContext::default()
    });
    let _ = runtime.event_bus.publish(event);
    Ok(DispatchSubmissionGraph {
        root_task_id: prepared.action_task_id.clone(),
        action_task_id: prepared.action_task_id.clone(),
        active_execution_chain,
        turn,
        materialize_rollback,
    })
}

/// accepted 前只写入一个 root task 和一条带原始请求的最小 Turn。
pub fn prepare_pending_dispatch_submission(
    runtime: &DispatchSubmissionRuntime<'_>,
    request: &DispatchSubmissionRequest,
) -> Result<PendingDispatchSubmission, DispatchSubmissionRunError> {
    let prepared = validate_dispatch_request(runtime, request, true)?;
    let (active_execution_chain, mut turn, user_message_item_id) =
        build_active_execution_chain(&prepared, request)?;
    // 不唤醒的插件上下文只在下一次真实模型 Task 接纳时领取。用户输入与这些
    // 消息在同一 canonical 事务建立 Input 绑定；失败不领取，重启不重放。
    if !request
        .user_message_metadata
        .contains_key("pluginCommandId")
    {
        for input in runtime.session_store.inbox_snapshot(&request.session_id).0 {
            let Some(origin) = input.payload.get("plugin_message").filter(|origin| {
                origin.get("delivery").and_then(|v| v.as_str()) == Some("context")
            }) else {
                continue;
            };
            let origin: magi_app_server_protocol::generated::PluginSessionMessageOrigin =
                serde_json::from_value(origin.clone()).map_err(|error| {
                    DispatchSubmissionRunError::InvalidInput(format!("插件消息来源损坏：{error}"))
                })?;
            let text = input
                .payload
                .pointer("/request/text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    DispatchSubmissionRunError::InvalidInput("插件消息缺少正文".into())
                })?;
            let mut item = crate::session_writeback::session_turn_item(
                "user_message",
                "completed",
                None,
                Some(text.into()),
                Some(input.user_message_id.clone()),
                prepared.orchestrator_thread_id.clone(),
            );
            item.item_seq = turn.items.len() + 1;
            item.request_id = Some(input.request_id.clone());
            item.user_message_id = Some(input.user_message_id);
            item.task_id = Some(prepared.action_task_id.clone());
            item.metadata.extend(std::collections::HashMap::from([
                (
                    "pluginMessage".into(),
                    serde_json::to_value(origin).unwrap(),
                ),
                (
                    "requestFingerprint".into(),
                    serde_json::Value::String(input.request_fingerprint),
                ),
            ]));
            turn.items.push(item);
        }
    }
    runtime
        .task_store
        .insert_task_without_checkpoint(prepared.task.clone())
        .map_err(|error| {
            DispatchSubmissionRunError::Internal(format!("写入 pending root task 失败: {error}"))
        })?;
    Ok(PendingDispatchSubmission {
        task: prepared.task,
        active_execution_chain,
        turn,
        user_message_item_id,
    })
}

/// 测试专用的完整 dispatch 装配入口。
///
/// 生产 API 使用 `prepare_pending_dispatch_submission` 与
/// `materialize_dispatch_submission` 分阶段接纳，避免保留第二条提交路径。
#[cfg(test)]
fn run_dispatch_submission_for_test(
    runtime: &DispatchSubmissionRuntime<'_>,
    request: &DispatchSubmissionRequest,
) -> Result<DispatchSubmissionGraph, DispatchSubmissionRunError> {
    let prepared = validate_dispatch_request(runtime, request, true)?;
    runtime
        .task_store
        .insert_task_without_checkpoint(prepared.task.clone())
        .map_err(|error| {
            DispatchSubmissionRunError::Internal(format!("写入 root task 失败: {error}"))
        })?;
    match materialize_execution(runtime, request, &prepared, None) {
        Ok(graph) => Ok(graph),
        Err(error) => {
            let error_message = error.into_message();
            if let Err(cleanup_error) = runtime.task_store.remove_task(&prepared.action_task_id) {
                return Err(DispatchSubmissionRunError::Internal(format!(
                    "{}；清理 root task 失败: {}",
                    error_message, cleanup_error
                )));
            }
            Err(DispatchSubmissionRunError::Internal(error_message))
        }
    }
}

/// accepted 后构建 Thread、执行计划和模型设置快照；不会重新写 canonical Turn 或 timeline。
pub fn materialize_dispatch_submission(
    runtime: &DispatchSubmissionRuntime<'_>,
    request: &DispatchSubmissionRequest,
    expected_turn_id: &str,
) -> Result<(), DispatchSubmissionRunError> {
    let sidecar = runtime
        .session_store
        .runtime_sidecar(&request.session_id)
        .ok_or_else(|| {
            DispatchSubmissionRunError::Internal("accepted Turn 缺少 runtime sidecar".to_string())
        })?;
    let current_turn = sidecar.current_turn.as_ref().ok_or_else(|| {
        DispatchSubmissionRunError::Internal("accepted Turn 缺少 current_turn".to_string())
    })?;
    // Sidecar 从 canonical 投影读取，Preparing 已归一为 Running；
    // 下方 Pending root task 校验才决定是否尚未开始执行。
    if current_turn.turn_id != expected_turn_id
        || !matches!(current_turn.status.as_str(), "accepted" | "running")
    {
        return Err(DispatchSubmissionRunError::InvalidInput(
            "accepted Turn 已被终态或新的 Turn 取代".to_string(),
        ));
    }
    let pending_chain = sidecar.active_execution_chain.clone().ok_or_else(|| {
        DispatchSubmissionRunError::Internal(
            "accepted Turn 缺少 active execution chain".to_string(),
        )
    })?;
    let prepared = validate_dispatch_request(runtime, request, false)?;
    if pending_chain.root_task_id != prepared.action_task_id
        || pending_chain.session_id != request.session_id
        || pending_chain.execution_chain_ref != prepared.execution_chain_ref
    {
        return Err(DispatchSubmissionRunError::Internal(
            "accepted active execution chain 与请求身份不一致".to_string(),
        ));
    }
    let task = runtime
        .task_store
        .get_task(&prepared.action_task_id)
        .ok_or_else(|| {
            DispatchSubmissionRunError::Internal("accepted root task 不存在".to_string())
        })?;
    if task.status != TaskStatus::Pending
        || task.mission_id != prepared.task.mission_id
        || task.root_task_id != prepared.task.root_task_id
        || task.goal != prepared.task.goal
    {
        return Err(DispatchSubmissionRunError::InvalidInput(
            "accepted root task 已被修改或不再可执行".to_string(),
        ));
    }
    // accepted journal 与 TaskStore 是同一接纳事实的两份持久投影。恢复时不能只
    // 比对 goal：篡改 pendingDispatch 的工作流身份或包摘要会让旧任务被错误地
    // 当作新实现装配。工作流摘要尤其决定恢复时允许加载哪个不可变插件包。
    if task.runtime_payload != prepared.task.runtime_payload {
        return Err(DispatchSubmissionRunError::InvalidInput(
            "accepted root task 的运行负载与持久化请求不一致，拒绝恢复".to_string(),
        ));
    }
    if let Some(existing_plan) = runtime.execution_registry.get(&prepared.action_task_id) {
        let TaskExecutionPlan::Dispatch {
            target,
            worker_id,
            thread_id,
            session_id,
            turn_id,
            ..
        } = existing_plan;
        let identity_matches = turn_id == expected_turn_id
            && session_id == request.session_id
            && target.mission_id == prepared.mission_id
            && target.root_task_id == prepared.action_task_id
            && target.task_id == prepared.action_task_id
            && target.requested_worker_id.as_ref() == Some(&prepared.worker_id)
            && target.execution_chain_ref.as_deref() == Some(prepared.execution_chain_ref.as_str())
            && worker_id == prepared.worker_id
            && thread_id == prepared.worker_thread_id;
        if identity_matches {
            return Ok(());
        }
        return Err(DispatchSubmissionRunError::InvalidInput(format!(
            "accepted root task 已被其他执行身份装配：期望 Turn {expected_turn_id}，当前 Turn {turn_id}"
        )));
    }
    materialize_execution(runtime, request, &prepared, Some(pending_chain))?;
    Ok(())
}

fn validate_interrupted_turn_checkpoint(
    session_store: &SessionStore,
    checkpoint: &TaskRecoveryCheckpoint,
) -> Result<(), DispatchSubmissionRunError> {
    let source_turn = session_store
        .canonical_turns_for_session(&checkpoint.source_session_id)
        .into_iter()
        .find(|turn| turn.turn_id == checkpoint.source_turn_id)
        .ok_or_else(|| {
            DispatchSubmissionRunError::InvalidInput(format!(
                "续接来源 Turn 不存在: {}",
                checkpoint.source_turn_id
            ))
        })?;
    if source_turn.status != magi_session_store::CanonicalTurnStatus::Cancelled {
        return Err(DispatchSubmissionRunError::InvalidInput(format!(
            "续接来源 Turn 不是用户中断状态: {}",
            checkpoint.source_turn_id
        )));
    }
    let interrupted_by_user = source_turn.items.iter().any(|item| {
        item.kind == CanonicalTurnItemKind::UserMessage
            && item
                .metadata
                .get("interruptionSource")
                .and_then(serde_json::Value::as_str)
                == Some("user")
    });
    if !interrupted_by_user {
        return Err(DispatchSubmissionRunError::InvalidInput(format!(
            "续接来源 Turn 不是用户主动中断: {}",
            checkpoint.source_turn_id
        )));
    }
    let source_thread = session_store
        .thread_registry_snapshot(&checkpoint.source_session_id)
        .into_iter()
        .find(|thread| thread.thread_id == checkpoint.source_thread_id)
        .ok_or_else(|| {
            DispatchSubmissionRunError::InvalidInput(format!(
                "续接来源任务缺少持久化 Thread: {}",
                checkpoint.source_task_id
            ))
        })?;
    if !source_thread
        .handled_task_ids
        .contains(&checkpoint.source_task_id)
    {
        return Err(DispatchSubmissionRunError::InvalidInput(format!(
            "续接来源 Thread 与任务不匹配: {}",
            checkpoint.source_task_id
        )));
    }
    if !source_turn.items.iter().any(|item| {
        item.worker
            .as_ref()
            .and_then(|worker| worker.task_id.as_ref())
            == Some(&checkpoint.source_task_id)
    }) {
        return Err(DispatchSubmissionRunError::InvalidInput(format!(
            "续接来源 Turn 与任务不匹配: {}",
            checkpoint.source_task_id
        )));
    }
    Ok(())
}

fn restore_interrupted_turn_context(
    store: &SessionStore,
    source_thread_id: &ThreadId,
    destination_thread_id: &ThreadId,
    now: UtcMillis,
) -> Result<(), DispatchSubmissionRunError> {
    let source = store
        .thread_context_snapshot(source_thread_id)
        .map_err(|error| {
            DispatchSubmissionRunError::Internal(format!("读取恢复来源快照失败: {error}"))
        })?;
    let destination = store
        .thread_context_snapshot(destination_thread_id)
        .map_err(|error| {
            DispatchSubmissionRunError::Internal(format!("读取恢复目标快照失败: {error}"))
        })?;
    store
        .inherit_thread_context_checkpoint(&source, &destination, now)
        .map_err(|error| {
            DispatchSubmissionRunError::Internal(format!("安装恢复上下文检查点失败: {error}"))
        })
}

pub fn accept_dispatch_submission(
    session_store: &SessionStore,
    task_store: &TaskStore,
    execution_registry: &TaskExecutionRegistry,
    request: DispatchSubmissionRequest,
    mut graph: DispatchSubmissionGraph,
) -> Result<DispatchSubmissionAccepted, DispatchSubmissionAcceptError> {
    let acceptance_task = task_store.get_task(&graph.root_task_id).ok_or_else(|| {
        DispatchSubmissionAcceptError::Internal {
            message: format!(
                "接纳任务派发前缺少 root task {}，拒绝提交不完整的 accepted 事实",
                graph.root_task_id
            ),
        }
    })?;
    let turn_id = graph.turn.turn_id.clone();
    let active_execution_chain = graph.active_execution_chain.clone();
    let accept_result = if let Some(goal_id) = request.turn_origin.continuation_goal_id() {
        CanonicalTurnEventSink::for_store(session_store, Some(task_store))
            .accept_goal_continuation_with_timeline_entry_and_task(
                request.session_id.clone(),
                goal_id,
                TimelineEntryInput::new(
                    request.entry_id.clone(),
                    request.turn_origin.timeline_kind(),
                    request.timeline_message.clone(),
                    request.accepted_at,
                ),
                active_execution_chain,
                graph.turn.clone(),
                &acceptance_task,
            )
            .map(|(_, _, canonical_turn)| (None, Some(canonical_turn)))
    } else if let Some(replace_turn_id) = request.replace_turn_id.as_deref() {
        CanonicalTurnEventSink::for_store(session_store, Some(task_store))
            .replace_current_turn_with_active_execution_chain_and_timeline_entry_and_task(
                request.session_id.clone(),
                replace_turn_id,
                TimelineEntryInput::new(
                    request.entry_id.clone(),
                    request.turn_origin.timeline_kind(),
                    request.timeline_message.clone(),
                    request.accepted_at,
                ),
                active_execution_chain,
                graph.turn.clone(),
                &acceptance_task,
            )
            .map(|(_, _, superseded_turn, canonical_turn)| {
                (Some(superseded_turn), Some(canonical_turn))
            })
    } else {
        CanonicalTurnEventSink::for_store(session_store, Some(task_store))
            .accept_active_execution_chain_with_timeline_entry_and_task(
                request.session_id.clone(),
                TimelineEntryInput::new(
                    request.entry_id.clone(),
                    request.turn_origin.timeline_kind(),
                    request.timeline_message.clone(),
                    request.accepted_at,
                ),
                active_execution_chain,
                graph.turn.clone(),
                &acceptance_task,
            )
            .map(|(_, _, canonical_turn)| (None, Some(canonical_turn)))
    };
    let (superseded_turn, accepted_canonical_turn) = match accept_result {
        Ok(result) => result,
        Err(error) => {
            if let Err(checkpoint_error) =
                cleanup_rejected_dispatch(Some(task_store), execution_registry, graph)
            {
                return Err(DispatchSubmissionAcceptError::Internal {
                    message: format!(
                        "接纳派发失败: {error}；回收任务 checkpoint 失败: {checkpoint_error}"
                    ),
                });
            }
            return Err(DispatchSubmissionAcceptError::from_store_error(error));
        }
    };

    if let Some(rollback) = graph.materialize_rollback.take() {
        rollback.commit();
    }

    let user_message_item_id = request.turn_origin.creates_user_message_item().then(|| {
        request
            .user_message_id
            .clone()
            .unwrap_or_else(|| format!("turn-item-user-{}", request.accepted_at.0))
    });

    let accepted_session_id = request.session_id.clone();
    let accepted_entry_id = request.entry_id.clone();
    let accepted_at = request.accepted_at;
    let created_session = request.created_session;
    Ok(DispatchSubmissionAccepted {
        request,
        session_id: accepted_session_id,
        entry_id: accepted_entry_id,
        accepted_at,
        created_session,
        root_task_id: graph.root_task_id,
        action_task_id: graph.action_task_id,
        turn_id,
        user_message_item_id,
        accepted_canonical_turn,
        runner_started: false,
        superseded_turn,
    })
}

#[cfg(test)]
mod tests;
