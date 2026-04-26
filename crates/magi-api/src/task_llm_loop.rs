use crate::{
    session_turn_writeback::{
        append_session_turn_item, build_completed_turn_timeline_snapshot,
        publish_session_turn_item_event, session_turn_item, upsert_session_turn_item,
    },
    settings_store::SettingsStore,
    skill_apply_tool::{SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime},
    tool_result_utils::{infer_tool_call_status, summarize_tool_result},
    usage_recording::{ModelUsageBinding, publish_model_usage_record},
};
use magi_bridge_client::{
    ChatMessage, ChatToolCall, ChatToolDefinition, ModelBridgeClient, ModelInvocationRequest,
    ModelStreamingDelta, SHADOW_MODEL_PROVIDER,
};
use magi_core::{
    ApprovalRequirement, EventId, LeaseId, RiskLevel, SessionId, TaskId, ToolCallId, UtcMillis,
    WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::ToolKind;
use magi_orchestrator::{ExecutionContextSummary, task_runner::TaskOutcome, task_store::TaskStore};
use magi_session_store::{SessionStore, TimelineEntryKind};
use magi_tool_runtime::{
    ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
};
use magi_usage_authority::UsageCallStatus;
use std::sync::Arc;

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
    pub context_summary: Option<ExecutionContextSummary>,
    pub system_prompt: Option<String>,
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

    for round in 0..MAX_TOOL_CALL_ROUNDS {
        let should_record_turn_artifacts = streaming_entry_id.is_some()
            || task_is_thread_visible_turn_owner(session_store, session_id, task_id);
        let thinking_item_id = format!("turn-item-assistant-thinking-{task_id}-{round}");
        let stream_item_id = format!("turn-item-assistant-stream-{task_id}-{round}");
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
            let last_sent_len = std::cell::Cell::new(0usize);
            let task_id_str = task.task_id.to_string();
            let mission_id_str = task.mission_id.to_string();
            let on_delta = |delta: &ModelStreamingDelta| {
                if should_record_turn_artifacts {
                    publish_task_thinking_delta(
                        event_bus,
                        session_store,
                        task,
                        session_id,
                        workspace_id,
                        &thinking_item_id,
                        &last_thinking_len,
                        &streamed_thinking,
                        &delta.thinking,
                    );
                }
                publish_stream_delta(
                    event_bus,
                    session_store,
                    task,
                    session_id,
                    workspace_id,
                    &stream_item_id,
                    &task_context,
                    &task_id_str,
                    &mission_id_str,
                    &last_sent_len,
                    &delta.content,
                );
            };

            match client.invoke_streaming(invocation_request, &on_delta) {
                Ok(response) => response,
                Err(error) => {
                    tracing::error!(task_id = %task.task_id, round = round, ?error, "LLM streaming invocation failed");
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
                    task,
                    session_id,
                    workspace_id,
                    &thinking_item_id,
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

        if parsed.tool_calls.is_empty() {
            publish_task_llm_completed(
                event_bus,
                task,
                session_id,
                workspace_id,
                streaming_entry_id.or(last_stream_item_id.as_deref()),
                final_content.len(),
                round + 1,
                &task_context,
            );
            break;
        }

        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: parsed.content.clone(),
            tool_calls: parsed.tool_calls.clone(),
            tool_call_id: None,
        });

        for tool_call in &parsed.tool_calls {
            let result = execute_task_tool_call(
                event_bus,
                tool_registry,
                skill_runtime,
                task,
                session_id,
                workspace_id,
                tool_call,
            );
            tool_call_records.push(tool_call_record(tool_call, &result));
            messages.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(result),
                tool_calls: Vec::new(),
                tool_call_id: Some(tool_call.id.clone()),
            });
        }
    }

    if final_content.is_empty() {
        final_content = "[LLM 未返回文本响应]".to_string();
    }
    final_content = crate::prompt_utils::normalize_model_visible_content(final_content);
    if !task_lease_is_current(task_store, task_id, lease_id) {
        return (
            TaskOutcome::Failed {
                error: "任务执行已被中断，丢弃晚到模型结果".to_string(),
            },
            context_summary,
        );
    }
    if streaming_entry_id.is_some()
        || task_is_thread_visible_turn_owner(session_store, session_id, task_id)
    {
        append_task_final_turn_item(
            event_bus,
            session_store,
            task,
            session_id,
            workspace_id,
            &final_content,
            streaming_entry_id.or(last_stream_item_id.as_deref()),
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
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    entry_id: &str,
    task_context: &EventContext,
    task_id: &str,
    mission_id: &str,
    last_sent_len: &std::cell::Cell<usize>,
    accumulated_text: &str,
) {
    session_store.upsert_timeline_entry(
        session_id.clone(),
        entry_id,
        TimelineEntryKind::AssistantMessage,
        accumulated_text,
    );
    let mut item = session_turn_item(
        "assistant_stream",
        "running",
        Some("生成回复".to_string()),
        Some(accumulated_text.to_string()),
        Some(entry_id.to_string()),
    );
    item.task_id = Some(task.task_id.clone());
    upsert_session_turn_item(session_store, session_id, item.clone());
    publish_session_turn_item_event(event_bus, session_id, workspace_id, &item);

    let previous_len = last_sent_len.get();
    let delta = &accumulated_text[previous_len..];
    if delta.is_empty() {
        return;
    }
    last_sent_len.set(accumulated_text.len());

    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-task-llm-delta-{}", UtcMillis::now().0)),
            "task.llm.delta",
            serde_json::json!({
                "task_id": task_id,
                "mission_id": mission_id,
                "session_id": session_id.to_string(),
                "entry_id": entry_id,
                "delta": delta,
            }),
        )
        .with_context(task_context.clone()),
    );
}

fn publish_task_thinking_delta(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    item_id: &str,
    last_sent_len: &std::cell::Cell<usize>,
    streamed_thinking: &std::cell::RefCell<String>,
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
        task,
        session_id,
        workspace_id,
        item_id,
        "running",
        accumulated_thinking,
    );
}

fn upsert_task_thinking_turn_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    item_id: &str,
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
    item.task_id = Some(task.task_id.clone());
    upsert_session_turn_item(session_store, session_id, item.clone());
    publish_session_turn_item_event(event_bus, session_id, workspace_id, &item);
}

fn publish_task_llm_completed(
    event_bus: &InMemoryEventBus,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    streaming_entry_id: Option<&str>,
    response_length: usize,
    rounds: usize,
    task_context: &EventContext,
) {
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-task-llm-completed-{}", UtcMillis::now().0)),
            "task.llm.completed",
            serde_json::json!({
                "task_id": task.task_id.to_string(),
                "mission_id": task.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "entry_id": streaming_entry_id,
                "response_length": response_length,
                "rounds": rounds,
            }),
        )
        .with_context(task_context.clone()),
    );
}

fn execute_task_tool_call(
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&magi_skill_runtime::SkillRuntime>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    tool_call: &ChatToolCall,
) -> String {
    let Some(registry) = tool_registry else {
        return serde_json::json!({ "error": "tool registry not available" }).to_string();
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
        let (payload, _) =
            execute_skill_apply_from_runtime(&tool_call.function.arguments, skill_runtime);
        return payload;
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
            worker_id: None,
            task_id: Some(task.task_id.clone()),
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.clone(),
        },
        &ToolExecutionPolicy::default(),
    );

    output.payload
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
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    final_content: &str,
    streaming_entry_id: Option<&str>,
) {
    let mut final_item = session_turn_item(
        "assistant_final",
        "completed",
        Some("最终回复".to_string()),
        Some(final_content.to_string()),
        streaming_entry_id.map(str::to_string),
    );
    final_item.task_id = Some(task.task_id.clone());
    if streaming_entry_id.is_some() {
        upsert_session_turn_item(session_store, session_id, final_item.clone());
    } else {
        append_session_turn_item(session_store, session_id, final_item.clone());
    }
    publish_session_turn_item_event(event_bus, session_id, workspace_id, &final_item);
    let _ = session_store.update_current_turn_status(session_id, "completed");
    let timeline_message = build_completed_turn_timeline_snapshot(
        session_store,
        session_id,
        Some(final_content),
        streaming_entry_id,
    )
    .unwrap_or_else(|| final_content.to_string());
    let fallback_entry_id = format!("timeline-turn-snapshot-{}", task.task_id);
    let entry_id = streaming_entry_id.unwrap_or(fallback_entry_id.as_str());
    session_store.upsert_timeline_entry(
        session_id.clone(),
        entry_id,
        TimelineEntryKind::AssistantMessage,
        timeline_message,
    );
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
