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
    AccessProfile, AgentContextAccessOperation, AgentContextAccessRecord, AgentContextPackage,
    AgentContextReference, AgentContextReferenceKind, AgentContextSupplement, EventId,
    ExecutionResultStatus, GoalId, SessionId, TASK_RUNTIME_FAILURE_PUBLIC_OUTPUT,
    TaskExecutorBinding, TaskId, TaskKind, TaskPolicy, TaskRuntimePayload, TaskStatus, TaskTier,
    ToolCallId, UtcMillis, WorkspaceId, estimate_text_tokens, public_task_output_refs,
    task_output_ref_is_internal_runtime_failure,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::GoalStatus;
use magi_session_store::{
    ExecutionThread, SessionStore, TimelineEntryKind, timeline_entry_visible_text,
};
use magi_snapshot::{SnapshotSession, ToolHook, ToolHookCtx};
use magi_tool_runtime::{
    BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
    builtin_permission_engine, canonical_builtin_tool_name, effective_tool_policy_allowed_paths,
    normalize_tool_policy_paths, tool_path_access_requests,
};

use crate::builtin_tool_schema::internal_builtin_tool_rejection_payload;
use crate::skill_apply_tool::{SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime};
use crate::task_execution_registry::SpawnedChildExecutionError;
use crate::tool_execution_ledger::{ToolCallExecutionDecision, ToolExecutionLedger};
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
const AGENT_CONTEXT_SUMMARY_MAX_CHARS: usize = 4_000;
const AGENT_CONTEXT_EXPECTED_OUTPUT_MAX_CHARS: usize = 2_000;
const AGENT_CONTEXT_CONSTRAINT_MAX_CHARS: usize = 600;
const AGENT_CONTEXT_PREVIEW_MAX_CHARS: usize = 600;
const AGENT_CONTEXT_REFERENCE_LIMIT: usize = 16;
const AGENT_CONTEXT_ACCESS_LIMIT: usize = 8;
const AGENT_UNAVAILABLE_PUBLIC_TEXT: &str = "代理当前不可用，主线需要改派或接管。";
const AGENT_SPAWN_STARTED_INSTRUCTION: &str = "代理已异步启动。若后续结论依赖该代理结果，必须调用 agent_wait，并传入 task_ids=[child_task_id] 收集终态结果；不要在未等待必要代理结果时直接给最终答复。";
const AGENT_WAIT_DEFAULT_TIMEOUT_MS: u64 = 300_000;
const AGENT_WAIT_MIN_TIMEOUT_MS: u64 = 1_000;
const AGENT_WAIT_MAX_TIMEOUT_MS: u64 = 1_800_000;
const TOOL_VISIBILITY_REJECTED_PUBLIC_ERROR: &str = "该工具在当前任务角色或阶段下不可用";
const TOOL_POLICY_CONTEXT_REJECTED_PUBLIC_ERROR: &str = "该工具受当前任务角色或策略限制";
const TOOL_POLICY_NEEDS_APPROVAL_PUBLIC_ERROR: &str =
    "受限访问已拦截该操作，请切换为完全访问权限后重试";

pub(crate) struct ToolPreflightDecision {
    pub(crate) payload: String,
    pub(crate) status: ExecutionResultStatus,
}

#[derive(Clone, Copy)]
struct TaskToolLifecycleContext<'a> {
    event_bus: &'a InMemoryEventBus,
    task: &'a magi_core::Task,
    session_id: &'a SessionId,
    workspace_id: &'a Option<WorkspaceId>,
    worker_id: Option<&'a magi_core::WorkerId>,
    execution_group_id: Option<&'a str>,
}

fn execute_task_tool_call_with_lifecycle<F>(
    context: TaskToolLifecycleContext<'_>,
    tool_call: &ChatToolCall,
    execute: F,
) -> (String, ExecutionResultStatus)
where
    F: FnOnce() -> (String, ExecutionResultStatus),
{
    let started_at = UtcMillis::now();
    publish_tool_lifecycle_event(
        context,
        "tool.call.started",
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
                task_id = %context.task.task_id.as_str(),
                session_id = %context.session_id.as_str(),
                "task tool execution panicked"
            );
            tool_execution_failed_result(&tool_call.function.name)
        });

    let finished_at = UtcMillis::now();
    publish_tool_lifecycle_event(
        context,
        "tool.call.finished",
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
    context: TaskToolLifecycleContext<'_>,
    event_type: &str,
    tool_call: &ChatToolCall,
    payload: serde_json::Value,
) {
    let payload = serde_json::json!({
        "tool_call_id": tool_call.id.as_str(),
        "tool_name": tool_call.function.name.as_str(),
        "task_id": context.task.task_id.as_str(),
        "session_id": context.session_id.as_str(),
        "workspace_id": context.workspace_id.as_ref().map(ToString::to_string),
        "worker_id": context.worker_id.map(ToString::to_string),
        "execution_group_id": context.execution_group_id,
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
        workspace_id: context.workspace_id.clone(),
        session_id: Some(context.session_id.clone()),
        mission_id: Some(context.task.mission_id.clone()),
        task_id: Some(context.task.task_id.clone()),
        ..EventContext::default()
    });
    let _ = context.event_bus.publish(event);
}

fn tool_arguments_preview(value: &str) -> String {
    const MAX_CHARS: usize = 2_000;
    let mut preview = value.chars().take(MAX_CHARS).collect::<String>();
    if value.chars().count() > MAX_CHARS {
        preview.push_str("...");
    }
    preview
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChildAgentOutput {
    summary: String,
    final_text: String,
    truncated: bool,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_task_tool_call_batch(
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
    plan_store: &magi_plan::PlanStore,
    project_memory: Option<&magi_project_memory::ProjectMemoryStore>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    workspace_root_path: Option<&PathBuf>,
    worker_id: Option<&magi_core::WorkerId>,
    tool_calls: &[ChatToolCall],
    tool_execution_ledger: &mut ToolExecutionLedger,
    snapshot_session: Option<Arc<SnapshotSession>>,
    execution_group_id: Option<String>,
) -> Vec<(String, ExecutionResultStatus)> {
    let decisions = tool_execution_ledger.plan(tool_calls, tool_registry);
    let execution_indices = decisions
        .iter()
        .enumerate()
        .filter_map(|(index, decision)| {
            matches!(decision, ToolCallExecutionDecision::Execute { .. }).then_some(index)
        })
        .collect::<Vec<_>>();
    let execution_calls = execution_indices
        .iter()
        .map(|index| tool_calls[*index].clone())
        .collect::<Vec<_>>();
    let executed_results = execute_task_tool_call_batch_unchecked(
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
        plan_store,
        project_memory,
        task,
        session_id,
        workspace_id,
        workspace_root_path,
        worker_id,
        &execution_calls,
        snapshot_session.clone(),
        execution_group_id.clone(),
    );
    let mut results = vec![None; tool_calls.len()];
    for (execution_index, result) in execution_indices.iter().zip(executed_results) {
        let ToolCallExecutionDecision::Execute { fingerprint } = &decisions[*execution_index]
        else {
            unreachable!("only execute decisions are dispatched");
        };
        tool_execution_ledger.record_execution(
            &tool_calls[*execution_index],
            fingerprint.as_ref(),
            &result,
        );
        results[*execution_index] = Some(result);
    }

    let mut fallback_indices = Vec::new();
    for (index, decision) in decisions.iter().enumerate() {
        match decision {
            ToolCallExecutionDecision::Reuse { result }
            | ToolCallExecutionDecision::BudgetExhausted { result } => {
                results[index] = Some((result.clone(), ExecutionResultStatus::Succeeded));
            }
            ToolCallExecutionDecision::ReuseAfterExecution {
                source_index,
                fingerprint,
            } => {
                let Some(source_result) = results[*source_index].as_ref() else {
                    unreachable!("duplicate source must execute before its reuse decision");
                };
                if let Some(reused) = tool_execution_ledger.reuse_after_execution(
                    &tool_calls[index],
                    fingerprint,
                    source_result,
                ) {
                    results[index] = Some(reused);
                } else {
                    fallback_indices.push(index);
                }
            }
            ToolCallExecutionDecision::Execute { .. } => {}
        }
    }

    if !fallback_indices.is_empty() {
        let fallback_calls = fallback_indices
            .iter()
            .map(|index| tool_calls[*index].clone())
            .collect::<Vec<_>>();
        let fallback_results = execute_task_tool_call_batch_unchecked(
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
            plan_store,
            project_memory,
            task,
            session_id,
            workspace_id,
            workspace_root_path,
            worker_id,
            &fallback_calls,
            snapshot_session,
            execution_group_id,
        );
        for (index, result) in fallback_indices.into_iter().zip(fallback_results) {
            let ToolCallExecutionDecision::ReuseAfterExecution { fingerprint, .. } =
                &decisions[index]
            else {
                unreachable!("only failed duplicate calls are retried");
            };
            tool_execution_ledger.record_execution(&tool_calls[index], Some(fingerprint), &result);
            results[index] = Some(result);
        }
    }

    results
        .into_iter()
        .enumerate()
        .map(|(index, result)| {
            result.unwrap_or_else(|| tool_execution_failed_result(&tool_calls[index].function.name))
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn execute_task_tool_call_batch_unchecked(
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
    plan_store: &magi_plan::PlanStore,
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
                        TaskToolLifecycleContext {
                            event_bus,
                            task,
                            session_id,
                            workspace_id,
                            worker_id,
                            execution_group_id: execution_group_id.as_deref(),
                        },
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
                                plan_store,
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
                                        TaskToolLifecycleContext {
                                            event_bus,
                                            task,
                                            session_id,
                                            workspace_id,
                                            worker_id,
                                            execution_group_id: execution_group_id_for_lifecycle
                                                .as_deref(),
                                        },
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
                                                plan_store,
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
#[derive(Clone, Copy)]
struct CoordinatorToolContext<'a> {
    event_bus: &'a InMemoryEventBus,
    agent_role_registry: &'a magi_agent_role::AgentRoleRegistry,
    task_store: &'a TaskStore,
    session_store: &'a SessionStore,
    execution_registry: &'a TaskExecutionRegistry,
    conversation_registry: &'a ConversationRegistry,
    spawn_graph: &'a Mutex<magi_spawn_graph::SpawnGraph>,
    plan_store: &'a magi_plan::PlanStore,
    task: &'a magi_core::Task,
    session_id: &'a SessionId,
    workspace_id: &'a Option<WorkspaceId>,
}

/// `agent_spawn` 在创建前失败时仍要返回可被模型、前端和通知中心共同消费的诊断合同。
///
/// 这里不携带底层错误链或配置内容；完整原因仅写入 tracing。`diagnostic_ref` 用于把
/// 用户可见结果关联回本次工具调用，而不是伪造一个不存在的子代理任务。
fn agent_spawn_failure_payload(
    tool_call: &ChatToolCall,
    status: &str,
    error_code: &str,
    failure_stage: &str,
    error: impl Into<String>,
    instruction: impl Into<String>,
) -> String {
    agent_spawn_failure_payload_for_child(
        tool_call,
        status,
        error_code,
        failure_stage,
        error,
        instruction,
        None,
    )
}

fn agent_spawn_failure_payload_for_child(
    tool_call: &ChatToolCall,
    status: &str,
    error_code: &str,
    failure_stage: &str,
    error: impl Into<String>,
    instruction: impl Into<String>,
    child_task_id: Option<&TaskId>,
) -> String {
    serde_json::json!({
        "tool": BuiltinToolName::AgentSpawn.as_str(),
        "status": status,
        "error_code": error_code,
        "failure_stage": failure_stage,
        "error": error.into(),
        "instruction": instruction.into(),
        "diagnostic_ref": format!("tool_call:{}", tool_call.id),
        "child_task_id": child_task_id.map(ToString::to_string),
    })
    .to_string()
}

fn execute_coordinator_tool(
    context: CoordinatorToolContext<'_>,
    tool: magi_tool_runtime::BuiltinToolName,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    let CoordinatorToolContext {
        event_bus,
        agent_role_registry,
        task_store,
        session_store,
        execution_registry,
        conversation_registry,
        spawn_graph,
        plan_store,
        task,
        session_id,
        workspace_id,
    } = context;
    let parsed: serde_json::Value = match serde_json::from_str(&tool_call.function.arguments) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(error = %err, tool = tool.as_str(), "coordinator tool arguments parse failed");
            if tool == BuiltinToolName::AgentSpawn {
                return (
                    agent_spawn_failure_payload(
                        tool_call,
                        "failed",
                        "invalid_arguments",
                        "input_validation",
                        "代理派发参数不是有效的 JSON 对象",
                        "请使用 agent_spawn 的完整 JSON Schema 重新提交参数；不要把嵌套对象编码成普通文本。",
                    ),
                    ExecutionResultStatus::Failed,
                );
            }
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
            let task_name = parsed
                .get("task_name")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if !valid_agent_task_name(&task_name) {
                return (
                    agent_spawn_failure_payload(
                        tool_call,
                        "failed",
                        "invalid_task_name",
                        "input_validation",
                        "agent_spawn task_name 只允许小写字母、数字和下划线，长度必须为 1-48",
                        "请生成一个唯一的小写 task_name 后重新调用 agent_spawn。",
                    ),
                    ExecutionResultStatus::Failed,
                );
            }
            let parent_canonical_name = task.canonical_task_name().unwrap_or("/root");
            let canonical_task_name = child_canonical_task_name(parent_canonical_name, &task_name);
            if task_store
                .get_children(&task.task_id)
                .iter()
                .any(|child| child.canonical_task_name() == Some(canonical_task_name.as_str()))
            {
                return (
                    agent_spawn_failure_payload(
                        tool_call,
                        "failed",
                        "duplicate_task_name",
                        "input_validation",
                        format!("同一父任务下 task_name 已存在: {task_name}"),
                        "请生成一个未使用的 task_name 后重新调用 agent_spawn。",
                    ),
                    ExecutionResultStatus::Failed,
                );
            }
            let plan_item_id = parsed
                .get("plan_item_id")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(magi_core::PlanItemId::new);
            if let Some(plan_item_id) = plan_item_id.as_ref()
                && !plan_store.has_item(plan_item_id)
            {
                return (
                    agent_spawn_failure_payload(
                        tool_call,
                        "failed",
                        "plan_item_not_found",
                        "input_validation",
                        format!("agent_spawn plan_item_id 不存在: {plan_item_id}"),
                        "请使用当前 update_plan 返回的顶层计划项 ID，或省略 plan_item_id 后重新派发。",
                    ),
                    ExecutionResultStatus::Failed,
                );
            }
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
                    agent_spawn_failure_payload(
                        tool_call,
                        "failed",
                        "missing_required_fields",
                        "input_validation",
                        "agent_spawn 缺少必需字段 role 或 goal",
                        "请补齐 role 和 goal，并保持其为非空字符串后重新调用 agent_spawn。",
                    ),
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
            let role = requested_role;
            let goal = requested_goal;
            let display_name = requested_display_name;
            let display_name_chars = display_name.chars().count();
            if display_name.is_empty() {
                return (
                    agent_spawn_failure_payload(
                        tool_call,
                        "failed",
                        "missing_display_name",
                        "input_validation",
                        "agent_spawn 缺少必需字段 display_name",
                        "请提供 3-30 个字符的 display_name 后重新调用 agent_spawn。",
                    ),
                    ExecutionResultStatus::Failed,
                );
            }
            if !agent_role_registry.is_spawnable_agent_role(&role) {
                let role_hint = spawnable_role_ids.join(" / ");
                return (
                    agent_spawn_failure_payload(
                        tool_call,
                        "degraded",
                        "agent_role_not_spawnable",
                        "input_validation",
                        "该 role 不是可派发代理角色。coordinator 是主线编排身份，不能通过 agent_spawn 派发。",
                        format!(
                            "请改派 {role_hint} 等可用专业代理；如果无需继续派发，则由主线基于已有上下文直接推进并给出结果。"
                        ),
                    ),
                    ExecutionResultStatus::Succeeded,
                );
            }
            if !(3..=30).contains(&display_name_chars) {
                return (
                    agent_spawn_failure_payload(
                        tool_call,
                        "failed",
                        "invalid_display_name",
                        "input_validation",
                        format!(
                            "agent_spawn display_name 长度必须在 3-30 个字符之间，实际 {display_name_chars}",
                        ),
                        "请提供长度为 3-30 个字符的 display_name 后重新调用 agent_spawn。",
                    ),
                    ExecutionResultStatus::Failed,
                );
            }
            if parsed.get("context").is_some() {
                return (
                    agent_spawn_failure_payload(
                        tool_call,
                        "failed",
                        "legacy_context_rejected",
                        "input_validation",
                        "agent_spawn 不再接受 context 字符串，请使用结构化 context_package",
                        "请移除 context，并按 Schema 传入 context_package 对象后重新调用 agent_spawn。",
                    ),
                    ExecutionResultStatus::Failed,
                );
            }
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
            let context_package = match parse_agent_context_package(
                &parsed,
                &task.task_id,
                now,
                seq,
            ) {
                Ok(package) => package,
                Err(error) => {
                    return (
                        agent_spawn_failure_payload(
                            tool_call,
                            "failed",
                            "invalid_context_package",
                            "input_validation",
                            error,
                            "请按 agent_spawn Schema 重新提供 context_package：summary、expected_output、constraints 必须为规定类型；references 中每条引用的 kind、source_ref 必须为字符串，若提供 title 也必须为字符串。",
                        ),
                        ExecutionResultStatus::Failed,
                    );
                }
            };
            let child_policy_snapshot =
                agent_spawn_child_policy_snapshot(task.policy_snapshot.as_ref());
            let child_access_profile = child_policy_snapshot.effective_access_profile();
            let child_dependency_ids = agent_spawn_child_dependency_ids(task);
            let child_input_refs = context_package
                .references
                .iter()
                .map(|reference| reference.source_ref.clone())
                .collect();
            let child = magi_core::Task {
                task_id: child_id.clone(),
                mission_id: task.mission_id.clone(),
                root_task_id: task.root_task_id.clone(),
                parent_task_id: Some(task.task_id.clone()),
                kind: task_kind,
                title: display_name,
                goal: goal.clone(),
                status: TaskStatus::Pending,
                dependency_ids: child_dependency_ids,
                required_children: Vec::new(),
                policy_snapshot: Some(child_policy_snapshot),
                executor_binding: Some(
                    TaskExecutorBinding::for_role(&role)
                        .with_parallelism_group(
                            parsed
                                .get("parallelism_group")
                                .and_then(|value| value.as_str())
                                .map(str::to_string),
                        )
                        .with_canonical_task_name(canonical_task_name.clone())
                        .with_plan_item_id(plan_item_id.clone()),
                ),
                knowledge_refs: Vec::new(),
                workspace_scope: task.workspace_scope.clone(),
                write_scope: task.write_scope.clone(),
                input_refs: child_input_refs,
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                runtime_payload: TaskRuntimePayload::AgentContext {
                    package: Box::new(context_package.clone()),
                    accesses: Vec::new(),
                },
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
                    if let SpawnedChildExecutionError::RoleCapacityExceeded {
                        role,
                        active,
                        limit,
                    } = error
                    {
                        return (
                            agent_spawn_failure_payload(
                                tool_call,
                                "rejected",
                                "agent_spawn_capacity_exceeded",
                                "registration",
                                format!(
                                    "角色 {role} 已达到代理实例上限：最多 {limit} 个活跃实例，当前 {active} 个"
                                ),
                                "请先用 agent_wait 收集该角色已启动代理的结果；有实例退出活跃状态后再继续创建同角色代理。其他角色不受该角色容量占用影响。",
                            ),
                            ExecutionResultStatus::Rejected,
                        );
                    }
                    return (
                        agent_spawn_failure_payload(
                            tool_call,
                            "failed",
                            "agent_spawn_registration_failed",
                            "registration",
                            "代理启动失败，请由主线继续或改派其他角色",
                            "请根据当前任务继续推进，或改派其他可用角色；如需排查，请使用本次诊断引用定位工具调用。",
                        ),
                        ExecutionResultStatus::Failed,
                    );
                }
            };
            conversation_registry.open_task_signal_channel(session_id, &child_id);
            if let Some(plan_item_id) = plan_item_id.clone() {
                match plan_store.bind_task(child_id.clone(), plan_item_id) {
                    Ok(plan) => magi_plan::publish_plan_event(
                        event_bus,
                        magi_plan::plan_event_type(&plan),
                        &plan,
                        workspace_id.as_ref(),
                        Some(&child_id),
                        Some(&task.mission_id),
                    ),
                    Err(error) => {
                        tracing::error!(
                            error = %error,
                            child_task_id = %child_id,
                            canonical_task_name,
                            "agent_spawn 计划绑定失败"
                        );
                        let _ = task_store.update_status(&child_id, TaskStatus::Killed);
                        return (
                            agent_spawn_failure_payload_for_child(
                                tool_call,
                                "failed",
                                "agent_spawn_plan_binding_failed",
                                "plan_binding",
                                "代理已创建但计划绑定失败，运行已终止",
                                "请修正或省略 plan_item_id 后重新派发；本次创建的代理已终止，可打开详情查看其执行记录。",
                                Some(&child_id),
                            ),
                            ExecutionResultStatus::Failed,
                        );
                    }
                }
            }
            publish_event(
                "task.coordinator.agent_spawn",
                serde_json::json!({
                    "parent_task_id": task.task_id.to_string(),
                    "child_task_id": child_id.to_string(),
                    "canonical_task_name": canonical_task_name,
                    "plan_item_id": plan_item_id.as_ref().map(ToString::to_string),
                    "role": role,
                    "access_profile": child_access_profile.as_str(),
                    "goal": goal,
                    "context_package_id": context_package.package_id,
                    "context_revision": context_package.revision,
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
                    "canonical_task_name": canonical_task_name,
                    "plan_item_id": plan_item_id.as_ref().map(ToString::to_string),
                    "role": role,
                    "access_profile": child_access_profile.as_str(),
                    "title": child.title,
                    "assignment": {
                        "title": child.title,
                        "goal": child.goal,
                        "role": role,
                        "access_profile": child_access_profile.as_str(),
                        "context_package_id": context_package.package_id,
                        "context_revision": context_package.revision,
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
            execute_agent_wait_with_runtime(
                AgentWaitRuntime {
                    task_store,
                    spawn_graph,
                    conversation_registry,
                    session_id,
                    session_threads: &session_threads,
                },
                task,
                tool,
                &parsed,
            )
        }
        magi_tool_runtime::BuiltinToolName::AgentSend => execute_agent_send(
            task_store,
            conversation_registry,
            task,
            session_id,
            tool,
            &parsed,
            &publish_event,
        ),
        _ => unreachable!("execute_coordinator_tool 只接收协调器代理工具变体"),
    }
}

fn valid_agent_task_name(task_name: &str) -> bool {
    (1..=48).contains(&task_name.len())
        && task_name.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

fn execute_agent_send(
    task_store: &TaskStore,
    conversation_registry: &ConversationRegistry,
    parent_task: &magi_core::Task,
    session_id: &SessionId,
    tool: BuiltinToolName,
    parsed: &serde_json::Value,
    publish_event: &impl Fn(&str, serde_json::Value),
) -> (String, ExecutionResultStatus) {
    let target_task_id = TaskId::new(
        parsed
            .get("task_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim(),
    );
    let message = match required_bounded_context_text(
        parsed.get("message"),
        "agent_send.message",
        AGENT_CONTEXT_SUMMARY_MAX_CHARS,
    ) {
        Ok(message) => message,
        Err(error) => return context_tool_failure(tool, "invalid_arguments", error),
    };
    let Some(target_task) = task_store.get_task(&target_task_id) else {
        return context_tool_failure(tool, "target_not_found", "目标代理任务不存在");
    };
    if target_task.root_task_id != parent_task.root_task_id
        || target_task.mission_id != parent_task.mission_id
        || target_task.task_id == parent_task.task_id
    {
        return context_tool_failure(tool, "target_out_of_scope", "目标代理不属于当前执行链");
    }
    if !matches!(
        target_task.status,
        TaskStatus::Pending | TaskStatus::Running
    ) {
        return context_tool_failure(
            tool,
            "target_terminal",
            "目标代理已进入终态，不能继续发送上下文",
        );
    }
    let now = UtcMillis::now();
    let sequence = AGENT_SPAWN_SEQ.fetch_add(1, Ordering::Relaxed);
    let references =
        match parse_agent_context_references(parsed.get("references"), now, sequence, "agent_send")
        {
            Ok(references) => references,
            Err(error) => return context_tool_failure(tool, "invalid_references", error),
        };
    let estimated_tokens = estimate_text_tokens(&message)
        + references
            .iter()
            .map(|reference| reference.estimated_tokens)
            .sum::<usize>();
    let supplement = AgentContextSupplement {
        supplement_id: format!("ctxsupp-{}-{}-{sequence}", target_task_id.as_str(), now.0),
        author_task_id: parent_task.task_id.clone(),
        message: message.clone(),
        references: references.clone(),
        estimated_tokens,
        created_at: now,
    };
    let access = AgentContextAccessRecord {
        record_id: format!(
            "ctxaccess-send-{}-{}-{sequence}",
            target_task_id.as_str(),
            now.0
        ),
        operation: AgentContextAccessOperation::Send,
        query: Some(message.clone()),
        reference_ids: references
            .iter()
            .map(|reference| reference.reference_id.clone())
            .collect(),
        estimated_tokens,
        occurred_at: now,
    };
    let commit =
        conversation_registry.enqueue_task_signal_with(session_id, &target_task_id, || {
            let package =
                task_store.append_agent_context_supplement(&target_task_id, supplement.clone())?;
            task_store.append_agent_context_access(&target_task_id, access.clone())?;
            let signal = RuntimeSignal {
                author: MailboxAuthor::Parent(parent_task.task_id.to_string()),
                kind: MailboxKind::Message,
                trigger_turn: true,
                payload: serde_json::json!({
                    "type": "agent_context_supplement",
                    "package_id": package.package_id,
                    "revision": package.revision,
                    "supplement": supplement,
                }),
                enqueued_at: now,
            };
            Ok::<_, magi_core::DomainError>((package, signal))
        });
    let package = match commit {
        Ok(package) => package,
        Err(crate::TaskSignalCommitError::ChannelClosed) => {
            return context_tool_failure(
                tool,
                "target_not_running",
                "目标代理尚未启动或已结束，无法接收运行中上下文",
            );
        }
        Err(crate::TaskSignalCommitError::Commit(error)) => {
            return context_tool_failure(tool, "context_persist_failed", error.to_string());
        }
    };
    publish_event(
        "task.coordinator.agent_send",
        serde_json::json!({
            "parent_task_id": parent_task.task_id,
            "child_task_id": target_task_id,
            "package_id": package.package_id,
            "revision": package.revision,
            "estimated_tokens": estimated_tokens,
        }),
    );
    (
        serde_json::json!({
            "tool": tool.as_str(),
            "status": "sent",
            "task_id": target_task_id,
            "package_id": package.package_id,
            "revision": package.revision,
            "estimated_tokens": estimated_tokens,
        })
        .to_string(),
        ExecutionResultStatus::Succeeded,
    )
}

fn context_tool_failure(
    tool: BuiltinToolName,
    error_code: &str,
    error: impl ToString,
) -> (String, ExecutionResultStatus) {
    (
        serde_json::json!({
            "tool": tool.as_str(),
            "status": "failed",
            "error_code": error_code,
            "error": error.to_string(),
        })
        .to_string(),
        ExecutionResultStatus::Failed,
    )
}

#[derive(Clone)]
struct ResolvedAgentContext {
    reference_id: String,
    kind: AgentContextReferenceKind,
    title: String,
    content: String,
}

fn execute_agent_context_tool(
    task_store: &TaskStore,
    session_store: &SessionStore,
    conversation_registry: &ConversationRegistry,
    task: &magi_core::Task,
    session_id: &SessionId,
    tool: BuiltinToolName,
    arguments: &str,
) -> (String, ExecutionResultStatus) {
    let parsed = match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(parsed) => parsed,
        Err(error) => return context_tool_failure(tool, "invalid_arguments", error),
    };
    match tool {
        BuiltinToolName::ContextSearch => {
            execute_context_search(task_store, session_store, task, session_id, tool, &parsed)
        }
        BuiltinToolName::ContextRead => {
            execute_context_read(task_store, session_store, task, session_id, tool, &parsed)
        }
        BuiltinToolName::ContextRequest => execute_context_request(
            task_store,
            conversation_registry,
            task,
            session_id,
            tool,
            &parsed,
        ),
        _ => unreachable!("execute_agent_context_tool 只接收上下文工具"),
    }
}

fn execute_context_search(
    task_store: &TaskStore,
    session_store: &SessionStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    tool: BuiltinToolName,
    parsed: &serde_json::Value,
) -> (String, ExecutionResultStatus) {
    let query =
        match required_bounded_context_text(parsed.get("query"), "context_search.query", 1_000) {
            Ok(query) => query,
            Err(error) => return context_tool_failure(tool, "invalid_query", error),
        };
    let limit = parsed
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(8)
        .clamp(1, 20) as usize;
    let mut candidates = collect_context_candidates(task_store, session_store, task, session_id);
    candidates.retain(|candidate| context_candidate_matches(&query, candidate));
    candidates.sort_by(|left, right| {
        context_candidate_score(&query, right)
            .cmp(&context_candidate_score(&query, left))
            .then_with(|| left.reference_id.cmp(&right.reference_id))
    });
    candidates.truncate(limit);
    let estimated_tokens = candidates
        .iter()
        .map(|candidate| estimate_text_tokens(&candidate.content))
        .sum();
    let reference_ids = candidates
        .iter()
        .map(|candidate| candidate.reference_id.clone())
        .collect::<Vec<_>>();
    if let Err(error) = record_agent_context_access(
        task_store,
        task,
        AgentContextAccessOperation::Search,
        Some(query.clone()),
        reference_ids,
        estimated_tokens,
    ) {
        return context_tool_failure(tool, "context_audit_failed", error);
    }
    let results = candidates
        .into_iter()
        .map(|candidate| {
            serde_json::json!({
                "reference_id": candidate.reference_id,
                "kind": candidate.kind,
                "title": candidate.title,
                "preview": compact_context_preview(&candidate.content),
                "estimated_tokens": estimate_text_tokens(&candidate.content),
            })
        })
        .collect::<Vec<_>>();
    (
        serde_json::json!({
            "tool": tool.as_str(),
            "status": "ok",
            "query": query,
            "results": results,
            "estimated_tokens": estimated_tokens,
        })
        .to_string(),
        ExecutionResultStatus::Succeeded,
    )
}

fn execute_context_read(
    task_store: &TaskStore,
    session_store: &SessionStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    tool: BuiltinToolName,
    parsed: &serde_json::Value,
) -> (String, ExecutionResultStatus) {
    let Some(reference_values) = parsed
        .get("reference_ids")
        .and_then(serde_json::Value::as_array)
    else {
        return context_tool_failure(
            tool,
            "invalid_references",
            "context_read.reference_ids 必须是非空数组",
        );
    };
    if reference_values.is_empty() || reference_values.len() > AGENT_CONTEXT_ACCESS_LIMIT {
        return context_tool_failure(
            tool,
            "invalid_references",
            format!("context_read 单次必须读取 1-{AGENT_CONTEXT_ACCESS_LIMIT} 条引用"),
        );
    }
    let mut requested_ids = Vec::with_capacity(reference_values.len());
    for value in reference_values {
        let Some(reference_id) = value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return context_tool_failure(
                tool,
                "invalid_references",
                "reference_ids 只能包含非空字符串",
            );
        };
        if !requested_ids
            .iter()
            .any(|existing| existing == reference_id)
        {
            requested_ids.push(reference_id.to_string());
        }
    }
    let candidates = collect_context_candidates(task_store, session_store, task, session_id);
    let mut results = Vec::with_capacity(requested_ids.len());
    for reference_id in &requested_ids {
        let Some(candidate) = candidates
            .iter()
            .find(|candidate| &candidate.reference_id == reference_id)
        else {
            return context_tool_failure(
                tool,
                "reference_out_of_scope",
                format!("引用 {reference_id} 不属于当前 session 或执行链"),
            );
        };
        results.push(candidate.clone());
    }
    let estimated_tokens = results
        .iter()
        .map(|candidate| estimate_text_tokens(&candidate.content))
        .sum();
    if let Err(error) = record_agent_context_access(
        task_store,
        task,
        AgentContextAccessOperation::Read,
        None,
        requested_ids.clone(),
        estimated_tokens,
    ) {
        return context_tool_failure(tool, "context_audit_failed", error);
    }
    (
        serde_json::json!({
            "tool": tool.as_str(),
            "status": "ok",
            "results": results.into_iter().map(|candidate| serde_json::json!({
                "reference_id": candidate.reference_id,
                "kind": candidate.kind,
                "title": candidate.title,
                "content": candidate.content,
                "estimated_tokens": estimate_text_tokens(&candidate.content),
            })).collect::<Vec<_>>(),
            "estimated_tokens": estimated_tokens,
        })
        .to_string(),
        ExecutionResultStatus::Succeeded,
    )
}

fn execute_context_request(
    task_store: &TaskStore,
    conversation_registry: &ConversationRegistry,
    task: &magi_core::Task,
    session_id: &SessionId,
    tool: BuiltinToolName,
    parsed: &serde_json::Value,
) -> (String, ExecutionResultStatus) {
    let request = match required_bounded_context_text(
        parsed.get("request"),
        "context_request.request",
        AGENT_CONTEXT_SUMMARY_MAX_CHARS,
    ) {
        Ok(request) => request,
        Err(error) => return context_tool_failure(tool, "invalid_request", error),
    };
    let Some(package) = task.agent_context_package() else {
        return context_tool_failure(
            tool,
            "context_package_missing",
            "当前任务没有 AgentContextPackage",
        );
    };
    let parent_task_id = package.parent_task_id.clone();
    let Some(parent_task) = task_store.get_task(&parent_task_id) else {
        return context_tool_failure(tool, "parent_not_found", "父任务不存在");
    };
    if parent_task.root_task_id != task.root_task_id
        || parent_task.mission_id != task.mission_id
        || !matches!(
            parent_task.status,
            TaskStatus::Pending | TaskStatus::Running
        )
    {
        return context_tool_failure(tool, "parent_not_running", "父任务不在当前活跃执行链中");
    }
    let now = UtcMillis::now();
    let sequence = AGENT_SPAWN_SEQ.fetch_add(1, Ordering::Relaxed);
    let estimated_tokens = estimate_text_tokens(&request);
    let access = AgentContextAccessRecord {
        record_id: format!(
            "ctxaccess-request-{}-{}-{sequence}",
            task.task_id.as_str(),
            now.0
        ),
        operation: AgentContextAccessOperation::Request,
        query: Some(request.clone()),
        reference_ids: Vec::new(),
        estimated_tokens,
        occurred_at: now,
    };
    let commit =
        conversation_registry.enqueue_task_signal_with(session_id, &parent_task_id, || {
            task_store.append_agent_context_access(&task.task_id, access.clone())?;
            Ok::<_, magi_core::DomainError>((
                (),
                RuntimeSignal {
                    author: MailboxAuthor::Child(task.task_id.to_string()),
                    kind: MailboxKind::Message,
                    trigger_turn: true,
                    payload: serde_json::json!({
                        "type": "agent_context_request",
                        "child_task_id": task.task_id,
                        "package_id": package.package_id,
                        "revision": package.revision,
                        "request": request,
                    }),
                    enqueued_at: now,
                },
            ))
        });
    match commit {
        Ok(()) => {
            task_store.notify_runtime_change();
            let timeout_ms = parsed
                .get("timeout_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(AGENT_WAIT_DEFAULT_TIMEOUT_MS)
                .clamp(AGENT_WAIT_MIN_TIMEOUT_MS, AGENT_WAIT_MAX_TIMEOUT_MS);
            let started_at = std::time::Instant::now();
            loop {
                let elapsed_ms =
                    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                if elapsed_ms >= timeout_ms {
                    return (
                        serde_json::json!({
                            "tool": tool.as_str(),
                            "status": "timeout",
                            "parent_task_id": parent_task_id,
                            "estimated_tokens": estimated_tokens,
                            "instruction": "父任务尚未回复。只在现有证据足以完成目标时继续；否则在最终答复中明确说明缺失上下文。",
                        })
                        .to_string(),
                        ExecutionResultStatus::Succeeded,
                    );
                }
                if task_store.get_task(&task.task_id).is_some_and(|current| {
                    matches!(current.status, TaskStatus::Failed | TaskStatus::Killed)
                }) {
                    return context_tool_failure(
                        tool,
                        "task_interrupted",
                        "等待上下文回复时任务已中断",
                    );
                }
                let wait_ms = (timeout_ms - elapsed_ms).min(250);
                let responses = conversation_registry.wait_for_task_signals(
                    session_id,
                    &task.task_id,
                    std::time::Duration::from_millis(wait_ms),
                );
                if !responses.is_empty() {
                    return (
                        serde_json::json!({
                            "tool": tool.as_str(),
                            "status": "resolved",
                            "parent_task_id": parent_task_id,
                            "responses": responses,
                            "estimated_tokens": estimated_tokens,
                            "instruction": "父任务已回复。请把 responses 中的 agent_context_supplement 作为当前任务补充事实继续执行。",
                        })
                        .to_string(),
                        ExecutionResultStatus::Succeeded,
                    );
                }
            }
        }
        Err(crate::TaskSignalCommitError::ChannelClosed) => context_tool_failure(
            tool,
            "parent_channel_closed",
            "父任务当前无法接收上下文请求",
        ),
        Err(crate::TaskSignalCommitError::Commit(error)) => {
            context_tool_failure(tool, "context_audit_failed", error)
        }
    }
}

fn collect_context_candidates(
    task_store: &TaskStore,
    session_store: &SessionStore,
    task: &magi_core::Task,
    session_id: &SessionId,
) -> Vec<ResolvedAgentContext> {
    let mut candidates = session_store
        .timeline_for_session(session_id)
        .into_iter()
        .filter(|entry| {
            matches!(
                entry.kind,
                TimelineEntryKind::UserMessage | TimelineEntryKind::AssistantMessage
            )
        })
        .filter_map(|entry| {
            let content = timeline_entry_visible_text(&entry.message)?;
            let title = match entry.kind {
                TimelineEntryKind::UserMessage => "用户消息",
                TimelineEntryKind::AssistantMessage => "助手消息",
                _ => return None,
            };
            Some(ResolvedAgentContext {
                reference_id: format!("turn:{}", entry.entry_id),
                kind: AgentContextReferenceKind::ConversationTurn,
                title: title.to_string(),
                content,
            })
        })
        .collect::<Vec<_>>();
    for candidate_task in task_store.all_tasks().into_iter().filter(|candidate| {
        candidate.root_task_id == task.root_task_id && candidate.mission_id == task.mission_id
    }) {
        for (index, content) in candidate_task.output_refs.iter().enumerate() {
            candidates.push(ResolvedAgentContext {
                reference_id: format!("task:{}:output:{index}", candidate_task.task_id.as_str()),
                kind: AgentContextReferenceKind::TaskOutput,
                title: format!("{} · 输出 {}", candidate_task.title, index + 1),
                content: content.clone(),
            });
        }
        for (index, content) in candidate_task.evidence_refs.iter().enumerate() {
            candidates.push(ResolvedAgentContext {
                reference_id: format!("task:{}:evidence:{index}", candidate_task.task_id.as_str()),
                kind: AgentContextReferenceKind::TaskEvidence,
                title: format!("{} · 证据 {}", candidate_task.title, index + 1),
                content: content.clone(),
            });
        }
    }
    if let Some(package) = task.agent_context_package() {
        for reference in package.references.iter().chain(
            package
                .supplements
                .iter()
                .flat_map(|supplement| supplement.references.iter()),
        ) {
            let content = resolve_package_reference_content(&candidates, reference)
                .unwrap_or_else(|| reference.preview.clone());
            candidates.push(ResolvedAgentContext {
                reference_id: reference.reference_id.clone(),
                kind: reference.kind,
                title: reference.title.clone(),
                content,
            });
        }
        for supplement in &package.supplements {
            candidates.push(ResolvedAgentContext {
                reference_id: supplement.supplement_id.clone(),
                kind: AgentContextReferenceKind::Other,
                title: "主线补充上下文".to_string(),
                content: supplement.message.clone(),
            });
        }
    }
    candidates
}

fn resolve_package_reference_content(
    candidates: &[ResolvedAgentContext],
    reference: &AgentContextReference,
) -> Option<String> {
    candidates
        .iter()
        .find(|candidate| candidate.reference_id == reference.source_ref)
        .map(|candidate| candidate.content.clone())
}

fn context_candidate_matches(query: &str, candidate: &ResolvedAgentContext) -> bool {
    context_candidate_score(query, candidate) > 0
}

fn context_candidate_score(query: &str, candidate: &ResolvedAgentContext) -> usize {
    let query = query.to_lowercase();
    let haystack = format!("{} {}", candidate.title, candidate.content).to_lowercase();
    if haystack.contains(&query) {
        return 100 + query.chars().count();
    }
    query
        .split(|character: char| character.is_whitespace() || character.is_ascii_punctuation())
        .filter(|term| !term.is_empty())
        .filter(|term| haystack.contains(term))
        .map(str::len)
        .sum()
}

fn compact_context_preview(content: &str) -> String {
    let compact = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= AGENT_CONTEXT_PREVIEW_MAX_CHARS {
        compact
    } else {
        compact
            .chars()
            .take(AGENT_CONTEXT_PREVIEW_MAX_CHARS)
            .collect()
    }
}

fn record_agent_context_access(
    task_store: &TaskStore,
    task: &magi_core::Task,
    operation: AgentContextAccessOperation,
    query: Option<String>,
    reference_ids: Vec<String>,
    estimated_tokens: usize,
) -> Result<(), magi_core::DomainError> {
    let now = UtcMillis::now();
    let sequence = AGENT_SPAWN_SEQ.fetch_add(1, Ordering::Relaxed);
    task_store.append_agent_context_access(
        &task.task_id,
        AgentContextAccessRecord {
            record_id: format!("ctxaccess-{}-{}-{sequence}", task.task_id.as_str(), now.0),
            operation,
            query,
            reference_ids,
            estimated_tokens,
            occurred_at: now,
        },
    )
}

fn child_canonical_task_name(parent_name: &str, task_name: &str) -> String {
    let parent_name = parent_name.trim().trim_end_matches('/');
    let parent_name = if parent_name.is_empty() {
        "/root"
    } else {
        parent_name
    };
    format!("{parent_name}/{task_name}")
}

pub(crate) fn execute_goal_tool(
    session_store: &SessionStore,
    session_id: &SessionId,
    thread_id: magi_core::ThreadId,
    access_profile: AccessProfile,
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
            match session_store.create_goal(
                session_id.clone(),
                thread_id,
                objective,
                access_profile,
                token_budget,
            ) {
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

fn agent_spawn_child_policy_snapshot(parent_policy: Option<&TaskPolicy>) -> TaskPolicy {
    parent_policy
        .cloned()
        .unwrap_or_else(default_agent_spawn_policy)
}

fn agent_spawn_child_dependency_ids(parent: &magi_core::Task) -> Vec<TaskId> {
    parent.dependency_ids.clone()
}

fn parse_agent_context_package(
    parsed: &serde_json::Value,
    parent_task_id: &TaskId,
    now: UtcMillis,
    sequence: u64,
) -> Result<AgentContextPackage, String> {
    // 部分 OpenAI-compatible 模型会把嵌套 object 二次序列化为 JSON 字符串。
    // 这里统一在 tool 参数边界还原一次，后续仍只接受同一份结构化合同，避免
    // DeepSeek 等模型因合法内容的表示差异被错误拒绝。
    let context_package = match parsed.get("context_package") {
        Some(serde_json::Value::Object(_)) => parsed
            .get("context_package")
            .cloned()
            .expect("context_package object should remain present"),
        Some(serde_json::Value::String(encoded)) => {
            serde_json::from_str(encoded).map_err(|_| {
                "agent_spawn 的 context_package 必须是对象或可解析为对象的 JSON 字符串".to_string()
            })?
        }
        _ => return Err("agent_spawn 缺少结构化 context_package".to_string()),
    };
    let value = context_package
        .as_object()
        .ok_or_else(|| "agent_spawn 的 context_package 必须是结构化对象".to_string())?;
    let summary = required_bounded_context_text(
        value.get("summary"),
        "context_package.summary",
        AGENT_CONTEXT_SUMMARY_MAX_CHARS,
    )?;
    let expected_output = required_bounded_context_text(
        value.get("expected_output"),
        "context_package.expected_output",
        AGENT_CONTEXT_EXPECTED_OUTPUT_MAX_CHARS,
    )?;
    let constraints = value
        .get("constraints")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "context_package.constraints 必须是数组".to_string())?
        .iter()
        .enumerate()
        .map(|(index, item)| {
            required_bounded_context_text(
                Some(item),
                &format!("context_package.constraints[{index}]"),
                AGENT_CONTEXT_CONSTRAINT_MAX_CHARS,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    // 空引用列表不承载额外事实，兼容模型省略该字段时等同于 []。
    // 显式传入非数组值仍由解析器拒绝，避免放宽结构校验。
    let references =
        parse_agent_context_references(value.get("references"), now, sequence, "spawn")?;
    Ok(AgentContextPackage {
        package_id: format!(
            "agent-context-{}-{}-{sequence}",
            parent_task_id.as_str(),
            now.0
        ),
        revision: 1,
        parent_task_id: parent_task_id.clone(),
        summary,
        constraints,
        expected_output,
        references,
        supplements: Vec::new(),
        created_at: now,
        updated_at: now,
    })
}

fn parse_agent_context_references(
    value: Option<&serde_json::Value>,
    now: UtcMillis,
    sequence: u64,
    scope: &str,
) -> Result<Vec<AgentContextReference>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| format!("{scope}.references 必须是数组"))?;
    if values.len() > AGENT_CONTEXT_REFERENCE_LIMIT {
        return Err(format!(
            "{scope}.references 最多允许 {AGENT_CONTEXT_REFERENCE_LIMIT} 条"
        ));
    }
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let object = value
                .as_object()
                .ok_or_else(|| format!("{scope}.references[{index}] 必须是对象"))?;
            let kind = parse_agent_context_reference_kind(
                object
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            )?;
            let source_ref = required_bounded_context_text(
                object.get("source_ref"),
                &format!("{scope}.references[{index}].source_ref"),
                1_000,
            )?;
            let title = optional_bounded_context_text(
                object.get("title"),
                &format!("{scope}.references[{index}].title"),
                200,
            )?
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| default_agent_context_reference_title(&source_ref));
            let preview = optional_bounded_context_text(
                object.get("preview"),
                &format!("{scope}.references[{index}].preview"),
                AGENT_CONTEXT_PREVIEW_MAX_CHARS,
            )?
            .unwrap_or_default();
            Ok(AgentContextReference {
                reference_id: format!("ctxref-{scope}-{}-{sequence}-{index}", now.0),
                kind,
                title,
                source_ref,
                estimated_tokens: estimate_text_tokens(&preview),
                preview,
            })
        })
        .collect()
}

fn parse_agent_context_reference_kind(value: &str) -> Result<AgentContextReferenceKind, String> {
    match value.trim() {
        "conversation_turn" => Ok(AgentContextReferenceKind::ConversationTurn),
        "task_output" => Ok(AgentContextReferenceKind::TaskOutput),
        "task_evidence" => Ok(AgentContextReferenceKind::TaskEvidence),
        "file" => Ok(AgentContextReferenceKind::File),
        "knowledge" => Ok(AgentContextReferenceKind::Knowledge),
        "other" => Ok(AgentContextReferenceKind::Other),
        _ => Err(format!("未知上下文引用类型: {value}")),
    }
}

fn required_bounded_context_text(
    value: Option<&serde_json::Value>,
    field: &str,
    max_chars: usize,
) -> Result<String, String> {
    let text = bounded_context_text(value, field, max_chars)?;
    if text.is_empty() {
        Err(format!("{field} 不能为空"))
    } else {
        Ok(text)
    }
}

fn optional_bounded_context_text(
    value: Option<&serde_json::Value>,
    field: &str,
    max_chars: usize,
) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    bounded_context_text(Some(value), field, max_chars).map(Some)
}

fn default_agent_context_reference_title(source_ref: &str) -> String {
    let mut title = source_ref.trim().chars().take(200).collect::<String>();
    if source_ref.trim().chars().count() > 200 {
        title.push('…');
    }
    title
}

fn bounded_context_text(
    value: Option<&serde_json::Value>,
    field: &str,
    max_chars: usize,
) -> Result<String, String> {
    let text = value
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{field} 必须是字符串"))?
        .trim()
        .to_string();
    if text.chars().count() > max_chars {
        return Err(format!("{field} 最多允许 {max_chars} 个字符"));
    }
    Ok(text)
}

fn default_agent_spawn_policy() -> TaskPolicy {
    TaskPolicy {
        autonomy_level: "Autonomous".to_string(),
        access_profile: magi_core::AccessProfile::Restricted,
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
    let access_profile = child
        .policy_snapshot
        .as_ref()
        .map(magi_core::TaskPolicy::effective_access_profile)
        .unwrap_or_default()
        .as_str();
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
                "access_profile": access_profile,
                "dependency_ids": child.dependency_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "context_package_id": child.agent_context_package().map(|package| package.package_id.as_str()),
                "context_revision": child.agent_context_package().map(|package| package.revision),
            }),
            enqueued_at: now,
        });
}

struct AgentWaitRuntime<'a> {
    task_store: &'a TaskStore,
    spawn_graph: &'a Mutex<magi_spawn_graph::SpawnGraph>,
    conversation_registry: &'a ConversationRegistry,
    session_id: &'a SessionId,
    session_threads: &'a [ExecutionThread],
}

fn execute_agent_wait_with_runtime(
    runtime: AgentWaitRuntime<'_>,
    parent_task: &magi_core::Task,
    tool: magi_tool_runtime::BuiltinToolName,
    parsed: &serde_json::Value,
) -> (String, ExecutionResultStatus) {
    let AgentWaitRuntime {
        task_store,
        spawn_graph,
        conversation_registry,
        session_id,
        session_threads,
    } = runtime;
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
        let runtime_signals =
            conversation_registry.drain_task_signals(session_id, &parent_task.task_id);
        if !runtime_signals.is_empty() {
            return (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "attention_required",
                    "timed_out": false,
                    "pending_task_ids": task_ids.iter().filter_map(|task_id| {
                        task_store.get_task(task_id).and_then(|task| {
                            matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
                                .then(|| task_id.to_string())
                        })
                    }).collect::<Vec<_>>(),
                    "runtime_signals": runtime_signals,
                    "instruction": "代理等待期间收到运行时消息。若为 agent_context_request，请先读取 child_task_id 和 request，使用 agent_send 回复对应代理，再继续 agent_wait。",
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            );
        }
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

#[cfg(test)]
fn execute_agent_wait(
    task_store: &TaskStore,
    spawn_graph: &Mutex<magi_spawn_graph::SpawnGraph>,
    parent_task: &magi_core::Task,
    session_threads: &[ExecutionThread],
    tool: magi_tool_runtime::BuiltinToolName,
    parsed: &serde_json::Value,
) -> (String, ExecutionResultStatus) {
    let registry = ConversationRegistry::new();
    let session_id = SessionId::new("session-agent-wait-test");
    registry.open_task_signal_channel(&session_id, &parent_task.task_id);
    execute_agent_wait_with_runtime(
        AgentWaitRuntime {
            task_store,
            spawn_graph,
            conversation_registry: &registry,
            session_id: &session_id,
            session_threads,
        },
        parent_task,
        tool,
        parsed,
    )
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
    plan_store: &magi_plan::PlanStore,
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
    // S9：UpdatePlan 同样在此层拦截，因为它要操作 session 维度的 PlanStore。
    // S10：MemoryWrite 同样在此层拦截，因为它要操作 workspace 维度的 ProjectMemoryStore。
    if let Some(canonical) =
        magi_tool_runtime::BuiltinToolName::from_name(tool_call.function.name.as_str())
        && !task_can_see_builtin_tool(Some(task), Some(agent_role_registry), canonical)
    {
        let decision = task_tool_visibility_decision_payload(canonical.as_str(), task);
        return (decision.payload, decision.status);
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
        magi_tool_runtime::BuiltinToolName::from_name(tool_call.function.name.as_str())
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
                task.policy_snapshot
                    .as_ref()
                    .map(|policy| policy.access_profile)
                    .unwrap_or_default(),
                canonical,
                &tool_call.function.arguments,
            );
        }
        if matches!(
            canonical,
            magi_tool_runtime::BuiltinToolName::AgentSpawn
                | magi_tool_runtime::BuiltinToolName::AgentSend
                | magi_tool_runtime::BuiltinToolName::AgentWait
        ) {
            return execute_coordinator_tool(
                CoordinatorToolContext {
                    event_bus,
                    agent_role_registry,
                    task_store,
                    session_store,
                    execution_registry,
                    conversation_registry,
                    spawn_graph,
                    plan_store,
                    task,
                    session_id,
                    workspace_id,
                },
                canonical,
                tool_call,
            );
        }
        if matches!(
            canonical,
            magi_tool_runtime::BuiltinToolName::ContextSearch
                | magi_tool_runtime::BuiltinToolName::ContextRead
                | magi_tool_runtime::BuiltinToolName::ContextRequest
        ) {
            return execute_agent_context_tool(
                task_store,
                session_store,
                conversation_registry,
                task,
                session_id,
                canonical,
                &tool_call.function.arguments,
            );
        }
        if matches!(canonical, magi_tool_runtime::BuiltinToolName::UpdatePlan) {
            return magi_plan::execute_update_plan_tool(
                event_bus,
                plan_store,
                session_id,
                workspace_id.as_ref(),
                Some(&task.task_id),
                Some(&task.mission_id),
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
        .map(magi_core::TaskPolicy::effective_access_profile)
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

    if let Some(result) = registry.execute_external_mcp_tool(
        &tool_call.function.name,
        &tool_call.function.arguments,
        access_profile,
    ) {
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
    let mut tool_policy = tool_execution_policy_scope(
        policy.access_profile,
        policy.command_mode.clone(),
        &policy.allowed_paths,
        &policy.denied_paths,
    );
    tool_policy.read_only_paths = policy.read_only_paths.clone();
    tool_policy
}

fn apply_task_policy_scope(
    tool_policy: &mut ToolExecutionPolicy,
    policy_snapshot: Option<&TaskPolicy>,
) {
    if let Some(policy) = policy_snapshot {
        tool_policy.access_profile = policy.access_profile;
        tool_policy.allowed_paths = policy.allowed_paths.clone();
        tool_policy.denied_paths = policy.denied_paths.clone();
        tool_policy.read_only_paths = policy.read_only_paths.clone();
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
        .map(magi_core::TaskPolicy::effective_access_profile)
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

    access_profile_tool_decision(AccessProfileToolDecisionInput {
        access_profile: policy_snapshot.access_profile,
        command_mode: &policy_snapshot.command_mode,
        allowed_tools: &policy_snapshot.allowed_tools,
        denied_tools: &policy_snapshot.denied_tools,
        allowed_paths: &policy_snapshot.allowed_paths,
        denied_paths: &policy_snapshot.denied_paths,
        read_only_paths: &policy_snapshot.read_only_paths,
        requested_tool_name,
        arguments,
        workspace_root_path,
    })
}

pub(crate) struct AccessProfileToolDecisionInput<'a> {
    pub access_profile: magi_core::AccessProfile,
    pub command_mode: &'a str,
    pub allowed_tools: &'a [String],
    pub denied_tools: &'a [String],
    pub allowed_paths: &'a [String],
    pub denied_paths: &'a [String],
    pub read_only_paths: &'a [String],
    pub requested_tool_name: &'a str,
    pub arguments: &'a str,
    pub workspace_root_path: Option<&'a PathBuf>,
}

pub(crate) fn access_profile_tool_decision(
    input: AccessProfileToolDecisionInput<'_>,
) -> Option<ToolPreflightDecision> {
    let AccessProfileToolDecisionInput {
        access_profile,
        command_mode,
        allowed_tools,
        denied_tools,
        allowed_paths,
        denied_paths,
        read_only_paths,
        requested_tool_name,
        arguments,
        workspace_root_path,
    } = input;
    let effective_access_profile = access_profile.constrained_by_command_mode(command_mode);
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
            effective_access_profile,
            allowed_paths,
            workspace_root_path.map(|path| path.as_path()),
        ),
        denied_paths: normalize_tool_policy_paths(
            denied_paths,
            workspace_root_path.map(|path| path.as_path()),
        ),
        command_mode: command_mode.to_string(),
    };
    let engine = builtin_permission_engine();
    let is_write_tool = BuiltinToolName::from_name(canonical_tool_name.as_str())
        .is_some_and(|tool| tool.is_access_profile_write_operation());
    let mut pending_decision = None;

    let tool_request = magi_permissions::PermissionRequest::ToolInvocation {
        tool_name: canonical_tool_name.as_str(),
        is_write_tool,
    };
    if let Some(decision) = select_access_profile_axis_decision(
        &mut pending_decision,
        permission_decision_payload(
            &canonical_tool_name,
            engine.decide(&tool_request, &canonical_policy, effective_access_profile),
            effective_access_profile,
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
                engine.decide(&shell_request, &canonical_policy, effective_access_profile),
                effective_access_profile,
            ),
        ) {
            return Some(decision);
        }
    }
    let normalized_read_only_paths = normalize_tool_policy_paths(
        read_only_paths,
        workspace_root_path.map(|path| path.as_path()),
    );
    for path_request in tool_path_access_requests(
        &canonical_tool_name,
        arguments,
        workspace_root_path.map(|path| path.as_path()),
        effective_access_profile,
    ) {
        if path_request.kind == magi_permissions::PathAccessKind::Write
            && normalized_read_only_paths
                .iter()
                .any(|root| path_request.absolute_path.starts_with(root))
        {
            return Some(task_policy_decision_payload(
                &canonical_tool_name,
                ExecutionResultStatus::Rejected,
                "上下文引用只允许读取".to_string(),
                Some(effective_access_profile),
            ));
        }
        let path_request = magi_permissions::PermissionRequest::PathAccess {
            absolute_path: path_request.absolute_path.as_path(),
            kind: path_request.kind,
        };
        if let Some(decision) = select_access_profile_axis_decision(
            &mut pending_decision,
            permission_decision_payload(
                &canonical_tool_name,
                engine.decide(&path_request, &canonical_policy, effective_access_profile),
                effective_access_profile,
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
        ExecutionResultStatus::Rejected => (
            "tool_policy_rejected",
            if access_profile == Some(magi_core::AccessProfile::FullAccess) {
                TOOL_POLICY_CONTEXT_REJECTED_PUBLIC_ERROR
            } else {
                "该工具在当前访问模式下不可用"
            },
        ),
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
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
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

    struct SnapshotReconcileProbeTool {
        name: &'static str,
        snapshot: Arc<SnapshotSession>,
    }

    struct PanicBuiltinTool {
        name: &'static str,
    }

    struct CountingBuiltinTool {
        name: &'static str,
        executions: Arc<AtomicUsize>,
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
            _tool_call_id: &ToolCallId,
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
            _tool_call_id: &ToolCallId,
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

    impl magi_tool_runtime::BuiltinTool for CountingBuiltinTool {
        fn name(&self) -> &'static str {
            self.name
        }

        fn execute(
            &self,
            _tool_call_id: &ToolCallId,
            input: &str,
            _context: &ToolExecutionContext,
            _resources: &magi_tool_runtime::ToolRuntimeResources,
        ) -> String {
            self.executions.fetch_add(1, Ordering::SeqCst);
            serde_json::json!({
                "tool": self.name,
                "status": "succeeded",
                "input": serde_json::from_str::<serde_json::Value>(input).unwrap_or_default(),
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

    #[test]
    fn idempotent_read_tools_execute_once_and_reuse_results_across_model_rounds() {
        let event_bus = InMemoryEventBus::new(16);
        let task_store = TaskStore::new();
        let session_store = SessionStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let conversation_registry = ConversationRegistry::new();
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let plan_store = crate::test_plan_store("test-idempotent-tool-reuse");
        let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
        let executions = Arc::new(AtomicUsize::new(0));
        let mut tool_registry = ToolRegistry::new(
            Arc::new(magi_governance::GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_builtin(Arc::new(CountingBuiltinTool {
            name: BuiltinToolName::WebSearch.as_str(),
            executions: executions.clone(),
        }));
        let task = test_task(
            "task-idempotent-tool-reuse",
            "task-idempotent-tool-reuse",
            None,
        );
        let session_id = SessionId::new("session-idempotent-tool-reuse");
        let workspace_id = Some(WorkspaceId::new("workspace-idempotent-tool-reuse"));
        let mut ledger = ToolExecutionLedger::default();
        let first_round_calls = vec![
            ChatToolCall {
                id: "call-web-search-1".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: BuiltinToolName::WebSearch.as_str().to_string(),
                    arguments: r#"{"query":"Magi","locale":"zh-CN"}"#.to_string(),
                },
            },
            ChatToolCall {
                id: "call-web-search-2".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: BuiltinToolName::WebSearch.as_str().to_string(),
                    arguments: r#"{"locale":"zh-CN","query":"Magi"}"#.to_string(),
                },
            },
        ];

        let first_results = execute_task_tool_call_batch(
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
            &plan_store,
            None,
            &task,
            &session_id,
            &workspace_id,
            None,
            None,
            &first_round_calls,
            &mut ledger,
            None,
            None,
        );

        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert!(
            first_results
                .iter()
                .all(|(_, status)| *status == ExecutionResultStatus::Succeeded)
        );
        let reused: serde_json::Value =
            serde_json::from_str(&first_results[1].0).expect("reused result should be json");
        assert_eq!(reused["execution"], "reused");

        let second_round_call = ChatToolCall {
            id: "call-web-search-3".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::WebSearch.as_str().to_string(),
                arguments: r#"{"query":"Magi","locale":"zh-CN"}"#.to_string(),
            },
        };
        let second_results = execute_task_tool_call_batch(
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
            &plan_store,
            None,
            &task,
            &session_id,
            &workspace_id,
            None,
            None,
            &[second_round_call],
            &mut ledger,
            None,
            None,
        );

        assert_eq!(executions.load(Ordering::SeqCst), 1);
        let reused: serde_json::Value =
            serde_json::from_str(&second_results[0].0).expect("reused result should be json");
        assert_eq!(reused["execution"], "reused");
    }

    #[test]
    fn explicit_single_tool_budget_prevents_different_read_calls() {
        let event_bus = InMemoryEventBus::new(16);
        let task_store = TaskStore::new();
        let session_store = SessionStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let conversation_registry = ConversationRegistry::new();
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let plan_store = crate::test_plan_store("test-explicit-tool-budget");
        let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
        let executions = Arc::new(AtomicUsize::new(0));
        let mut tool_registry = ToolRegistry::new(
            Arc::new(magi_governance::GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_builtin(Arc::new(CountingBuiltinTool {
            name: BuiltinToolName::WebSearch.as_str(),
            executions: executions.clone(),
        }));
        let task = test_task(
            "task-explicit-tool-budget",
            "task-explicit-tool-budget",
            None,
        );
        let session_id = SessionId::new("session-explicit-tool-budget");
        let workspace_id = Some(WorkspaceId::new("workspace-explicit-tool-budget"));
        let mut ledger = ToolExecutionLedger::for_task_goal("请只调用一次 web_search。");

        let first_results = execute_task_tool_call_batch(
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
            &plan_store,
            None,
            &task,
            &session_id,
            &workspace_id,
            None,
            None,
            &[ChatToolCall {
                id: "call-web-search-budget-1".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: BuiltinToolName::WebSearch.as_str().to_string(),
                    arguments: r#"{"query":"Magi"}"#.to_string(),
                },
            }],
            &mut ledger,
            None,
            None,
        );
        assert_eq!(first_results[0].1, ExecutionResultStatus::Succeeded);

        let second_results = execute_task_tool_call_batch(
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
            &plan_store,
            None,
            &task,
            &session_id,
            &workspace_id,
            None,
            None,
            &[ChatToolCall {
                id: "call-web-search-budget-2".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: BuiltinToolName::WebSearch.as_str().to_string(),
                    arguments: r#"{"query":"Grok"}"#.to_string(),
                },
            }],
            &mut ledger,
            None,
            None,
        );

        assert_eq!(executions.load(Ordering::SeqCst), 1);
        let skipped: serde_json::Value =
            serde_json::from_str(&second_results[0].0).expect("budget result should be json");
        assert_eq!(skipped["execution"], "skipped");
        assert_eq!(skipped["reason"], "tool_call_budget_exhausted");
    }

    fn read_only_agent_spawn_policy() -> TaskPolicy {
        let mut policy = default_agent_spawn_policy();
        policy.access_profile = magi_core::AccessProfile::ReadOnly;
        policy.command_mode = "read_only".to_string();
        policy
    }

    #[test]
    fn agent_spawn_child_policy_preserves_parent_full_access() {
        let parent = default_agent_spawn_policy();
        let child = agent_spawn_child_policy_snapshot(Some(&parent));

        assert_eq!(child.access_profile, parent.access_profile);
        assert_eq!(child.command_mode, parent.command_mode);
        assert_eq!(child.network_mode, parent.network_mode);
        assert_eq!(child.task_tier, parent.task_tier);

        let mut full_access_parent = default_agent_spawn_policy();
        full_access_parent.access_profile = magi_core::AccessProfile::FullAccess;
        let full_access_child = agent_spawn_child_policy_snapshot(Some(&full_access_parent));
        assert_eq!(
            full_access_child.access_profile,
            magi_core::AccessProfile::FullAccess
        );
        assert_eq!(full_access_child.command_mode, "full");
    }

    #[test]
    fn agent_spawn_child_policy_never_escalates_parent_read_only() {
        let parent = read_only_agent_spawn_policy();

        let child = agent_spawn_child_policy_snapshot(Some(&parent));

        assert_eq!(child.access_profile, magi_core::AccessProfile::ReadOnly);
        assert_eq!(child.command_mode, "read_only");
    }

    #[test]
    fn agent_spawn_child_policy_inherits_parent_reference_boundaries() {
        let mut parent = default_agent_spawn_policy();
        parent.allowed_tools = vec!["file_read".to_string(), "shell_exec".to_string()];
        parent.denied_tools = vec!["memory_write".to_string()];
        parent.allowed_paths = vec!["/tmp/workspace".to_string(), "/tmp/reference".to_string()];
        parent.denied_paths = vec!["/tmp/workspace/private".to_string()];
        parent.read_only_paths = vec!["/tmp/reference".to_string()];
        parent.network_mode = "disabled".to_string();
        parent.command_mode = "read_only".to_string();

        let child = agent_spawn_child_policy_snapshot(Some(&parent));

        assert_eq!(child.allowed_tools, parent.allowed_tools);
        assert_eq!(child.denied_tools, parent.denied_tools);
        assert_eq!(child.allowed_paths, parent.allowed_paths);
        assert_eq!(child.denied_paths, parent.denied_paths);
        assert_eq!(child.read_only_paths, parent.read_only_paths);
        assert_eq!(child.network_mode, parent.network_mode);
        assert_eq!(child.command_mode, parent.command_mode);
    }

    #[test]
    fn agent_spawn_context_package_is_structured_and_does_not_inherit_parent_refs() {
        let parent = test_task("task-agent-context-parent", "task-agent-context-root", None);
        let parsed = serde_json::json!({
            "context_package": {
                "summary": "修复会话同步",
                "constraints": ["不得跨 workspace 读取 session"],
                "expected_output": "给出根因和测试结果",
                "references": [{
                    "kind": "task_evidence",
                    "title": "bootstrap 证据",
                    "source_ref": "task:task-agent-context-parent:evidence:0",
                    "preview": "bootstrap 只接受后端 session"
                }]
            }
        });

        let package = parse_agent_context_package(&parsed, &parent.task_id, UtcMillis(100), 7)
            .expect("结构化上下文包应解析成功");

        assert_eq!(package.parent_task_id, parent.task_id);
        assert_eq!(package.revision, 1);
        assert_eq!(package.references.len(), 1);
        assert_eq!(
            package.references[0].source_ref,
            "task:task-agent-context-parent:evidence:0"
        );
        assert!(package.render_for_prompt().contains("context_read"));
    }

    #[test]
    fn agent_spawn_context_package_accepts_model_json_string_encoding() {
        let parent = test_task(
            "task-agent-context-string-parent",
            "task-agent-context-string-root",
            None,
        );
        let package = serde_json::json!({
            "summary": "检查配置文件",
            "constraints": ["只读，不修改文件"],
            "expected_output": "明确报告文件是否存在",
            "references": []
        });
        let parsed = serde_json::json!({
            "context_package": serde_json::to_string(&package)
                .expect("context package JSON should serialize")
        });

        let parsed_package =
            parse_agent_context_package(&parsed, &parent.task_id, UtcMillis(101), 8).expect(
                "JSON-string encoded model argument should restore to a structured package",
            );

        assert_eq!(parsed_package.summary, "检查配置文件");
        assert_eq!(parsed_package.constraints, vec!["只读，不修改文件"]);
        assert_eq!(parsed_package.references.len(), 0);
    }

    #[test]
    fn agent_spawn_context_package_rejects_non_string_reference_title() {
        let parent = test_task(
            "task-agent-context-invalid-reference-parent",
            "task-agent-context-invalid-reference-root",
            None,
        );
        let parsed = serde_json::json!({
            "context_package": {
                "summary": "检查派发参数",
                "constraints": ["只读"],
                "expected_output": "返回校验结论",
                "references": [{
                    "kind": "file",
                    "title": { "label": "Cargo.toml" },
                    "source_ref": "path:Cargo.toml"
                }]
            }
        });

        let error = parse_agent_context_package(&parsed, &parent.task_id, UtcMillis(102), 9)
            .expect_err("引用 title 不是字符串时必须拒绝创建代理");

        assert_eq!(error, "spawn.references[0].title 必须是字符串");
    }

    #[test]
    fn agent_spawn_context_package_derives_missing_reference_title_from_source() {
        let parent = test_task(
            "task-agent-context-derived-title-parent",
            "task-agent-context-derived-title-root",
            None,
        );
        let parsed = serde_json::json!({
            "context_package": {
                "summary": "检查派发参数",
                "constraints": ["只读"],
                "expected_output": "返回校验结论",
                "references": [{
                    "kind": "file",
                    "source_ref": "path:Cargo.toml"
                }]
            }
        });

        let package = parse_agent_context_package(&parsed, &parent.task_id, UtcMillis(103), 10)
            .expect("缺少可选展示标题时应从 source_ref 派生");

        assert_eq!(package.references[0].title, "path:Cargo.toml");
    }

    #[test]
    fn agent_spawn_failure_contract_preserves_safe_diagnosis() {
        let tool_call = ChatToolCall {
            id: "call-agent-spawn-invalid-context".to_string(),
            kind: "function".to_string(),
            function: magi_bridge_client::ChatToolFunction {
                name: "agent_spawn".to_string(),
                arguments: "{}".to_string(),
            },
        };

        let payload: serde_json::Value = serde_json::from_str(&agent_spawn_failure_payload(
            &tool_call,
            "failed",
            "invalid_context_package",
            "input_validation",
            "spawn.references[0].title 必须是字符串",
            "请按 Schema 修正引用字段后重试。",
        ))
        .expect("failure payload should be JSON");

        assert_eq!(payload["status"], "failed");
        assert_eq!(payload["error_code"], "invalid_context_package");
        assert_eq!(payload["failure_stage"], "input_validation");
        assert_eq!(
            payload["diagnostic_ref"],
            "tool_call:call-agent-spawn-invalid-context"
        );
        assert_eq!(payload["child_task_id"], serde_json::Value::Null);
    }

    #[test]
    fn read_only_agent_policy_rejects_write_tool() {
        let mut task = test_task("task-read-only-agent", "task-read-only-agent", None);
        task.policy_snapshot = Some(agent_spawn_child_policy_snapshot(Some(
            &read_only_agent_spawn_policy(),
        )));

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
    fn read_only_policy_allows_internal_coordination_but_rejects_project_memory_write() {
        let mut task = test_task(
            "task-read-only-state-tools",
            "task-read-only-state-tools",
            None,
        );
        task.policy_snapshot = Some(agent_spawn_child_policy_snapshot(Some(
            &read_only_agent_spawn_policy(),
        )));

        for tool in [
            BuiltinToolName::AgentSpawn,
            BuiltinToolName::CreateGoal,
            BuiltinToolName::UpdateGoal,
            BuiltinToolName::UpdatePlan,
        ] {
            assert!(
                task_policy_tool_decision(&task, tool.as_str(), "{}").is_none(),
                "只读访问只限制外部副作用，内部协调工具 {} 应保持可用",
                tool.as_str()
            );
        }

        let decision = task_policy_tool_decision(
            &task,
            BuiltinToolName::MemoryWrite.as_str(),
            r#"{"action":"save"}"#,
        )
        .expect("project memory write should remain blocked in read-only mode");
        assert_eq!(decision.status, ExecutionResultStatus::Rejected);
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
    fn task_runtime_allows_update_plan_in_read_only_mode() {
        let event_bus = InMemoryEventBus::new(16);
        let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();
        let task_store = TaskStore::new();
        let session_store = SessionStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let conversation_registry = ConversationRegistry::new();
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let session_id = SessionId::new("session-read-only-state-tool");
        let workspace_id = Some(WorkspaceId::new("workspace-read-only-state-tool"));
        session_store
            .create_session(session_id.clone(), "read only internal state")
            .expect("session should exist");
        let plan_store = magi_plan::PlanStore::from_store(&session_store, session_id.clone());
        let mut task = coordinator_task(test_task(
            "task-read-only-state-tool",
            "task-read-only-state-tool",
            None,
        ));
        let mut policy = default_agent_spawn_policy();
        policy.access_profile = magi_core::AccessProfile::ReadOnly;
        task.policy_snapshot = Some(policy);
        let tool_call = ChatToolCall {
            id: "tool-call-read-only-plan-update".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::UpdatePlan.as_str().to_string(),
                arguments: serde_json::json!({
                    "expectedRevision": 0,
                    "language": "zh-CN",
                    "plan": [
                        {
                            "itemId": "read-only-analysis",
                            "step": "保持只读任务进度可见",
                            "status": "in_progress"
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
            &plan_store,
            None,
            &task,
            &session_id,
            &workspace_id,
            None,
            None,
            &tool_call,
        );
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("plan payload json");

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        assert_eq!(parsed["status"].as_str(), Some("succeeded"));
        assert!(!plan_store.is_empty(), "只读访问不能阻止会话内部计划更新");
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
        let plan_store = crate::test_plan_store("test-plan");
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
                &plan_store,
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
                "token_budget": null,
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
        task.policy_snapshot = Some(agent_spawn_child_policy_snapshot(Some(
            &read_only_agent_spawn_policy(),
        )));

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
    fn read_only_agent_policy_allows_compound_repository_inspection_with_remote_query() {
        let mut task = test_task(
            "task-read-only-shell-repository-inspection",
            "task-read-only-shell-repository-inspection",
            None,
        );
        task.policy_snapshot = Some(agent_spawn_child_policy_snapshot(Some(
            &read_only_agent_spawn_policy(),
        )));
        let arguments = serde_json::json!({
            "access_mode": "read_only",
            "command": "ls -la && if [ -f README.md ]; then echo README; else echo NO_README; fi; if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then git remote -v 2>/dev/null | head -5; git log --oneline -5 2>/dev/null; else echo NOT_GIT_WORKTREE; fi"
        })
        .to_string();

        let decision =
            task_policy_tool_decision(&task, BuiltinToolName::ShellExec.as_str(), &arguments);

        assert!(decision.is_none(), "只读仓库探查不应在对话预检层被拒绝");
    }

    #[test]
    fn full_access_policy_allows_search_patterns_named_after_write_commands() {
        let mut task = test_task(
            "task-full-access-shell-search-pattern",
            "task-full-access-shell-search-pattern",
            None,
        );
        let mut policy = default_agent_spawn_policy();
        policy.access_profile = magi_core::AccessProfile::FullAccess;
        policy.command_mode = "full".to_string();
        task.policy_snapshot = Some(policy);
        let arguments = serde_json::json!({
            "access_mode": "read_only",
            "command": "grep -rn \"compress\\|truncate\" crates/magi-context-runtime crates/magi-conversation-runtime --include='*.rs' 2>/dev/null | head -40"
        })
        .to_string();

        let decision =
            task_policy_tool_decision(&task, BuiltinToolName::ShellExec.as_str(), &arguments);

        assert!(
            decision.is_none(),
            "检索参数中的写命令名称不应被误判为实际写操作"
        );
    }

    #[test]
    fn full_access_policy_reclassifies_misdeclared_shell_without_rejecting_it() {
        let mut task = test_task(
            "task-full-access-shell-reclassification",
            "task-full-access-shell-reclassification",
            None,
        );
        let mut policy = default_agent_spawn_policy();
        policy.access_profile = magi_core::AccessProfile::FullAccess;
        policy.command_mode = "full".to_string();
        task.policy_snapshot = Some(policy);

        let decision = task_policy_tool_decision(
            &task,
            BuiltinToolName::ShellExec.as_str(),
            r#"{"command":"touch created.txt","access_mode":"read_only"}"#,
        );

        assert!(
            decision.is_none(),
            "完全访问不应因模型错误声明 read_only 而拒绝 shell"
        );
    }

    #[test]
    fn full_access_task_constraint_is_not_reported_as_access_mode_failure() {
        let mut task = test_task(
            "task-full-access-explicit-tool-denial",
            "task-full-access-explicit-tool-denial",
            None,
        );
        let mut policy = default_agent_spawn_policy();
        policy.access_profile = magi_core::AccessProfile::FullAccess;
        policy
            .denied_tools
            .push(BuiltinToolName::FileRead.as_str().to_string());
        task.policy_snapshot = Some(policy);

        let decision = task_policy_tool_decision(
            &task,
            BuiltinToolName::FileRead.as_str(),
            r#"{"path":"README.md"}"#,
        )
        .expect("explicit task constraint should reject the tool");
        let payload: serde_json::Value =
            serde_json::from_str(&decision.payload).expect("decision should be json");

        assert_eq!(decision.status, ExecutionResultStatus::Rejected);
        assert_eq!(
            payload["error"].as_str(),
            Some(TOOL_POLICY_CONTEXT_REJECTED_PUBLIC_ERROR)
        );
        assert_ne!(
            payload["error"].as_str(),
            Some("该工具在当前访问模式下不可用")
        );
    }

    #[test]
    fn restricted_policy_reclassifies_misdeclared_shell_as_needs_approval() {
        let mut task = test_task(
            "task-restricted-shell-reclassification",
            "task-restricted-shell-reclassification",
            None,
        );
        task.policy_snapshot = Some(default_agent_spawn_policy());

        let decision = task_policy_tool_decision(
            &task,
            BuiltinToolName::ShellExec.as_str(),
            r#"{"command":"touch blocked.txt","access_mode":"read_only"}"#,
        )
        .expect("受限访问应把错误只读声明升级为审批，而不是直接拒绝");

        assert_eq!(decision.status, ExecutionResultStatus::NeedsApproval);
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

        let decision = access_profile_tool_decision(AccessProfileToolDecisionInput {
            access_profile: magi_core::AccessProfile::Restricted,
            command_mode: "",
            allowed_tools: &[],
            denied_tools: &[],
            allowed_paths: &[],
            denied_paths: &[],
            read_only_paths: &[],
            requested_tool_name: BuiltinToolName::ShellExec.as_str(),
            arguments: r#"{"command":"printf restricted > out.txt"}"#,
            workspace_root_path: Some(&link_root),
        })
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
    fn read_only_command_mode_constrains_full_access_preflight_scope() {
        let workspace = tempdir().expect("workspace tempdir");
        let workspace_root = workspace.path().to_path_buf();
        let outside_path = workspace_root
            .parent()
            .expect("workspace should have parent")
            .join("magi-command-mode-outside-target.txt");
        let mut task = test_task(
            "task-command-mode-read-only-scope",
            "task-command-mode-read-only-scope",
            None,
        );
        let mut policy = default_agent_spawn_policy();
        policy.access_profile = magi_core::AccessProfile::FullAccess;
        policy.command_mode = "read_only".to_string();
        task.policy_snapshot = Some(policy);

        let decision = task_policy_tool_decision_with_workspace_root(
            &task,
            BuiltinToolName::FileRead.as_str(),
            &serde_json::json!({
                "path": outside_path.display().to_string()
            })
            .to_string(),
            Some(&workspace_root),
        )
        .expect("read_only command mode should keep reads inside workspace scope");
        let payload: serde_json::Value =
            serde_json::from_str(&decision.payload).expect("decision should be json");

        assert_eq!(decision.status, ExecutionResultStatus::Rejected);
        assert_eq!(payload["access_profile"].as_str(), Some("read_only"));
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
    fn task_preflight_uses_effective_read_only_command_mode_for_safety_gate() {
        let mut task = test_task(
            "task-effective-read-only-safety",
            "task-effective-read-only-safety",
            None,
        );
        let mut policy = default_agent_spawn_policy();
        policy.access_profile = magi_core::AccessProfile::FullAccess;
        policy.command_mode = "read_only".to_string();
        task.policy_snapshot = Some(policy);
        let gate = magi_safety_gate::SafetyGate::new(vec![magi_safety_gate::SafetyRule::new(
            "deploy-prod",
            magi_safety_gate::SafetyCategory::Custom,
        )]);

        let decision = task_tool_preflight_decision(
            &task,
            Some(&gate),
            BuiltinToolName::ShellExec.as_str(),
            r#"{"command":"printf deploy-prod","access_mode":"read_only"}"#,
            None,
        )
        .expect("effective read-only mode should not skip restricted safety rules");
        let payload: serde_json::Value =
            serde_json::from_str(&decision.payload).expect("decision should be json");

        assert_eq!(decision.status, ExecutionResultStatus::Rejected);
        assert_eq!(payload["error_code"].as_str(), Some("tool_safety_rejected"));
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
        let plan_store = crate::test_plan_store("test-plan");
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
            &plan_store,
            None,
            &task,
            &session_id,
            &workspace_id,
            Some(&dir.path().to_path_buf()),
            None,
            &[tool_call],
            &mut ToolExecutionLedger::default(),
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
        let plan_store = crate::test_plan_store("test-plan");
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
            &plan_store,
            None,
            &task,
            &session_id,
            &workspace_id,
            None,
            None,
            &[tool_call],
            &mut ToolExecutionLedger::default(),
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
        let plan_store = crate::test_plan_store("test-plan");
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
            &plan_store,
            None,
            &task,
            &session_id,
            &workspace_id,
            Some(&workspace_root),
            Some(&worker_id),
            &[tool_call],
            &mut ToolExecutionLedger::default(),
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
            &plan_store,
            None,
            &task,
            &session_id,
            &workspace_id,
            Some(&workspace_root),
            None,
            &[mainline_tool_call],
            &mut ToolExecutionLedger::default(),
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
        let plan_store = crate::test_plan_store("test-plan");
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
            &plan_store,
            None,
            &task,
            &session_id,
            &workspace_id,
            Some(&workspace_root),
            Some(&worker_id),
            &tool_calls,
            &mut ToolExecutionLedger::default(),
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
    fn agent_spawn_uses_stable_hierarchical_task_names() {
        assert!(valid_agent_task_name("windows_validation"));
        assert!(!valid_agent_task_name("Windows Validation"));
        assert_eq!(
            child_canonical_task_name("/root", "windows_validation"),
            "/root/windows_validation"
        );
        assert_eq!(
            child_canonical_task_name("/root/windows_validation", "path_test"),
            "/root/windows_validation/path_test"
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
        child.runtime_payload = TaskRuntimePayload::AgentContext {
            package: Box::new(AgentContextPackage {
                package_id: "agent-context-assignment".to_string(),
                revision: 1,
                parent_task_id: parent.task_id.clone(),
                summary: "只读检查目录".to_string(),
                constraints: vec!["不得修改文件".to_string()],
                expected_output: "返回目录清单".to_string(),
                references: Vec::new(),
                supplements: Vec::new(),
                created_at: UtcMillis(42),
                updated_at: UtcMillis(42),
            }),
            accesses: Vec::new(),
        };

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
                    signal.payload["context_package_id"],
                    "agent-context-assignment"
                );
                assert_eq!(signal.payload["context_revision"], 1);
            }
            other => panic!("expected runtime assignment message, got {other:?}"),
        }
    }

    #[test]
    fn context_search_and_read_are_scoped_and_audited() {
        let task_store = TaskStore::new();
        let session_store = SessionStore::new();
        let session_id = SessionId::new("session-context-gateway");
        session_store
            .create_session(session_id.clone(), "上下文网关")
            .expect("session should create");
        session_store.append_timeline_entry(
            session_id.clone(),
            TimelineEntryKind::UserMessage,
            "登录流程必须校验 workspaceId",
        );
        let mut task = test_task(
            "task-context-worker",
            "task-context-root",
            Some(TaskId::new("task-context-root")),
        );
        task.runtime_payload = TaskRuntimePayload::AgentContext {
            package: Box::new(AgentContextPackage {
                package_id: "agent-context-gateway".to_string(),
                revision: 1,
                parent_task_id: TaskId::new("task-context-root"),
                summary: "检查登录流程".to_string(),
                constraints: Vec::new(),
                expected_output: "返回检查结果".to_string(),
                references: Vec::new(),
                supplements: Vec::new(),
                created_at: UtcMillis(1),
                updated_at: UtcMillis(1),
            }),
            accesses: Vec::new(),
        };
        task_store.insert_task(task.clone());

        let (search_payload, search_status) = execute_context_search(
            &task_store,
            &session_store,
            &task,
            &session_id,
            BuiltinToolName::ContextSearch,
            &serde_json::json!({"query": "workspaceId"}),
        );
        assert_eq!(search_status, ExecutionResultStatus::Succeeded);
        let search: serde_json::Value = serde_json::from_str(&search_payload).unwrap();
        let reference_id = search["results"][0]["reference_id"]
            .as_str()
            .expect("search should return a scoped turn reference")
            .to_string();

        let (read_payload, read_status) = execute_context_read(
            &task_store,
            &session_store,
            &task,
            &session_id,
            BuiltinToolName::ContextRead,
            &serde_json::json!({"reference_ids": [reference_id]}),
        );
        assert_eq!(read_status, ExecutionResultStatus::Succeeded);
        assert!(read_payload.contains("登录流程必须校验 workspaceId"));
        let stored = task_store.get_task(&task.task_id).unwrap();
        assert_eq!(stored.agent_context_accesses().len(), 2);
        assert_eq!(
            stored.agent_context_accesses()[0].operation,
            AgentContextAccessOperation::Search
        );
        assert_eq!(
            stored.agent_context_accesses()[1].operation,
            AgentContextAccessOperation::Read
        );
    }

    #[test]
    fn agent_send_updates_package_revision_and_reaches_running_child() {
        let task_store = TaskStore::new();
        let registry = ConversationRegistry::new();
        let session_id = SessionId::new("session-agent-send");
        let parent = coordinator_task(test_task(
            "task-agent-send-parent",
            "task-agent-send-parent",
            None,
        ));
        let mut child = test_task(
            "task-agent-send-child",
            "task-agent-send-parent",
            Some(parent.task_id.clone()),
        );
        child.runtime_payload = TaskRuntimePayload::AgentContext {
            package: Box::new(AgentContextPackage {
                package_id: "agent-context-send".to_string(),
                revision: 1,
                parent_task_id: parent.task_id.clone(),
                summary: "初始任务".to_string(),
                constraints: Vec::new(),
                expected_output: "完成检查".to_string(),
                references: Vec::new(),
                supplements: Vec::new(),
                created_at: UtcMillis(1),
                updated_at: UtcMillis(1),
            }),
            accesses: Vec::new(),
        };
        task_store.insert_task(parent.clone());
        task_store.insert_task(child.clone());
        registry.open_task_signal_channel(&session_id, &child.task_id);

        let (payload, status) = execute_agent_send(
            &task_store,
            &registry,
            &parent,
            &session_id,
            BuiltinToolName::AgentSend,
            &serde_json::json!({
                "task_id": child.task_id,
                "message": "补充：只检查当前分支"
            }),
            &|_, _| {},
        );
        assert_eq!(status, ExecutionResultStatus::Succeeded, "{payload}");
        let stored = task_store.get_task(&child.task_id).unwrap();
        let package = stored.agent_context_package().unwrap();
        assert_eq!(package.revision, 2);
        assert_eq!(package.supplements.len(), 1);
        assert_eq!(stored.agent_context_accesses().len(), 1);
        let signals = registry.drain_task_signals(&session_id, &child.task_id);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].payload["revision"], 2);
    }

    #[test]
    fn context_request_reaches_parent_task_channel() {
        let task_store = TaskStore::new();
        let registry = ConversationRegistry::new();
        let session_id = SessionId::new("session-context-request");
        let parent = coordinator_task(test_task(
            "task-context-request-parent",
            "task-context-request-parent",
            None,
        ));
        let mut child = test_task(
            "task-context-request-child",
            "task-context-request-parent",
            Some(parent.task_id.clone()),
        );
        child.runtime_payload = TaskRuntimePayload::AgentContext {
            package: Box::new(AgentContextPackage {
                package_id: "agent-context-request".to_string(),
                revision: 1,
                parent_task_id: parent.task_id.clone(),
                summary: "检查配置".to_string(),
                constraints: Vec::new(),
                expected_output: "完成检查".to_string(),
                references: Vec::new(),
                supplements: Vec::new(),
                created_at: UtcMillis(1),
                updated_at: UtcMillis(1),
            }),
            accesses: Vec::new(),
        };
        task_store.insert_task(parent.clone());
        task_store.insert_task(child.clone());
        registry.open_task_signal_channel(&session_id, &parent.task_id);
        registry.open_task_signal_channel(&session_id, &child.task_id);

        std::thread::scope(|scope| {
            let request_handle = scope.spawn(|| {
                execute_context_request(
                    &task_store,
                    &registry,
                    &child,
                    &session_id,
                    BuiltinToolName::ContextRequest,
                    &serde_json::json!({
                        "request": "请提供模型配置来源",
                        "timeout_ms": 5000,
                    }),
                )
            });
            let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
            let (wait_payload, wait_status) = execute_agent_wait_with_runtime(
                AgentWaitRuntime {
                    task_store: &task_store,
                    spawn_graph: &spawn_graph,
                    conversation_registry: &registry,
                    session_id: &session_id,
                    session_threads: &[],
                },
                &parent,
                BuiltinToolName::AgentWait,
                &serde_json::json!({
                    "task_ids": [child.task_id],
                    "timeout_ms": 5000,
                }),
            );
            assert_eq!(wait_status, ExecutionResultStatus::Succeeded);
            let wait: serde_json::Value = serde_json::from_str(&wait_payload).unwrap();
            assert_eq!(wait["status"], "attention_required");
            assert_eq!(
                wait["runtime_signals"][0]["payload"]["type"],
                "agent_context_request"
            );

            let (send_payload, send_status) = execute_agent_send(
                &task_store,
                &registry,
                &parent,
                &session_id,
                BuiltinToolName::AgentSend,
                &serde_json::json!({
                    "task_id": child.task_id,
                    "message": "模型配置来自当前 session 的 orchestrator 设置",
                }),
                &|_, _| {},
            );
            assert_eq!(
                send_status,
                ExecutionResultStatus::Succeeded,
                "{send_payload}"
            );
            let (request_payload, request_status) = request_handle.join().unwrap();
            assert_eq!(
                request_status,
                ExecutionResultStatus::Succeeded,
                "{request_payload}"
            );
            let request: serde_json::Value = serde_json::from_str(&request_payload).unwrap();
            assert_eq!(request["status"], "resolved");
            assert_eq!(
                request["responses"][0]["payload"]["type"],
                "agent_context_supplement"
            );
        });
        assert_eq!(
            task_store
                .get_task(&child.task_id)
                .unwrap()
                .agent_context_accesses()[0]
                .operation,
            AgentContextAccessOperation::Request
        );
    }

    #[test]
    fn tool_execution_rejects_role_invisible_builtin_tools() {
        let event_bus = InMemoryEventBus::new(16);
        let task_store = TaskStore::new();
        let session_store = SessionStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let conversation_registry = ConversationRegistry::new();
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let plan_store = crate::test_plan_store("test-plan");
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
            &plan_store,
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
            &mut ToolExecutionLedger::default(),
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
