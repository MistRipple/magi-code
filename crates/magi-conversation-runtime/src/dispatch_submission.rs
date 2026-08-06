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
    AccessProfile, DomainError, ExecutionOwnership, GoalId, MissionId, PlanItemId, SessionId,
    TaskCompletionContract, TaskExecutionTarget, TaskExecutorBinding, TaskId, TaskKind,
    TaskRecoveryCheckpoint, TaskStatus, TaskTier, UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::{EventContext, InMemoryEventBus, task_events};
use magi_orchestrator::{
    DispatchMemoryExtractionInput, ExecutionWritebackPlans, task_store::TaskStore,
};
use magi_session_store::{
    ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
    ActiveExecutionTurn, ActiveExecutionTurnItem, CanonicalTurn, CanonicalTurnItemKind,
    SessionStore, ThreadChatMessage, ThreadContextCheckpoint, TimelineEntryInput,
    TimelineEntryKind,
};
use magi_spawn_graph::SpawnGraph;

use crate::session_thread;

use crate::context_reference::{
    SessionContextReference, browser_annotation_artifact_paths,
    browser_annotation_reference_input_refs, browser_annotation_references_metadata,
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
}

/// Root coordinator Turn 的来源。
///
/// 用户输入和 Goal 自动续跑都使用同一条 ExecutionChain；区别只体现在持久化的
/// 时间线及 canonical item，可见性不能再由另一套 session runner 决定。
#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug)]
pub struct DispatchSubmissionRequest {
    pub accepted_at: UtcMillis,
    pub session_id: SessionId,
    pub workspace_id: Option<WorkspaceId>,
    pub entry_id: String,
    pub timeline_message: String,
    pub images: Vec<SessionTurnImage>,
    pub context_references: Vec<SessionContextReference>,
    /// 已由 Magi API 的 BrowserAuthority 解析并校验的页面标记引用。
    pub browser_annotation_refs: Vec<serde_json::Value>,
    pub created_session: bool,
    pub mission_title: String,
    pub task_title: String,
    pub trimmed_text: Option<String>,
    pub execution_goal: Option<String>,
    pub task_tier: TaskTier,
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
    pub turn_origin: DispatchTurnOrigin,
}

#[derive(Clone, Debug)]
pub struct DispatchSubmissionAccepted {
    pub session_id: SessionId,
    pub entry_id: String,
    pub accepted_at: UtcMillis,
    pub created_session: bool,
    pub root_task_id: TaskId,
    pub action_task_id: TaskId,
    pub user_message_item_id: Option<String>,
    pub runner_started: bool,
    pub superseded_turn: Option<CanonicalTurn>,
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
    graph: &DispatchSubmissionGraph,
) {
    if let Some(chain) = graph.active_execution_chain.as_ref() {
        for branch in &chain.branches {
            let _ = execution_registry.remove(&branch.task_id);
        }
    }
    if let Some(task_store) = task_store {
        let _ = task_store.remove_task(&graph.root_task_id);
    }
}

fn build_task_policy(
    task_tier: TaskTier,
    access_profile: AccessProfile,
    context_references: &[SessionContextReference],
    browser_annotation_refs: &[serde_json::Value],
    workspace_root_path: Option<&Path>,
    denied_tools: Vec<String>,
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
    access_profile: AccessProfile,
    context_references: &'a [SessionContextReference],
    workspace_root_path: Option<&'a Path>,
    required_tool_chain: Vec<String>,
    completion_contract: TaskCompletionContract,
    recovery_checkpoint: Option<TaskRecoveryCheckpoint>,
    denied_tools: Vec<String>,
    plan_item_id: Option<PlanItemId>,
    browser_annotation_refs: &'a [serde_json::Value],
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
        workspace_root_path,
        required_tool_chain,
        completion_contract,
        recovery_checkpoint,
        denied_tools,
        plan_item_id,
        browser_annotation_refs,
    } = input;
    let executor_binding = TaskExecutorBinding::for_role(target_role)
        .with_active_skill_id(active_skill_id.map(str::to_string))
        .with_required_tool_chain(required_tool_chain)
        .with_plan_item_id(plan_item_id);
    let mut input_refs = session_context_reference_input_refs(context_references);
    input_refs.extend(browser_annotation_reference_input_refs(
        browser_annotation_refs,
    ));

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
        runtime_payload: if browser_annotation_refs.is_empty() {
            magi_core::TaskRuntimePayload::None
        } else {
            magi_core::TaskRuntimePayload::BrowserAnnotations {
                references: browser_annotation_refs.to_vec(),
            }
        },
        created_at: now,
        updated_at: now,
    }
}

pub fn run_dispatch_submission(
    runtime: &DispatchSubmissionRuntime<'_>,
    request: &DispatchSubmissionRequest,
) -> Result<DispatchSubmissionGraph, DispatchSubmissionRunError> {
    runtime
        .session_store
        .ensure_current_turn_acceptance_available(&request.session_id)
        .map_err(DispatchSubmissionAcceptError::from_store_error)
        .map_err(|err| match err {
            DispatchSubmissionAcceptError::Conflict { message }
            | DispatchSubmissionAcceptError::Internal { message } => {
                DispatchSubmissionRunError::Internal(message)
            }
        })?;

    let accepted_at = request.accepted_at;
    let session_id = &request.session_id;
    let entry_id = request.entry_id.as_str();
    let trimmed_text = request.trimmed_text.as_deref();
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

    // 恢复来源校验必须先于任何任务、thread 或事件写入。否则无效检查点会在返回错误时
    // 留下没有 execution chain 的 pending task。
    let interrupted_turn_checkpoint = request
        .recovery_checkpoint
        .as_ref()
        .map(|checkpoint| prepare_interrupted_turn_checkpoint(runtime.session_store, checkpoint))
        .transpose()?;
    // Skill 是本轮方法上下文，不是角色路由信号。聊天框进入的主线任务必须保留 coordinator
    // 权限面，具体 worker role 只能由显式 target_role 或后续 agent_spawn 决定。
    let target_role = request.target_role.as_deref().unwrap_or("coordinator");
    if !runtime
        .agent_role_registry
        .role_supports_task_kind(target_role, TaskKind::LocalAgent)
    {
        return Err(DispatchSubmissionRunError::InvalidInput(format!(
            "role {target_role} 不支持 local_agent 任务"
        )));
    }

    let now = UtcMillis::now();
    let (mission_id, orchestrator_thread_id) =
        runtime
            .session_store
            .ensure_session_mission(session_id, now, || {
                MissionId::new(format!("mission-session-action-{}", accepted_at.0))
            });
    let worker_id = WorkerId::new(format!("worker-session-action-{}", accepted_at.0));

    let act_task_id = TaskId::new(format!("task-local-agent-{}", accepted_at.0));

    let task_goal_text = execution_goal.to_string();
    let plan_store =
        magi_plan::PlanStore::from_store(runtime.session_store, request.session_id.clone());
    let plan_item_id = plan_store.active_item_id();
    let task = make_dispatch_task(DispatchTaskInput {
        task_id: act_task_id.clone(),
        mission_id: mission_id.clone(),
        title: request.task_title.clone(),
        goal: task_goal_text.clone(),
        now,
        target_role,
        active_skill_id: request.skill_name.as_deref(),
        task_tier: request.task_tier,
        access_profile: request.access_profile,
        context_references: &request.context_references,
        browser_annotation_refs: &request.browser_annotation_refs,
        workspace_root_path: runtime.workspace_root_path,
        required_tool_chain: request.required_tool_chain.clone(),
        completion_contract: request.completion_contract.clone(),
        recovery_checkpoint: request.recovery_checkpoint.clone(),
        denied_tools: request.denied_tools.clone(),
        plan_item_id: plan_item_id.clone(),
    });
    runtime.task_store.insert_task(task);
    if let Some(plan_item_id) = plan_item_id {
        match plan_store.bind_task(act_task_id.clone(), plan_item_id) {
            Ok(_) => {}
            Err(error) => {
                let _ = runtime.task_store.remove_task(&act_task_id);
                return Err(DispatchSubmissionRunError::Internal(format!(
                    "将主线任务绑定到当前计划阶段失败: {error}"
                )));
            }
        }
    }
    let event =
        task_events::task_submission_created_event(mission_id.as_str(), act_task_id.as_str(), 1)
            .with_context(EventContext {
                mission_id: Some(mission_id.clone()),
                task_id: Some(act_task_id.clone()),
                ..EventContext::default()
            });
    let _ = runtime.event_bus.publish(event);

    let workspace_id = request.workspace_id.clone();
    let execution_chain_ref = format!("session-action-chain-{}", accepted_at.0);
    let worker_thread_id = session_thread::ensure_thread_for_role(
        runtime.session_store,
        session_id,
        &mission_id,
        target_role,
        &worker_id,
        &act_task_id,
        now,
    );
    if let Some(checkpoint) = interrupted_turn_checkpoint {
        install_interrupted_turn_checkpoint(
            runtime.session_store,
            &worker_thread_id,
            checkpoint,
            now,
        );
    }
    let ownership = ExecutionOwnership {
        session_id: Some(session_id.clone()),
        workspace_id: workspace_id.clone(),
        mission_id: Some(mission_id.clone()),
        task_id: Some(act_task_id.clone()),
        worker_id: Some(worker_id.clone()),
        execution_chain_ref: Some(execution_chain_ref.clone()),
    };
    let execution_settings_snapshot = runtime
        .settings_store
        .map(|store| Arc::new(store.execution_snapshot()));
    runtime.execution_registry.insert(
        act_task_id.clone(),
        TaskExecutionPlan::Dispatch {
            target: TaskExecutionTarget {
                mission_id: mission_id.clone(),
                root_task_id: act_task_id.clone(),
                task_id: act_task_id.clone(),
                requested_worker_id: Some(worker_id.clone()),
                recovery_id: None,
                execution_chain_ref: Some(execution_chain_ref.clone()),
            },
            worker_id: worker_id.clone(),
            thread_id: worker_thread_id.clone(),
            is_primary: true,
            session_id: session_id.clone(),
            workspace_id: workspace_id.clone(),
            ownership: ownership.clone(),
            writebacks: ExecutionWritebackPlans::from_session_action_input(
                DispatchMemoryExtractionInput {
                    accepted_at,
                    session_id,
                    timeline_entry_id: entry_id,
                    text: trimmed_text,
                    skill_name: request.skill_name.as_deref(),
                },
            ),
            use_tools: true,
            skill_name: request.skill_name.clone(),
            images: request.images.clone(),
            execution_settings_snapshot,
        },
    );

    let branches = vec![ActiveExecutionBranch {
        task_id: act_task_id.clone(),
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
        skill_name: request.skill_name.clone(),
        is_primary: true,
        thread_id: worker_thread_id.clone(),
    }];
    let request_id = request.request_id.clone();
    let user_message_id = request.user_message_id.clone();
    let placeholder_message_id = request.placeholder_message_id.clone();
    let user_message_item_id = request.turn_origin.creates_user_message_item().then(|| {
        user_message_id
            .clone()
            .unwrap_or_else(|| format!("turn-item-user-{}", accepted_at.0))
    });
    let mut current_turn = ActiveExecutionTurn {
        turn_id: match &request.turn_origin {
            DispatchTurnOrigin::User => format!("turn-session-action-{}", accepted_at.0),
            DispatchTurnOrigin::GoalContinuation(_) => {
                format!("turn-goal-continuation-{}-{}", session_id, accepted_at.0)
            }
        },
        turn_seq: accepted_at.0,
        accepted_at,
        status: "accepted".to_string(),
        completed_at: None,
        user_message: request
            .turn_origin
            .creates_user_message_item()
            .then(|| request.timeline_message.clone()),
        items: user_message_item_id
            .clone()
            .into_iter()
            .map(|item_id| ActiveExecutionTurnItem {
                item_id,
                item_seq: 1,
                kind: "user_message".to_string(),
                status: "completed".to_string(),
                source: "user".to_string(),
                title: None,
                content: Some(request.timeline_message.clone()),
                task_id: Some(act_task_id.clone()),
                worker_id: None,
                role_id: None,
                tool_call_id: None,
                tool_name: None,
                tool_status: None,
                tool_arguments: None,
                tool_result: None,
                tool_error: None,
                request_id: request_id.clone(),
                user_message_id: user_message_id.clone(),
                placeholder_message_id: placeholder_message_id.clone(),
                metadata: {
                    let mut metadata =
                        crate::session_images::session_turn_images_metadata(&request.images);
                    metadata.extend(session_context_references_metadata(
                        &request.context_references,
                    ));
                    metadata.extend(browser_annotation_references_metadata(
                        &request.browser_annotation_refs,
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
                    metadata.insert(
                        "goalMode".to_string(),
                        serde_json::Value::Bool(request.goal_mode),
                    );
                    metadata
                },
                timeline_entry_id: Some(entry_id.to_string()),
                source_thread_id: orchestrator_thread_id.clone(),
            })
            .collect(),
    };
    current_turn.normalize();
    Ok(DispatchSubmissionGraph {
        root_task_id: act_task_id.clone(),
        action_task_id: act_task_id.clone(),
        active_execution_chain: Some(ActiveExecutionChain {
            session_id: request.session_id.clone(),
            mission_id,
            root_task_id: act_task_id,
            execution_chain_ref,
            workspace_id,
            active_branch_task_ids: branches
                .iter()
                .map(|branch| branch.task_id.clone())
                .collect(),
            active_worker_bindings: branches
                .iter()
                .map(|branch| branch.worker_id.clone())
                .collect(),
            branches,
            recovery_ref: None,
            dispatch_context: ActiveExecutionDispatchContext {
                accepted_at,
                entry_id: entry_id.to_string(),
                trimmed_text: trimmed_text.map(str::to_string),
                skill_name: request.skill_name.clone(),
            },
            current_turn: Some(current_turn),
        }),
    })
}

struct InterruptedTurnCheckpointSeed {
    message_history: Vec<ThreadChatMessage>,
    context_checkpoint: Option<ThreadContextCheckpoint>,
}

fn prepare_interrupted_turn_checkpoint(
    session_store: &SessionStore,
    checkpoint: &TaskRecoveryCheckpoint,
) -> Result<InterruptedTurnCheckpointSeed, DispatchSubmissionRunError> {
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
) {
    session_store.replace_thread_messages(destination_thread_id, checkpoint.message_history, now);
    if let Some(source_checkpoint) = checkpoint.context_checkpoint {
        session_store.install_thread_context_checkpoint(
            destination_thread_id,
            magi_session_store::ThreadContextCheckpoint {
                thread_id: destination_thread_id.clone(),
                ..source_checkpoint
            },
            now,
        );
    }
}

pub fn accept_dispatch_submission(
    session_store: &SessionStore,
    task_store: Option<&TaskStore>,
    execution_registry: &TaskExecutionRegistry,
    request: DispatchSubmissionRequest,
    graph: DispatchSubmissionGraph,
) -> Result<DispatchSubmissionAccepted, DispatchSubmissionAcceptError> {
    if let Some(active_execution_chain) = graph.active_execution_chain.clone() {
        let accept_result = if let Some(goal_id) = request.turn_origin.continuation_goal_id() {
            session_store
                .accept_goal_continuation_with_timeline_entry(
                    request.session_id.clone(),
                    goal_id,
                    TimelineEntryInput::new(
                        request.entry_id.clone(),
                        request.turn_origin.timeline_kind(),
                        request.timeline_message.clone(),
                        request.accepted_at,
                    ),
                    active_execution_chain,
                )
                .map(|_| None)
        } else if let Some(replace_turn_id) = request.replace_turn_id.as_deref() {
            session_store
                .replace_current_turn_with_active_execution_chain_and_timeline_entry(
                    request.session_id.clone(),
                    replace_turn_id,
                    TimelineEntryInput::new(
                        request.entry_id.clone(),
                        request.turn_origin.timeline_kind(),
                        request.timeline_message.clone(),
                        request.accepted_at,
                    ),
                    active_execution_chain,
                )
                .map(|(_, _, superseded_turn)| Some(superseded_turn))
        } else {
            session_store
                .accept_active_execution_chain_with_timeline_entry(
                    request.session_id.clone(),
                    TimelineEntryInput::new(
                        request.entry_id.clone(),
                        request.turn_origin.timeline_kind(),
                        request.timeline_message.clone(),
                        request.accepted_at,
                    ),
                    active_execution_chain,
                )
                .map(|_| None)
        };
        let superseded_turn = match accept_result {
            Ok(superseded_turn) => superseded_turn,
            Err(error) => {
                cleanup_rejected_dispatch(task_store, execution_registry, &graph);
                let plan_store =
                    magi_plan::PlanStore::from_store(session_store, request.session_id.clone());
                if let Err(unbind_error) = plan_store.unbind_task(&graph.root_task_id) {
                    tracing::warn!(
                        task_id = %graph.root_task_id,
                        error = %unbind_error,
                        "拒绝的派发清理计划任务绑定失败"
                    );
                }
                return Err(DispatchSubmissionAcceptError::from_store_error(error));
            }
        };

        let user_message_item_id = request.turn_origin.creates_user_message_item().then(|| {
            request
                .user_message_id
                .clone()
                .unwrap_or_else(|| format!("turn-item-user-{}", request.accepted_at.0))
        });

        return Ok(DispatchSubmissionAccepted {
            session_id: request.session_id,
            entry_id: request.entry_id,
            accepted_at: request.accepted_at,
            created_session: request.created_session,
            root_task_id: graph.root_task_id,
            action_task_id: graph.action_task_id,
            user_message_item_id,
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

    Ok(DispatchSubmissionAccepted {
        session_id: request.session_id,
        entry_id: request.entry_id,
        accepted_at: request.accepted_at,
        created_session: request.created_session,
        root_task_id: graph.root_task_id,
        action_task_id: graph.action_task_id,
        user_message_item_id,
        runner_started: false,
        superseded_turn: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_session_store::{
        ExecutionThread, ExecutionThreadStatus, GoalContinuationPhase, GoalStatus,
        ThreadChatMessage, ThreadChatToolCall, ThreadChatToolFunction,
    };

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
        session_store.register_thread(ExecutionThread {
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
        });

        let request = DispatchSubmissionRequest {
            accepted_at: UtcMillis(2_000),
            session_id: session_id.clone(),
            workspace_id: Some(WorkspaceId::new("workspace-dispatch-fresh-thread")),
            entry_id: "timeline-dispatch-fresh-thread".to_string(),
            timeline_message: "创建当前任务文件".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            created_session: false,
            mission_title: "当前任务推进".to_string(),
            task_title: "当前任务推进".to_string(),
            trimmed_text: Some("创建 task-system-e2e.md".to_string()),
            execution_goal: Some("创建 task-system-e2e.md 并写入当前 marker".to_string()),
            task_tier: TaskTier::ExecutionChain,
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
            entry_id: "timeline-dispatch-browser-annotation".to_string(),
            timeline_message: "根据浏览器标记检查页面".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: vec![annotation.clone()],
            created_session: false,
            mission_title: "浏览器标记检查".to_string(),
            task_title: "浏览器标记检查".to_string(),
            trimmed_text: Some("根据浏览器标记检查页面".to_string()),
            execution_goal: Some("核对标记位置并报告问题".to_string()),
            task_tier: TaskTier::ExecutionChain,
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
        session_store.register_thread(ExecutionThread {
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
        });
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
            entry_id: "timeline-dispatch-resume-checkpoint".to_string(),
            timeline_message: "继续".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            created_session: false,
            mission_title: "继续中断任务".to_string(),
            task_title: "继续: 画当前项目流程图".to_string(),
            trimmed_text: Some("继续".to_string()),
            execution_goal: Some("继续原始流程图任务".to_string()),
            task_tier: TaskTier::ExecutionChain,
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
            entry_id: "timeline-invalid-resume-checkpoint".to_string(),
            timeline_message: "继续".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            created_session: false,
            mission_title: "继续中断任务".to_string(),
            task_title: "继续: 验证恢复原子性".to_string(),
            trimmed_text: Some("继续".to_string()),
            execution_goal: Some("继续验证恢复原子性".to_string()),
            task_tier: TaskTier::ExecutionChain,
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
            entry_id: "timeline-exec-chain-tier".to_string(),
            timeline_message: "修复明确 bug 并跑相关验证".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            created_session: false,
            mission_title: "修复 bug + 验证".to_string(),
            task_title: "修复 bug + 验证".to_string(),
            trimmed_text: Some("修复明确 bug 并跑相关验证".to_string()),
            execution_goal: Some("定位并修复 bug、再跑相关验证命令".to_string()),
            task_tier: TaskTier::ExecutionChain,
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
            entry_id: "timeline-plan-root-binding".to_string(),
            timeline_message: "继续计划".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            created_session: false,
            mission_title: "继续计划".to_string(),
            task_title: "继续计划".to_string(),
            trimmed_text: Some("继续计划".to_string()),
            execution_goal: Some("完成当前计划全部阶段".to_string()),
            task_tier: TaskTier::ExecutionChain,
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
            entry_id: "timeline-dispatch-skill-mainline-role".to_string(),
            timeline_message: "使用 browser Skill 创建 explorer 子代理".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            created_session: false,
            mission_title: "Skill 子代理继承".to_string(),
            task_title: "Skill 子代理继承".to_string(),
            trimmed_text: Some("使用 browser Skill 创建 explorer 子代理".to_string()),
            execution_goal: Some("创建 explorer 子代理并等待结果".to_string()),
            task_tier: TaskTier::ExecutionChain,
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
            entry_id: "timeline-dispatch-active-skill".to_string(),
            timeline_message: "使用代码审查 skill 检查当前改动".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            created_session: false,
            mission_title: "代码审查".to_string(),
            task_title: "代码审查".to_string(),
            trimmed_text: Some("使用代码审查 skill 检查当前改动".to_string()),
            execution_goal: Some("检查当前改动并给出问题列表".to_string()),
            task_tier: TaskTier::ExecutionChain,
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
            entry_id: "timeline-dispatch-context-reference".to_string(),
            timeline_message: "检查引用文件".to_string(),
            images: Vec::new(),
            context_references: vec![SessionContextReference {
                kind: crate::context_reference::SessionContextReferenceKind::File,
                path: std::path::PathBuf::from("/tmp/external/reference.md"),
                name: "reference.md".to_string(),
            }],
            browser_annotation_refs: Vec::new(),
            created_session: false,
            mission_title: "检查引用文件".to_string(),
            task_title: "检查引用文件".to_string(),
            trimmed_text: Some("检查引用文件".to_string()),
            execution_goal: Some("读取并分析引用文件".to_string()),
            task_tier: TaskTier::ExecutionChain,
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
            entry_id: "timeline-goal-continuation-dispatch".to_string(),
            timeline_message: "目标自动推进: 完成验收".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            created_session: false,
            mission_title: "目标自动推进".to_string(),
            task_title: "执行: 目标自动推进".to_string(),
            trimmed_text: None,
            execution_goal: Some("先读取当前目标，再继续推进".to_string()),
            task_tier: TaskTier::ExecutionChain,
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
        assert!(turn.items.is_empty());
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
            Some(&task_store),
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
            entry_id: "timeline-goal-continuation-pause-race".to_string(),
            timeline_message: "目标自动推进: 验证暂停竞态".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            created_session: false,
            mission_title: "目标自动推进".to_string(),
            task_title: "执行: 目标自动推进".to_string(),
            trimmed_text: None,
            execution_goal: Some("先读取当前目标，再继续推进".to_string()),
            task_tier: TaskTier::ExecutionChain,
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
                Some(&task_store),
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
                Some(&task_store),
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
}
