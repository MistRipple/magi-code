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
    AccessProfile, DomainError, ExecutionOwnership, MissionId, SessionId, TaskExecutionTarget,
    TaskExecutorBinding, TaskId, TaskKind, TaskStatus, TaskTier, UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::{EventContext, InMemoryEventBus, task_events};
use magi_orchestrator::{
    DispatchMemoryExtractionInput, ExecutionWritebackPlans, task_store::TaskStore,
};
use magi_session_store::{
    ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
    ActiveExecutionTurn, ActiveExecutionTurnItem, SessionStore, TimelineEntryKind,
};
use magi_spawn_graph::SpawnGraph;

use crate::session_thread;

use crate::context_reference::{
    SessionContextReference, session_context_reference_input_refs,
    session_context_reference_policy, session_context_references_metadata,
};
use crate::session_images::SessionTurnImage;
use crate::task_execution_registry::{TaskExecutionPlan, TaskExecutionRegistry};
use magi_settings_store::SettingsStore;

pub struct DispatchSubmissionGraph {
    pub root_task_id: TaskId,
    pub action_task_id: TaskId,
    pub active_execution_chain: Option<ActiveExecutionChain>,
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
    pub created_session: bool,
    pub mission_title: String,
    pub task_title: String,
    pub trimmed_text: Option<String>,
    pub execution_goal: Option<String>,
    pub task_tier: TaskTier,
    pub access_profile: AccessProfile,
    pub skill_name: Option<String>,
    pub target_role: Option<String>,
    pub request_id: Option<String>,
    pub user_message_id: Option<String>,
    pub placeholder_message_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DispatchSubmissionAccepted {
    pub session_id: SessionId,
    pub entry_id: String,
    pub accepted_at: UtcMillis,
    pub created_session: bool,
    pub root_task_id: TaskId,
    pub action_task_id: TaskId,
    pub user_message_item_id: String,
    pub runner_started: bool,
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
            DomainError::InvalidState { message } if message.contains("active current_turn") => {
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
    workspace_root_path: Option<&Path>,
) -> magi_core::TaskPolicy {
    let reference_policy = session_context_reference_policy(
        context_references,
        workspace_root_path
            .map(|path| path.to_string_lossy())
            .as_deref(),
        access_profile,
    );
    magi_core::TaskPolicy {
        autonomy_level: "Autonomous".to_string(),
        access_profile,
        allowed_tools: Vec::new(),
        denied_tools: Vec::new(),
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
    } = input;
    let executor_binding = TaskExecutorBinding::for_role(target_role)
        .with_active_skill_id(active_skill_id.map(str::to_string));

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
            workspace_root_path,
        )),
        executor_binding: Some(executor_binding),
        knowledge_refs: Vec::new(),
        workspace_scope: None,
        write_scope: None,
        input_refs: session_context_reference_input_refs(context_references),
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        runtime_payload: magi_core::TaskRuntimePayload::None,
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
        workspace_root_path: runtime.workspace_root_path,
    });
    runtime.task_store.insert_task(task);
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
    let user_message_item_id = user_message_id
        .clone()
        .unwrap_or_else(|| format!("turn-item-user-{}", accepted_at.0));
    let mut current_turn = ActiveExecutionTurn {
        turn_id: format!("turn-session-action-{}", accepted_at.0),
        turn_seq: accepted_at.0,
        accepted_at,
        status: "accepted".to_string(),
        completed_at: None,
        user_message: Some(request.timeline_message.clone()),
        items: vec![ActiveExecutionTurnItem {
            item_id: user_message_item_id,
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
                metadata
            },
            timeline_entry_id: Some(entry_id.to_string()),
            source_thread_id: orchestrator_thread_id.clone(),
        }],
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

pub fn accept_dispatch_submission(
    session_store: &SessionStore,
    task_store: Option<&TaskStore>,
    execution_registry: &TaskExecutionRegistry,
    request: DispatchSubmissionRequest,
    graph: DispatchSubmissionGraph,
) -> Result<DispatchSubmissionAccepted, DispatchSubmissionAcceptError> {
    if let Some(active_execution_chain) = graph.active_execution_chain.clone() {
        let accept_result = session_store.accept_active_execution_chain_with_timeline_entry(
            request.session_id.clone(),
            request.entry_id.clone(),
            TimelineEntryKind::UserMessage,
            request.timeline_message.clone(),
            request.accepted_at,
            active_execution_chain,
        );
        if let Err(error) = accept_result {
            cleanup_rejected_dispatch(task_store, execution_registry, &graph);
            return Err(DispatchSubmissionAcceptError::from_store_error(error));
        }
    }

    let user_message_item_id = request
        .user_message_id
        .clone()
        .unwrap_or_else(|| format!("turn-item-user-{}", request.accepted_at.0));

    Ok(DispatchSubmissionAccepted {
        session_id: request.session_id,
        entry_id: request.entry_id,
        accepted_at: request.accepted_at,
        created_session: request.created_session,
        root_task_id: graph.root_task_id,
        action_task_id: graph.action_task_id,
        user_message_item_id,
        runner_started: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_session_store::{ExecutionThread, ExecutionThreadStatus, ThreadChatMessage};

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
            created_session: false,
            mission_title: "当前任务推进".to_string(),
            task_title: "当前任务推进".to_string(),
            trimmed_text: Some("创建 task-system-e2e.md".to_string()),
            execution_goal: Some("创建 task-system-e2e.md 并写入当前 marker".to_string()),
            task_tier: TaskTier::ExecutionChain,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            target_role: Some("executor".to_string()),
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
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
            created_session: false,
            mission_title: "修复 bug + 验证".to_string(),
            task_title: "修复 bug + 验证".to_string(),
            trimmed_text: Some("修复明确 bug 并跑相关验证".to_string()),
            execution_goal: Some("定位并修复 bug、再跑相关验证命令".to_string()),
            task_tier: TaskTier::ExecutionChain,
            access_profile: AccessProfile::FullAccess,
            skill_name: None,
            target_role: Some("executor".to_string()),
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
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
            created_session: false,
            mission_title: "Skill 子代理继承".to_string(),
            task_title: "Skill 子代理继承".to_string(),
            trimmed_text: Some("使用 browser Skill 创建 explorer 子代理".to_string()),
            execution_goal: Some("创建 explorer 子代理并等待结果".to_string()),
            task_tier: TaskTier::ExecutionChain,
            access_profile: AccessProfile::FullAccess,
            skill_name: Some("stellarlinkco/myclaude/skills/browser".to_string()),
            target_role: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
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
            created_session: false,
            mission_title: "代码审查".to_string(),
            task_title: "代码审查".to_string(),
            trimmed_text: Some("使用代码审查 skill 检查当前改动".to_string()),
            execution_goal: Some("检查当前改动并给出问题列表".to_string()),
            task_tier: TaskTier::ExecutionChain,
            access_profile: AccessProfile::Restricted,
            skill_name: Some("code-review".to_string()),
            target_role: Some("reviewer".to_string()),
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
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
            created_session: false,
            mission_title: "检查引用文件".to_string(),
            task_title: "检查引用文件".to_string(),
            trimmed_text: Some("检查引用文件".to_string()),
            execution_goal: Some("读取并分析引用文件".to_string()),
            task_tier: TaskTier::ExecutionChain,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            target_role: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
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
}
