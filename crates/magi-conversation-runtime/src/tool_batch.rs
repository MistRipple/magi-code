//! 任务系统 — tool batch / coordinator / single 工具执行入口。
//!
//! - `execute_task_tool_call_batch`：按 concurrency 分组并发或串行调度本轮工具。
//! - `execute_task_tool_call`：单工具入口，按 BuiltinToolName 走 coordinator/写工具/policy/
//!   safety gate/tool registry 各分支。
//! - `execute_coordinator_tool`：协调器工具（agent_spawn / agent_wait）入口。
//! - `task_policy_tool_rejection` / `safety_gate_rejection` 等支撑判定。

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use magi_bridge_client::{
    ChatToolCall,
    tool_concurrency::{ToolBatchKind, ToolConcurrencyInput, partition_tool_calls_with_inputs},
};
use magi_core::{
    ApprovalRequirement, EventId, ExecutionResultStatus, RiskLevel, SessionId, TaskId, TaskKind,
    TaskPolicy, TaskStatus, TaskTier, ToolCallId, UtcMillis, WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::ToolKind;
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::SessionStore;
use magi_snapshot::{SnapshotSession, ToolHook, ToolHookCtx};
use magi_tool_runtime::{
    BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
};

use crate::builtin_tool_schema::internal_builtin_tool_rejection_payload;
use crate::skill_apply_tool::{SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime};
use crate::{
    ConversationRegistry, MailboxAuthor, MailboxKind, RuntimeSignal,
    task_execution_registry::{SpawnedChildExecutionRequest, TaskExecutionRegistry},
    task_helpers::{task_can_see_builtin_tool, task_is_long_mission},
    tool_declared_paths::{append_result_declared_paths, derive_declared_paths},
};

/// agent_spawn 生成 child task_id 时使用的进程内单调序号。
///
/// 仅靠 `UtcMillis::now()` 在同一毫秒内的多次并行 agent_spawn 会产生重复
/// child_id，进而触发 SpawnGraph 的边冲突。配合毫秒时间戳一起拼到 task_id
/// 末尾，保证同一进程内绝对唯一。
static AGENT_SPAWN_SEQ: AtomicU64 = AtomicU64::new(0);
const AGENT_SPAWN_SUMMARY_MAX_CHARS: usize = 1200;
const AGENT_SPAWN_FINAL_TEXT_MAX_CHARS: usize = 6000;
const AGENT_WAIT_DEFAULT_TIMEOUT_MS: u64 = 300_000;
const AGENT_WAIT_MIN_TIMEOUT_MS: u64 = 1_000;
const AGENT_WAIT_MAX_TIMEOUT_MS: u64 = 1_800_000;
const AGENT_ROLE_IDS: &[&str] = &["architect", "executor", "explorer", "reviewer", "tester"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentSpawnAccessMode {
    ReadOnly,
    ReadWrite,
}

impl AgentSpawnAccessMode {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "read_only" | "readonly" | "read-only" | "read" => Some(Self::ReadOnly),
            "read_write" | "readwrite" | "read-write" | "write" | "full" => Some(Self::ReadWrite),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::ReadWrite => "read_write",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AgentSpawnParameterContract {
    role: Option<String>,
    display_name: String,
    access_mode: Option<AgentSpawnAccessMode>,
    goal: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChildAgentOutput {
    summary: String,
    final_text: String,
    truncated: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn execute_task_tool_call_batch(
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    agent_role_registry: &magi_agent_role::AgentRoleRegistry,
    skill_runtime: Option<&magi_skill_runtime::SkillRuntime>,
    task_store: &TaskStore,
    session_store: &SessionStore,
    execution_registry: &TaskExecutionRegistry,
    conversation_registry: &ConversationRegistry,
    spawn_graph: &Mutex<magi_spawn_graph::SpawnGraph>,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    todo_ledger: &magi_todo_ledger::TodoLedger,
    project_memory: Option<&magi_project_memory::ProjectMemoryStore>,
    mission_charter: Option<&magi_mission_charter::MissionCharterStore>,
    plan: Option<&magi_plan::PlanStore>,
    knowledge_graph: Option<&magi_knowledge_graph::KnowledgeGraphStore>,
    validation_runner: Option<&magi_validation_runner::ValidationStore>,
    checkpoint: Option<&magi_checkpoint::CheckpointStore>,
    human_checkpoint: Option<&magi_human_checkpoint::HumanCheckpointStore>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    workspace_root_path: Option<&PathBuf>,
    worker_id: Option<&magi_core::WorkerId>,
    tool_calls: &[ChatToolCall],
    snapshot_session: Option<Arc<SnapshotSession>>,
    execution_group_id: Option<String>,
) -> Vec<(String, ExecutionResultStatus)> {
    let parsed_arguments = tool_calls
        .iter()
        .map(|tool_call| {
            serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments).ok()
        })
        .collect::<Vec<_>>();
    let tool_inputs = tool_calls
        .iter()
        .zip(parsed_arguments.iter())
        .map(|(tool_call, arguments)| ToolConcurrencyInput {
            tool_name: tool_call.function.name.as_str(),
            arguments: arguments.as_ref(),
        })
        .collect::<Vec<_>>();
    let mut results = vec![None; tool_calls.len()];
    let hook_contexts = tool_calls
        .iter()
        .map(|tool_call| ToolHookCtx {
            tool_call_id: tool_call.id.clone(),
            worker_id: worker_id.map(ToString::to_string),
            execution_group_id: execution_group_id.clone(),
            declared_paths: derive_declared_paths(tool_call),
        })
        .collect::<Vec<_>>();

    for batch in partition_tool_calls_with_inputs(&tool_inputs) {
        match batch.kind {
            ToolBatchKind::Serial => {
                for tool_index in batch.tool_indices {
                    let mut hook_ctx = hook_contexts[tool_index].clone();
                    if let Some(snapshot) = snapshot_session.as_deref() {
                        snapshot.before_tool(&hook_ctx);
                    }
                    let result = execute_task_tool_call(
                        event_bus,
                        tool_registry,
                        agent_role_registry,
                        skill_runtime,
                        task_store,
                        session_store,
                        execution_registry,
                        conversation_registry,
                        spawn_graph,
                        safety_gate,
                        todo_ledger,
                        project_memory,
                        mission_charter,
                        plan,
                        knowledge_graph,
                        validation_runner,
                        checkpoint,
                        human_checkpoint,
                        task,
                        session_id,
                        workspace_id,
                        workspace_root_path,
                        worker_id,
                        &tool_calls[tool_index],
                    );
                    append_result_declared_paths(&mut hook_ctx.declared_paths, &result.0);
                    if let Some(snapshot) = snapshot_session.as_deref() {
                        snapshot.after_tool(&hook_ctx);
                    }
                    results[tool_index] = Some(result);
                }
            }
            ToolBatchKind::Concurrent => {
                thread::scope(|scope| {
                    let handles = batch
                        .tool_indices
                        .iter()
                        .copied()
                        .map(|tool_index| {
                            let tool_call = &tool_calls[tool_index];
                            let mut hook_ctx = hook_contexts[tool_index].clone();
                            let snapshot_session = snapshot_session.clone();
                            (
                                tool_index,
                                scope.spawn(move || {
                                    let result = execute_task_tool_call(
                                        event_bus,
                                        tool_registry,
                                        agent_role_registry,
                                        skill_runtime,
                                        task_store,
                                        session_store,
                                        execution_registry,
                                        conversation_registry,
                                        spawn_graph,
                                        safety_gate,
                                        todo_ledger,
                                        project_memory,
                                        mission_charter,
                                        plan,
                                        knowledge_graph,
                                        validation_runner,
                                        checkpoint,
                                        human_checkpoint,
                                        task,
                                        session_id,
                                        workspace_id,
                                        workspace_root_path,
                                        worker_id,
                                        tool_call,
                                    );
                                    append_result_declared_paths(
                                        &mut hook_ctx.declared_paths,
                                        &result.0,
                                    );
                                    if let Some(snapshot) = snapshot_session.as_deref() {
                                        snapshot.after_tool(&hook_ctx);
                                    }
                                    result
                                }),
                            )
                        })
                        .collect::<Vec<_>>();

                    for (tool_index, handle) in handles {
                        let result = handle.join().unwrap_or_else(|_| {
                            (
                                serde_json::json!({
                                    "tool": tool_calls[tool_index].function.name,
                                    "status": "failed",
                                    "error": "任务工具执行线程异常"
                                })
                                .to_string(),
                                ExecutionResultStatus::Failed,
                            )
                        });
                        results[tool_index] = Some(result);
                    }
                });
            }
        }
    }
    if let Some(snapshot) = snapshot_session.as_deref()
        && let Err(err) = snapshot.reconcile()
    {
        tracing::warn!(
            session_id = %session_id.as_str(),
            task_id = %task.task_id.as_str(),
            error = %err,
            "snapshot reconcile after task tool batch failed"
        );
    }

    results
        .into_iter()
        .enumerate()
        .map(|(tool_index, result)| {
            result.unwrap_or_else(|| {
                (
                    serde_json::json!({
                        "tool": tool_calls[tool_index].function.name,
                        "status": "failed",
                        "error": "任务工具未产生执行结果"
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                )
            })
        })
        .collect()
}

/// S7-E：协调器工具（agent_spawn）的统一拦截入口。返回 (payload_json, status)，与
/// `execute_task_tool_call` 的常规工具路径形状一致，便于上层把回执拼回 LLM 消息流。
fn execute_coordinator_tool(
    event_bus: &InMemoryEventBus,
    agent_role_registry: &magi_agent_role::AgentRoleRegistry,
    task_store: &TaskStore,
    session_store: &SessionStore,
    execution_registry: &TaskExecutionRegistry,
    conversation_registry: &ConversationRegistry,
    spawn_graph: &Mutex<magi_spawn_graph::SpawnGraph>,
    mission_charter: Option<&magi_mission_charter::MissionCharterStore>,
    plan: Option<&magi_plan::PlanStore>,
    human_checkpoint: Option<&magi_human_checkpoint::HumanCheckpointStore>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    tool: magi_tool_runtime::BuiltinToolName,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    let parsed: serde_json::Value = match serde_json::from_str(&tool_call.function.arguments) {
        Ok(value) => value,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "failed",
                    "error": format!("协调器工具参数解析失败: {err}"),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };

    let publish_event = |kind: &str, payload: serde_json::Value| {
        let _ = event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!("event-coordinator-{kind}-{}", UtcMillis::now().0)),
                kind,
                payload,
            )
            .with_context(EventContext {
                workspace_id: workspace_id.clone(),
                session_id: Some(session_id.clone()),
                mission_id: Some(task.mission_id.clone()),
                task_id: Some(task.task_id.clone()),
                ..EventContext::default()
            }),
        );
    };

    match tool {
        magi_tool_runtime::BuiltinToolName::AgentSpawn => {
            if human_checkpoint.is_none() && task_is_long_mission(Some(task)) {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": "long mission 缺少 HumanCheckpointStore，无法确认 pending 人审状态，禁止 agent_spawn",
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            if let Some(rejection) =
                long_mission_agent_spawn_prerequisite_rejection(tool, mission_charter, plan, task)
            {
                return rejection;
            }
            if let Some(store) = human_checkpoint {
                match store.has_pending(&task.mission_id) {
                    Ok(true) => {
                        return (
                            serde_json::json!({
                                "tool": tool.as_str(),
                                "status": "rejected",
                                "error": "mission 存在 pending HumanCheckpoint，operator resolve 前禁止 agent_spawn 派发新工作",
                            })
                            .to_string(),
                            ExecutionResultStatus::Rejected,
                        );
                    }
                    Ok(false) => {}
                    Err(error) => {
                        return (
                            serde_json::json!({
                                "tool": tool.as_str(),
                                "status": "failed",
                                "error": format!("HumanCheckpoint pending 状态读取失败: {error}"),
                            })
                            .to_string(),
                            ExecutionResultStatus::Failed,
                        );
                    }
                }
            }
            let requested_role = parsed
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let requested_goal = parsed
                .get("goal")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if requested_role.is_empty() || requested_goal.is_empty() {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": "agent_spawn 缺少必需字段 role 或 goal",
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            // display_name 是 LLM 提供的代理展示名，作为 Task.title 直接面向用户。
            // 长度限制 3-30 个 Unicode 字符：下限 3 既能拒绝『x』『ab』之类的占位符，
            // 又允许典型 4 字中文短语（如『探索目录』『统计行数』）这一最自然的命名密度；
            // 上限 30 防止破坏前端代理卡片版式。
            let requested_display_name = parsed
                .get("display_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let parameter_contract = current_turn_user_message(session_store, session_id)
                .as_deref()
                .and_then(|message| {
                    select_agent_spawn_parameter_contract(
                        message,
                        &requested_role,
                        &requested_display_name,
                        &requested_goal,
                    )
                });
            let role = parameter_contract
                .as_ref()
                .and_then(|contract| contract.role.clone())
                .unwrap_or(requested_role);
            let goal = parameter_contract
                .as_ref()
                .and_then(|contract| contract.goal.clone())
                .unwrap_or(requested_goal);
            let display_name = parameter_contract
                .as_ref()
                .map(|contract| contract.display_name.clone())
                .unwrap_or(requested_display_name);
            let display_name_chars = display_name.chars().count();
            if display_name.is_empty() {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": "agent_spawn 缺少必需字段 display_name",
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            if !agent_role_registry.is_spawnable_agent_role(&role) {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "degraded",
                        "fallback_mode": "mainline_or_reassign",
                        "role": role,
                        "available_roles": agent_role_registry.spawnable_agent_role_ids(),
                        "error": "该 role 不是可派发代理角色。coordinator 是主线编排身份，不能通过 agent_spawn 派发。",
                        "instruction": "请改派 architect / executor / explorer / reviewer / tester 等专业代理；如果无需继续派发，则由主线基于已有上下文直接推进并给出结果。",
                    })
                    .to_string(),
                    ExecutionResultStatus::Succeeded,
                );
            }
            if !(3..=30).contains(&display_name_chars) {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": format!(
                            "agent_spawn display_name 长度必须在 3-30 个字符之间，实际 {display_name_chars}",
                        ),
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            let context = parsed
                .get("context")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let child_goal = match context {
                Some(context) if context != goal => format!("{goal}\n\n上下文：{context}"),
                _ => goal.clone(),
            };
            let access_mode = match parsed.get("access_mode").and_then(|v| v.as_str()) {
                Some(value) => match AgentSpawnAccessMode::parse(value) {
                    Some(mode) => mode,
                    None => {
                        return (
                            serde_json::json!({
                                "tool": tool.as_str(),
                                "status": "failed",
                                "error": "agent_spawn access_mode 只能是 read_only 或 read_write",
                            })
                            .to_string(),
                            ExecutionResultStatus::Failed,
                        );
                    }
                },
                None => default_agent_spawn_access_mode(&role),
            };
            let access_mode = parameter_contract
                .as_ref()
                .and_then(|contract| contract.access_mode)
                .unwrap_or(access_mode);
            let task_kind = parsed
                .get("task_kind")
                .and_then(|v| v.as_str())
                .and_then(|s| match s.to_ascii_lowercase().as_str() {
                    "action" => Some(TaskKind::LocalAgent),
                    "validation" => Some(TaskKind::LocalAgent),
                    "repair" => Some(TaskKind::LocalAgent),
                    "decision" => Some(TaskKind::LocalAgent),
                    "work_package" | "workpackage" => Some(TaskKind::LocalAgent),
                    "phase" => Some(TaskKind::LocalAgent),
                    "objective" => Some(TaskKind::LocalAgent),
                    _ => None,
                })
                .unwrap_or(TaskKind::LocalAgent);
            let now = UtcMillis::now();
            // 单调序号 + 毫秒时间戳一起拼接，避免同一毫秒内多次并行 agent_spawn
            // 生成同名 child_id（会击穿 SpawnGraph 边唯一性约束）。
            let seq = AGENT_SPAWN_SEQ.fetch_add(1, Ordering::Relaxed);
            let child_id = TaskId::new(format!(
                "task-spawn-{}-{}-{}",
                task.task_id.as_str(),
                now.0,
                seq
            ));
            let child_policy_snapshot =
                agent_spawn_child_policy_snapshot(task.policy_snapshot.as_ref(), access_mode);
            let child = magi_core::Task {
                task_id: child_id.clone(),
                mission_id: task.mission_id.clone(),
                root_task_id: task.root_task_id.clone(),
                parent_task_id: Some(task.task_id.clone()),
                kind: task_kind,
                title: display_name,
                goal: child_goal,
                status: TaskStatus::Pending,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: Some(child_policy_snapshot),
                executor_binding: Some(serde_json::json!({
                    "target_role": role,
                    "access_mode": access_mode.as_str(),
                    "capability_requirements": [],
                    "parallelism_group": parsed
                        .get("parallelism_group")
                        .and_then(|v| v.as_str()),
                    "exclusive_scope": null,
                    "worker_selector": null,
                })),
                knowledge_refs: Vec::new(),
                workspace_scope: task.workspace_scope.clone(),
                write_scope: task.write_scope.clone(),
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                runtime_payload: magi_core::TaskRuntimePayload::default(),
                created_at: now,
                updated_at: now,
            };
            let registered_execution = match execution_registry.register_spawned_local_agent_child(
                SpawnedChildExecutionRequest {
                    task_store,
                    spawn_graph,
                    session_store,
                    child_task: &child,
                    session_id,
                    workspace_id,
                    role: &role,
                    now,
                },
            ) {
                Ok(registered) => registered,
                Err(error) => {
                    return (
                        serde_json::json!({
                            "tool": tool.as_str(),
                            "status": "failed",
                            "error": format!("agent_spawn 注册子执行失败: {error}"),
                        })
                        .to_string(),
                        ExecutionResultStatus::Failed,
                    );
                }
            };
            publish_event(
                "task.coordinator.agent_spawn",
                serde_json::json!({
                    "parent_task_id": task.task_id.to_string(),
                    "child_task_id": child_id.to_string(),
                    "role": role,
                    "access_mode": access_mode.as_str(),
                    "goal": goal,
                    "task_kind": format!("{:?}", task_kind),
                    "worker_id": registered_execution.worker_id.to_string(),
                    "thread_id": registered_execution.thread_id.to_string(),
                    "execution_chain_ref": registered_execution.execution_chain_ref,
                }),
            );
            enqueue_agent_assignment_message(
                conversation_registry,
                session_id,
                task,
                &child,
                &role,
                now,
            );

            (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "started",
                    "child_task_id": child_id.to_string(),
                    "role": role,
                    "access_mode": access_mode.as_str(),
                    "title": child.title,
                    "assignment": {
                        "title": child.title,
                        "goal": child.goal,
                        "role": role,
                        "access_mode": access_mode.as_str(),
                    },
                    "worker_id": registered_execution.worker_id.to_string(),
                    "thread_id": registered_execution.thread_id.to_string(),
                    "execution_chain_ref": registered_execution.execution_chain_ref,
                    "instruction": "代理已异步启动。若后续结论依赖该代理结果，必须调用 agent_wait，并传入 child_task_id 收集终态结果；不要在未等待必要代理结果时直接给最终答复。",
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            )
        }
        magi_tool_runtime::BuiltinToolName::AgentWait => {
            execute_agent_wait(task_store, tool, &parsed)
        }
        _ => unreachable!("execute_coordinator_tool 只接收协调器代理工具变体"),
    }
}

fn long_mission_agent_spawn_prerequisite_rejection(
    tool: magi_tool_runtime::BuiltinToolName,
    mission_charter: Option<&magi_mission_charter::MissionCharterStore>,
    plan: Option<&magi_plan::PlanStore>,
    task: &magi_core::Task,
) -> Option<(String, ExecutionResultStatus)> {
    if !task_is_long_mission(Some(task)) {
        return None;
    }
    let Some(mission_charter) = mission_charter else {
        return Some((
            serde_json::json!({
                "tool": tool.as_str(),
                "status": "failed",
                "error": "long mission 缺少 MissionCharterStore，无法确认 mission 契约，禁止 agent_spawn",
                "instruction": "先修复 workspace 绑定，让 mission_charter_write 能落盘；长任务不能在没有 mission 契约的情况下派发代理。",
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        ));
    };
    match mission_charter.load(&task.mission_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Some((
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "rejected",
                    "error": "LongMission 尚未创建 mission charter，禁止 agent_spawn",
                    "instruction": "先调用 mission_charter_write 写入 title、goal、success_criteria 和 constraints，再调用 plan_write 建立执行计划，然后再派发代理。",
                })
                .to_string(),
                ExecutionResultStatus::Rejected,
            ));
        }
        Err(error) => {
            return Some((
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "failed",
                    "error": format!("读取 mission charter 失败：{error}"),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            ));
        }
    }

    let Some(plan) = plan else {
        return Some((
            serde_json::json!({
                "tool": tool.as_str(),
                "status": "failed",
                "error": "long mission 缺少 PlanStore，无法确认 mission plan，禁止 agent_spawn",
                "instruction": "先修复 workspace 绑定，让 plan_write 能落盘；长任务不能在没有执行计划的情况下派发代理。",
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        ));
    };
    match plan.load(&task.mission_id) {
        Ok(Some(plan)) if !plan.steps.is_empty() => None,
        Ok(_) => Some((
            serde_json::json!({
                "tool": tool.as_str(),
                "status": "rejected",
                "error": "LongMission 尚未创建非空 mission plan，禁止 agent_spawn",
                "instruction": "先调用 plan_write 写入 pending / in_progress 步骤，再派发代理；不要用 todo_write 替代 mission plan。",
            })
            .to_string(),
            ExecutionResultStatus::Rejected,
        )),
        Err(error) => Some((
            serde_json::json!({
                "tool": tool.as_str(),
                "status": "failed",
                "error": format!("读取 mission plan 失败：{error}"),
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        )),
    }
}

fn current_turn_user_message(
    session_store: &SessionStore,
    session_id: &SessionId,
) -> Option<String> {
    session_store
        .active_execution_chain(session_id)
        .and_then(|chain| chain.current_turn)
        .and_then(|turn| turn.user_message)
        .map(|message| message.trim().to_string())
        .filter(|message| !message.is_empty())
}

fn select_agent_spawn_parameter_contract(
    user_message: &str,
    requested_role: &str,
    requested_display_name: &str,
    requested_goal: &str,
) -> Option<AgentSpawnParameterContract> {
    parse_agent_spawn_parameter_contracts(user_message)
        .into_iter()
        .filter_map(|contract| {
            let score = score_agent_spawn_parameter_contract(
                &contract,
                requested_role,
                requested_display_name,
                requested_goal,
            );
            (score >= 4).then_some((score, contract))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, contract)| contract)
}

fn parse_agent_spawn_parameter_contracts(user_message: &str) -> Vec<AgentSpawnParameterContract> {
    user_message
        .match_indices("display_name")
        .filter_map(|(index, _)| {
            let display_name =
                extract_display_name_after(&user_message[index + "display_name".len()..])?;
            Some(AgentSpawnParameterContract {
                role: extract_contract_role(user_message, index),
                display_name,
                access_mode: extract_contract_access_mode(user_message, index),
                goal: extract_contract_goal(user_message, index),
            })
        })
        .collect()
}

fn extract_display_name_after(value: &str) -> Option<String> {
    let trimmed = value.trim_start_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, '=' | ':' | '：' | '`' | '"' | '\'' | '为')
    });
    if let Some(rest) = trimmed.strip_prefix('「') {
        return rest
            .split_once('」')
            .map(|(name, _)| name.trim().to_string())
            .filter(|name| !name.is_empty());
    }
    let display_name = trimmed
        .chars()
        .take_while(|ch| !matches!(ch, ',' | '，' | ';' | '；' | '。' | '\n' | '\r'))
        .collect::<String>()
        .trim_matches(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '`'))
        .trim()
        .to_string();
    (!display_name.is_empty()).then_some(display_name)
}

fn extract_contract_role(user_message: &str, index: usize) -> Option<String> {
    let window = surrounding_text_window(user_message, index, 120, 0).to_ascii_lowercase();
    AGENT_ROLE_IDS
        .iter()
        .filter_map(|role| window.rfind(role).map(|pos| (pos, *role)))
        .max_by_key(|(pos, _)| *pos)
        .map(|(_, role)| role.to_string())
}

fn extract_contract_access_mode(user_message: &str, index: usize) -> Option<AgentSpawnAccessMode> {
    let window = surrounding_text_window(user_message, index, 20, 180).to_ascii_lowercase();
    if window.contains("read_only") || window.contains("readonly") || window.contains("read-only") {
        Some(AgentSpawnAccessMode::ReadOnly)
    } else if window.contains("read_write")
        || window.contains("readwrite")
        || window.contains("read-write")
    {
        Some(AgentSpawnAccessMode::ReadWrite)
    } else {
        None
    }
}

fn extract_contract_goal(user_message: &str, index: usize) -> Option<String> {
    let window = surrounding_text_window(user_message, index, 0, 260);
    let goal_start = window
        .find("目标：")
        .map(|pos| pos + "目标：".len())
        .or_else(|| window.find("目标:").map(|pos| pos + "目标:".len()))?;
    let goal = window[goal_start..]
        .chars()
        .take_while(|ch| !matches!(ch, '；' | ';' | '。' | '\n' | '\r'))
        .collect::<String>()
        .trim()
        .to_string();
    (!goal.is_empty()).then_some(goal)
}

fn surrounding_text_window(
    value: &str,
    index: usize,
    before_chars: usize,
    after_chars: usize,
) -> String {
    let before = value[..index]
        .chars()
        .rev()
        .take(before_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    let after = value[index..].chars().take(after_chars).collect::<String>();
    format!("{before}{after}")
}

fn score_agent_spawn_parameter_contract(
    contract: &AgentSpawnParameterContract,
    requested_role: &str,
    requested_display_name: &str,
    requested_goal: &str,
) -> i32 {
    let mut score = 0;
    if contract
        .role
        .as_deref()
        .is_some_and(|role| role == requested_role.trim())
    {
        score += 4;
    }
    if contract.display_name == requested_display_name.trim() {
        score += 20;
    }
    score += keyword_overlap_score(&contract.display_name, requested_display_name);
    if let Some(goal) = contract.goal.as_deref() {
        score += keyword_overlap_score(goal, requested_goal);
    }
    score
}

fn keyword_overlap_score(left: &str, right: &str) -> i32 {
    const KEYWORDS: &[&str] = &[
        "目录",
        "探查",
        "顶层",
        "配置",
        "审查",
        "检查",
        "文件",
        "README",
        "package.json",
        "tsconfig",
        "测试",
        "架构",
        "实现",
    ];
    let left_lower = left.to_ascii_lowercase();
    let right_lower = right.to_ascii_lowercase();
    KEYWORDS
        .iter()
        .filter(|keyword| {
            let keyword_lower = keyword.to_ascii_lowercase();
            left_lower.contains(&keyword_lower) && right_lower.contains(&keyword_lower)
        })
        .count() as i32
        * 4
}

fn default_agent_spawn_access_mode(role: &str) -> AgentSpawnAccessMode {
    match role.trim() {
        "architect" | "explorer" | "reviewer" => AgentSpawnAccessMode::ReadOnly,
        _ => AgentSpawnAccessMode::ReadWrite,
    }
}

fn agent_spawn_child_policy_snapshot(
    parent_policy: Option<&TaskPolicy>,
    access_mode: AgentSpawnAccessMode,
) -> TaskPolicy {
    let mut policy = parent_policy
        .cloned()
        .unwrap_or_else(default_agent_spawn_policy);
    if access_mode == AgentSpawnAccessMode::ReadOnly
        || policy.command_mode.eq_ignore_ascii_case("read_only")
    {
        policy.command_mode = "read_only".to_string();
    } else if policy.command_mode.trim().is_empty() {
        policy.command_mode = "full".to_string();
    }
    policy
}

fn default_agent_spawn_policy() -> TaskPolicy {
    TaskPolicy {
        autonomy_level: "Autonomous".to_string(),
        approval_mode: "DecisionOnly".to_string(),
        allowed_tools: Vec::new(),
        denied_tools: Vec::new(),
        allowed_paths: Vec::new(),
        denied_paths: Vec::new(),
        network_mode: "full".to_string(),
        command_mode: "full".to_string(),
        retry_limit: 1,
        validation_profile: None,
        checkpoint_mode: "turn".to_string(),
        task_tier: TaskTier::ExecutionChain,
        background_allowed: true,
        escalation_conditions: Vec::new(),
    }
}

fn enqueue_agent_assignment_message(
    conversation_registry: &ConversationRegistry,
    session_id: &SessionId,
    parent: &magi_core::Task,
    child: &magi_core::Task,
    role: &str,
    now: UtcMillis,
) {
    let access_mode = child
        .executor_binding
        .as_ref()
        .and_then(|binding| binding.get("access_mode"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("read_write");
    let child_conversation =
        conversation_registry.conversation_for_task(session_id, &child.task_id);
    child_conversation
        .lock()
        .expect("child conversation mutex poisoned")
        .ingest_runtime_signal(RuntimeSignal {
            author: MailboxAuthor::Parent(parent.task_id.to_string()),
            kind: MailboxKind::Message,
            trigger_turn: true,
            payload: serde_json::json!({
                "type": "agent_assignment",
                "parent_task_id": parent.task_id.to_string(),
                "child_task_id": child.task_id.to_string(),
                "title": child.title,
                "goal": child.goal,
                "role": role,
                "access_mode": access_mode,
            }),
            enqueued_at: now,
        });
}

fn execute_agent_wait(
    task_store: &TaskStore,
    tool: magi_tool_runtime::BuiltinToolName,
    parsed: &serde_json::Value,
) -> (String, ExecutionResultStatus) {
    let task_ids = parse_agent_wait_task_ids(parsed);
    if task_ids.is_empty() {
        return (
            serde_json::json!({
                "tool": tool.as_str(),
                "status": "failed",
                "error": "agent_wait 缺少必需字段 task_ids",
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    }
    let timeout_ms = parse_agent_wait_timeout_ms(parsed);
    let started_at = std::time::Instant::now();
    let mut observed_status_version = task_store.status_change_version();
    loop {
        let mut results = Vec::with_capacity(task_ids.len());
        let mut pending_task_ids = Vec::new();
        let requested_task_ids = task_ids.iter().map(ToString::to_string).collect::<Vec<_>>();
        for task_id in &task_ids {
            let Some(child) = task_store.get_task(task_id) else {
                results.push(serde_json::json!({
                    "child_task_id": task_id.to_string(),
                    "status": "failed",
                    "child_status": "missing",
                    "error": "TaskStore 中未找到该代理任务",
                }));
                continue;
            };
            if matches!(child.status, TaskStatus::Pending | TaskStatus::Running) {
                pending_task_ids.push(task_id.to_string());
            }
            results.push(child_agent_terminal_payload(&child));
        }
        if pending_task_ids.is_empty() {
            return (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "succeeded",
                    "timed_out": false,
                    "results": results,
                    "merge_requirements": {
                        "must_consume_child_task_ids": requested_task_ids,
                        "must_read_fields": ["assignment.goal", "status", "child_status", "result.final_text", "error"],
                        "final_answer_rule": "最终答复必须明确吸收每个代理结果；如果代理失败或降级，必须说明改派、主线接管或遗留风险。",
                    },
                    "instruction": "请读取 results 中每个代理的 assignment.goal 与 result.final_text，合并结论、证据、风险与缺口后再向用户答复。",
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            );
        }
        let elapsed_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        if elapsed_ms >= timeout_ms {
            return (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "timeout",
                    "timed_out": true,
                    "pending_task_ids": pending_task_ids,
                    "results": results,
                    "merge_requirements": {
                        "must_consume_child_task_ids": requested_task_ids,
                        "must_read_fields": ["assignment.goal", "status", "child_status", "result.final_text", "error"],
                        "final_answer_rule": "未完成代理不能被当成已完成结论；最终答复依赖这些代理时必须稍后再次 agent_wait。",
                    },
                    "instruction": "仍有代理未完成。可以继续处理不依赖这些代理结果的工作；如果最终答复依赖它们，请稍后再次调用 agent_wait。",
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            );
        }
        observed_status_version = task_store.wait_for_status_change_since(
            observed_status_version,
            std::time::Duration::from_millis(timeout_ms - elapsed_ms),
        );
    }
}

fn parse_agent_wait_task_ids(parsed: &serde_json::Value) -> Vec<TaskId> {
    parsed
        .get("task_ids")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(TaskId::new)
        .collect()
}

fn parse_agent_wait_timeout_ms(parsed: &serde_json::Value) -> u64 {
    parsed
        .get("timeout_ms")
        .and_then(|value| value.as_u64())
        .unwrap_or(AGENT_WAIT_DEFAULT_TIMEOUT_MS)
        .clamp(AGENT_WAIT_MIN_TIMEOUT_MS, AGENT_WAIT_MAX_TIMEOUT_MS)
}

fn child_agent_terminal_payload(child: &magi_core::Task) -> serde_json::Value {
    let role = child.executor_binding_target_role().unwrap_or("agent");
    let base = |status: &str, child_status: &str| {
        serde_json::json!({
            "status": status,
            "child_status": child_status,
            "child_task_id": child.task_id.to_string(),
            "role": role,
            "title": child.title,
            "assignment": {
                "title": child.title,
                "goal": child.goal,
                "role": role,
            },
        })
    };
    match child.status {
        TaskStatus::Completed => {
            let output = child_agent_output(&child.output_refs);
            let mut payload = base("succeeded", "completed");
            payload["result"] = serde_json::json!({
                "final_text": output.final_text,
                "truncated": output.truncated,
                "output_ref_count": child.output_refs.len(),
            });
            payload["summary"] = serde_json::Value::String(output.summary);
            payload["output_ref_count"] = serde_json::json!(child.output_refs.len());
            payload
        }
        TaskStatus::Failed => {
            let error = child
                .output_refs
                .first()
                .cloned()
                .unwrap_or_else(|| "代理任务执行失败".to_string());
            let output = child_agent_output(&child.output_refs);
            let mut payload = if agent_unavailable_failure(&error) {
                let mut payload = base("degraded", "failed");
                payload["fallback_mode"] =
                    serde_json::Value::String("mainline_or_reassign".to_string());
                payload["instruction"] = serde_json::Value::String(
                    "代理当前不可用。请不要停止任务：优先改派其他可用角色继续；如果没有必要继续派发，则由主线根据已有上下文直接推进并给出最终结果。".to_string(),
                );
                payload
            } else {
                base("failed", "failed")
            };
            payload["result"] = serde_json::json!({
                "final_text": output.final_text,
                "truncated": output.truncated,
                "output_ref_count": child.output_refs.len(),
            });
            payload["summary"] = serde_json::Value::String(output.summary);
            payload["output_ref_count"] = serde_json::json!(child.output_refs.len());
            payload["error"] = serde_json::Value::String(error);
            payload
        }
        TaskStatus::Killed => {
            let mut payload = base("failed", "killed");
            payload["error"] = serde_json::Value::String("代理任务被终止".to_string());
            payload
        }
        TaskStatus::Pending => base("pending", "pending"),
        TaskStatus::Running => base("running", "running"),
    }
}

fn child_agent_output(output_refs: &[String]) -> ChildAgentOutput {
    let raw = output_refs
        .iter()
        .rev()
        .find_map(|output| child_agent_final_text(output).or_else(|| non_empty_text(output)))
        .unwrap_or_else(|| "代理未返回可展示输出".to_string());
    let (final_text, truncated) = truncate_for_agent_spawn_final_text(&raw);
    ChildAgentOutput {
        summary: truncate_for_agent_spawn_summary(&raw),
        final_text,
        truncated,
    }
}

fn child_agent_final_text(output: &str) -> Option<String> {
    let parsed = serde_json::from_str::<serde_json::Value>(output).ok()?;
    let blocks = parsed.get("blocks")?.as_array()?;
    blocks.iter().rev().find_map(|block| {
        let block_type = block.get("type").and_then(|value| value.as_str())?;
        if block_type != "text" {
            return None;
        }
        block
            .get("content")
            .and_then(|value| value.as_str())
            .and_then(non_empty_text)
    })
}

fn non_empty_text(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn truncate_for_agent_spawn_summary(value: &str) -> String {
    truncate_for_agent_spawn_text(value, AGENT_SPAWN_SUMMARY_MAX_CHARS).0
}

fn truncate_for_agent_spawn_final_text(value: &str) -> (String, bool) {
    truncate_for_agent_spawn_text(value, AGENT_SPAWN_FINAL_TEXT_MAX_CHARS)
}

fn truncate_for_agent_spawn_text(value: &str, max_chars: usize) -> (String, bool) {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max_chars {
        return (trimmed.to_string(), false);
    }
    let mut output = trimmed.chars().take(max_chars).collect::<String>();
    output.push('…');
    (output, true)
}

fn agent_unavailable_failure(error: &str) -> bool {
    let normalized = error.trim().to_ascii_lowercase();
    [
        "llm invocation failed",
        "模型配置不可用",
        "model bridge client",
        "代理不可用",
        "没有匹配角色",
        "没有匹配",
        "provider transport failed",
        "provider rejected request",
        "invalid base_url",
        "connection refused",
        "timed out",
        "timeout",
    ]
    .iter()
    .any(|needle| normalized.contains(&needle.to_ascii_lowercase()))
}

#[allow(clippy::too_many_arguments)]
fn execute_task_tool_call(
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    agent_role_registry: &magi_agent_role::AgentRoleRegistry,
    skill_runtime: Option<&magi_skill_runtime::SkillRuntime>,
    task_store: &TaskStore,
    session_store: &SessionStore,
    execution_registry: &TaskExecutionRegistry,
    conversation_registry: &ConversationRegistry,
    spawn_graph: &Mutex<magi_spawn_graph::SpawnGraph>,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    todo_ledger: &magi_todo_ledger::TodoLedger,
    project_memory: Option<&magi_project_memory::ProjectMemoryStore>,
    mission_charter: Option<&magi_mission_charter::MissionCharterStore>,
    plan: Option<&magi_plan::PlanStore>,
    knowledge_graph: Option<&magi_knowledge_graph::KnowledgeGraphStore>,
    validation_runner: Option<&magi_validation_runner::ValidationStore>,
    checkpoint: Option<&magi_checkpoint::CheckpointStore>,
    human_checkpoint: Option<&magi_human_checkpoint::HumanCheckpointStore>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    workspace_root_path: Option<&PathBuf>,
    worker_id: Option<&magi_core::WorkerId>,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    // S7-E：协调器工具（agent_spawn）由 orchestration 层拦截，
    // 不进 BuiltinTool::execute —— 它需要 task_store / spawn_graph / event_bus 等上下文。
    // S9：TodoWrite 同样在此层拦截，因为它要操作 session 维度的 TodoLedger。
    // S10：MemoryWrite 同样在此层拦截，因为它要操作 workspace 维度的 ProjectMemoryStore。
    // S11：MissionCharterWrite 同样在此层拦截，因为它要操作 mission 维度的 MissionCharterStore。
    // S12：PlanWrite 同样在此层拦截，因为它要操作 mission 维度的 PlanStore。
    if let Some(canonical) =
        magi_tool_runtime::BuiltinToolName::from_str(tool_call.function.name.as_str())
    {
        if !task_can_see_builtin_tool(Some(task), Some(agent_role_registry), canonical) {
            return (
                serde_json::json!({
                    "tool": canonical.as_str(),
                    "status": "rejected",
                    "error": "tool is not visible for this task role or task tier",
                })
                .to_string(),
                ExecutionResultStatus::Rejected,
            );
        }
        if matches!(
            canonical,
            magi_tool_runtime::BuiltinToolName::AgentSpawn
                | magi_tool_runtime::BuiltinToolName::AgentWait
        ) {
            return execute_coordinator_tool(
                event_bus,
                agent_role_registry,
                task_store,
                session_store,
                execution_registry,
                conversation_registry,
                spawn_graph,
                mission_charter,
                plan,
                human_checkpoint,
                task,
                session_id,
                workspace_id,
                canonical,
                tool_call,
            );
        }
        if matches!(canonical, magi_tool_runtime::BuiltinToolName::TodoWrite) {
            return magi_todo_ledger::execute_todo_write_tool(
                event_bus,
                todo_ledger,
                session_id,
                workspace_id.as_ref(),
                &task.task_id,
                &task.mission_id,
                &tool_call.function.arguments,
            );
        }
        if matches!(canonical, magi_tool_runtime::BuiltinToolName::MemoryWrite) {
            return magi_project_memory::execute_memory_write_tool(
                event_bus,
                project_memory,
                session_id,
                workspace_id.as_ref(),
                &task.task_id,
                &task.mission_id,
                &tool_call.function.arguments,
            );
        }
        if matches!(
            canonical,
            magi_tool_runtime::BuiltinToolName::MissionCharterWrite
        ) {
            return magi_mission_charter::execute_mission_charter_write_tool(
                event_bus,
                mission_charter,
                human_checkpoint,
                session_id,
                workspace_id.as_ref(),
                &task.task_id,
                &task.mission_id,
                &tool_call.function.arguments,
            );
        }
        if matches!(canonical, magi_tool_runtime::BuiltinToolName::PlanWrite) {
            return magi_plan::execute_plan_write_tool(
                event_bus,
                plan,
                validation_runner,
                session_id,
                workspace_id.as_ref(),
                &task.task_id,
                &task.mission_id,
                &tool_call.function.arguments,
            );
        }
        // S14：KgWrite 同样在此层拦截，因为它要操作 mission 维度的 KnowledgeGraphStore。
        if matches!(canonical, magi_tool_runtime::BuiltinToolName::KgWrite) {
            return magi_knowledge_graph::execute_kg_write_tool(
                event_bus,
                knowledge_graph,
                session_id,
                workspace_id.as_ref(),
                &task.task_id,
                &task.mission_id,
                &tool_call.function.arguments,
            );
        }
        // S15：ValidationRecord 同样在此层拦截，因为它要操作 mission 维度的 ValidationStore。
        if matches!(
            canonical,
            magi_tool_runtime::BuiltinToolName::ValidationRecord
        ) {
            return magi_validation_runner::execute_validation_record_tool(
                event_bus,
                validation_runner,
                session_id,
                workspace_id.as_ref(),
                &task.task_id,
                &task.mission_id,
                &tool_call.function.arguments,
            );
        }
        // S16：Checkpoint 同样在此层拦截，因为它要操作 mission 维度的 CheckpointStore。
        if matches!(canonical, magi_tool_runtime::BuiltinToolName::Checkpoint) {
            return magi_checkpoint::execute_checkpoint_create_tool(
                event_bus,
                checkpoint,
                session_id,
                workspace_id.as_ref(),
                &task.task_id,
                &task.mission_id,
                &tool_call.function.arguments,
            );
        }
        // S17：HumanCheckpointRequest 同样在此层拦截，因为它要操作 mission 维度的
        // HumanCheckpointStore。pending 后续由 agent_spawn 拦截与 TaskRunner gate 硬阻塞。
        if matches!(
            canonical,
            magi_tool_runtime::BuiltinToolName::HumanCheckpointRequest
        ) {
            return magi_human_checkpoint::execute_human_checkpoint_request_tool(
                event_bus,
                human_checkpoint,
                session_id,
                workspace_id.as_ref(),
                &task.task_id,
                &task.mission_id,
                &tool_call.function.arguments,
            );
        }
    }

    let Some(registry) = tool_registry else {
        return (
            serde_json::json!({ "error": "tool registry not available" }).to_string(),
            ExecutionResultStatus::Failed,
        );
    };

    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-task-tool-invoked-{}", UtcMillis::now().0)),
            "task.tool.invoked",
            serde_json::json!({
                "task_id": task.task_id.to_string(),
                "mission_id": task.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "worker_id": worker_id.map(ToString::to_string),
                "tool_name": tool_call.function.name,
                "tool_call_id": tool_call.id,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            mission_id: Some(task.mission_id.clone()),
            task_id: Some(task.task_id.clone()),
            ..EventContext::default()
        }),
    );

    if tool_call.function.name == SKILL_APPLY_TOOL_NAME {
        return execute_skill_apply_from_runtime(&tool_call.function.arguments, skill_runtime);
    }

    if let Some(rejection) = internal_builtin_tool_rejection_payload(&tool_call.function.name) {
        return (rejection, ExecutionResultStatus::Failed);
    }

    if let Some(rejection) = task_policy_tool_rejection(
        task,
        &tool_call.function.name,
        &tool_call.function.arguments,
    ) {
        return (rejection, ExecutionResultStatus::Rejected);
    }

    // S8：SafetyGate 语义判定。Permission 通过后仍可能命中"高危子串"（如
    // `git push --force` / `rm -rf`），此处对 arguments 内容直接做匹配。
    if let Some(gate) = safety_gate {
        if let Some(rejection) = safety_gate_rejection(
            gate,
            &tool_call.function.name,
            &tool_call.function.arguments,
        ) {
            return (rejection, ExecutionResultStatus::Rejected);
        }
    }

    let output = registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new(&tool_call.id),
            tool_name: tool_call.function.name.clone(),
            tool_kind: ToolKind::Builtin,
            input: tool_call.function.arguments.clone(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext {
            worker_id: worker_id.cloned(),
            task_id: Some(task.task_id.clone()),
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.clone(),
            working_directory: workspace_root_path.cloned(),
        },
        &ToolExecutionPolicy::default(),
    );

    (output.payload, output.status)
}

fn task_policy_tool_rejection(
    task: &magi_core::Task,
    requested_tool_name: &str,
    arguments: &str,
) -> Option<String> {
    let policy_snapshot = task.policy_snapshot.as_ref()?;
    let canonical_tool_name = canonical_builtin_tool_name(requested_tool_name)
        .unwrap_or_else(|| requested_tool_name.trim().to_string());
    // no_tools 是 PermissionEngine 三维之外的全局开关，本层先单独拦截。
    if policy_snapshot
        .command_mode
        .eq_ignore_ascii_case("no_tools")
    {
        return Some(task_policy_rejection_payload(
            &canonical_tool_name,
            format!("当前任务阶段不允许调用工具: {canonical_tool_name}"),
        ));
    }
    // PermissionEngine 比对工具名是按字面比对，因此把 policy 中的别名先 canonical 化。
    let mut canonical_policy =
        magi_permissions::PermissionPolicy::from_core_policy(policy_snapshot);
    canonical_policy.allowed_tools = policy_snapshot
        .allowed_tools
        .iter()
        .map(|tool| canonical_builtin_tool_name(tool).unwrap_or_else(|| tool.trim().to_string()))
        .collect();
    canonical_policy.denied_tools = policy_snapshot
        .denied_tools
        .iter()
        .map(|tool| canonical_builtin_tool_name(tool).unwrap_or_else(|| tool.trim().to_string()))
        .collect();

    let engine = magi_permissions::PermissionEngine::with_builtin_defaults();
    let is_write_tool = BuiltinToolName::from_str(canonical_tool_name.as_str())
        .is_some_and(|tool| tool.is_write_operation());

    let tool_request = magi_permissions::PermissionRequest::ToolInvocation {
        tool_name: canonical_tool_name.as_str(),
        is_write_tool,
    };
    if let magi_permissions::Decision::Deny { reason } = engine.decide(
        &tool_request,
        &canonical_policy,
        magi_permissions::PermissionMode::Default,
    ) {
        return Some(task_policy_rejection_payload(&canonical_tool_name, reason));
    }
    // shell_exec 在只读任务下需要 access_mode=read_only —— 走 ShellCommand 轴判定。
    if canonical_tool_name == BuiltinToolName::ShellExec.as_str() {
        let shell_request = magi_permissions::PermissionRequest::ShellCommand {
            arguments_json: arguments,
        };
        if let magi_permissions::Decision::Deny { reason } = engine.decide(
            &shell_request,
            &canonical_policy,
            magi_permissions::PermissionMode::Default,
        ) {
            return Some(task_policy_rejection_payload(&canonical_tool_name, reason));
        }
    }
    None
}

fn canonical_builtin_tool_name(tool_name: &str) -> Option<String> {
    BuiltinToolName::from_str(tool_name.trim()).map(|tool| tool.as_str().to_string())
}

fn task_policy_rejection_payload(tool_name: &str, error: String) -> String {
    serde_json::json!({
        "tool": tool_name,
        "status": "rejected",
        "error": error,
    })
    .to_string()
}

/// S8：把 SafetyGate 的 Block / RequireApproval 判定折叠成"Rejected payload"。
/// 当前 conversation_loop 没有交互审批通道（governance 走自己的回路），所以
/// RequireApproval 在本层与 Block 同语义：拒绝执行并把原因回灌给模型，由模型决定
/// 是否换更精确的命令或转向人审通道。
fn safety_gate_rejection(
    gate: &magi_safety_gate::SafetyGate,
    tool_name: &str,
    arguments: &str,
) -> Option<String> {
    let canonical_tool_name =
        canonical_builtin_tool_name(tool_name).unwrap_or_else(|| tool_name.trim().to_string());
    match gate.evaluate(&canonical_tool_name, arguments) {
        magi_safety_gate::SafetyDecision::Allow => None,
        magi_safety_gate::SafetyDecision::Block {
            category,
            pattern,
            reason,
        }
        | magi_safety_gate::SafetyDecision::RequireApproval {
            category,
            pattern,
            reason,
        } => Some(
            serde_json::json!({
                "tool": canonical_tool_name,
                "status": "rejected",
                "error": reason,
                "safety_gate": {
                    "category": category.as_str(),
                    "pattern": pattern,
                },
            })
            .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::ChatToolFunction;
    use magi_core::{MissionId, Task, TaskPolicy, TaskRuntimePayload, WorkspaceRootPath};
    use std::sync::Arc;
    use tempfile::tempdir;

    fn test_task(task_id: &str, root_task_id: &str, parent_task_id: Option<TaskId>) -> Task {
        Task {
            task_id: TaskId::new(task_id),
            mission_id: MissionId::new("mission-mailbox"),
            root_task_id: TaskId::new(root_task_id),
            parent_task_id,
            kind: TaskKind::LocalAgent,
            title: format!("task {task_id}"),
            goal: format!("run task {task_id}"),
            status: TaskStatus::Running,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: TaskRuntimePayload::default(),
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        }
    }

    fn coordinator_task(mut task: Task) -> Task {
        task.executor_binding = Some(serde_json::json!({
            "target_role": "coordinator",
        }));
        task
    }

    fn long_mission_policy() -> TaskPolicy {
        TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "HumanCheckpoint".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "restricted".to_string(),
            command_mode: "restricted".to_string(),
            retry_limit: 1,
            validation_profile: Some("Required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            task_tier: magi_core::TaskTier::LongMission,
            background_allowed: true,
            escalation_conditions: vec!["human_checkpoint".to_string()],
        }
    }

    #[test]
    fn agent_spawn_access_mode_defaults_to_read_only_for_review_roles() {
        assert_eq!(
            default_agent_spawn_access_mode("explorer"),
            AgentSpawnAccessMode::ReadOnly
        );
        assert_eq!(
            default_agent_spawn_access_mode("reviewer"),
            AgentSpawnAccessMode::ReadOnly
        );
        assert_eq!(
            default_agent_spawn_access_mode("architect"),
            AgentSpawnAccessMode::ReadOnly
        );
        assert_eq!(
            default_agent_spawn_access_mode("executor"),
            AgentSpawnAccessMode::ReadWrite
        );
    }

    #[test]
    fn agent_spawn_child_policy_applies_read_only_access_mode() {
        let parent = default_agent_spawn_policy();
        let child =
            agent_spawn_child_policy_snapshot(Some(&parent), AgentSpawnAccessMode::ReadOnly);

        assert_eq!(child.command_mode, "read_only");
        assert_eq!(child.network_mode, parent.network_mode);
        assert_eq!(child.task_tier, parent.task_tier);
    }

    #[test]
    fn agent_spawn_child_policy_never_escalates_parent_read_only() {
        let mut parent = default_agent_spawn_policy();
        parent.command_mode = "read_only".to_string();

        let child =
            agent_spawn_child_policy_snapshot(Some(&parent), AgentSpawnAccessMode::ReadWrite);

        assert_eq!(child.command_mode, "read_only");
    }

    #[test]
    fn agent_spawn_contract_parser_extracts_explicit_user_parameters() {
        let contracts = parse_agent_spawn_parameter_contracts(
            "必须同一轮并行启动 2 个代理：1) role=explorer，display_name=「目录探查代理」，access_mode=read_only，目标：只读查看 /Users/xie/code/TEST 顶层目录；2) role=reviewer，display_name=「配置审查代理」，access_mode=read_only，目标：只读查看 README.md 和 package.json 是否存在。",
        );

        assert_eq!(contracts.len(), 2);
        assert_eq!(contracts[0].role.as_deref(), Some("explorer"));
        assert_eq!(contracts[0].display_name, "目录探查代理");
        assert_eq!(
            contracts[0].access_mode,
            Some(AgentSpawnAccessMode::ReadOnly)
        );
        assert_eq!(contracts[1].role.as_deref(), Some("reviewer"));
        assert_eq!(contracts[1].display_name, "配置审查代理");
        assert!(
            contracts[1]
                .goal
                .as_deref()
                .unwrap_or_default()
                .contains("package.json")
        );
    }

    #[test]
    fn agent_spawn_contract_role_does_not_bleed_from_next_agent_clause() {
        let contracts = parse_agent_spawn_parameter_contracts(
            "第一轮同时 agent_spawn 两个只读代理：explorer display_name「冻结目录代理」只做根目录巡检；reviewer display_name「冻结配置代理」只读取 package.json 指出一个风险。",
        );

        assert_eq!(contracts.len(), 2);
        assert_eq!(contracts[0].role.as_deref(), Some("explorer"));
        assert_eq!(contracts[0].display_name, "冻结目录代理");
        assert_eq!(contracts[1].role.as_deref(), Some("reviewer"));
        assert_eq!(contracts[1].display_name, "冻结配置代理");
    }

    #[test]
    fn agent_spawn_contract_selection_corrects_model_rewritten_display_name() {
        let message = "必须同一轮并行启动 2 个代理：1) role=explorer，display_name=「目录探查代理」，access_mode=read_only，目标：只读查看 /Users/xie/code/TEST 顶层目录；2) role=reviewer，display_name=「配置审查代理」，access_mode=read_only，目标：只读查看 README.md 和 package.json 是否存在。";

        let directory = select_agent_spawn_parameter_contract(
            message,
            "explorer",
            "目录探查员",
            "只读检查当前工作区目录 /Users/xie/code/TEST",
        )
        .expect("directory contract should be selected");
        assert_eq!(directory.role.as_deref(), Some("explorer"));
        assert_eq!(directory.display_name, "目录探查代理");

        let config = select_agent_spawn_parameter_contract(
            message,
            "reviewer",
            "文件存在审查员",
            "只读查看 README.md 和 package.json 是否存在",
        )
        .expect("config contract should be selected");
        assert_eq!(config.role.as_deref(), Some("reviewer"));
        assert_eq!(config.display_name, "配置审查代理");
    }

    #[test]
    fn agent_spawn_contract_selection_prefers_goal_when_model_rewrites_role() {
        let message = "必须同一轮并行启动 2 个代理：1) role=explorer，display_name=「目录探查代理」，access_mode=read_only，目标：只读查看 /Users/xie/code/TEST 顶层目录；2) role=reviewer，display_name=「配置审查代理」，access_mode=read_only，目标：只读查看 README.md 和 package.json 是否存在。";

        let config = select_agent_spawn_parameter_contract(
            message,
            "explorer",
            "关键文件检查员",
            "只读验证 /Users/xie/code/TEST/README.md 与 /Users/xie/code/TEST/package.json 是否存在且为普通文件",
        )
        .expect("config contract should be selected by goal overlap");

        assert_eq!(config.role.as_deref(), Some("reviewer"));
        assert_eq!(config.display_name, "配置审查代理");
    }

    #[test]
    fn read_only_agent_policy_rejects_write_tool() {
        let mut task = test_task("task-read-only-agent", "task-read-only-agent", None);
        task.policy_snapshot = Some(agent_spawn_child_policy_snapshot(
            Some(&default_agent_spawn_policy()),
            AgentSpawnAccessMode::ReadOnly,
        ));

        let rejection = task_policy_tool_rejection(
            &task,
            BuiltinToolName::FileWrite.as_str(),
            r#"{"path":"probe.txt","content":""}"#,
        )
        .expect("read-only agent should reject file_write");
        let payload: serde_json::Value =
            serde_json::from_str(&rejection).expect("rejection should be json");

        assert_eq!(payload["status"].as_str(), Some("rejected"));
        assert_eq!(payload["tool"].as_str(), Some("file_write"));
        assert!(
            payload["error"]
                .as_str()
                .unwrap_or_default()
                .contains("只读任务不允许执行写入工具")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn task_tool_batch_records_snapshot_worker_attribution() {
        let dir = tempdir().expect("temp dir");
        let workspace_root = dir.path().to_path_buf();
        let snapshot = magi_snapshot::SnapshotManager::new()
            .start_session("session-task-snapshot".to_string(), workspace_root.clone())
            .await
            .expect("snapshot session should start");

        let event_bus = InMemoryEventBus::new(16);
        let task_store = TaskStore::new();
        let session_store = SessionStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let conversation_registry = ConversationRegistry::new();
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let todo_ledger = magi_todo_ledger::TodoLedger::new();
        let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
        let mut tool_registry = ToolRegistry::new(
            Arc::new(magi_governance::GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_default_builtins();

        let task = test_task("task-snapshot-agent", "task-snapshot-agent", None);
        let session_id = SessionId::new("session-task-snapshot");
        let workspace_id = Some(WorkspaceId::new("workspace-task-snapshot"));
        let worker_id = magi_core::WorkerId::new("worker-agent-edit");
        let tool_call = ChatToolCall {
            id: "call-file-write-agent".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::FileWrite.as_str().to_string(),
                arguments: serde_json::json!({
                    "path": "agent.txt",
                    "content": "hello from agent"
                })
                .to_string(),
            },
        };

        let result = execute_task_tool_call_batch(
            &event_bus,
            Some(&tool_registry),
            &agent_role_registry,
            None,
            &task_store,
            &session_store,
            &execution_registry,
            &conversation_registry,
            &spawn_graph,
            None,
            &todo_ledger,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &session_id,
            &workspace_id,
            Some(&workspace_root),
            Some(&worker_id),
            &[tool_call],
            Some(snapshot.clone()),
            Some(task.mission_id.to_string()),
        );

        assert_eq!(result[0].1, ExecutionResultStatus::Succeeded);
        let pending = snapshot.pending_changes().expect("pending changes");
        let change = pending
            .iter()
            .find(|change| change.path == "agent.txt")
            .expect("agent.txt should be tracked");
        assert_eq!(change.source, magi_snapshot::SourceKind::Tool);
        assert_eq!(
            change.tool_call_id.as_deref(),
            Some("call-file-write-agent")
        );
        assert_eq!(change.worker_id.as_deref(), Some("worker-agent-edit"));
        assert_eq!(
            change.execution_group_id.as_deref(),
            Some("mission-mailbox")
        );

        let mainline_tool_call = ChatToolCall {
            id: "call-file-write-mainline".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::FileWrite.as_str().to_string(),
                arguments: serde_json::json!({
                    "path": "mainline.txt",
                    "content": "hello from mainline"
                })
                .to_string(),
            },
        };
        let mainline_result = execute_task_tool_call_batch(
            &event_bus,
            Some(&tool_registry),
            &agent_role_registry,
            None,
            &task_store,
            &session_store,
            &execution_registry,
            &conversation_registry,
            &spawn_graph,
            None,
            &todo_ledger,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &session_id,
            &workspace_id,
            Some(&workspace_root),
            None,
            &[mainline_tool_call],
            Some(snapshot.clone()),
            Some(task.mission_id.to_string()),
        );

        assert_eq!(mainline_result[0].1, ExecutionResultStatus::Succeeded);
        let pending = snapshot.pending_changes().expect("pending changes");
        let mainline_change = pending
            .iter()
            .find(|change| change.path == "mainline.txt")
            .expect("mainline.txt should be tracked");
        assert_eq!(mainline_change.source, magi_snapshot::SourceKind::Tool);
        assert_eq!(
            mainline_change.tool_call_id.as_deref(),
            Some("call-file-write-mainline")
        );
        assert_eq!(mainline_change.worker_id, None);
        assert_eq!(
            mainline_change.execution_group_id.as_deref(),
            Some("mission-mailbox")
        );
    }

    fn pending_human_checkpoint_store(
        mission_id: &MissionId,
    ) -> magi_human_checkpoint::HumanCheckpointStore {
        let tmp = std::env::temp_dir().join(format!(
            "magi-tool-batch-human-checkpoint-{}-{}",
            std::process::id(),
            UtcMillis::now().0
        ));
        std::fs::create_dir_all(&tmp).expect("temp human checkpoint home should be created");
        let workspace_root = WorkspaceRootPath::new(format!("{}/workspace", tmp.display()));
        let store =
            magi_human_checkpoint::HumanCheckpointStore::open_with_home(&tmp, &workspace_root)
                .expect("human checkpoint store should open");
        let mut log =
            magi_human_checkpoint::HumanCheckpointLog::new(mission_id.clone(), UtcMillis(0));
        magi_human_checkpoint::append_human_checkpoint_request(
            &mut log,
            magi_human_checkpoint::HumanCheckpointRequestArgs {
                plan_step_id: "review-before-spawn".to_string(),
                prompt_to_human: "确认后才能继续派发代理".to_string(),
                label: Some("spawn-gate".to_string()),
                context: None,
            },
            UtcMillis(1),
        );
        store.save(&log).expect("human checkpoint log should save");
        store
    }

    #[test]
    fn long_mission_agent_spawn_requires_charter_and_non_empty_plan() {
        let mut parent = coordinator_task(test_task(
            "task-parent-long-mission-prerequisites",
            "task-parent-long-mission-prerequisites",
            None,
        ));
        parent.policy_snapshot = Some(long_mission_policy());
        let tmp = std::env::temp_dir().join(format!(
            "magi-tool-batch-long-mission-prereq-{}-{}",
            std::process::id(),
            UtcMillis::now().0
        ));
        std::fs::create_dir_all(&tmp).expect("temp long mission home should be created");
        let workspace_root = WorkspaceRootPath::new(format!("{}/workspace", tmp.display()));
        let charter_store =
            magi_mission_charter::MissionCharterStore::open_with_home(&tmp, &workspace_root)
                .expect("charter store should open");
        let plan_store = magi_plan::PlanStore::open_with_home(&tmp, &workspace_root)
            .expect("plan store should open");

        let missing_charter = long_mission_agent_spawn_prerequisite_rejection(
            BuiltinToolName::AgentSpawn,
            Some(&charter_store),
            Some(&plan_store),
            &parent,
        )
        .expect("missing charter should reject agent_spawn");
        assert_eq!(missing_charter.1, ExecutionResultStatus::Rejected);
        assert!(missing_charter.0.contains("mission_charter_write"));

        let mut charter = magi_mission_charter::MissionCharter::new(
            parent.mission_id.clone(),
            "LongMission 前置约束",
            "验证长任务必须先建立 mission 契约和执行计划，然后才能派发代理。",
            UtcMillis(1),
        );
        charter.success_criteria = vec!["代理派发前存在可追踪计划".to_string()];
        charter.constraints = vec!["不能跳过治理前置步骤".to_string()];
        charter_store.save(&charter).expect("charter should save");

        let missing_plan = long_mission_agent_spawn_prerequisite_rejection(
            BuiltinToolName::AgentSpawn,
            Some(&charter_store),
            Some(&plan_store),
            &parent,
        )
        .expect("missing plan should reject agent_spawn");
        assert_eq!(missing_plan.1, ExecutionResultStatus::Rejected);
        assert!(missing_plan.0.contains("plan_write"));

        let mut plan = magi_plan::Plan::new(parent.mission_id.clone(), UtcMillis(2));
        plan.steps.push(magi_plan::PlanStep {
            id: "spawn-wave".to_string(),
            content: "建立首轮代理派发计划".to_string(),
            status: magi_plan::PlanStepStatus::Pending,
            depends_on: Vec::new(),
            notes: None,
        });
        plan_store.save(&plan).expect("plan should save");

        assert!(
            long_mission_agent_spawn_prerequisite_rejection(
                BuiltinToolName::AgentSpawn,
                Some(&charter_store),
                Some(&plan_store),
                &parent,
            )
            .is_none(),
            "charter 与非空 plan 都存在时才允许 LongMission 派发代理"
        );
    }

    #[test]
    fn child_agent_output_extracts_final_text_and_marks_truncation() {
        let long_final_text = "结果".repeat(4000);
        let output_refs = vec![
            serde_json::json!({
                "blocks": [
                    {
                        "type": "tool_call",
                        "content": "shell_exec: ok"
                    },
                    {
                        "type": "text",
                        "content": long_final_text
                    }
                ]
            })
            .to_string(),
        ];

        let output = child_agent_output(&output_refs);

        assert!(output.summary.chars().count() <= AGENT_SPAWN_SUMMARY_MAX_CHARS + 1);
        assert_eq!(
            output.final_text.chars().count(),
            AGENT_SPAWN_FINAL_TEXT_MAX_CHARS + 1
        );
        assert!(output.truncated);
        assert!(output.final_text.starts_with("结果结果"));
    }

    #[test]
    fn agent_wait_returns_completed_agent_final_text() {
        let task_store = TaskStore::new();
        let mut child = test_task(
            "task-agent-wait-child",
            "task-agent-wait-root",
            Some(TaskId::new("task-agent-wait-root")),
        );
        child.status = TaskStatus::Completed;
        child.title = "目录探索".to_string();
        child.goal = "列出目录并汇报".to_string();
        child.executor_binding = Some(serde_json::json!({
            "target_role": "explorer",
        }));
        child.output_refs = vec![
            serde_json::json!({
                "blocks": [
                    {
                        "type": "text",
                        "content": "已完成目录探索，发现 README.md。"
                    }
                ]
            })
            .to_string(),
        ];
        task_store.insert_task(child);

        let (payload, status) = execute_agent_wait(
            &task_store,
            BuiltinToolName::AgentWait,
            &serde_json::json!({
                "task_ids": ["task-agent-wait-child"],
                "timeout_ms": 1000,
            }),
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value =
            serde_json::from_str(&payload).expect("agent_wait result should be json");
        assert_eq!(parsed["status"].as_str(), Some("succeeded"));
        assert_eq!(parsed["timed_out"].as_bool(), Some(false));
        assert_eq!(
            parsed["results"][0]["child_status"].as_str(),
            Some("completed")
        );
        assert_eq!(
            parsed["results"][0]["assignment"]["goal"].as_str(),
            Some("列出目录并汇报")
        );
        assert_eq!(
            parsed["results"][0]["result"]["final_text"].as_str(),
            Some("已完成目录探索，发现 README.md。")
        );
    }

    #[test]
    fn agent_wait_marks_unavailable_agent_as_degradable() {
        let task_store = TaskStore::new();
        let mut child = test_task(
            "task-agent-wait-unavailable",
            "task-agent-wait-root",
            Some(TaskId::new("task-agent-wait-root")),
        );
        child.status = TaskStatus::Failed;
        child.title = "配置审查代理".to_string();
        child.goal = "检查模型配置是否可用".to_string();
        child.executor_binding = Some(serde_json::json!({
            "target_role": "reviewer",
        }));
        child.output_refs = vec!["provider transport failed: connection refused".to_string()];
        task_store.insert_task(child);

        let (payload, status) = execute_agent_wait(
            &task_store,
            BuiltinToolName::AgentWait,
            &serde_json::json!({
                "task_ids": ["task-agent-wait-unavailable"],
                "timeout_ms": 1000,
            }),
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value =
            serde_json::from_str(&payload).expect("agent_wait result should be json");
        let result = &parsed["results"][0];
        assert_eq!(result["status"].as_str(), Some("degraded"));
        assert_eq!(result["child_status"].as_str(), Some("failed"));
        assert_eq!(
            result["fallback_mode"].as_str(),
            Some("mainline_or_reassign")
        );
        assert!(
            result["instruction"]
                .as_str()
                .unwrap_or_default()
                .contains("不要停止任务")
        );
        assert_eq!(
            result["result"]["final_text"].as_str(),
            Some("provider transport failed: connection refused")
        );
    }

    #[test]
    fn agent_wait_preserves_non_degradable_agent_failure() {
        let task_store = TaskStore::new();
        let mut child = test_task(
            "task-agent-wait-real-failure",
            "task-agent-wait-root",
            Some(TaskId::new("task-agent-wait-root")),
        );
        child.status = TaskStatus::Failed;
        child.title = "冒烟测试代理".to_string();
        child.goal = "运行冒烟测试并报告失败原因".to_string();
        child.executor_binding = Some(serde_json::json!({
            "target_role": "tester",
        }));
        child.output_refs = vec!["测试失败：断言不匹配".to_string()];
        task_store.insert_task(child);

        let (payload, status) = execute_agent_wait(
            &task_store,
            BuiltinToolName::AgentWait,
            &serde_json::json!({
                "task_ids": ["task-agent-wait-real-failure"],
                "timeout_ms": 1000,
            }),
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value =
            serde_json::from_str(&payload).expect("agent_wait result should be json");
        let result = &parsed["results"][0];
        assert_eq!(result["status"].as_str(), Some("failed"));
        assert_eq!(result["child_status"].as_str(), Some("failed"));
        assert!(result.get("fallback_mode").is_none());
        assert_eq!(
            result["result"]["final_text"].as_str(),
            Some("测试失败：断言不匹配")
        );
    }

    #[test]
    fn agent_spawn_assignment_message_enters_child_conversation_mailbox() {
        let registry = ConversationRegistry::new();
        let session_id = SessionId::new("session-agent-assignment-mailbox");
        let parent = coordinator_task(test_task(
            "task-agent-assignment-parent",
            "task-agent-assignment-parent",
            None,
        ));
        let mut child = test_task(
            "task-agent-assignment-child",
            "task-agent-assignment-parent",
            Some(parent.task_id.clone()),
        );
        child.title = "目录探索".to_string();
        child.goal = "列出目录并汇报".to_string();

        enqueue_agent_assignment_message(
            &registry,
            &session_id,
            &parent,
            &child,
            "explorer",
            UtcMillis(42),
        );

        let child_conversation = registry.conversation_for_task(&session_id, &child.task_id);
        let items = child_conversation.lock().unwrap().drain_mailbox_items();

        assert_eq!(items.len(), 1);
        match &items[0] {
            crate::MailboxItem::Runtime(signal) => {
                assert_eq!(
                    signal.author,
                    MailboxAuthor::Parent("task-agent-assignment-parent".to_string())
                );
                assert_eq!(signal.kind, MailboxKind::Message);
                assert!(signal.trigger_turn);
                assert_eq!(signal.payload["type"].as_str(), Some("agent_assignment"));
                assert_eq!(signal.payload["goal"].as_str(), Some("列出目录并汇报"));
            }
            other => panic!("expected runtime assignment message, got {other:?}"),
        }
    }

    #[test]
    fn tool_execution_rejects_role_or_tier_invisible_builtin_tools() {
        let event_bus = InMemoryEventBus::new(16);
        let task_store = TaskStore::new();
        let session_store = SessionStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let conversation_registry = ConversationRegistry::new();
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let todo_ledger = magi_todo_ledger::TodoLedger::new();
        let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
        let session_id = SessionId::new("session-tool-scope");
        let workspace_id = Some(WorkspaceId::new("workspace-tool-scope"));

        let worker = test_task("task-worker", "task-worker", None);
        let worker_result = execute_task_tool_call_batch(
            &event_bus,
            None,
            &agent_role_registry,
            None,
            &task_store,
            &session_store,
            &execution_registry,
            &conversation_registry,
            &spawn_graph,
            None,
            &todo_ledger,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &worker,
            &session_id,
            &workspace_id,
            None,
            None,
            &[ChatToolCall {
                id: "call-agent-spawn-rejected".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: BuiltinToolName::AgentSpawn.as_str().to_string(),
                    arguments: "{}".to_string(),
                },
            }],
            None,
            None,
        );
        assert_eq!(worker_result[0].1, ExecutionResultStatus::Rejected);

        let coordinator = coordinator_task(test_task("task-coordinator", "task-coordinator", None));
        let coordinator_result = execute_task_tool_call_batch(
            &event_bus,
            None,
            &agent_role_registry,
            None,
            &task_store,
            &session_store,
            &execution_registry,
            &conversation_registry,
            &spawn_graph,
            None,
            &todo_ledger,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &coordinator,
            &session_id,
            &workspace_id,
            None,
            None,
            &[ChatToolCall {
                id: "call-plan-write-rejected".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: BuiltinToolName::PlanWrite.as_str().to_string(),
                    arguments: "{}".to_string(),
                },
            }],
            None,
            None,
        );
        assert_eq!(coordinator_result[0].1, ExecutionResultStatus::Rejected);
    }

    #[test]
    fn agent_spawn_rejects_when_human_checkpoint_is_pending() {
        let event_bus = InMemoryEventBus::new(16);
        let task_store = TaskStore::new();
        let session_store = SessionStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let conversation_registry = ConversationRegistry::new();
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let todo_ledger = magi_todo_ledger::TodoLedger::new();
        let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();

        let session_id = SessionId::new("session-agent-spawn-human-gate");
        let workspace_id = Some(WorkspaceId::new("workspace-agent-spawn-human-gate"));
        let parent = coordinator_task(test_task(
            "task-parent-human-gate",
            "task-parent-human-gate",
            None,
        ));
        task_store.insert_task(parent.clone());
        let human_checkpoint = pending_human_checkpoint_store(&parent.mission_id);

        let tool_call = ChatToolCall {
            id: "call-agent-spawn-human-gate".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::AgentSpawn.as_str().to_string(),
                arguments: serde_json::json!({
                    "role": "executor",
                    "goal": "不应创建的代理任务"
                })
                .to_string(),
            },
        };

        let result = execute_task_tool_call_batch(
            &event_bus,
            None,
            &agent_role_registry,
            None,
            &task_store,
            &session_store,
            &execution_registry,
            &conversation_registry,
            &spawn_graph,
            None,
            &todo_ledger,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&human_checkpoint),
            &parent,
            &session_id,
            &workspace_id,
            None,
            None,
            &[tool_call],
            None,
            None,
        );

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, ExecutionResultStatus::Rejected);
        let payload: serde_json::Value =
            serde_json::from_str(&result[0].0).expect("agent_spawn rejection should be json");
        assert_eq!(payload["status"].as_str(), Some("rejected"));
        assert!(task_store.get_children(&parent.task_id).is_empty());
    }

    #[test]
    fn long_mission_agent_spawn_fails_without_human_checkpoint_store() {
        let event_bus = InMemoryEventBus::new(16);
        let task_store = TaskStore::new();
        let session_store = SessionStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let conversation_registry = ConversationRegistry::new();
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let todo_ledger = magi_todo_ledger::TodoLedger::new();
        let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();

        let session_id = SessionId::new("session-agent-spawn-missing-human-store");
        let workspace_id = Some(WorkspaceId::new(
            "workspace-agent-spawn-missing-human-store",
        ));
        let mut parent = coordinator_task(test_task(
            "task-parent-missing-human-store",
            "task-parent-missing-human-store",
            None,
        ));
        parent.policy_snapshot = Some(long_mission_policy());
        task_store.insert_task(parent.clone());

        let tool_call = ChatToolCall {
            id: "call-agent-spawn-missing-human-store".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::AgentSpawn.as_str().to_string(),
                arguments: serde_json::json!({
                    "role": "executor",
                    "goal": "缺少人审 store 时不应创建"
                })
                .to_string(),
            },
        };

        let result = execute_task_tool_call_batch(
            &event_bus,
            None,
            &agent_role_registry,
            None,
            &task_store,
            &session_store,
            &execution_registry,
            &conversation_registry,
            &spawn_graph,
            None,
            &todo_ledger,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &parent,
            &session_id,
            &workspace_id,
            None,
            None,
            &[tool_call],
            None,
            None,
        );

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, ExecutionResultStatus::Failed);
        let payload: serde_json::Value =
            serde_json::from_str(&result[0].0).expect("agent_spawn failure should be json");
        assert_eq!(payload["status"].as_str(), Some("failed"));
        assert!(task_store.get_children(&parent.task_id).is_empty());
    }

    #[test]
    fn agent_unavailable_failure_is_degradable() {
        assert!(agent_unavailable_failure(
            "LLM invocation failed (round 0): provider transport failed: timed out"
        ));
        assert!(agent_unavailable_failure(
            "模型配置不可用: model bridge client 未配置"
        ));
        assert!(!agent_unavailable_failure(
            "工具执行失败，任务不能标记完成：file_write: denied"
        ));
    }
}
