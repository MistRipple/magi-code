use super::*;

/// S7-E：协调器工具（agent_spawn）的统一拦截入口。返回 (payload_json, status)，与
/// `execute_task_tool_call` 的常规工具路径形状一致，便于上层把回执拼回 LLM 消息流。
#[derive(Clone, Copy)]
pub(super) struct CoordinatorToolContext<'a> {
    pub(super) event_bus: &'a InMemoryEventBus,
    pub(super) agent_role_registry: &'a magi_agent_role::AgentRoleRegistry,
    pub(super) task_store: &'a TaskStore,
    pub(super) session_store: &'a SessionStore,
    pub(super) execution_registry: &'a TaskExecutionRegistry,
    pub(super) conversation_registry: &'a ConversationRegistry,
    pub(super) plan_store: &'a magi_plan::PlanStore,
    pub(super) task: &'a magi_core::Task,
    pub(super) session_id: &'a SessionId,
    pub(super) workspace_id: &'a Option<WorkspaceId>,
    pub(super) workspace_root_path: Option<&'a PathBuf>,
    pub(super) execution_allowed: &'a (dyn Fn() -> bool + Sync),
}

pub(super) fn execute_coordinator_tool(
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

/// 交互必须绑定发起调用所在的 canonical Turn。独立 Agent Turn 不会改写
/// Session 当前 Turn 指针，因此不能只从 runtime sidecar 读取目标。
pub(super) fn execute_execution_steps(
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
