//! Task System v2 — tool batch / coordinator / single 工具执行入口。
//!
//! - `execute_task_tool_call_batch`：按 concurrency 分组并发或串行调度本轮工具。
//! - `execute_task_tool_call`：单工具入口，按 BuiltinToolName 走 coordinator/写工具/policy/
//!   safety gate/tool registry 各分支。
//! - `execute_coordinator_tool`：S7-E 协调器三件套（agent_spawn / send_message / task_stop）。
//! - `task_policy_tool_rejection` / `safety_gate_rejection` 等支撑判定。

use std::{path::PathBuf, sync::Mutex, thread};

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
use magi_session_store::{SessionStore, ThreadChatMessage};
use magi_tool_runtime::{
    BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
};

use crate::builtin_tool_schema::internal_builtin_tool_rejection_payload;
use crate::skill_apply_tool::{SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime};
use crate::{
    ConversationRegistry, MailboxAuthor, MailboxKind, RuntimeSignal,
    task_execution_registry::{
        SpawnedChildExecutionRequest, TaskExecutionPlan, TaskExecutionRegistry,
    },
    task_helpers::{task_can_see_builtin_tool, task_is_long_mission},
};

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

/// S7-E：协调器三件套统一拦截入口。返回 (payload_json, status)，与
/// `execute_task_tool_call` 的常规工具路径形状一致，便于上层把回执拼回 LLM 消息流。
fn execute_coordinator_tool(
    event_bus: &InMemoryEventBus,
    task_store: &TaskStore,
    session_store: &SessionStore,
    execution_registry: &TaskExecutionRegistry,
    conversation_registry: &ConversationRegistry,
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
            let child_id = TaskId::new(format!("task-spawn-{}-{}", task.task_id.as_str(), now.0));
            let child = magi_core::Task {
                task_id: child_id.clone(),
                mission_id: task.mission_id.clone(),
                root_task_id: task.root_task_id.clone(),
                parent_task_id: Some(task.task_id.clone()),
                kind: task_kind,
                title: format!("{role}: {goal}"),
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
                    "lane_id": registered_execution.lane_id,
                    "lane_seq": registered_execution.lane_seq,
                    "thread_id": registered_execution.thread_id.to_string(),
                    "execution_chain_ref": registered_execution.execution_chain_ref,
                }),
            );
            (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "succeeded",
                    "child_task_id": child_id.to_string(),
                    "role": role,
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            )
        }
        magi_tool_runtime::BuiltinToolName::SendMessage => {
            let target = parsed
                .get("target_task_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let payload = parsed
                .get("payload")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            if target.is_empty() || payload.is_null() {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": "send_message 缺少必需字段 target_task_id 或 payload",
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            let target_id = TaskId::new(target.clone());
            let Some(target_task) = task_store.get_task(&target_id) else {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": format!("send_message 目标 task {target} 不存在"),
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            };
            if target_id == task.task_id {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": "send_message 不允许投递给当前 task；请直接在当前 Turn 内处理该信息",
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            if target_task.mission_id != task.mission_id
                || target_task.root_task_id != task.root_task_id
            {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": format!("send_message 目标 task {target} 不属于当前任务树"),
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            let related_by_spawn_graph = match spawn_graph.lock() {
                Ok(graph) => {
                    graph.parent_of(&target_id) == Some(&task.task_id)
                        || graph.parent_of(&task.task_id) == Some(&target_id)
                        || graph
                            .ancestors(&target_id)
                            .iter()
                            .any(|id| id == &task.task_id)
                        || graph
                            .ancestors(&task.task_id)
                            .iter()
                            .any(|id| id == &target_id)
                }
                Err(err) => {
                    tracing::warn!(?err, "send_message SpawnGraph mutex poisoned");
                    false
                }
            };
            if !related_by_spawn_graph {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": format!("send_message 目标 task {target} 不是当前 task 的 SpawnGraph 父子/后代节点"),
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            let Some(TaskExecutionPlan::Dispatch { thread_id, .. }) =
                execution_registry.get(&target_id)
            else {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": format!("send_message 目标 task {target} 尚未注册执行 thread，无法投递运行时输入"),
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            };
            let now = UtcMillis::now();
            let mailbox_kind = parsed
                .get("kind")
                .and_then(|v| v.as_str())
                .and_then(parse_mailbox_kind)
                .unwrap_or(MailboxKind::Message);
            let signal_payload = serde_json::json!({
                "from_task_id": task.task_id.to_string(),
                "target_task_id": target,
                "payload": payload,
            });
            conversation_registry
                .conversation_for_task(session_id, &target_id)
                .lock()
                .expect("target task Conversation mutex poisoned")
                .ingest_runtime_signal(RuntimeSignal {
                    author: MailboxAuthor::Parent(task.task_id.to_string()),
                    kind: mailbox_kind,
                    trigger_turn: true,
                    payload: signal_payload.clone(),
                    enqueued_at: now,
                });
            session_store.append_thread_messages(
                &thread_id,
                vec![ThreadChatMessage {
                    role: "system".to_string(),
                    content: Some(format!(
                        "[mailbox]\nauthor=parent:{}\nkind={}\npayload={}",
                        task.task_id,
                        mailbox_kind_name(mailbox_kind),
                        signal_payload
                    )),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                }],
                now,
            );
            publish_event(
                "task.coordinator.send_message",
                serde_json::json!({
                    "from_task_id": task.task_id.to_string(),
                    "target_task_id": target,
                    "thread_id": thread_id.to_string(),
                    "kind": mailbox_kind_name(mailbox_kind),
                    "payload": signal_payload,
                }),
            );
            (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "succeeded",
                    "target_task_id": target,
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            )
        }
        magi_tool_runtime::BuiltinToolName::TaskStop => {
            let target = parsed
                .get("target_task_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if target.is_empty() {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": "task_stop 缺少必需字段 target_task_id",
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            let target_id = TaskId::new(target.clone());
            if task_store.get_task(&target_id).is_none() {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": format!("task_stop 目标 task {target} 不存在"),
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            let mut cancelled: Vec<String> = Vec::new();
            // 先收集 open 子孙，再统一标记 cancel —— 锁内只做拓扑查询，避免持锁更新 store。
            let descendants = match spawn_graph.lock() {
                Ok(graph) => graph.open_descendants(&target_id),
                Err(err) => {
                    tracing::warn!(?err, "task_stop SpawnGraph mutex poisoned，仅取消目标任务");
                    Vec::new()
                }
            };
            for id in std::iter::once(target_id.clone()).chain(descendants.into_iter()) {
                if task_store.update_status(&id, TaskStatus::Killed).is_ok() {
                    cancelled.push(id.to_string());
                    if let Ok(mut graph) = spawn_graph.lock() {
                        let _ = graph.mark_closed(&id, std::time::SystemTime::now());
                    }
                }
            }
            publish_event(
                "task.coordinator.task_stop",
                serde_json::json!({
                    "from_task_id": task.task_id.to_string(),
                    "target_task_id": target,
                    "cancelled_task_ids": cancelled,
                    "reason": parsed.get("reason").and_then(|v| v.as_str()).unwrap_or(""),
                }),
            );
            (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "succeeded",
                    "cancelled_task_ids": cancelled,
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            )
        }
        _ => unreachable!("execute_coordinator_tool 只接收 3 个协调器变体"),
    }
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
    // S7-E：协调器三件套（agent_spawn / send_message / task_stop）由 orchestration 层拦截，
    // 不进 BuiltinTool::execute —— 它们需要 task_store / spawn_graph / event_bus 等上下文。
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
                | magi_tool_runtime::BuiltinToolName::SendMessage
                | magi_tool_runtime::BuiltinToolName::TaskStop
        ) {
            return execute_coordinator_tool(
                event_bus,
                task_store,
                session_store,
                execution_registry,
                conversation_registry,
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

fn parse_mailbox_kind(raw: &str) -> Option<MailboxKind> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" => None,
        "message" | "user" => Some(MailboxKind::Message),
        "decision" => Some(MailboxKind::Decision),
        "interrupt" => Some(MailboxKind::Interrupt),
        "agent_result" | "agent-result" | "result" => Some(MailboxKind::AgentResult),
        "followup" | "follow_up" | "follow-up" => Some(MailboxKind::Followup),
        _ => None,
    }
}

fn mailbox_kind_name(kind: MailboxKind) -> &'static str {
    match kind {
        MailboxKind::Message => "message",
        MailboxKind::Decision => "decision",
        MailboxKind::Interrupt => "interrupt",
        MailboxKind::AgentResult => "agent_result",
        MailboxKind::Followup => "followup",
    }
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
    use crate::MailboxItem;
    use magi_bridge_client::ChatToolFunction;
    use magi_core::{
        ExecutionOwnership, MissionId, Task, TaskExecutionTarget, TaskPolicy, TaskRuntimePayload,
        ThreadId, WorkerId, WorkspaceRootPath,
    };
    use magi_orchestrator::ExecutionWritebackPlans;
    use magi_session_store::{
        ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
        ActiveExecutionTurn,
    };

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

    fn dispatch_plan(
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        task: &Task,
        thread_id: &ThreadId,
    ) -> TaskExecutionPlan {
        let worker_id = WorkerId::new(format!("worker-{}", task.task_id));
        TaskExecutionPlan::Dispatch {
            target: TaskExecutionTarget {
                mission_id: task.mission_id.clone(),
                root_task_id: task.root_task_id.clone(),
                task_id: task.task_id.clone(),
                requested_worker_id: Some(worker_id.clone()),
                recovery_id: None,
                execution_chain_ref: None,
            },
            worker_id: worker_id.clone(),
            lane_id: None,
            lane_seq: None,
            thread_id: thread_id.clone(),
            is_primary: false,
            session_id: session_id.clone(),
            workspace_id: workspace_id.clone(),
            ownership: ExecutionOwnership {
                session_id: Some(session_id.clone()),
                workspace_id: workspace_id.clone(),
                mission_id: Some(task.mission_id.clone()),
                task_id: Some(task.task_id.clone()),
                worker_id: Some(worker_id),
                execution_chain_ref: None,
            },
            writebacks: ExecutionWritebackPlans::default(),
            use_tools: true,
            skill_name: None,
        }
    }

    #[test]
    fn agent_spawn_registers_child_execution_plan_and_lane() {
        let event_bus = InMemoryEventBus::new(16);
        let task_store = TaskStore::new();
        let session_store = SessionStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let conversation_registry = ConversationRegistry::new();
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let todo_ledger = magi_todo_ledger::TodoLedger::new();
        let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();

        let session_id = SessionId::new("session-agent-spawn");
        let workspace_id = Some(WorkspaceId::new("workspace-agent-spawn"));
        let parent = coordinator_task(test_task("task-parent", "task-parent", None));
        task_store.insert_task(parent.clone());
        let now = UtcMillis::now();
        let _ =
            session_store.ensure_session_mission(&session_id, now, || parent.mission_id.clone());
        let parent_worker_id = WorkerId::new("worker-parent");
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: parent.mission_id.clone(),
                    root_task_id: parent.root_task_id.clone(),
                    execution_chain_ref: "chain-agent-spawn".to_string(),
                    workspace_id: workspace_id.clone(),
                    active_branch_task_ids: vec![parent.task_id.clone()],
                    active_worker_bindings: vec![parent_worker_id.clone()],
                    branches: vec![ActiveExecutionBranch {
                        task_id: parent.task_id.clone(),
                        worker_id: parent_worker_id,
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
                        skill_name: None,
                        is_primary: true,
                    }],
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-agent-spawn".to_string(),
                        trimmed_text: Some("spawn child".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-agent-spawn".to_string(),
                        turn_seq: now.0,
                        accepted_at: now,
                        completed_at: None,
                        status: "running".to_string(),
                        user_message: Some("spawn child".to_string()),
                        items: Vec::new(),
                        worker_lanes: Vec::new(),
                    }),
                },
            )
            .expect("active chain should be accepted");

        let tool_call = ChatToolCall {
            id: "call-agent-spawn".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::AgentSpawn.as_str().to_string(),
                arguments: serde_json::json!({
                    "role": "integration-dev",
                    "goal": "执行子任务",
                    "context": "必须调用 shell_exec 输出 CHILD_OK",
                    "task_kind": "action"
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
            Some(&WorkerId::new("worker-parent")),
            &[tool_call],
        );

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, ExecutionResultStatus::Succeeded);
        let payload: serde_json::Value =
            serde_json::from_str(&result[0].0).expect("agent_spawn result should be json");
        let child_task_id = TaskId::new(
            payload["child_task_id"]
                .as_str()
                .expect("child_task_id should be present"),
        );
        let child = task_store
            .get_task(&child_task_id)
            .expect("spawned child task should exist");
        assert!(child.goal.contains("CHILD_OK"));

        let plan = execution_registry
            .get(&child_task_id)
            .expect("spawned child should have v2 execution plan");
        match plan {
            TaskExecutionPlan::Dispatch {
                lane_id,
                lane_seq,
                session_id: plan_session_id,
                use_tools,
                ..
            } => {
                assert_eq!(plan_session_id, session_id);
                assert!(lane_id.is_some());
                assert_eq!(lane_seq, Some(1));
                assert!(use_tools);
            }
        }
        let sidecar = session_store
            .runtime_sidecar(&session_id)
            .expect("runtime sidecar should exist");
        let chain = sidecar
            .active_execution_chain
            .expect("active chain should exist");
        assert!(
            chain
                .branches
                .iter()
                .any(|branch| branch.task_id == child_task_id)
        );
        assert_eq!(
            chain
                .current_turn
                .as_ref()
                .map(|turn| turn.worker_lanes.len()),
            Some(1)
        );
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
                    "role": "integration-dev",
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
                    "role": "integration-dev",
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
    fn send_message_routes_to_target_task_mailbox_and_thread_history() {
        let event_bus = InMemoryEventBus::new(16);
        let task_store = TaskStore::new();
        let session_store = SessionStore::new();
        let execution_registry = TaskExecutionRegistry::default();
        let conversation_registry = ConversationRegistry::new();
        let spawn_graph = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let todo_ledger = magi_todo_ledger::TodoLedger::new();
        let agent_role_registry = magi_agent_role::AgentRoleRegistry::load_default();

        let session_id = SessionId::new("session-mailbox");
        let workspace_id = Some(WorkspaceId::new("workspace-mailbox"));
        let parent = coordinator_task(test_task("task-parent", "task-parent", None));
        let child = test_task(
            "task-child",
            "task-parent",
            Some(TaskId::new("task-parent")),
        );
        task_store.insert_task(parent.clone());
        task_store.insert_task(child.clone());
        spawn_graph
            .lock()
            .expect("spawn graph lock should hold")
            .add_edge(
                parent.task_id.clone(),
                child.task_id.clone(),
                child.kind,
                std::time::SystemTime::UNIX_EPOCH,
            )
            .expect("parent/child edge should register");
        let (_, target_thread_id) =
            session_store
                .ensure_session_mission(&session_id, UtcMillis::now(), || child.mission_id.clone());
        execution_registry.insert(
            child.task_id.clone(),
            dispatch_plan(&session_id, &workspace_id, &child, &target_thread_id),
        );

        let tool_call = ChatToolCall {
            id: "call-send-message".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::SendMessage.as_str().to_string(),
                arguments: serde_json::json!({
                    "target_task_id": child.task_id.to_string(),
                    "kind": "agent-result",
                    "payload": {"summary": "child result is ready"}
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
        assert_eq!(result[0].1, ExecutionResultStatus::Succeeded);

        let target_conversation =
            conversation_registry.conversation_for_task(&session_id, &child.task_id);
        let pending = target_conversation
            .lock()
            .expect("target conversation lock poisoned")
            .drain_mailbox_items();
        assert_eq!(pending.len(), 1);
        match &pending[0] {
            MailboxItem::Runtime(signal) => {
                assert_eq!(signal.kind, MailboxKind::AgentResult);
                assert_eq!(
                    signal.author,
                    MailboxAuthor::Parent(parent.task_id.to_string())
                );
                assert!(signal.trigger_turn);
                assert_eq!(
                    signal.payload["payload"]["summary"].as_str(),
                    Some("child result is ready")
                );
            }
            MailboxItem::User(_) => panic!("send_message must enqueue runtime mailbox item"),
        }

        let history = session_store.thread_message_history(&target_thread_id);
        assert_eq!(history.len(), 1);
        let content = history[0].content.as_deref().unwrap_or_default();
        assert!(content.contains("[mailbox]"));
        assert!(content.contains("kind=agent_result"));
        assert!(content.contains("child result is ready"));
    }
}
