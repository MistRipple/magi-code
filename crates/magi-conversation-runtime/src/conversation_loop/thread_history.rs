use super::*;

/// 将 canonical 线程投影转换为模型消息，不复制为另一份持久历史。
pub(crate) fn thread_chat_message_to_chat_message(message: &ThreadChatMessage) -> ChatMessage {
    ChatMessage {
        input_request_id: message.input_request_id.clone(),
        context_origin: match message.role.as_str() {
            "system" | "developer" => magi_bridge_client::ChatMessageOrigin::SystemInstruction,
            _ => magi_bridge_client::ChatMessageOrigin::Conversation,
        },
        role: message.role.clone(),
        content: message.content.clone(),
        images: message
            .images
            .iter()
            .map(|image| magi_bridge_client::llm_types::ImageSource {
                kind: image.kind.clone(),
                media_type: image.media_type.clone(),
                data: image.data.clone(),
            })
            .collect(),
        tool_calls: message
            .tool_calls
            .iter()
            .map(|call| ChatToolCall {
                id: call.id.clone(),
                kind: call.kind.clone(),
                function: magi_bridge_client::ChatToolFunction {
                    name: call.function.name.clone(),
                    arguments: call.function.arguments.clone(),
                },
            })
            .collect(),
        tool_call_id: message.tool_call_id.clone(),
        provider_context: message
            .provider_context
            .iter()
            .map(|context| magi_bridge_client::ModelProviderContext {
                provider: context.provider.clone(),
                kind: context.kind.clone(),
                data: context.data.clone(),
            })
            .collect(),
    }
}

pub(super) fn task_is_resuming_existing_thread(
    session_store: &SessionStore,
    session_id: &SessionId,
    task_id: &TaskId,
    thread_id: &ThreadId,
) -> bool {
    if session_store
        .agent_turn_for_task(session_id, task_id)
        .ok()
        .flatten()
        .and_then(|turn| turn.agent_turn_binding().ok().flatten())
        .is_some_and(|binding| {
            binding.thread_id == *thread_id && binding.previous_turn_id.is_some()
        })
    {
        return true;
    }
    session_store
        .runtime_sidecar(session_id)
        .is_some_and(|sidecar| {
            matches!(
                sidecar.status,
                magi_session_store::SessionExecutionSidecarStatus::Resumed
            ) && sidecar.active_execution_chain.is_some_and(|chain| {
                chain
                    .branches
                    .iter()
                    .any(|branch| branch.task_id == *task_id && branch.thread_id == *thread_id)
            })
        })
}

pub(super) fn successful_tool_evidence_from_thread_history(
    history: &[ThreadChatMessage],
) -> Vec<TaskCompletionEvidence> {
    let mut calls = BTreeMap::<String, (&str, &str)>::new();
    for message in history {
        if message.role == "assistant" {
            for call in &message.tool_calls {
                calls.insert(
                    call.id.clone(),
                    (
                        call.function.name.as_str(),
                        call.function.arguments.as_str(),
                    ),
                );
            }
        }
    }
    let mut evidence = Vec::new();
    for message in history {
        if message.role != "tool" {
            continue;
        }
        let Some(call_id) = message.tool_call_id.as_deref() else {
            continue;
        };
        let Some(result) = message.content.as_deref() else {
            continue;
        };
        if infer_tool_call_status(result) != "success" {
            continue;
        }
        let Some((tool_name, arguments)) = calls.get(call_id) else {
            continue;
        };
        evidence.push(TaskCompletionEvidence::SuccessfulToolCall {
            call_id: call_id.to_string(),
            tool_name: canonical_tool_call_name(tool_name),
            arguments: serde_json::from_str(arguments)
                .unwrap_or_else(|_| serde_json::Value::String((*arguments).to_string())),
            result: result.to_string(),
        });
    }
    evidence
}

pub(super) fn tool_call_records_from_thread_history(
    history: &[ThreadChatMessage],
) -> Vec<serde_json::Value> {
    let mut calls = BTreeMap::<String, ChatToolCall>::new();
    let mut results = BTreeMap::<String, String>::new();
    for message in history {
        if message.role == "assistant" {
            for call in &message.tool_calls {
                calls.insert(
                    call.id.clone(),
                    ChatToolCall {
                        id: call.id.clone(),
                        kind: call.kind.clone(),
                        function: magi_bridge_client::ChatToolFunction {
                            name: call.function.name.clone(),
                            arguments: call.function.arguments.clone(),
                        },
                    },
                );
            }
        }
        if message.role == "tool"
            && let (Some(call_id), Some(result)) =
                (message.tool_call_id.as_deref(), message.content.as_deref())
        {
            results.insert(call_id.to_string(), result.to_string());
        }
    }
    calls
        .into_iter()
        .filter_map(|(call_id, call)| {
            results
                .get(&call_id)
                .map(|result| tool_call_record(&call, result))
        })
        .collect()
}

pub(super) fn task_required_evidence_tools(task: &Task) -> Vec<String> {
    let mut required = Vec::new();
    for requirement in &task.completion_contract().evidence_requirements {
        let TaskEvidenceRequirement::SuccessfulToolCall { tool_name, .. } = requirement;
        let canonical_name = canonical_tool_call_name(tool_name);
        if !canonical_name.is_empty() && !required.contains(&canonical_name) {
            required.push(canonical_name);
        }
    }
    required
}

pub(super) fn missing_required_evidence_tool(
    task: &Task,
    evidence: &[TaskCompletionEvidence],
) -> Option<String> {
    task.completion_contract()
        .first_missing_evidence(evidence)
        .map(|requirement| requirement.tool_name().to_string())
}

pub(super) fn required_evidence_recovery_prompt(
    task: &Task,
    evidence: &[TaskCompletionEvidence],
) -> String {
    let missing = task
        .completion_contract()
        .evidence_requirements
        .iter()
        .filter(|requirement| !requirement.is_satisfied_by(evidence))
        .map(|requirement| requirement.tool_name().to_string())
        .collect::<Vec<_>>();
    let completed = evidence
        .iter()
        .filter_map(|item| match item {
            TaskCompletionEvidence::SuccessfulToolCall { tool_name, .. } => Some(tool_name.clone()),
            TaskCompletionEvidence::ExternalExecutor { .. } => None,
        })
        .collect::<Vec<_>>();
    format!(
        "当前任务的最终答复尚缺少结构化交付证据。已完成证据工具：{}；仍缺少：{}。不要承诺稍后补充，也不要重复已有工具；现在调用下一个缺失证据工具，成功后再给完整最终答复。",
        if completed.is_empty() {
            "无".to_string()
        } else {
            completed.join(", ")
        },
        missing.join(", ")
    )
}
