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

mod agent_tools;
use agent_tools::*;
mod context_tools;
pub(crate) use context_tools::context_reference_ids;
use context_tools::*;
mod tool_policy;
#[cfg(test)]
pub(crate) use tool_policy::AccessProfileToolDecisionInput;
#[cfg(test)]
pub(crate) use tool_policy::access_profile_tool_decision;
use tool_policy::*;
pub(crate) use tool_policy::{
    SafetyEvaluationAuditContext, ToolPreflightDecision, policy_tool_decision,
    policy_tool_preflight_decision_in_context, publish_safety_evaluation_audit,
};
mod coordinator_tools;
pub(crate) use coordinator_tools::execute_goal_tool;
use coordinator_tools::*;

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

/// 一批工具调用执行所需的运行环境。调用方按字段名构造，避免 25 个位置参数的顺序错误。
#[derive(Clone)]
pub(crate) struct TaskToolBatchEnv<'a> {
    pub(crate) event_bus: &'a InMemoryEventBus,
    pub(crate) tool_registry: Option<&'a ToolRegistry>,
    pub(crate) agent_role_registry: &'a magi_agent_role::AgentRoleRegistry,
    pub(crate) skill_runtime: Option<&'a magi_skill_runtime::SkillRuntime>,
    pub(crate) skill_dispatch_runtime: Option<&'a magi_skill_runtime::SkillDispatchRuntime>,
    pub(crate) skill_name: Option<&'a str>,
    pub(crate) task_store: &'a TaskStore,
    pub(crate) session_store: &'a SessionStore,
    pub(crate) execution_registry: &'a TaskExecutionRegistry,
    pub(crate) conversation_registry: &'a ConversationRegistry,
    pub(crate) safety_gate: Option<&'a magi_safety_gate::SafetyGate>,
    pub(crate) plan_store: &'a magi_plan::PlanStore,
    pub(crate) project_memory: Option<&'a magi_project_memory::ProjectMemoryStore>,
    pub(crate) task: &'a magi_core::Task,
    pub(crate) session_id: &'a SessionId,
    pub(crate) workspace_id: &'a Option<WorkspaceId>,
    pub(crate) workspace_root_path: Option<&'a PathBuf>,
    pub(crate) worker_id: Option<&'a magi_core::WorkerId>,
    pub(crate) browser_capability_snapshot: Option<BrowserCapabilitySnapshot>,
    pub(crate) on_progress:
        Option<&'a (dyn Fn(ToolExecutionProgress) -> Result<(), String> + Sync)>,
    pub(crate) workspace_resources: Option<crate::workspace_execution::WorkspaceExecutionResources>,
    pub(crate) execution_group_id: Option<String>,
    pub(crate) execution_allowed: &'a (dyn Fn() -> bool + Sync),
}

pub(crate) fn execute_task_tool_call_batch(
    env: &TaskToolBatchEnv<'_>,
    tool_calls: &[ChatToolCall],
    tool_execution_ledger: &mut ToolExecutionLedger,
) -> Vec<(String, ExecutionResultStatus)> {
    let execution_allowed = env.execution_allowed;
    let execution_registry = env.execution_registry;
    let task = env.task;
    let workspace_id = env.workspace_id;
    let tool_registry = env.tool_registry;
    let workspace_resources = &env.workspace_resources;
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
            execute_task_tool_call_batch(env, &calls, tool_execution_ledger)
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
        execute_task_tool_call_batch_unchecked(env, execution_calls)
    })
}

fn execute_task_tool_call_batch_unchecked(
    env: &TaskToolBatchEnv<'_>,
    tool_calls: &[ChatToolCall],
) -> Vec<(String, ExecutionResultStatus)> {
    let TaskToolBatchEnv {
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
        browser_capability_snapshot,
        on_progress,
        workspace_resources,
        execution_group_id,
        execution_allowed,
    } = env.clone();
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
            payload: progress_payload,
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

#[cfg(test)]
mod tests;
