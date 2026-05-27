//! Task System v2 — 派发提交载体。
//!
//! 这两个 DTO 与 ApiState / ApiError 无运行期耦合，是 v2 dispatch 流程的
//! "请求 → 接受" 一次性数据载体。magi-api 通过 `pub use` 重导出维持外部
//! import 路径不变。

use std::path::Path;
use std::sync::{Arc, Mutex};

use magi_agent_role::AgentRoleRegistry;
use magi_bridge_client::ModelBridgeClient;
use magi_core::{
    DomainError, ExecutionOwnership, MissionId, SessionId, TaskExecutionTarget, TaskId, TaskKind,
    TaskStatus, TaskTier, UtcMillis, WorkerId, WorkspaceId,
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

use crate::settings_store::SettingsStore;
use crate::task_execution_registry::{TaskExecutionPlan, TaskExecutionRegistry};

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
    pub created_session: bool,
    pub mission_title: String,
    pub task_title: String,
    pub trimmed_text: Option<String>,
    pub execution_goal: Option<String>,
    pub task_tier: TaskTier,
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

fn build_task_policy(task_tier: TaskTier) -> magi_core::TaskPolicy {
    magi_core::TaskPolicy {
        autonomy_level: "Autonomous".to_string(),
        approval_mode: "DecisionOnly".to_string(),
        allowed_tools: Vec::new(),
        denied_tools: Vec::new(),
        allowed_paths: Vec::new(),
        denied_paths: Vec::new(),
        network_mode: "full".to_string(),
        command_mode: "full".to_string(),
        retry_limit: 1,
        validation_profile: matches!(task_tier, TaskTier::LongMission)
            .then_some("Required".to_string()),
        checkpoint_mode: if matches!(task_tier, TaskTier::LongMission) {
            "task_or_phase".to_string()
        } else {
            "turn".to_string()
        },
        task_tier,
        background_allowed: matches!(task_tier, TaskTier::LongMission),
        escalation_conditions: if matches!(task_tier, TaskTier::LongMission) {
            vec!["human_checkpoint".to_string()]
        } else {
            Vec::new()
        },
    }
}

fn infer_dispatch_task_role(skill_name: Option<&str>, task_tier: TaskTier) -> &'static str {
    // agent_spawn / agent_wait 都是 coordinator 工具，主线入口任务（用户从聊天框发出的 turn）
    // 必须由 coordinator role 承接，否则 `task_can_see_builtin_tool` 会把 agent_spawn /
    // agent_wait / TodoWrite / MemoryWrite 全部判为不可见，模型在运行期看不到协调器工具——再没有任何
    // 后续路径能补救。LongMission / ExecutionChain 在这一点上同构：主线入口都是协调器。
    //
    // 代理（executor / reviewer / tester / explorer / architect）由 `execute_coordinator_tool`
    // 通过 agent_spawn 子派发显式创建，不走本函数；本函数只决定**主线入口**的默认 role。
    if matches!(task_tier, TaskTier::LongMission) {
        return "coordinator";
    }
    let Some(skill_name) = skill_name.map(str::trim).filter(|value| !value.is_empty()) else {
        return "coordinator";
    };
    let skill = skill_name.to_ascii_lowercase();
    if skill.contains("review") || skill.contains("audit") {
        "reviewer"
    } else if skill.contains("test") || skill.contains("qa") || skill.contains("verify") {
        "tester"
    } else if skill.contains("debug")
        || skill.contains("fix")
        || skill.contains("bug")
        || skill.contains("explore")
        || skill.contains("investigate")
        || skill.contains("doc")
    {
        "explorer"
    } else if skill.contains("arch") || skill.contains("design") {
        "architect"
    } else {
        // 所有具体落地（前/后端/数据/运维/安全/集成）统一收敛到 executor
        "executor"
    }
}

fn make_dispatch_task(
    task_id: TaskId,
    mission_id: MissionId,
    title: String,
    goal: String,
    now: UtcMillis,
    target_role: &str,
    task_tier: TaskTier,
) -> magi_core::Task {
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
        policy_snapshot: Some(build_task_policy(task_tier)),
        executor_binding: Some(serde_json::json!({
            "target_role": target_role,
            "capability_requirements": [],
            "parallelism_group": null,
            "exclusive_scope": null,
            "worker_selector": null,
        })),
        knowledge_refs: Vec::new(),
        workspace_scope: None,
        write_scope: None,
        input_refs: Vec::new(),
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
    let _ = runtime
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
    let target_role = request.target_role.as_deref().unwrap_or_else(|| {
        infer_dispatch_task_role(request.skill_name.as_deref(), request.task_tier)
    });
    if !runtime
        .agent_role_registry
        .role_supports_task_kind(target_role, TaskKind::LocalAgent)
    {
        return Err(DispatchSubmissionRunError::InvalidInput(format!(
            "role {target_role} 不支持 local_agent 任务"
        )));
    }
    // LongMission 与 coordinator role 必须共生：long-mission 工具可见性 gate
    // (`task_can_see_builtin_tool`) 同时要求 tier=LongMission + coordinator_mode。
    // 显式传入非 coordinator role 又指定 LongMission 是契约自相矛盾，必须从源头拒绝，
    // 而不是放行后让模型在运行期看不到工具再失败。
    if matches!(request.task_tier, TaskTier::LongMission)
        && !runtime
            .agent_role_registry
            .get(target_role)
            .is_some_and(|role| role.coordinator_mode)
    {
        return Err(DispatchSubmissionRunError::InvalidInput(format!(
            "LongMission 必须由 coordinator_mode role 承接，但显式指定了 role {target_role}"
        )));
    }
    let task = make_dispatch_task(
        act_task_id.clone(),
        mission_id.clone(),
        request.task_title.clone(),
        task_goal_text.clone(),
        now,
        target_role,
        request.task_tier,
    );
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
    let execution_chain_ref = Some(format!("session-action-chain-{}", accepted_at.0));
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
        execution_chain_ref: execution_chain_ref.clone(),
        ..ExecutionOwnership::default()
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
                execution_chain_ref: execution_chain_ref.clone(),
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
        user_message: trimmed_text.map(str::to_string),
        items: vec![ActiveExecutionTurnItem {
            item_id: user_message_item_id,
            item_seq: 1,
            kind: "user_message".to_string(),
            status: "completed".to_string(),
            source: "user".to_string(),
            title: None,
            content: trimmed_text.map(str::to_string),
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
            execution_chain_ref: execution_chain_ref
                .expect("dispatch execution chain ref should exist"),
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

    Ok(DispatchSubmissionAccepted {
        session_id: request.session_id,
        entry_id: request.entry_id,
        accepted_at: request.accepted_at,
        created_session: request.created_session,
        root_task_id: graph.root_task_id,
        action_task_id: graph.action_task_id,
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
            created_session: false,
            mission_title: "当前任务推进".to_string(),
            task_title: "当前任务推进".to_string(),
            trimmed_text: Some("创建 v2-task-system-e2e.md".to_string()),
            execution_goal: Some("创建 v2-task-system-e2e.md 并写入当前 marker".to_string()),
            task_tier: TaskTier::ExecutionChain,
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

    /// Task System v2 §3.2 验收：中等单 root task 走 ExecutionChain 路径，
    /// **不**进入 Long-Mission 层。
    ///
    /// 验收点：
    /// - route 已是 task（由 classifier 决定，本处不重测）；
    /// - dispatch 创建 action task 并落入 TaskStore；
    /// - `policy_snapshot.task_tier == ExecutionChain`——下游 Charter/Plan/KG/
    ///   Validation/Checkpoint/HumanCheckpoint 写端入口据 tier 判定是否启用；
    /// - 同步产生 ActiveExecutionChain，让运行期具备可观察的执行链。
    ///
    /// 注：Task #117 之后所有 tier 的 dispatch 都由 `runner_manager.start` 后台
    /// 驱动，tier 字段只决定 Charter/Plan/KG/Validation/Checkpoint/HumanCheckpoint
    /// 这 7 件套是否启用，不再决定调度模型本身。
    ///
    /// 反证："不启用 Long-Mission 层"的关键 invariant 是 task tier——一旦 tier 是
    /// ExecutionChain，§3.4 的 Charter/Plan/KG/Validation/Checkpoint/HumanCheckpoint
    /// 写端入口（`runner_manager` 内）就不会被触达。tier 字段是单源真相。
    #[test]
    fn execution_chain_dispatch_creates_action_task_with_chain_tier_and_skips_long_mission() {
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
            created_session: false,
            mission_title: "修复 bug + 验证".to_string(),
            task_title: "修复 bug + 验证".to_string(),
            trimmed_text: Some("修复明确 bug 并跑相关验证".to_string()),
            execution_goal: Some("定位并修复 bug、再跑相关验证命令".to_string()),
            task_tier: TaskTier::ExecutionChain,
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
            "§3.2: action task tier 必须是 ExecutionChain（非 LongMission），\
             否则 runner 内部会错误激活 LongMission 7 件套写入入口",
        );
        assert_ne!(
            policy.task_tier,
            TaskTier::LongMission,
            "§3.2 反证：ExecutionChain 路径的 task 绝不应被记为 LongMission tier",
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

    /// Task System v2 §3.4 验收：复杂 Mission 走 LongMission 路径——dispatch 把
    /// action task 的 `policy_snapshot` 写成 LongMission 形态，runner 据此在
    /// 内部启用 Charter / Plan / Workspace / KG / Validation / Checkpoint /
    /// HumanCheckpoint 7 件套的写入与编排。
    ///
    /// 这里覆盖的是 §3.4 的**路由前置条件**，是端到端链路的单点真相：
    /// - `policy_snapshot.task_tier == LongMission`——下游 runner / dispatcher
    ///   根据该字段决定是否激活 LongMission 7 件套写端入口；
    /// - `validation_profile == Some("Required")`——P3 Validation gate 的入口；
    /// - `checkpoint_mode == "task_or_phase"`——§1.4 复杂任务 checkpoint 节奏；
    /// - `background_allowed == true`——LongMission 才允许的长跑后台执行；
    /// - `escalation_conditions` 含 `"human_checkpoint"`——§1.5 HumanCheckpoint 阻塞钩。
    ///
    /// 注：Task #117 后所有 tier 的 dispatch 都由后台 `runner_manager.start` 驱动，
    /// tier 字段只决定 7 件套写端是否启用，不再决定调度模型本身。
    ///
    /// §3.4 其余 invariant 由各自专属测试覆盖、本测不重复：
    /// - Charter 写入：`magi_mission_charter::*` + `tool_batch::*` 中
    ///   `MissionCharterWrite` 工具拦截路径；
    /// - Plan + Validation gate：`magi_plan::apply_plan_update` 单测；
    /// - Checkpoint 恢复集与读端聚合：`magi-checkpoint` + `magi-mission` 单测 +
    ///   `magi-mission::contract_round_trip` 集成测试；
    /// - 进程重启恢复：`magi_daemon::daemon::mission_recovery::*` 单测；
    /// - pending HumanCheckpoint 阻断 spawn：`agent_spawn_rejects_when_human_
    ///   checkpoint_is_pending`（`tool_batch.rs` 单测）。
    #[test]
    fn long_mission_dispatch_writes_policy_snapshot_that_routes_to_runner_manager() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-long-mission-tier");

        session_store
            .create_session(session_id.clone(), "long mission tier")
            .expect("session should be creatable");

        let request = DispatchSubmissionRequest {
            accepted_at: UtcMillis(4_000),
            session_id: session_id.clone(),
            workspace_id: Some(WorkspaceId::new("workspace-long-mission")),
            entry_id: "timeline-long-mission".to_string(),
            timeline_message: "跨多阶段重构：拆模块、迁数据、灰度切换".to_string(),
            created_session: false,
            mission_title: "复杂重构 mission".to_string(),
            task_title: "复杂重构 mission".to_string(),
            trimmed_text: Some("跨多阶段重构：拆模块、迁数据、灰度切换".to_string()),
            execution_goal: Some("完成跨多阶段重构并保留每阶段可恢复的 checkpoint".to_string()),
            task_tier: TaskTier::LongMission,
            skill_name: None,
            target_role: Some("coordinator".to_string()),
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
            .expect("long mission dispatch should build graph");

        let action_task = task_store
            .get_task(&graph.action_task_id)
            .expect("action task should be persisted in TaskStore");
        let policy = action_task
            .policy_snapshot
            .as_ref()
            .expect("dispatch 必须给 long mission action task 写入 policy_snapshot");

        assert_eq!(
            policy.task_tier,
            TaskTier::LongMission,
            "§3.4 路由前置：action task tier 必须是 LongMission，\
             否则 runner 内部不会激活 LongMission 7 件套写入入口",
        );
        assert_eq!(
            policy.validation_profile.as_deref(),
            Some("Required"),
            "§3.4 + §1.3：LongMission 必须开启 Required 校验档位，是 Plan completion gate 的入口",
        );
        assert_eq!(
            policy.checkpoint_mode, "task_or_phase",
            "§3.4 + §1.4：LongMission 必须使用 task_or_phase checkpoint 节奏，\
             与单 turn 任务的 turn-level checkpoint 区分",
        );
        assert!(
            policy.background_allowed,
            "§3.4：LongMission 必须允许后台执行（长跑 mission 不能阻塞 chat session）",
        );
        assert!(
            policy
                .escalation_conditions
                .iter()
                .any(|c| c == "human_checkpoint"),
            "§3.4 + §1.5：LongMission 必须把 human_checkpoint 列为 escalation 条件，\
             否则 runner 不知道何时阻塞等待人审：实际 {:?}",
            policy.escalation_conditions,
        );

        // 对称反证：LongMission 路径**也**必须产出 ActiveExecutionChain，让恢复 / 看板能感知；
        // 运行期由 runner_manager 接管驱动，但 chain 状态仍是单源真相。
        let chain = graph
            .active_execution_chain
            .as_ref()
            .expect("LongMission 路径仍须产出 ActiveExecutionChain（供恢复链路消费）");
        assert!(
            chain.current_turn.is_some(),
            "ActiveExecutionChain 必须带 current_turn，作为 mission 首轮的 lane 入口",
        );

        // §3.4 子契约：dispatch 必须把 LongMission 行动任务交给 coordinator role 承接，
        // 否则 long-mission 工具可见性 gate 会屏蔽 Charter/Plan/Checkpoint 工具。
        let executor_role = action_task
            .executor_binding_target_role()
            .expect("LongMission action task 必须写入 executor_binding.target_role");
        let executor_role_entry = agent_role_registry
            .get(executor_role)
            .expect("LongMission action task 的 target_role 必须能在 registry 中查到");
        assert!(
            executor_role_entry.coordinator_mode,
            "§3.4：LongMission action task 必须由 coordinator_mode role 承接\
             （long-mission 工具可见性 gate 同时要求 tier=LongMission + coordinator_mode），\
             实际 role={executor_role}",
        );
    }

    /// §3.4 反证：显式把 LongMission action task 指定给非 coordinator role 是契约
    /// 自相矛盾——`task_can_see_builtin_tool` 必然否决 Charter/Plan/Checkpoint，
    /// runner 必失败。从源头拒绝，不放行错误配置。
    #[test]
    fn long_mission_dispatch_rejects_non_coordinator_target_role() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = AgentRoleRegistry::load_default();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_id = SessionId::new("session-long-mission-bad-role");

        session_store
            .create_session(session_id.clone(), "long mission bad role")
            .expect("session should be creatable");

        let request = DispatchSubmissionRequest {
            accepted_at: UtcMillis(5_000),
            session_id,
            workspace_id: Some(WorkspaceId::new("workspace-long-mission-bad-role")),
            entry_id: "timeline-long-mission-bad-role".to_string(),
            timeline_message: "复杂任务模式：跨多阶段重构".to_string(),
            created_session: false,
            mission_title: "重构 mission".to_string(),
            task_title: "重构 mission".to_string(),
            trimmed_text: Some("复杂任务模式：跨多阶段重构".to_string()),
            execution_goal: Some("跨多阶段重构".to_string()),
            task_tier: TaskTier::LongMission,
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

        let err = run_dispatch_submission(&runtime, &request);
        let message = match err {
            Err(DispatchSubmissionRunError::InvalidInput(msg)) => msg,
            Ok(_) => {
                panic!("非 coordinator role 配 LongMission tier 必须被 dispatch 拒绝，但放行了")
            }
            Err(other) => panic!("期待 InvalidInput，实际 {other:?}"),
        };
        assert!(
            message.contains("LongMission") && message.contains("coordinator_mode"),
            "错误消息必须明确指向 LongMission + coordinator_mode 契约：{message}",
        );
    }
}
