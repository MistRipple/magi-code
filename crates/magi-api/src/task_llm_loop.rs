use crate::{
    prompt_utils::{normalize_model_stream_preview_content, normalize_model_visible_content},
    session_turn_writeback::{
        append_session_turn_item_with_task_store, build_completed_turn_timeline_snapshot,
        publish_current_session_turn_item_event, publish_session_turn_item_event,
        session_turn_item, upsert_session_turn_item_with_task_store,
    },
    settings_store::SettingsStore,
    skill_apply_tool::{SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime},
    tool_result_utils::{
        infer_tool_call_status, summarize_tool_result, tool_execution_status_label,
        turn_item_status_for_tool_result,
    },
    usage_recording::{ModelUsageBinding, publish_model_usage_record},
};
use magi_bridge_client::{
    ChatMessage, ChatToolCall, ChatToolDefinition, ModelBridgeClient, ModelInvocationRequest,
    ModelStreamingDelta, SHADOW_MODEL_PROVIDER,
    tool_concurrency::{ToolBatchKind, ToolConcurrencyInput, partition_tool_calls_with_inputs},
};
use magi_core::{
    ApprovalRequirement, EventId, ExecutionResultStatus, LeaseId, RiskLevel, SessionId, TaskId,
    TaskStatus, ToolCallId, UtcMillis, WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::ToolKind;
use magi_orchestrator::{
    ExecutionContextSummary, task_runner::TaskOutcome, task_store::TaskStore,
    task_worker_catalog::resolve_task_role,
};
use magi_session_store::{ActiveExecutionTurnItem, SessionStore, TimelineEntryKind};
use magi_tool_runtime::{
    ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
};
use magi_usage_authority::UsageCallStatus;
use std::{sync::Arc, thread};

const MAX_TOOL_CALL_ROUNDS: usize = 8;

pub(crate) struct TaskLlmLoopRequest<'a> {
    pub client: &'a dyn ModelBridgeClient,
    pub event_bus: &'a InMemoryEventBus,
    pub session_store: &'a SessionStore,
    pub settings_store: Option<&'a Arc<SettingsStore>>,
    pub tool_registry: Option<&'a ToolRegistry>,
    pub skill_runtime: Option<&'a magi_skill_runtime::SkillRuntime>,
    pub task_store: &'a TaskStore,
    pub task: &'a magi_core::Task,
    pub task_id: &'a TaskId,
    pub lease_id: &'a LeaseId,
    pub session_id: &'a SessionId,
    pub workspace_id: &'a Option<WorkspaceId>,
    pub prompt: String,
    pub tools: Option<Vec<ChatToolDefinition>>,
    pub usage_binding: &'a ModelUsageBinding,
    pub streaming_entry_id: Option<&'a str>,
    pub worker_lane_id: Option<&'a str>,
    pub worker_lane_seq: Option<usize>,
    pub worker_id: Option<&'a magi_core::WorkerId>,
    pub context_summary: Option<ExecutionContextSummary>,
    pub system_prompt: Option<String>,
}

#[derive(Clone, Debug)]
struct TaskTurnVisibility {
    thread_visible: bool,
    worker_visible: bool,
    role_id: Option<String>,
    worker_id: Option<magi_core::WorkerId>,
    lane_id: Option<String>,
    lane_seq: Option<usize>,
}

fn task_turn_visibility(
    task: &magi_core::Task,
    thread_visible: bool,
    worker_lane_id: Option<&str>,
    worker_lane_seq: Option<usize>,
    worker_id: Option<&magi_core::WorkerId>,
) -> TaskTurnVisibility {
    let role_id = resolve_task_role(task)
        .map(str::trim)
        .filter(|role| !role.is_empty())
        .map(ToOwned::to_owned);
    let lane_id = worker_lane_id
        .map(str::trim)
        .filter(|lane| !lane.is_empty())
        .map(ToOwned::to_owned);
    TaskTurnVisibility {
        thread_visible,
        worker_visible: lane_id.is_some(),
        role_id,
        worker_id: worker_id.cloned(),
        lane_id,
        lane_seq: worker_lane_seq,
    }
}

fn apply_task_turn_visibility(
    item: &mut ActiveExecutionTurnItem,
    task: &magi_core::Task,
    visibility: &TaskTurnVisibility,
) {
    item.task_id = Some(task.task_id.clone());
    item.thread_visible = visibility.thread_visible;
    item.worker_visible = visibility.worker_visible;
    if visibility.worker_visible {
        item.lane_id = visibility.lane_id.clone();
        item.lane_seq = visibility.lane_seq;
        item.worker_id = visibility.worker_id.clone();
    }
    if let Some(role_id) = visibility.role_id.as_ref() {
        item.role_id = Some(role_id.clone());
        item.source = role_id.clone();
    }
}

pub(crate) fn run_task_llm_loop(
    request: TaskLlmLoopRequest<'_>,
) -> (TaskOutcome, Option<ExecutionContextSummary>) {
    let TaskLlmLoopRequest {
        client,
        event_bus,
        session_store,
        settings_store,
        tool_registry,
        skill_runtime,
        task_store,
        task,
        task_id,
        lease_id,
        session_id,
        workspace_id,
        prompt,
        tools,
        usage_binding,
        streaming_entry_id,
        worker_lane_id,
        worker_lane_seq,
        worker_id,
        context_summary,
        system_prompt,
    } = request;

    let mut messages = Vec::new();
    if let Some(system) = system_prompt {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(system),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: Some(prompt.clone()),
        tool_calls: Vec::new(),
        tool_call_id: None,
    });
    let task_context = task_event_context(task, session_id, workspace_id);
    publish_task_llm_started(
        event_bus,
        task,
        session_id,
        workspace_id,
        &prompt,
        &task_context,
    );

    let mut final_content = String::new();
    let mut tool_call_records: Vec<serde_json::Value> = Vec::new();
    let mut last_stream_item_id: Option<String> = None;
    let mut had_tool_calls = false;
    let turn_visibility = task_turn_visibility(
        task,
        streaming_entry_id.is_some()
            || task_is_thread_visible_turn_owner(session_store, session_id, task_id),
        worker_lane_id,
        worker_lane_seq,
        worker_id,
    );

    for round in 0..MAX_TOOL_CALL_ROUNDS {
        let should_record_turn_artifacts =
            turn_visibility.thread_visible || turn_visibility.worker_visible;
        let thinking_item_id = format!("turn-item-assistant-thinking-{task_id}-{round}");
        let stream_item_id = task_stream_item_id(task_id, round, streaming_entry_id);
        last_stream_item_id = Some(stream_item_id.clone());
        let streamed_thinking = std::cell::RefCell::new(String::new());
        let last_thinking_len = std::cell::Cell::new(0usize);
        let invocation_request = ModelInvocationRequest {
            provider: SHADOW_MODEL_PROVIDER.to_string(),
            prompt: prompt.clone(),
            messages: Some(messages.clone()),
            tools: tools.clone(),
            tool_choice: None,
        };

        let response = if streaming_entry_id.is_some() {
            let on_delta = |delta: &ModelStreamingDelta| {
                if should_record_turn_artifacts {
                    publish_task_thinking_delta(
                        event_bus,
                        session_store,
                        task_store,
                        task,
                        session_id,
                        workspace_id,
                        &thinking_item_id,
                        &last_thinking_len,
                        &streamed_thinking,
                        &turn_visibility,
                        &delta.thinking,
                    );
                }
                publish_stream_delta(
                    event_bus,
                    session_store,
                    task_store,
                    task,
                    session_id,
                    workspace_id,
                    &stream_item_id,
                    (round == 0).then_some(stream_item_id.as_str()),
                    &turn_visibility,
                    &delta.content,
                );
            };

            match client.invoke_streaming(invocation_request, &on_delta) {
                Ok(response) => response,
                Err(error) => {
                    tracing::error!(task_id = %task.task_id, round = round, ?error, "LLM streaming invocation failed");
                    if should_record_turn_artifacts
                        && task_lease_is_current(task_store, task_id, lease_id)
                    {
                        append_task_error_turn_item(
                            event_bus,
                            session_store,
                            task_store,
                            task,
                            session_id,
                            workspace_id,
                            &turn_visibility,
                            &format!("LLM invocation failed (round {round}): {error:?}"),
                            streaming_entry_id.or(last_stream_item_id.as_deref()),
                        );
                    }
                    return (
                        TaskOutcome::Failed {
                            error: format!("LLM invocation failed (round {round}): {error:?}"),
                        },
                        context_summary,
                    );
                }
            }
        } else {
            match client.invoke(invocation_request) {
                Ok(response) => response,
                Err(error) => {
                    tracing::error!(task_id = %task.task_id, round = round, ?error, "LLM invocation failed");
                    if should_record_turn_artifacts
                        && task_lease_is_current(task_store, task_id, lease_id)
                    {
                        append_task_error_turn_item(
                            event_bus,
                            session_store,
                            task_store,
                            task,
                            session_id,
                            workspace_id,
                            &turn_visibility,
                            &format!("LLM invocation failed (round {round}): {error:?}"),
                            streaming_entry_id.or(last_stream_item_id.as_deref()),
                        );
                    }
                    return (
                        TaskOutcome::Failed {
                            error: format!("LLM invocation failed (round {round}): {error:?}"),
                        },
                        context_summary,
                    );
                }
            }
        };

        let parsed = response.parse_chat_payload();
        if should_record_turn_artifacts {
            let final_thinking = parsed
                .thinking
                .as_deref()
                .map(str::trim)
                .filter(|thinking| !thinking.is_empty())
                .map(ToOwned::to_owned)
                .or_else(|| {
                    let thinking = streamed_thinking.borrow();
                    let trimmed = thinking.trim();
                    (!trimmed.is_empty()).then(|| trimmed.to_string())
                });
            if let Some(thinking) = final_thinking {
                upsert_task_thinking_turn_item(
                    event_bus,
                    session_store,
                    task_store,
                    task,
                    session_id,
                    workspace_id,
                    &thinking_item_id,
                    &turn_visibility,
                    "completed",
                    &thinking,
                );
            }
        }
        publish_model_usage_record(
            event_bus,
            session_store,
            settings_store,
            session_id,
            workspace_id,
            usage_binding,
            format!("task-{}-{}-{round}", task_id, lease_id),
            parsed.usage.as_ref(),
            UsageCallStatus::Success,
            Some(lease_id.to_string()),
            None,
        );

        if let Some(ref content) = parsed.content {
            final_content = content.clone();
        }
        if !parsed.tool_calls.is_empty() {
            had_tool_calls = true;
        }

        if parsed.tool_calls.is_empty() {
            break;
        }

        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: parsed.content.clone(),
            tool_calls: parsed.tool_calls.clone(),
            tool_call_id: None,
        });

        for tool_call in &parsed.tool_calls {
            if should_record_turn_artifacts {
                append_task_tool_call_started_turn_item(
                    event_bus,
                    session_store,
                    task_store,
                    task,
                    session_id,
                    workspace_id,
                    &turn_visibility,
                    tool_call,
                );
            }
        }

        let tool_results = execute_task_tool_call_batch(
            event_bus,
            tool_registry,
            skill_runtime,
            task,
            session_id,
            workspace_id,
            turn_visibility.worker_id.as_ref(),
            &parsed.tool_calls,
        );

        for (tool_call, (result, tool_status)) in parsed.tool_calls.iter().zip(tool_results) {
            if should_record_turn_artifacts {
                upsert_task_tool_call_result_turn_item(
                    event_bus,
                    session_store,
                    task_store,
                    task,
                    session_id,
                    workspace_id,
                    &turn_visibility,
                    tool_call,
                    &result,
                    tool_status,
                );
            }
            tool_call_records.push(tool_call_record(tool_call, &result));
            messages.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(result),
                tool_calls: Vec::new(),
                tool_call_id: Some(tool_call.id.clone()),
            });
        }
    }

    if final_content.trim().is_empty() {
        let failure_reason = if had_tool_calls {
            "模型在工具调用后未返回最终回复"
        } else {
            "模型未返回可显示回复"
        };
        if turn_visibility.thread_visible || turn_visibility.worker_visible {
            append_task_error_turn_item(
                event_bus,
                session_store,
                task_store,
                task,
                session_id,
                workspace_id,
                &turn_visibility,
                failure_reason,
                streaming_entry_id.or(last_stream_item_id.as_deref()),
            );
        }
        return (
            TaskOutcome::Failed {
                error: failure_reason.to_string(),
            },
            context_summary,
        );
    }
    final_content = normalize_model_visible_content(final_content);
    if final_content.trim().is_empty() {
        let failure_reason = "模型未返回可显示回复";
        if turn_visibility.thread_visible || turn_visibility.worker_visible {
            append_task_error_turn_item(
                event_bus,
                session_store,
                task_store,
                task,
                session_id,
                workspace_id,
                &turn_visibility,
                failure_reason,
                streaming_entry_id.or(last_stream_item_id.as_deref()),
            );
        }
        return (
            TaskOutcome::Failed {
                error: failure_reason.to_string(),
            },
            context_summary,
        );
    }
    if !task_lease_is_current(task_store, task_id, lease_id) {
        return (
            TaskOutcome::Failed {
                error: "任务执行已被中断，丢弃晚到模型结果".to_string(),
            },
            context_summary,
        );
    }
    if turn_visibility.thread_visible || turn_visibility.worker_visible {
        append_task_final_turn_item(
            event_bus,
            session_store,
            task_store,
            task,
            session_id,
            workspace_id,
            &final_content,
            last_stream_item_id.as_deref().or(streaming_entry_id),
            streaming_entry_id,
            &turn_visibility,
        );
    }

    (
        TaskOutcome::Completed {
            output_refs: vec![build_output_content(tool_call_records, final_content)],
        },
        context_summary,
    )
}

fn task_event_context(
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
) -> EventContext {
    EventContext {
        workspace_id: workspace_id.clone(),
        session_id: Some(session_id.clone()),
        mission_id: Some(task.mission_id.clone()),
        task_id: Some(task.task_id.clone()),
        ..EventContext::default()
    }
}

fn publish_task_llm_started(
    event_bus: &InMemoryEventBus,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    prompt: &str,
    task_context: &EventContext,
) {
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-task-llm-started-{}", UtcMillis::now().0)),
            "task.llm.started",
            serde_json::json!({
                "task_id": task.task_id.to_string(),
                "mission_id": task.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "prompt_length": prompt.len(),
            }),
        )
        .with_context(task_context.clone()),
    );
}

fn publish_stream_delta(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    item_id: &str,
    timeline_entry_id: Option<&str>,
    turn_visibility: &TaskTurnVisibility,
    accumulated_text: &str,
) {
    let visible_text = normalize_model_stream_preview_content(accumulated_text);
    if visible_text.trim().is_empty() {
        return;
    }
    if let Some(timeline_entry_id) = timeline_entry_id.filter(|_| turn_visibility.thread_visible) {
        session_store.upsert_timeline_entry(
            session_id.clone(),
            timeline_entry_id,
            TimelineEntryKind::AssistantMessage,
            &visible_text,
        );
    }
    let mut item = session_turn_item(
        "assistant_stream",
        "running",
        Some("生成回复".to_string()),
        Some(visible_text),
        Some(item_id.to_string()),
    );
    apply_task_turn_visibility(&mut item, task, turn_visibility);
    if let Some(published) =
        upsert_session_turn_item_with_task_store(session_store, session_id, item, Some(task_store))
    {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
}

fn publish_task_thinking_delta(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    item_id: &str,
    last_sent_len: &std::cell::Cell<usize>,
    streamed_thinking: &std::cell::RefCell<String>,
    turn_visibility: &TaskTurnVisibility,
    accumulated_thinking: &str,
) {
    if accumulated_thinking.len() <= last_sent_len.get() {
        return;
    }
    last_sent_len.set(accumulated_thinking.len());
    {
        let mut thinking = streamed_thinking.borrow_mut();
        thinking.clear();
        thinking.push_str(accumulated_thinking);
    }
    upsert_task_thinking_turn_item(
        event_bus,
        session_store,
        task_store,
        task,
        session_id,
        workspace_id,
        item_id,
        turn_visibility,
        "running",
        accumulated_thinking,
    );
}

fn upsert_task_thinking_turn_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    item_id: &str,
    turn_visibility: &TaskTurnVisibility,
    status: &str,
    thinking: &str,
) {
    let trimmed = thinking.trim();
    if trimmed.is_empty() {
        return;
    }
    let mut item = session_turn_item(
        "assistant_thinking",
        status,
        Some("模型思考".to_string()),
        Some(trimmed.to_string()),
        Some(item_id.to_string()),
    );
    apply_task_turn_visibility(&mut item, task, turn_visibility);
    if let Some(published) =
        upsert_session_turn_item_with_task_store(session_store, session_id, item, Some(task_store))
    {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
}

fn append_task_tool_call_started_turn_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    turn_visibility: &TaskTurnVisibility,
    tool_call: &ChatToolCall,
) {
    let mut item = session_turn_item(
        "tool_call_started",
        "running",
        Some(tool_call.function.name.clone()),
        Some(format!("正在调用工具：{}", tool_call.function.name)),
        Some(format!("turn-item-tool-{}", tool_call.id)),
    );
    apply_task_turn_visibility(&mut item, task, turn_visibility);
    item.tool_call_id = Some(tool_call.id.clone());
    item.tool_name = Some(tool_call.function.name.clone());
    item.tool_status = Some("running".to_string());
    item.tool_arguments = Some(tool_call.function.arguments.clone());
    if let Some(published) =
        upsert_session_turn_item_with_task_store(session_store, session_id, item, Some(task_store))
    {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
}

fn upsert_task_tool_call_result_turn_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    turn_visibility: &TaskTurnVisibility,
    tool_call: &ChatToolCall,
    tool_result: &str,
    tool_status: ExecutionResultStatus,
) {
    let status_label = tool_execution_status_label(tool_status);
    let mut item = session_turn_item(
        "tool_call_result",
        turn_item_status_for_tool_result(tool_status),
        Some(tool_call.function.name.clone()),
        Some(summarize_tool_result(tool_result)),
        Some(format!("turn-item-tool-{}", tool_call.id)),
    );
    apply_task_turn_visibility(&mut item, task, turn_visibility);
    item.tool_call_id = Some(tool_call.id.clone());
    item.tool_name = Some(tool_call.function.name.clone());
    item.tool_status = Some(status_label.to_string());
    item.tool_arguments = Some(tool_call.function.arguments.clone());
    item.tool_result = Some(tool_result.to_string());
    if !matches!(tool_status, ExecutionResultStatus::Succeeded) {
        item.tool_error = Some(tool_result.to_string());
    }
    if let Some(published) =
        upsert_session_turn_item_with_task_store(session_store, session_id, item, Some(task_store))
    {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
}

fn execute_task_tool_call_batch(
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&magi_skill_runtime::SkillRuntime>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
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
                        task,
                        session_id,
                        workspace_id,
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
                                        task,
                                        session_id,
                                        workspace_id,
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

fn execute_task_tool_call(
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&magi_skill_runtime::SkillRuntime>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    worker_id: Option<&magi_core::WorkerId>,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
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
        },
        &ToolExecutionPolicy::default(),
    );

    (output.payload, output.status)
}

fn tool_call_record(tool_call: &ChatToolCall, result: &str) -> serde_json::Value {
    let status = infer_tool_call_status(result);
    serde_json::json!({
        "type": "tool_call",
        "content": format!("{}: {}", tool_call.function.name, summarize_tool_result(result)),
        "toolCall": {
            "id": tool_call.id,
            "name": tool_call.function.name,
            "arguments": serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments)
                .unwrap_or(serde_json::Value::String(tool_call.function.arguments.clone())),
            "status": status,
            "result": result,
        }
    })
}

fn append_task_final_turn_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    final_content: &str,
    final_item_id: Option<&str>,
    timeline_entry_id: Option<&str>,
    turn_visibility: &TaskTurnVisibility,
) {
    let has_requested_final_item_id = final_item_id.is_some();
    let mut final_item = session_turn_item(
        "assistant_final",
        "completed",
        Some("最终回复".to_string()),
        Some(final_content.to_string()),
        final_item_id.map(str::to_string),
    );
    apply_task_turn_visibility(&mut final_item, task, turn_visibility);
    if let Some(timeline_entry_id) = timeline_entry_id {
        final_item.timeline_entry_id = Some(timeline_entry_id.to_string());
    }
    let final_item_id = final_item.item_id.clone();
    if has_requested_final_item_id {
        if let Some(published) = upsert_session_turn_item_with_task_store(
            session_store,
            session_id,
            final_item,
            Some(task_store),
        ) {
            publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
        }
    } else if let Some(published) = append_session_turn_item_with_task_store(
        session_store,
        session_id,
        final_item,
        Some(task_store),
    ) {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
    let root_task_completed = task_store
        .get_task(&task.root_task_id)
        .is_some_and(|root_task| root_task.status == TaskStatus::Completed);
    if turn_visibility.thread_visible && root_task_completed {
        let _ = session_store.update_current_turn_status(session_id, "completed");
        publish_current_session_turn_item_event(
            event_bus,
            session_store,
            session_id,
            workspace_id,
            &final_item_id,
            Some(task_store),
        );
        let snapshot_entry_id =
            timeline_entry_id.or(has_requested_final_item_id.then_some(final_item_id.as_str()));
        let timeline_message = build_completed_turn_timeline_snapshot(
            session_store,
            session_id,
            Some(final_content),
            snapshot_entry_id,
            Some(task_store),
        )
        .unwrap_or_else(|| final_content.to_string());
        let fallback_entry_id = format!("timeline-turn-snapshot-{}", task.task_id);
        let entry_id = snapshot_entry_id.unwrap_or(fallback_entry_id.as_str());
        session_store.upsert_timeline_entry(
            session_id.clone(),
            entry_id,
            TimelineEntryKind::AssistantMessage,
            timeline_message,
        );
    }
}

fn append_task_error_turn_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    turn_visibility: &TaskTurnVisibility,
    error_text: &str,
    streaming_entry_id: Option<&str>,
) {
    let mut error_item = session_turn_item(
        "assistant_error",
        "failed",
        Some("回复生成失败".to_string()),
        Some(error_text.to_string()),
        Some(format!("turn-item-assistant-error-{}", UtcMillis::now().0)),
    );
    apply_task_turn_visibility(&mut error_item, task, turn_visibility);
    let error_item_id = error_item.item_id.clone();
    if let Some(published) = append_session_turn_item_with_task_store(
        session_store,
        session_id,
        error_item,
        Some(task_store),
    ) {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
    if turn_visibility.thread_visible {
        let _ = session_store.update_current_turn_status(session_id, "failed");
        publish_current_session_turn_item_event(
            event_bus,
            session_store,
            session_id,
            workspace_id,
            &error_item_id,
            Some(task_store),
        );
        let timeline_message = build_completed_turn_timeline_snapshot(
            session_store,
            session_id,
            Some(error_text),
            streaming_entry_id,
            Some(task_store),
        )
        .unwrap_or_else(|| error_text.to_string());
        let fallback_entry_id = format!("timeline-turn-snapshot-error-{}", task.task_id);
        let entry_id = streaming_entry_id.unwrap_or(fallback_entry_id.as_str());
        session_store.upsert_timeline_entry(
            session_id.clone(),
            entry_id,
            TimelineEntryKind::AssistantMessage,
            timeline_message,
        );
    }
}

fn build_output_content(
    mut tool_call_records: Vec<serde_json::Value>,
    final_content: String,
) -> String {
    if tool_call_records.is_empty() {
        return final_content;
    }
    tool_call_records.push(serde_json::json!({
        "type": "text",
        "content": final_content,
    }));
    serde_json::json!({ "blocks": tool_call_records }).to_string()
}

fn task_lease_is_current(task_store: &TaskStore, task_id: &TaskId, lease_id: &LeaseId) -> bool {
    task_store
        .get_active_lease(task_id)
        .is_some_and(|lease| lease.lease_id == *lease_id)
}

fn task_stream_item_id(task_id: &TaskId, round: usize, streaming_entry_id: Option<&str>) -> String {
    if round == 0 {
        return streaming_entry_id
            .map(str::to_string)
            .unwrap_or_else(|| format!("turn-item-assistant-stream-{task_id}-{round}"));
    }
    format!("turn-item-assistant-stream-{task_id}-{round}")
}

fn task_is_thread_visible_turn_owner(
    session_store: &SessionStore,
    session_id: &SessionId,
    task_id: &TaskId,
) -> bool {
    session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .is_some_and(|turn| {
            turn.items.iter().any(|item| {
                item.task_id.as_ref() == Some(task_id)
                    && item.thread_visible
                    && item.kind == "assistant_phase"
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::{BridgeClientError, BridgeErrorLayer, BridgeResponse};
    use magi_core::{MissionId, Task, TaskKind, TaskStatus, WorkerId};
    use magi_governance::GovernanceService;
    use magi_session_store::{ActiveExecutionTurn, ActiveExecutionTurnLane, TimelineEntryKind};
    use magi_tool_runtime::{BuiltinTool, BuiltinToolSpec};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    struct TaskToolBatchModelBridgeClient {
        invoke_count: AtomicUsize,
    }

    struct FailingTaskModelBridgeClient;

    impl ModelBridgeClient for TaskToolBatchModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            let index = self.invoke_count.fetch_add(1, Ordering::SeqCst);
            let payload = if index == 0 {
                serde_json::json!({
                    "content": null,
                    "finish_reason": "tool_calls",
                    "tool_calls": [
                        {
                            "id": "task-tool-shell-a",
                            "type": "function",
                            "function": {
                                "name": "shell_exec",
                                "arguments": serde_json::json!({
                                    "command": "printf a",
                                    "access_mode": "read_only"
                                }).to_string()
                            }
                        },
                        {
                            "id": "task-tool-shell-b",
                            "type": "function",
                            "function": {
                                "name": "shell",
                                "arguments": serde_json::json!({
                                    "command": "printf b",
                                    "access_mode": "read_only"
                                }).to_string()
                            }
                        }
                    ]
                })
            } else {
                let tool_message_ids = request
                    .messages
                    .as_ref()
                    .expect("工具响应轮次必须携带消息上下文")
                    .iter()
                    .filter(|message| message.role == "tool")
                    .map(|message| message.tool_call_id.as_deref())
                    .collect::<Vec<_>>();
                assert_eq!(
                    tool_message_ids,
                    vec![Some("task-tool-shell-a"), Some("task-tool-shell-b")]
                );
                serde_json::json!({
                    "content": "任务工具调用完成",
                    "finish_reason": "stop"
                })
            };
            Ok(BridgeResponse {
                ok: true,
                payload: payload.to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            if self.invoke_count.load(Ordering::SeqCst) > 0 {
                on_delta(&ModelStreamingDelta {
                    content: "任务工具调用完成".to_string(),
                    thinking: String::new(),
                });
            }
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for FailingTaskModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32099),
                message: "model bridge unavailable".to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            self.invoke(request)
        }
    }

    struct ConcurrentTaskToolProbe {
        active: AtomicUsize,
        max_active: AtomicUsize,
        delay: Duration,
    }

    impl ConcurrentTaskToolProbe {
        fn new(delay: Duration) -> Self {
            Self {
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
                delay,
            }
        }

        fn max_active(&self) -> usize {
            self.max_active.load(Ordering::SeqCst)
        }

        fn record_active_call(&self) {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            thread::sleep(self.delay);
            self.active.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct ProbeTaskBuiltinTool {
        name: &'static str,
        probe: Arc<ConcurrentTaskToolProbe>,
    }

    impl ProbeTaskBuiltinTool {
        fn new(name: &'static str, probe: Arc<ConcurrentTaskToolProbe>) -> Self {
            Self { name, probe }
        }
    }

    impl BuiltinTool for ProbeTaskBuiltinTool {
        fn name(&self) -> &'static str {
            self.name
        }

        fn execute(&self, input: &str, _context: &ToolExecutionContext) -> String {
            self.probe.record_active_call();
            serde_json::json!({
                "tool": self.name,
                "status": "succeeded",
                "stdout": format!("{} done", self.name),
                "input": input,
            })
            .to_string()
        }

        fn spec(&self) -> BuiltinToolSpec {
            BuiltinToolSpec {
                name: self.name.to_string(),
                risk_level: RiskLevel::Low,
                approval_requirement: ApprovalRequirement::None,
            }
        }
    }

    fn make_task_loop_test_task(task_id: &str) -> Task {
        Task {
            task_id: TaskId::new(task_id),
            mission_id: MissionId::new("mission-task-loop"),
            root_task_id: TaskId::new(task_id),
            parent_task_id: None,
            kind: TaskKind::Action,
            title: "验证 worker 工具并发".to_string(),
            goal: "同一轮只读 shell 工具需要并发执行，并保持消息顺序".to_string(),
            status: TaskStatus::Running,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            context_refs: Vec::new(),
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            repair_count: 0,
            decision_payload: None,
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        }
    }

    #[test]
    fn task_stream_item_id_reuses_main_timeline_streaming_entry_only_for_first_round() {
        let task_id = TaskId::new("task-stream-main");

        assert_eq!(
            task_stream_item_id(&task_id, 0, Some("timeline-streaming-task-stream-main")),
            "timeline-streaming-task-stream-main"
        );
        assert_eq!(
            task_stream_item_id(&task_id, 3, Some("timeline-streaming-task-stream-main")),
            "turn-item-assistant-stream-task-stream-main-3"
        );
    }

    #[test]
    fn task_stream_item_id_keeps_round_scope_without_main_streaming_entry() {
        let task_id = TaskId::new("task-stream-worker");

        assert_eq!(
            task_stream_item_id(&task_id, 2, None),
            "turn-item-assistant-stream-task-stream-worker-2"
        );
    }

    #[test]
    fn task_turn_visibility_does_not_promote_primary_role_to_worker_lane() {
        let task = make_task_loop_test_task("task-primary-role-only");

        let visibility = task_turn_visibility(&task, true, None, None, None);

        assert!(visibility.thread_visible);
        assert!(!visibility.worker_visible);
        assert_eq!(visibility.role_id.as_deref(), Some("integration-dev"));
        assert!(visibility.lane_id.is_none());
        assert!(visibility.lane_seq.is_none());
    }

    #[test]
    fn task_turn_visibility_uses_authoritative_worker_lane_from_plan() {
        let task = make_task_loop_test_task("task-worker-lane-order");
        let worker_id = WorkerId::new("worker-worker-lane-order");
        let visibility = task_turn_visibility(
            &task,
            false,
            Some("lane-task-worker-lane-order"),
            Some(3),
            Some(&worker_id),
        );
        let mut item = session_turn_item(
            "assistant_final",
            "completed",
            Some("最终回复".to_string()),
            Some("worker 输出".to_string()),
            Some("turn-item-worker-final".to_string()),
        );

        apply_task_turn_visibility(&mut item, &task, &visibility);

        assert!(!item.thread_visible);
        assert!(item.worker_visible);
        assert_eq!(item.lane_id.as_deref(), Some("lane-task-worker-lane-order"));
        assert_eq!(item.lane_seq, Some(3));
        assert_eq!(item.worker_id.as_ref(), Some(&worker_id));
        assert_eq!(item.role_id.as_deref(), Some("integration-dev"));
    }

    #[test]
    fn task_final_turn_item_does_not_complete_turn_before_root_task_completes() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(16);
        let session_id = SessionId::new("session-task-final-root-running");
        session_store
            .create_session(session_id.clone(), "task final root running")
            .expect("session should be creatable");
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-task-final-root-running".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("执行深度任务".to_string()),
                    items: Vec::new(),
                    worker_lanes: Vec::new(),
                },
            )
            .expect("turn should be stored");

        let task_store = TaskStore::new();
        let root_task_id = TaskId::new("task-root-final-root-running");
        let task_id = TaskId::new("task-action-final-root-running");
        let mut root_task = make_task_loop_test_task(root_task_id.as_str());
        root_task.kind = TaskKind::Objective;
        root_task.status = TaskStatus::Running;
        task_store.insert_task(root_task);
        let mut task = make_task_loop_test_task(task_id.as_str());
        task.root_task_id = root_task_id;
        task.status = TaskStatus::Completed;
        task_store.insert_task(task.clone());
        let visibility = task_turn_visibility(
            &task,
            true,
            None,
            None,
            Some(&WorkerId::new("worker-final-root-running")),
        );

        append_task_final_turn_item(
            &event_bus,
            &session_store,
            &task_store,
            &task,
            &session_id,
            &None,
            "primary action 已完成",
            Some("timeline-streaming-task-action-final-root-running"),
            Some("timeline-streaming-task-action-final-root-running"),
            &visibility,
        );

        let current_turn = session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("current turn should remain");
        assert_eq!(current_turn.status, "running");
        assert!(current_turn.completed_at.is_none());
        assert!(
            session_store
                .timeline_for_session(&session_id)
                .iter()
                .all(|entry| !matches!(entry.kind, TimelineEntryKind::AssistantMessage)),
            "root 未完成时不能写入 completed turn snapshot"
        );
    }

    #[test]
    fn task_llm_loop_model_failure_writes_failed_turn_item_and_snapshot() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(64);
        let session_id = SessionId::new("session-task-model-failure");
        let workspace_id = Some(WorkspaceId::new("workspace-task-model-failure"));
        let task_id = TaskId::new("task-model-failure");
        let worker_id = WorkerId::new("worker-task-model-failure");
        let streaming_entry_id = "timeline-streaming-task-model-failure";
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "task model failure session",
                workspace_id.as_ref().map(ToString::to_string),
            )
            .expect("session should be creatable");
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-task-model-failure".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    status: "running".to_string(),
                    user_message: Some("验证模型失败写回".to_string()),
                    items: Vec::new(),
                    worker_lanes: Vec::new(),
                    completed_at: None,
                },
            )
            .expect("turn should be creatable");

        let task_store = TaskStore::new();
        let task = make_task_loop_test_task(task_id.as_str());
        task_store.insert_task(task.clone());
        let lease = task_store
            .grant_lease(
                &task.task_id,
                &task.root_task_id,
                &worker_id,
                "integration-dev",
                60_000,
            )
            .expect("lease should be granted");
        let usage_binding = crate::usage_recording::session_turn_model_usage_binding(true);

        let (outcome, _) = run_task_llm_loop(TaskLlmLoopRequest {
            client: &FailingTaskModelBridgeClient,
            event_bus: &event_bus,
            session_store: &session_store,
            settings_store: None,
            tool_registry: None,
            skill_runtime: None,
            task_store: &task_store,
            task: &task,
            task_id: &task.task_id,
            lease_id: &lease.lease_id,
            session_id: &session_id,
            workspace_id: &workspace_id,
            prompt: "请生成回复".to_string(),
            tools: None,
            usage_binding: &usage_binding,
            streaming_entry_id: Some(streaming_entry_id),
            worker_lane_id: None,
            worker_lane_seq: None,
            worker_id: None,
            context_summary: None,
            system_prompt: None,
        });

        match outcome {
            TaskOutcome::Failed { error } => {
                assert!(error.contains("model bridge unavailable"));
            }
            other => panic!("model failure must fail the task loop, got {other:?}"),
        }

        let turn = session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("failed turn should remain inspectable");
        assert_eq!(turn.status, "failed");
        assert!(turn.completed_at.is_some());
        let error_item = turn
            .items
            .iter()
            .find(|item| item.kind == "assistant_error")
            .expect("assistant_error should be appended");
        assert!(error_item.thread_visible);
        assert_eq!(error_item.status, "failed");
        assert_eq!(error_item.task_id.as_ref(), Some(&task_id));
        assert!(
            error_item
                .content
                .as_deref()
                .is_some_and(|content| content.contains("model bridge unavailable"))
        );

        let timeline = session_store.timeline_for_session(&session_id);
        let failure_snapshot = timeline
            .iter()
            .find(|entry| entry.entry_id == streaming_entry_id)
            .expect("failed turn snapshot should reuse streaming entry id");
        assert!(matches!(
            failure_snapshot.kind,
            TimelineEntryKind::AssistantMessage
        ));
        assert!(failure_snapshot.message.contains("assistant_error"));
        assert!(
            failure_snapshot
                .message
                .contains("model bridge unavailable")
        );

        let terminal_error_event = event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .rev()
            .find(|event| {
                event.event_type == "session.turn.item"
                    && event.payload["item"]["kind"] == "assistant_error"
            })
            .expect("assistant_error item event should be published");
        assert_eq!(
            terminal_error_event.payload["current_turn"]["status"],
            serde_json::Value::String("failed".to_string())
        );
        assert!(
            terminal_error_event.payload["current_turn"]["response_duration_ms"].is_number(),
            "terminal error event must carry backend duration for live UI"
        );
    }

    #[test]
    fn task_llm_loop_read_only_shell_tools_execute_concurrently_and_preserve_order() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(64);
        let session_id = SessionId::new("session-task-tool-batch");
        let workspace_id = Some(WorkspaceId::new("workspace-task-tool-batch"));
        let task_id = TaskId::new("task-tool-batch");
        let worker_id = WorkerId::new("worker-task-tool-batch");
        let lane_id = "lane-task-tool-batch".to_string();
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "task tool batch session",
                workspace_id.as_ref().map(ToString::to_string),
            )
            .expect("session should be creatable");
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-task-tool-batch".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    status: "running".to_string(),
                    user_message: Some("验证 worker 工具并发".to_string()),
                    items: Vec::new(),
                    worker_lanes: vec![ActiveExecutionTurnLane {
                        lane_id: lane_id.clone(),
                        lane_seq: 2,
                        task_id: task_id.clone(),
                        worker_id: worker_id.clone(),
                        role_id: Some("integration-dev".to_string()),
                        title: "验证 worker 工具并发".to_string(),
                        is_primary: false,
                    }],
                    completed_at: None,
                },
            )
            .expect("turn should be creatable");

        let task_store = TaskStore::new();
        let task = make_task_loop_test_task(task_id.as_str());
        task_store.insert_task(task.clone());
        let lease = task_store
            .grant_lease(
                &task.task_id,
                &task.root_task_id,
                &worker_id,
                "integration-dev",
                60_000,
            )
            .expect("lease should be granted");

        let probe = Arc::new(ConcurrentTaskToolProbe::new(Duration::from_millis(180)));
        let tool_event_bus = Arc::new(InMemoryEventBus::new(8));
        let mut tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::clone(&tool_event_bus),
        );
        tool_registry.register_builtin(Arc::new(ProbeTaskBuiltinTool::new(
            "shell_exec",
            Arc::clone(&probe),
        )));
        tool_registry.register_builtin(Arc::new(ProbeTaskBuiltinTool::new(
            "shell",
            Arc::clone(&probe),
        )));
        let client = TaskToolBatchModelBridgeClient {
            invoke_count: AtomicUsize::new(0),
        };
        let usage_binding = crate::usage_recording::session_turn_model_usage_binding(true);

        let (outcome, _) = run_task_llm_loop(TaskLlmLoopRequest {
            client: &client,
            event_bus: &event_bus,
            session_store: &session_store,
            settings_store: None,
            tool_registry: Some(&tool_registry),
            skill_runtime: None,
            task_store: &task_store,
            task: &task,
            task_id: &task.task_id,
            lease_id: &lease.lease_id,
            session_id: &session_id,
            workspace_id: &workspace_id,
            prompt: "请执行两个只读 shell 工具".to_string(),
            tools: None,
            usage_binding: &usage_binding,
            streaming_entry_id: Some("timeline-streaming-task-tool-batch"),
            worker_lane_id: Some(&lane_id),
            worker_lane_seq: Some(2),
            worker_id: Some(&worker_id),
            context_summary: None,
            system_prompt: None,
        });

        assert!(
            probe.max_active() > 1,
            "task worker 中的多个只读 shell 工具调用必须并发执行"
        );
        let output_refs = match outcome {
            TaskOutcome::Completed { output_refs } => output_refs,
            other => panic!("task loop should complete, got {other:?}"),
        };
        let output: serde_json::Value =
            serde_json::from_str(&output_refs[0]).expect("output blocks json");
        assert_eq!(
            output["blocks"][0]["toolCall"]["id"],
            serde_json::Value::String("task-tool-shell-a".to_string())
        );
        assert_eq!(
            output["blocks"][1]["toolCall"]["id"],
            serde_json::Value::String("task-tool-shell-b".to_string())
        );

        let sidecar = session_store
            .runtime_sidecar(&session_id)
            .expect("sidecar should exist");
        let turn = sidecar.current_turn.expect("turn should exist");
        assert!(
            turn.items.iter().all(|item| {
                item.worker_visible
                    && item.lane_id.as_deref() == Some(lane_id.as_str())
                    && item.lane_seq == Some(2)
            }),
            "worker 输出必须沿用执行计划中的 lane 归属与顺序"
        );
        assert_eq!(
            turn.items
                .iter()
                .map(|item| (item.kind.as_str(), item.tool_call_id.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("tool_call_result", Some("task-tool-shell-a")),
                ("tool_call_result", Some("task-tool-shell-b")),
                ("assistant_final", None),
            ]
        );
        assert!(
            session_store
                .timeline_for_session(&session_id)
                .iter()
                .all(|entry| entry.entry_id != "turn-item-assistant-stream-task-tool-batch-1"),
            "工具后的第二轮流式内容不能写成独立主线 timeline entry"
        );
        let tool_events = event_bus.snapshot().recent_events;
        let invoked_events = tool_events
            .iter()
            .filter(|event| event.event_type == "task.tool.invoked")
            .collect::<Vec<_>>();
        assert_eq!(invoked_events.len(), 2);
        assert!(
            invoked_events.iter().all(|event| event.payload["worker_id"]
                == serde_json::Value::String(worker_id.to_string())),
            "worker 工具事件必须携带执行 worker，供 worker tab 和 runtime 归属使用"
        );
        let runtime_tool_events = tool_event_bus.snapshot().recent_events;
        assert!(
            runtime_tool_events
                .iter()
                .filter(|event| {
                    event.event_type == "tool.invoked" || event.event_type == "tool.usage.recorded"
                })
                .all(|event| event.payload["worker_id"]
                    == serde_json::Value::String(worker_id.to_string())),
            "工具运行时事件也必须沿用同一个 worker 归属"
        );
    }
}
