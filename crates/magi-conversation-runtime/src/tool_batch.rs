//! Task System v2 — tool batch / coordinator / single 工具执行入口。
//!
//! - `execute_task_tool_call_batch`：按 concurrency 分组并发或串行调度本轮工具。
//! - `execute_task_tool_call`：单工具入口，按 BuiltinToolName 走 coordinator/写工具/policy/
//!   safety gate/tool registry 各分支。
//! - `execute_coordinator_tool`：协调器工具（agent_spawn）入口。
//! - `task_policy_tool_rejection` / `safety_gate_rejection` 等支撑判定。

use std::{
    path::PathBuf,
    sync::{
        Mutex,
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
    TaskStatus, ToolCallId, UtcMillis, WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::ToolKind;
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::SessionStore;
use magi_tool_runtime::{
    BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
};

use crate::builtin_tool_schema::internal_builtin_tool_rejection_payload;
use crate::skill_apply_tool::{SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime};
use crate::{
    ConversationRegistry,
    task_execution_registry::{SpawnedChildExecutionRequest, TaskExecutionRegistry},
    task_helpers::{task_can_see_builtin_tool, task_is_long_mission},
};

/// agent_spawn 生成 child task_id 时使用的进程内单调序号。
///
/// 仅靠 `UtcMillis::now()` 在同一毫秒内的多次并行 agent_spawn 会产生重复
/// child_id，进而触发 SpawnGraph 的边冲突。配合毫秒时间戳一起拼到 task_id
/// 末尾，保证同一进程内绝对唯一。
static AGENT_SPAWN_SEQ: AtomicU64 = AtomicU64::new(0);
const AGENT_SPAWN_SUMMARY_MAX_CHARS: usize = 1200;

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

    for batch in partition_tool_calls_with_inputs(&tool_inputs) {
        match batch.kind {
            ToolBatchKind::Serial => {
                for tool_index in batch.tool_indices {
                    results[tool_index] = Some(execute_task_tool_call(
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
                    ));
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
                            (
                                tool_index,
                                scope.spawn(move || {
                                    execute_task_tool_call(
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
                                    )
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
    spawn_graph: &Mutex<magi_spawn_graph::SpawnGraph>,
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
            let role = parsed
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let goal = parsed
                .get("goal")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if role.is_empty() || goal.is_empty() {
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
            // display_name 是 LLM 提供的代理展示名，作为 Task.title 直接面向用户。
            // 长度限制 3-30 个 Unicode 字符：下限 3 既能拒绝『x』『ab』之类的占位符，
            // 又允许典型 4 字中文短语（如『探索目录』『统计行数』）这一最自然的命名密度；
            // 上限 30 防止破坏前端代理卡片版式。
            let display_name = parsed
                .get("display_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
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
                policy_snapshot: task.policy_snapshot.clone(),
                executor_binding: Some(serde_json::json!({
                    "target_role": role,
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
                            "error": format!("agent_spawn 注册 v2 子执行失败: {error}"),
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
                    "goal": goal,
                    "task_kind": format!("{:?}", task_kind),
                    "worker_id": registered_execution.worker_id.to_string(),
                    "thread_id": registered_execution.thread_id.to_string(),
                    "execution_chain_ref": registered_execution.execution_chain_ref,
                }),
            );

            // 同步阻塞：父代理本轮停在该 tool call 上，等待代理终态。
            // 后台 TaskRunner 调度线程独立运行，会持续派发本子任务到 worker；
            // 子任务终态由 TaskRunner.apply_results 写入 TaskStore，本线程在此轮询读取。
            wait_for_child_terminal_outcome(task_store, &child_id, tool, &role)
        }
        _ => unreachable!("execute_coordinator_tool 只接收 AgentSpawn 变体"),
    }
}

/// 父代理在本线程同步阻塞至子任务进入 Completed / Failed / Killed 终态，
/// 把 TaskStore 中写下的 output_refs 直接打包成 tool_call_result 返回。
///
/// - 不再依赖 mailbox 旁路信号或 Conversation 重新发起轮次；
/// - TaskRunner 后台线程独立运行，本线程阻塞不影响其调度；
/// - 父任务的租约由 TaskRunner.heartbeat 持续续期，不会因等待过期。
fn wait_for_child_terminal_outcome(
    task_store: &TaskStore,
    child_id: &TaskId,
    tool: magi_tool_runtime::BuiltinToolName,
    role: &str,
) -> (String, ExecutionResultStatus) {
    const POLL_INTERVAL_MS: u64 = 100;
    loop {
        let Some(child) = task_store.get_task(child_id) else {
            return (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "failed",
                    "child_task_id": child_id.to_string(),
                    "role": role,
                    "error": "agent_spawn 等待子任务时 TaskStore 中未找到记录",
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        };
        match child.status {
            TaskStatus::Completed => {
                let summary = compact_child_agent_output(&child.output_refs);
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "succeeded",
                        "child_task_id": child_id.to_string(),
                        "role": role,
                        "title": child.title,
                        "summary": summary,
                        "output_ref_count": child.output_refs.len(),
                    })
                    .to_string(),
                    ExecutionResultStatus::Succeeded,
                );
            }
            TaskStatus::Failed => {
                let error = child
                    .output_refs
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "子任务执行失败".to_string());
                let summary = compact_child_agent_output(&child.output_refs);
                if agent_unavailable_failure(&error) {
                    return (
                        serde_json::json!({
                            "tool": tool.as_str(),
                            "status": "degraded",
                            "child_status": "failed",
                            "fallback_mode": "mainline_or_reassign",
                            "child_task_id": child_id.to_string(),
                            "role": role,
                            "title": child.title,
                            "summary": summary,
                            "output_ref_count": child.output_refs.len(),
                            "error": error,
                            "instruction": "代理当前不可用。请不要停止任务：优先改派其他可用角色继续；如果没有必要继续派发，则由主线根据已有上下文直接推进并给出最终结果。",
                        })
                        .to_string(),
                        ExecutionResultStatus::Succeeded,
                    );
                }
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "child_task_id": child_id.to_string(),
                        "role": role,
                        "title": child.title,
                        "summary": summary,
                        "output_ref_count": child.output_refs.len(),
                        "error": error,
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            TaskStatus::Killed => {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "killed",
                        "child_task_id": child_id.to_string(),
                        "role": role,
                        "title": child.title,
                        "error": "子任务被终止",
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            TaskStatus::Pending | TaskStatus::Running => {
                thread::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS));
            }
        }
    }
}

fn compact_child_agent_output(output_refs: &[String]) -> String {
    let raw = output_refs
        .iter()
        .rev()
        .find_map(|output| child_agent_final_text(output).or_else(|| non_empty_text(output)))
        .unwrap_or_else(|| "代理未返回可展示输出".to_string());
    truncate_for_agent_spawn_summary(&raw)
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
    let trimmed = value.trim();
    if trimmed.chars().count() <= AGENT_SPAWN_SUMMARY_MAX_CHARS {
        return trimmed.to_string();
    }
    let mut output = trimmed
        .chars()
        .take(AGENT_SPAWN_SUMMARY_MAX_CHARS)
        .collect::<String>();
    output.push('…');
    output
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
    _conversation_registry: &ConversationRegistry,
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
        if matches!(canonical, magi_tool_runtime::BuiltinToolName::AgentSpawn) {
            return execute_coordinator_tool(
                event_bus,
                agent_role_registry,
                task_store,
                session_store,
                execution_registry,
                spawn_graph,
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
                prompt_to_human: "确认后才能继续派发子任务".to_string(),
                label: Some("spawn-gate".to_string()),
                context: None,
            },
            UtcMillis(1),
        );
        store.save(&log).expect("human checkpoint log should save");
        store
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
                    "goal": "不应创建的子任务"
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
