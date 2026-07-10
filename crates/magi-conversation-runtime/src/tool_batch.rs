//! 任务系统 — tool batch / coordinator / single 工具执行入口。
//!
//! - `execute_task_tool_call_batch`：按 concurrency 分组并发或串行调度本轮工具。
//! - `execute_task_tool_call`：单工具入口，按 BuiltinToolName 走 coordinator/写工具/policy/
//!   safety gate/tool registry 各分支。
//! - `execute_coordinator_tool`：协调器工具（agent_spawn / agent_wait）入口。
//! - `task_policy_tool_decision` / `safety_gate_tool_decision` 等支撑判定。

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
    AccessProfile, EventId, ExecutionResultStatus, GoalId, SessionId,
    TASK_RUNTIME_FAILURE_PUBLIC_OUTPUT, TaskExecutorBinding, TaskId, TaskKind, TaskPolicy,
    TaskStatus, TaskTier, ToolCallId, UtcMillis, WorkspaceId, public_task_output_refs,
    task_output_ref_is_internal_runtime_failure,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::GoalStatus;
use magi_session_store::{ExecutionThread, SessionStore};
use magi_snapshot::{SnapshotSession, ToolHook, ToolHookCtx};
use magi_tool_runtime::{
    BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
    builtin_permission_engine, canonical_builtin_tool_name, effective_tool_policy_allowed_paths,
    normalize_tool_policy_paths, tool_path_access_requests,
};

use crate::builtin_tool_schema::internal_builtin_tool_rejection_payload;
use crate::skill_apply_tool::{SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime};
use crate::task_execution_registry::SpawnedChildExecutionError;
use crate::{
    ConversationRegistry, MailboxAuthor, MailboxKind, RuntimeSignal,
    task_execution_registry::{SpawnedChildExecutionRequest, TaskExecutionRegistry},
    task_helpers::task_can_see_builtin_tool,
    tool_declared_paths::{append_result_declared_paths, derive_declared_paths},
    tool_result_utils::{
        safety_gate_public_error, tool_execution_failed_result, tool_execution_status_label,
    },
};
use crate::{
    active_skill_tool_execution_policy, execute_skill_custom_tool, parse_skill_custom_tool_name,
    tool_execution_policy_scope,
};

const MIN_CREATE_GOAL_TOKEN_BUDGET: u64 = 16_000;

/// agent_spawn 生成 child task_id 时使用的进程内单调序号。
///
/// 仅靠 `UtcMillis::now()` 在同一毫秒内的多次并行 agent_spawn 会产生重复
/// child_id，进而触发 SpawnGraph 的边冲突。配合毫秒时间戳一起拼到 task_id
/// 末尾，保证同一进程内绝对唯一。
static AGENT_SPAWN_SEQ: AtomicU64 = AtomicU64::new(0);
const AGENT_SPAWN_SUMMARY_MAX_CHARS: usize = 1200;
const AGENT_SPAWN_FINAL_TEXT_MAX_CHARS: usize = 6000;
const AGENT_SPAWN_PARENT_CONTEXT_MAX_CHARS: usize = 600;
const AGENT_SPAWN_INHERITED_INPUT_REF_MAX: usize = 16;
const AGENT_UNAVAILABLE_PUBLIC_TEXT: &str = "代理当前不可用，主线需要改派或接管。";
const AGENT_SPAWN_STARTED_INSTRUCTION: &str = "代理已异步启动。若后续结论依赖该代理结果，必须调用 agent_wait，并传入 task_ids=[child_task_id] 收集终态结果；不要在未等待必要代理结果时直接给最终答复。";
const AGENT_WAIT_DEFAULT_TIMEOUT_MS: u64 = 300_000;
const AGENT_WAIT_MIN_TIMEOUT_MS: u64 = 1_000;
const AGENT_WAIT_MAX_TIMEOUT_MS: u64 = 1_800_000;
const TOOL_VISIBILITY_REJECTED_PUBLIC_ERROR: &str = "该工具在当前任务角色或阶段下不可用";
const TOOL_POLICY_NEEDS_APPROVAL_PUBLIC_ERROR: &str =
    "受限访问已拦截该操作，请切换为完全访问权限后重试";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentSpawnAccessMode {
    ReadOnly,
    ReadWrite,
}

pub(crate) struct ToolPreflightDecision {
    pub(crate) payload: String,
    pub(crate) status: ExecutionResultStatus,
}

fn execute_task_tool_call_with_lifecycle<F>(
    event_bus: &InMemoryEventBus,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    worker_id: Option<&magi_core::WorkerId>,
    execution_group_id: Option<&str>,
    tool_call: &ChatToolCall,
    execute: F,
) -> (String, ExecutionResultStatus)
where
    F: FnOnce() -> (String, ExecutionResultStatus),
{
    let started_at = UtcMillis::now();
    publish_tool_lifecycle_event(
        event_bus,
        "tool.call.started",
        task,
        session_id,
        workspace_id,
        worker_id,
        execution_group_id,
        tool_call,
        serde_json::json!({
            "phase": "started",
            "arguments_preview": tool_arguments_preview(&tool_call.function.arguments),
        }),
    );

    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(execute)).unwrap_or_else(|_| {
            tracing::warn!(
                tool_name = %tool_call.function.name,
                tool_call_id = %tool_call.id,
                task_id = %task.task_id.as_str(),
                session_id = %session_id.as_str(),
                "task tool execution panicked"
            );
            tool_execution_failed_result(&tool_call.function.name)
        });

    let finished_at = UtcMillis::now();
    publish_tool_lifecycle_event(
        event_bus,
        "tool.call.finished",
        task,
        session_id,
        workspace_id,
        worker_id,
        execution_group_id,
        tool_call,
        serde_json::json!({
            "phase": "finished",
            "status": tool_execution_status_label(result.1),
            "duration_ms": finished_at.0.saturating_sub(started_at.0),
            "result_preview": tool_arguments_preview(&result.0),
        }),
    );

    result
}

fn publish_tool_lifecycle_event(
    event_bus: &InMemoryEventBus,
    event_type: &str,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    worker_id: Option<&magi_core::WorkerId>,
    execution_group_id: Option<&str>,
    tool_call: &ChatToolCall,
    payload: serde_json::Value,
) {
    let payload = serde_json::json!({
        "tool_call_id": tool_call.id.as_str(),
        "tool_name": tool_call.function.name.as_str(),
        "task_id": task.task_id.as_str(),
        "session_id": session_id.as_str(),
        "workspace_id": workspace_id.as_ref().map(ToString::to_string),
        "worker_id": worker_id.map(ToString::to_string),
        "execution_group_id": execution_group_id,
        "lifecycle": payload,
    });
    let event = EventEnvelope::domain(
        EventId::new(format!(
            "event-tool-lifecycle-{}-{}-{}",
            event_type,
            tool_call.id,
            UtcMillis::now().0
        )),
        event_type,
        payload,
    )
    .with_context(EventContext {
        workspace_id: workspace_id.clone(),
        session_id: Some(session_id.clone()),
        mission_id: Some(task.mission_id.clone()),
        task_id: Some(task.task_id.clone()),
        ..EventContext::default()
    });
    let _ = event_bus.publish(event);
}

fn tool_arguments_preview(value: &str) -> String {
    const MAX_CHARS: usize = 2_000;
    let mut preview = value.chars().take(MAX_CHARS).collect::<String>();
    if value.chars().count() > MAX_CHARS {
        preview.push_str("...");
    }
    preview
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
    skill_dispatch_runtime: Option<&magi_skill_runtime::SkillDispatchRuntime>,
    skill_name: Option<&str>,
    task_store: &TaskStore,
    session_store: &SessionStore,
    execution_registry: &TaskExecutionRegistry,
    conversation_registry: &ConversationRegistry,
    spawn_graph: &Mutex<magi_spawn_graph::SpawnGraph>,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    todo_ledger: &magi_todo_ledger::TodoLedger,
    project_memory: Option<&magi_project_memory::ProjectMemoryStore>,
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
                    let tool_call = &tool_calls[tool_index];
                    let result = execute_task_tool_call_with_lifecycle(
                        event_bus,
                        task,
                        session_id,
                        workspace_id,
                        worker_id,
                        execution_group_id.as_deref(),
                        tool_call,
                        || {
                            execute_task_tool_call(
                                event_bus,
                                tool_registry,
                                agent_role_registry,
                                skill_runtime,
                                skill_dispatch_runtime,
                                skill_name,
                                task_store,
                                session_store,
                                execution_registry,
                                conversation_registry,
                                spawn_graph,
                                safety_gate,
                                todo_ledger,
                                project_memory,
                                task,
                                session_id,
                                workspace_id,
                                workspace_root_path,
                                worker_id,
                                tool_call,
                            )
                        },
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
                            let execution_group_id_for_lifecycle = execution_group_id.clone();
                            (
                                tool_index,
                                scope.spawn(move || {
                                    if let Some(snapshot) = snapshot_session.as_deref() {
                                        snapshot.before_tool(&hook_ctx);
                                    }
                                    let result = execute_task_tool_call_with_lifecycle(
                                        event_bus,
                                        task,
                                        session_id,
                                        workspace_id,
                                        worker_id,
                                        execution_group_id_for_lifecycle.as_deref(),
                                        tool_call,
                                        || {
                                            execute_task_tool_call(
                                                event_bus,
                                                tool_registry,
                                                agent_role_registry,
                                                skill_runtime,
                                                skill_dispatch_runtime,
                                                skill_name,
                                                task_store,
                                                session_store,
                                                execution_registry,
                                                conversation_registry,
                                                spawn_graph,
                                                safety_gate,
                                                todo_ledger,
                                                project_memory,
                                                task,
                                                session_id,
                                                workspace_id,
                                                workspace_root_path,
                                                worker_id,
                                                tool_call,
                                            )
                                        },
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
                            tracing::warn!(
                                tool_name = %tool_calls[tool_index].function.name,
                                tool_call_id = %tool_calls[tool_index].id,
                                task_id = %task.task_id.as_str(),
                                session_id = %session_id.as_str(),
                                "task tool execution thread panicked"
                            );
                            tool_execution_failed_result(&tool_calls[tool_index].function.name)
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
                tool_execution_failed_result(&tool_calls[tool_index].function.name)
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
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    tool: magi_tool_runtime::BuiltinToolName,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    let parsed: serde_json::Value = match serde_json::from_str(&tool_call.function.arguments) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(error = %err, tool = tool.as_str(), "coordinator tool arguments parse failed");
            return (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "failed",
                    "error_code": "invalid_arguments",
                    "error": "协调器工具参数格式无效",
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
            let spawnable_role_ids = agent_role_registry.spawnable_agent_role_ids();
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
                        &spawnable_role_ids,
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
                let role_hint = spawnable_role_ids.join(" / ");
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "degraded",
                        "fallback_mode": "mainline_or_reassign",
                        "role": role,
                        "available_roles": spawnable_role_ids,
                        "error_code": "agent_role_not_spawnable",
                        "error": "该 role 不是可派发代理角色。coordinator 是主线编排身份，不能通过 agent_spawn 派发。",
                        "instruction": format!("请改派 {role_hint} 等可用专业代理；如果无需继续派发，则由主线基于已有上下文直接推进并给出结果。"),
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
            let child_dependency_ids = agent_spawn_child_dependency_ids(task);
            let child_input_refs = agent_spawn_child_input_refs(task);
            let child = magi_core::Task {
                task_id: child_id.clone(),
                mission_id: task.mission_id.clone(),
                root_task_id: task.root_task_id.clone(),
                parent_task_id: Some(task.task_id.clone()),
                kind: task_kind,
                title: display_name,
                goal: child_goal,
                status: TaskStatus::Pending,
                dependency_ids: child_dependency_ids,
                required_children: Vec::new(),
                policy_snapshot: Some(child_policy_snapshot),
                executor_binding: Some(
                    TaskExecutorBinding::for_role(&role).with_parallelism_group(
                        parsed
                            .get("parallelism_group")
                            .and_then(|value| value.as_str())
                            .map(str::to_string),
                    ),
                ),
                knowledge_refs: Vec::new(),
                workspace_scope: task.workspace_scope.clone(),
                write_scope: task.write_scope.clone(),
                input_refs: child_input_refs,
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
                    tracing::warn!(
                        error = %error,
                        parent_task_id = %task.task_id,
                        child_task_id = %child_id,
                        "agent_spawn child execution registration failed"
                    );
                    if let SpawnedChildExecutionError::CapacityExceeded { active, limit } = error {
                        return (
                            serde_json::json!({
                                "tool": tool.as_str(),
                                "status": "rejected",
                                "error_code": "agent_spawn_capacity_exceeded",
                                "active_branch_count": active,
                                "max_active_branch_count": limit,
                                "error": format!("当前会话已达到多代理并发上限：最多 {limit} 条活跃执行分支（含主线），当前 {active} 条"),
                                "instruction": "请先用 agent_wait 收集已启动代理结果，合并后再按下一批继续派发；不要在同一时刻创建超过上限的代理。",
                            })
                            .to_string(),
                            ExecutionResultStatus::Rejected,
                        );
                    }
                    return (
                        serde_json::json!({
                            "tool": tool.as_str(),
                            "status": "failed",
                            "error_code": "agent_spawn_registration_failed",
                            "error": "代理启动失败，请由主线继续或改派其他角色",
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
                    "instruction": AGENT_SPAWN_STARTED_INSTRUCTION,
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            )
        }
        magi_tool_runtime::BuiltinToolName::AgentWait => {
            let session_threads = session_store.thread_registry_snapshot(session_id);
            execute_agent_wait(
                task_store,
                spawn_graph,
                task,
                &session_threads,
                tool,
                &parsed,
            )
        }
        _ => unreachable!("execute_coordinator_tool 只接收协调器代理工具变体"),
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

pub(crate) fn execute_goal_tool(
    session_store: &SessionStore,
    session_id: &SessionId,
    thread_id: magi_core::ThreadId,
    tool: BuiltinToolName,
    arguments: &str,
) -> (String, ExecutionResultStatus) {
    let parsed = match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(value) => value,
        Err(error) => {
            return (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "failed",
                    "error": format!("目标工具参数不是有效 JSON: {error}"),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };

    match tool {
        BuiltinToolName::GetGoal => {
            let goal = session_store.current_unfinished_goal(session_id);
            (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "ok",
                    "goal": goal,
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            )
        }
        BuiltinToolName::CreateGoal => {
            let objective = parsed
                .get("objective")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .trim();
            let token_budget = parsed.get("token_budget").and_then(|value| value.as_u64());
            if token_budget == Some(0) {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": "token_budget 必须大于 0",
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            if token_budget.is_some_and(|budget| budget < MIN_CREATE_GOAL_TOKEN_BUDGET) {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": format!(
                            "token_budget 不能低于 {MIN_CREATE_GOAL_TOKEN_BUDGET}；若用户没有明确给出预算，请省略 token_budget"
                        ),
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            if token_budget.is_some_and(|budget| {
                !objective_text_explicitly_allows_goal_budget(objective, budget)
            }) {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": "token_budget 必须来自用户目标原文里的明确预算数值；若用户未明确给预算或要求不要设置预算，请省略 token_budget",
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            match session_store.create_goal(session_id.clone(), thread_id, objective, token_budget)
            {
                Ok(goal) => (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "created",
                        "goal": goal,
                    })
                    .to_string(),
                    ExecutionResultStatus::Succeeded,
                ),
                Err(error) => (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": error.to_string(),
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                ),
            }
        }
        BuiltinToolName::UpdateGoal => {
            let status = match parsed.get("status").and_then(|value| value.as_str()) {
                Some("complete") => GoalStatus::Complete,
                Some("blocked") => GoalStatus::Blocked,
                _ => {
                    return (
                        serde_json::json!({
                            "tool": tool.as_str(),
                            "status": "failed",
                            "error": "update_goal status 只能是 complete 或 blocked",
                        })
                        .to_string(),
                        ExecutionResultStatus::Failed,
                    );
                }
            };
            let goal_id = parsed
                .get("goal_id")
                .and_then(|value| value.as_str())
                .map(|value| GoalId::new(value.trim().to_string()))
                .or_else(|| {
                    session_store
                        .active_goal(session_id)
                        .map(|goal| goal.goal_id)
                });
            let Some(goal_id) = goal_id else {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": "当前会话没有 active goal",
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            };
            match session_store.update_goal_status(session_id, &goal_id, status) {
                Ok(goal) => (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "updated",
                        "goal": goal,
                    })
                    .to_string(),
                    ExecutionResultStatus::Succeeded,
                ),
                Err(error) => (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": error.to_string(),
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                ),
            }
        }
        _ => unreachable!("execute_goal_tool 只接收 goal 工具"),
    }
}

fn objective_text_explicitly_allows_goal_budget(objective: &str, token_budget: u64) -> bool {
    let normalized = objective.to_ascii_lowercase();
    if [
        "不要设置 token_budget",
        "不要设置预算",
        "不要给预算",
        "不要设预算",
        "no token_budget",
        "no token budget",
        "without token_budget",
        "without token budget",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
    {
        return false;
    }
    if ![
        "token_budget",
        "token budget",
        "token 预算",
        "tokens",
        "token",
        "预算",
        "budget",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
    {
        return false;
    }
    normalized.contains(&token_budget.to_string())
}

fn select_agent_spawn_parameter_contract(
    user_message: &str,
    requested_role: &str,
    requested_display_name: &str,
    requested_goal: &str,
    spawnable_role_ids: &[String],
) -> Option<AgentSpawnParameterContract> {
    parse_agent_spawn_parameter_contracts(user_message, spawnable_role_ids)
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

fn parse_agent_spawn_parameter_contracts(
    user_message: &str,
    spawnable_role_ids: &[String],
) -> Vec<AgentSpawnParameterContract> {
    user_message
        .match_indices("display_name")
        .filter_map(|(index, _)| {
            let display_name =
                extract_display_name_after(&user_message[index + "display_name".len()..])?;
            Some(AgentSpawnParameterContract {
                role: extract_contract_role(user_message, index, spawnable_role_ids),
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

fn extract_contract_role(
    user_message: &str,
    index: usize,
    spawnable_role_ids: &[String],
) -> Option<String> {
    let window = surrounding_text_window(user_message, index, 120, 0).to_ascii_lowercase();
    spawnable_role_ids
        .iter()
        .filter_map(|role| {
            let role_lower = role.to_ascii_lowercase();
            window.rfind(&role_lower).map(|pos| (pos, role.as_str()))
        })
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
        policy.access_profile = magi_core::AccessProfile::ReadOnly;
        policy.command_mode = "read_only".to_string();
    } else if policy.command_mode.trim().is_empty() {
        policy.command_mode = "full".to_string();
    }
    policy
}

fn agent_spawn_child_dependency_ids(parent: &magi_core::Task) -> Vec<TaskId> {
    parent.dependency_ids.clone()
}

fn agent_spawn_child_input_refs(parent: &magi_core::Task) -> Vec<String> {
    let mut refs = Vec::new();
    push_agent_spawn_input_ref(
        &mut refs,
        format!(
            "父任务事实：id={} title={} goal={}",
            parent.task_id,
            compact_agent_spawn_context_ref(&parent.title),
            compact_agent_spawn_context_ref(&parent.goal)
        ),
    );
    for input_ref in &parent.input_refs {
        push_agent_spawn_input_ref(&mut refs, input_ref.clone());
    }
    for evidence_ref in &parent.evidence_refs {
        push_agent_spawn_input_ref(&mut refs, format!("父任务证据：{evidence_ref}"));
    }
    refs
}

fn push_agent_spawn_input_ref(refs: &mut Vec<String>, value: String) {
    if refs.len() >= AGENT_SPAWN_INHERITED_INPUT_REF_MAX {
        return;
    }
    let compact = compact_agent_spawn_context_ref(&value);
    if compact.is_empty() || refs.iter().any(|existing| existing == &compact) {
        return;
    }
    refs.push(compact);
}

fn compact_agent_spawn_context_ref(value: &str) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= AGENT_SPAWN_PARENT_CONTEXT_MAX_CHARS {
        return compact;
    }
    compact
        .chars()
        .take(AGENT_SPAWN_PARENT_CONTEXT_MAX_CHARS)
        .collect::<String>()
}

fn default_agent_spawn_policy() -> TaskPolicy {
    TaskPolicy {
        autonomy_level: "Autonomous".to_string(),
        access_profile: magi_core::AccessProfile::Restricted,
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
    let access_mode = task_policy_access_mode(child.policy_snapshot.as_ref()).as_str();
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
                "dependency_ids": child.dependency_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "input_refs": &child.input_refs,
            }),
            enqueued_at: now,
        });
}

fn task_policy_access_mode(policy: Option<&TaskPolicy>) -> AgentSpawnAccessMode {
    if policy
        .map(|policy| policy.access_profile == AccessProfile::ReadOnly)
        .unwrap_or(false)
    {
        AgentSpawnAccessMode::ReadOnly
    } else {
        AgentSpawnAccessMode::ReadWrite
    }
}

fn execute_agent_wait(
    task_store: &TaskStore,
    spawn_graph: &Mutex<magi_spawn_graph::SpawnGraph>,
    parent_task: &magi_core::Task,
    session_threads: &[ExecutionThread],
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
    if let Some(task_id) = task_ids.iter().find(|task_id| {
        !agent_wait_task_is_direct_child(task_store, spawn_graph, parent_task, task_id)
    }) {
        return (
            serde_json::json!({
                "tool": tool.as_str(),
                "status": "rejected",
                "error_code": "agent_wait_scope_mismatch",
                "child_task_id": task_id.to_string(),
                "error": "agent_wait 只能等待当前任务派发的代理",
            })
            .to_string(),
            ExecutionResultStatus::Rejected,
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
                    "error_code": "agent_task_unavailable",
                    "error": "代理任务不可用",
                }));
                continue;
            };
            if matches!(child.status, TaskStatus::Pending | TaskStatus::Running) {
                pending_task_ids.push(task_id.to_string());
            }
            results.push(child_agent_terminal_payload(
                &child,
                agent_thread_for_task(session_threads, &child.task_id),
            ));
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

fn agent_wait_task_is_direct_child(
    task_store: &TaskStore,
    spawn_graph: &Mutex<magi_spawn_graph::SpawnGraph>,
    parent_task: &magi_core::Task,
    child_task_id: &TaskId,
) -> bool {
    let graph_match = spawn_graph
        .lock()
        .ok()
        .and_then(|graph| graph.parent_of(child_task_id).cloned())
        .as_ref()
        == Some(&parent_task.task_id);
    match task_store.get_task(child_task_id) {
        Some(child) => {
            let parent_match = child.parent_task_id.as_ref() == Some(&parent_task.task_id);
            (graph_match || parent_match)
                && agent_wait_child_execution_scope_matches(parent_task, &child)
        }
        None => graph_match,
    }
}

fn agent_wait_child_execution_scope_matches(
    parent_task: &magi_core::Task,
    child: &magi_core::Task,
) -> bool {
    child.mission_id == parent_task.mission_id
        && child.root_task_id == parent_task.root_task_id
        && child.workspace_scope == parent_task.workspace_scope
        && child.write_scope == parent_task.write_scope
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

fn agent_thread_for_task<'a>(
    session_threads: &'a [ExecutionThread],
    task_id: &TaskId,
) -> Option<&'a ExecutionThread> {
    session_threads.iter().find(|thread| {
        thread
            .handled_task_ids
            .iter()
            .any(|handled| handled == task_id)
    })
}

fn child_agent_terminal_payload(
    child: &magi_core::Task,
    thread: Option<&ExecutionThread>,
) -> serde_json::Value {
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
    let transcript_output = thread.and_then(child_agent_output_from_thread);
    match child.status {
        TaskStatus::Completed => {
            let output =
                transcript_output.unwrap_or_else(|| child_agent_output(&child.output_refs));
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
            let public_output_refs =
                public_task_output_refs(TaskStatus::Failed, &child.output_refs);
            let error = public_output_refs
                .first()
                .cloned()
                .unwrap_or_else(|| "代理任务执行失败".to_string());
            let unavailable = public_output_refs
                .iter()
                .any(|output| agent_unavailable_failure(output))
                || public_output_refs
                    .first()
                    .is_some_and(|output| output == TASK_RUNTIME_FAILURE_PUBLIC_OUTPUT);
            if unavailable {
                let mut payload = base("degraded", "failed");
                payload["fallback_mode"] =
                    serde_json::Value::String("mainline_or_reassign".to_string());
                payload["error_code"] = serde_json::Value::String("agent_unavailable".to_string());
                payload["instruction"] = serde_json::Value::String(
                    "代理当前不可用。请不要停止任务：优先改派其他可用角色继续；如果没有必要继续派发，则由主线根据已有上下文直接推进并给出最终结果。".to_string(),
                );
                payload["result"] = serde_json::json!({
                    "final_text": AGENT_UNAVAILABLE_PUBLIC_TEXT,
                    "truncated": false,
                    "output_ref_count": child.output_refs.len(),
                });
                payload["summary"] =
                    serde_json::Value::String(AGENT_UNAVAILABLE_PUBLIC_TEXT.to_string());
                payload["output_ref_count"] = serde_json::json!(child.output_refs.len());
                payload["error"] = serde_json::Value::String("代理当前不可用".to_string());
                return payload;
            }
            let output = child_agent_output(&public_output_refs);
            let mut payload = base("failed", "failed");
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
            payload["error_code"] = serde_json::Value::String("agent_killed".to_string());
            payload["error"] = serde_json::Value::String("代理任务被终止".to_string());
            payload
        }
        TaskStatus::Pending => base("pending", "pending"),
        TaskStatus::Running => base("running", "running"),
    }
}

fn child_agent_output_from_thread(thread: &ExecutionThread) -> Option<ChildAgentOutput> {
    thread
        .message_history
        .iter()
        .rev()
        .find(|message| message.role.trim().eq_ignore_ascii_case("assistant"))
        .and_then(|message| message.content.as_deref())
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .map(|content| {
            let (final_text, truncated) = truncate_for_agent_spawn_final_text(content);
            ChildAgentOutput {
                summary: truncate_for_agent_spawn_summary(content),
                final_text,
                truncated,
            }
        })
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
    if task_output_ref_is_internal_runtime_failure(error) {
        return true;
    }
    let normalized = error.trim().to_ascii_lowercase();
    ["模型配置不可用", "代理不可用", "没有匹配角色", "没有匹配"]
        .iter()
        .any(|needle| normalized.contains(&needle.to_ascii_lowercase()))
}

#[allow(clippy::too_many_arguments)]
fn execute_task_tool_call(
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    agent_role_registry: &magi_agent_role::AgentRoleRegistry,
    skill_runtime: Option<&magi_skill_runtime::SkillRuntime>,
    skill_dispatch_runtime: Option<&magi_skill_runtime::SkillDispatchRuntime>,
    skill_name: Option<&str>,
    task_store: &TaskStore,
    session_store: &SessionStore,
    execution_registry: &TaskExecutionRegistry,
    conversation_registry: &ConversationRegistry,
    spawn_graph: &Mutex<magi_spawn_graph::SpawnGraph>,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    todo_ledger: &magi_todo_ledger::TodoLedger,
    project_memory: Option<&magi_project_memory::ProjectMemoryStore>,
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
    if let Some(canonical) =
        magi_tool_runtime::BuiltinToolName::from_str(tool_call.function.name.as_str())
    {
        if !task_can_see_builtin_tool(Some(task), Some(agent_role_registry), canonical) {
            let decision = task_tool_visibility_decision_payload(canonical.as_str(), task);
            return (decision.payload, decision.status);
        }
    }

    if let Some(decision) = task_tool_preflight_decision(
        task,
        safety_gate,
        &tool_call.function.name,
        &tool_call.function.arguments,
        workspace_root_path,
    ) {
        return (decision.payload, decision.status);
    }

    if let Some(canonical) =
        magi_tool_runtime::BuiltinToolName::from_str(tool_call.function.name.as_str())
    {
        if matches!(
            canonical,
            magi_tool_runtime::BuiltinToolName::GetGoal
                | magi_tool_runtime::BuiltinToolName::CreateGoal
                | magi_tool_runtime::BuiltinToolName::UpdateGoal
        ) {
            return execute_goal_tool(
                session_store,
                session_id,
                session_store
                    .ensure_session_mission(session_id, UtcMillis::now(), || {
                        task.mission_id.clone()
                    })
                    .1,
                canonical,
                &tool_call.function.arguments,
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

    let access_profile = task
        .policy_snapshot
        .as_ref()
        .map(|policy| policy.access_profile)
        .unwrap_or_default();
    if let Some((tool_skill_name, binding_id)) =
        parse_skill_custom_tool_name(&tool_call.function.name)
    {
        return execute_skill_custom_tool(
            tool_call,
            &tool_skill_name,
            &binding_id,
            skill_name,
            task_tool_execution_policy_scope(task),
            safety_gate,
            skill_runtime,
            skill_dispatch_runtime,
            ToolExecutionContext {
                worker_id: worker_id.cloned(),
                task_id: Some(task.task_id.clone()),
                session_id: Some(session_id.clone()),
                workspace_id: workspace_id.clone(),
                access_profile,
                working_directory: workspace_root_path.cloned(),
            },
            workspace_root_path
                .as_ref()
                .map(|path| path.display().to_string()),
        );
    }

    if let Some(result) =
        registry.execute_external_mcp_tool(&tool_call.function.name, &tool_call.function.arguments)
    {
        if access_profile == AccessProfile::ReadOnly {
            return (
                serde_json::json!({
                    "tool": tool_call.function.name,
                    "status": "failed",
                    "error": "只读访问模式不允许调用 MCP 工具",
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
        return result;
    }

    if let Some(rejection) = internal_builtin_tool_rejection_payload(&tool_call.function.name) {
        return (rejection, ExecutionResultStatus::Failed);
    }

    let mut tool_policy =
        active_skill_tool_execution_policy(access_profile, skill_runtime, skill_name);
    apply_task_policy_scope(&mut tool_policy, task.policy_snapshot.as_ref());
    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new(&tool_call.id),
            &tool_call.function.name,
            tool_call.function.arguments.clone(),
        ),
        ToolExecutionContext {
            worker_id: worker_id.cloned(),
            task_id: Some(task.task_id.clone()),
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.clone(),
            access_profile: tool_policy.access_profile,
            working_directory: workspace_root_path.cloned(),
        },
        &tool_policy,
    );

    (output.payload, output.status)
}

fn task_tool_execution_policy_scope(task: &magi_core::Task) -> ToolExecutionPolicy {
    let Some(policy) = task.policy_snapshot.as_ref() else {
        return tool_execution_policy_scope(AccessProfile::default(), "", &[], &[]);
    };
    tool_execution_policy_scope(
        policy.access_profile,
        policy.command_mode.clone(),
        &policy.allowed_paths,
        &policy.denied_paths,
    )
}

fn apply_task_policy_scope(
    tool_policy: &mut ToolExecutionPolicy,
    policy_snapshot: Option<&TaskPolicy>,
) {
    if let Some(policy) = policy_snapshot {
        tool_policy.access_profile = policy.access_profile;
        tool_policy.allowed_paths = policy.allowed_paths.clone();
        tool_policy.denied_paths = policy.denied_paths.clone();
        tool_policy.command_mode = policy.command_mode.clone();
    }
}

#[cfg(test)]
fn task_policy_tool_decision(
    task: &magi_core::Task,
    requested_tool_name: &str,
    arguments: &str,
) -> Option<ToolPreflightDecision> {
    task_policy_tool_decision_with_workspace_root(task, requested_tool_name, arguments, None)
}

fn task_tool_preflight_decision(
    task: &magi_core::Task,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    requested_tool_name: &str,
    arguments: &str,
    workspace_root_path: Option<&PathBuf>,
) -> Option<ToolPreflightDecision> {
    let task_policy_decision = task_policy_tool_decision_with_workspace_root(
        task,
        requested_tool_name,
        arguments,
        workspace_root_path,
    );
    // S8：SafetyGate 语义判定。它和 TaskPolicy 都属于执行前判定：
    // HardBlock 必须压过常规风险拦截，TaskPolicy 的 Rejected 也不能被 SafetyGate
    // 的 NeedsApproval 降级。
    let access_profile = task
        .policy_snapshot
        .as_ref()
        .map(|policy| policy.access_profile)
        .unwrap_or_default();
    let safety_gate_decision = safety_gate.and_then(|gate| {
        safety_gate_tool_decision(gate, access_profile, requested_tool_name, arguments)
    });
    select_preflight_decision(task_policy_decision, safety_gate_decision)
}

fn task_policy_tool_decision_with_workspace_root(
    task: &magi_core::Task,
    requested_tool_name: &str,
    arguments: &str,
    workspace_root_path: Option<&PathBuf>,
) -> Option<ToolPreflightDecision> {
    let policy_snapshot = task.policy_snapshot.as_ref()?;
    let canonical_tool_name = canonical_builtin_tool_name(requested_tool_name)
        .unwrap_or_else(|| requested_tool_name.trim().to_string());
    // no_tools 是 PermissionEngine 三维之外的全局开关，本层先单独拦截。
    if policy_snapshot
        .command_mode
        .eq_ignore_ascii_case("no_tools")
    {
        return Some(task_policy_decision_payload(
            &canonical_tool_name,
            ExecutionResultStatus::Rejected,
            format!("当前任务阶段不允许调用工具: {canonical_tool_name}"),
            Some(policy_snapshot.access_profile),
        ));
    }

    access_profile_tool_decision(
        policy_snapshot.access_profile,
        &policy_snapshot.command_mode,
        &policy_snapshot.allowed_tools,
        &policy_snapshot.denied_tools,
        &policy_snapshot.allowed_paths,
        &policy_snapshot.denied_paths,
        requested_tool_name,
        arguments,
        workspace_root_path,
    )
}

pub(crate) fn access_profile_tool_decision(
    access_profile: magi_core::AccessProfile,
    command_mode: &str,
    allowed_tools: &[String],
    denied_tools: &[String],
    allowed_paths: &[String],
    denied_paths: &[String],
    requested_tool_name: &str,
    arguments: &str,
    workspace_root_path: Option<&PathBuf>,
) -> Option<ToolPreflightDecision> {
    let canonical_tool_name = canonical_builtin_tool_name(requested_tool_name)
        .unwrap_or_else(|| requested_tool_name.trim().to_string());
    // PermissionEngine 比对工具名是按字面比对，因此把 policy 中的别名先 canonical 化。
    let canonical_policy = magi_permissions::PermissionPolicy {
        allowed_tools: allowed_tools
            .iter()
            .map(|tool| {
                canonical_builtin_tool_name(tool).unwrap_or_else(|| tool.trim().to_string())
            })
            .collect(),
        denied_tools: denied_tools
            .iter()
            .map(|tool| {
                canonical_builtin_tool_name(tool).unwrap_or_else(|| tool.trim().to_string())
            })
            .collect(),
        allowed_paths: effective_tool_policy_allowed_paths(
            access_profile,
            allowed_paths,
            workspace_root_path.map(|path| path.as_path()),
        ),
        denied_paths: normalize_tool_policy_paths(
            denied_paths,
            workspace_root_path.map(|path| path.as_path()),
        ),
        command_mode: command_mode.to_string(),
        ..magi_permissions::PermissionPolicy::default()
    };
    let engine = builtin_permission_engine();
    let is_write_tool = BuiltinToolName::from_str(canonical_tool_name.as_str())
        .is_some_and(|tool| tool.is_write_operation());
    let mut pending_decision = None;

    let tool_request = magi_permissions::PermissionRequest::ToolInvocation {
        tool_name: canonical_tool_name.as_str(),
        is_write_tool,
    };
    if let Some(decision) = select_access_profile_axis_decision(
        &mut pending_decision,
        permission_decision_payload(
            &canonical_tool_name,
            engine.decide(&tool_request, &canonical_policy, access_profile),
            access_profile,
        ),
    ) {
        return Some(decision);
    }
    // shell_exec 在只读任务下需要 access_mode=read_only —— 走 ShellCommand 轴判定。
    if canonical_tool_name == BuiltinToolName::ShellExec.as_str() {
        let shell_request = magi_permissions::PermissionRequest::ShellCommand {
            arguments_json: arguments,
        };
        if let Some(decision) = select_access_profile_axis_decision(
            &mut pending_decision,
            permission_decision_payload(
                &canonical_tool_name,
                engine.decide(&shell_request, &canonical_policy, access_profile),
                access_profile,
            ),
        ) {
            return Some(decision);
        }
    }
    for path_request in tool_path_access_requests(
        &canonical_tool_name,
        arguments,
        workspace_root_path.map(|path| path.as_path()),
        access_profile,
    ) {
        let path_request = magi_permissions::PermissionRequest::PathAccess {
            absolute_path: path_request.absolute_path.as_path(),
            kind: path_request.kind,
        };
        if let Some(decision) = select_access_profile_axis_decision(
            &mut pending_decision,
            permission_decision_payload(
                &canonical_tool_name,
                engine.decide(&path_request, &canonical_policy, access_profile),
                access_profile,
            ),
        ) {
            return Some(decision);
        }
    }
    pending_decision
}

fn select_access_profile_axis_decision(
    pending_decision: &mut Option<ToolPreflightDecision>,
    decision: Option<ToolPreflightDecision>,
) -> Option<ToolPreflightDecision> {
    match decision {
        Some(decision) if decision.status == ExecutionResultStatus::Rejected => Some(decision),
        Some(decision) => {
            if pending_decision.is_none() {
                *pending_decision = Some(decision);
            }
            None
        }
        None => None,
    }
}

pub(crate) fn select_preflight_decision(
    task_policy_decision: Option<ToolPreflightDecision>,
    safety_gate_decision: Option<ToolPreflightDecision>,
) -> Option<ToolPreflightDecision> {
    match (task_policy_decision, safety_gate_decision) {
        (Some(policy), Some(safety)) => match (policy.status, safety.status) {
            (_, ExecutionResultStatus::Rejected) => Some(safety),
            (ExecutionResultStatus::Rejected, _) => Some(policy),
            (_, ExecutionResultStatus::NeedsApproval) => Some(safety),
            (ExecutionResultStatus::NeedsApproval, _) => Some(policy),
            _ => Some(policy),
        },
        (Some(policy), None) => Some(policy),
        (None, Some(safety)) => Some(safety),
        (None, None) => None,
    }
}

fn permission_decision_payload(
    tool_name: &str,
    decision: magi_permissions::Decision,
    access_profile: magi_core::AccessProfile,
) -> Option<ToolPreflightDecision> {
    match decision {
        magi_permissions::Decision::Allow => None,
        magi_permissions::Decision::Deny { reason } => Some(task_policy_decision_payload(
            tool_name,
            ExecutionResultStatus::Rejected,
            reason,
            Some(access_profile),
        )),
        magi_permissions::Decision::NeedsApproval { reason } => Some(task_policy_decision_payload(
            tool_name,
            ExecutionResultStatus::NeedsApproval,
            reason,
            Some(access_profile),
        )),
    }
}

fn task_policy_decision_payload(
    tool_name: &str,
    status: ExecutionResultStatus,
    reason: String,
    access_profile: Option<magi_core::AccessProfile>,
) -> ToolPreflightDecision {
    let (error_code, public_error) = match status {
        ExecutionResultStatus::NeedsApproval => (
            "tool_policy_needs_approval",
            TOOL_POLICY_NEEDS_APPROVAL_PUBLIC_ERROR,
        ),
        ExecutionResultStatus::Rejected => ("tool_policy_rejected", "该工具在当前访问模式下不可用"),
        _ => ("tool_policy_failed", "该工具暂不可用"),
    };
    tracing::warn!(
        tool_name,
        status = %tool_execution_status_label(status),
        access_profile = access_profile.map(|profile| profile.as_str()).unwrap_or_default(),
        reason = %reason,
        "tool preflight policy decision"
    );
    ToolPreflightDecision {
        payload: serde_json::json!({
            "tool": tool_name,
            "status": tool_execution_status_label(status),
            "error_code": error_code,
            "error": public_error,
            "access_profile": access_profile.map(|profile| profile.as_str()),
        })
        .to_string(),
        status,
    }
}

fn task_tool_visibility_decision_payload(
    tool_name: &str,
    task: &magi_core::Task,
) -> ToolPreflightDecision {
    let task_tier = task
        .policy_snapshot
        .as_ref()
        .map(|policy| format!("{:?}", policy.task_tier))
        .unwrap_or_else(|| "unknown".to_string());
    tracing::warn!(
        tool_name,
        task_id = %task.task_id,
        task_tier = %task_tier,
        "tool preflight visibility decision"
    );
    ToolPreflightDecision {
        payload: serde_json::json!({
            "tool": tool_name,
            "status": tool_execution_status_label(ExecutionResultStatus::Rejected),
            "error_code": "tool_policy_rejected",
            "error": TOOL_VISIBILITY_REJECTED_PUBLIC_ERROR,
        })
        .to_string(),
        status: ExecutionResultStatus::Rejected,
    }
}

/// S8：SafetyGate 的 HardBlock / RequireApprovalInRestricted 都是执行前判定。
/// HardBlock 代表禁止执行；RequireApprovalInRestricted 代表受限模式拦截、完全访问审计放行。
pub(crate) fn safety_gate_tool_decision(
    gate: &magi_safety_gate::SafetyGate,
    access_profile: magi_core::AccessProfile,
    tool_name: &str,
    arguments: &str,
) -> Option<ToolPreflightDecision> {
    let canonical_tool_name =
        canonical_builtin_tool_name(tool_name).unwrap_or_else(|| tool_name.trim().to_string());
    match gate.evaluate(&canonical_tool_name, arguments) {
        magi_safety_gate::SafetyDecision::Allow => None,
        magi_safety_gate::SafetyDecision::AuditOnly { .. } => None,
        magi_safety_gate::SafetyDecision::HardBlock {
            category,
            pattern,
            reason,
        } => Some(safety_gate_decision_payload(
            &canonical_tool_name,
            ExecutionResultStatus::Rejected,
            category,
            magi_safety_gate::SafetyAction::HardBlock,
            pattern,
            reason,
        )),
        magi_safety_gate::SafetyDecision::RequireApprovalInRestricted {
            category,
            pattern,
            reason,
        } => match access_profile {
            magi_core::AccessProfile::FullAccess => None,
            magi_core::AccessProfile::Restricted => Some(safety_gate_decision_payload(
                &canonical_tool_name,
                ExecutionResultStatus::NeedsApproval,
                category,
                magi_safety_gate::SafetyAction::RequireApprovalInRestricted,
                pattern,
                reason,
            )),
            magi_core::AccessProfile::ReadOnly => Some(safety_gate_decision_payload(
                &canonical_tool_name,
                ExecutionResultStatus::Rejected,
                category,
                magi_safety_gate::SafetyAction::RequireApprovalInRestricted,
                pattern,
                format!("{reason}；只读分析模式不支持升级访问模式执行"),
            )),
        },
    }
}

fn safety_gate_decision_payload(
    tool_name: &str,
    status: ExecutionResultStatus,
    category: magi_safety_gate::SafetyCategory,
    action: magi_safety_gate::SafetyAction,
    pattern: String,
    reason: String,
) -> ToolPreflightDecision {
    let public_error = safety_gate_public_error(status);
    tracing::warn!(
        tool_name,
        status = %tool_execution_status_label(status),
        category = category.as_str(),
        action = action.as_str(),
        pattern = %pattern,
        reason = %reason,
        "tool preflight safety gate decision"
    );
    ToolPreflightDecision {
        payload: serde_json::json!({
            "tool": tool_name,
            "status": tool_execution_status_label(status),
            "error_code": public_error.error_code,
            "error": public_error.error,
        })
        .to_string(),
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::ChatToolFunction;
    use magi_core::{MissionId, Task, TaskRuntimePayload};
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
        task.executor_binding = Some(TaskExecutorBinding::for_role("coordinator"));
        task
    }

    fn default_spawnable_role_ids() -> Vec<String> {
        magi_agent_role::AgentRoleRegistry::load_default().spawnable_agent_role_ids()
    }

    fn spawnable_role_ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    struct SnapshotReconcileProbeTool {
        name: &'static str,
        snapshot: Arc<SnapshotSession>,
    }

    struct PanicBuiltinTool {
        name: &'static str,
    }

    impl SnapshotReconcileProbeTool {
        fn new(name: &'static str, snapshot: Arc<SnapshotSession>) -> Self {
            Self { name, snapshot }
        }
    }

    impl magi_tool_runtime::BuiltinTool for SnapshotReconcileProbeTool {
        fn name(&self) -> &'static str {
            self.name
        }

        fn execute(
            &self,
            input: &str,
            context: &ToolExecutionContext,
            _resources: &magi_tool_runtime::ToolRuntimeResources,
        ) -> String {
            let arguments = serde_json::from_str::<serde_json::Value>(input).unwrap_or_default();
            let path = arguments
                .get("changed_paths")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| items.first())
                .and_then(serde_json::Value::as_str)
                .expect("probe changed path");
            let workspace_root = context
                .working_directory
                .as_ref()
                .expect("probe working directory");
            std::fs::write(workspace_root.join(path), format!("probe {path}"))
                .expect("probe file write");
            self.snapshot.reconcile().expect("probe reconcile");
            serde_json::json!({
                "tool": self.name,
                "status": "succeeded",
                "stdout": "snapshot reconciled"
            })
            .to_string()
        }

        fn spec(&self) -> magi_tool_runtime::BuiltinToolSpec {
            magi_tool_runtime::BuiltinToolSpec {
                name: self.name.to_string(),
                risk_level: magi_core::RiskLevel::Low,
                approval_requirement: magi_core::ApprovalRequirement::None,
            }
        }
    }

    impl magi_tool_runtime::BuiltinTool for PanicBuiltinTool {
        fn name(&self) -> &'static str {
            self.name
        }

        fn execute(
            &self,
            _input: &str,
            _context: &ToolExecutionContext,
            _resources: &magi_tool_runtime::ToolRuntimeResources,
        ) -> String {
            panic!("internal task tool panic detail must stay private")
        }

        fn spec(&self) -> magi_tool_runtime::BuiltinToolSpec {
            magi_tool_runtime::BuiltinToolSpec {
                name: self.name.to_string(),
                risk_level: magi_core::RiskLevel::Low,
                approval_requirement: magi_core::ApprovalRequirement::None,
            }
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

        assert_eq!(child.access_profile, magi_core::AccessProfile::ReadOnly);
        assert_eq!(child.command_mode, "read_only");
        assert_eq!(child.network_mode, parent.network_mode);
        assert_eq!(child.task_tier, parent.task_tier);

        let mut full_access_parent = default_agent_spawn_policy();
        full_access_parent.access_profile = magi_core::AccessProfile::FullAccess;
        let full_access_child = agent_spawn_child_policy_snapshot(
            Some(&full_access_parent),
            AgentSpawnAccessMode::ReadOnly,
        );
        assert_eq!(
            full_access_child.access_profile,
            magi_core::AccessProfile::ReadOnly
        );
        assert_eq!(full_access_child.command_mode, "read_only");
    }

    #[test]
    fn agent_spawn_child_policy_never_escalates_parent_read_only() {
        let mut parent = default_agent_spawn_policy();
        parent.command_mode = "read_only".to_string();

        let child =
            agent_spawn_child_policy_snapshot(Some(&parent), AgentSpawnAccessMode::ReadWrite);

        assert_eq!(child.access_profile, magi_core::AccessProfile::ReadOnly);
        assert_eq!(child.command_mode, "read_only");
    }

    #[test]
    fn agent_spawn_child_context_inherits_parent_task_facts() {
        let mut parent = test_task("task-agent-context-parent", "task-agent-context-root", None);
        parent.title = "修复会话同步".to_string();
        parent.goal = "必须检查 sessionId 和 workspaceId 是否匹配".to_string();
        parent.input_refs = vec![
            "用户要求：不要跨 workspace 读取 session".to_string(),
            "用户要求：不要跨 workspace 读取 session".to_string(),
        ];
        parent.evidence_refs = vec!["证据：bootstrap 只接受后端 session".to_string()];
        parent.dependency_ids = vec![TaskId::new("task-parent-dependency")];

        let input_refs = agent_spawn_child_input_refs(&parent);
        let dependency_ids = agent_spawn_child_dependency_ids(&parent);

        assert_eq!(dependency_ids, vec![TaskId::new("task-parent-dependency")]);
        assert!(
            input_refs.iter().any(|value| value.contains("父任务事实")
                && value.contains("task-agent-context-parent")
                && value.contains("sessionId")),
            "子代理 input_refs 必须包含父任务标题/目标事实，实际: {input_refs:?}"
        );
        assert_eq!(
            input_refs
                .iter()
                .filter(|value| value.contains("不要跨 workspace 读取 session"))
                .count(),
            1,
            "重复父 input_refs 只能继承一次"
        );
        assert!(
            input_refs
                .iter()
                .any(|value| value.contains("父任务证据：证据：bootstrap")),
            "父任务 evidence_refs 应作为子代理输入参考继承"
        );
    }

    #[test]
    fn agent_spawn_contract_parser_extracts_explicit_user_parameters() {
        let role_ids = default_spawnable_role_ids();
        let contracts = parse_agent_spawn_parameter_contracts(
            "必须同一轮并行启动 2 个代理：1) role=explorer，display_name=「目录探查代理」，access_mode=read_only，目标：只读查看 /Users/xie/code/TEST 顶层目录；2) role=reviewer，display_name=「配置审查代理」，access_mode=read_only，目标：只读查看 README.md 和 package.json 是否存在。",
            &role_ids,
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
        let role_ids = default_spawnable_role_ids();
        let contracts = parse_agent_spawn_parameter_contracts(
            "第一轮同时 agent_spawn 两个只读代理：explorer display_name「冻结目录代理」只做根目录巡检；reviewer display_name「冻结配置代理」只读取 package.json 指出一个风险。",
            &role_ids,
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
        let role_ids = default_spawnable_role_ids();

        let directory = select_agent_spawn_parameter_contract(
            message,
            "explorer",
            "目录探查员",
            "只读检查当前工作区目录 /Users/xie/code/TEST",
            &role_ids,
        )
        .expect("directory contract should be selected");
        assert_eq!(directory.role.as_deref(), Some("explorer"));
        assert_eq!(directory.display_name, "目录探查代理");

        let config = select_agent_spawn_parameter_contract(
            message,
            "reviewer",
            "文件存在审查员",
            "只读查看 README.md 和 package.json 是否存在",
            &role_ids,
        )
        .expect("config contract should be selected");
        assert_eq!(config.role.as_deref(), Some("reviewer"));
        assert_eq!(config.display_name, "配置审查代理");
    }

    #[test]
    fn agent_spawn_contract_selection_prefers_goal_when_model_rewrites_role() {
        let message = "必须同一轮并行启动 2 个代理：1) role=explorer，display_name=「目录探查代理」，access_mode=read_only，目标：只读查看 /Users/xie/code/TEST 顶层目录；2) role=reviewer，display_name=「配置审查代理」，access_mode=read_only，目标：只读查看 README.md 和 package.json 是否存在。";
        let role_ids = default_spawnable_role_ids();

        let config = select_agent_spawn_parameter_contract(
            message,
            "explorer",
            "关键文件检查员",
            "只读验证 /Users/xie/code/TEST/README.md 与 /Users/xie/code/TEST/package.json 是否存在且为普通文件",
            &role_ids,
        )
        .expect("config contract should be selected by goal overlap");

        assert_eq!(config.role.as_deref(), Some("reviewer"));
        assert_eq!(config.display_name, "配置审查代理");
    }

    #[test]
    fn agent_spawn_contract_parser_uses_registry_role_ids() {
        let role_ids = spawnable_role_ids(&["auditor", "executor"]);
        let contracts = parse_agent_spawn_parameter_contracts(
            "请启动 role=auditor，display_name=「安全审计代理」，access_mode=read_only，目标：检查鉴权风险。",
            &role_ids,
        );

        assert_eq!(contracts.len(), 1);
        assert_eq!(contracts[0].role.as_deref(), Some("auditor"));
        assert_eq!(contracts[0].display_name, "安全审计代理");
    }

    #[test]
    fn read_only_agent_policy_rejects_write_tool() {
        let mut task = test_task("task-read-only-agent", "task-read-only-agent", None);
        task.policy_snapshot = Some(agent_spawn_child_policy_snapshot(
            Some(&default_agent_spawn_policy()),
            AgentSpawnAccessMode::ReadOnly,
        ));

        let decision = task_policy_tool_decision(
            &task,
            BuiltinToolName::FileWrite.as_str(),
            r#"{"path":"probe.txt","content":""}"#,
        )
        .expect("read-only agent should reject file_write");
        let payload: serde_json::Value =
            serde_json::from_str(&decision.payload).expect("rejection should be json");

        assert_eq!(decision.status, ExecutionResultStatus::Rejected);
        assert_eq!(payload["status"].as_str(), Some("rejected"));
        assert_eq!(payload["tool"].as_str(), Some("file_write"));
        assert_eq!(payload["error_code"].as_str(), Some("tool_policy_rejected"));
        assert_eq!(
            payload["error"].as_str(),
            Some("该工具在当前访问模式下不可用")
        );
    }

    #[test]
    fn read_only_agent_policy_rejects_state_write_tools() {
        let mut task = test_task(
            "task-read-only-state-tools",
            "task-read-only-state-tools",
            None,
        );
        task.policy_snapshot = Some(agent_spawn_child_policy_snapshot(
            Some(&default_agent_spawn_policy()),
            AgentSpawnAccessMode::ReadOnly,
        ));

        for tool in [
            BuiltinToolName::AgentSpawn,
            BuiltinToolName::CreateGoal,
            BuiltinToolName::UpdateGoal,
            BuiltinToolName::TodoWrite,
            BuiltinToolName::MemoryWrite,
        ] {
            let decision = task_policy_tool_decision(&task, tool.as_str(), "{}")
                .unwrap_or_else(|| panic!("read-only agent should reject {}", tool.as_str()));
            let payload: serde_json::Value =
                serde_json::from_str(&decision.payload).expect("rejection should be json");

            assert_eq!(decision.status, ExecutionResultStatus::Rejected);
            assert_eq!(payload["status"].as_str(), Some("rejected"));
            assert_eq!(payload["tool"].as_str(), Some(tool.as_str()));
            assert_eq!(
                payload["error_code"].as_str(),
                Some("tool_policy_rejected"),
                "{} should be rejected as a write tool, got {}",
                tool.as_str(),
                decision.payload
            );
            assert_eq!(
                payload["error"].as_str(),
                Some("该工具在当前访问模式下不可用")
            );
        }
    }

    #[test]
    fn no_tools_policy_rejects_state_write_tool() {
        let mut task = test_task("task-no-tools-state-tool", "task-no-tools-state-tool", None);
        let mut policy = default_agent_spawn_policy();
        policy.command_mode = "no_tools".to_string();
        task.policy_snapshot = Some(policy);

        let decision = task_policy_tool_decision(&task, BuiltinToolName::AgentSpawn.as_str(), "{}")
            .expect("no_tools should reject agent_spawn");
        let payload: serde_json::Value =
            serde_json::from_str(&decision.payload).expect("rejection should be json");

        assert_eq!(decision.status, ExecutionResultStatus::Rejected);
        assert_eq!(
            payload["tool"].as_str(),
            Some(BuiltinToolName::AgentSpawn.as_str())
        );
        assert_eq!(payload["error_code"].as_str(), Some("tool_policy_rejected"));
        assert_eq!(
            payload["error"].as_str(),
            Some("该工具在当前访问模式下不可用")
        );
        assert!(
            !decision.payload.contains("当前任务阶段不允许调用工具"),
            "公开 payload 不应泄漏内部任务策略原因: {}",
            decision.payload
        );
    }

    #[test]
    fn task_runtime_preflight_blocks_read_only_state_tool_before_special_execution() {
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
        let task_store = TaskStore::new();
        let session_store = SessionStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let conversation_registry = ConversationRegistry::new();
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let todo_ledger = crate::test_todo_ledger("test-todo-ledger");
        let session_id = SessionId::new("session-read-only-state-tool");
        let workspace_id = Some(WorkspaceId::new("workspace-read-only-state-tool"));
        let mut task = coordinator_task(test_task(
            "task-read-only-state-tool",
            "task-read-only-state-tool",
            None,
        ));
        let mut policy = default_agent_spawn_policy();
        policy.access_profile = magi_core::AccessProfile::ReadOnly;
        task.policy_snapshot = Some(policy);
        let tool_call = ChatToolCall {
            id: "tool-call-read-only-todo-write".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::TodoWrite.as_str().to_string(),
                arguments: serde_json::json!({
                    "todos": [
                        {
                            "content": "不应写入",
                            "status": "pending"
                        }
                    ]
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            None,
            &agent_role_registry,
            None,
            None,
            None,
            &task_store,
            &session_store,
            &execution_registry,
            &conversation_registry,
            &spawn_graph,
            None,
            &todo_ledger,
            None,
            &task,
            &session_id,
            &workspace_id,
            None,
            None,
            &tool_call,
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&payload).expect("preflight rejection should be json");

        assert_eq!(status, ExecutionResultStatus::Rejected);
        assert_eq!(
            parsed["tool"].as_str(),
            Some(BuiltinToolName::TodoWrite.as_str())
        );
        assert_eq!(parsed["error_code"].as_str(), Some("tool_policy_rejected"));
        assert_eq!(
            parsed["error"].as_str(),
            Some("该工具在当前访问模式下不可用")
        );
        assert!(
            todo_ledger.is_empty(),
            "只读 preflight 必须发生在 todo_write 特殊执行前"
        );
    }

    #[test]
    fn goal_tools_use_session_goal_store_as_single_state_source() {
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
        let task_store = TaskStore::new();
        let session_store = SessionStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let conversation_registry = ConversationRegistry::new();
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let todo_ledger = crate::test_todo_ledger("test-todo-ledger");
        let session_id = SessionId::new("session-goal-tool-state");
        let workspace_id = Some(WorkspaceId::new("workspace-goal-tool-state"));
        session_store
            .create_session(session_id.clone(), "goal tool state")
            .expect("session should exist for goal tools");
        let task = coordinator_task(test_task(
            "task-goal-tool-state",
            "task-goal-tool-state",
            None,
        ));

        let call = |name: BuiltinToolName, arguments: serde_json::Value| {
            let tool_call = ChatToolCall {
                id: format!("tool-call-{}", name.as_str()),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: name.as_str().to_string(),
                    arguments: arguments.to_string(),
                },
            };
            execute_task_tool_call(
                &event_bus,
                None,
                &agent_role_registry,
                None,
                None,
                None,
                &task_store,
                &session_store,
                &execution_registry,
                &conversation_registry,
                &spawn_graph,
                None,
                &todo_ledger,
                None,
                &task,
                &session_id,
                &workspace_id,
                None,
                None,
                &tool_call,
            )
        };

        let (small_budget_payload, small_budget_status) = call(
            BuiltinToolName::CreateGoal,
            serde_json::json!({
                "objective": "完成 goal 模式升级",
                "token_budget": 1000,
            }),
        );
        assert_eq!(small_budget_status, ExecutionResultStatus::Failed);
        let small_budget: serde_json::Value =
            serde_json::from_str(&small_budget_payload).expect("failure payload should be json");
        assert!(
            small_budget["error"]
                .as_str()
                .unwrap_or_default()
                .contains("token_budget 不能低于")
        );

        let (created_payload, created_status) = call(
            BuiltinToolName::CreateGoal,
            serde_json::json!({
                "objective": "完成 goal 模式升级",
            }),
        );
        assert_eq!(created_status, ExecutionResultStatus::Succeeded);
        let created: serde_json::Value =
            serde_json::from_str(&created_payload).expect("create_goal payload should be json");
        let goal_id = created["goal"]["goalId"]
            .as_str()
            .expect("created goal should expose goal id")
            .to_string();
        assert_eq!(created["goal"]["status"].as_str(), Some("active"));
        assert!(created["goal"].get("tokenBudget").is_none());

        let (current_payload, current_status) =
            call(BuiltinToolName::GetGoal, serde_json::json!({}));
        assert_eq!(current_status, ExecutionResultStatus::Succeeded);
        let current: serde_json::Value =
            serde_json::from_str(&current_payload).expect("get_goal payload should be json");
        assert_eq!(current["goal"]["goalId"].as_str(), Some(goal_id.as_str()));
        assert_eq!(
            session_store
                .current_goal(&session_id)
                .expect("store should hold current goal")
                .objective,
            "完成 goal 模式升级"
        );

        let (updated_payload, updated_status) = call(
            BuiltinToolName::UpdateGoal,
            serde_json::json!({
                "status": "complete",
            }),
        );
        assert_eq!(updated_status, ExecutionResultStatus::Succeeded);
        let updated: serde_json::Value =
            serde_json::from_str(&updated_payload).expect("update_goal payload should be json");
        assert_eq!(updated["goal"]["goalId"].as_str(), Some(goal_id.as_str()));
        assert_eq!(updated["goal"]["status"].as_str(), Some("complete"));
        assert_eq!(
            session_store
                .current_goal(&session_id)
                .expect("store should hold updated goal")
                .status,
            GoalStatus::Complete
        );
    }

    #[test]
    fn goal_token_budget_must_be_explicit_in_objective_text() {
        assert!(!objective_text_explicitly_allows_goal_budget(
            "目标模式抽屉验收：不要设置 token_budget。只回复 DONE。",
            16_000,
        ));
        assert!(!objective_text_explicitly_allows_goal_budget(
            "完成一个没有预算要求的目标模式任务",
            16_000,
        ));
        assert!(objective_text_explicitly_allows_goal_budget(
            "以 token 预算 16000 完成目标模式任务",
            16_000,
        ));
    }

    #[test]
    fn read_only_agent_policy_rejects_shell_with_write_redirection() {
        let mut task = test_task(
            "task-read-only-shell-redirection",
            "task-read-only-shell-redirection",
            None,
        );
        task.policy_snapshot = Some(agent_spawn_child_policy_snapshot(
            Some(&default_agent_spawn_policy()),
            AgentSpawnAccessMode::ReadOnly,
        ));

        let decision = task_policy_tool_decision(
            &task,
            BuiltinToolName::ShellExec.as_str(),
            r#"{"command":"printf hidden > out.txt","access_mode":"read_only"}"#,
        )
        .expect("read-only agent should reject write-like shell even when declared read_only");
        let payload: serde_json::Value =
            serde_json::from_str(&decision.payload).expect("rejection should be json");

        assert_eq!(decision.status, ExecutionResultStatus::Rejected);
        assert_eq!(payload["status"].as_str(), Some("rejected"));
        assert_eq!(payload["tool"].as_str(), Some("shell_exec"));
        assert_eq!(payload["error_code"].as_str(), Some("tool_policy_rejected"));
        assert_eq!(
            payload["error"].as_str(),
            Some("该工具在当前访问模式下不可用")
        );
    }

    #[test]
    fn full_access_policy_allows_read_only_shell_dev_null_probe() {
        let mut task = test_task(
            "task-full-access-shell-dev-null",
            "task-full-access-shell-dev-null",
            None,
        );
        let mut policy = default_agent_spawn_policy();
        policy.access_profile = magi_core::AccessProfile::FullAccess;
        task.policy_snapshot = Some(policy);

        let decision = task_policy_tool_decision(
            &task,
            BuiltinToolName::ShellExec.as_str(),
            r#"{"command":"if command -v rg >/dev/null 2>&1; then rg --files; fi","access_mode":"read_only"}"#,
        );

        assert!(
            decision.is_none(),
            "full_access 下只读探测丢弃输出到 /dev/null 不应被 preflight 拒绝"
        );
    }

    #[test]
    fn restricted_policy_marks_write_shell_as_needs_approval() {
        let mut task = test_task(
            "task-human-approval-shell",
            "task-human-approval-shell",
            None,
        );
        task.policy_snapshot = Some(default_agent_spawn_policy());

        let decision = task_policy_tool_decision(
            &task,
            BuiltinToolName::ShellExec.as_str(),
            r#"{"command":"cargo test"}"#,
        )
        .expect("human checkpoint policy should require approval for write-like shell");
        let payload: serde_json::Value =
            serde_json::from_str(&decision.payload).expect("decision should be json");

        assert_eq!(decision.status, ExecutionResultStatus::NeedsApproval);
        assert_eq!(payload["status"].as_str(), Some("needs_approval"));
        assert_eq!(
            payload["error_code"].as_str(),
            Some("tool_policy_needs_approval")
        );
        assert_eq!(
            payload["error"].as_str(),
            Some("受限访问已拦截该操作，请切换为完全访问权限后重试")
        );
        assert_eq!(payload["access_profile"].as_str(), Some("restricted"));
    }

    #[test]
    fn full_access_policy_keeps_write_shell_autonomous() {
        let mut task = test_task("task-full-access-shell", "task-full-access-shell", None);
        task.policy_snapshot = Some(default_agent_spawn_policy());
        task.policy_snapshot
            .as_mut()
            .expect("policy")
            .access_profile = magi_core::AccessProfile::FullAccess;

        assert!(
            task_policy_tool_decision(
                &task,
                BuiltinToolName::ShellExec.as_str(),
                r#"{"command":"cargo test"}"#,
            )
            .is_none()
        );
    }

    #[test]
    fn restricted_default_scope_rejects_paths_outside_workspace() {
        let workspace = tempdir().expect("workspace tempdir");
        let workspace_root = workspace.path().to_path_buf();
        let outside_path = workspace_root
            .parent()
            .expect("workspace should have parent")
            .join("magi-outside-write-target.txt");
        let mut task = test_task(
            "task-restricted-default-path-scope",
            "task-restricted-default-path-scope",
            None,
        );
        task.policy_snapshot = Some(default_agent_spawn_policy());

        assert!(
            task_policy_tool_decision_with_workspace_root(
                &task,
                BuiltinToolName::FileWrite.as_str(),
                r#"{"path":"src/lib.rs","content":"ok"}"#,
                Some(&workspace_root),
            )
            .is_none()
        );

        let decision = task_policy_tool_decision_with_workspace_root(
            &task,
            BuiltinToolName::FileWrite.as_str(),
            &serde_json::json!({
                "path": outside_path.display().to_string(),
                "content": "outside"
            })
            .to_string(),
            Some(&workspace_root),
        )
        .expect("restricted default scope should reject outside workspace path");
        let payload: serde_json::Value =
            serde_json::from_str(&decision.payload).expect("decision should be json");

        assert_eq!(decision.status, ExecutionResultStatus::Rejected);
        assert_eq!(payload["error_code"].as_str(), Some("tool_policy_rejected"));
        assert_eq!(
            payload["error"].as_str(),
            Some("该工具在当前访问模式下不可用")
        );
        assert!(
            !decision
                .payload
                .contains(outside_path.to_string_lossy().as_ref())
        );
    }

    #[cfg(unix)]
    #[test]
    fn restricted_shell_workspace_fallback_uses_canonical_root_for_symlink_workspace() {
        let workspace = tempdir().expect("workspace tempdir");
        let real_root = workspace.path().join("real-root");
        let link_root = workspace.path().join("link-root");
        std::fs::create_dir_all(&real_root).expect("real workspace root should be creatable");
        std::os::unix::fs::symlink(&real_root, &link_root)
            .expect("workspace symlink should be creatable");

        let decision = access_profile_tool_decision(
            magi_core::AccessProfile::Restricted,
            "",
            &[],
            &[],
            &[],
            &[],
            BuiltinToolName::ShellExec.as_str(),
            r#"{"command":"printf restricted > out.txt"}"#,
            Some(&link_root),
        )
        .expect("restricted write shell inside symlinked workspace should require approval");

        assert_eq!(decision.status, ExecutionResultStatus::NeedsApproval);
    }

    #[test]
    fn restricted_default_scope_checks_filesystem_alias_paths() {
        let workspace = tempdir().expect("workspace tempdir");
        let workspace_root = workspace.path().to_path_buf();
        let outside_path = workspace_root
            .parent()
            .expect("workspace should have parent")
            .join("magi-outside-alias-target.txt");
        let mut task = test_task(
            "task-restricted-default-alias-path-scope",
            "task-restricted-default-alias-path-scope",
            None,
        );
        task.policy_snapshot = Some(default_agent_spawn_policy());

        for (tool, arguments) in [
            (
                BuiltinToolName::FileCopy,
                serde_json::json!({
                    "source": "src/input.txt",
                    "destination": outside_path.display().to_string()
                }),
            ),
            (
                BuiltinToolName::FileMove,
                serde_json::json!({
                    "source": outside_path.display().to_string(),
                    "destination": "src/output.txt"
                }),
            ),
            (
                BuiltinToolName::FileMkdir,
                serde_json::json!({
                    "path": outside_path.display().to_string()
                }),
            ),
        ] {
            let decision = task_policy_tool_decision_with_workspace_root(
                &task,
                tool.as_str(),
                &arguments.to_string(),
                Some(&workspace_root),
            )
            .expect("restricted default scope should reject alias path outside workspace");
            let payload: serde_json::Value =
                serde_json::from_str(&decision.payload).expect("decision should be json");

            assert_eq!(decision.status, ExecutionResultStatus::Rejected);
            assert_eq!(
                payload["error_code"].as_str(),
                Some("tool_policy_rejected"),
                "unexpected payload: {payload}"
            );
            assert_eq!(
                payload["error"].as_str(),
                Some("该工具在当前访问模式下不可用")
            );
            assert!(
                !decision
                    .payload
                    .contains(outside_path.to_string_lossy().as_ref())
            );
        }
    }

    #[test]
    fn restricted_default_scope_checks_read_alias_and_raw_paths() {
        let workspace = tempdir().expect("workspace tempdir");
        let workspace_root = workspace.path().to_path_buf();
        let outside_path = workspace_root
            .parent()
            .expect("workspace should have parent")
            .join("magi-outside-read-target.png");
        let mut task = test_task(
            "task-restricted-default-read-path-scope",
            "task-restricted-default-read-path-scope",
            None,
        );
        task.policy_snapshot = Some(default_agent_spawn_policy());

        for (tool, arguments) in [
            (
                BuiltinToolName::ViewImage,
                serde_json::json!({
                    "path": outside_path.display().to_string()
                })
                .to_string(),
            ),
            (
                BuiltinToolName::FileRead,
                serde_json::json!({
                    "path": outside_path.display().to_string()
                })
                .to_string(),
            ),
        ] {
            let decision = task_policy_tool_decision_with_workspace_root(
                &task,
                tool.as_str(),
                &arguments,
                Some(&workspace_root),
            )
            .expect("restricted default scope should reject outside read path");
            let payload: serde_json::Value =
                serde_json::from_str(&decision.payload).expect("decision should be json");

            assert_eq!(decision.status, ExecutionResultStatus::Rejected);
            assert_eq!(
                payload["error_code"].as_str(),
                Some("tool_policy_rejected"),
                "unexpected payload: {payload}"
            );
            assert_eq!(
                payload["error"].as_str(),
                Some("该工具在当前访问模式下不可用")
            );
            assert!(
                !decision
                    .payload
                    .contains(outside_path.to_string_lossy().as_ref())
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn restricted_default_scope_rejects_symlink_escape_paths() {
        let workspace = tempdir().expect("workspace tempdir");
        let outside = tempdir().expect("outside tempdir");
        let workspace_root = workspace.path().to_path_buf();
        let outside_secret = outside.path().join("secret.txt");
        std::fs::write(&outside_secret, "secret").expect("write outside secret");
        std::os::unix::fs::symlink(&outside_secret, workspace_root.join("linked-secret.txt"))
            .expect("create file symlink");
        std::os::unix::fs::symlink(outside.path(), workspace_root.join("linked-dir"))
            .expect("create dir symlink");

        let mut task = test_task(
            "task-restricted-default-symlink-path-scope",
            "task-restricted-default-symlink-path-scope",
            None,
        );
        task.policy_snapshot = Some(default_agent_spawn_policy());

        for (tool, arguments) in [
            (
                BuiltinToolName::FileRead,
                serde_json::json!({ "path": "linked-secret.txt" }),
            ),
            (
                BuiltinToolName::FileWrite,
                serde_json::json!({
                    "path": "linked-dir/new-file.txt",
                    "content": "outside"
                }),
            ),
        ] {
            let decision = task_policy_tool_decision_with_workspace_root(
                &task,
                tool.as_str(),
                &arguments.to_string(),
                Some(&workspace_root),
            )
            .expect("restricted default scope should reject symlink escape path");
            let payload: serde_json::Value =
                serde_json::from_str(&decision.payload).expect("decision should be json");

            assert_eq!(decision.status, ExecutionResultStatus::Rejected);
            assert_eq!(
                payload["error_code"].as_str(),
                Some("tool_policy_rejected"),
                "unexpected payload: {payload}"
            );
            assert_eq!(
                payload["error"].as_str(),
                Some("该工具在当前访问模式下不可用")
            );
        }
    }

    #[test]
    fn full_access_default_scope_allows_paths_outside_workspace() {
        let workspace = tempdir().expect("workspace tempdir");
        let workspace_root = workspace.path().to_path_buf();
        let outside_path = workspace_root
            .parent()
            .expect("workspace should have parent")
            .join("magi-full-access-outside-target.txt");
        let mut task = test_task(
            "task-full-access-default-path-scope",
            "task-full-access-default-path-scope",
            None,
        );
        let mut policy = default_agent_spawn_policy();
        policy.access_profile = magi_core::AccessProfile::FullAccess;
        task.policy_snapshot = Some(policy);

        assert!(
            task_policy_tool_decision_with_workspace_root(
                &task,
                BuiltinToolName::FileWrite.as_str(),
                &serde_json::json!({
                    "path": outside_path.display().to_string(),
                    "content": "outside"
                })
                .to_string(),
                Some(&workspace_root),
            )
            .is_none()
        );
    }

    #[test]
    fn task_policy_allowed_paths_are_resolved_against_workspace_root() {
        let workspace = tempdir().expect("workspace tempdir");
        let workspace_root = workspace.path().to_path_buf();
        let mut task = test_task("task-allowed-paths", "task-allowed-paths", None);
        let mut policy = default_agent_spawn_policy();
        policy.allowed_paths = vec!["src".to_string()];
        task.policy_snapshot = Some(policy);

        assert!(
            task_policy_tool_decision_with_workspace_root(
                &task,
                BuiltinToolName::FileRead.as_str(),
                r#"{"path":"src/lib.rs"}"#,
                Some(&workspace_root),
            )
            .is_none()
        );

        let decision = task_policy_tool_decision_with_workspace_root(
            &task,
            BuiltinToolName::FileRead.as_str(),
            r#"{"path":"README.md"}"#,
            Some(&workspace_root),
        )
        .expect("path outside allow list should be rejected");
        let payload: serde_json::Value =
            serde_json::from_str(&decision.payload).expect("decision should be json");

        assert_eq!(decision.status, ExecutionResultStatus::Rejected);
        assert_eq!(payload["error_code"].as_str(), Some("tool_policy_rejected"));
        assert_eq!(
            payload["error"].as_str(),
            Some("该工具在当前访问模式下不可用")
        );
    }

    #[test]
    fn denied_path_overrides_restricted_shell_approval() {
        let workspace = tempdir().expect("workspace tempdir");
        let workspace_root = workspace.path().to_path_buf();
        let mut task = test_task("task-denied-shell-path", "task-denied-shell-path", None);
        let mut policy = default_agent_spawn_policy();
        policy.denied_paths = vec!["private".to_string()];
        task.policy_snapshot = Some(policy);

        let decision = task_policy_tool_decision_with_workspace_root(
            &task,
            BuiltinToolName::ShellExec.as_str(),
            r#"{"command":"cat secret.txt > out.txt","cwd":"private"}"#,
            Some(&workspace_root),
        )
        .expect("denied path should reject before approval");
        let payload: serde_json::Value =
            serde_json::from_str(&decision.payload).expect("decision should be json");

        assert_eq!(decision.status, ExecutionResultStatus::Rejected);
        assert_eq!(payload["error_code"].as_str(), Some("tool_policy_rejected"));
        assert_eq!(
            payload["error"].as_str(),
            Some("该工具在当前访问模式下不可用")
        );
    }

    #[test]
    fn safety_gate_custom_rule_keeps_require_approval_status() {
        let gate = magi_safety_gate::SafetyGate::new(vec![magi_safety_gate::SafetyRule::new(
            "deploy-prod",
            magi_safety_gate::SafetyCategory::Custom,
        )]);

        let decision = safety_gate_tool_decision(
            &gate,
            magi_core::AccessProfile::Restricted,
            BuiltinToolName::ShellExec.as_str(),
            r#"{"command":"deploy-prod"}"#,
        )
        .expect("custom rule should require approval");
        let payload: serde_json::Value =
            serde_json::from_str(&decision.payload).expect("decision should be json");

        assert_eq!(decision.status, ExecutionResultStatus::NeedsApproval);
        assert_eq!(payload["status"].as_str(), Some("needs_approval"));
        assert_eq!(
            payload["error_code"].as_str(),
            Some("tool_safety_needs_approval")
        );
        assert_eq!(
            payload["error"].as_str(),
            Some("安全防护已在受限访问下拦截该操作，请切换为完全访问权限后重试")
        );
        assert!(payload.get("safety_gate").is_none());
        assert!(!decision.payload.contains("deploy-prod"));
        assert!(!decision.payload.contains("custom"));
    }

    #[test]
    fn safety_gate_restricted_approval_is_skipped_in_full_access() {
        let gate = magi_safety_gate::SafetyGate::new(vec![magi_safety_gate::SafetyRule::new(
            "deploy-prod",
            magi_safety_gate::SafetyCategory::Custom,
        )]);

        assert!(
            safety_gate_tool_decision(
                &gate,
                magi_core::AccessProfile::FullAccess,
                BuiltinToolName::ShellExec.as_str(),
                r#"{"command":"deploy-prod"}"#,
            )
            .is_none()
        );
    }

    #[test]
    fn safety_gate_hard_block_is_not_skipped_in_full_access() {
        let gate =
            magi_safety_gate::SafetyGate::new(vec![magi_safety_gate::SafetyRule::with_action(
                "destroy-everything",
                magi_safety_gate::SafetyCategory::Custom,
                magi_safety_gate::SafetyAction::HardBlock,
            )]);

        let decision = safety_gate_tool_decision(
            &gate,
            magi_core::AccessProfile::FullAccess,
            BuiltinToolName::ShellExec.as_str(),
            r#"{"command":"destroy-everything"}"#,
        )
        .expect("hard block must reject even in full access");

        assert_eq!(decision.status, ExecutionResultStatus::Rejected);
        let payload: serde_json::Value =
            serde_json::from_str(&decision.payload).expect("decision should be json");
        assert_eq!(payload["error_code"].as_str(), Some("tool_safety_rejected"));
        assert_eq!(payload["error"].as_str(), Some("该操作已被安全防护阻止"));
        assert!(payload.get("safety_gate").is_none());
        assert!(!decision.payload.contains("destroy-everything"));
    }

    #[test]
    fn preflight_selects_safety_hard_block_over_restricted_shell_approval() {
        let mut task = test_task("task-hard-block-shell", "task-hard-block-shell", None);
        task.policy_snapshot = Some(default_agent_spawn_policy());
        let gate =
            magi_safety_gate::SafetyGate::new(vec![magi_safety_gate::SafetyRule::with_action(
                "destroy-everything",
                magi_safety_gate::SafetyCategory::Custom,
                magi_safety_gate::SafetyAction::HardBlock,
            )]);

        let policy_decision = task_policy_tool_decision(
            &task,
            BuiltinToolName::ShellExec.as_str(),
            r#"{"command":"destroy-everything"}"#,
        )
        .expect("restricted shell should require approval before safety merge");
        let safety_decision = safety_gate_tool_decision(
            &gate,
            magi_core::AccessProfile::Restricted,
            BuiltinToolName::ShellExec.as_str(),
            r#"{"command":"destroy-everything"}"#,
        )
        .expect("hard block should reject");

        let selected = select_preflight_decision(Some(policy_decision), Some(safety_decision))
            .expect("preflight should select a decision");
        let payload: serde_json::Value =
            serde_json::from_str(&selected.payload).expect("decision should be json");

        assert_eq!(selected.status, ExecutionResultStatus::Rejected);
        assert_eq!(payload["error_code"].as_str(), Some("tool_safety_rejected"));
        assert_eq!(payload["error"].as_str(), Some("该操作已被安全防护阻止"));
        assert!(payload.get("safety_gate").is_none());
        assert!(!selected.payload.contains("destroy-everything"));
        assert!(!selected.payload.contains("hard_block"));
    }

    #[test]
    fn preflight_keeps_task_rejection_over_safety_approval() {
        let mut task = test_task("task-no-tools", "task-no-tools", None);
        let mut policy = default_agent_spawn_policy();
        policy.command_mode = "no_tools".to_string();
        task.policy_snapshot = Some(policy);
        let gate = magi_safety_gate::SafetyGate::new(vec![magi_safety_gate::SafetyRule::new(
            "deploy-prod",
            magi_safety_gate::SafetyCategory::Custom,
        )]);

        let policy_decision = task_policy_tool_decision(
            &task,
            BuiltinToolName::ShellExec.as_str(),
            r#"{"command":"deploy-prod"}"#,
        )
        .expect("no_tools should reject tool calls");
        let safety_decision = safety_gate_tool_decision(
            &gate,
            magi_core::AccessProfile::Restricted,
            BuiltinToolName::ShellExec.as_str(),
            r#"{"command":"deploy-prod"}"#,
        )
        .expect("safety should require approval");

        let selected = select_preflight_decision(Some(policy_decision), Some(safety_decision))
            .expect("preflight should select a decision");
        let payload: serde_json::Value =
            serde_json::from_str(&selected.payload).expect("decision should be json");

        assert_eq!(selected.status, ExecutionResultStatus::Rejected);
        assert_eq!(payload["error_code"].as_str(), Some("tool_policy_rejected"));
        assert_eq!(
            payload["error"].as_str(),
            Some("该工具在当前访问模式下不可用")
        );
        assert!(
            !selected.payload.contains("当前任务阶段不允许调用工具"),
            "公开 payload 不应泄漏内部任务策略原因: {}",
            selected.payload
        );
    }

    #[test]
    fn task_tool_call_requires_approval_for_file_remove_in_restricted_profile() {
        let event_bus = InMemoryEventBus::new(16);
        let task_store = TaskStore::new();
        let session_store = SessionStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let conversation_registry = ConversationRegistry::new();
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let todo_ledger = crate::test_todo_ledger("test-todo-ledger");
        let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
        let mut tool_registry = ToolRegistry::new(
            Arc::new(magi_governance::GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_default_builtins();
        let dir = tempdir().expect("temp dir");
        let target = dir.path().join("probe.txt");
        std::fs::write(&target, "probe").expect("write probe");
        let task = test_task("task-file-remove", "task-file-remove", None);
        let session_id = SessionId::new("session-file-remove");
        let workspace_id = Some(WorkspaceId::new("workspace-file-remove"));
        let tool_call = ChatToolCall {
            id: "call-file-remove".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::FileRemove.as_str().to_string(),
                arguments: serde_json::json!({
                    "path": target.to_string_lossy()
                })
                .to_string(),
            },
        };

        let result = execute_task_tool_call_batch(
            &event_bus,
            Some(&tool_registry),
            &agent_role_registry,
            None,
            None,
            None,
            &task_store,
            &session_store,
            &execution_registry,
            &conversation_registry,
            &spawn_graph,
            None,
            &todo_ledger,
            None,
            &task,
            &session_id,
            &workspace_id,
            Some(&dir.path().to_path_buf()),
            None,
            &[tool_call],
            None,
            None,
        );

        assert_eq!(result[0].1, ExecutionResultStatus::NeedsApproval);
        let payload: serde_json::Value =
            serde_json::from_str(&result[0].0).expect("policy payload should be json");
        assert_eq!(payload["tool"].as_str(), Some("file_remove"));
        assert_eq!(payload["status"].as_str(), Some("needs_approval"));
        assert_eq!(
            payload["error_code"].as_str(),
            Some("tool_policy_needs_approval")
        );
        assert!(target.exists(), "受限访问拦截的删除不能提前执行");
    }

    #[test]
    fn task_serial_tool_panic_returns_terminal_public_failure() {
        let event_bus = InMemoryEventBus::new(16);
        let task_store = TaskStore::new();
        let session_store = SessionStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let conversation_registry = ConversationRegistry::new();
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let todo_ledger = crate::test_todo_ledger("test-todo-ledger");
        let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
        let mut tool_registry = ToolRegistry::new(
            Arc::new(magi_governance::GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_builtin(Arc::new(PanicBuiltinTool {
            name: "unstable_tool",
        }));

        let task = test_task("task-panic-tool", "task-panic-tool", None);
        let session_id = SessionId::new("session-panic-tool");
        let workspace_id = Some(WorkspaceId::new("workspace-panic-tool"));
        let tool_call = ChatToolCall {
            id: "call-panic-tool".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "unstable_tool".to_string(),
                arguments: "{}".to_string(),
            },
        };

        let result = execute_task_tool_call_batch(
            &event_bus,
            Some(&tool_registry),
            &agent_role_registry,
            None,
            None,
            None,
            &task_store,
            &session_store,
            &execution_registry,
            &conversation_registry,
            &spawn_graph,
            None,
            &todo_ledger,
            None,
            &task,
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
            serde_json::from_str(&result[0].0).expect("panic result should be json");
        assert_eq!(payload["status"], "failed");
        assert_eq!(payload["error_code"], "tool_execution_failed");
        assert_eq!(payload["error"], "工具执行失败，请稍后重试");
        assert!(!result[0].0.contains("panic"));
        assert!(!result[0].0.contains("线程"));
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
        let todo_ledger = crate::test_todo_ledger("test-todo-ledger");
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
            None,
            None,
            &task_store,
            &session_store,
            &execution_registry,
            &conversation_registry,
            &spawn_graph,
            None,
            &todo_ledger,
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
            None,
            None,
            &task_store,
            &session_store,
            &execution_registry,
            &conversation_registry,
            &spawn_graph,
            None,
            &todo_ledger,
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

    #[tokio::test(flavor = "multi_thread")]
    async fn task_concurrent_tool_batch_keeps_snapshot_context_during_execution() {
        let dir = tempdir().expect("temp dir");
        let workspace_root = dir.path().to_path_buf();
        let snapshot = magi_snapshot::SnapshotManager::new()
            .start_session(
                "session-task-concurrent-snapshot".to_string(),
                workspace_root.clone(),
            )
            .await
            .expect("snapshot session should start");

        let event_bus = InMemoryEventBus::new(16);
        let task_store = TaskStore::new();
        let session_store = SessionStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let conversation_registry = ConversationRegistry::new();
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let todo_ledger = crate::test_todo_ledger("test-todo-ledger");
        let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
        let mut tool_registry = ToolRegistry::new(
            Arc::new(magi_governance::GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_builtin(Arc::new(SnapshotReconcileProbeTool::new(
            BuiltinToolName::ShellExec.as_str(),
            snapshot.clone(),
        )));

        let mut task = test_task("task-concurrent-snapshot", "task-concurrent-snapshot", None);
        task.policy_snapshot = Some(default_agent_spawn_policy());
        task.policy_snapshot
            .as_mut()
            .expect("policy")
            .access_profile = magi_core::AccessProfile::FullAccess;
        let session_id = SessionId::new("session-task-concurrent-snapshot");
        let workspace_id = Some(WorkspaceId::new("workspace-task-concurrent-snapshot"));
        let worker_id = magi_core::WorkerId::new("worker-concurrent-snapshot");
        let tool_calls = vec![
            ChatToolCall {
                id: "call-concurrent-snapshot-a".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: BuiltinToolName::ShellExec.as_str().to_string(),
                    arguments: serde_json::json!({
                        "command": "printf a",
                        "access_mode": "read_only",
                        "changed_paths": ["agent-a.txt"]
                    })
                    .to_string(),
                },
            },
            ChatToolCall {
                id: "call-concurrent-snapshot-b".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: BuiltinToolName::ShellExec.as_str().to_string(),
                    arguments: serde_json::json!({
                        "command": "printf b",
                        "access_mode": "read_only",
                        "changed_paths": ["agent-b.txt"]
                    })
                    .to_string(),
                },
            },
        ];

        let result = execute_task_tool_call_batch(
            &event_bus,
            Some(&tool_registry),
            &agent_role_registry,
            None,
            None,
            None,
            &task_store,
            &session_store,
            &execution_registry,
            &conversation_registry,
            &spawn_graph,
            None,
            &todo_ledger,
            None,
            &task,
            &session_id,
            &workspace_id,
            Some(&workspace_root),
            Some(&worker_id),
            &tool_calls,
            Some(snapshot.clone()),
            Some(task.mission_id.to_string()),
        );

        assert_eq!(result[0].1, ExecutionResultStatus::Succeeded);
        assert_eq!(result[1].1, ExecutionResultStatus::Succeeded);
        let pending = snapshot.pending_changes().expect("pending changes");
        for (path, call_id) in [
            ("agent-a.txt", "call-concurrent-snapshot-a"),
            ("agent-b.txt", "call-concurrent-snapshot-b"),
        ] {
            let change = pending
                .iter()
                .find(|change| change.path == path)
                .expect("concurrent tool change should be tracked");
            assert_eq!(change.source, magi_snapshot::SourceKind::Tool);
            assert_eq!(change.tool_call_id.as_deref(), Some(call_id));
            assert_eq!(
                change.worker_id.as_deref(),
                Some("worker-concurrent-snapshot")
            );
            assert_eq!(
                change.execution_group_id.as_deref(),
                Some("mission-mailbox")
            );
        }
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
        let parent = test_task("task-agent-wait-root", "task-agent-wait-root", None);
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let mut child = test_task(
            "task-agent-wait-child",
            "task-agent-wait-root",
            Some(TaskId::new("task-agent-wait-root")),
        );
        child.status = TaskStatus::Completed;
        child.title = "目录探索".to_string();
        child.goal = "列出目录并汇报".to_string();
        child.executor_binding = Some(TaskExecutorBinding::for_role("explorer"));
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
            &spawn_graph,
            &parent,
            &[],
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
    fn agent_wait_prefers_thread_transcript_over_task_output_refs() {
        let task_store = TaskStore::new();
        let parent = test_task("task-agent-wait-root", "task-agent-wait-root", None);
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let mut child = test_task(
            "task-agent-wait-thread-child",
            "task-agent-wait-root",
            Some(TaskId::new("task-agent-wait-root")),
        );
        child.status = TaskStatus::Completed;
        child.title = "代码审查".to_string();
        child.goal = "审查实现并汇报".to_string();
        child.executor_binding = Some(TaskExecutorBinding::for_role("reviewer"));
        child.output_refs = vec!["旧 Task output，不应覆盖 thread transcript。".to_string()];
        let child_id = child.task_id.clone();
        task_store.insert_task(child);
        let session_threads = vec![ExecutionThread {
            thread_id: magi_core::ThreadId::new("thread-agent-wait-reviewer"),
            session_id: SessionId::new("session-agent-wait-thread"),
            mission_id: parent.mission_id.clone(),
            role_id: "reviewer".to_string(),
            worker_instance_id: magi_core::WorkerId::new("worker-agent-wait-reviewer"),
            status: magi_session_store::ExecutionThreadStatus::Idle,
            created_at: UtcMillis(1),
            last_used_at: UtcMillis(2),
            handled_task_ids: vec![child_id],
            message_history: vec![magi_session_store::ThreadChatMessage {
                role: "assistant".to_string(),
                content: Some("代理完成：agent_wait 读取 thread transcript。".to_string()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            }],
        }];

        let (payload, status) = execute_agent_wait(
            &task_store,
            &spawn_graph,
            &parent,
            &session_threads,
            BuiltinToolName::AgentWait,
            &serde_json::json!({
                "task_ids": ["task-agent-wait-thread-child"],
                "timeout_ms": 1000,
            }),
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value =
            serde_json::from_str(&payload).expect("agent_wait result should be json");
        assert_eq!(
            parsed["results"][0]["result"]["final_text"].as_str(),
            Some("代理完成：agent_wait 读取 thread transcript。")
        );
        assert!(
            !payload.contains("旧 Task output"),
            "agent_wait 必须以 thread transcript 作为子代理结果权威"
        );
    }

    #[test]
    fn agent_spawn_instruction_points_to_agent_wait_task_ids_contract() {
        assert!(
            AGENT_SPAWN_STARTED_INSTRUCTION.contains("agent_wait")
                && AGENT_SPAWN_STARTED_INSTRUCTION.contains("task_ids=[child_task_id]"),
            "agent_spawn 成功回执必须指向 agent_wait 的唯一 task_ids 参数契约"
        );
        assert!(
            !AGENT_SPAWN_STARTED_INSTRUCTION.contains("传入 child_task_id 收集"),
            "agent_spawn 回执不能把返回字段误写成 agent_wait 输入字段"
        );
    }

    #[test]
    fn agent_wait_rejects_task_outside_current_spawn_scope() {
        let task_store = TaskStore::new();
        let parent = test_task("task-agent-wait-root", "task-agent-wait-root", None);
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let mut foreign_child = test_task(
            "task-agent-wait-foreign-child",
            "task-other-root",
            Some(TaskId::new("task-other-root")),
        );
        foreign_child.status = TaskStatus::Completed;
        foreign_child.output_refs = vec!["foreign result".to_string()];
        task_store.insert_task(foreign_child);

        let (payload, status) = execute_agent_wait(
            &task_store,
            &spawn_graph,
            &parent,
            &[],
            BuiltinToolName::AgentWait,
            &serde_json::json!({
                "task_ids": ["task-agent-wait-foreign-child"],
                "timeout_ms": 1000,
            }),
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        let parsed: serde_json::Value =
            serde_json::from_str(&payload).expect("agent_wait rejection should be json");
        assert_eq!(parsed["status"].as_str(), Some("rejected"));
        assert_eq!(
            parsed["error_code"].as_str(),
            Some("agent_wait_scope_mismatch")
        );
        assert!(!payload.contains("foreign result"));
    }

    #[test]
    fn agent_wait_rejects_same_parent_id_outside_execution_scope() {
        let task_store = TaskStore::new();
        let parent = test_task("task-agent-wait-root", "task-agent-wait-root", None);
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let mut foreign_child = test_task(
            "task-agent-wait-same-parent-foreign-scope",
            "task-other-root",
            Some(parent.task_id.clone()),
        );
        foreign_child.mission_id = MissionId::new("mission-other");
        foreign_child.status = TaskStatus::Completed;
        foreign_child.output_refs = vec!["foreign scoped result".to_string()];
        task_store.insert_task(foreign_child);

        let (payload, status) = execute_agent_wait(
            &task_store,
            &spawn_graph,
            &parent,
            &[],
            BuiltinToolName::AgentWait,
            &serde_json::json!({
                "task_ids": ["task-agent-wait-same-parent-foreign-scope"],
                "timeout_ms": 1000,
            }),
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        let parsed: serde_json::Value =
            serde_json::from_str(&payload).expect("agent_wait rejection should be json");
        assert_eq!(
            parsed["error_code"].as_str(),
            Some("agent_wait_scope_mismatch")
        );
        assert!(!payload.contains("foreign scoped result"));
    }

    #[test]
    fn agent_wait_rejects_spawn_graph_edge_with_different_workspace_scope() {
        let task_store = TaskStore::new();
        let mut parent = test_task("task-agent-wait-root", "task-agent-wait-root", None);
        parent.workspace_scope = Some("/workspace-a".to_string());
        parent.write_scope = Some("/workspace-a/src".to_string());
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let mut child = test_task(
            "task-agent-wait-workspace-mismatch",
            "task-agent-wait-root",
            Some(parent.task_id.clone()),
        );
        child.workspace_scope = Some("/workspace-b".to_string());
        child.write_scope = Some("/workspace-b/src".to_string());
        child.status = TaskStatus::Completed;
        child.output_refs = vec!["workspace mismatched result".to_string()];
        spawn_graph
            .lock()
            .expect("spawn graph lock should be available")
            .add_edge(
                parent.task_id.clone(),
                child.task_id.clone(),
                child.kind,
                std::time::SystemTime::now(),
            )
            .expect("test spawn graph edge should be accepted");
        task_store.insert_task(child);

        let (payload, status) = execute_agent_wait(
            &task_store,
            &spawn_graph,
            &parent,
            &[],
            BuiltinToolName::AgentWait,
            &serde_json::json!({
                "task_ids": ["task-agent-wait-workspace-mismatch"],
                "timeout_ms": 1000,
            }),
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        let parsed: serde_json::Value =
            serde_json::from_str(&payload).expect("agent_wait rejection should be json");
        assert_eq!(
            parsed["error_code"].as_str(),
            Some("agent_wait_scope_mismatch")
        );
        assert!(!payload.contains("workspace mismatched result"));
    }

    #[test]
    fn agent_wait_marks_unavailable_agent_as_degradable() {
        let task_store = TaskStore::new();
        let parent = test_task("task-agent-wait-root", "task-agent-wait-root", None);
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let mut child = test_task(
            "task-agent-wait-unavailable",
            "task-agent-wait-root",
            Some(TaskId::new("task-agent-wait-root")),
        );
        child.status = TaskStatus::Failed;
        child.title = "配置审查代理".to_string();
        child.goal = "检查模型配置是否可用".to_string();
        child.executor_binding = Some(TaskExecutorBinding::for_role("reviewer"));
        child.output_refs = vec!["provider transport failed: connection refused".to_string()];
        task_store.insert_task(child);

        let (payload, status) = execute_agent_wait(
            &task_store,
            &spawn_graph,
            &parent,
            &[],
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
        assert_eq!(result["error_code"].as_str(), Some("agent_unavailable"));
        assert_eq!(result["error"].as_str(), Some("代理当前不可用"));
        assert_eq!(
            result["result"]["final_text"].as_str(),
            Some(AGENT_UNAVAILABLE_PUBLIC_TEXT)
        );
        assert!(
            !result.to_string().contains("provider transport failed"),
            "agent_wait degraded payload should not expose provider transport detail"
        );
    }

    #[test]
    fn agent_wait_preserves_non_degradable_agent_failure() {
        let task_store = TaskStore::new();
        let parent = test_task("task-agent-wait-root", "task-agent-wait-root", None);
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let mut child = test_task(
            "task-agent-wait-real-failure",
            "task-agent-wait-root",
            Some(TaskId::new("task-agent-wait-root")),
        );
        child.status = TaskStatus::Failed;
        child.title = "冒烟测试代理".to_string();
        child.goal = "运行冒烟测试并报告失败原因".to_string();
        child.executor_binding = Some(TaskExecutorBinding::for_role("tester"));
        child.output_refs = vec!["测试失败：断言不匹配".to_string()];
        task_store.insert_task(child);

        let (payload, status) = execute_agent_wait(
            &task_store,
            &spawn_graph,
            &parent,
            &[],
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
    fn agent_wait_redacts_internal_failure_details_from_failed_agent_output() {
        let task_store = TaskStore::new();
        let parent = test_task("task-agent-wait-root", "task-agent-wait-root", None);
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let mut child = test_task(
            "task-agent-wait-redacted-failure",
            "task-agent-wait-root",
            Some(TaskId::new("task-agent-wait-root")),
        );
        child.status = TaskStatus::Failed;
        child.title = "验证代理".to_string();
        child.goal = "运行验证并报告失败原因".to_string();
        child.executor_binding = Some(TaskExecutorBinding::for_role("tester"));
        child.output_refs = vec![
            "测试失败：断言不匹配".to_string(),
            "provider transport failed: connection refused".to_string(),
        ];
        task_store.insert_task(child);

        let (payload, status) = execute_agent_wait(
            &task_store,
            &spawn_graph,
            &parent,
            &[],
            BuiltinToolName::AgentWait,
            &serde_json::json!({
                "task_ids": ["task-agent-wait-redacted-failure"],
                "timeout_ms": 1000,
            }),
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value =
            serde_json::from_str(&payload).expect("agent_wait result should be json");
        let result = &parsed["results"][0];
        assert_eq!(result["status"].as_str(), Some("failed"));
        assert_eq!(result["error"].as_str(), Some("测试失败：断言不匹配"));
        assert_eq!(
            result["result"]["final_text"].as_str(),
            Some("测试失败：断言不匹配")
        );
        assert!(
            !result.to_string().contains("provider transport failed"),
            "agent_wait failed payload should not expose internal provider detail"
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
        child.dependency_ids = vec![TaskId::new("task-agent-assignment-dependency")];
        child.input_refs = vec!["父任务要求：只读检查目录".to_string()];

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
                assert_eq!(
                    signal.payload["dependency_ids"],
                    serde_json::json!(["task-agent-assignment-dependency"])
                );
                assert_eq!(
                    signal.payload["input_refs"],
                    serde_json::json!(["父任务要求：只读检查目录"])
                );
            }
            other => panic!("expected runtime assignment message, got {other:?}"),
        }
    }

    #[test]
    fn tool_execution_rejects_role_invisible_builtin_tools() {
        let event_bus = InMemoryEventBus::new(16);
        let task_store = TaskStore::new();
        let session_store = SessionStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let conversation_registry = ConversationRegistry::new();
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let todo_ledger = crate::test_todo_ledger("test-todo-ledger");
        let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
        let session_id = SessionId::new("session-tool-scope");
        let workspace_id = Some(WorkspaceId::new("workspace-tool-scope"));

        let worker = test_task("task-worker", "task-worker", None);
        let worker_result = execute_task_tool_call_batch(
            &event_bus,
            None,
            &agent_role_registry,
            None,
            None,
            None,
            &task_store,
            &session_store,
            &execution_registry,
            &conversation_registry,
            &spawn_graph,
            None,
            &todo_ledger,
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
        let worker_payload: serde_json::Value =
            serde_json::from_str(&worker_result[0].0).expect("worker rejection should be json");
        assert_eq!(worker_payload["status"].as_str(), Some("rejected"));
        assert_eq!(
            worker_payload["error_code"].as_str(),
            Some("tool_policy_rejected")
        );
        assert_eq!(
            worker_payload["error"].as_str(),
            Some(TOOL_VISIBILITY_REJECTED_PUBLIC_ERROR)
        );
        assert!(!worker_result[0].0.contains("tool is not visible"));
    }

    #[test]
    fn agent_unavailable_failure_is_degradable() {
        assert!(agent_unavailable_failure(
            "LLM invocation failed (round 0): provider transport failed: timed out"
        ));
        assert!(agent_unavailable_failure(
            "dispatch spawn_blocking panicked: runtime worker crashed"
        ));
        assert!(agent_unavailable_failure(
            "模型配置不可用: model bridge client 未配置"
        ));
        assert!(!agent_unavailable_failure(
            "工具执行失败，任务不能标记完成：file_write: denied"
        ));
    }
}
