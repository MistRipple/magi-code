//! 任务系统 — tool batch / coordinator / single 工具执行入口。
//!
//! - `execute_task_tool_call_batch`：按 concurrency 分组并发或串行调度本轮工具。
//! - `execute_task_tool_call`：单工具入口，按 BuiltinToolName 走 coordinator/写工具/policy/
//!   safety gate/tool registry 各分支。
//! - `execute_coordinator_tool`：协调器工具（agent_spawn / agent_wait）入口。
//! - `task_policy_tool_decision` / `safety_gate_tool_decision` 等支撑判定。

use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
};

use magi_app_server_protocol as execution_steps_protocol;
use magi_bridge_client::{
    ChatToolCall,
    tool_concurrency::{ToolBatchKind, ToolConcurrencyInput, partition_tool_calls_with_inputs},
};
use magi_browser_authority::BrowserCapabilitySnapshot;
use magi_core::{
    AccessProfile, AgentContextAccessOperation, AgentContextAccessRecord, AgentContextReference,
    AgentContextReferenceKind, AgentContextSupplement, EventId, ExecutionResultStatus, GoalId,
    SessionId, TaskExecutorBinding, TaskId, TaskPolicy, TaskRuntimePayload, TaskStatus, ToolCallId,
    UtcMillis, WorkspaceId, estimate_text_tokens,
};
#[cfg(test)]
use magi_core::{AgentContextPackage, TaskTier};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::{
    GoalRevisionExpectation, GoalStatus, SessionStore, TimelineEntryKind,
    timeline_entry_visible_text,
};
#[cfg(test)]
use magi_snapshot::SnapshotSession;
use magi_snapshot::{ToolHook, ToolHookCtx};
use magi_tool_runtime::{
    BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy,
    ToolExecutionProgress, ToolRegistry, builtin_permission_engine, canonical_builtin_tool_name,
    effective_tool_policy_allowed_paths, normalize_tool_policy_paths, tool_path_access_requests,
};

use crate::agent_spawn_preflight::{
    AGENT_CONTEXT_PREVIEW_MAX_CHARS, AGENT_CONTEXT_SUMMARY_MAX_CHARS,
    AGENT_SPAWN_DUPLICATE_INSTRUCTION, AgentSpawnPreflightInput, parse_agent_context_references,
    preflight_agent_spawn, required_bounded_context_text,
};
use crate::builtin_tool_schema::internal_builtin_tool_rejection_payload;
use crate::goal_tool_failure;
use crate::session_writeback::publish_committed_canonical_item_event;
use crate::skill_apply_tool::{SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime};
use crate::task_execution_registry::SpawnedChildExecutionError;
use crate::tool_execution_ledger::ToolExecutionLedger;
use crate::{
    ConversationRegistry, MailboxAuthor, MailboxKind, RuntimeSignal,
    task_execution_registry::{SpawnedChildExecutionRequest, TaskExecutionRegistry},
    task_helpers::{task_can_delegate, task_can_see_builtin_tool, task_is_coordinator},
    tool_declared_paths::{append_result_declared_paths, derive_declared_paths},
    tool_result_utils::{
        approval_resume_contract_failure, approval_resume_is_safe, safety_gate_public_error,
        tool_execution_failed_result, tool_interrupted_before_execution_result,
    },
};
use crate::{
    active_skill_tool_execution_policy, execute_skill_custom_tool, parse_skill_custom_tool_name,
    tool_execution_policy_scope,
};

/// agent_spawn 生成 child task_id 时使用的进程内单调序号。
///
/// 仅靠 `UtcMillis::now()` 在同一毫秒内的多次并行 agent_spawn 会产生重复
/// child_id，进而触发 TaskStore 的重复插入。配合毫秒时间戳一起拼到 task_id
/// 末尾，保证同一进程内绝对唯一。
static AGENT_SPAWN_SEQ: AtomicU64 = AtomicU64::new(0);
const AGENT_CONTEXT_ACCESS_LIMIT: usize = 8;
const AGENT_SPAWN_STARTED_INSTRUCTION: &str = "代理已异步启动。若后续结论依赖该代理结果，必须调用 agent_wait，并传入 task_ids=[child_task_id] 收集终态结果；不要在未等待必要代理结果时直接给最终答复。";
const AGENT_WAIT_DEFAULT_TIMEOUT_MS: u64 = 300_000;
const AGENT_WAIT_MIN_TIMEOUT_MS: u64 = 1_000;
const AGENT_WAIT_MAX_TIMEOUT_MS: u64 = 1_800_000;
const TOOL_VISIBILITY_REJECTED_PUBLIC_ERROR: &str = "该工具在当前任务角色或阶段下不可用";
const TOOL_POLICY_CONTEXT_REJECTED_PUBLIC_ERROR: &str = "该工具受当前任务角色或策略限制";
const TOOL_POLICY_NEEDS_APPROVAL_PUBLIC_ERROR: &str = "该操作需要你的确认，授权后将继续当前调用";

pub(crate) struct ToolPreflightDecision {
    pub(crate) payload: String,
    pub(crate) status: ExecutionResultStatus,
}

fn project_directory_rejection(
    registry: &TaskExecutionRegistry,
    task: &magi_core::Task,
    workspace_id: &Option<WorkspaceId>,
    tool: &str,
) -> Option<(String, ExecutionResultStatus)> {
    let error = registry
        .validate_project_directories(task, workspace_id)
        .err()?;
    Some((
        serde_json::json!({
            "tool": tool, "status": "rejected", "error_code": "project_directory_invalid",
            "execution_started": false, "error": format!("原任务的源文件夹授权已失效：{error}"),
        })
        .to_string(),
        ExecutionResultStatus::Rejected,
    ))
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
            "status": result.1.wire_label(),
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
        EventId::unique(format!(
            "event-tool-lifecycle-{}-{}",
            event_type, tool_call.id
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
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    plan_store: &magi_plan::PlanStore,
    project_memory: Option<&magi_project_memory::ProjectMemoryStore>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    workspace_root_path: Option<&PathBuf>,
    worker_id: Option<&magi_core::WorkerId>,
    browser_capability_snapshot: Option<BrowserCapabilitySnapshot>,
    tool_calls: &[ChatToolCall],
    tool_execution_ledger: &mut ToolExecutionLedger,
    on_progress: Option<&(dyn Fn(ToolExecutionProgress) -> Result<(), String> + Sync)>,
    workspace_resources: Option<crate::workspace_execution::WorkspaceExecutionResources>,
    execution_group_id: Option<String>,
    execution_allowed: &(dyn Fn() -> bool + Sync),
) -> Vec<(String, ExecutionResultStatus)> {
    if !execution_allowed() {
        return tool_calls
            .iter()
            .map(|call| tool_interrupted_before_execution_result(&call.function.name))
            .collect();
    }
    if let Some((payload, status)) =
        project_directory_rejection(execution_registry, task, workspace_id, "tool_batch")
    {
        return tool_calls
            .iter()
            .map(|call| {
                let mut value: serde_json::Value =
                    serde_json::from_str(&payload).expect("rejection JSON");
                value["tool"] = serde_json::json!(call.function.name);
                (value.to_string(), status)
            })
            .collect();
    }
    if let Some(binding) = task
        .executor_binding
        .as_ref()
        .and_then(|executor| executor.project_directories.as_ref())
    {
        let valid = workspace_resources
            .as_ref()
            .ok_or_else(|| "缺少完整项目执行映射".to_string())
            .and_then(|resources| resources.validate_for_binding(binding));
        if let Err(error) = valid {
            return tool_calls.iter().map(|call| (serde_json::json!({
                "tool":call.function.name,"status":"rejected","error_code":"project_directory_invalid",
                "execution_started":false,"error":error,
            }).to_string(),ExecutionResultStatus::Rejected)).collect();
        }
    }
    // 步骤更新要先于同批次的实际工具生效：后者按“当前 running 步骤”绑定归属。
    // 模型习惯把“进入下一步”和该步的第一个工具放在同一轮，这里拆成先后两段执行，不拒绝整批。
    let is_steps_call = |call: &ChatToolCall| call.function.name == "update_execution_steps";
    if tool_calls.iter().any(is_steps_call) && tool_calls.iter().any(|call| !is_steps_call(call)) {
        let (steps_calls, other_calls): (Vec<_>, Vec<_>) = tool_calls
            .iter()
            .cloned()
            .enumerate()
            .partition(|(_, call)| is_steps_call(call));
        let mut run = |group: &[(usize, ChatToolCall)]| {
            let calls = group
                .iter()
                .map(|(_, call)| call.clone())
                .collect::<Vec<_>>();
            execute_task_tool_call_batch(
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
                safety_gate,
                plan_store,
                project_memory,
                task,
                session_id,
                workspace_id,
                workspace_root_path,
                worker_id,
                browser_capability_snapshot.clone(),
                &calls,
                tool_execution_ledger,
                on_progress,
                workspace_resources.clone(),
                execution_group_id.clone(),
                execution_allowed,
            )
        };
        let mut merged: Vec<Option<(String, ExecutionResultStatus)>> = vec![None; tool_calls.len()];
        for group in [&steps_calls, &other_calls] {
            for ((index, _), result) in group.iter().zip(run(group)) {
                merged[*index] = Some(result);
            }
        }
        return merged
            .into_iter()
            .map(|result| result.expect("每个工具调用都有结果"))
            .collect();
    }
    tool_execution_ledger.rebind_project_directories(
        workspace_resources
            .as_ref()
            .and_then(|resources| resources.directories.as_ref()),
    );
    tool_execution_ledger.execute_batch_with(tool_calls, tool_registry, |execution_calls| {
        execute_task_tool_call_batch_unchecked(
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
            safety_gate,
            plan_store,
            project_memory,
            task,
            session_id,
            workspace_id,
            workspace_root_path,
            worker_id,
            browser_capability_snapshot.clone(),
            execution_calls,
            on_progress,
            workspace_resources.clone(),
            execution_group_id.clone(),
            execution_allowed,
        )
    })
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
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    plan_store: &magi_plan::PlanStore,
    project_memory: Option<&magi_project_memory::ProjectMemoryStore>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    workspace_root_path: Option<&PathBuf>,
    worker_id: Option<&magi_core::WorkerId>,
    browser_capability_snapshot: Option<BrowserCapabilitySnapshot>,
    tool_calls: &[ChatToolCall],
    on_progress: Option<&(dyn Fn(ToolExecutionProgress) -> Result<(), String> + Sync)>,
    workspace_resources: Option<crate::workspace_execution::WorkspaceExecutionResources>,
    execution_group_id: Option<String>,
    execution_allowed: &(dyn Fn() -> bool + Sync),
) -> Vec<(String, ExecutionResultStatus)> {
    let hook_contexts = tool_calls
        .iter()
        .map(|tool_call| ToolHookCtx {
            tool_call_id: tool_call.id.clone(),
            worker_id: worker_id.map(ToString::to_string),
            execution_group_id: execution_group_id.clone(),
            declared_paths: derive_declared_paths(tool_call),
        })
        .collect::<Vec<_>>();
    // 任务一旦进入终态（被中断为 Killed、租约失效收口为 Failed 等），执行权已不再属于
    // 本次 Runner，剩余调用不得继续产生副作用。
    let batch_execution_allowed = || {
        execution_allowed()
            && !task_store.get_task(&task.task_id).is_some_and(|current| {
                matches!(
                    current.status,
                    magi_core::TaskStatus::Completed
                        | magi_core::TaskStatus::Failed
                        | magi_core::TaskStatus::Killed
                )
            })
    };
    let results = run_tool_call_batches(tool_calls, &batch_execution_allowed, &|tool_index| {
        let mut hook_ctx = hook_contexts[tool_index].clone();
        let tool_call = &tool_calls[tool_index];
        let selected_root = ToolExecutionContext {
            workspace_id: workspace_id.clone(),
            working_directory: workspace_root_path.cloned(),
            project_directory_map: workspace_resources
                .as_ref()
                .and_then(|resources| resources.directories.clone()),
            ..Default::default()
        }
        .for_tool_arguments(&tool_call.function.arguments)
        .ok()
        .and_then(|context| context.working_directory);
        let snapshot_context = |snapshot: &magi_snapshot::SnapshotSession,
                                context: &ToolHookCtx| {
            crate::tool_declared_paths::snapshot_tool_context(
                snapshot.workspace_root(),
                selected_root.as_deref(),
                context,
            )
        };
        let snapshots = workspace_resources
            .as_ref()
            .map(|resources| {
                resources
                    .snapshots
                    .iter()
                    .filter(|snapshot| {
                        tool_call.function.name == BuiltinToolName::ShellExec.as_str()
                            || hook_ctx.declared_paths.is_empty()
                            || !snapshot_context(snapshot, &hook_ctx)
                                .declared_paths
                                .is_empty()
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for snapshot in &snapshots {
            snapshot.before_tool(&snapshot_context(snapshot, &hook_ctx));
        }
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
                    safety_gate,
                    plan_store,
                    project_memory,
                    task,
                    session_id,
                    workspace_id,
                    workspace_root_path,
                    worker_id,
                    browser_capability_snapshot.clone(),
                    tool_call,
                    on_progress,
                    execution_allowed,
                    workspace_resources.as_ref(),
                )
            },
        );
        append_result_declared_paths(&mut hook_ctx.declared_paths, &result.0);
        for snapshot in &snapshots {
            snapshot.after_tool(&snapshot_context(snapshot, &hook_ctx));
        }
        result
    });
    if let Some(resources) = workspace_resources.as_ref() {
        for snapshot in &resources.snapshots {
            if let Err(err) = snapshot.reconcile() {
                tracing::warn!(
                    session_id = %session_id.as_str(), task_id = %task.task_id.as_str(),
                    error = %err, "snapshot reconcile after task tool batch failed"
                );
            }
        }
    }
    results
}

/// 按并发分区执行一批已经通过账本规划的工具调用。
///
/// 任务轮次与对话轮次共用这里唯一的中断检查点：串行调用逐个检查，并发分区在启动前
/// 检查。`execution_allowed` 一旦返回 false，本批剩余调用全部以“中断、未执行”收口，
/// 不再产生任何副作用。`execute_one` 负责单个调用的完整生命周期（含 panic 收口）。
pub(crate) fn run_tool_call_batches(
    tool_calls: &[ChatToolCall],
    execution_allowed: &(dyn Fn() -> bool + Sync),
    execute_one: &(dyn Fn(usize) -> (String, ExecutionResultStatus) + Sync),
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
    let mut interrupted = false;

    for batch in partition_tool_calls_with_inputs(&tool_inputs) {
        match batch.kind {
            ToolBatchKind::Serial => {
                for tool_index in batch.tool_indices {
                    interrupted = interrupted || !execution_allowed();
                    results[tool_index] = Some(if interrupted {
                        tool_interrupted_before_execution_result(
                            &tool_calls[tool_index].function.name,
                        )
                    } else {
                        execute_one(tool_index)
                    });
                }
            }
            ToolBatchKind::Concurrent => {
                interrupted = interrupted || !execution_allowed();
                if interrupted {
                    for tool_index in batch.tool_indices {
                        results[tool_index] = Some(tool_interrupted_before_execution_result(
                            &tool_calls[tool_index].function.name,
                        ));
                    }
                    continue;
                }
                thread::scope(|scope| {
                    let handles = batch
                        .tool_indices
                        .iter()
                        .copied()
                        .map(|tool_index| {
                            (tool_index, scope.spawn(move || execute_one(tool_index)))
                        })
                        .collect::<Vec<_>>();
                    for (tool_index, handle) in handles {
                        let result = handle.join().unwrap_or_else(|_| {
                            tracing::warn!(
                                tool_name = %tool_calls[tool_index].function.name,
                                tool_call_id = %tool_calls[tool_index].id,
                                "tool execution thread panicked"
                            );
                            tool_execution_failed_result(&tool_calls[tool_index].function.name)
                        });
                        results[tool_index] = Some(result);
                    }
                });
            }
        }
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
    plan_store: &'a magi_plan::PlanStore,
    task: &'a magi_core::Task,
    session_id: &'a SessionId,
    workspace_id: &'a Option<WorkspaceId>,
    workspace_root_path: Option<&'a PathBuf>,
    execution_allowed: &'a (dyn Fn() -> bool + Sync),
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
    agent_spawn_failure_payload_for_existing_task(
        tool_call,
        status,
        error_code,
        failure_stage,
        error,
        instruction,
        None,
    )
}

fn agent_spawn_failure_payload_for_existing_task(
    tool_call: &ChatToolCall,
    status: &str,
    error_code: &str,
    failure_stage: &str,
    error: impl Into<String>,
    instruction: impl Into<String>,
    existing_task_id: Option<&TaskId>,
) -> String {
    // retryable 只说明可按诊断修正输入/注册冲突，不构成再次派发的决定。
    let retryable =
        existing_task_id.is_none() && matches!(failure_stage, "input_validation" | "registration");
    let instruction = format!(
        "{} 具体诊断：{}",
        magi_tool_runtime::AGENT_DELEGATION_RECOVERY_GUIDANCE,
        instruction.into()
    );
    let mut payload = serde_json::json!({
        "tool": BuiltinToolName::AgentSpawn.as_str(),
        "status": status,
        "error_code": error_code,
        "failure_stage": failure_stage,
        "retryable": retryable,
        "error": error.into(),
        "instruction": instruction,
        "diagnostic_ref": format!("tool_call:{}", tool_call.id),
        "child_task_id": null,
    });
    if let Some(existing_task_id) = existing_task_id {
        // 这是已存在任务的引用，不能伪装成本次拒绝调用创建的新 child。
        payload["existing_task_id"] = serde_json::json!(existing_task_id);
    }
    payload.to_string()
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
        plan_store,
        task,
        session_id,
        workspace_id,
        workspace_root_path,
        execution_allowed,
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
                EventId::unique(format!("event-coordinator-{kind}")),
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
        magi_tool_runtime::BuiltinToolName::AgentSpawn => execute_agent_spawn(
            CoordinatorToolContext {
                event_bus,
                agent_role_registry,
                task_store,
                session_store,
                execution_registry,
                conversation_registry,
                plan_store,
                task,
                session_id,
                workspace_id,
                workspace_root_path,
                execution_allowed,
            },
            tool_call,
            &parsed,
            &publish_event,
        ),
        magi_tool_runtime::BuiltinToolName::AgentWait => execute_agent_wait_with_runtime(
            AgentWaitRuntime {
                task_store,
                session_store,
                admission: &execution_registry.execution_admission(),
                conversation_registry,
                session_id,
                execution_allowed,
            },
            task,
            tool,
            &parsed,
        ),
        magi_tool_runtime::BuiltinToolName::AgentSend => execute_agent_send(
            task_store,
            conversation_registry,
            task,
            session_id,
            tool,
            &parsed,
            &publish_event,
        ),
        magi_tool_runtime::BuiltinToolName::AgentCancel => run_with_task_execution_wait(
            task_store,
            execution_registry,
            task,
            tool.as_str(),
            || {
                execute_agent_cancel(
                    task_store,
                    execution_registry,
                    session_store,
                    task,
                    session_id,
                    tool,
                    &parsed,
                )
            },
            execution_allowed,
        )
        .unwrap_or_else(std::convert::identity),
        _ => unreachable!("execute_coordinator_tool 只接收协调器代理工具变体"),
    }
}

fn execute_agent_spawn(
    context: CoordinatorToolContext<'_>,
    tool_call: &ChatToolCall,
    parsed: &serde_json::Value,
    publish_event: &impl Fn(&str, serde_json::Value),
) -> (String, ExecutionResultStatus) {
    let CoordinatorToolContext {
        event_bus,
        agent_role_registry,
        task_store,
        session_store,
        execution_registry,
        conversation_registry,
        plan_store,
        task,
        session_id,
        workspace_id,
        workspace_root_path,
        ..
    } = context;

    // 预检只读事实；直到它成功后才生成 child_task_id 或写入任何任务状态。
    let now = UtcMillis::now();
    let sequence = AGENT_SPAWN_SEQ.fetch_add(1, Ordering::Relaxed);
    let preflight = match preflight_agent_spawn(AgentSpawnPreflightInput {
        parsed,
        tool_call_id: &tool_call.id,
        parent_task: task,
        task_store,
        session_store,
        execution_registry,
        agent_role_registry,
        plan_store,
        session_id,
        workspace_id,
        execution_root: workspace_root_path,
        now,
        sequence,
    }) {
        Ok(preflight) => preflight,
        Err(error) => {
            tracing::info!(
                error_code = %error.error_code,
                failure_stage = %error.failure_stage,
                parent_task_id = %task.task_id,
                "agent_spawn 创建前预检拒绝"
            );
            return (
                agent_spawn_failure_payload_for_existing_task(
                    tool_call,
                    "rejected",
                    &error.error_code,
                    &error.failure_stage,
                    error.message,
                    error.instruction,
                    error.existing_task_id.as_ref(),
                ),
                ExecutionResultStatus::Rejected,
            );
        }
    };

    let child_task_id = TaskId::new(format!(
        "task-spawn-{}-{}-{}",
        task.task_id.as_str(),
        now.0,
        sequence
    ));
    let activation = magi_core::AgentActivation {
        agent_id: preflight
            .previous_activation
            .as_ref()
            .map(|source| source.agent_id.clone())
            .unwrap_or_else(|| child_task_id.clone()),
        session_id: session_id.clone(),
        lifetime: preflight.agent_lifetime.clone(),
        generation: match preflight.previous_activation.as_ref() {
            Some(source) => match source.generation.checked_add(1) {
                Some(value) => value,
                None => {
                    return context_tool_failure(
                        BuiltinToolName::AgentSpawn,
                        "agent_activation_overflow",
                        "Agent activation generation 溢出",
                    );
                }
            },
            None => 1,
        },
        previous_task_id: preflight.previous_task_id.clone(),
    };
    let context_package_id = preflight.context_package.package_id.clone();
    let context_revision = preflight.context_package.revision;
    let context_mode = if preflight.context_package.fork.is_some() {
        "fork"
    } else {
        "fresh"
    };
    let context_source = preflight.context_package.fork.as_ref().map(|snapshot| {
        serde_json::json!({
            "task_id": snapshot.source_task_id, "thread_id": snapshot.source_thread_id,
            "turn_id": snapshot.source_turn_id, "step_id": snapshot.source_step_id,
            "before_item_id": snapshot.before_item_id, "item_count": snapshot.items.len(),
        })
    });
    let plan_item_id = preflight.plan_item_id.clone();
    let child = magi_core::Task {
        task_id: child_task_id.clone(),
        mission_id: task.mission_id.clone(),
        root_task_id: task.root_task_id.clone(),
        parent_task_id: Some(task.task_id.clone()),
        kind: magi_core::TaskKind::LocalAgent,
        title: preflight.display_name.clone(),
        goal: preflight.goal.clone(),
        status: TaskStatus::Pending,
        dependency_ids: preflight.child_dependency_ids.clone(),
        required_children: Vec::new(),
        policy_snapshot: Some(preflight.child_policy_snapshot.clone()),
        executor_binding: Some(
            TaskExecutorBinding::for_role(&preflight.role)
                .with_capability_ids(preflight.capability_ids.clone())
                .with_parallelism_group(preflight.parallelism_group.clone())
                .with_canonical_task_name(preflight.canonical_task_name.clone())
                .with_plan_item_id(plan_item_id.clone()),
        ),
        completion_contract: magi_core::TaskCompletionContract::default(),
        recovery_checkpoint: None,
        knowledge_refs: Vec::new(),
        workspace_scope: task.workspace_scope.clone(),
        write_scope: task.write_scope.clone(),
        input_refs: preflight.child_input_refs.clone(),
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        runtime_payload: TaskRuntimePayload::AgentContext {
            package: Box::new(preflight.context_package.clone()),
            accesses: Vec::new(),
        },
        created_at: now,
        updated_at: now,
    };

    let mut child = child;
    child
        .executor_binding
        .as_mut()
        .expect("child executor binding")
        .project_directories = task
        .executor_binding
        .as_ref()
        .and_then(|binding| binding.project_directories.clone());
    child
        .executor_binding
        .as_mut()
        .expect("spawn binding")
        .model_selection = preflight.model_selection.clone();
    child
        .executor_binding
        .as_mut()
        .expect("spawn role binding")
        .agent_activation = Some(activation.clone());
    child
        .executor_binding
        .as_mut()
        .expect("spawn role binding")
        .delegation_allowed = preflight.delegation_allowed;
    // 初始消息必须早于 TaskStore 的可调度发布，和后续 agent_send 共用同一 FIFO。
    conversation_registry.open_task_signal_channel(session_id, &child_task_id);
    if let Err(error) = enqueue_agent_assignment_message(
        conversation_registry,
        session_id,
        task,
        &child,
        &preflight.role,
        now,
    ) {
        conversation_registry.close_task_signal_channel(session_id, &child_task_id);
        return context_tool_failure(BuiltinToolName::AgentSpawn, "agent_input_closed", error);
    }
    let registered_execution = match execution_registry.register_spawned_local_agent_child(
        SpawnedChildExecutionRequest {
            task_store,
            session_store,
            child_task: &child,
            session_id,
            workspace_id,
            role: &preflight.role,
            delegation_purpose: preflight.delegation.purpose,
            role_parallelism_limit: agent_role_registry
                .get(&preflight.role)
                .and_then(|definition| definition.parallelism_limit),
            plan_store: preflight.plan_item_id.as_ref().map(|_| plan_store),
            plan_item_id: plan_item_id.clone(),
            execution_root: preflight
                .working_dir
                .clone()
                .or_else(|| workspace_root_path.cloned()),
            inherit_skill: preflight.inherit_skill,
            now,
        },
    ) {
        Ok(registered) => registered,
        Err(error) => {
            conversation_registry.close_task_signal_channel(session_id, &child_task_id);
            tracing::warn!(
                error = %error,
                parent_task_id = %task.task_id,
                child_task_id = %child_task_id,
                "agent_spawn 子任务原子注册失败"
            );
            let (error_code, failure_stage, status, execution_status, message, instruction) =
                match error {
                    SpawnedChildExecutionError::GoalPaused(message) => (
                        "agent_goal_paused",
                        "registration",
                        "rejected",
                        ExecutionResultStatus::Rejected,
                        message,
                        "由用户显式恢复当前 Goal 后再创建新的子代理。",
                    ),
                    SpawnedChildExecutionError::RoleCapacityExceeded {
                        role,
                        active,
                        limit,
                    } => (
                        "agent_capacity_exceeded",
                        "registration",
                        "rejected",
                        ExecutionResultStatus::Rejected,
                        format!(
                            "角色 {role} 已达到代理实例上限：最多 {limit} 个活跃实例，当前 {active} 个"
                        ),
                        "请先用 agent_wait 收集该角色已启动代理的结果；有实例退出活跃状态后再继续创建同角色代理。",
                    ),
                    SpawnedChildExecutionError::SharedWorkspaceWriterActive => (
                        "agent_workspace_busy",
                        "registration",
                        "rejected",
                        ExecutionResultStatus::Rejected,
                        SpawnedChildExecutionError::SharedWorkspaceWriterActive.to_string(),
                        "本次未创建代理。请用 agent_wait 等待已有代理结束后再派发；用户明确要求多个真实代理时可以按序完成，不能用主线自述代替。角色的只读职责不等于只读授权。",
                    ),
                    SpawnedChildExecutionError::DuplicateTaskName { existing_task_id } => {
                        return (
                            agent_spawn_failure_payload_for_existing_task(
                                tool_call,
                                "rejected",
                                "duplicate_task_name",
                                "registration",
                                "同一父任务下 task_name 已存在",
                                AGENT_SPAWN_DUPLICATE_INSTRUCTION,
                                Some(&existing_task_id),
                            ),
                            ExecutionResultStatus::Rejected,
                        );
                    }
                    SpawnedChildExecutionError::DuplicateAgentWork { existing_task_id } => {
                        return (
                            agent_spawn_failure_payload_for_existing_task(
                                tool_call,
                                "rejected",
                                "duplicate_agent_work",
                                "registration",
                                "相同工作包已有活跃代理，本次未创建新代理",
                                AGENT_SPAWN_DUPLICATE_INSTRUCTION,
                                Some(&existing_task_id),
                            ),
                            ExecutionResultStatus::Rejected,
                        );
                    }
                    SpawnedChildExecutionError::InvalidState(_) => (
                        "agent_spawn_registration_failed",
                        "registration",
                        "failed",
                        ExecutionResultStatus::Failed,
                        "代理启动失败".to_string(),
                        "使用本次诊断引用定位工具调用，核对当前执行链和注册状态；不要重复相同调用。",
                    ),
                };
            return (
                agent_spawn_failure_payload(
                    tool_call,
                    status,
                    error_code,
                    failure_stage,
                    message,
                    instruction,
                ),
                execution_status,
            );
        }
    };

    if let Some(plan) = registered_execution.plan.as_ref() {
        magi_plan::publish_plan_event(
            event_bus,
            magi_plan::plan_event_type(plan),
            plan,
            workspace_id.as_ref(),
            Some(&child_task_id),
            Some(&task.mission_id),
        );
    }
    publish_event(
        "task.coordinator.agent_spawn",
        serde_json::json!({
            "delegation": preflight.delegation,
            "parent_task_id": task.task_id.to_string(),
            "child_task_id": child_task_id.to_string(),
            "agent_id": activation.agent_id,
            "activation": activation,
            "task_name": preflight.task_name.clone(),
            "canonical_task_name": preflight.canonical_task_name,
            "plan_item_id": plan_item_id.as_ref().map(ToString::to_string),
            "role": preflight.role,
            "capabilities": preflight.capability_ids,
            "access_profile": preflight.child_access_profile.as_str(),
            "goal": preflight.goal,
            "context_package_id": context_package_id,
            "context_revision": context_revision,
            "context_mode": context_mode,
            "context_source": context_source,
            "task_kind": "local_agent",
            "worker_id": registered_execution.worker_id.to_string(),
            "thread_id": registered_execution.thread_id.to_string(),
            "execution_chain_ref": registered_execution.execution_chain_ref,
            "model_source": preflight.model_source,
            "model_selection": preflight.model_selection,
            "queue_reason": registered_execution.queue_reason,
        }),
    );
    let queue_reason = registered_execution.queue_reason.clone();
    let status = if queue_reason.is_some() {
        "queued"
    } else {
        "started"
    };
    let instruction = queue_reason
        .as_ref()
        .map(|_| "代理已创建并排队等待执行资源。资源可用后会自动启动；若后续结论依赖该代理，必须调用 agent_wait 收集终态结果。".to_string())
        .unwrap_or_else(|| AGENT_SPAWN_STARTED_INSTRUCTION.to_string());
    (
        serde_json::json!({
            "tool": BuiltinToolName::AgentSpawn.as_str(),
            "status": status,
            "delegation": preflight.delegation,
            "child_task_id": child_task_id.to_string(),
            "agent_id": activation.agent_id,
            "activation": activation,
            "task_name": preflight.task_name,
            "canonical_task_name": preflight.canonical_task_name,
            "plan_item_id": plan_item_id.as_ref().map(ToString::to_string),
            "role": preflight.role,
            "capabilities": preflight.capability_ids,
            "access_profile": preflight.child_access_profile.as_str(),
            "title": child.title,
            "assignment": {
                "title": child.title,
                "goal": child.goal,
                "role": preflight.role,
                "capabilities": preflight.capability_ids,
                "access_profile": preflight.child_access_profile.as_str(),
                "context_package_id": context_package_id,
                "context_revision": context_revision,
                "context_mode": context_mode,
                "context_source": context_source,
            },
            "worker_id": registered_execution.worker_id.to_string(),
            "thread_id": registered_execution.thread_id.to_string(),
            "execution_chain_ref": registered_execution.execution_chain_ref,
            "model_selection": preflight.model_selection,
            "model_source": preflight.model_source.unwrap_or_else(|| "unconfigured".to_string()),
            "queue_reason": queue_reason,
            "instruction": instruction,
        })
        .to_string(),
        ExecutionResultStatus::Succeeded,
    )
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
    let Some(object) = parsed.as_object() else {
        return context_tool_failure(tool, "invalid_arguments", "agent_send 参数必须是 JSON 对象");
    };
    for key in object.keys() {
        if !matches!(key.as_str(), "task_id" | "message" | "references") {
            return context_tool_failure(
                tool,
                "invalid_arguments",
                format!("agent_send 不支持字段 {key}"),
            );
        }
    }
    let target_task_id = match object
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(task_id) => TaskId::new(task_id),
        None => {
            return context_tool_failure(
                tool,
                "invalid_arguments",
                "agent_send.task_id 必须是非空字符串",
            );
        }
    };
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
    if target_task.parent_task_id.as_ref() != Some(&parent_task.task_id)
        || target_task.root_task_id != parent_task.root_task_id
        || target_task.mission_id != parent_task.mission_id
        || target_task.workspace_scope != parent_task.workspace_scope
        || target_task.write_scope != parent_task.write_scope
    {
        return context_tool_failure(
            tool,
            "target_out_of_scope",
            "agent_send 只能向当前任务直接派发且作用域一致的子代理发送上下文",
        );
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
    deliver_agent_send(
        task_store,
        conversation_registry,
        parent_task,
        session_id,
        tool,
        &target_task_id,
        target_task.status,
        supplement,
        access,
        estimated_tokens,
        publish_event,
    )
}

#[allow(clippy::too_many_arguments)]
fn deliver_agent_send(
    task_store: &TaskStore,
    conversation_registry: &ConversationRegistry,
    parent_task: &magi_core::Task,
    session_id: &SessionId,
    tool: BuiltinToolName,
    target_task_id: &TaskId,
    observed_status: TaskStatus,
    supplement: AgentContextSupplement,
    access: AgentContextAccessRecord,
    estimated_tokens: usize,
    publish_event: &impl Fn(&str, serde_json::Value),
) -> (String, ExecutionResultStatus) {
    let now = supplement.created_at;
    // 排队中的代理尚未开始：补充写入其上下文包即可，启动时随上下文包一起渲染进提示词，
    // 不依赖只在运行期存在的信号通道。
    if observed_status == TaskStatus::Pending {
        let committed = task_store.deliver_agent_context_supplement(
            target_task_id,
            TaskStatus::Pending,
            supplement.clone(),
            access.clone(),
        );
        let package = match committed {
            Ok(package) => Some(package),
            // 启动事务可能已在读取状态后提交。原 Pending 写入未发生，沿运行中
            // FIFO 入口提交同一个 supplement；不能把正常启动竞态报成写盘失败。
            Err(magi_core::DomainError::InvalidState { .. })
                if task_store
                    .get_task(target_task_id)
                    .is_some_and(|task| task.status == TaskStatus::Running) =>
            {
                None
            }
            Err(error) => {
                return context_tool_failure(tool, "context_persist_failed", error.to_string());
            }
        };
        if let Some(package) = package {
            task_store.notify_runtime_change();
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
            return (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "sent",
                    "task_id": target_task_id.to_string(),
                    "package_id": package.package_id,
                    "revision": package.revision,
                    "estimated_tokens": estimated_tokens,
                    "delivery": "on_start",
                    "instruction": "代理尚在排队，补充上下文会在它开始执行时一并提供。",
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            );
        }
    }
    let commit = conversation_registry.enqueue_task_signal_with(session_id, target_task_id, || {
        let package = task_store.deliver_agent_context_supplement(
            target_task_id,
            TaskStatus::Running,
            supplement.clone(),
            access.clone(),
        )?;
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
    // agent_wait 等待 TaskStore 的变化通知；FIFO 就绪通知只唤醒 context_request。
    // 补充和运行中投递都已提交后，才能唤醒正在等待其子代理的接收方。
    task_store.notify_runtime_change();
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
            "delivery": "next_model_round",
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
    let mut candidates =
        match collect_context_candidates(task_store, session_store, task, session_id) {
            Ok(candidates) => candidates,
            Err(error) => return context_tool_failure(tool, "context_history_unavailable", error),
        };
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
    let candidates = match collect_context_candidates(task_store, session_store, task, session_id) {
        Ok(candidates) => candidates,
        Err(error) => return context_tool_failure(tool, "context_history_unavailable", error),
    };
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

fn is_parent_context_reply(signal: &RuntimeSignal, parent_task_id: &TaskId) -> bool {
    matches!(&signal.author, MailboxAuthor::Parent(id) if id == parent_task_id.as_str())
        && signal
            .payload
            .get("type")
            .and_then(serde_json::Value::as_str)
            == Some("agent_context_supplement")
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
                if task_store.get_task(&task.task_id).is_none_or(|current| {
                    !matches!(current.status, TaskStatus::Pending | TaskStatus::Running)
                }) {
                    return context_tool_failure(
                        tool,
                        "task_interrupted",
                        "等待上下文回复时任务已中断",
                    );
                }
                let wait_ms = (timeout_ms - elapsed_ms).min(250);
                // 只有直接父任务的补充才是这次请求的答复；孙代理的请求、审批通知等
                // 留在通道里，由任务循环或 agent_wait 按原路径处理。
                let Some(responses) = conversation_registry.wait_for_matching_task_signals(
                    session_id,
                    &task.task_id,
                    std::time::Duration::from_millis(wait_ms),
                    |signal| is_parent_context_reply(signal, &parent_task_id),
                ) else {
                    return context_tool_failure(
                        tool,
                        "task_interrupted",
                        "等待上下文回复时任务输入已关闭",
                    );
                };
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
                // 已投递的补充先交付；没有答复且父任务已退出时不再等到工具超时。
                if task_store.get_task(&parent_task_id).is_none_or(|parent| {
                    !matches!(parent.status, TaskStatus::Pending | TaskStatus::Running)
                }) {
                    return context_tool_failure(
                        tool,
                        "parent_not_running",
                        "等待上下文回复时父任务已结束",
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

/// 子代理 context_read 能够解析的引用 ID（会话消息、同一执行链的任务输出与证据）。
pub(crate) fn context_reference_ids(
    task_store: &TaskStore,
    session_store: &SessionStore,
    task: &magi_core::Task,
    session_id: &SessionId,
) -> magi_core::DomainResult<std::collections::HashSet<String>> {
    Ok(
        collect_context_candidates(task_store, session_store, task, session_id)?
            .into_iter()
            .map(|candidate| candidate.reference_id)
            .collect(),
    )
}

fn collect_context_candidates(
    task_store: &TaskStore,
    session_store: &SessionStore,
    task: &magi_core::Task,
    session_id: &SessionId,
) -> magi_core::DomainResult<Vec<ResolvedAgentContext>> {
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
    let previous_activations =
        crate::agent_activation::previous_activation_tasks(task_store, task, session_id)
            .map_err(|message| magi_core::DomainError::InvalidState { message })?
            .into_iter()
            .map(|task| task.task_id)
            .collect::<Vec<_>>();
    for candidate_task in task_store.all_tasks().into_iter().filter(|candidate| {
        (candidate.root_task_id == task.root_task_id && candidate.mission_id == task.mission_id)
            || previous_activations.contains(&candidate.task_id)
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
    if !previous_activations.is_empty() {
        for thread in session_store.thread_registry_snapshot(session_id) {
            for previous in &previous_activations {
                if !thread.handled_task_ids.contains(previous) {
                    continue;
                }
                // Thread 仍归原 activation；按引用读取的 transcript 不成为新调用授权。
                let snapshot = session_store.thread_context_snapshot(&thread.thread_id)?;
                let history = snapshot.messages().iter().map(|message| serde_json::json!({
                    "role":message.role, "content":message.content, "tool_calls":message.tool_calls,
                    "tool_call_id":message.tool_call_id,
                })).collect::<Vec<_>>();
                candidates.push(ResolvedAgentContext {
                    reference_id: format!("task:{previous}:transcript"),
                    kind: AgentContextReferenceKind::Other,
                    title: "Agent 前序执行记录（历史参考）".into(),
                    content: serde_json::to_string(&history).expect("thread projection serializes"),
                });
            }
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
    Ok(candidates)
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
    // 以非字母数字字符（含中文标点）切词；中文没有空格，整句词项再退化为相邻二字组匹配，
    // 命中至少一半二字组才算相关，避免单个常用字组造成噪声。
    query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(|term| {
            if haystack.contains(term) {
                return term.chars().count();
            }
            if term.is_ascii() {
                return 0;
            }
            let characters = term.chars().collect::<Vec<_>>();
            let total = characters.len().saturating_sub(1);
            let matched = characters
                .windows(2)
                .filter(|pair| haystack.contains(pair.iter().collect::<String>().as_str()))
                .count();
            if total > 0 && matched * 2 >= total {
                matched * 2
            } else {
                0
            }
        })
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

pub(crate) fn execute_goal_tool(
    session_store: &SessionStore,
    session_id: &SessionId,
    thread_id: magi_core::ThreadId,
    access_profile: AccessProfile,
    turn_id: &str,
    tool: BuiltinToolName,
    arguments: &str,
) -> (String, ExecutionResultStatus) {
    let parsed = match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(value) => value,
        Err(error) => {
            return goal_tool_failure::invalid_input(
                tool.as_str(),
                format!("目标工具参数不是有效 JSON: {error}"),
                "参数必须是 JSON 对象，按工具 schema 重新生成后再调用。",
            );
        }
    };

    match tool {
        BuiltinToolName::GetGoal => {
            let goal = session_store.current_goal(session_id);
            let plan = goal.as_ref().and_then(|goal| {
                session_store
                    .plan(session_id)
                    .filter(|plan| plan.goal_id.as_ref() == Some(&goal.goal_id))
            });
            (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "succeeded",
                    "goal": goal,
                    "plan": plan,
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
            match session_store.create_goal(
                session_id.clone(),
                thread_id,
                turn_id,
                objective,
                access_profile,
            ) {
                Ok(goal) => (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "succeeded",
                        "outcome": "created",
                        "goal": goal,
                    })
                    .to_string(),
                    ExecutionResultStatus::Succeeded,
                ),
                Err(error) => goal_tool_failure::store_failure(
                    tool.as_str(),
                    error,
                    session_store,
                    session_id,
                ),
            }
        }
        BuiltinToolName::UpdateGoal => {
            let status = match parsed.get("status").and_then(|value| value.as_str()) {
                Some("complete") => GoalStatus::Complete,
                Some("blocked") => GoalStatus::Blocked,
                _ => {
                    return goal_tool_failure::invalid_input(
                        tool.as_str(),
                        "update_goal status 只能是 complete 或 blocked",
                        "先用 get_goal 取得 goal_id、control_revision 与 plan.revision，再按工具参数要求重新调用。",
                    );
                }
            };
            let goal_id = parsed
                .get("goal_id")
                .and_then(|value| value.as_str())
                .map(|value| GoalId::new(value.trim().to_string()));
            let Some(goal_id) = goal_id else {
                return goal_tool_failure::invalid_input(
                    tool.as_str(),
                    "update_goal 必须提供 get_goal 返回的 goal_id",
                    "先用 get_goal 取得 goal_id、control_revision 与 plan.revision，再按工具参数要求重新调用。",
                );
            };
            let Some(expected_revision) = parsed
                .get("expected_revision")
                .and_then(|value| value.as_u64())
            else {
                return goal_tool_failure::invalid_input(
                    tool.as_str(),
                    "update_goal 必须提供 get_goal 返回的 control_revision",
                    "先用 get_goal 取得 goal_id、control_revision 与 plan.revision，再按工具参数要求重新调用。",
                );
            };
            let expected_plan_revision = match parsed.get("expected_plan_revision") {
                Some(serde_json::Value::Null) => None,
                Some(value) => match value.as_u64().filter(|revision| *revision > 0) {
                    Some(revision) => Some(revision),
                    None => {
                        return goal_tool_failure::invalid_input(
                            tool.as_str(),
                            "update_goal expected_plan_revision 必须是 get_goal 返回的 plan.revision，当前目标没有绑定计划时传 null",
                            "先用 get_goal 取得 goal_id、control_revision 与 plan.revision，再按工具参数要求重新调用。",
                        );
                    }
                },
                None => {
                    return goal_tool_failure::invalid_input(
                        tool.as_str(),
                        "update_goal 必须提供 expected_plan_revision；使用 get_goal 返回的 plan.revision，当前目标没有绑定计划时传 null",
                        "先用 get_goal 取得 goal_id、control_revision 与 plan.revision，再按工具参数要求重新调用。",
                    );
                }
            };
            let result = match status {
                GoalStatus::Complete => {
                    let summary = parsed
                        .get("completion_summary")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default();
                    let evidence_refs = parsed
                        .get("evidence_refs")
                        .and_then(|value| value.as_array())
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    session_store.complete_goal(
                        session_id,
                        &goal_id,
                        GoalRevisionExpectation::new(expected_revision, expected_plan_revision),
                        turn_id,
                        summary,
                        evidence_refs,
                    )
                }
                GoalStatus::Blocked => session_store.observe_goal_blocker(
                    session_id,
                    &goal_id,
                    expected_revision,
                    expected_plan_revision,
                    turn_id,
                    parsed
                        .get("blocker_key")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default(),
                    parsed
                        .get("reason")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default(),
                ),
                _ => unreachable!("model goal status is limited above"),
            };
            match result {
                Ok(goal) => (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "succeeded",
                        "outcome": if status == GoalStatus::Blocked && goal.status == GoalStatus::Active {
                            "blocker_observed"
                        } else {
                            "updated"
                        },
                        "goal": goal,
                    })
                    .to_string(),
                    ExecutionResultStatus::Succeeded,
                ),
                Err(error) => goal_tool_failure::store_failure(tool.as_str(), error, session_store, session_id),
            }
        }
        _ => unreachable!("execute_goal_tool 只接收 goal 工具"),
    }
}

#[cfg(test)]
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
) -> Result<(), String> {
    let access_profile = child
        .policy_snapshot
        .as_ref()
        .map(magi_core::TaskPolicy::effective_access_profile)
        .unwrap_or_default()
        .as_str();
    conversation_registry.enqueue_task_signal(session_id, &child.task_id, RuntimeSignal {
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
        })
}

struct AgentWaitRuntime<'a> {
    task_store: &'a TaskStore,
    session_store: &'a SessionStore,
    admission: &'a crate::execution_admission::ExecutionAdmissionController,
    conversation_registry: &'a ConversationRegistry,
    session_id: &'a SessionId,
    execution_allowed: &'a (dyn Fn() -> bool + Sync),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentWaitMode {
    /// 全部代理进入终态才返回。
    All,
    /// 任一代理进入终态即返回。
    Any,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AgentWaitRequest {
    task_ids: Vec<TaskId>,
    timeout_ms: u64,
    mode: AgentWaitMode,
}

fn execute_agent_wait_with_runtime(
    runtime: AgentWaitRuntime<'_>,
    parent_task: &magi_core::Task,
    tool: magi_tool_runtime::BuiltinToolName,
    parsed: &serde_json::Value,
) -> (String, ExecutionResultStatus) {
    let AgentWaitRuntime {
        task_store,
        session_store,
        admission,
        conversation_registry,
        session_id,
        execution_allowed,
    } = runtime;
    let request = match parse_agent_wait_request(parsed) {
        Ok(request) => request,
        Err(error) => {
            return (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "rejected",
                    "error_code": "invalid_arguments",
                    "failure_stage": "input_validation",
                    "child_task_id": serde_json::Value::Null,
                    "error": error,
                    "instruction": "请传入不含未知字段的对象；task_ids 必须是至少一个不重复的 child_task_id，timeout_ms 可省略或使用 1000-1800000 的整数，mode 可省略或为 all/any。",
                })
                .to_string(),
                ExecutionResultStatus::Rejected,
            );
        }
    };
    let task_ids = request.task_ids;
    if let Some(task_id) = task_ids
        .iter()
        .find(|task_id| !agent_wait_task_is_direct_child(task_store, parent_task, task_id))
    {
        return (
            serde_json::json!({
                "tool": tool.as_str(),
                "status": "rejected",
                "error_code": "scope_mismatch",
                "failure_stage": "scope_validation",
                "child_task_id": task_id.to_string(),
                "error": "agent_wait 只能等待当前任务派发的代理",
                "instruction": "请使用最近一次 agent_spawn 返回的 child_task_id，且只能等待当前任务直接派发的代理。",
            })
            .to_string(),
            ExecutionResultStatus::Rejected,
        );
    }
    let started_at = std::time::Instant::now();
    let mut observed_status_version = task_store.status_change_version();
    // 等待子代理期间让出执行名额，避免与自己派发的子代理争抢同一批名额。
    let waiting = admission.suspend_while_waiting(&parent_task.task_id);
    let parent_is_active = || {
        execution_allowed()
            && task_store
                .get_task(&parent_task.task_id)
                .is_some_and(|task| {
                    matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
                })
    };
    loop {
        if !execution_allowed()
            || waiting.is_some() && !parent_is_active()
            || task_store
                .get_task(&parent_task.task_id)
                .is_some_and(|task| {
                    !matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
                })
        {
            return (
                serde_json::json!({"tool":tool.as_str(),"status":"cancelled",
                "error_code":"agent_wait_cancelled","execution_started":false})
                .to_string(),
                ExecutionResultStatus::Cancelled,
            );
        }
        let runtime_signals =
            conversation_registry.drain_task_signals(session_id, &parent_task.task_id);
        let mut receipts = Vec::new();
        let mut pending_task_ids = Vec::new();
        for task_id in &task_ids {
            match task_store.get_task(task_id).and_then(|child| {
                crate::agent_receipt::AgentReceipt::from_terminal_task_in_session(
                    &child,
                    session_store,
                    session_id,
                )
            }) {
                Some(receipt) => receipts.push(receipt),
                None => pending_task_ids.push(task_id.to_string()),
            }
        }
        let condition_met = match request.mode {
            AgentWaitMode::All => pending_task_ids.is_empty(),
            AgentWaitMode::Any => !receipts.is_empty(),
        };
        let elapsed_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        let timed_out = !condition_met && elapsed_ms >= request.timeout_ms;
        if runtime_signals.is_empty() && !condition_met && !timed_out {
            observed_status_version = task_store.wait_for_status_change_since(
                observed_status_version,
                std::time::Duration::from_millis(request.timeout_ms - elapsed_ms),
            );
            continue;
        }
        let count = |child_status: &str| {
            receipts
                .iter()
                .filter(|receipt| receipt.child_status() == child_status)
                .count()
        };
        let (completed, failed, killed) = (count("completed"), count("failed"), count("killed"));
        let (status, instruction) = if !runtime_signals.is_empty() {
            (
                "attention_required",
                "代理等待期间收到运行时消息。若为 agent_context_request，请先读取 child_task_id 和 request，使用 agent_send 回复对应代理；若为审批等待，告知用户需要处理的审批；若来自 parent:* 的 agent_context_supplement，是你的父任务补充的事实与约束，按当前任务补充处理，不能扩大目标范围或权限。之后对 pending_task_ids 继续 agent_wait。",
            )
        } else if timed_out {
            (
                "timed_out",
                "仍有代理未结束。results 中已结束代理的结果已经收集；可以先处理不依赖未结束代理的工作，需要时再对 pending_task_ids 调用 agent_wait，或用 agent_cancel 取消不再需要的代理。",
            )
        } else if pending_task_ids.is_empty() {
            (
                "completed",
                "对照每个代理的 assignment.goal 阅读 result.final_text 与 activity（实际执行的命令、退出码和改动文件），合并结论、证据、风险与缺口。",
            )
        } else {
            (
                "completed",
                "已有代理结束，results 中的结果已经收集。处理后对 pending_task_ids 继续 agent_wait。",
            )
        };
        let instruction = if failed > 0 || killed > 0 {
            format!(
                "{instruction} {}",
                magi_tool_runtime::AGENT_DELEGATION_RECOVERY_GUIDANCE
            )
        } else {
            instruction.to_string()
        };
        let mut payload = serde_json::json!({
            "tool": tool.as_str(),
            "status": status,
            "mode": match request.mode {
                AgentWaitMode::All => "all",
                AgentWaitMode::Any => "any",
            },
            "timed_out": timed_out,
            "all_terminal": pending_task_ids.is_empty(),
            "completed_count": completed,
            "failed_count": failed,
            "killed_count": killed,
            "pending_task_ids": pending_task_ids,
            "results": crate::agent_receipt::render_receipts(
                receipts,
                crate::agent_receipt::AGENT_WAIT_FINAL_TEXT_BUDGET_BYTES,
            ),
            // agent_wait 已按代理分配最终答复的展示预算，模型视图不再做通用截断。
            "model_budgeted": true,
            "instruction": instruction,
        });
        if !runtime_signals.is_empty() {
            payload["runtime_signals"] = serde_json::json!(runtime_signals);
        }
        if waiting
            .as_ref()
            .is_some_and(|guard| !guard.resume(parent_is_active))
        {
            return (
                serde_json::json!({"tool":tool.as_str(),"status":"cancelled",
                "error_code":"agent_wait_cancelled","execution_started":false})
                .to_string(),
                ExecutionResultStatus::Cancelled,
            );
        }
        return (payload.to_string(), ExecutionResultStatus::Succeeded);
    }
}

#[cfg(test)]
fn execute_agent_wait(
    task_store: &TaskStore,
    parent_task: &magi_core::Task,
    tool: magi_tool_runtime::BuiltinToolName,
    parsed: &serde_json::Value,
) -> (String, ExecutionResultStatus) {
    let registry = ConversationRegistry::new();
    let session_id = SessionId::new("session-agent-wait-test");
    registry.open_task_signal_channel(&session_id, &parent_task.task_id);
    execute_agent_wait_with_runtime(
        AgentWaitRuntime {
            session_store: &SessionStore::new(),
            task_store,
            admission: &crate::execution_admission::ExecutionAdmissionController::default(),
            conversation_registry: &registry,
            session_id: &session_id,
            execution_allowed: &|| true,
        },
        parent_task,
        tool,
        parsed,
    )
}

/// 主线取消自己直接派发的代理：级联终止该代理子树。进程、浏览器租约等执行资源
/// 由 TaskStore 终态回调统一释放，这里不另走一条清理路径。
fn execute_agent_cancel(
    task_store: &TaskStore,
    execution_registry: &TaskExecutionRegistry,
    session_store: &SessionStore,
    parent_task: &magi_core::Task,
    session_id: &SessionId,
    tool: magi_tool_runtime::BuiltinToolName,
    parsed: &serde_json::Value,
) -> (String, ExecutionResultStatus) {
    let rejected = |error_code: &str, error: String, instruction: &str| {
        (
            serde_json::json!({
                "tool": tool.as_str(),
                "status": "rejected",
                "error_code": error_code,
                "error": error,
                "instruction": instruction,
            })
            .to_string(),
            ExecutionResultStatus::Rejected,
        )
    };
    let non_empty = |key: &str| {
        parsed
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let (Some(task_id), Some(reason)) = (non_empty("task_id"), non_empty("reason")) else {
        return rejected(
            "invalid_arguments",
            "agent_cancel 需要非空的 task_id 和 reason".to_string(),
            "请传入 agent_spawn 返回的 child_task_id，并说明取消原因。",
        );
    };
    let child_task_id = TaskId::new(task_id);
    if !agent_wait_task_is_direct_child(task_store, parent_task, &child_task_id) {
        return rejected(
            "scope_mismatch",
            format!("{child_task_id} 不是当前任务直接派发的代理"),
            "只能取消当前任务通过 agent_spawn 直接派发的代理。",
        );
    }
    let changed = match crate::task_runner::kill_subtree_in_store(
        task_store,
        &execution_registry.execution_admission(),
        &child_task_id,
        vec![format!("主线取消：{reason}")],
    ) {
        Ok(changed) => changed,
        Err(error) => {
            return (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "failed",
                    "error_code": "cancel_failed",
                    "child_task_id": child_task_id.to_string(),
                    "error": error,
                    "instruction": "取消未完全成功；可稍后用 agent_wait 查看该代理状态。",
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    // TaskStore 终态只撤销执行资格；线程、进程及 worktree 仍可能正在退出。
    // 父任务本身也在执行，不能等待整个 root，否则会等待当前调用而死锁。
    let task_ids = task_store.collect_subtree_ids(&child_task_id);
    let settlement = execution_registry.settle_agent_subtree(session_id, &task_ids);
    // 取消与完成可能竞争。没有改变状态不等于主线已经读过产物，不能把
    // already_terminal 当作无结果的取消成功，也不能引导模型重新派发同一工作。
    let receipt = task_store.get_task(&child_task_id).and_then(|child| {
        crate::agent_receipt::AgentReceipt::from_terminal_task_in_session(
            &child,
            session_store,
            session_id,
        )
    });
    let Some(receipt) = receipt else {
        return (
            serde_json::json!({
                "tool": tool.as_str(),
                "status": "failed",
                "error_code": "cancel_unsettled",
                "child_task_id": child_task_id,
                "instruction": "取消后未能核对代理终态；先用 agent_wait 核对原任务，不得假定已停止或重复执行其工作。",
            }).to_string(),
            ExecutionResultStatus::Failed,
        );
    };
    let child_status = receipt.child_status().to_string();
    let resources_settled = matches!(settlement, Ok(true));
    let settlement_error = settlement.err();
    let instruction = if !resources_settled {
        "Task 已进入终态，但尚未确认执行资源退出。不得开始与该子树冲突的替代工作或宣称资源已释放；先核对原产物及结算诊断。"
    } else if changed {
        "取消已结算且执行资源已退出，results 包含终态回执；取消不保证此前没有副作用。"
    } else {
        "代理在取消生效前已进入终态，原结果未被覆盖。先读取 results 中的 assignment.goal、result.final_text、activity 和 workspace，核对已有产物，不要把 already_terminal 当成任务尚未执行。"
    };
    (
        serde_json::json!({
            "tool": tool.as_str(),
            "status": if settlement_error.is_some() { "failed" } else if changed { "cancelled" } else { "already_terminal" },
            "cancellation_status": if changed { "cancelled" } else { "already_terminal" },
            "error_code": settlement_error.as_ref().map(|_| "cancel_unsettled"),
            "child_task_id": child_task_id.to_string(),
            "child_status": child_status,
            "reason": reason,
            "resources_settled": resources_settled,
            "settlement_error": settlement_error,
            "results": crate::agent_receipt::render_receipts(
                vec![receipt],
                crate::agent_receipt::AGENT_WAIT_FINAL_TEXT_BUDGET_BYTES,
            ),
            "model_budgeted": true,
            "instruction": format!("{instruction} {}", magi_tool_runtime::AGENT_DELEGATION_RECOVERY_GUIDANCE),
        })
        .to_string(),
        if settlement_error.is_none() {
            ExecutionResultStatus::Succeeded
        } else {
            ExecutionResultStatus::Failed
        },
    )
}

fn agent_wait_task_is_direct_child(
    task_store: &TaskStore,
    parent_task: &magi_core::Task,
    child_task_id: &TaskId,
) -> bool {
    task_store.get_task(child_task_id).is_some_and(|child| {
        child.parent_task_id.as_ref() == Some(&parent_task.task_id)
            && agent_wait_child_execution_scope_matches(parent_task, &child)
    })
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

fn parse_agent_wait_request(parsed: &serde_json::Value) -> Result<AgentWaitRequest, String> {
    let object = parsed
        .as_object()
        .ok_or_else(|| "agent_wait 参数必须是 JSON 对象".to_string())?;
    for key in object.keys() {
        if !matches!(key.as_str(), "task_ids" | "timeout_ms" | "mode") {
            return Err(format!("agent_wait 不支持字段 {key}"));
        }
    }
    let values = object
        .get("task_ids")
        .ok_or_else(|| "agent_wait 缺少必需字段 task_ids".to_string())?
        .as_array()
        .ok_or_else(|| "agent_wait task_ids 必须是数组".to_string())?;
    if values.is_empty() {
        return Err("agent_wait task_ids 至少需要一个任务 ID".to_string());
    }
    let mut seen = std::collections::HashSet::with_capacity(values.len());
    let mut task_ids = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let task_id = value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("agent_wait task_ids[{index}] 必须是非空字符串"))?;
        if !seen.insert(task_id.to_string()) {
            return Err(format!("agent_wait task_ids 不能重复: {task_id}"));
        }
        task_ids.push(TaskId::new(task_id));
    }
    let timeout_ms = match object.get("timeout_ms") {
        None => AGENT_WAIT_DEFAULT_TIMEOUT_MS,
        Some(value) => {
            let timeout_ms = value
                .as_u64()
                .ok_or_else(|| "agent_wait timeout_ms 必须是整数".to_string())?;
            if !(AGENT_WAIT_MIN_TIMEOUT_MS..=AGENT_WAIT_MAX_TIMEOUT_MS).contains(&timeout_ms) {
                return Err(format!(
                    "agent_wait timeout_ms 必须在 {AGENT_WAIT_MIN_TIMEOUT_MS}-{AGENT_WAIT_MAX_TIMEOUT_MS} 之间"
                ));
            }
            timeout_ms
        }
    };
    let mode = match object.get("mode") {
        None => AgentWaitMode::All,
        Some(value) => match value.as_str() {
            Some("all") => AgentWaitMode::All,
            Some("any") => AgentWaitMode::Any,
            _ => return Err("agent_wait mode 只能是 all 或 any".to_string()),
        },
    };
    Ok(AgentWaitRequest {
        task_ids,
        timeout_ms,
        mode,
    })
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
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    plan_store: &magi_plan::PlanStore,
    project_memory: Option<&magi_project_memory::ProjectMemoryStore>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    workspace_root_path: Option<&PathBuf>,
    worker_id: Option<&magi_core::WorkerId>,
    browser_capability_snapshot: Option<BrowserCapabilitySnapshot>,
    tool_call: &ChatToolCall,
    on_progress: Option<&(dyn Fn(ToolExecutionProgress) -> Result<(), String> + Sync)>,
    execution_allowed: &(dyn Fn() -> bool + Sync),
    workspace_resources: Option<&crate::workspace_execution::WorkspaceExecutionResources>,
) -> (String, ExecutionResultStatus) {
    if !execution_allowed() {
        return tool_interrupted_before_execution_result(&tool_call.function.name);
    }
    if let Some(rejection) = project_directory_rejection(
        execution_registry,
        task,
        workspace_id,
        &tool_call.function.name,
    ) {
        return rejection;
    }
    // S7-E：协调器工具（agent_spawn）由 orchestration 层拦截，
    // 不进 BuiltinTool::execute —— 它需要 task_store / event_bus 等上下文。
    // S9：UpdatePlan 同样在此层拦截，因为它要操作 session 维度的 PlanStore。
    // S10：MemoryWrite 同样在此层拦截，因为它要操作 workspace 维度的 ProjectMemoryStore。
    if let Some(canonical) =
        magi_tool_runtime::BuiltinToolName::from_name(tool_call.function.name.as_str())
        && !task_can_see_builtin_tool(Some(task), Some(agent_role_registry), canonical)
    {
        let decision = task_tool_visibility_decision_payload(canonical.as_str(), task);
        return (decision.payload, decision.status);
    }

    // 关闭 Skill 的工具范围是执行约束，不能只隐藏 schema。所有标准工具先在
    // 同一入口校验，避免 MCP/插件或编排拦截分支绕过稍后的 Builtin registry。
    // Goal 模式的四个生命周期工具由运行时保留；自定义 handler 另有绑定校验。
    let goal_lifecycle = task.is_goal_mode()
        && matches!(
            tool_call.function.name.as_str(),
            "get_goal" | "create_goal" | "update_goal" | "update_plan"
        );
    let live_skill_rejection = || {
        if !goal_lifecycle
            && tool_call.function.name != "update_execution_steps"
            && parse_skill_custom_tool_name(&tool_call.function.name).is_none()
        {
            let access_profile = task
                .policy_snapshot
                .as_ref()
                .map(TaskPolicy::effective_access_profile)
                .unwrap_or_default();
            let policy =
                active_skill_tool_execution_policy(access_profile, skill_runtime, skill_name);
            // 持久化配置类工具只有被激活 Skill 明确声明时才能调用，不能靠“没有限制”放行。
            if BuiltinToolName::from_name(&tool_call.function.name)
                .is_some_and(|tool| tool.requires_skill_declaration())
                && !policy
                    .allowed_tool_names
                    .as_ref()
                    .is_some_and(|names| names.iter().any(|name| name == &tool_call.function.name))
            {
                let decision = task_policy_decision_payload(
                    &tool_call.function.name,
                    ExecutionResultStatus::Rejected,
                    format!(
                        "tool_not_in_allowed_scope: 当前没有声明 {} 的 Skill 处于激活状态",
                        tool_call.function.name
                    ),
                    Some(access_profile),
                );
                return Some((decision.payload, decision.status));
            }
            if let Some(rejection) = policy.tool_name_rejection(&tool_call.function.name) {
                let decision = task_policy_decision_payload(
                    &tool_call.function.name,
                    ExecutionResultStatus::Rejected,
                    format!(
                        "{}: 当前 Skill 不允许调用 {}",
                        rejection.reason_code(),
                        tool_call.function.name
                    ),
                    Some(access_profile),
                );
                return Some((decision.payload, decision.status));
            }
        }

        None
    };

    if let Some(rejection) = live_skill_rejection() {
        return rejection;
    }

    if let Some(gate) = safety_gate {
        publish_safety_evaluation_audit(
            event_bus,
            gate,
            &tool_call.function.name,
            &tool_call.function.arguments,
            &tool_call.id,
            SafetyEvaluationAuditContext {
                access_profile: task
                    .policy_snapshot
                    .as_ref()
                    .map(magi_core::TaskPolicy::effective_access_profile)
                    .unwrap_or_default(),
                session_id: Some(session_id),
                workspace_id: workspace_id.as_ref(),
                mission_id: Some(&task.mission_id),
                task_id: Some(&task.task_id),
            },
        );
    }

    // 执行映射由 dispatcher 的实际 cwd 派生，原 Task 只保存目录授权。
    // 多根映射必须由资源准备入口逐项提供；未准备完整映射时拒绝工具调用。
    let project_directory_map: magi_core::DomainResult<_> = task
        .executor_binding
        .as_ref()
        .and_then(|executor| executor.project_directories.as_ref())
        .map(|binding| {
            let map = workspace_resources
                .and_then(|resources| resources.directories.clone())
                .ok_or_else(|| magi_core::DomainError::Validation {
                    message: "本轮缺少完整项目目录执行映射，工具未执行".into(),
                })?;
            map.validate_for_binding(binding)?;
            Ok(map)
        })
        .transpose();
    let project_directory_map = match project_directory_map {
        Ok(map) => map,
        Err(error) => {
            return (
                serde_json::json!({
                    "tool": tool_call.function.name,
                    "status": "failed",
                    "error_code": "project_directory_invalid",
                    "error": error.to_string(),
                    "execution_started": false,
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let selected_context = match (ToolExecutionContext {
        workspace_id: workspace_id.clone(),
        working_directory: workspace_root_path.cloned(),
        project_directory_map: project_directory_map.clone(),
        ..Default::default()
    }).for_tool_arguments(&tool_call.function.arguments) {
        Ok(context) => context,
        Err(error) => return (serde_json::json!({
            "tool":tool_call.function.name, "status":"rejected", "error_code":"project_directory_invalid",
            "error":error.to_string(), "execution_started":false,
        }).to_string(), ExecutionResultStatus::Rejected),
    };
    let workspace_root_path = selected_context.working_directory.as_ref();
    let projected_policy = task
        .policy_snapshot
        .as_ref()
        .map(|policy| {
            let mut tool_policy = ToolExecutionPolicy::default();
            tool_policy.apply_task_policy(policy);
            selected_context
                .project_execution_policy(&tool_policy)
                .map(|projected| {
                    let mut policy = policy.clone();
                    policy.allowed_paths = projected.allowed_paths;
                    policy.denied_paths = projected.denied_paths;
                    policy.read_only_paths = projected.read_only_paths;
                    policy
                })
        })
        .transpose();
    let projected_policy = match projected_policy {
        Ok(policy) => policy,
        Err(error) => return (serde_json::json!({
            "tool":tool_call.function.name, "status":"rejected", "error_code":"project_directory_invalid",
            "error":error.to_string(), "execution_started":false,
        }).to_string(), ExecutionResultStatus::Rejected),
    };
    let mut approval_granted = false;
    if let Some(decision) = policy_tool_preflight_decision_in_context(
        projected_policy.as_ref(),
        safety_gate,
        &tool_call.function.name,
        &tool_call.function.arguments,
        workspace_root_path,
        Some(&selected_context),
    ) {
        if decision.status == ExecutionResultStatus::NeedsApproval {
            match run_with_task_execution_wait(
                task_store,
                execution_registry,
                task,
                &tool_call.function.name,
                || {
                    await_task_tool_approval(
                        event_bus,
                        conversation_registry,
                        task_store,
                        session_store,
                        task,
                        session_id,
                        workspace_id,
                        workspace_root_path,
                        project_directory_map.as_ref(),
                        tool_call,
                        &decision,
                        on_progress,
                        execution_allowed,
                    )
                },
                execution_allowed,
            )
            .and_then(std::convert::identity)
            {
                Ok(()) => approval_granted = true,
                Err(result) => return result,
            }
        } else {
            return (decision.payload, decision.status);
        }
    }

    if approval_granted && let Some(rejection) = live_skill_rejection() {
        return rejection;
    }
    if let Some(rejection) = project_directory_rejection(
        execution_registry,
        task,
        workspace_id,
        &tool_call.function.name,
    ) {
        return rejection;
    }

    if let Err(error) = selected_context.for_tool_arguments(&tool_call.function.arguments) {
        return (serde_json::json!({
            "tool":tool_call.function.name,"status":"rejected","error_code":"project_directory_invalid",
            "error":error.to_string(),"execution_started":false,
        }).to_string(), ExecutionResultStatus::Rejected);
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
            let (payload, mut status) = execute_goal_tool(
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
                task.task_id.as_str(),
                canonical,
                &tool_call.function.arguments,
            );
            if canonical != BuiltinToolName::UpdateGoal {
                return (payload, status);
            }
            // Goal 的控制事实可能已经提交，即使本次更新因旧版本被拒绝也应核对
            // 原撤销范围。不能因此重建代理，或等待/取消正在调用工具的主线。
            let settlement = match run_with_task_execution_wait(
                task_store,
                execution_registry,
                task,
                canonical.as_str(),
                || {
                    execution_registry.settle_revoked_goal_agent_runs(
                        task_store,
                        session_store,
                        session_id,
                    )
                },
                execution_allowed,
            ) {
                Ok(settlement) => settlement,
                Err(interrupted) => return interrupted,
            };
            let mut result: serde_json::Value =
                serde_json::from_str(&payload).expect("Goal 工具返回 JSON");
            result["agent_resources_settled"] = serde_json::json!(settlement.is_ok());
            if let Err(error) = settlement {
                result["status"] = serde_json::json!("failed");
                result["error_code"] = serde_json::json!("update_goal_agent_settlement_failed");
                result["settlement_error"] = serde_json::json!(error);
                result["instruction"] = serde_json::json!(
                    "Goal 的更新结果见 goal；所属代理尚未确认退出，不能宣称资源已释放或重新派发同一工作。先核对原代理与产物，勿重复目标更新或副作用操作。"
                );
                status = ExecutionResultStatus::Failed;
            }
            return (result.to_string(), status);
        }
        if matches!(
            canonical,
            magi_tool_runtime::BuiltinToolName::AgentSpawn
                | magi_tool_runtime::BuiltinToolName::AgentSend
                | magi_tool_runtime::BuiltinToolName::AgentCancel
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
                    plan_store,
                    task,
                    session_id,
                    workspace_id,
                    workspace_root_path,
                    execution_allowed,
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
            if canonical != BuiltinToolName::ContextRequest {
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
            return run_with_task_execution_wait(
                task_store,
                execution_registry,
                task,
                &tool_call.function.name,
                || {
                    execute_agent_context_tool(
                        task_store,
                        session_store,
                        conversation_registry,
                        task,
                        session_id,
                        canonical,
                        &tool_call.function.arguments,
                    )
                },
                execution_allowed,
            )
            .unwrap_or_else(std::convert::identity);
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
        if matches!(
            canonical,
            magi_tool_runtime::BuiltinToolName::UpdateExecutionSteps
        ) {
            return execute_execution_steps(
                event_bus,
                session_store,
                task_store,
                task,
                session_id,
                tool_call,
            );
        }
        if matches!(
            canonical,
            magi_tool_runtime::BuiltinToolName::AskUserQuestion
        ) {
            return run_with_task_execution_wait(
                task_store,
                execution_registry,
                task,
                &tool_call.function.name,
                || {
                    await_user_question_answer(
                        event_bus,
                        conversation_registry,
                        task_store,
                        session_store,
                        task,
                        session_id,
                        workspace_id,
                        tool_call,
                        on_progress,
                        execution_allowed,
                    )
                },
                execution_allowed,
            )
            .unwrap_or_else(std::convert::identity);
        }
        if canonical == BuiltinToolName::AgentRoleGet {
            return crate::agent_role_tool::execute_agent_role_get(
                agent_role_registry,
                &tool_call.function.arguments,
            );
        }
        if canonical == BuiltinToolName::AgentRoleSave {
            // 角色是持久的全局配置：无论访问模式如何，保存前都必须由用户逐次确认。
            let proposal = match crate::agent_role_tool::prepare_agent_role_save(
                agent_role_registry,
                &tool_call.function.arguments,
            ) {
                Ok(crate::agent_role_tool::RoleSavePreparation::Confirm(proposal)) => proposal,
                Ok(crate::agent_role_tool::RoleSavePreparation::Unchanged(result)) => {
                    return result;
                }
                Err(result) => return result,
            };
            let decision = ToolPreflightDecision {
                payload: serde_json::json!({
                    "tool": tool_call.function.name,
                    "status": "needs_approval",
                    "error": proposal.summary(),
                })
                .to_string(),
                status: ExecutionResultStatus::NeedsApproval,
            };
            if let Err(rejected) = run_with_task_execution_wait(
                task_store,
                execution_registry,
                task,
                &tool_call.function.name,
                || {
                    await_task_tool_approval(
                        event_bus,
                        conversation_registry,
                        task_store,
                        session_store,
                        task,
                        session_id,
                        workspace_id,
                        workspace_root_path,
                        project_directory_map.as_ref(),
                        tool_call,
                        &decision,
                        on_progress,
                        execution_allowed,
                    )
                },
                execution_allowed,
            )
            .and_then(std::convert::identity)
            {
                return rejected;
            }
            let (result, saved_role_id) =
                crate::agent_role_tool::commit_agent_role_save(agent_role_registry, proposal);
            if let Some(role_id) = saved_role_id {
                let _ = event_bus.publish(EventEnvelope::domain(
                    EventId::unique("event-agent-role-saved"),
                    "settings.agent_roles.changed",
                    serde_json::json!({
                        "role_id": role_id,
                        "registry_revision": agent_role_registry.registry_revision(),
                    }),
                ));
            }
            return result;
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
            EventId::unique("event-task-tool-invoked"),
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
        if skill_name.is_some()
            && serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments)
                .ok()
                .is_none_or(|args| args["mode"] != "reference")
        {
            return (serde_json::json!({"status":"rejected", "error_code":"skill_scope_conflict",
                "error":"已有 active Skill，仅允许 mode=reference 读取补充说明；原权限限制继续有效"}).to_string(), ExecutionResultStatus::Rejected);
        }
        return execute_skill_apply_from_runtime(&tool_call.function.arguments, skill_runtime);
    }

    let access_profile = if approval_granted {
        AccessProfile::FullAccess
    } else {
        task.policy_snapshot
            .as_ref()
            .map(magi_core::TaskPolicy::effective_access_profile)
            .unwrap_or_default()
    };
    let execute_runtime_tool = |effective_access_profile: AccessProfile| {
        if !execution_allowed()
            || task_store.get_task(&task.task_id).is_some_and(|latest| {
                matches!(latest.status, TaskStatus::Failed | TaskStatus::Killed)
            })
        {
            return (
                serde_json::json!({"tool":tool_call.function.name,"status":"cancelled",
                    "error_code":"task_stopped_before_tool","execution_started":false,
                })
                .to_string(),
                ExecutionResultStatus::Cancelled,
            );
        }
        if let Some(rejection) = live_skill_rejection() {
            return rejection;
        }
        if let Some(rejection) = project_directory_rejection(
            execution_registry,
            task,
            workspace_id,
            &tool_call.function.name,
        ) {
            return rejection;
        }
        if let Err(error) = selected_context.for_tool_arguments(&tool_call.function.arguments) {
            return (serde_json::json!({
                "tool":tool_call.function.name,"status":"rejected","error_code":"project_directory_invalid",
                "error":error.to_string(),"execution_started":false,
            }).to_string(), ExecutionResultStatus::Rejected);
        }
        if let Some((tool_skill_name, binding_id)) =
            parse_skill_custom_tool_name(&tool_call.function.name)
        {
            return execute_skill_custom_tool(
                tool_call,
                &tool_skill_name,
                &binding_id,
                skill_name,
                task_tool_execution_policy_scope_with_access_profile(
                    task,
                    effective_access_profile,
                ),
                safety_gate,
                skill_runtime,
                skill_dispatch_runtime,
                ToolExecutionContext {
                    caller_scope: if task_is_coordinator(Some(task), Some(agent_role_registry)) {
                        magi_tool_runtime::ToolCallerScope::SessionMainline
                    } else if task_can_delegate(Some(task), Some(agent_role_registry)) {
                        magi_tool_runtime::ToolCallerScope::DelegatingTask
                    } else {
                        magi_tool_runtime::ToolCallerScope::DelegatedTask
                    },
                    worker_id: worker_id.cloned(),
                    task_id: Some(task.task_id.clone()),
                    session_id: Some(session_id.clone()),
                    workspace_id: workspace_id.clone(),
                    access_profile: effective_access_profile,
                    working_directory: workspace_root_path.cloned(),
                    project_directory_map: project_directory_map.clone(),
                    browser_capability_snapshot: browser_capability_snapshot.clone(),
                    browser_execution_id: Some(format!("task:{}", task.task_id)),
                },
                workspace_root_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
            );
        }

        if let Some(result) = registry.execute_external_mcp_tool(
            &tool_call.function.name,
            &tool_call.function.arguments,
            effective_access_profile,
        ) {
            return result;
        }

        if let Some(rejection) = internal_builtin_tool_rejection_payload(&tool_call.function.name) {
            return (rejection, ExecutionResultStatus::Failed);
        }

        let mut tool_policy =
            active_skill_tool_execution_policy(effective_access_profile, skill_runtime, skill_name);
        if let Some(policy) = task.policy_snapshot.as_ref() {
            tool_policy.apply_task_policy(policy);
        }
        tool_policy.access_profile = effective_access_profile;
        let input = ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new(&tool_call.id),
            &tool_call.function.name,
            tool_call.function.arguments.clone(),
        );
        let context = ToolExecutionContext {
            caller_scope: if task_is_coordinator(Some(task), Some(agent_role_registry)) {
                magi_tool_runtime::ToolCallerScope::SessionMainline
            } else if task_can_delegate(Some(task), Some(agent_role_registry)) {
                magi_tool_runtime::ToolCallerScope::DelegatingTask
            } else {
                magi_tool_runtime::ToolCallerScope::DelegatedTask
            },
            worker_id: worker_id.cloned(),
            task_id: Some(task.task_id.clone()),
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.clone(),
            access_profile: tool_policy.access_profile,
            working_directory: workspace_root_path.cloned(),
            project_directory_map: project_directory_map.clone(),
            browser_capability_snapshot: browser_capability_snapshot.clone(),
            browser_execution_id: Some(format!("task:{}", task.task_id)),
        };
        if let Some(result) = registry.execute_plugin_tool(
            &tool_call.function.name,
            &tool_call.function.arguments,
            &context,
            effective_access_profile,
        ) {
            return result;
        }
        // 等待 Job 不占模型执行名额；后台进程仍可能写入，不能让出仓库租约。
        let job_wait = (tool_call.function.name == BuiltinToolName::ShellExec.as_str()
            && serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments)
                .ok()
                .is_some_and(|arguments| arguments["action"] == "wait"))
        .then(|| {
            execution_registry
                .execution_admission()
                .suspend_while_waiting(&task.task_id)
        })
        .flatten();
        let output = match on_progress {
            Some(on_progress) => registry.execute_with_policy_and_progress(
                input,
                context,
                &tool_policy,
                &|progress| {
                    let _ = on_progress(progress);
                },
            ),
            None => registry.execute_with_policy(input, context, &tool_policy),
        };
        let resumed = job_wait.as_ref().is_none_or(|guard| {
            guard.resume(|| {
                execution_allowed()
                    && task_store.get_task(&task.task_id).is_some_and(|current| {
                        matches!(current.status, TaskStatus::Pending | TaskStatus::Running)
                    })
            })
        });
        if !resumed
            || !execution_allowed()
            || task_store.get_task(&task.task_id).is_some_and(|latest| {
                matches!(latest.status, TaskStatus::Failed | TaskStatus::Killed)
            })
        {
            // 取消可能发生在进程尚未登记时；工具返回前再结算该任务迟到创建的资源。
            registry.cancel_active_executions(&magi_tool_runtime::ToolExecutionContextQuery {
                session_id: Some(session_id.clone()),
                task_id: Some(task.task_id.clone()),
                ..Default::default()
            });
            return (serde_json::json!({"tool":tool_call.function.name,"status":"cancelled",
                "error_code":"task_stopped_during_tool","execution":"unknown",
                "source_result":serde_json::from_str::<serde_json::Value>(&output.payload).unwrap_or(serde_json::Value::String(output.payload)),
            }).to_string(), ExecutionResultStatus::Cancelled);
        }
        if tool_call.function.name == BuiltinToolName::ToolCatalog.as_str()
            && output.status == ExecutionResultStatus::Succeeded
            && task_can_delegate(Some(task), Some(agent_role_registry))
            && let Ok(mut payload) = serde_json::from_str::<serde_json::Value>(&output.payload)
        {
            payload["session_agents"] = serde_json::json!(crate::agent_activation::retained_agent_heads(
                task_store, session_store, session_id, task,
            ).into_iter()
                .map(|head| serde_json::json!({
                    "activation":head.agent_activation(), "task_id":head.task_id,
                    "title":head.title, "status":head.status, "role":head.executor_binding_target_role(),
                })).collect::<Vec<_>>());
            return (payload.to_string(), output.status);
        }
        (output.payload, output.status)
    };

    let mut result = execute_runtime_tool(access_profile);
    if result.1 == ExecutionResultStatus::NeedsApproval && !approval_granted {
        if !approval_resume_is_safe(&result.0) {
            return approval_resume_contract_failure(&tool_call.function.name);
        }
        let runtime_decision = ToolPreflightDecision {
            payload: result.0,
            status: result.1,
        };
        match run_with_task_execution_wait(
            task_store,
            execution_registry,
            task,
            &tool_call.function.name,
            || {
                await_task_tool_approval(
                    event_bus,
                    conversation_registry,
                    task_store,
                    session_store,
                    task,
                    session_id,
                    workspace_id,
                    workspace_root_path,
                    project_directory_map.as_ref(),
                    tool_call,
                    &runtime_decision,
                    on_progress,
                    execution_allowed,
                )
            },
            execution_allowed,
        )
        .and_then(std::convert::identity)
        {
            Ok(()) => result = execute_runtime_tool(AccessProfile::FullAccess),
            Err(rejected) => return rejected,
        }
    }
    if result.1 == ExecutionResultStatus::NeedsApproval {
        return approval_resume_contract_failure(&tool_call.function.name);
    }
    result
}

/// 等待外部答复时释放执行名额，答复后重新准入；不修改工作区资源归属。
fn run_with_task_execution_wait<T>(
    task_store: &TaskStore,
    execution_registry: &TaskExecutionRegistry,
    task: &magi_core::Task,
    tool: &str,
    run: impl FnOnce() -> T,
    execution_allowed: &(dyn Fn() -> bool + Sync),
) -> Result<T, (String, ExecutionResultStatus)> {
    let waiting = execution_registry
        .execution_admission()
        .suspend_while_waiting(&task.task_id);
    let result = run();
    if !execution_allowed()
        || waiting.as_ref().is_some_and(|guard| {
            !guard.resume(|| {
                execution_allowed()
                    && task_store.get_task(&task.task_id).is_some_and(|current| {
                        matches!(current.status, TaskStatus::Pending | TaskStatus::Running)
                    })
            })
        })
    {
        return Err((
            serde_json::json!({"tool":tool,"status":"cancelled",
                "error_code":"task_stopped_while_waiting","execution_started":false,
            })
            .to_string(),
            ExecutionResultStatus::Cancelled,
        ));
    }
    Ok(result)
}

/// 同一会话里除 `task` 以外是否还有任务在运行：它们仍在使用工作区，
/// 所以等人期间不能让出仓库执行租约。
fn session_has_other_active_tasks(task_store: &TaskStore, task: &magi_core::Task) -> bool {
    task_store.all_tasks().iter().any(|other| {
        other.mission_id == task.mission_id
            && other.task_id != task.task_id
            && matches!(other.status, TaskStatus::Pending | TaskStatus::Running)
    })
}

/// 交互必须绑定发起调用所在的 canonical Turn。独立 Agent Turn 不会改写
/// Session 当前 Turn 指针，因此不能只从 runtime sidecar 读取目标。
fn execute_execution_steps(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    let turn_id = task_execution_turn_id(session_store, session_id, &task.task_id);
    let result = (|| -> Result<magi_session_store::CanonicalItemCommitNotice, String> {
        let turn_id = turn_id
            .as_deref()
            .ok_or("当前 Task 没有关联的可写入 Turn")?;
        let input: execution_steps_protocol::UpdateExecutionStepsParams =
            serde_json::from_str(&call.function.arguments).map_err(|e| e.to_string())?;
        for step in &input.steps {
            for child_id in step.task_ids.iter().flatten() {
                let child = task_store
                    .get_task(&TaskId::new(child_id))
                    .ok_or("关联子任务不存在")?;
                if child.parent_task_id.as_ref() != Some(&task.task_id)
                    || child.mission_id != task.mission_id
                {
                    return Err("只能关联当前 Task 的直接子任务".into());
                }
                if step.status == execution_steps_protocol::ExecutionStepStatus::Completed
                    && child.status != TaskStatus::Completed
                {
                    return Err("关联子任务尚未成功完成，不能完成步骤".into());
                }
            }
        }
        session_store
            .update_execution_steps(session_id, turn_id, &task.task_id, input)
            .map_err(|e| e.to_string())
    })();
    match result {
        Ok(notice) => {
            let response = serde_json::json!({"tool":"update_execution_steps","status":"succeeded","execution":notice.item.metadata.get("executionSteps")});
            publish_committed_canonical_item_event(event_bus, notice);
            (response.to_string(), ExecutionResultStatus::Succeeded)
        }
        Err(error) => {
            let current = turn_id.as_deref().and_then(|id| {
                session_store
                    .execution_steps_for_task(session_id, id, &task.task_id)
                    .ok()
                    .flatten()
            });
            let code = "execution_steps_invalid_update";
            let failure=magi_core::ToolFailure::coded("update_execution_steps",code,error)
                .instruction("按 current_steps 里的稳定 stepId 修正后重试，每次提交完整步骤列表。只能在步骤 running 期间有成功执行后完成步骤；取消、失败和阻塞要填写原因。")
                .with("current_steps",serde_json::to_value(current).expect("steps serializable"));
            (failure.into_payload(), ExecutionResultStatus::Failed)
        }
    }
}

fn task_execution_turn_id(
    session_store: &SessionStore,
    session_id: &SessionId,
    task_id: &TaskId,
) -> Option<String> {
    let owned = session_store
        .agent_turn_for_task(session_id, task_id)
        .ok()?;
    if let Some(turn) = owned {
        return session_store
            .turn_execution_is_active(session_id, &turn.turn_id)
            .then_some(turn.turn_id);
    }
    session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .filter(|turn| {
            turn.items
                .iter()
                .any(|item| item.task_id.as_ref() == Some(task_id))
        })
        .map(|turn| turn.turn_id)
}

/// `ask_user_question`：把选择题交给用户，阻塞到用户回答、跳过，或轮次 / 任务结束。
/// 没有超时——用户可能需要想很久；收口条件只有回答、跳过与停止。
#[allow(clippy::too_many_arguments)]
fn await_user_question_answer(
    event_bus: &InMemoryEventBus,
    conversation_registry: &ConversationRegistry,
    task_store: &TaskStore,
    session_store: &SessionStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    tool_call: &ChatToolCall,
    on_progress: Option<&(dyn Fn(ToolExecutionProgress) -> Result<(), String> + Sync)>,
    execution_allowed: &(dyn Fn() -> bool + Sync),
) -> (String, ExecutionResultStatus) {
    let tool_name = tool_call.function.name.as_str();
    let failed = |code: &str, message: String| {
        (
            serde_json::json!({
                "tool": tool_name,
                "status": "failed",
                "error_code": code,
                "error": message,
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        )
    };
    let cancelled = |message: &str| {
        (
            serde_json::json!({
                "tool": tool_name,
                "status": "cancelled",
                "error_code": "user_question_cancelled",
                "error": message,
            })
            .to_string(),
            ExecutionResultStatus::Cancelled,
        )
    };
    let questions = match crate::user_question::parse_questions(&tool_call.function.arguments) {
        Ok(questions) => questions,
        Err(message) => return failed("ask_user_question_invalid", message),
    };
    let Some(turn_id) =
        task_execution_turn_id(session_store, session_id, &task.task_id).filter(|turn_id| {
            session_store
                .canonical_turn_for_session_turn_id(session_id, turn_id)
                .is_some_and(|turn| !turn.status.is_terminal() && turn.completed_at.is_none())
        })
    else {
        return cancelled("当前对话轮次已经结束，问题没有发给用户");
    };
    let registry = conversation_registry.user_questions();
    let question_id = EventId::unique("user-question").to_string();
    let request = crate::PendingUserQuestion {
        question_id: question_id.clone(),
        session_id: session_id.clone(),
        task_id: Some(task.task_id.clone()),
        turn_id: turn_id.clone(),
        tool_call_id: tool_call.id.clone(),
        questions: questions.clone(),
        requested_at: UtcMillis::now(),
        can_skip: true,
    };
    let waiter = match registry.request_persisted(session_store, request.clone()) {
        Ok(waiter) => waiter,
        Err(error) => return failed("user_question_persist_failed", error),
    };
    if let Some(on_progress) = on_progress
        && let Err(error) = on_progress(ToolExecutionProgress {
            tool_call_id: ToolCallId::new(&tool_call.id),
            tool_name: tool_name.to_string(),
            payload: serde_json::json!({
                "tool": tool_name,
                "status": "awaiting_user_input",
                "question_id": question_id,
                "questions": request.questions,
            })
            .to_string(),
        })
    {
        registry.cancel(&question_id);
        return failed(
            "user_question_persist_failed",
            format!("问题持久化失败：{error}"),
        );
    }
    let publish = |event_name: &str, extra: serde_json::Value| {
        let mut payload = serde_json::json!({
            "session_id": session_id,
            "workspace_id": workspace_id,
            "task_id": task.task_id,
            "turn_id": turn_id,
            "tool_call_id": tool_call.id,
            "question_id": question_id,
        });
        if let (Some(target), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
            target.extend(extra.clone());
        }
        let _ = event_bus.publish(
            EventEnvelope::domain(EventId::unique(event_name), event_name, payload).with_context(
                EventContext {
                    workspace_id: workspace_id.clone(),
                    session_id: Some(session_id.clone()),
                    mission_id: Some(task.mission_id.clone()),
                    task_id: Some(task.task_id.clone()),
                    ..EventContext::default()
                },
            ),
        );
    };
    publish("user.question.requested", serde_json::json!({}));
    // 等用户期间不占 repository 执行租约；回答 / 跳过后在把结果交还模型前抢回。
    let human_wait = conversation_registry.human_wait().begin(
        session_id,
        !session_has_other_active_tasks(task_store, task),
    );
    let reacquire_execution = || {
        human_wait.finish(|| {
            execution_allowed()
                && task_store.get_task(&task.task_id).is_some_and(|current| {
                    matches!(current.status, TaskStatus::Pending | TaskStatus::Running)
                })
                && session_store.turn_execution_is_active(session_id, &turn_id)
        })
    };

    loop {
        match waiter
            .response_rx
            .recv_timeout(std::time::Duration::from_millis(250))
        {
            Ok(crate::UserQuestionResponse::Answered { answers }) => {
                if !reacquire_execution() {
                    return cancelled("任务或对话轮次已停止，回答没有交给模型");
                }
                return (
                    crate::user_question::answered_result_payload(&questions, &answers).to_string(),
                    ExecutionResultStatus::Succeeded,
                );
            }
            Ok(crate::UserQuestionResponse::Skipped) => {
                if !reacquire_execution() {
                    return cancelled("任务或对话轮次已停止，回答没有交给模型");
                }
                return (
                    serde_json::json!({
                        "tool": tool_name,
                        "status": "skipped",
                        "message": "用户选择不回答这些问题。不要反复追问：按你的最佳判断继续，并在回复里说明你做的假设。",
                    })
                    .to_string(),
                    ExecutionResultStatus::Succeeded,
                );
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let task_is_active = execution_allowed()
                    && task_store.get_task(&task.task_id).is_some_and(|current| {
                        matches!(current.status, TaskStatus::Pending | TaskStatus::Running)
                    });
                let turn_is_active = session_store.turn_execution_is_active(session_id, &turn_id);
                if !task_is_active || !turn_is_active {
                    registry.cancel(&question_id);
                    publish(
                        "user.question.resolved",
                        serde_json::json!({ "outcome": "cancelled" }),
                    );
                    return cancelled("任务或对话轮次已停止，问题没有得到回答");
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                registry.cancel(&question_id);
                publish(
                    "user.question.resolved",
                    serde_json::json!({ "outcome": "cancelled" }),
                );
                return failed(
                    "user_question_runtime_failed",
                    "提问等待通道已关闭".to_string(),
                );
            }
        }
    }
}

// 等待审批明确表示未执行；获批后的 running 必须先提交，才能恢复真实执行。
// 否则写盘失败后重启仍看到 awaiting_approval，会错误地允许重复副作用。
fn publish_approved_tool_progress(
    on_progress: Option<&(dyn Fn(ToolExecutionProgress) -> Result<(), String> + Sync)>,
    tool_call: &ChatToolCall,
    approval_id: &str,
) -> Result<(), (String, ExecutionResultStatus)> {
    if let Some(on_progress) = on_progress {
        on_progress(ToolExecutionProgress {
            tool_call_id: ToolCallId::new(&tool_call.id),
            tool_name: tool_call.function.name.clone(),
            payload: serde_json::json!({
                "tool": tool_call.function.name, "status": "running", "approval_id": approval_id,
            })
            .to_string(),
        })
        .map_err(|error| tool_progress_persist_failure(tool_call, error))?;
    }
    Ok(())
}

fn tool_progress_persist_failure(
    tool_call: &ChatToolCall,
    error: String,
) -> (String, ExecutionResultStatus) {
    (
        serde_json::json!({
            "tool": tool_call.function.name, "status": "failed", "execution_started": false,
            "error_code": "tool_progress_persist_failed", "error": error,
        })
        .to_string(),
        ExecutionResultStatus::Failed,
    )
}

fn tool_approval_policy_snapshot(
    task: &magi_core::Task,
    directories: Option<&magi_core::ProjectDirectoryExecutionMap>,
) -> serde_json::Value {
    let mut policy = serde_json::json!({"policy":task.policy_snapshot,"workspaceScope":task.workspace_scope,"writeScope":task.write_scope});
    if let Some(binding) = task
        .executor_binding
        .as_ref()
        .and_then(|executor| executor.project_directories.as_ref())
    {
        policy["projectDirectories"] = serde_json::json!(binding);
        policy["projectExecutionMap"] = serde_json::json!(directories);
    }
    policy
}

#[allow(clippy::too_many_arguments)]
fn await_task_tool_approval(
    event_bus: &InMemoryEventBus,
    conversation_registry: &ConversationRegistry,
    task_store: &TaskStore,
    session_store: &SessionStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    workspace_root_path: Option<&PathBuf>,
    directories: Option<&magi_core::ProjectDirectoryExecutionMap>,
    tool_call: &ChatToolCall,
    decision: &ToolPreflightDecision,
    on_progress: Option<&(dyn Fn(ToolExecutionProgress) -> Result<(), String> + Sync)>,
    execution_allowed: &(dyn Fn() -> bool + Sync),
) -> Result<(), (String, ExecutionResultStatus)> {
    let Some(turn_id) =
        task_execution_turn_id(session_store, session_id, &task.task_id).filter(|turn_id| {
            session_store
                .canonical_turn_for_session_turn_id(session_id, turn_id)
                .is_some_and(|turn| !turn.status.is_terminal() && turn.completed_at.is_none())
        })
    else {
        return Err((
            serde_json::json!({
                "tool": tool_call.function.name,
                "status": "cancelled",
                "error_code": "tool_approval_cancelled",
                "error": "当前对话轮次已经结束，待授权操作未执行",
            })
            .to_string(),
            ExecutionResultStatus::Cancelled,
        ));
    };
    let execution_root = match workspace_root_path.map(std::fs::canonicalize).transpose() {
        Ok(root) => root,
        Err(error) => return Err(tool_progress_persist_failure(tool_call, error.to_string())),
    };
    let policy = tool_approval_policy_snapshot(task, directories);
    let mut sources = vec![task.task_id.clone()];
    let mut source = task.recovery_checkpoint().cloned();
    while let Some(checkpoint) = source {
        let Some(previous) = task_store.get_task(&checkpoint.source_task_id) else {
            return Err(tool_progress_persist_failure(
                tool_call,
                "审批恢复来源任务缺失".into(),
            ));
        };
        if checkpoint.source_session_id != *session_id
            || sources.contains(&previous.task_id)
            || previous.mission_id != task.mission_id
            || previous.workspace_scope != task.workspace_scope
            || previous.write_scope != task.write_scope
            || previous
                .executor_binding
                .as_ref()
                .and_then(|executor| executor.project_directories.as_ref())
                != task
                    .executor_binding
                    .as_ref()
                    .and_then(|executor| executor.project_directories.as_ref())
        {
            return Err(tool_progress_persist_failure(
                tool_call,
                "审批恢复来源越界或存在循环".into(),
            ));
        }
        sources.push(previous.task_id.clone());
        source = previous.recovery_checkpoint().cloned();
    }
    let claim = |id: Option<&str>| {
        let current = task_store.get_task(&task.task_id).ok_or_else(|| {
            tool_progress_persist_failure(tool_call, "审批目标任务已不存在".into())
        })?;
        let current_policy = tool_approval_policy_snapshot(&current, directories);
        if !execution_allowed()
            || !matches!(current.status, TaskStatus::Pending | TaskStatus::Running)
            || current_policy != policy
        {
            return Err(tool_progress_persist_failure(
                tool_call,
                "审批等待期间任务已停止或权限范围已变化，需要按当前权限重新评估".into(),
            ));
        }
        session_store
            .claim_tool_approval(
                session_id,
                &task.task_id,
                &tool_call.id,
                &tool_call.function.name,
                &tool_call.function.arguments,
                execution_root.as_deref(),
                &policy,
                &sources,
                id,
            )
            .map_err(|error| tool_progress_persist_failure(tool_call, error.to_string()))
    };
    if let Some(receipt) = claim(None)? {
        if receipt.decision == crate::ToolApprovalDecision::Deny {
            return Err(crate::tool_approval::rejected_tool_approval_result(
                &tool_call.function.name,
                &receipt.approval_id,
                true,
            ));
        }
        return publish_approved_tool_progress(on_progress, tool_call, &receipt.approval_id);
    }
    let registry = conversation_registry.tool_approvals();
    let approval_id = EventId::unique("tool-approval").to_string();
    let reason = serde_json::from_str::<serde_json::Value>(&decision.payload)
        .ok()
        .and_then(|payload| {
            payload
                .get("error")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "该操作需要用户授权".to_string());
    let request = crate::PendingToolApproval {
        approval_id: approval_id.clone(),
        session_id: session_id.clone(),
        task_id: task.task_id.clone(),
        turn_id: turn_id.clone(),
        tool_call_id: tool_call.id.clone(),
        tool_name: tool_call.function.name.clone(),
        reason: reason.clone(),
        requested_at: UtcMillis::now(),
        agent: crate::tool_approval::ApprovalRequestAgent::for_task(task),
    };
    let waiter = match registry.request_persisted(
        session_store,
        request.clone(),
        &tool_call.function.arguments,
        execution_root.clone(),
        policy.clone(),
    ) {
        Ok(crate::ToolApprovalRequestOutcome::AlreadyAllowed) => {
            if claim(Some(&approval_id))?.is_none() {
                return Err(tool_progress_persist_failure(
                    tool_call,
                    "审批批准不可领取".into(),
                ));
            }
            return publish_approved_tool_progress(on_progress, tool_call, &approval_id);
        }
        Ok(crate::ToolApprovalRequestOutcome::PreviouslyDenied) => {
            return Err(crate::tool_approval::rejected_tool_approval_result(
                &tool_call.function.name,
                &approval_id,
                true,
            ));
        }
        Ok(crate::ToolApprovalRequestOutcome::Pending(waiter)) => waiter,
        Err(error) => {
            return Err((
                serde_json::json!({
                    "tool": tool_call.function.name,
                    "status": "failed",
                    "error_code": "tool_approval_runtime_failed",
                    "error": error,
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            ));
        }
    };

    let progress_payload = serde_json::json!({
        "tool": tool_call.function.name,
        "status": "awaiting_approval",
        "error_code": "tool_policy_needs_approval",
        "error": reason,
        "approval_id": approval_id,
        "approval": request,
    })
    .to_string();
    if let Some(on_progress) = on_progress
        && let Err(error) = on_progress(ToolExecutionProgress {
            tool_call_id: ToolCallId::new(&tool_call.id),
            tool_name: tool_call.function.name.clone(),
            payload: progress_payload.clone(),
        })
    {
        registry.cancel(&approval_id);
        return Err(tool_progress_persist_failure(tool_call, error));
    }
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::unique("event-tool-approval-requested"),
            "tool.approval.requested",
            serde_json::json!({
                "session_id": session_id,
                "workspace_id": workspace_id,
                "task_id": task.task_id,
                "turn_id": turn_id,
                "tool_call_id": tool_call.id,
                "tool_name": tool_call.function.name,
                "approval_id": waiter.request.approval_id,
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

    // 等授权期间不占 repository 执行租约；授权结果出来后在继续前抢回。
    let human_wait = conversation_registry.human_wait().begin(
        session_id,
        !session_has_other_active_tasks(task_store, task),
    );
    let reacquire_execution = || {
        human_wait.finish(|| {
            execution_allowed()
                && task_store.get_task(&task.task_id).is_some_and(|current| {
                    matches!(current.status, TaskStatus::Pending | TaskStatus::Running)
                })
                && session_store.turn_execution_is_active(session_id, &turn_id)
        })
    };
    let stopped_while_reacquiring = || {
        (
            serde_json::json!({
                "tool": tool_call.function.name,
                "status": "cancelled",
                "error_code": "tool_approval_cancelled",
                "error": "任务或对话轮次已停止，待授权操作未执行",
                "approval_id": approval_id,
            })
            .to_string(),
            ExecutionResultStatus::Cancelled,
        )
    };

    // 子代理等待审批时主线并不知情：通知父任务，让它的 agent_wait 立即返回
    // attention_required，而不是等到超时才发现子代理卡在审批上。
    if let Some(parent_task_id) = task.parent_task_id.as_ref() {
        let signal = RuntimeSignal {
            author: MailboxAuthor::Child(task.task_id.to_string()),
            kind: MailboxKind::Message,
            trigger_turn: true,
            payload: serde_json::json!({
                "type": "agent_awaiting_approval",
                "child_task_id": task.task_id,
                "tool_name": tool_call.function.name,
                "reason": reason,
                "approval_id": approval_id,
            }),
            enqueued_at: UtcMillis::now(),
        };
        if conversation_registry
            .enqueue_task_signal(session_id, parent_task_id, signal)
            .is_ok()
        {
            task_store.notify_runtime_change();
        }
    }

    loop {
        match waiter
            .decision_rx
            .recv_timeout(std::time::Duration::from_millis(250))
        {
            Ok(
                crate::ToolApprovalDecision::AllowOnce | crate::ToolApprovalDecision::AllowForTurn,
            ) => {
                let task_is_active = execution_allowed()
                    && task_store.get_task(&task.task_id).is_some_and(|current| {
                        matches!(current.status, TaskStatus::Pending | TaskStatus::Running)
                    });
                let turn_is_active = session_store.turn_execution_is_active(session_id, &turn_id);
                if !task_is_active || !turn_is_active {
                    if !turn_is_active {
                        registry.remove_turn(session_id, &turn_id);
                    } else {
                        registry.remove_task(session_id, &task.task_id);
                    }
                    return Err((
                        serde_json::json!({
                            "tool": tool_call.function.name,
                            "status": "cancelled",
                            "error_code": "tool_approval_cancelled",
                            "error": "任务或对话轮次已停止，待授权操作未执行",
                            "approval_id": approval_id,
                        })
                        .to_string(),
                        ExecutionResultStatus::Cancelled,
                    ));
                }
                if !reacquire_execution() {
                    return Err(stopped_while_reacquiring());
                }
                if claim(Some(&approval_id))?.is_none() {
                    return Err(tool_progress_persist_failure(
                        tool_call,
                        "审批批准已过期或不可领取".into(),
                    ));
                }
                return publish_approved_tool_progress(on_progress, tool_call, &approval_id);
            }
            Ok(crate::ToolApprovalDecision::Deny) => {
                if !reacquire_execution() {
                    return Err(stopped_while_reacquiring());
                }
                return Err(crate::tool_approval::rejected_tool_approval_result(
                    &tool_call.function.name,
                    &approval_id,
                    false,
                ));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if registry.is_expired(session_id, &approval_id) {
                    if !reacquire_execution() {
                        return Err(stopped_while_reacquiring());
                    }
                    return Err(crate::tool_approval::expired_tool_approval_result(
                        &tool_call.function.name,
                        &approval_id,
                    ));
                }
                let task_is_active = execution_allowed()
                    && task_store.get_task(&task.task_id).is_some_and(|current| {
                        matches!(current.status, TaskStatus::Pending | TaskStatus::Running)
                    });
                let turn_is_active = session_store.turn_execution_is_active(session_id, &turn_id);
                if !task_is_active || !turn_is_active {
                    if !turn_is_active {
                        registry.remove_turn(session_id, &turn_id);
                    } else {
                        registry.remove_task(session_id, &task.task_id);
                    }
                    return Err((
                        serde_json::json!({
                            "tool": tool_call.function.name,
                            "status": "cancelled",
                            "error_code": "tool_approval_cancelled",
                            "error": "任务已停止，待授权操作未执行",
                            "approval_id": approval_id,
                        })
                        .to_string(),
                        ExecutionResultStatus::Cancelled,
                    ));
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if registry.is_expired(session_id, &approval_id) {
                    if !reacquire_execution() {
                        return Err(stopped_while_reacquiring());
                    }
                    return Err(crate::tool_approval::expired_tool_approval_result(
                        &tool_call.function.name,
                        &approval_id,
                    ));
                }
                let task_is_active = execution_allowed()
                    && task_store.get_task(&task.task_id).is_some_and(|current| {
                        matches!(current.status, TaskStatus::Pending | TaskStatus::Running)
                    });
                let turn_is_active = session_store.turn_execution_is_active(session_id, &turn_id);
                if !task_is_active || !turn_is_active {
                    if !turn_is_active {
                        registry.remove_turn(session_id, &turn_id);
                    } else {
                        registry.remove_task(session_id, &task.task_id);
                    }
                    return Err((
                        serde_json::json!({
                            "tool": tool_call.function.name,
                            "status": "cancelled",
                            "error_code": "tool_approval_cancelled",
                            "error": "任务或对话轮次已停止，待授权操作未执行",
                            "approval_id": approval_id,
                        })
                        .to_string(),
                        ExecutionResultStatus::Cancelled,
                    ));
                }
                registry.cancel(&approval_id);
                return Err((
                    serde_json::json!({
                        "tool": tool_call.function.name,
                        "status": "failed",
                        "error_code": "tool_approval_runtime_failed",
                        "error": "工具授权等待通道已关闭",
                        "approval_id": approval_id,
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                ));
            }
        }
    }
}

fn task_tool_execution_policy_scope_with_access_profile(
    task: &magi_core::Task,
    access_profile: AccessProfile,
) -> ToolExecutionPolicy {
    let Some(policy) = task.policy_snapshot.as_ref() else {
        return tool_execution_policy_scope(access_profile, "", &[], &[]);
    };
    let mut tool_policy = tool_execution_policy_scope(
        access_profile,
        policy.command_mode.clone(),
        &policy.allowed_paths,
        &policy.denied_paths,
    );
    tool_policy.read_only_paths = policy.read_only_paths.clone();
    tool_policy
}

#[cfg(test)]
fn task_policy_tool_decision(
    task: &magi_core::Task,
    requested_tool_name: &str,
    arguments: &str,
) -> Option<ToolPreflightDecision> {
    policy_tool_decision(
        task.policy_snapshot.as_ref()?,
        requested_tool_name,
        arguments,
        None,
    )
}

#[cfg(test)]
fn task_tool_preflight_decision(
    task: &magi_core::Task,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    requested_tool_name: &str,
    arguments: &str,
    workspace_root_path: Option<&PathBuf>,
) -> Option<ToolPreflightDecision> {
    policy_tool_preflight_decision(
        task.policy_snapshot.as_ref(),
        safety_gate,
        requested_tool_name,
        arguments,
        workspace_root_path,
    )
}

#[cfg(test)]
pub(crate) fn policy_tool_preflight_decision(
    policy_snapshot: Option<&TaskPolicy>,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    requested_tool_name: &str,
    arguments: &str,
    workspace_root_path: Option<&PathBuf>,
) -> Option<ToolPreflightDecision> {
    policy_tool_preflight_decision_in_context(
        policy_snapshot,
        safety_gate,
        requested_tool_name,
        arguments,
        workspace_root_path,
        None,
    )
}

pub(crate) fn policy_tool_preflight_decision_in_context(
    policy_snapshot: Option<&TaskPolicy>,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    requested_tool_name: &str,
    arguments: &str,
    workspace_root_path: Option<&PathBuf>,
    context: Option<&ToolExecutionContext>,
) -> Option<ToolPreflightDecision> {
    let task_policy_decision = policy_snapshot.and_then(|policy| {
        policy_tool_decision_in_context(
            policy,
            requested_tool_name,
            arguments,
            workspace_root_path,
            context,
        )
    });
    // S8：SafetyGate 语义判定。它和 TaskPolicy 都属于执行前判定：
    // HardBlock 必须压过常规风险拦截，TaskPolicy 的 Rejected 也不能被 SafetyGate
    // 的 NeedsApproval 降级。
    let access_profile = policy_snapshot
        .map(magi_core::TaskPolicy::effective_access_profile)
        .unwrap_or_default();
    let safety_gate_decision = safety_gate.and_then(|gate| {
        safety_gate_tool_decision(gate, access_profile, requested_tool_name, arguments)
    });
    select_preflight_decision(task_policy_decision, safety_gate_decision)
}

#[cfg(test)]
fn task_policy_tool_decision_with_workspace_root(
    task: &magi_core::Task,
    requested_tool_name: &str,
    arguments: &str,
    workspace_root_path: Option<&PathBuf>,
) -> Option<ToolPreflightDecision> {
    policy_tool_decision(
        task.policy_snapshot.as_ref()?,
        requested_tool_name,
        arguments,
        workspace_root_path,
    )
}

/// 按策略快照做访问档判定。Task 执行与外部（MCP）调用共用这一份判定，
/// 外部调用使用合成的 `TaskPolicy`，不需要构造 Task。
pub(crate) fn policy_tool_decision(
    policy_snapshot: &TaskPolicy,
    requested_tool_name: &str,
    arguments: &str,
    workspace_root_path: Option<&PathBuf>,
) -> Option<ToolPreflightDecision> {
    policy_tool_decision_in_context(
        policy_snapshot,
        requested_tool_name,
        arguments,
        workspace_root_path,
        None,
    )
}

fn policy_tool_decision_in_context(
    policy_snapshot: &TaskPolicy,
    requested_tool_name: &str,
    arguments: &str,
    workspace_root_path: Option<&PathBuf>,
    context: Option<&ToolExecutionContext>,
) -> Option<ToolPreflightDecision> {
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

    access_profile_tool_decision_in_context(
        AccessProfileToolDecisionInput {
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
        },
        context,
    )
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

#[cfg(test)]
pub(crate) fn access_profile_tool_decision(
    input: AccessProfileToolDecisionInput<'_>,
) -> Option<ToolPreflightDecision> {
    access_profile_tool_decision_in_context(input, None)
}

fn access_profile_tool_decision_in_context(
    input: AccessProfileToolDecisionInput<'_>,
    context: Option<&ToolExecutionContext>,
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
    let mut canonical_policy = magi_permissions::PermissionPolicy {
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
    if let Some(context) = context {
        canonical_policy = magi_tool_runtime::execution_permission_policy(
            &ToolExecutionPolicy {
                access_profile,
                command_mode: command_mode.into(),
                allowed_tool_names: (!allowed_tools.is_empty()).then(|| allowed_tools.to_vec()),
                denied_tool_names: denied_tools.to_vec(),
                allowed_paths: allowed_paths.to_vec(),
                denied_paths: denied_paths.to_vec(),
                read_only_paths: read_only_paths.to_vec(),
                ..Default::default()
            },
            context,
        );
    }
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
    // 浏览器工具对网页的写动作按访问档位判定：只读拒绝，受限下执行脚本 / 改站点存储需要确认。
    if let Some(class) = magi_tool_runtime::browser_action_class(&canonical_tool_name, arguments) {
        let browser_request = magi_permissions::PermissionRequest::BrowserAction { class };
        if let Some(decision) = select_access_profile_axis_decision(
            &mut pending_decision,
            permission_decision_payload(
                &canonical_tool_name,
                engine.decide(
                    &browser_request,
                    &canonical_policy,
                    effective_access_profile,
                ),
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
        if context.is_some_and(|context| {
            context
                .references_retired_project_root(&path_request.absolute_path)
                .unwrap_or(true)
        }) {
            return Some(task_policy_decision_payload(
                &canonical_tool_name,
                ExecutionResultStatus::Rejected,
                "路径指向已隔离的源目录，工具未执行".into(),
                Some(effective_access_profile),
            ));
        }
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
        if let Some(decision) = select_access_profile_axis_decision(
            &mut pending_decision,
            permission_decision_payload(
                &canonical_tool_name,
                magi_tool_runtime::decide_tool_path_access(
                    &engine,
                    &canonical_policy,
                    canonical_tool_name.as_str(),
                    &path_request,
                    allowed_paths,
                    effective_access_profile,
                ),
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
    let restriction_kind = policy_restriction_kind(status, &reason);
    let constrained_by_skill = restriction_kind == "skill_tool_scope";
    let constrained_by_access_profile = reason.starts_with("只读任务")
        || reason.starts_with("受限执行")
        || reason.starts_with("只读访问");
    let (error_code, public_error, required_access_profile, instruction) = match status {
        ExecutionResultStatus::NeedsApproval => (
            "tool_policy_needs_approval",
            TOOL_POLICY_NEEDS_APPROVAL_PUBLIC_ERROR,
            Some("full_access"),
            "当前调用已暂停并等待用户授权，不要重复调用。若用户拒绝，再改用专用读取、搜索工具或缩小操作范围。",
        ),
        ExecutionResultStatus::Rejected => (
            "tool_policy_rejected",
            if constrained_by_skill {
                "当前激活 Skill 不允许调用该工具"
            } else if !constrained_by_access_profile
                || access_profile == Some(magi_core::AccessProfile::FullAccess)
            {
                TOOL_POLICY_CONTEXT_REJECTED_PUBLIC_ERROR
            } else {
                "该工具在当前访问模式下不可用"
            },
            constrained_by_access_profile.then_some("full_access"),
            if constrained_by_skill {
                "不要重复相同调用。请使用当前 Skill 明确允许的工具；Skill 被移除时需要重新选择有效 Skill。"
            } else if constrained_by_access_profile {
                "不要重复相同调用。只读目的必须改用专用读取、搜索工具；需要写入时应等待当前授权结果。"
            } else {
                "不要重复相同调用。该工具受当前任务角色、路径范围或显式策略限制，请使用当前任务允许的工具和路径。"
            },
        ),
        _ => (
            "tool_policy_failed",
            "该工具暂不可用",
            None,
            "不要重复相同调用，请改用当前可用工具继续。",
        ),
    };
    tracing::warn!(
        tool_name,
        status = %status.wire_label(),
        access_profile = access_profile.map(|profile| profile.as_str()).unwrap_or_default(),
        reason = %reason,
        "tool preflight policy decision"
    );
    ToolPreflightDecision {
        payload: serde_json::json!({
            "tool": tool_name,
            "status": status.wire_label(),
            "error_code": error_code,
            "error": public_error,
            "access_profile": access_profile.map(|profile| profile.as_str()),
            "required_access_profile": required_access_profile,
            "restriction_kind": restriction_kind,
            "retryable_with_same_arguments": false,
            "instruction": instruction,
        })
        .to_string(),
        status,
    }
}

fn policy_restriction_kind(status: ExecutionResultStatus, reason: &str) -> &'static str {
    if status == ExecutionResultStatus::NeedsApproval {
        return "approval_required";
    }
    if reason.starts_with("tool_not_in_allowed_scope:")
        || reason.starts_with("tool_explicitly_denied:")
    {
        return "skill_tool_scope";
    }
    if reason.starts_with("只读任务") || reason.starts_with("只读访问") {
        return "read_only";
    }
    if reason.starts_with("策略拒绝访问路径") || reason.starts_with("策略未授权访问路径")
    {
        return "path_scope";
    }
    if reason.starts_with("任务策略拒绝工具") || reason.starts_with("任务策略未授权工具")
    {
        return "tool_scope";
    }
    "policy"
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
            "status": ExecutionResultStatus::Rejected.wire_label(),
            "error_code": "tool_policy_rejected",
            "error": TOOL_VISIBILITY_REJECTED_PUBLIC_ERROR,
            "reason_code": "model_call_scope_mismatch",
            "model_call_scope": magi_tool_runtime::BuiltinToolName::from_name(tool_name)
                .map(|tool| tool.model_call_scope().as_str()),
            "task_id": task.task_id,
            "retryable_with_same_arguments": false,
            "instruction": "不要重复相同调用，请改用当前可用工具或直接返回文本结果。",
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

pub(crate) struct SafetyEvaluationAuditContext<'a> {
    pub access_profile: magi_core::AccessProfile,
    pub session_id: Option<&'a SessionId>,
    pub workspace_id: Option<&'a WorkspaceId>,
    pub mission_id: Option<&'a magi_core::MissionId>,
    pub task_id: Option<&'a TaskId>,
}

pub(crate) fn publish_safety_evaluation_audit(
    event_bus: &InMemoryEventBus,
    gate: &magi_safety_gate::SafetyGate,
    tool_name: &str,
    arguments: &str,
    tool_call_id: &str,
    context: SafetyEvaluationAuditContext<'_>,
) {
    let canonical_tool_name =
        canonical_builtin_tool_name(tool_name).unwrap_or_else(|| tool_name.trim().to_string());
    let evaluation = gate.evaluate_with_evidence(&canonical_tool_name, arguments);
    if evaluation.matches.is_empty() {
        return;
    }

    let decision = match &evaluation.decision {
        magi_safety_gate::SafetyDecision::Allow => "allow",
        magi_safety_gate::SafetyDecision::HardBlock { .. } => "hard_block",
        magi_safety_gate::SafetyDecision::RequireApprovalInRestricted { .. } => {
            "require_approval_in_restricted"
        }
        magi_safety_gate::SafetyDecision::AuditOnly { .. } => "audit_only",
    };
    let _ = event_bus.publish(
        EventEnvelope::audit(
            EventId::unique(format!("event-safety-evaluation-{}", tool_call_id)),
            "security.safety.evaluated",
            serde_json::json!({
                "tool_name": canonical_tool_name,
                "tool_call_id": tool_call_id,
                "access_profile": format!("{:?}", context.access_profile),
                "decision": decision,
                "matched_rules": evaluation.matches.iter().map(|matched| serde_json::json!({
                    "category": matched.category.as_str(),
                    "pattern": matched.pattern,
                    "action": matched.action.as_str(),
                })).collect::<Vec<_>>(),
            }),
        )
        .with_context(EventContext {
            workspace_id: context.workspace_id.cloned(),
            session_id: context.session_id.cloned(),
            mission_id: context.mission_id.cloned(),
            task_id: context.task_id.cloned(),
            ..EventContext::default()
        }),
    );
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
        status = %status.wire_label(),
        category = category.as_str(),
        action = action.as_str(),
        pattern = %pattern,
        reason = %reason,
        "tool preflight safety gate decision"
    );
    ToolPreflightDecision {
        payload: serde_json::json!({
            "tool": tool_name,
            "status": status.wire_label(),
            "error_code": public_error.error_code,
            "error": public_error.error,
            "retryable_with_same_arguments": status == ExecutionResultStatus::Rejected,
            "instruction": if status == ExecutionResultStatus::Rejected {
                "该安全策略拒绝不可重试的相同调用，请改用安全替代方案。"
            } else {
                "当前调用已暂停并等待用户授权，不要重复调用。"
            },
        })
        .to_string(),
        status,
    }
}

#[cfg(test)]
mod tests;
