use super::*;

#[derive(Clone, Copy)]
pub(super) struct TaskTurnWritebackContext<'a> {
    pub(super) event_bus: &'a InMemoryEventBus,
    pub(super) session_store: &'a SessionStore,
    pub(super) task_store: &'a TaskStore,
    pub(super) task: &'a magi_core::Task,
    pub(super) session_id: &'a SessionId,
    pub(super) workspace_id: &'a Option<WorkspaceId>,
    pub(super) turn_visibility: &'a TaskTurnVisibility,
    pub(super) persist_session_state: Option<&'a SessionStatePersistCallback>,
    pub(super) expected_turn_id: Option<&'a str>,
    pub(super) model_step_id: Option<&'a str>,
    pub(super) writeback_error: &'a std::sync::Mutex<Option<String>>,
}

pub(super) fn apply_step_identity(
    item: &mut magi_session_store::ActiveExecutionTurnItem,
    context: TaskTurnWritebackContext<'_>,
) {
    if let Some(step_id) = context.model_step_id {
        item.metadata
            .insert("modelStepId".into(), serde_json::json!(step_id));
    }
}

pub(super) fn record_task_writeback_error(context: TaskTurnWritebackContext<'_>, error: String) {
    let mut writeback_error = context
        .writeback_error
        .lock()
        .expect("writeback error lock should hold");
    if writeback_error.is_none() {
        *writeback_error = Some(error);
    }
}

pub(super) fn take_task_writeback_error(context: TaskTurnWritebackContext<'_>) -> Option<String> {
    context
        .writeback_error
        .lock()
        .expect("writeback error lock should hold")
        .take()
}

pub(super) fn task_failure_with_error_item(
    context: TaskTurnWritebackContext<'_>,
    error_text: &str,
    timeline_entry_id: Option<&str>,
    model_failure: Option<&ModelFailureDiagnostic>,
    tool_call_failure: Option<&ToolCallFailureDiagnostic>,
) -> String {
    match append_task_error_turn_item(
        context,
        error_text,
        timeline_entry_id,
        model_failure,
        tool_call_failure,
    ) {
        Ok(()) => error_text.to_string(),
        Err(writeback_error) => {
            format!("{error_text}；失败事实写回失败：{writeback_error}")
        }
    }
}

pub(super) fn task_context_compaction_terminal_outcome(
    context: TaskTurnWritebackContext<'_>,
    terminal: ContextCompactionTerminal,
    timeline_entry_id: Option<&str>,
) -> Result<TaskOutcome, String> {
    match terminal {
        ContextCompactionTerminal::Cancelled => Ok(TaskOutcome::Failed {
            error: "任务已中断".to_string(),
        }),
        ContextCompactionTerminal::Failed => {
            let error = "上下文压缩失败，任务已停止。请检查辅助模型配置或网络后继续目标。";
            append_task_error_turn_item(context, error, timeline_entry_id, None, None)?;
            Ok(TaskOutcome::Failed {
                error: error.to_string(),
            })
        }
    }
}

pub(super) fn task_context_compaction_terminal_outcome_or_failed(
    context: TaskTurnWritebackContext<'_>,
    terminal: ContextCompactionTerminal,
    timeline_entry_id: Option<&str>,
) -> TaskOutcome {
    match task_context_compaction_terminal_outcome(context, terminal, timeline_entry_id) {
        Ok(outcome) => outcome,
        Err(error) => TaskOutcome::Failed { error },
    }
}

pub(super) fn publish_task_llm_started(
    event_bus: &InMemoryEventBus,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    prompt: &str,
    task_context: &EventContext,
) {
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::unique("event-task-llm-started"),
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

pub(super) fn publish_task_thinking_delta(
    context: TaskTurnWritebackContext<'_>,
    item_id: &str,
    model_round: usize,
    last_sent_len: &std::cell::Cell<usize>,
    streamed_thinking: &std::cell::RefCell<String>,
    publish_gate: &std::cell::RefCell<SessionTurnStreamPublishGate>,
    accumulated_thinking: &str,
) -> Result<(), String> {
    if accumulated_thinking.len() <= last_sent_len.get() {
        return Ok(());
    }
    let trimmed = accumulated_thinking.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let stream_update = {
        let previous = streamed_thinking.borrow();
        let update = session_turn_stream_update(previous.trim(), trimmed);
        if update.is_none() {
            return Ok(());
        }
        update
    };
    last_sent_len.set(accumulated_thinking.len());
    {
        let mut thinking = streamed_thinking.borrow_mut();
        thinking.clear();
        thinking.push_str(accumulated_thinking);
    }
    upsert_task_thinking_turn_item(
        context,
        item_id,
        model_round,
        "running",
        trimmed,
        stream_update.as_ref(),
        publish_gate,
    )
}

pub(super) fn upsert_task_thinking_turn_item(
    context: TaskTurnWritebackContext<'_>,
    item_id: &str,
    model_round: usize,
    status: &str,
    thinking: &str,
    stream_update: Option<&SessionTurnStreamUpdate>,
    publish_gate: &std::cell::RefCell<SessionTurnStreamPublishGate>,
) -> Result<(), String> {
    let trimmed = thinking.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let mut item = session_turn_item(
        "assistant_thinking",
        status,
        Some("模型思考".to_string()),
        Some(trimmed.to_string()),
        Some(item_id.to_string()),
        context.turn_visibility.thread_id().clone(),
    );
    apply_model_response_round(&mut item, model_round);
    apply_step_identity(&mut item, context);
    apply_task_worker_detail_visibility(&mut item, context.task, context.turn_visibility);
    let Some(published) = upsert_session_turn_item_for_turn(
        context.session_store,
        context.session_id,
        context.expected_turn_id,
        item,
        Some(context.task_store),
    )?
    else {
        return Err(format!(
            "会话 {} 的当前 Turn 已不可写，无法保存任务思考 item {}",
            context.session_id, item_id
        ));
    };
    {
        if let Some(stream_update) = stream_update {
            publish_session_turn_item_stream_event(
                context.event_bus,
                context.session_id,
                context.workspace_id,
                &published,
                stream_update,
                &mut publish_gate.borrow_mut(),
            );
        } else {
            publish_session_turn_item_event(
                context.event_bus,
                context.session_id,
                context.workspace_id,
                &published,
            );
        }
    }
    Ok(())
}

pub(super) struct TaskContentDelta<'a> {
    pub(super) item_id: &'a str,
    pub(super) model_round: usize,
    pub(super) last_sent_len: &'a std::cell::Cell<usize>,
    pub(super) streamed_content: &'a std::cell::RefCell<String>,
    pub(super) streamed_visible_content: &'a std::cell::RefCell<String>,
    pub(super) publish_gate: &'a std::cell::RefCell<SessionTurnStreamPublishGate>,
    pub(super) accumulated_content: &'a str,
}

pub(super) fn publish_task_content_delta(
    context: TaskTurnWritebackContext<'_>,
    input: TaskContentDelta<'_>,
) -> Result<(), String> {
    let TaskContentDelta {
        item_id,
        model_round,
        last_sent_len,
        streamed_content,
        streamed_visible_content,
        publish_gate,
        accumulated_content,
    } = input;
    if accumulated_content.len() <= last_sent_len.get() {
        return Ok(());
    }
    last_sent_len.set(accumulated_content.len());
    {
        let mut content = streamed_content.borrow_mut();
        content.clear();
        content.push_str(accumulated_content);
    }
    let visible_content = normalize_model_visible_content(accumulated_content);
    if visible_content.trim().is_empty() {
        return Ok(());
    }
    let stream_update = {
        let previous = streamed_visible_content.borrow();
        let update = session_turn_stream_update(previous.as_str(), &visible_content);
        if update.is_none() {
            return Ok(());
        }
        update
    };
    {
        let mut current_visible = streamed_visible_content.borrow_mut();
        current_visible.clear();
        current_visible.push_str(&visible_content);
    }
    upsert_task_stream_turn_item(
        context,
        item_id,
        model_round,
        "running",
        &visible_content,
        &[],
        stream_update.as_ref(),
        publish_gate,
    )
}

pub(super) fn persist_task_input(
    context: TaskTurnWritebackContext<'_>,
    message: &ChatMessage,
) -> Result<(), String> {
    let turn = context
        .expected_turn_id
        .and_then(|turn_id| {
            context
                .session_store
                .canonical_turn_for_session_turn_id(context.session_id, turn_id)
        })
        .ok_or_else(|| "任务输入缺少 canonical Turn".to_string())?;
    let item_id = format!(
        "turn-item-task-input-{}-{}",
        context.task.task_id,
        message.input_request_id.as_deref().unwrap_or("initial")
    );
    if turn.items.iter().any(|item| {
        item.kind == magi_session_store::CanonicalTurnItemKind::UserMessage
            && &item.source_thread_id == context.turn_visibility.thread_id()
            && (item.item_id == item_id
                || message
                    .input_request_id
                    .as_deref()
                    .is_some_and(|request_id| {
                        item.metadata
                            .get("requestId")
                            .and_then(serde_json::Value::as_str)
                            == Some(request_id)
                    }))
    }) {
        return Ok(());
    }
    let mut item = session_turn_item(
        "user_message",
        "completed",
        None,
        message.content.clone(),
        Some(item_id),
        context.turn_visibility.thread_id().clone(),
    );
    apply_task_worker_detail_visibility(&mut item, context.task, context.turn_visibility);
    item.metadata
        .insert("renderable".into(), serde_json::Value::Bool(false));
    item.request_id = message.input_request_id.clone();
    upsert_session_turn_item_for_turn(
        context.session_store,
        context.session_id,
        context.expected_turn_id,
        item,
        Some(context.task_store),
    )?
    .ok_or_else(|| "当前 Turn 已不可写，无法保存任务输入".to_string())?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn upsert_task_stream_turn_item(
    context: TaskTurnWritebackContext<'_>,
    item_id: &str,
    model_round: usize,
    status: &str,
    content: &str,
    provider_context: &[ModelProviderContext],
    stream_update: Option<&SessionTurnStreamUpdate>,
    publish_gate: &std::cell::RefCell<SessionTurnStreamPublishGate>,
) -> Result<(), String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let mut item = session_turn_item(
        "assistant_stream",
        status,
        Some("生成回复".to_string()),
        Some(trimmed.to_string()),
        Some(item_id.to_string()),
        context.turn_visibility.thread_id().clone(),
    );
    apply_model_response_round(&mut item, model_round);
    if !provider_context.is_empty() {
        item.metadata.insert(
            "providerContext".to_string(),
            serde_json::to_value(provider_context).expect("模型提供方上下文必须能够序列化"),
        );
    }
    apply_step_identity(&mut item, context);
    apply_task_worker_detail_visibility(&mut item, context.task, context.turn_visibility);
    let Some(published) = upsert_session_turn_item_for_turn(
        context.session_store,
        context.session_id,
        context.expected_turn_id,
        item,
        Some(context.task_store),
    )?
    else {
        return Err(format!(
            "会话 {} 的当前 Turn 已不可写，无法保存任务回复 item {}",
            context.session_id, item_id
        ));
    };
    {
        if let Some(stream_update) = stream_update {
            publish_session_turn_item_stream_event(
                context.event_bus,
                context.session_id,
                context.workspace_id,
                &published,
                stream_update,
                &mut publish_gate.borrow_mut(),
            );
        } else {
            publish_session_turn_item_event(
                context.event_bus,
                context.session_id,
                context.workspace_id,
                &published,
            );
        }
    }
    Ok(())
}

pub(super) fn task_tool_turn_item_id(
    context: TaskTurnWritebackContext<'_>,
    call: &ChatToolCall,
) -> String {
    use sha2::{Digest, Sha256};
    // Provider 只保证响应内的 call id；不同 Task/Step 复用 ID 不能覆盖先前执行事实。
    let identity = serde_json::to_vec(&(&context.task.task_id, context.model_step_id, &call.id))
        .expect("tool item identity is serializable");
    format!(
        "turn-item-tool-{}",
        magi_core::hex_lower(Sha256::digest(identity))
    )
}

pub(super) fn append_task_tool_call_started_turn_item(
    context: TaskTurnWritebackContext<'_>,
    tool_call: &ChatToolCall,
    provider_context: &[ModelProviderContext],
) -> Result<(), String> {
    let mut item = session_turn_item(
        "tool_call_started",
        "running",
        Some(tool_call.function.name.clone()),
        Some(format!("正在调用工具：{}", tool_call.function.name)),
        Some(task_tool_turn_item_id(context, tool_call)),
        context.turn_visibility.thread_id().clone(),
    );
    apply_step_identity(&mut item, context);
    apply_task_worker_detail_visibility(&mut item, context.task, context.turn_visibility);
    item.tool_call_id = Some(tool_call.id.clone());
    item.tool_name = Some(tool_call.function.name.clone());
    item.tool_status = Some("running".to_string());
    item.tool_arguments = Some(tool_call.function.arguments.clone());
    if !provider_context.is_empty() {
        item.metadata.insert(
            "providerContext".to_string(),
            serde_json::to_value(provider_context).expect("模型提供方上下文必须能够序列化"),
        );
    }
    let Some(published) = upsert_session_turn_item_for_turn(
        context.session_store,
        context.session_id,
        context.expected_turn_id,
        item,
        Some(context.task_store),
    )?
    else {
        return Err(format!(
            "会话 {} 的当前 Turn 已不可写，无法保存工具 {} 的开始事实",
            context.session_id, tool_call.function.name
        ));
    };
    persist_session_state_checkpoint(context.persist_session_state, "task_turn_tool_started")?;
    publish_session_turn_item_event(
        context.event_bus,
        context.session_id,
        context.workspace_id,
        &published,
    );
    Ok(())
}

pub(super) fn upsert_task_tool_call_result_turn_item(
    context: TaskTurnWritebackContext<'_>,
    tool_call: &ChatToolCall,
    tool_result: &str,
    tool_status: ExecutionResultStatus,
) -> Result<(), String> {
    let status_label = tool_status.wire_label();
    let mut item = session_turn_item(
        "tool_call_result",
        turn_item_status_for_tool_result(tool_status),
        Some(tool_call.function.name.clone()),
        Some(summarize_tool_result(tool_result)),
        Some(task_tool_turn_item_id(context, tool_call)),
        context.turn_visibility.thread_id().clone(),
    );
    apply_step_identity(&mut item, context);
    apply_task_worker_detail_visibility(&mut item, context.task, context.turn_visibility);
    item.tool_call_id = Some(tool_call.id.clone());
    item.tool_name = Some(tool_call.function.name.clone());
    item.tool_status = Some(status_label.to_string());
    item.tool_arguments = Some(tool_call.function.arguments.clone());
    item.tool_result = Some(tool_result.to_string());
    if matches!(
        tool_status,
        ExecutionResultStatus::Failed
            | ExecutionResultStatus::Rejected
            | ExecutionResultStatus::NeedsApproval
    ) {
        item.tool_error = Some(tool_result.to_string());
    }
    let Some(published) = upsert_session_turn_item_for_turn(
        context.session_store,
        context.session_id,
        context.expected_turn_id,
        item,
        Some(context.task_store),
    )?
    else {
        return Err(format!(
            "会话 {} 的当前 Turn 已不可写，无法保存工具 {} 的结果事实",
            context.session_id, tool_call.function.name
        ));
    };
    persist_session_state_checkpoint(context.persist_session_state, "task_turn_tool_result")?;
    publish_session_turn_item_event(
        context.event_bus,
        context.session_id,
        context.workspace_id,
        &published,
    );
    Ok(())
}

pub(super) fn upsert_task_tool_call_progress_turn_item(
    context: TaskTurnWritebackContext<'_>,
    tool_call: &ChatToolCall,
    progress: ToolExecutionProgress,
) -> Result<(), String> {
    if progress.tool_call_id.as_str() != tool_call.id {
        return Ok(());
    }
    let payload = serde_json::from_str::<serde_json::Value>(&progress.payload).ok();
    let status = payload
        .as_ref()
        .and_then(|payload| payload.get("status"))
        .and_then(|status| status.as_str());
    let progress_status = if status == Some("awaiting_approval") {
        "awaiting_approval"
    } else {
        "running"
    };
    let interaction_boundary = matches!(status, Some("awaiting_approval" | "awaiting_user_input"))
        || payload
            .as_ref()
            .is_some_and(|payload| payload.get("approval_id").is_some());
    let mut item = session_turn_item(
        "tool_call_started",
        progress_status,
        Some(tool_call.function.name.clone()),
        Some(summarize_tool_result(&progress.payload)),
        Some(task_tool_turn_item_id(context, tool_call)),
        context.turn_visibility.thread_id().clone(),
    );
    apply_step_identity(&mut item, context);
    apply_task_worker_detail_visibility(&mut item, context.task, context.turn_visibility);
    item.tool_call_id = Some(tool_call.id.clone());
    item.tool_name = Some(progress.tool_name);
    item.tool_status = Some(progress_status.to_string());
    item.tool_arguments = Some(tool_call.function.arguments.clone());
    item.tool_result = Some(progress.payload);
    let Some(published) = upsert_session_turn_item_for_turn(
        context.session_store,
        context.session_id,
        context.expected_turn_id,
        item,
        Some(context.task_store),
    )?
    else {
        return Err(format!(
            "会话 {} 的当前 Turn 已不可写，无法保存工具 {} 的进度事实",
            context.session_id, tool_call.function.name
        ));
    };
    if interaction_boundary {
        persist_session_state_checkpoint(context.persist_session_state, "task_turn_tool_progress")?;
    }
    publish_session_turn_item_event(
        context.event_bus,
        context.session_id,
        context.workspace_id,
        &published,
    );
    Ok(())
}

pub(super) fn tool_call_record(tool_call: &ChatToolCall, result: &str) -> serde_json::Value {
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

pub(super) fn append_task_final_turn_item(
    context: TaskTurnWritebackContext<'_>,
    final_content: &str,
    final_item_id: Option<&str>,
    timeline_entry_id: Option<&str>,
    model_round: Option<usize>,
    provider_context: &[ModelProviderContext],
    plugin_completions: &[magi_extension_host::protocol::PluginToolCompletion],
) -> Result<(), String> {
    let mut final_item = session_turn_item(
        "assistant_final",
        "completed",
        Some("最终回复".to_string()),
        Some(final_content.to_string()),
        final_item_id.map(str::to_string),
        context.turn_visibility.thread_id().clone(),
    );
    if let Some(model_round) = model_round {
        apply_model_response_round(&mut final_item, model_round);
    }
    if !plugin_completions.is_empty() {
        final_item.metadata.insert(
            "pluginToolCompletion".into(),
            serde_json::to_value(plugin_completions).expect("tool completions serialize"),
        );
    }
    // 最终回复沿用同一模型响应，不能在替换 stream item 时丢失签名、执行证据和 Step。
    apply_step_identity(&mut final_item, context);
    if !provider_context.is_empty() {
        final_item.metadata.insert(
            "providerContext".to_string(),
            serde_json::to_value(provider_context).expect("模型提供方上下文必须能够序列化"),
        );
    }
    apply_task_final_visibility(
        &mut final_item,
        context.task_store,
        context.task,
        context.turn_visibility,
    );
    if let Some(timeline_entry_id) = timeline_entry_id {
        final_item.timeline_entry_id = Some(timeline_entry_id.to_string());
    }
    let final_item_id = final_item.item_id.clone();
    let published = upsert_session_turn_item_for_turn(
        context.session_store,
        context.session_id,
        context.expected_turn_id,
        final_item,
        Some(context.task_store),
    )?;
    let Some(published) = published else {
        return Err(format!(
            "会话 {} 的当前 Turn 已不可写，无法保存最终回复 item {}",
            context.session_id, final_item_id
        ));
    };
    persist_session_state_checkpoint(context.persist_session_state, "task_turn_final_item")?;
    publish_session_turn_item_event(
        context.event_bus,
        context.session_id,
        context.workspace_id,
        &published,
    );
    let root_task_completed = context
        .task_store
        .get_task(&context.task.root_task_id)
        .is_some_and(|root_task| root_task.status == TaskStatus::Completed);
    if context.turn_visibility.is_mainline() && root_task_completed {
        CanonicalTurnEventSink::for_store(context.session_store, Some(context.task_store))
            .set_status_domain(context.session_id, context.expected_turn_id, "completed")
            .map_err(|error| {
                format!(
                    "提交会话 {} 的 Turn completed 状态失败: {error}",
                    context.session_id
                )
            })?
            .ok_or_else(|| {
                format!(
                    "会话 {} 没有可更新的当前 Turn，无法提交 completed 状态",
                    context.session_id
                )
            })?;
        persist_session_state_checkpoint(context.persist_session_state, "task_turn_completed")?;
        publish_current_session_turn_item_event(
            context.event_bus,
            context.session_store,
            context.session_id,
            context.workspace_id,
            &final_item_id,
            Some(context.task_store),
        )?;
    }
    Ok(())
}

pub(super) fn append_task_error_turn_item(
    context: TaskTurnWritebackContext<'_>,
    error_text: &str,
    _streaming_entry_id: Option<&str>,
    model_failure: Option<&ModelFailureDiagnostic>,
    tool_call_failure: Option<&ToolCallFailureDiagnostic>,
) -> Result<(), String> {
    let mut error_item = session_turn_item(
        "assistant_error",
        "failed",
        Some("回复生成失败".to_string()),
        Some(error_text.to_string()),
        Some(format!(
            "turn-item-assistant-error-{}",
            context.task.task_id
        )),
        context.turn_visibility.thread_id().clone(),
    );
    apply_task_turn_visibility(&mut error_item, context.task, context.turn_visibility);
    if let Some(model_failure) = model_failure {
        error_item.metadata.insert(
            "modelFailure".to_string(),
            serde_json::to_value(model_failure).expect("model failure diagnostic must serialize"),
        );
    }
    if let Some(tool_call_failure) = tool_call_failure {
        error_item.metadata.insert(
            "toolCallFailure".to_string(),
            serde_json::to_value(tool_call_failure)
                .expect("tool call failure diagnostic must serialize"),
        );
    }
    let error_item_id = error_item.item_id.clone();
    let published = upsert_session_turn_item_for_turn(
        context.session_store,
        context.session_id,
        context.expected_turn_id,
        error_item,
        Some(context.task_store),
    )?
    .ok_or_else(|| {
        format!(
            "会话 {} 没有可写入的当前 Turn，无法记录失败 item {}",
            context.session_id, error_item_id
        )
    })?;
    if context.turn_visibility.is_mainline() {
        // Mainline Task 的失败 item 只是 Task 执行事实，不能在 TaskStore durable
        // 终态提交之前把它发布成 Turn 终态。否则消费者会看到 failed snapshot，
        // 但 TaskStore/Coordinator 仍未 settlement，下一轮会在短窗口内得到 409。
        // TaskCompletionNotifier -> finalizer 会在 TaskStore Failed 提交后补齐
        // Coordinator 收口、资源 settlement 和唯一 terminal event；这里仅持久化
        // 可恢复的错误 item。Sidechain item 不拥有 root Turn 终态，仍可即时展示。
        persist_session_state_checkpoint(context.persist_session_state, "task_turn_error_item")?;
        return Ok(());
    }
    publish_session_turn_item_event(
        context.event_bus,
        context.session_id,
        context.workspace_id,
        &published,
    );
    Ok(())
}

pub(super) fn build_output_content(
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

pub(super) fn task_stream_item_id(
    task_id: &TaskId,
    round: usize,
    streaming_entry_id: Option<&str>,
) -> String {
    if round == 0 {
        return streaming_entry_id
            .map(str::to_string)
            .unwrap_or_else(|| format!("turn-item-assistant-stream-{task_id}-{round}"));
    }
    format!("turn-item-assistant-stream-{task_id}-{round}")
}
