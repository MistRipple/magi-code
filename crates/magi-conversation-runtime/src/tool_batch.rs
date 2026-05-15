//! Task System v2 — tool batch / coordinator / single 工具执行入口。
//!
//! 从 v1 `magi-api::task_llm_loop` 迁入：
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
use magi_tool_runtime::{
    BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
};

use crate::builtin_tool_schema::internal_builtin_tool_rejection_payload;
use crate::skill_apply_tool::{SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime};

#[allow(clippy::too_many_arguments)]
pub fn execute_task_tool_call_batch(
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&magi_skill_runtime::SkillRuntime>,
    task_store: &TaskStore,
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
                        skill_runtime,
                        task_store,
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
                                        skill_runtime,
                                        task_store,
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
            let task_kind = parsed
                .get("task_kind")
                .and_then(|v| v.as_str())
                .and_then(|s| match s.to_ascii_lowercase().as_str() {
                    "action" => Some(TaskKind::Action),
                    "validation" => Some(TaskKind::Validation),
                    "repair" => Some(TaskKind::Repair),
                    "decision" => Some(TaskKind::Decision),
                    "work_package" | "workpackage" => Some(TaskKind::WorkPackage),
                    "phase" => Some(TaskKind::Phase),
                    "objective" => Some(TaskKind::Objective),
                    _ => None,
                })
                .unwrap_or(TaskKind::Action);
            let now = UtcMillis::now();
            let child_id = TaskId::new(format!(
                "task-spawn-{}-{}",
                task.task_id.as_str(),
                now.0
            ));
            let child = magi_core::Task {
                task_id: child_id.clone(),
                mission_id: task.mission_id.clone(),
                root_task_id: task.root_task_id.clone(),
                parent_task_id: Some(task.task_id.clone()),
                kind: task_kind,
                title: format!("{role}: {goal}"),
                goal: goal.clone(),
                status: TaskStatus::Ready,
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
                context_refs: Vec::new(),
                knowledge_refs: Vec::new(),
                workspace_scope: task.workspace_scope.clone(),
                write_scope: task.write_scope.clone(),
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                repair_count: 0,
                decision_payload: None,
                variant: magi_core::TaskVariant::default(),
                created_at: now,
                updated_at: now,
            };
            task_store.insert_task(child);
            // SpawnGraph 边：失败仅 warn（与 dispatch_execution::register_spawn_edge 一致策略）。
            if let Ok(mut graph) = spawn_graph.lock() {
                if let Err(err) = graph.add_edge(
                    task.task_id.clone(),
                    child_id.clone(),
                    task_kind,
                    std::time::SystemTime::now(),
                ) {
                    tracing::warn!(
                        parent = %task.task_id.as_str(),
                        child = %child_id.as_str(),
                        error = %err,
                        "agent_spawn SpawnGraph add_edge 失败，子任务已插入但拓扑边缺失",
                    );
                }
            }
            publish_event(
                "task.coordinator.agent_spawn",
                serde_json::json!({
                    "parent_task_id": task.task_id.to_string(),
                    "child_task_id": child_id.to_string(),
                    "role": role,
                    "goal": goal,
                    "task_kind": format!("{:?}", task_kind),
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
            let payload = parsed.get("payload").cloned().unwrap_or(serde_json::Value::Null);
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
            if task_store.get_task(&target_id).is_none() {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": format!("send_message 目标 task {target} 不存在"),
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            // S7 暂以事件总线作为跨 task 消息通道；后续 slice 接入 Mailbox 后再切换路由。
            publish_event(
                "task.coordinator.send_message",
                serde_json::json!({
                    "from_task_id": task.task_id.to_string(),
                    "target_task_id": target,
                    "kind": parsed.get("kind").and_then(|v| v.as_str()).unwrap_or("user"),
                    "payload": payload,
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
                if task_store
                    .update_status(&id, TaskStatus::Cancelled)
                    .is_ok()
                {
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
    skill_runtime: Option<&magi_skill_runtime::SkillRuntime>,
    task_store: &TaskStore,
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
        if matches!(
            canonical,
            magi_tool_runtime::BuiltinToolName::AgentSpawn
                | magi_tool_runtime::BuiltinToolName::SendMessage
                | magi_tool_runtime::BuiltinToolName::TaskStop
        ) {
            return execute_coordinator_tool(
                event_bus,
                task_store,
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
        if matches!(canonical, magi_tool_runtime::BuiltinToolName::MissionCharterWrite) {
            return magi_mission_charter::execute_mission_charter_write_tool(
                event_bus,
                mission_charter,
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
        if matches!(canonical, magi_tool_runtime::BuiltinToolName::ValidationRecord) {
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
        // HumanCheckpointStore，并触发 awaiting_human 状态。
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
        if let Some(rejection) =
            safety_gate_rejection(gate, &tool_call.function.name, &tool_call.function.arguments)
        {
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
    if policy_snapshot.command_mode.eq_ignore_ascii_case("no_tools") {
        return Some(task_policy_rejection_payload(
            &canonical_tool_name,
            format!("当前任务阶段不允许调用工具: {canonical_tool_name}"),
        ));
    }
    // PermissionEngine 比对工具名是按字面比对，因此把 policy 中的别名先 canonical 化。
    let mut canonical_policy = magi_permissions::PermissionPolicy::from_core_policy(policy_snapshot);
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
/// 当前 task_llm_loop 没有交互审批通道（governance 走自己的回路），所以
/// RequireApproval 在本层暂时与 Block 同语义——拒绝执行并把原因回灌给模型，
/// 由模型决定是否换更精确的命令或转向人审通道。
fn safety_gate_rejection(
    gate: &magi_safety_gate::SafetyGate,
    tool_name: &str,
    arguments: &str,
) -> Option<String> {
    let canonical_tool_name = canonical_builtin_tool_name(tool_name)
        .unwrap_or_else(|| tool_name.trim().to_string());
    match gate.evaluate(&canonical_tool_name, arguments) {
        magi_safety_gate::SafetyDecision::Allow => None,
        magi_safety_gate::SafetyDecision::Block {
            category, pattern, reason,
        }
        | magi_safety_gate::SafetyDecision::RequireApproval {
            category, pattern, reason,
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
