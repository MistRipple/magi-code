//! 任务系统 — 派发提交载体。
//!
//! 这两个 DTO 与 ApiState / ApiError 无运行期耦合，是 dispatch 流程的
//! "请求 → 接受" 一次性数据载体。magi-api 通过 `pub use` 重导出维持外部
//! import 路径不变。

use std::path::Path;
use std::sync::{Arc, Mutex};

use magi_agent_role::AgentRoleRegistry;
use magi_bridge_client::ModelBridgeClient;
use magi_core::{
    AccessProfile, CollaborationMode, DomainError, DomainResult, ExecutionOwnership, GoalId,
    MissionId, PlanItemId, SessionId, TaskCompletionContract, TaskExecutionTarget,
    TaskExecutorBinding, TaskId, TaskKind, TaskRecoveryCheckpoint, TaskStatus, TaskTier, ThreadId,
    UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::{EventContext, InMemoryEventBus, task_events};
use magi_orchestrator::{
    DispatchMemoryExtractionInput, ExecutionWritebackPlans, task_store::TaskStore,
};
use magi_session_store::{
    ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
    ActiveExecutionTurn, ActiveExecutionTurnItem, CanonicalTurn, CanonicalTurnItemKind,
    ExecutionThread, SessionPlan, SessionRuntimeSidecar, SessionStore, ThreadChatMessage,
    ThreadContextCheckpoint, TimelineEntryInput, TimelineEntryKind,
};
use magi_spawn_graph::SpawnGraph;
use serde::{Deserialize, Serialize};

use crate::session_thread;

use crate::context_reference::{
    SessionContextReference, browser_annotation_artifact_paths,
    browser_annotation_reference_input_refs, browser_annotation_references_metadata,
    browser_node_selection_input_refs, browser_node_selections_metadata,
    session_context_reference_input_refs, session_context_reference_policy,
    session_context_references_metadata,
};
use crate::session_images::SessionTurnImage;
use crate::task_execution_registry::{TaskExecutionPlan, TaskExecutionRegistry};
use magi_settings_store::SettingsStore;

pub struct DispatchSubmissionGraph {
    pub root_task_id: TaskId,
    pub action_task_id: TaskId,
    pub active_execution_chain: Option<ActiveExecutionChain>,
    pub(crate) materialize_rollback: Option<MaterializeRollback>,
}

impl DispatchSubmissionGraph {
    pub fn new(
        root_task_id: TaskId,
        action_task_id: TaskId,
        active_execution_chain: Option<ActiveExecutionChain>,
    ) -> Self {
        Self {
            root_task_id,
            action_task_id,
            active_execution_chain,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DispatchSubmissionRequest {
    pub accepted_at: UtcMillis,
    pub session_id: SessionId,
    pub workspace_id: Option<WorkspaceId>,
    /// 任务执行 cwd。项目会话使用项目根目录；个人会话使用 Magi 管理的私有目录。
    pub execution_root: Option<std::path::PathBuf>,
    /// 新会话首次执行时使用的模型配置。它是 accepted 请求事实的一部分，实际写入
    /// settings store 必须延后到后台 materialize 阶段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator_session_config: Option<serde_json::Value>,
    pub entry_id: String,
    pub timeline_message: String,
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
    pub collaboration_mode: CollaborationMode,
    pub access_profile: AccessProfile,
    pub skill_name: Option<String>,
    pub goal_mode: bool,
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
    pub turn_id: String,
    pub user_message_item_id: Option<String>,
}

/// 从 accepted journal 中恢复后台 materialize 所需的完整请求。
///
/// pending dispatch 是控制面 accepted 事实的一部分，不能依赖进程内的 future 或
/// runner 闭包；重启后唯一可信来源是 current turn item 的持久化元数据。
pub fn recover_dispatch_submission_request(
    sidecar: &SessionRuntimeSidecar,
) -> Result<DispatchSubmissionRequest, String> {
    let pending_dispatch = sidecar
        .current_turn
        .as_ref()
        .and_then(|turn| {
            turn.items
                .iter()
                .find_map(|item| item.metadata.get("pendingDispatch"))
        })
        .ok_or_else(|| "accepted Turn 缺少 pendingDispatch 请求事实".to_string())?;
    serde_json::from_value(pending_dispatch.clone())
        .map_err(|error| format!("解析 accepted Turn 的 pendingDispatch 请求失败: {error}"))
}

pub struct DispatchSubmissionRuntime<'a> {
    pub session_store: &'a SessionStore,
    pub task_store: &'a TaskStore,
    pub execution_registry: &'a TaskExecutionRegistry,
    pub event_bus: &'a InMemoryEventBus,
    pub agent_role_registry: &'a AgentRoleRegistry,
    pub spawn_graph: &'a Mutex<SpawnGraph>,
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
    if let Some(chain) = graph.active_execution_chain.as_ref()
        && let Some(turn_id) = chain
            .current_turn
            .as_ref()
            .map(|turn| turn.turn_id.as_str())
    {
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

fn build_task_policy(
    task_tier: TaskTier,
    access_profile: AccessProfile,
    context_references: &[SessionContextReference],
    browser_annotation_refs: &[serde_json::Value],
    workspace_root_path: Option<&Path>,
    denied_tools: Vec<String>,
    collaboration_mode: CollaborationMode,
) -> magi_core::TaskPolicy {
    let mut reference_policy = session_context_reference_policy(
        context_references,
        workspace_root_path
            .map(|path| path.to_string_lossy())
            .as_deref(),
        access_profile,
    );
    if access_profile != AccessProfile::FullAccess
        && reference_policy.allowed_paths.is_empty()
        && let Some(workspace_root_path) = workspace_root_path
    {
        reference_policy
            .allowed_paths
            .push(workspace_root_path.to_string_lossy().into_owned());
    }
    for artifact_path in browser_annotation_artifact_paths(browser_annotation_refs) {
        if access_profile != AccessProfile::FullAccess
            && !reference_policy.allowed_paths.contains(&artifact_path)
        {
            reference_policy.allowed_paths.push(artifact_path.clone());
        }
        if !reference_policy.read_only_paths.contains(&artifact_path) {
            reference_policy.read_only_paths.push(artifact_path);
        }
    }
    magi_core::TaskPolicy {
        autonomy_level: "Autonomous".to_string(),
        access_profile,
        collaboration_mode,
        allowed_tools: Vec::new(),
        denied_tools,
        allowed_paths: reference_policy.allowed_paths,
        denied_paths: Vec::new(),
        read_only_paths: reference_policy.read_only_paths,
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
    collaboration_mode: CollaborationMode,
    access_profile: AccessProfile,
    context_references: &'a [SessionContextReference],
    workspace_root_path: Option<&'a Path>,
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
        collaboration_mode,
        access_profile,
        context_references,
        workspace_root_path,
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
            workspace_root_path,
            denied_tools,
            collaboration_mode,
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
            magi_core::TaskRuntimePayload::BrowserAnnotations {
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
    if !runtime
        .agent_role_registry
        .role_supports_task_kind(target_role, TaskKind::LocalAgent)
    {
        return Err(DispatchSubmissionRunError::InvalidInput(format!(
            "role {target_role} 不支持 local_agent 任务"
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
    let task = make_dispatch_task(DispatchTaskInput {
        task_id: action_task_id.clone(),
        mission_id: mission_id.clone(),
        title: request.task_title.clone(),
        goal: execution_goal.to_string(),
        now,
        target_role,
        active_skill_id: request.skill_name.as_deref(),
        task_tier: request.task_tier,
        collaboration_mode: request.collaboration_mode,
        access_profile: request.access_profile,
        context_references: &request.context_references,
        browser_annotation_refs: &request.browser_annotation_refs,
        browser_node_selections: &request.browser_node_selections,
        workspace_root_path: runtime.workspace_root_path,
        required_tool_chain: request.required_tool_chain.clone(),
        goal_mode: request.goal_mode,
        completion_contract: request.completion_contract.clone(),
        recovery_checkpoint: request.recovery_checkpoint.clone(),
        denied_tools: request.denied_tools.clone(),
        plan_item_id: plan_item_id.clone(),
    });
    let worker_thread_id = if target_role == "coordinator" && request.recovery_checkpoint.is_none()
    {
        orchestrator_thread_id.clone()
    } else {
        ThreadId::new(format!(
            "thread-{target_role}-{}-{}",
            action_task_id, accepted_at.0
        ))
    };
    let turn_id = match &request.turn_origin {
        DispatchTurnOrigin::User => format!("turn-session-action-{}", accepted_at.0),
        DispatchTurnOrigin::GoalContinuation(_) => {
            format!(
                "turn-goal-continuation-{}-{}",
                request.session_id, accepted_at.0
            )
        }
    };
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
) -> Result<(ActiveExecutionChain, Option<String>), DispatchSubmissionRunError> {
    let branch = ActiveExecutionBranch {
        task_id: prepared.action_task_id.clone(),
        worker_id: prepared.worker_id.clone(),
        stage: "execute".to_string(),
        lease_id: None,
        execution_intent_ref: None,
        binding_lifecycle: None,
        checkpoint_stage: Some("execute".to_string()),
        next_step_index: Some(0),
        checkpoint_at: Some(prepared.now),
        resume_mode: Some("stage-restart".to_string()),
        resume_token: None,
        use_tools: true,
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
        let mut metadata = std::collections::HashMap::new();
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
            active_branch_task_ids: vec![prepared.action_task_id.clone()],
            active_worker_bindings: vec![prepared.worker_id.clone()],
            branches,
            recovery_ref: None,
            dispatch_context: ActiveExecutionDispatchContext {
                accepted_at: request.accepted_at,
                entry_id: request.entry_id.clone(),
                trimmed_text: request.trimmed_text.clone(),
                skill_name: request.skill_name.clone(),
            },
            current_turn: Some(current_turn),
        },
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
    let active_execution_chain = match pending_chain {
        Some(chain) => chain,
        None => build_active_execution_chain(prepared, request)?.0,
    };
    let interrupted_checkpoint = request
        .recovery_checkpoint
        .as_ref()
        .map(|checkpoint| prepare_interrupted_turn_checkpoint(runtime.session_store, checkpoint))
        .transpose()?;
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
    if let Some(checkpoint) = interrupted_checkpoint
        && let Err(error) = install_interrupted_turn_checkpoint(
            runtime.session_store,
            &worker_thread_id,
            checkpoint,
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
    let ownership = ExecutionOwnership {
        session_id: Some(request.session_id.clone()),
        workspace_id: request.workspace_id.clone(),
        mission_id: Some(prepared.mission_id.clone()),
        task_id: Some(prepared.action_task_id.clone()),
        worker_id: Some(prepared.worker_id.clone()),
        execution_chain_ref: Some(prepared.execution_chain_ref.clone()),
    };
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
        ownership,
        writebacks: ExecutionWritebackPlans::from_session_action_input(
            DispatchMemoryExtractionInput {
                accepted_at: request.accepted_at,
                session_id: &request.session_id,
                timeline_entry_id: &request.entry_id,
                text: request.trimmed_text.as_deref(),
                skill_name: request.skill_name.as_deref(),
            },
        ),
        use_tools: true,
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
        && let Err(error) = runtime.session_store.upsert_active_execution_chain(
            request.session_id.clone(),
            active_execution_chain.clone(),
        )
    {
        return attempt.fail(DispatchSubmissionRunError::Internal(format!(
            "写回 materialize execution chain 失败: {error}"
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
        active_execution_chain: Some(active_execution_chain),
        materialize_rollback,
    })
}

/// accepted 前只写入一个 root task 和一条带原始请求的最小 Turn。
pub fn prepare_pending_dispatch_submission(
    runtime: &DispatchSubmissionRuntime<'_>,
    request: &DispatchSubmissionRequest,
) -> Result<PendingDispatchSubmission, DispatchSubmissionRunError> {
    let prepared = validate_dispatch_request(runtime, request, true)?;
    let (active_execution_chain, user_message_item_id) =
        build_active_execution_chain(&prepared, request)?;
    runtime
        .task_store
        .insert_task_without_checkpoint(prepared.task.clone())
        .map_err(|error| {
            DispatchSubmissionRunError::Internal(format!("写入 pending root task 失败: {error}"))
        })?;
    Ok(PendingDispatchSubmission {
        task: prepared.task,
        active_execution_chain,
        turn_id: prepared.turn_id,
        user_message_item_id,
    })
}

/// 由直接调用 runtime 的旧测试/内部入口使用，完整装配仍在返回 graph 前完成。
pub fn run_dispatch_submission(
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
    if current_turn.turn_id != expected_turn_id
        || !matches!(current_turn.status.as_str(), "accepted" | "preparing")
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

struct InterruptedTurnCheckpointSeed {
    message_history: Vec<ThreadChatMessage>,
    context_checkpoint: Option<ThreadContextCheckpoint>,
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

fn prepare_interrupted_turn_checkpoint(
    session_store: &SessionStore,
    checkpoint: &TaskRecoveryCheckpoint,
) -> Result<InterruptedTurnCheckpointSeed, DispatchSubmissionRunError> {
    validate_interrupted_turn_checkpoint(session_store, checkpoint)?;
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
    let context_checkpoint = session_store.thread_context_checkpoint(&checkpoint.source_thread_id);
    Ok(InterruptedTurnCheckpointSeed {
        message_history: source_thread.message_history,
        context_checkpoint,
    })
}

fn install_interrupted_turn_checkpoint(
    session_store: &SessionStore,
    destination_thread_id: &magi_core::ThreadId,
    checkpoint: InterruptedTurnCheckpointSeed,
    now: UtcMillis,
) -> Result<(), DispatchSubmissionRunError> {
    session_store
        .replace_thread_messages_checked(destination_thread_id, checkpoint.message_history, now)
        .map_err(|error| {
            DispatchSubmissionRunError::Internal(format!("替换恢复 thread 对话历史失败: {error}"))
        })?;
    if let Some(source_checkpoint) = checkpoint.context_checkpoint {
        session_store
            .install_thread_context_checkpoint_checked(
                destination_thread_id,
                magi_session_store::ThreadContextCheckpoint {
                    thread_id: destination_thread_id.clone(),
                    ..source_checkpoint
                },
                now,
            )
            .map_err(|error| {
                DispatchSubmissionRunError::Internal(format!(
                    "安装恢复 thread 上下文检查点失败: {error}"
                ))
            })?;
    }
    Ok(())
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
    let turn_id = graph
        .active_execution_chain
        .as_ref()
        .and_then(|chain| chain.current_turn.as_ref())
        .map(|turn| turn.turn_id.clone())
        .or_else(|| {
            session_store
                .runtime_sidecar(&request.session_id)
                .and_then(|sidecar| sidecar.current_turn)
                .map(|turn| turn.turn_id)
        })
        .ok_or_else(|| DispatchSubmissionAcceptError::Internal {
            message: "接纳任务派发后缺少当前 Turn".to_string(),
        })?;
    if let Some(active_execution_chain) = graph.active_execution_chain.clone() {
        let accept_result = if let Some(goal_id) = request.turn_origin.continuation_goal_id() {
            session_store
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
                    &acceptance_task,
                )
                .map(|(_, _, canonical_turn)| (None, canonical_turn))
        } else if let Some(replace_turn_id) = request.replace_turn_id.as_deref() {
            session_store
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
                    &acceptance_task,
                )
                .map(|(_, _, superseded_turn, canonical_turn)| {
                    (Some(superseded_turn), Some(canonical_turn))
                })
        } else {
            session_store
                .accept_active_execution_chain_with_timeline_entry_and_task(
                    request.session_id.clone(),
                    TimelineEntryInput::new(
                        request.entry_id.clone(),
                        request.turn_origin.timeline_kind(),
                        request.timeline_message.clone(),
                        request.accepted_at,
                    ),
                    active_execution_chain,
                    &acceptance_task,
                )
                .map(|(_, _, canonical_turn)| (None, canonical_turn))
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
        return Ok(DispatchSubmissionAccepted {
            request,
            session_id: accepted_session_id,
            entry_id: accepted_entry_id,
            accepted_at,
            created_session,
            root_task_id: graph.root_task_id,
            action_task_id: graph.action_task_id,
            turn_id: turn_id.clone(),
            user_message_item_id,
            accepted_canonical_turn,
            runner_started: false,
            superseded_turn,
        });
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
    if let Some(rollback) = graph.materialize_rollback.take() {
        rollback.commit();
    }
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
        accepted_canonical_turn: None,
        runner_started: false,
        superseded_turn: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_session_store::{
        CanonicalTurnEventWriter, CanonicalTurnMutation, ExecutionThread, ExecutionThreadStatus,
        GoalContinuationPhase, GoalStatus, SessionAcceptanceRecord, ThreadChatMessage,
        ThreadChatToolCall, ThreadChatToolFunction,
    };

    struct RejectingCanonicalWriter;

    impl CanonicalTurnEventWriter for RejectingCanonicalWriter {
        fn append_canonical_turn_transaction(
            &self,
            _session_id: &SessionId,
            _mutations: &[CanonicalTurnMutation],
        ) -> DomainResult<()> {
            Err(DomainError::Persistence {
                message: "test canonical write rejection".to_string(),
            })
        }

        fn append_canonical_turn_transaction_with_acceptance(
            &self,
            _session_id: &SessionId,
            _mutations: &[CanonicalTurnMutation],
            _acceptance: &SessionAcceptanceRecord,
            _task: &magi_core::Task,
        ) -> DomainResult<()> {
            Err(DomainError::Persistence {
                message: "test acceptance write rejection".to_string(),
            })
        }
    }

    fn rollback_test_request(
        session_id: SessionId,
        accepted_at: UtcMillis,
        target_role: &str,
    ) -> DispatchSubmissionRequest {
        DispatchSubmissionRequest {
            accepted_at,
            session_id,
            workspace_id: Some(WorkspaceId::new("workspace-materialize-rollback")),
            execution_root: None,
            orchestrator_session_config: None,
            entry_id: format!("timeline-materialize-rollback-{}", accepted_at.0),
            timeline_message: "验证执行装配回滚".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            browser_node_selections: Vec::new(),
            created_session: false,
            mission_title: "执行装配回滚".to_string(),
            task_title: "执行: 装配回滚".to_string(),
            trimmed_text: Some("验证执行装配回滚".to_string()),
            execution_goal: Some("验证执行装配失败时不留下任何运行资源".to_string()),
            task_tier: TaskTier::ExecutionChain,
            collaboration_mode: CollaborationMode::Auto,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            goal_mode: false,
            target_role: Some(target_role.to_string()),
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            replace_turn_id: None,
            required_tool_chain: Vec::new(),
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: None,
            denied_tools: Vec::new(),
            user_message_metadata: Default::default(),
            turn_origin: DispatchTurnOrigin::User,
        }
    }

    fn rollback_test_execution_plan(
        session_id: &SessionId,
        mission_id: &MissionId,
        task_id: &TaskId,
        worker_id: &WorkerId,
        thread_id: &ThreadId,
        turn_id: &str,
    ) -> TaskExecutionPlan {
        TaskExecutionPlan::Dispatch {
            target: TaskExecutionTarget {
                mission_id: mission_id.clone(),
                root_task_id: task_id.clone(),
                task_id: task_id.clone(),
                requested_worker_id: Some(worker_id.clone()),
                recovery_id: None,
                execution_chain_ref: Some("chain-materialize-rollback".to_string()),
            },
            worker_id: worker_id.clone(),
            thread_id: thread_id.clone(),
            is_primary: true,
            session_id: session_id.clone(),
            turn_id: turn_id.to_string(),
            workspace_id: None,
            execution_root: None,
            ownership: ExecutionOwnership {
                session_id: Some(session_id.clone()),
                mission_id: Some(mission_id.clone()),
                task_id: Some(task_id.clone()),
                worker_id: Some(worker_id.clone()),
                execution_chain_ref: Some("chain-materialize-rollback".to_string()),
                ..ExecutionOwnership::default()
            },
            writebacks: ExecutionWritebackPlans::default(),
            use_tools: true,
            skill_name: None,
            images: Vec::new(),
            execution_settings_snapshot: None,
        }
    }

    #[test]
    fn dispatch_submission_creates_fresh_worker_thread_even_when_role_has_idle_history() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-dispatch-fresh-thread");
        let mission_id = MissionId::new("mission-dispatch-fresh-thread");
        let old_thread_id = magi_core::ThreadId::new("thread-executor-old");

        session_store
            .create_session(session_id.clone(), "dispatch fresh thread")
            .expect("session should be creatable");
        session_store
            .register_thread(ExecutionThread {
                thread_id: old_thread_id.clone(),
                session_id: session_id.clone(),
                mission_id: mission_id.clone(),
                role_id: "executor".to_string(),
                worker_instance_id: WorkerId::new("worker-old"),
                status: ExecutionThreadStatus::Idle,
                created_at: UtcMillis(1_000),
                last_used_at: UtcMillis(1_000),
                observed_context_window_tokens: None,
                handled_task_ids: vec![TaskId::new("task-old")],
                message_history: vec![ThreadChatMessage {
                    role: "user".to_string(),
                    content: Some(
                        "历史验收任务：写 validation_auto_save_marker.txt / COMPLEX_WORKER_LANE_OK"
                            .to_string(),
                    ),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: Vec::new(),
                }],
            })
            .expect("旧 thread 测试数据应注册成功");

        let request = DispatchSubmissionRequest {
            accepted_at: UtcMillis(2_000),
            session_id: session_id.clone(),
            workspace_id: Some(WorkspaceId::new("workspace-dispatch-fresh-thread")),
            execution_root: None,
            orchestrator_session_config: None,
            entry_id: "timeline-dispatch-fresh-thread".to_string(),
            timeline_message: "创建当前任务文件".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            browser_node_selections: Vec::new(),
            created_session: false,
            mission_title: "当前任务推进".to_string(),
            task_title: "当前任务推进".to_string(),
            trimmed_text: Some("创建 task-system-e2e.md".to_string()),
            execution_goal: Some("创建 task-system-e2e.md 并写入当前 marker".to_string()),
            task_tier: TaskTier::ExecutionChain,
            collaboration_mode: CollaborationMode::Auto,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            goal_mode: false,
            target_role: Some("executor".to_string()),
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            replace_turn_id: None,
            required_tool_chain: Vec::new(),
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: None,
            denied_tools: Vec::new(),
            user_message_metadata: Default::default(),
            turn_origin: DispatchTurnOrigin::User,
        };
        let runtime = DispatchSubmissionRuntime {
            session_store: &session_store,
            task_store: &task_store,
            execution_registry: &execution_registry,
            event_bus: &event_bus,
            agent_role_registry: &agent_role_registry,
            spawn_graph: &spawn_graph,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: None,
        };

        let graph = run_dispatch_submission(&runtime, &request)
            .expect("dispatch submission should build graph");
        let chain = graph
            .active_execution_chain
            .expect("dispatch should create active execution chain");
        let lane_thread_id = chain.branches[0].thread_id.clone();

        assert_ne!(lane_thread_id, old_thread_id);
        assert!(
            session_store
                .thread_message_history(&lane_thread_id)
                .is_empty(),
            "新的 worker thread 不能继承旧 role thread 的 message_history"
        );
        assert_eq!(
            session_store.thread_message_history(&old_thread_id)[0]
                .content
                .as_deref(),
            Some("历史验收任务：写 validation_auto_save_marker.txt / COMPLEX_WORKER_LANE_OK")
        );
    }

    #[test]
    fn coordinator_dispatch_reuses_session_orchestrator_thread_for_continuous_context() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-dispatch-orchestrator-thread");
        session_store
            .create_session(session_id.clone(), "coordinator thread")
            .expect("session should be creatable");

        let runtime = DispatchSubmissionRuntime {
            session_store: &session_store,
            task_store: &task_store,
            execution_registry: &execution_registry,
            event_bus: &event_bus,
            agent_role_registry: &agent_role_registry,
            spawn_graph: &spawn_graph,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: None,
        };
        let make_request = |accepted_at| DispatchSubmissionRequest {
            accepted_at,
            session_id: session_id.clone(),
            workspace_id: None,
            execution_root: None,
            orchestrator_session_config: None,
            entry_id: format!("timeline-{}", accepted_at.0),
            timeline_message: format!("主线回合 {}", accepted_at.0),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            browser_node_selections: Vec::new(),
            created_session: false,
            mission_title: "连续上下文".to_string(),
            task_title: "连续上下文".to_string(),
            trimmed_text: Some("继续处理".to_string()),
            execution_goal: Some("继续处理当前任务".to_string()),
            task_tier: TaskTier::ExecutionChain,
            collaboration_mode: CollaborationMode::Auto,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            goal_mode: false,
            target_role: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            replace_turn_id: None,
            required_tool_chain: Vec::new(),
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: None,
            denied_tools: Vec::new(),
            user_message_metadata: Default::default(),
            turn_origin: DispatchTurnOrigin::User,
        };

        let first = run_dispatch_submission(&runtime, &make_request(UtcMillis(1_000)))
            .expect("first coordinator dispatch should build");
        let second = run_dispatch_submission(&runtime, &make_request(UtcMillis(2_000)))
            .expect("second coordinator dispatch should build");
        let orchestrator_thread_id = session_store
            .orchestrator_thread_for_session(&session_id)
            .expect("session should have orchestrator thread")
            .thread_id;
        let first_thread_id = first
            .active_execution_chain
            .as_ref()
            .and_then(|chain| chain.branches.first())
            .expect("first branch should exist")
            .thread_id
            .clone();
        let second_thread_id = second
            .active_execution_chain
            .as_ref()
            .and_then(|chain| chain.branches.first())
            .expect("second branch should exist")
            .thread_id
            .clone();

        assert_eq!(first_thread_id, orchestrator_thread_id);
        assert_eq!(second_thread_id, orchestrator_thread_id);
        let thread = session_store
            .thread_registry_snapshot(&session_id)
            .into_iter()
            .find(|thread| thread.thread_id == orchestrator_thread_id)
            .expect("orchestrator thread should be registered");
        assert_eq!(thread.handled_task_ids.len(), 2);
        assert_eq!(thread.status, ExecutionThreadStatus::Active);
    }

    #[test]
    fn dispatch_submission_persists_authoritative_browser_annotations_on_turn_and_task() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-dispatch-browser-annotation");
        session_store
            .create_session(session_id.clone(), "dispatch browser annotation")
            .expect("session should be creatable");
        let annotation = serde_json::json!({
            "annotationId": "browser-annotation-dispatch",
            "browserSessionId": "browser-session-dispatch",
            "tabId": "browser-tab-dispatch",
            "comment": "检查保存按钮",
            "anchor": {
                "kind": "region",
                "url": "https://example.com/settings",
                "snapshotRevision": 4
            },
            "screenshotArtifactId": "session-dispatch/annotation.png",
            "screenshotPath": "/tmp/browser-artifacts/session-dispatch/annotation.png",
            "status": "active"
        });
        let request = DispatchSubmissionRequest {
            accepted_at: UtcMillis(2_100),
            session_id: session_id.clone(),
            workspace_id: Some(WorkspaceId::new("workspace-dispatch-browser-annotation")),
            execution_root: None,
            orchestrator_session_config: None,
            entry_id: "timeline-dispatch-browser-annotation".to_string(),
            timeline_message: "根据浏览器标记检查页面".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: vec![annotation.clone()],
            browser_node_selections: Vec::new(),
            created_session: false,
            mission_title: "浏览器标记检查".to_string(),
            task_title: "浏览器标记检查".to_string(),
            trimmed_text: Some("根据浏览器标记检查页面".to_string()),
            execution_goal: Some("核对标记位置并报告问题".to_string()),
            task_tier: TaskTier::ExecutionChain,
            collaboration_mode: CollaborationMode::Auto,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            goal_mode: false,
            target_role: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            replace_turn_id: None,
            required_tool_chain: Vec::new(),
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: None,
            denied_tools: Vec::new(),
            user_message_metadata: std::collections::HashMap::from([(
                "route".to_string(),
                serde_json::json!("chat"),
            )]),
            turn_origin: DispatchTurnOrigin::User,
        };
        let runtime = DispatchSubmissionRuntime {
            session_store: &session_store,
            task_store: &task_store,
            execution_registry: &execution_registry,
            event_bus: &event_bus,
            agent_role_registry: &agent_role_registry,
            spawn_graph: &spawn_graph,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: Some(Path::new("/tmp/workspace-dispatch-browser-annotation")),
        };

        let graph = run_dispatch_submission(&runtime, &request)
            .expect("browser annotation dispatch should build graph");
        let task = task_store
            .get_task(&graph.root_task_id)
            .expect("root task should persist");
        assert_eq!(
            task.browser_annotation_references(),
            std::slice::from_ref(&annotation)
        );
        let policy = task
            .policy_snapshot
            .as_ref()
            .expect("browser annotation task should have policy");
        assert_eq!(
            policy.allowed_paths,
            vec![
                "/tmp/workspace-dispatch-browser-annotation",
                "/tmp/browser-artifacts/session-dispatch/annotation.png"
            ]
        );
        assert_eq!(
            policy.read_only_paths,
            vec!["/tmp/browser-artifacts/session-dispatch/annotation.png"]
        );
        assert!(task.input_refs.iter().any(|reference| {
            reference.contains("/tmp/browser-artifacts/session-dispatch/annotation.png")
        }));
        let user_item = graph
            .active_execution_chain
            .as_ref()
            .and_then(|chain| chain.current_turn.as_ref())
            .and_then(|turn| turn.items.iter().find(|item| item.kind == "user_message"))
            .expect("canonical user item should persist");
        assert_eq!(
            user_item.metadata.get("browserAnnotationRefs"),
            Some(&serde_json::Value::Array(vec![annotation]))
        );
        accept_dispatch_submission(
            &session_store,
            &task_store,
            &execution_registry,
            request,
            graph,
        )
        .expect("dispatch acceptance should persist the complete user item atomically");
        let canonical_user_item = session_store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .flat_map(|turn| turn.items)
            .find(|item| item.kind == CanonicalTurnItemKind::UserMessage)
            .expect("canonical user item should be restored");
        assert_eq!(
            canonical_user_item.metadata.get("route"),
            Some(&serde_json::json!("chat"))
        );
    }

    #[test]
    fn interrupted_turn_resume_uses_new_thread_with_explicit_checkpoint_history() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-dispatch-resume-checkpoint");
        let source_task_id = TaskId::new("task-dispatch-resume-source");
        let now = UtcMillis(2_500);

        session_store
            .create_session(session_id.clone(), "dispatch resume checkpoint")
            .expect("session should be creatable");
        let (mission_id, orchestrator_thread_id) =
            session_store.ensure_session_mission(&session_id, now, || {
                MissionId::new("mission-dispatch-resume-checkpoint")
            });
        let source_thread_id = magi_core::ThreadId::new("thread-dispatch-resume-source");
        let source_history = vec![
            ThreadChatMessage {
                role: "user".to_string(),
                content: Some("画当前项目流程图".to_string()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: Vec::new(),
            },
            ThreadChatMessage {
                role: "assistant".to_string(),
                content: None,
                images: Vec::new(),
                tool_calls: vec![ThreadChatToolCall {
                    id: "call-resume-file-read".to_string(),
                    kind: "function".to_string(),
                    function: ThreadChatToolFunction {
                        name: "file_read".to_string(),
                        arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
                    },
                }],
                tool_call_id: None,
                provider_context: Vec::new(),
            },
            ThreadChatMessage {
                role: "tool".to_string(),
                content: Some(r#"{"status":"succeeded","content":"workspace"}"#.to_string()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: Some("call-resume-file-read".to_string()),
                provider_context: Vec::new(),
            },
        ];
        session_store
            .register_thread(ExecutionThread {
                thread_id: source_thread_id.clone(),
                session_id: session_id.clone(),
                mission_id,
                role_id: "coordinator".to_string(),
                worker_instance_id: WorkerId::new("worker-dispatch-resume-source"),
                status: ExecutionThreadStatus::Idle,
                created_at: now,
                last_used_at: now,
                observed_context_window_tokens: None,
                handled_task_ids: vec![source_task_id.clone()],
                message_history: source_history.clone(),
            })
            .expect("source thread 测试数据应注册成功");
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-dispatch-resume-source".to_string(),
                    turn_seq: now.0,
                    accepted_at: now,
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("画当前项目流程图".to_string()),
                    items: vec![ActiveExecutionTurnItem {
                        item_id: "user-dispatch-resume-source".to_string(),
                        item_seq: 1,
                        kind: "user_message".to_string(),
                        status: "completed".to_string(),
                        source: "user".to_string(),
                        title: None,
                        content: Some("画当前项目流程图".to_string()),
                        task_id: Some(source_task_id.clone()),
                        worker_id: None,
                        role_id: None,
                        tool_call_id: None,
                        tool_name: None,
                        tool_status: None,
                        tool_arguments: None,
                        tool_result: None,
                        tool_error: None,
                        request_id: None,
                        user_message_id: Some("user-dispatch-resume-source".to_string()),
                        placeholder_message_id: None,
                        metadata: Default::default(),
                        timeline_entry_id: None,
                        source_thread_id: orchestrator_thread_id,
                    }],
                },
            )
            .expect("source turn should persist");
        session_store
            .interrupt_current_turn_by_user(&session_id)
            .expect("source turn should be interrupted by user");

        let request = DispatchSubmissionRequest {
            accepted_at: UtcMillis(3_000),
            session_id: session_id.clone(),
            workspace_id: Some(WorkspaceId::new("workspace-dispatch-resume-checkpoint")),
            execution_root: None,
            orchestrator_session_config: None,
            entry_id: "timeline-dispatch-resume-checkpoint".to_string(),
            timeline_message: "继续".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            browser_node_selections: Vec::new(),
            created_session: false,
            mission_title: "继续中断任务".to_string(),
            task_title: "继续: 画当前项目流程图".to_string(),
            trimmed_text: Some("继续".to_string()),
            execution_goal: Some("继续原始流程图任务".to_string()),
            task_tier: TaskTier::ExecutionChain,
            collaboration_mode: CollaborationMode::Auto,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            goal_mode: false,
            target_role: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            replace_turn_id: None,
            required_tool_chain: vec!["file_read".to_string()],
            completion_contract: TaskCompletionContract::default().with_evidence_requirements(
                vec![magi_core::TaskEvidenceRequirement::successful_tool_call(
                    "diagram_render",
                )],
            ),
            recovery_checkpoint: Some(TaskRecoveryCheckpoint {
                source_session_id: session_id.clone(),
                source_task_id: source_task_id.clone(),
                source_turn_id: "turn-dispatch-resume-source".to_string(),
                source_thread_id: source_thread_id.clone(),
            }),
            denied_tools: Vec::new(),
            user_message_metadata: Default::default(),
            turn_origin: DispatchTurnOrigin::User,
        };
        let runtime = DispatchSubmissionRuntime {
            session_store: &session_store,
            task_store: &task_store,
            execution_registry: &execution_registry,
            event_bus: &event_bus,
            agent_role_registry: &agent_role_registry,
            spawn_graph: &spawn_graph,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: None,
        };

        let graph = run_dispatch_submission(&runtime, &request)
            .expect("resume dispatch should build graph");
        let destination_thread_id = graph
            .active_execution_chain
            .as_ref()
            .and_then(|chain| chain.branches.first())
            .map(|branch| branch.thread_id.clone())
            .expect("resume task should own a thread");
        assert_ne!(destination_thread_id, source_thread_id);
        let destination_history = session_store.thread_message_history(&destination_thread_id);
        assert_eq!(destination_history.len(), source_history.len());
        assert_eq!(
            destination_history[0].content.as_deref(),
            Some("画当前项目流程图")
        );
        assert_eq!(
            destination_history[1].tool_calls[0].function.name,
            "file_read"
        );
        assert_eq!(
            destination_history[2].tool_call_id.as_deref(),
            Some("call-resume-file-read")
        );
        let resumed_task = task_store
            .get_task(&graph.action_task_id)
            .expect("resume action task should persist");
        assert_eq!(
            resumed_task
                .recovery_checkpoint()
                .map(|checkpoint| checkpoint.source_turn_id.as_str()),
            Some("turn-dispatch-resume-source")
        );
        assert_eq!(resumed_task.required_tool_chain(), ["file_read"]);
        assert_eq!(
            resumed_task.completion_contract().evidence_requirements,
            [magi_core::TaskEvidenceRequirement::successful_tool_call(
                "diagram_render"
            )]
        );
    }

    #[test]
    fn invalid_interrupted_turn_checkpoint_is_rejected_before_dispatch_side_effects() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-invalid-resume-checkpoint");
        let source_task_id = TaskId::new("task-invalid-resume-source");
        let now = UtcMillis(3_500);

        session_store
            .create_session(session_id.clone(), "invalid resume checkpoint")
            .expect("session should be creatable");
        let (_, orchestrator_thread_id) =
            session_store.ensure_session_mission(&session_id, now, || {
                MissionId::new("mission-invalid-resume-checkpoint")
            });
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-invalid-resume-source".to_string(),
                    turn_seq: now.0,
                    accepted_at: now,
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("继续验证".to_string()),
                    items: vec![ActiveExecutionTurnItem {
                        item_id: "user-invalid-resume-source".to_string(),
                        item_seq: 1,
                        kind: "user_message".to_string(),
                        status: "completed".to_string(),
                        source: "user".to_string(),
                        title: None,
                        content: Some("继续验证".to_string()),
                        task_id: Some(source_task_id.clone()),
                        worker_id: None,
                        role_id: None,
                        tool_call_id: None,
                        tool_name: None,
                        tool_status: None,
                        tool_arguments: None,
                        tool_result: None,
                        tool_error: None,
                        request_id: None,
                        user_message_id: None,
                        placeholder_message_id: None,
                        metadata: Default::default(),
                        timeline_entry_id: None,
                        source_thread_id: orchestrator_thread_id.clone(),
                    }],
                },
            )
            .expect("source turn should persist");
        session_store
            .interrupt_current_turn_by_user(&session_id)
            .expect("source turn should be interrupted by user");
        let thread_count_before = session_store.thread_registry_snapshot(&session_id).len();
        let event_count_before = event_bus.snapshot().recent_events.len();
        let request = DispatchSubmissionRequest {
            accepted_at: UtcMillis(4_000),
            session_id: session_id.clone(),
            workspace_id: Some(WorkspaceId::new("workspace-invalid-resume-checkpoint")),
            execution_root: None,
            orchestrator_session_config: None,
            entry_id: "timeline-invalid-resume-checkpoint".to_string(),
            timeline_message: "继续".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            browser_node_selections: Vec::new(),
            created_session: false,
            mission_title: "继续中断任务".to_string(),
            task_title: "继续: 验证恢复原子性".to_string(),
            trimmed_text: Some("继续".to_string()),
            execution_goal: Some("继续验证恢复原子性".to_string()),
            task_tier: TaskTier::ExecutionChain,
            collaboration_mode: CollaborationMode::Auto,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            goal_mode: false,
            target_role: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            replace_turn_id: None,
            required_tool_chain: vec!["shell_exec".to_string()],
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: Some(TaskRecoveryCheckpoint {
                source_session_id: session_id.clone(),
                source_task_id,
                source_turn_id: "turn-invalid-resume-source".to_string(),
                // 主线 thread 不处理 action task，正是本次真实故障中的错误参数。
                source_thread_id: orchestrator_thread_id,
            }),
            denied_tools: Vec::new(),
            user_message_metadata: Default::default(),
            turn_origin: DispatchTurnOrigin::User,
        };
        let runtime = DispatchSubmissionRuntime {
            session_store: &session_store,
            task_store: &task_store,
            execution_registry: &execution_registry,
            event_bus: &event_bus,
            agent_role_registry: &agent_role_registry,
            spawn_graph: &spawn_graph,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: None,
        };

        let error = match run_dispatch_submission(&runtime, &request) {
            Ok(_) => panic!("mismatched source thread should reject dispatch"),
            Err(error) => error,
        };

        assert!(
            error
                .into_message()
                .contains("续接来源 Thread 与任务不匹配")
        );
        assert!(task_store.all_tasks().is_empty());
        assert!(
            execution_registry
                .get(&TaskId::new("task-local-agent-4000"))
                .is_none()
        );
        assert_eq!(
            session_store.thread_registry_snapshot(&session_id).len(),
            thread_count_before,
            "拒绝恢复不得创建空的 task thread",
        );
        assert_eq!(event_bus.snapshot().recent_events.len(), event_count_before);
        assert_eq!(
            session_store
                .canonical_turns_for_session(&session_id)
                .last()
                .map(|turn| turn.status),
            Some(magi_session_store::CanonicalTurnStatus::Cancelled),
        );
    }

    /// 任务系统验收：所有 action task 统一走 ExecutionChain 路径。
    ///
    /// 验收点：
    /// - route 已是 task（由 classifier 决定，本处不重测）；
    /// - dispatch 创建 action task 并落入 TaskStore；
    /// - `policy_snapshot.task_tier == ExecutionChain`；
    /// - 同步产生 ActiveExecutionChain，让运行期具备可观察的执行链。
    ///
    #[test]
    fn execution_chain_dispatch_creates_action_task_with_chain_tier() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-exec-chain-tier");

        session_store
            .create_session(session_id.clone(), "execution chain tier")
            .expect("session should be creatable");

        let request = DispatchSubmissionRequest {
            accepted_at: UtcMillis(3_000),
            session_id: session_id.clone(),
            workspace_id: Some(WorkspaceId::new("workspace-exec-chain-tier")),
            execution_root: None,
            orchestrator_session_config: None,
            entry_id: "timeline-exec-chain-tier".to_string(),
            timeline_message: "修复明确 bug 并跑相关验证".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            browser_node_selections: Vec::new(),
            created_session: false,
            mission_title: "修复 bug + 验证".to_string(),
            task_title: "修复 bug + 验证".to_string(),
            trimmed_text: Some("修复明确 bug 并跑相关验证".to_string()),
            execution_goal: Some("定位并修复 bug、再跑相关验证命令".to_string()),
            task_tier: TaskTier::ExecutionChain,
            collaboration_mode: CollaborationMode::Auto,
            access_profile: AccessProfile::FullAccess,
            skill_name: None,
            goal_mode: false,
            target_role: Some("executor".to_string()),
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            replace_turn_id: None,
            required_tool_chain: Vec::new(),
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: None,
            denied_tools: Vec::new(),
            user_message_metadata: Default::default(),
            turn_origin: DispatchTurnOrigin::User,
        };
        let runtime = DispatchSubmissionRuntime {
            session_store: &session_store,
            task_store: &task_store,
            execution_registry: &execution_registry,
            event_bus: &event_bus,
            agent_role_registry: &agent_role_registry,
            spawn_graph: &spawn_graph,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: None,
        };

        let graph = run_dispatch_submission(&runtime, &request)
            .expect("execution chain dispatch should build graph");

        let action_task = task_store
            .get_task(&graph.action_task_id)
            .expect("action task should be persisted in TaskStore");
        let policy = action_task
            .policy_snapshot
            .as_ref()
            .expect("dispatch 必须给 action task 写入 policy_snapshot");
        assert_eq!(
            policy.task_tier,
            TaskTier::ExecutionChain,
            "action task tier 必须统一为 ExecutionChain",
        );
        assert_eq!(
            policy.access_profile,
            AccessProfile::FullAccess,
            "用户选择的访问模式必须写入 action task policy_snapshot",
        );

        let chain = graph
            .active_execution_chain
            .as_ref()
            .expect("ExecutionChain 路径必须同步产出 ActiveExecutionChain");
        assert!(
            chain.current_turn.is_some(),
            "ActiveExecutionChain 必须带 current_turn，作为运行期 lane 调度入口",
        );
    }

    #[test]
    fn dispatch_binds_root_task_to_current_plan_stage() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-plan-root-binding");
        session_store
            .create_session(session_id.clone(), "plan root binding")
            .expect("session should be creatable");
        let plan_store = magi_plan::PlanStore::from_store(&session_store, session_id.clone());
        let plan = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: None,
                expected_goal_control_revision: None,
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![
                    magi_plan::UpdatePlanItemInput {
                        item_id: Some("implement".to_string()),
                        step: "完成实现".to_string(),
                        status: magi_core::PlanItemStatus::InProgress,
                    },
                    magi_plan::UpdatePlanItemInput {
                        item_id: Some("verify".to_string()),
                        step: "完成验证".to_string(),
                        status: magi_core::PlanItemStatus::Pending,
                    },
                ],
            })
            .expect("plan should create");
        let request = DispatchSubmissionRequest {
            accepted_at: UtcMillis(3_100),
            session_id: session_id.clone(),
            workspace_id: Some(WorkspaceId::new("workspace-plan-root-binding")),
            execution_root: None,
            orchestrator_session_config: None,
            entry_id: "timeline-plan-root-binding".to_string(),
            timeline_message: "继续计划".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            browser_node_selections: Vec::new(),
            created_session: false,
            mission_title: "继续计划".to_string(),
            task_title: "继续计划".to_string(),
            trimmed_text: Some("继续计划".to_string()),
            execution_goal: Some("完成当前计划全部阶段".to_string()),
            task_tier: TaskTier::ExecutionChain,
            collaboration_mode: CollaborationMode::Auto,
            access_profile: AccessProfile::FullAccess,
            skill_name: None,
            goal_mode: false,
            target_role: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            replace_turn_id: None,
            required_tool_chain: Vec::new(),
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: None,
            denied_tools: Vec::new(),
            user_message_metadata: Default::default(),
            turn_origin: DispatchTurnOrigin::User,
        };
        let runtime = DispatchSubmissionRuntime {
            session_store: &session_store,
            task_store: &task_store,
            execution_registry: &execution_registry,
            event_bus: &event_bus,
            agent_role_registry: &agent_role_registry,
            spawn_graph: &spawn_graph,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: None,
        };

        let graph = run_dispatch_submission(&runtime, &request).expect("dispatch should build");
        let root_task = task_store
            .get_task(&graph.root_task_id)
            .expect("root task should exist");
        assert_eq!(root_task.plan_item_id(), Some(&plan.items[0].item_id));
        let bound_plan = plan_store.snapshot().expect("plan should remain");
        assert_eq!(
            bound_plan.task_bindings.get(&graph.root_task_id),
            Some(&plan.items[0].item_id)
        );
    }

    #[test]
    fn selected_skill_does_not_reassign_mainline_coordinator_role() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-dispatch-skill-mainline-role");

        session_store
            .create_session(session_id.clone(), "dispatch skill mainline role")
            .expect("session should be creatable");

        let request = DispatchSubmissionRequest {
            accepted_at: UtcMillis(3_250),
            session_id,
            workspace_id: Some(WorkspaceId::new("workspace-dispatch-skill-mainline-role")),
            execution_root: None,
            orchestrator_session_config: None,
            entry_id: "timeline-dispatch-skill-mainline-role".to_string(),
            timeline_message: "使用 browser Skill 创建 explorer 子代理".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            browser_node_selections: Vec::new(),
            created_session: false,
            mission_title: "Skill 子代理继承".to_string(),
            task_title: "Skill 子代理继承".to_string(),
            trimmed_text: Some("使用 browser Skill 创建 explorer 子代理".to_string()),
            execution_goal: Some("创建 explorer 子代理并等待结果".to_string()),
            task_tier: TaskTier::ExecutionChain,
            collaboration_mode: CollaborationMode::Auto,
            access_profile: AccessProfile::FullAccess,
            skill_name: Some("stellarlinkco/myclaude/skills/browser".to_string()),
            goal_mode: false,
            target_role: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            replace_turn_id: None,
            required_tool_chain: Vec::new(),
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: None,
            denied_tools: Vec::new(),
            user_message_metadata: Default::default(),
            turn_origin: DispatchTurnOrigin::User,
        };
        let runtime = DispatchSubmissionRuntime {
            session_store: &session_store,
            task_store: &task_store,
            execution_registry: &execution_registry,
            event_bus: &event_bus,
            agent_role_registry: &agent_role_registry,
            spawn_graph: &spawn_graph,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: None,
        };

        let graph = run_dispatch_submission(&runtime, &request)
            .expect("dispatch submission should build graph");
        let action_task = task_store
            .get_task(&graph.action_task_id)
            .expect("action task should be persisted in TaskStore");

        assert_eq!(
            action_task.executor_binding_target_role(),
            Some("coordinator"),
            "Skill 只决定本轮执行方法，不能把主线入口降级为不能创建子代理的 worker role"
        );
        assert_eq!(
            action_task.executor_binding_active_skill_id(),
            Some("stellarlinkco/myclaude/skills/browser"),
            "主线保持 coordinator 时仍必须保留完整 Skill ID"
        );
    }

    #[test]
    fn dispatch_submission_persists_active_skill_id_on_action_task() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-dispatch-active-skill");

        session_store
            .create_session(session_id.clone(), "dispatch active skill")
            .expect("session should be creatable");

        let request = DispatchSubmissionRequest {
            accepted_at: UtcMillis(3_500),
            session_id,
            workspace_id: Some(WorkspaceId::new("workspace-dispatch-active-skill")),
            execution_root: None,
            orchestrator_session_config: None,
            entry_id: "timeline-dispatch-active-skill".to_string(),
            timeline_message: "使用代码审查 skill 检查当前改动".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            browser_node_selections: Vec::new(),
            created_session: false,
            mission_title: "代码审查".to_string(),
            task_title: "代码审查".to_string(),
            trimmed_text: Some("使用代码审查 skill 检查当前改动".to_string()),
            execution_goal: Some("检查当前改动并给出问题列表".to_string()),
            task_tier: TaskTier::ExecutionChain,
            collaboration_mode: CollaborationMode::Auto,
            access_profile: AccessProfile::Restricted,
            skill_name: Some("code-review".to_string()),
            goal_mode: false,
            target_role: Some("reviewer".to_string()),
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            replace_turn_id: None,
            required_tool_chain: Vec::new(),
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: None,
            denied_tools: Vec::new(),
            user_message_metadata: Default::default(),
            turn_origin: DispatchTurnOrigin::User,
        };
        let runtime = DispatchSubmissionRuntime {
            session_store: &session_store,
            task_store: &task_store,
            execution_registry: &execution_registry,
            event_bus: &event_bus,
            agent_role_registry: &agent_role_registry,
            spawn_graph: &spawn_graph,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: None,
        };

        let graph = run_dispatch_submission(&runtime, &request)
            .expect("dispatch submission should build graph");
        let action_task = task_store
            .get_task(&graph.action_task_id)
            .expect("action task should be persisted in TaskStore");

        assert_eq!(
            action_task.executor_binding_active_skill_id(),
            Some("code-review"),
            "active skill 必须进入 Task executor_binding，任务重跑才能恢复同一 skill 上下文"
        );
        assert_eq!(
            action_task
                .executor_binding
                .as_ref()
                .and_then(|binding| binding.active_skill_id.as_deref()),
            Some("code-review"),
            "Task executor_binding 已类型化，不能再写入旧 skill_name 字段"
        );
    }

    #[test]
    fn dispatch_submission_propagates_context_references_to_task_policy_and_turn_metadata() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-dispatch-context-reference");
        session_store
            .create_session(session_id.clone(), "dispatch context reference")
            .expect("session should be creatable");

        let request = DispatchSubmissionRequest {
            accepted_at: UtcMillis(4_000),
            session_id,
            workspace_id: Some(WorkspaceId::new("workspace-dispatch-context-reference")),
            execution_root: None,
            orchestrator_session_config: None,
            entry_id: "timeline-dispatch-context-reference".to_string(),
            timeline_message: "检查引用文件".to_string(),
            images: Vec::new(),
            context_references: vec![SessionContextReference {
                kind: crate::context_reference::SessionContextReferenceKind::File,
                path: std::path::PathBuf::from("/tmp/external/reference.md"),
                name: "reference.md".to_string(),
            }],
            browser_annotation_refs: Vec::new(),
            browser_node_selections: Vec::new(),
            created_session: false,
            mission_title: "检查引用文件".to_string(),
            task_title: "检查引用文件".to_string(),
            trimmed_text: Some("检查引用文件".to_string()),
            execution_goal: Some("读取并分析引用文件".to_string()),
            task_tier: TaskTier::ExecutionChain,
            collaboration_mode: CollaborationMode::Auto,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            goal_mode: false,
            target_role: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            replace_turn_id: None,
            required_tool_chain: Vec::new(),
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: None,
            denied_tools: Vec::new(),
            user_message_metadata: Default::default(),
            turn_origin: DispatchTurnOrigin::User,
        };
        let workspace_root = std::path::PathBuf::from("/tmp/workspace");
        let runtime = DispatchSubmissionRuntime {
            session_store: &session_store,
            task_store: &task_store,
            execution_registry: &execution_registry,
            event_bus: &event_bus,
            agent_role_registry: &agent_role_registry,
            spawn_graph: &spawn_graph,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: Some(&workspace_root),
        };

        let graph = run_dispatch_submission(&runtime, &request)
            .expect("dispatch submission should propagate context reference");
        let task = task_store
            .get_task(&graph.action_task_id)
            .expect("action task should exist");
        let policy = task
            .policy_snapshot
            .as_ref()
            .expect("task policy should exist");
        assert_eq!(
            policy.allowed_paths,
            vec![
                "/tmp/workspace".to_string(),
                "/tmp/external/reference.md".to_string()
            ]
        );
        assert_eq!(
            policy.read_only_paths,
            vec!["/tmp/external/reference.md".to_string()]
        );
        assert!(
            task.input_refs
                .iter()
                .any(|value| value.contains("/tmp/external/reference.md"))
        );
        let user_item = graph
            .active_execution_chain
            .as_ref()
            .and_then(|chain| chain.current_turn.as_ref())
            .and_then(|turn| turn.items.first())
            .expect("canonical user item should exist");
        assert!(user_item.metadata.contains_key("contextReferences"));
    }

    #[test]
    fn goal_continuation_uses_root_execution_chain_without_faking_user_message() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-goal-continuation-dispatch");
        session_store
            .create_session(session_id.clone(), "goal continuation dispatch")
            .expect("session should be creatable");
        let (_, orchestrator_thread_id) =
            session_store.ensure_session_mission(&session_id, UtcMillis(4_999), || {
                MissionId::new("mission-goal-continuation-dispatch")
            });
        let goal = session_store
            .create_goal(
                session_id.clone(),
                orchestrator_thread_id,
                "task-goal-creator",
                "完成验收",
                AccessProfile::Restricted,
                None,
            )
            .expect("goal should be creatable");
        let request = DispatchSubmissionRequest {
            accepted_at: UtcMillis(5_000),
            session_id: session_id.clone(),
            workspace_id: Some(WorkspaceId::new("workspace-goal-continuation-dispatch")),
            execution_root: None,
            orchestrator_session_config: None,
            entry_id: "timeline-goal-continuation-dispatch".to_string(),
            timeline_message: "目标自动推进: 完成验收".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            browser_node_selections: Vec::new(),
            created_session: false,
            mission_title: "目标自动推进".to_string(),
            task_title: "执行: 目标自动推进".to_string(),
            trimmed_text: None,
            execution_goal: Some("先读取当前目标，再继续推进".to_string()),
            task_tier: TaskTier::ExecutionChain,
            collaboration_mode: CollaborationMode::Auto,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            goal_mode: true,
            target_role: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            replace_turn_id: None,
            required_tool_chain: vec!["get_goal".to_string()],
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: None,
            denied_tools: Vec::new(),
            user_message_metadata: Default::default(),
            turn_origin: DispatchTurnOrigin::GoalContinuation(goal.goal_id.clone()),
        };
        let runtime = DispatchSubmissionRuntime {
            session_store: &session_store,
            task_store: &task_store,
            execution_registry: &execution_registry,
            event_bus: &event_bus,
            agent_role_registry: &agent_role_registry,
            spawn_graph: &spawn_graph,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: None,
        };

        let graph = run_dispatch_submission(&runtime, &request).expect("dispatch should build");
        let chain = graph
            .active_execution_chain
            .as_ref()
            .expect("execution chain should exist");
        let turn = chain
            .current_turn
            .as_ref()
            .expect("continuation turn should exist");
        assert!(turn.user_message.is_none());
        assert_eq!(turn.items.len(), 1);
        assert_eq!(turn.items[0].kind, "assistant_phase");
        assert_eq!(
            turn.items[0]
                .metadata
                .get("renderable")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        let root_task = task_store
            .get_task(&graph.root_task_id)
            .expect("root task should exist");
        assert_eq!(
            root_task.executor_binding_target_role(),
            Some("coordinator")
        );
        assert_eq!(root_task.required_tool_chain(), ["get_goal"]);
        let root_task_id = graph.root_task_id.clone();
        accept_dispatch_submission(
            &session_store,
            &task_store,
            &execution_registry,
            request,
            graph,
        )
        .expect("continuation should be accepted");
        let accepted_goal = session_store
            .current_goal(&session_id)
            .expect("goal should remain visible");
        assert_eq!(
            accepted_goal.continuation.phase,
            GoalContinuationPhase::Running
        );
        assert_eq!(
            accepted_goal.continuation.turn_id.as_deref(),
            Some(root_task_id.as_str())
        );
        assert!(
            session_store
                .runtime_sidecar(&session_id)
                .and_then(|sidecar| sidecar.current_turn)
                .is_some()
        );
    }

    #[test]
    fn paused_goal_rejects_built_continuation_without_leaving_pending_turn() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-goal-continuation-pause-race");
        session_store
            .create_session(session_id.clone(), "goal continuation pause race")
            .expect("session should be creatable");
        let (_, orchestrator_thread_id) =
            session_store.ensure_session_mission(&session_id, UtcMillis(5_999), || {
                MissionId::new("mission-goal-continuation-pause-race")
            });
        let goal = session_store
            .create_goal(
                session_id.clone(),
                orchestrator_thread_id,
                "task-goal-creator",
                "验证暂停竞态",
                AccessProfile::Restricted,
                None,
            )
            .expect("goal should be creatable");
        let request = DispatchSubmissionRequest {
            accepted_at: UtcMillis(6_000),
            session_id: session_id.clone(),
            workspace_id: Some(WorkspaceId::new("workspace-goal-continuation-pause-race")),
            execution_root: None,
            orchestrator_session_config: None,
            entry_id: "timeline-goal-continuation-pause-race".to_string(),
            timeline_message: "目标自动推进: 验证暂停竞态".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            browser_node_selections: Vec::new(),
            created_session: false,
            mission_title: "目标自动推进".to_string(),
            task_title: "执行: 目标自动推进".to_string(),
            trimmed_text: None,
            execution_goal: Some("先读取当前目标，再继续推进".to_string()),
            task_tier: TaskTier::ExecutionChain,
            collaboration_mode: CollaborationMode::Auto,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            goal_mode: true,
            target_role: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            replace_turn_id: None,
            required_tool_chain: vec!["get_goal".to_string()],
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: None,
            denied_tools: Vec::new(),
            user_message_metadata: Default::default(),
            turn_origin: DispatchTurnOrigin::GoalContinuation(goal.goal_id.clone()),
        };
        let runtime = DispatchSubmissionRuntime {
            session_store: &session_store,
            task_store: &task_store,
            execution_registry: &execution_registry,
            event_bus: &event_bus,
            agent_role_registry: &agent_role_registry,
            spawn_graph: &spawn_graph,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: None,
        };

        let graph = run_dispatch_submission(&runtime, &request).expect("dispatch should build");
        let root_task_id = graph.root_task_id.clone();
        let mut clear_request = request.clone();
        clear_request.accepted_at = UtcMillis(6_001);
        clear_request.entry_id = "timeline-goal-continuation-clear-race".to_string();
        clear_request.timeline_message = "目标自动推进: 验证清除竞态".to_string();
        session_store
            .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, None)
            .expect("goal should pause before acceptance");

        assert!(
            accept_dispatch_submission(
                &session_store,
                &task_store,
                &execution_registry,
                request,
                graph,
            )
            .is_err()
        );
        assert!(task_store.get_task(&root_task_id).is_none());
        assert!(execution_registry.get(&root_task_id).is_none());
        assert!(
            session_store
                .runtime_sidecar(&session_id)
                .and_then(|sidecar| sidecar.current_turn)
                .is_none()
        );
        assert!(
            session_store
                .timeline_for_session(&session_id)
                .iter()
                .all(|entry| entry.entry_id != "timeline-goal-continuation-pause-race")
        );
        let paused = session_store
            .current_goal(&session_id)
            .expect("paused goal should remain");
        assert_eq!(paused.status, GoalStatus::Paused);
        assert_eq!(paused.continuation.phase, GoalContinuationPhase::Idle);

        let clear_graph = run_dispatch_submission(&runtime, &clear_request)
            .expect("dispatch should build before goal clear");
        let clear_root_task_id = clear_graph.root_task_id.clone();
        session_store
            .clear_goal_with_plan(&session_id, &goal.goal_id, paused.control_revision, None)
            .expect("goal should clear before acceptance");
        assert!(
            accept_dispatch_submission(
                &session_store,
                &task_store,
                &execution_registry,
                clear_request,
                clear_graph,
            )
            .is_err()
        );
        assert!(task_store.get_task(&clear_root_task_id).is_none());
        assert!(execution_registry.get(&clear_root_task_id).is_none());
        assert!(session_store.current_goal(&session_id).is_none());
        assert!(
            session_store
                .runtime_sidecar(&session_id)
                .and_then(|sidecar| sidecar.current_turn)
                .is_none()
        );
        assert!(
            session_store
                .timeline_for_session(&session_id)
                .iter()
                .all(|entry| entry.entry_id != "timeline-goal-continuation-clear-race")
        );
    }

    #[test]
    fn worker_thread_conflict_rolls_back_new_coordinator_task_and_registry() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-worker-thread-conflict-rollback");
        session_store
            .create_session(session_id.clone(), "worker thread conflict rollback")
            .expect("session should be creatable");
        let request = rollback_test_request(session_id.clone(), UtcMillis(7_100), "executor");
        let runtime = DispatchSubmissionRuntime {
            session_store: &session_store,
            task_store: &task_store,
            execution_registry: &execution_registry,
            event_bus: &event_bus,
            agent_role_registry: &agent_role_registry,
            spawn_graph: &spawn_graph,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: None,
        };
        let prepared = validate_dispatch_request(&runtime, &request, true)
            .expect("request should validate before conflict injection");
        session_store
            .register_thread(ExecutionThread {
                thread_id: prepared.worker_thread_id.clone(),
                session_id: session_id.clone(),
                mission_id: prepared.mission_id.clone(),
                role_id: "executor".to_string(),
                worker_instance_id: WorkerId::new("worker-existing-conflict"),
                status: ExecutionThreadStatus::Idle,
                created_at: UtcMillis(7_000),
                last_used_at: UtcMillis(7_000),
                observed_context_window_tokens: None,
                handled_task_ids: vec![TaskId::new("task-existing-conflict")],
                message_history: Vec::new(),
            })
            .expect("conflicting thread should register");

        let error = match run_dispatch_submission(&runtime, &request) {
            Ok(_) => panic!("duplicate worker thread id must reject materialization"),
            Err(error) => error,
        };

        assert!(error.into_message().contains("拒绝重复注册"));
        assert!(task_store.get_task(&prepared.action_task_id).is_none());
        assert!(execution_registry.get(&prepared.action_task_id).is_none());
        assert!(
            session_store
                .orchestrator_thread_for_session(&session_id)
                .is_none(),
            "materialize 创建的 coordinator 必须随失败一起回收"
        );
        let threads = session_store.thread_registry_snapshot(&session_id);
        assert_eq!(threads.len(), 1);
        assert_eq!(
            threads[0].worker_instance_id.as_str(),
            "worker-existing-conflict"
        );
    }

    #[test]
    fn materialize_rollback_restores_full_resources_and_keeps_plan_revision_monotonic() {
        let session_store = SessionStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let session_id = SessionId::new("session-materialize-owned-resource-rollback");
        let mission_id = MissionId::new("mission-materialize-owned-resource-rollback");
        let task_id = TaskId::new("task-materialize-owned-resource-rollback");
        let worker_id = WorkerId::new("worker-materialize-owned-resource-rollback");
        let turn_id = "turn-materialize-owned-resource-rollback";
        session_store
            .create_session(session_id.clone(), "materialize owned resource rollback")
            .expect("session should be creatable");
        let (_, coordinator_thread_id) =
            session_store
                .ensure_session_mission(&session_id, UtcMillis(8_000), || mission_id.clone());
        let coordinator_before = session_store
            .orchestrator_thread_for_session(&session_id)
            .expect("coordinator should exist");
        let coordinator_after = session_store
            .activate_thread_checked(&coordinator_thread_id, &task_id, UtcMillis(8_100))
            .expect("coordinator should activate");

        let worker_thread_id = ThreadId::new("thread-materialize-owned-resource-rollback");
        let worker_thread = ExecutionThread {
            thread_id: worker_thread_id.clone(),
            session_id: session_id.clone(),
            mission_id: mission_id.clone(),
            role_id: "executor".to_string(),
            worker_instance_id: worker_id.clone(),
            status: ExecutionThreadStatus::Active,
            created_at: UtcMillis(8_000),
            last_used_at: UtcMillis(8_100),
            observed_context_window_tokens: None,
            handled_task_ids: vec![task_id.clone()],
            message_history: vec![ThreadChatMessage {
                role: "user".to_string(),
                content: Some("恢复上下文".to_string()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: Vec::new(),
            }],
        };
        session_store
            .register_thread(worker_thread.clone())
            .expect("worker thread should register");
        session_store
            .install_thread_context_checkpoint_checked(
                &worker_thread_id,
                ThreadContextCheckpoint {
                    thread_id: worker_thread_id.clone(),
                    checkpoint_id: "checkpoint-materialize-rollback".to_string(),
                    source_message_count: 1,
                    summary_message: ThreadChatMessage {
                        role: "system".to_string(),
                        content: Some("恢复摘要".to_string()),
                        images: Vec::new(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        provider_context: Vec::new(),
                    },
                    reason: "test".to_string(),
                    original_token_estimate: 100,
                    checkpoint_token_estimate: 20,
                    created_at: UtcMillis(8_100),
                    generation: 1,
                    source_fingerprint: "fingerprint-materialize-rollback".to_string(),
                    model_provider: None,
                    model: None,
                    binding_revision: None,
                    projected_request_tokens: 20,
                    context_window_limit_tokens: None,
                    preserved_tail_message_count: 0,
                    file_fact_versions: Vec::new(),
                },
                UtcMillis(8_100),
            )
            .expect("worker checkpoint should install");

        let plan_store = magi_plan::PlanStore::from_store(&session_store, session_id.clone());
        let original_plan = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: None,
                expected_goal_control_revision: None,
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("materialize-step".to_string()),
                    step: "验证装配回滚".to_string(),
                    status: magi_core::PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should create");
        let (plan_before_binding, plan_after_binding) = plan_store
            .bind_task_for_materialization(task_id.clone(), original_plan.items[0].item_id.clone())
            .expect("plan binding should succeed")
            .expect("new task binding should mutate plan");
        execution_registry
            .insert(
                task_id.clone(),
                rollback_test_execution_plan(
                    &session_id,
                    &mission_id,
                    &task_id,
                    &worker_id,
                    &worker_thread_id,
                    turn_id,
                ),
            )
            .expect("execution plan should register");

        let mut rollback = MaterializeRollback::new(
            &session_store,
            &execution_registry,
            session_id.clone(),
            task_id.clone(),
            turn_id.to_string(),
        );
        rollback.record_created_thread(worker_thread);
        rollback.record_coordinator_before(coordinator_before.clone());
        rollback.record_coordinator_after(coordinator_after);
        rollback.record_plan_materialization(PlanMaterialization {
            original_plan: Some(plan_before_binding.clone()),
            current_revision: plan_after_binding.revision,
        });
        rollback.record_registry_inserted();

        assert!(rollback.rollback_now().is_empty());
        assert!(execution_registry.get(&task_id).is_none());
        assert_eq!(
            session_store
                .orchestrator_thread_for_session(&session_id)
                .expect("coordinator should remain"),
            coordinator_before
        );
        assert!(
            session_store
                .thread_registry_snapshot(&session_id)
                .iter()
                .all(|thread| thread.thread_id != worker_thread_id)
        );
        assert!(
            session_store
                .thread_context_checkpoint(&worker_thread_id)
                .is_none(),
            "回收恢复 thread 时必须同步删除 context checkpoint"
        );
        let restored_plan = plan_store.snapshot().expect("plan should remain");
        assert_eq!(restored_plan.items, plan_before_binding.items);
        assert_eq!(restored_plan.state, plan_before_binding.state);
        assert_eq!(
            restored_plan.task_bindings,
            plan_before_binding.task_bindings
        );
        assert_eq!(
            restored_plan.task_statuses,
            plan_before_binding.task_statuses
        );
        assert_eq!(restored_plan.revision, plan_after_binding.revision);
        assert!(restored_plan.revision > plan_before_binding.revision);
    }

    #[test]
    fn accepted_runner_start_cleanup_is_idempotent_for_registry_worker_and_plan_binding() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-accepted-runner-cleanup");
        session_store
            .create_session(session_id.clone(), "accepted runner cleanup")
            .expect("session should be creatable");
        let plan_store = magi_plan::PlanStore::from_store(&session_store, session_id.clone());
        plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: None,
                expected_goal_control_revision: None,
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("accepted-runner-step".to_string()),
                    step: "验证 runner 启动失败清理".to_string(),
                    status: magi_core::PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should create");
        let request = rollback_test_request(session_id.clone(), UtcMillis(9_100), "executor");
        let runtime = DispatchSubmissionRuntime {
            session_store: &session_store,
            task_store: &task_store,
            execution_registry: &execution_registry,
            event_bus: &event_bus,
            agent_role_registry: &agent_role_registry,
            spawn_graph: &spawn_graph,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: None,
        };
        let pending = prepare_pending_dispatch_submission(&runtime, &request)
            .expect("pending dispatch should prepare");
        let graph = DispatchSubmissionGraph::new(
            pending.task.task_id.clone(),
            pending.task.task_id,
            Some(pending.active_execution_chain),
        );
        let accepted = accept_dispatch_submission(
            &session_store,
            &task_store,
            &execution_registry,
            request.clone(),
            graph,
        )
        .expect("dispatch should be accepted");
        materialize_dispatch_submission(&runtime, &request, &accepted.turn_id)
            .expect("accepted dispatch should materialize");
        let worker_thread_id = session_store
            .active_execution_chain(&session_id)
            .and_then(|chain| {
                chain
                    .branches
                    .first()
                    .map(|branch| branch.thread_id.clone())
            })
            .expect("accepted chain should contain worker thread");
        assert!(execution_registry.get(&accepted.root_task_id).is_some());
        assert!(
            plan_store
                .snapshot()
                .expect("plan should remain")
                .task_bindings
                .contains_key(&accepted.root_task_id)
        );

        for _ in 0..2 {
            cleanup_materialized_dispatch_submission_if_not_started(
                &session_store,
                &execution_registry,
                &session_id,
                &accepted.root_task_id,
                &accepted.turn_id,
            )
            .expect("runner start cleanup should be idempotent");
        }

        assert!(execution_registry.get(&accepted.root_task_id).is_none());
        assert!(
            session_store
                .thread_registry_snapshot(&session_id)
                .iter()
                .all(|thread| thread.thread_id != worker_thread_id)
        );
        assert!(
            !plan_store
                .snapshot()
                .expect("plan should remain")
                .task_bindings
                .contains_key(&accepted.root_task_id)
        );
        assert_eq!(
            task_store
                .get_task(&accepted.root_task_id)
                .expect("accepted task fact must remain")
                .status,
            TaskStatus::Pending
        );
        assert_eq!(
            session_store
                .runtime_sidecar(&session_id)
                .and_then(|sidecar| sidecar.current_turn)
                .expect("accepted turn fact must remain")
                .turn_id,
            accepted.turn_id
        );
    }

    #[test]
    fn accepted_materialize_rejects_registry_owned_by_another_execution_identity() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-stale-materialize-registry");
        session_store
            .create_session(session_id.clone(), "stale materialize registry")
            .expect("session should be creatable");
        let request = rollback_test_request(session_id.clone(), UtcMillis(9_200), "executor");
        let runtime = DispatchSubmissionRuntime {
            session_store: &session_store,
            task_store: &task_store,
            execution_registry: &execution_registry,
            event_bus: &event_bus,
            agent_role_registry: &agent_role_registry,
            spawn_graph: &spawn_graph,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: None,
        };
        let pending = prepare_pending_dispatch_submission(&runtime, &request)
            .expect("pending dispatch should prepare");
        let prepared = validate_dispatch_request(&runtime, &request, false)
            .expect("accepted request should validate");
        let graph = DispatchSubmissionGraph::new(
            pending.task.task_id.clone(),
            pending.task.task_id,
            Some(pending.active_execution_chain),
        );
        let accepted = accept_dispatch_submission(
            &session_store,
            &task_store,
            &execution_registry,
            request.clone(),
            graph,
        )
        .expect("dispatch should be accepted");
        execution_registry
            .insert(
                accepted.root_task_id.clone(),
                rollback_test_execution_plan(
                    &session_id,
                    &prepared.mission_id,
                    &accepted.root_task_id,
                    &prepared.worker_id,
                    &prepared.worker_thread_id,
                    &accepted.turn_id,
                ),
            )
            .expect("conflicting execution plan should register");

        let error = materialize_dispatch_submission(&runtime, &request, &accepted.turn_id)
            .expect_err("different Turn registry entry must not count as idempotent success");

        assert!(error.into_message().contains("已被其他执行身份装配"));
        assert_eq!(
            execution_registry
                .turn_id(&accepted.root_task_id)
                .as_deref(),
            Some(accepted.turn_id.as_str())
        );
        assert!(
            session_store
                .thread_registry_snapshot(&session_id)
                .is_empty(),
            "身份冲突必须在创建执行 thread 前被拒绝"
        );
    }

    #[test]
    fn active_chain_writeback_failure_rolls_back_materialized_execution_resources() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-chain-writeback-rollback");
        session_store
            .create_session(session_id.clone(), "chain writeback rollback")
            .expect("session should be creatable");
        let plan_store = magi_plan::PlanStore::from_store(&session_store, session_id.clone());
        let original_plan = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: None,
                expected_goal_control_revision: None,
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("chain-writeback-step".to_string()),
                    step: "验证 chain 写回失败回滚".to_string(),
                    status: magi_core::PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should create");
        let request = rollback_test_request(session_id.clone(), UtcMillis(9_300), "executor");
        let runtime = DispatchSubmissionRuntime {
            session_store: &session_store,
            task_store: &task_store,
            execution_registry: &execution_registry,
            event_bus: &event_bus,
            agent_role_registry: &agent_role_registry,
            spawn_graph: &spawn_graph,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: None,
        };
        let pending = prepare_pending_dispatch_submission(&runtime, &request)
            .expect("pending dispatch should prepare");
        let graph = DispatchSubmissionGraph::new(
            pending.task.task_id.clone(),
            pending.task.task_id,
            Some(pending.active_execution_chain),
        );
        let accepted = accept_dispatch_submission(
            &session_store,
            &task_store,
            &execution_registry,
            request.clone(),
            graph,
        )
        .expect("dispatch should be accepted");
        let prepared = validate_dispatch_request(&runtime, &request, false)
            .expect("accepted request should validate");
        let mut changed_chain = session_store
            .active_execution_chain(&session_id)
            .expect("accepted chain should exist");
        changed_chain
            .current_turn
            .as_mut()
            .and_then(|turn| turn.items.first_mut())
            .expect("accepted user item should exist")
            .metadata
            .insert("forceWriteback".to_string(), serde_json::json!(true));
        session_store.install_canonical_event_writer(Arc::new(RejectingCanonicalWriter));

        let error = match materialize_execution(&runtime, &request, &prepared, Some(changed_chain))
        {
            Ok(_) => panic!("canonical write rejection must fail materialization"),
            Err(error) => error,
        };

        assert!(
            error
                .into_message()
                .contains("写回 materialize execution chain 失败")
        );
        assert!(execution_registry.get(&accepted.root_task_id).is_none());
        assert!(
            session_store
                .thread_registry_snapshot(&session_id)
                .is_empty(),
            "写回失败后 coordinator 和 worker thread 都必须回收"
        );
        let restored_plan = plan_store.snapshot().expect("plan should remain");
        assert_eq!(restored_plan.items, original_plan.items);
        assert!(
            !restored_plan
                .task_bindings
                .contains_key(&accepted.root_task_id)
        );
        assert!(restored_plan.revision > original_plan.revision);
        assert!(
            session_store
                .canonical_turns_for_session(&session_id)
                .into_iter()
                .flat_map(|turn| turn.items)
                .all(|item| !item.metadata.contains_key("forceWriteback")),
            "失败的 chain 写回不能污染 canonical projection"
        );
    }

    #[test]
    fn rejected_dispatch_cleanup_does_not_remove_foreign_registry_generation() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-rejected-cleanup-generation");
        session_store
            .create_session(session_id.clone(), "rejected cleanup generation")
            .expect("session should be creatable");
        let request = rollback_test_request(session_id.clone(), UtcMillis(9_400), "executor");
        let runtime = DispatchSubmissionRuntime {
            session_store: &session_store,
            task_store: &task_store,
            execution_registry: &execution_registry,
            event_bus: &event_bus,
            agent_role_registry: &agent_role_registry,
            spawn_graph: &spawn_graph,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: None,
        };
        let pending = prepare_pending_dispatch_submission(&runtime, &request)
            .expect("pending dispatch should prepare");
        let prepared = validate_dispatch_request(&runtime, &request, false)
            .expect("pending request should validate");
        let task_id = pending.task.task_id.clone();
        execution_registry
            .insert(
                task_id.clone(),
                rollback_test_execution_plan(
                    &session_id,
                    &prepared.mission_id,
                    &task_id,
                    &prepared.worker_id,
                    &prepared.worker_thread_id,
                    "turn-foreign-generation",
                ),
            )
            .expect("foreign generation should register");
        let graph = DispatchSubmissionGraph::new(
            task_id.clone(),
            task_id.clone(),
            Some(pending.active_execution_chain),
        );

        cleanup_rejected_dispatch(Some(&task_store), &execution_registry, graph)
            .expect("rejected dispatch facts should clean up");

        assert!(task_store.get_task(&task_id).is_none());
        assert_eq!(
            execution_registry.turn_id(&task_id).as_deref(),
            Some("turn-foreign-generation"),
            "拒绝旧 Turn 时不能删除同 task id 下的新执行代际"
        );
    }
}
