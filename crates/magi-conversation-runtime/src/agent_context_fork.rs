//! 从 canonical 历史冻结父代理的上下文；快照只进入交接包，不进入工具重放账本。
use magi_core::{AgentContextFork, AgentContextForkItem, SessionId, Task, ThreadId};
use magi_session_store::{
    CanonicalTurnItemKind, ModelStepStatus, ORCHESTRATOR_ROLE_ID, SessionStore,
};
use serde_json::Value;
use std::collections::HashSet;

pub(crate) fn freeze_parent_history(
    store: &SessionStore,
    parent: &Task,
    session_id: &SessionId,
    thread_id: &ThreadId,
    turn_id: &str,
    call_id: &str,
) -> Result<AgentContextFork, String> {
    let thread = store
        .thread_registry_snapshot(session_id)
        .into_iter()
        .find(|thread| &thread.thread_id == thread_id)
        .ok_or("fork 来源 Thread 不属于当前会话")?;
    if thread.mission_id != parent.mission_id
        || !thread.handled_task_ids.contains(&parent.task_id)
        || (thread.role_id == ORCHESTRATOR_ROLE_ID) != parent.parent_task_id.is_none()
    {
        return Err("fork 来源 Thread 与父任务不匹配".into());
    }
    // 一次读取冻结 canonical 集合，不能先读取边界再从可变 Thread 投影拼正文。
    let turns = store.canonical_turns_for_session(session_id);
    let source = turns
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .ok_or("fork 来源 Turn 不存在")?;
    let binding = source
        .agent_turn_binding()
        .map_err(|error| error.to_string())?;
    if parent.parent_task_id.is_some()
        && binding.as_ref().is_none_or(|binding| {
            binding.task_id != parent.task_id
                || binding.thread_id != *thread_id
                || binding.mission_id != parent.mission_id
        })
    {
        return Err("fork 来源 Agent Turn 与父任务不匹配".into());
    }
    if parent.parent_task_id.is_none() && binding.is_some() {
        return Err("主线不能 fork 其他 Agent Turn".into());
    }
    let boundary = source
        .items
        .iter()
        .rev()
        .find(|item| {
            item.source_thread_id == *thread_id
                && item.kind == CanonicalTurnItemKind::AssistantText
                && item
                    .worker
                    .as_ref()
                    .and_then(|worker| worker.task_id.as_ref())
                    == Some(&parent.task_id)
                && item
                    .metadata
                    .get("toolCalls")
                    .and_then(Value::as_array)
                    .is_some_and(|calls| {
                        calls
                            .iter()
                            .any(|call| call.get("id").and_then(Value::as_str) == Some(call_id))
                    })
        })
        .ok_or("fork 缺少当前派发调用的 canonical assistant 边界")?;
    let source_step_id = boundary
        .metadata
        .get("modelStepId")
        .and_then(Value::as_str)
        .ok_or("fork 边界缺少模型 Step 来源")?;
    let step_index = source
        .model_steps
        .iter()
        .position(|step| {
            step.step_id == source_step_id
                && step.task_id.as_ref() == Some(&parent.task_id)
                && step.thread_id == *thread_id
                && step.status == ModelStepStatus::ResponseReceived
        })
        .ok_or("fork 边界不是父任务已收到响应的 Step")?;
    let consumed = source.model_steps[..=step_index]
        .iter()
        .flat_map(|step| step.input_request_ids.iter().map(String::as_str))
        .collect::<HashSet<_>>();
    let mut items = Vec::new();
    for turn in turns
        .iter()
        .take_while(|turn| turn.turn_seq <= source.turn_seq)
    {
        if turn.is_session_command() {
            continue;
        }
        for item in &turn.items {
            if item.source_thread_id != *thread_id
                || (turn.turn_id == source.turn_id && item.item_seq >= boundary.item_seq)
            {
                continue;
            }
            let (kind, content) = match item.kind {
                CanonicalTurnItemKind::UserMessage => {
                    // 在父请求冻结后才附加的引导不属于该请求的历史快照。
                    if turn.turn_id == source.turn_id
                        && item
                            .metadata
                            .get("requestId")
                            .and_then(Value::as_str)
                            .is_some_and(|request| !consumed.contains(request))
                    {
                        continue;
                    }
                    ("user_message", item.content.clone())
                }
                CanonicalTurnItemKind::AssistantText => ("assistant_text", item.content.clone()),
                CanonicalTurnItemKind::ToolCall => {
                    let tool = item
                        .tool
                        .as_ref()
                        .ok_or("fork 历史工具缺少 canonical 调用事实")?;
                    if !item.status.is_terminal()
                        && item
                            .background_job()
                            .map_err(|error| error.to_string())?
                            .is_none_or(|job| {
                                job.status == magi_tool_runtime::BackgroundJobStatus::Starting
                                    || tool.result.is_none()
                            })
                    {
                        return Err("fork 历史包含尚未结算的工具调用".into());
                    }
                    ("tool_result", Some(serde_json::json!({
                        "name": tool.name, "arguments": tool.arguments, "result": tool.result,
                        "error": tool.error, "status": item.status,
                    }).to_string()))
                }
                // 不复制隐藏思考、Provider 私有上下文、系统提示或宿主状态。
                _ => continue,
            };
            if let Some(content) = content.filter(|text| !text.trim().is_empty()) {
                items.push(AgentContextForkItem {
                    turn_id: turn.turn_id.clone(),
                    item_id: item.item_id.clone(),
                    kind: kind.into(),
                    content,
                });
            }
        }
    }
    Ok(AgentContextFork {
        session_id: session_id.clone(),
        source_task_id: parent.task_id.clone(),
        source_thread_id: thread_id.clone(),
        source_turn_id: source.turn_id.clone(),
        source_step_id: source_step_id.into(),
        before_item_id: boundary.item_id.clone(),
        source_assignment: match parent.agent_context_package() {
            Some(package) => format!(
                "{}\n{}\n{}",
                parent.goal,
                package.render_for_prompt(),
                package
                    .fork
                    .as_ref()
                    .map(|fork| fork.render_for_prompt())
                    .unwrap_or_default()
            ),
            None => parent.goal.clone(),
        },
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::TaskId;
    use magi_session_store::{CanonicalTurn, SessionStoreState};
    use serde_json::json;

    fn fixture() -> (Task, SessionStoreState) {
        let parent: Task = serde_json::from_value(json!({
            "task_id":"parent", "root_task_id":"parent", "mission_id":"mission",
            "parent_task_id":null, "kind":"local_agent", "title":"parent", "goal":"parent assignment",
            "status":"running", "dependency_ids":[], "required_children":[],
            "policy_snapshot":null, "executor_binding":null, "completion_contract":{},
            "recovery_checkpoint":null, "knowledge_refs":[], "workspace_scope":null,
            "write_scope":null, "input_refs":[], "output_refs":[], "evidence_refs":[],
            "runtime_payload":{"kind":"none"}, "retry_count":0, "created_at":1, "updated_at":1
        })).unwrap();
        let item = |id, seq, kind, content, thread, metadata, tool| {
            json!({
                "sessionId":"session", "turnId":"turn", "turnSeq":1, "itemId":id,
                "itemSeq":seq, "kind":kind, "createdAt":1, "updatedAt":2,
                "status":"completed", "content":content, "sourceThreadId":thread,
                "worker":{"taskId":"parent"}, "metadata":metadata, "tool":tool,
            })
        };
        let step = |id, inputs| {
            json!({
                "stepId":id, "attempts":[{"attemptId":format!("{id}:attempt:1"),
                    "startedAt":1,"settledAt":2,"status":"response_received"}],
                "taskId":"parent", "threadId":"thread", "checkpointGeneration":0,
                "requestFingerprint":"a".repeat(64), "responseFingerprint":"b".repeat(64),
                "provider":"fixture", "messageCount":1, "toolCount":1,
                "inputRequestIds":inputs, "startedAt":1,"settledAt":2,"status":"response_received"
            })
        };
        let turn: CanonicalTurn = serde_json::from_value(json!({
            "sessionId":"session", "turnId":"turn", "turnSeq":1,
            "acceptedAt":1,"status":"running", "modelSteps":[step("old",json!(["consumed"])),step("current",json!([]))],
            "items":[
                item("input",0,"user_message","known input","thread",json!({"requestId":"consumed"}),Value::Null),
                item("steer",1,"user_message","NOT_YET_CONSUMED","thread",json!({"requestId":"pending"}),Value::Null),
                item("old-call",2,"assistant_text","prior evidence","thread",json!({
                    "toolCalls":[{"id":"reused"}],"modelStepId":"old","providerContext":["PRIVATE_PROVIDER"]}),Value::Null),
                item("read",3,"tool_call","","thread",json!({}),json!({"callId":"reused","name":"file_read","arguments":{"path":"a.txt"},"result":{"content":"ORIGINAL_RESULT"}})),
                item("thinking",4,"assistant_thinking","PRIVATE_THINKING","thread",json!({}),Value::Null),
                item("sibling",5,"assistant_text","PRIVATE_SIBLING","sibling-thread",json!({}),Value::Null),
                item("boundary",6,"assistant_text","CURRENT_STEP_OUTPUT","thread",json!({"toolCalls":[{"id":"reused"}],"modelStepId":"current"}),Value::Null),
                item("later",7,"assistant_text","AFTER_BOUNDARY","thread",json!({}),Value::Null)
            ]
        })).unwrap();
        let thread = serde_json::from_value(json!({
            "threadId":"thread","sessionId":"session","missionId":"mission",
            "roleId":ORCHESTRATOR_ROLE_ID,"workerInstanceId":"worker","status":"active",
            "createdAt":1,"lastUsedAt":1,"handledTaskIds":["parent"]
        }))
        .unwrap();
        (
            parent,
            SessionStoreState {
                canonical_turns: vec![turn],
                thread_registry: vec![thread],
                ..Default::default()
            },
        )
    }

    #[test]
    fn fork_freezes_the_current_legal_boundary_without_private_or_unconsumed_history() {
        let (parent, state) = fixture();
        let store = SessionStore::from_state(state.clone());
        let snapshot = freeze_parent_history(
            &store,
            &parent,
            &SessionId::new("session"),
            &ThreadId::new("thread"),
            "turn",
            "reused",
        )
        .unwrap();
        assert_eq!(snapshot.source_step_id, "current");
        assert_eq!(snapshot.before_item_id, "boundary");
        let text = serde_json::to_string(&snapshot).unwrap();
        assert!(text.contains("ORIGINAL_RESULT"));
        for excluded in [
            "PRIVATE_PROVIDER",
            "PRIVATE_THINKING",
            "PRIVATE_SIBLING",
            "NOT_YET_CONSUMED",
            "CURRENT_STEP_OUTPUT",
            "AFTER_BOUNDARY",
        ] {
            assert!(!text.contains(excluded), "{excluded}");
        }
        let mut later = state;
        later.canonical_turns[0].items[3]
            .tool
            .as_mut()
            .unwrap()
            .result = Some(json!("CHANGED_RESULT"));
        let later = SessionStore::from_state(later);
        assert!(
            serde_json::to_string(
                &freeze_parent_history(
                    &later,
                    &parent,
                    &SessionId::new("session"),
                    &ThreadId::new("thread"),
                    "turn",
                    "reused"
                )
                .unwrap()
            )
            .unwrap()
            .contains("CHANGED_RESULT")
        );
        assert_eq!(serde_json::to_string(&snapshot).unwrap(), text);
    }

    #[test]
    fn fork_rejects_cross_scope_forged_steps_and_incomplete_tools() {
        let (parent, state) = fixture();
        for mutation in 0..6 {
            let mut state = state.clone();
            let mut parent = parent.clone();
            match mutation {
                0 => state.thread_registry[0].session_id = SessionId::new("other-session"),
                1 => parent.task_id = TaskId::new("sibling"),
                2 => state.canonical_turns[0].model_steps[1].task_id = Some(TaskId::new("sibling")),
                3 => {
                    state.canonical_turns[0].items[6]
                        .metadata
                        .remove("modelStepId");
                }
                4 => {
                    state.canonical_turns[0].items[3].status =
                        magi_session_store::CanonicalTurnItemStatus::Running
                }
                5 => {}
                _ => unreachable!(),
            }
            let call_id = if mutation == 5 {
                "forged-call"
            } else {
                "reused"
            };
            assert!(
                freeze_parent_history(
                    &SessionStore::from_state(state),
                    &parent,
                    &SessionId::new("session"),
                    &ThreadId::new("thread"),
                    "turn",
                    call_id
                )
                .is_err(),
                "{mutation}"
            );
        }
    }
}
