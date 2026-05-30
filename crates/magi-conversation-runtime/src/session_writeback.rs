use crate::tool_declared_paths::{append_result_declared_paths, derive_declared_paths};
use crate::tool_result_utils::{
    summarize_tool_result, tool_execution_status_label, turn_item_status_for_tool_result,
};
use crate::{
    SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime, execute_skill_custom_tool,
    internal_builtin_tool_rejection_payload, parse_skill_custom_tool_name,
    tool_batch::{
        access_profile_tool_decision, safety_gate_tool_decision, select_preflight_decision,
    },
};
use magi_bridge_client::{
    ChatMessage, ChatToolCall,
    tool_concurrency::{ToolBatchKind, ToolConcurrencyInput, partition_tool_calls_with_inputs},
};
use magi_core::{
    EventId, ExecutionResultStatus, SessionId, TaskId, ThreadId, ToolCallId, UtcMillis, WorkspaceId,
};
use magi_event_bus::{
    EventContext, EventEnvelope, InMemoryEventBus, SessionRuntimeTurnItemSummaryEntry,
    SessionRuntimeTurnSummaryEntry,
};
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::{
    ActiveExecutionChain, ActiveExecutionTurn, ActiveExecutionTurnItem,
    CANONICAL_TURN_SCHEMA_VERSION, CanonicalToolCall, CanonicalTurn, CanonicalTurnEventKind,
    CanonicalTurnItem, CanonicalTurnItemKind, CanonicalTurnItemStatus, CanonicalTurnStatus,
    CanonicalTurnVisibility, CanonicalWorkerRef, SessionRuntimeSidecar, SessionStore,
};
use magi_skill_runtime::{SkillDispatchRuntime, SkillRuntime};
use magi_snapshot::{SnapshotSession, ToolHook, ToolHookCtx};
use magi_tool_runtime::{
    BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
};
use serde_json::Value;
use std::{collections::HashMap, path::PathBuf, sync::Arc, thread};

pub type SessionStatePersistCallback = dyn Fn(&str) + Send + Sync;

pub fn persist_session_state_checkpoint(
    callback: Option<&SessionStatePersistCallback>,
    checkpoint: &'static str,
) {
    if let Some(callback) = callback {
        callback(checkpoint);
    }
}

#[derive(Clone, Debug)]
pub struct PublishedSessionTurnItem {
    pub turn_id: String,
    pub turn_seq: u64,
    pub item: ActiveExecutionTurnItem,
    pub current_turn: SessionRuntimeTurnSummaryEntry,
    pub turn_items: Vec<SessionRuntimeTurnItemSummaryEntry>,
    pub canonical_turn: Option<CanonicalTurn>,
    pub canonical_item: Option<CanonicalTurnItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTurnStreamUpdate {
    pub delta: String,
    pub content_length: usize,
    pub reset: bool,
}

pub fn session_turn_stream_update(
    previous_content: &str,
    current_content: &str,
) -> Option<SessionTurnStreamUpdate> {
    if previous_content == current_content {
        return None;
    }
    let (delta, reset) = current_content
        .strip_prefix(previous_content)
        .map(|delta| (delta.to_string(), false))
        .unwrap_or_else(|| (current_content.to_string(), true));
    Some(SessionTurnStreamUpdate {
        delta,
        content_length: current_content.chars().count(),
        reset,
    })
}

fn published_session_turn_item_from_sidecar(
    sidecar: SessionRuntimeSidecar,
    item_id: &str,
    task_store: Option<&TaskStore>,
) -> Option<PublishedSessionTurnItem> {
    let turn = sidecar.current_turn.as_ref()?;
    let item = turn
        .items
        .iter()
        .find(|candidate| candidate.item_id == item_id)?
        .clone();
    let chain = chain_for_turn_summary(&sidecar, turn);
    let response_duration_ms = turn
        .completed_at
        .map(|completed_at| completed_at.0.saturating_sub(turn.accepted_at.0));
    let canonical_turn = to_canonical_turn(&sidecar.session_id, turn);
    let canonical_item = to_canonical_turn_item(&sidecar.session_id, turn, &item);
    Some(PublishedSessionTurnItem {
        turn_id: turn.turn_id.clone(),
        turn_seq: turn.turn_seq,
        item,
        current_turn: SessionRuntimeTurnSummaryEntry {
            turn_id: turn.turn_id.clone(),
            turn_seq: turn.turn_seq,
            accepted_at: Some(turn.accepted_at),
            completed_at: turn.completed_at,
            response_duration_ms,
            status: turn.status.clone(),
            user_message: turn.user_message.clone(),
            mission_id: chain.map(|chain| chain.mission_id.to_string()),
            root_task_id: chain.map(|chain| chain.root_task_id.to_string()),
            execution_chain_ref: chain.map(|chain| chain.execution_chain_ref.clone()),
        },
        turn_items: turn
            .items
            .iter()
            .map(|item| to_turn_item_summary(item, task_store))
            .collect(),
        canonical_turn,
        canonical_item,
    })
}

fn chain_for_turn_summary<'a>(
    sidecar: &'a SessionRuntimeSidecar,
    turn: &ActiveExecutionTurn,
) -> Option<&'a ActiveExecutionChain> {
    if turn_has_execution_chain_items(turn) {
        sidecar.active_execution_chain.as_ref()
    } else {
        None
    }
}

fn turn_has_execution_chain_items(turn: &ActiveExecutionTurn) -> bool {
    turn.items
        .iter()
        .any(|item| item.task_id.is_some() || item.worker_id.is_some())
}

fn canonical_turn_status(status: &str) -> Option<CanonicalTurnStatus> {
    match status.trim().to_ascii_lowercase().as_str() {
        "pending" | "queued" | "accepted" => Some(CanonicalTurnStatus::Pending),
        "running" | "started" | "streaming" | "awaiting_approval" | "review_required"
        | "repairing" | "verifying" => Some(CanonicalTurnStatus::Running),
        "completed" | "complete" | "succeeded" | "success" => Some(CanonicalTurnStatus::Completed),
        "blocked" => Some(CanonicalTurnStatus::Blocked),
        "failed" | "error" => Some(CanonicalTurnStatus::Failed),
        "cancelled" | "canceled" => Some(CanonicalTurnStatus::Cancelled),
        _ => None,
    }
}

fn canonical_item_status(status: &str) -> Option<CanonicalTurnItemStatus> {
    match canonical_turn_status(status)? {
        CanonicalTurnStatus::Pending => Some(CanonicalTurnItemStatus::Pending),
        CanonicalTurnStatus::Running => Some(CanonicalTurnItemStatus::Running),
        CanonicalTurnStatus::Completed => Some(CanonicalTurnItemStatus::Completed),
        CanonicalTurnStatus::Blocked => Some(CanonicalTurnItemStatus::Blocked),
        CanonicalTurnStatus::Failed => Some(CanonicalTurnItemStatus::Failed),
        CanonicalTurnStatus::Cancelled => Some(CanonicalTurnItemStatus::Cancelled),
    }
}

fn terminal_item_status_for_turn_status(
    status: CanonicalTurnStatus,
) -> Option<CanonicalTurnItemStatus> {
    match status {
        CanonicalTurnStatus::Completed => Some(CanonicalTurnItemStatus::Completed),
        CanonicalTurnStatus::Blocked => Some(CanonicalTurnItemStatus::Blocked),
        CanonicalTurnStatus::Failed => Some(CanonicalTurnItemStatus::Failed),
        CanonicalTurnStatus::Cancelled => Some(CanonicalTurnItemStatus::Cancelled),
        CanonicalTurnStatus::Pending | CanonicalTurnStatus::Running => None,
    }
}

fn canonical_item_kind(kind: &str) -> Option<CanonicalTurnItemKind> {
    match kind {
        "user_message" => Some(CanonicalTurnItemKind::UserMessage),
        "assistant_stream" | "assistant_final" | "assistant_error" => {
            Some(CanonicalTurnItemKind::AssistantText)
        }
        "assistant_thinking" => Some(CanonicalTurnItemKind::AssistantThinking),
        "assistant_phase" => Some(CanonicalTurnItemKind::SystemNotice),
        "tool_call_started" | "tool_call_result" => Some(CanonicalTurnItemKind::ToolCall),
        "task_status" => Some(CanonicalTurnItemKind::TaskStatus),
        _ => None,
    }
}

fn canonical_tool_arguments(arguments: &Option<String>) -> Option<Value> {
    let arguments = arguments.as_ref()?.trim();
    if arguments.is_empty() {
        return None;
    }
    serde_json::from_str(arguments)
        .ok()
        .or_else(|| Some(Value::String(arguments.to_string())))
}

fn canonical_tool_result(result: &Option<String>) -> Option<Value> {
    let result = result.as_ref()?.trim();
    if result.is_empty() {
        return None;
    }
    serde_json::from_str(result)
        .ok()
        .or_else(|| Some(Value::String(result.to_string())))
}

fn to_canonical_tool_call(item: &ActiveExecutionTurnItem) -> Option<CanonicalToolCall> {
    let call_id = item.tool_call_id.clone()?;
    let name = item.tool_name.clone()?;
    Some(CanonicalToolCall {
        call_id,
        name,
        arguments: canonical_tool_arguments(&item.tool_arguments),
        result: canonical_tool_result(&item.tool_result),
        error: item.tool_error.clone(),
    })
}

fn to_canonical_worker_ref(item: &ActiveExecutionTurnItem) -> Option<CanonicalWorkerRef> {
    if item.task_id.is_none() && item.worker_id.is_none() && item.role_id.is_none() {
        return None;
    }
    Some(CanonicalWorkerRef {
        task_id: item.task_id.clone(),
        worker_id: item.worker_id.clone(),
        role_id: item.role_id.clone(),
        title: item.title.clone(),
    })
}

fn canonical_item_metadata(item: &ActiveExecutionTurnItem) -> HashMap<String, Value> {
    let mut metadata = HashMap::new();
    if let Some(value) = item
        .request_id
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        metadata.insert("requestId".to_string(), Value::String(value.clone()));
    }
    if let Some(value) = item
        .user_message_id
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        metadata.insert("userMessageId".to_string(), Value::String(value.clone()));
    }
    if let Some(value) = item
        .placeholder_message_id
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        metadata.insert(
            "placeholderMessageId".to_string(),
            Value::String(value.clone()),
        );
    }
    metadata
}

fn canonical_item_renderable(
    item: &ActiveExecutionTurnItem,
    kind: CanonicalTurnItemKind,
    status: CanonicalTurnItemStatus,
) -> bool {
    if kind == CanonicalTurnItemKind::ToolCall
        && item
            .tool_name
            .as_deref()
            .and_then(BuiltinToolName::from_str)
            .is_some_and(|tool| tool.is_runtime_internal_tool_call())
    {
        return false;
    }
    let has_content = item
        .content
        .as_ref()
        .is_some_and(|content| !content.trim().is_empty());
    if kind == CanonicalTurnItemKind::AssistantText {
        return has_content || !status.is_terminal();
    }
    has_content || item.tool_call_id.is_some() || item.worker_id.is_some() || item.task_id.is_some()
}

fn to_canonical_turn_item(
    session_id: &SessionId,
    turn: &ActiveExecutionTurn,
    item: &ActiveExecutionTurnItem,
) -> Option<CanonicalTurnItem> {
    let kind = canonical_item_kind(&item.kind)?;
    let turn_status = canonical_turn_status(&turn.status)?;
    let mut status = canonical_item_status(&item.status)?;
    if let Some(terminal_item_status) = terminal_item_status_for_turn_status(turn_status)
        && !status.is_terminal()
    {
        status = terminal_item_status;
    }
    let tool = to_canonical_tool_call(item);
    if kind == CanonicalTurnItemKind::ToolCall && tool.is_none() {
        return None;
    }
    Some(CanonicalTurnItem {
        session_id: session_id.clone(),
        turn_id: turn.turn_id.clone(),
        turn_seq: turn.turn_seq,
        item_id: item.item_id.clone(),
        item_seq: item.item_seq,
        kind,
        created_at: turn.accepted_at,
        status,
        item_version: None,
        updated_at: UtcMillis::now(),
        title: item.title.clone(),
        content: item.content.clone(),
        blocks: Vec::new(),
        tool,
        worker: to_canonical_worker_ref(item),
        source_thread_id: item.source_thread_id.clone(),
        visibility: CanonicalTurnVisibility {
            renderable: canonical_item_renderable(item, kind, status),
        },
        metadata: canonical_item_metadata(item),
    })
}

fn canonical_event_kind(turn: Option<&CanonicalTurn>) -> CanonicalTurnEventKind {
    match turn.map(|turn| turn.status) {
        Some(status) if status.is_terminal() => CanonicalTurnEventKind::TurnCompleted,
        Some(CanonicalTurnStatus::Pending) => CanonicalTurnEventKind::TurnStarted,
        _ => CanonicalTurnEventKind::TurnItemUpsert,
    }
}

fn to_canonical_turn(session_id: &SessionId, turn: &ActiveExecutionTurn) -> Option<CanonicalTurn> {
    let items = turn
        .items
        .iter()
        .filter_map(|item| to_canonical_turn_item(session_id, turn, item))
        .collect::<Vec<_>>();
    let mut canonical_turn = CanonicalTurn {
        session_id: session_id.clone(),
        turn_id: turn.turn_id.clone(),
        turn_seq: turn.turn_seq,
        accepted_at: turn.accepted_at,
        completed_at: turn.completed_at,
        status: canonical_turn_status(&turn.status)?,
        response_duration_ms: turn
            .completed_at
            .map(|completed_at| completed_at.0.saturating_sub(turn.accepted_at.0)),
        usage: None,
        items,
        metadata: HashMap::new(),
    };
    canonical_turn.normalize();
    Some(canonical_turn)
}

pub fn session_turn_item(
    kind: &str,
    status: &str,
    title: Option<String>,
    content: Option<String>,
    item_id: Option<String>,
    source_thread_id: ThreadId,
) -> ActiveExecutionTurnItem {
    ActiveExecutionTurnItem {
        item_id: item_id.unwrap_or_else(|| format!("turn-item-{}-{}", kind, UtcMillis::now().0)),
        item_seq: 0,
        kind: kind.to_string(),
        status: status.to_string(),
        source: "orchestrator".to_string(),
        title,
        content,
        task_id: None,
        worker_id: None,
        role_id: None,
        tool_call_id: None,
        tool_name: None,
        tool_status: None,
        tool_arguments: None,
        tool_result: None,
        tool_error: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        timeline_entry_id: None,
        source_thread_id,
    }
}

pub fn append_session_turn_item(
    session_store: &SessionStore,
    session_id: &SessionId,
    item: ActiveExecutionTurnItem,
) -> Option<PublishedSessionTurnItem> {
    append_session_turn_item_with_task_store(session_store, session_id, item, None)
}

pub fn append_session_turn_item_with_task_store(
    session_store: &SessionStore,
    session_id: &SessionId,
    item: ActiveExecutionTurnItem,
    task_store: Option<&TaskStore>,
) -> Option<PublishedSessionTurnItem> {
    let item_id = item.item_id.clone();
    let sidecar = session_store
        .append_current_turn_item(session_id, item)
        .ok()
        .flatten()?;
    published_session_turn_item_from_sidecar(sidecar, &item_id, task_store)
}

pub fn upsert_session_turn_item(
    session_store: &SessionStore,
    session_id: &SessionId,
    item: ActiveExecutionTurnItem,
) -> Option<PublishedSessionTurnItem> {
    upsert_session_turn_item_with_task_store(session_store, session_id, item, None)
}

pub fn upsert_session_turn_item_with_task_store(
    session_store: &SessionStore,
    session_id: &SessionId,
    item: ActiveExecutionTurnItem,
    task_store: Option<&TaskStore>,
) -> Option<PublishedSessionTurnItem> {
    let item_id = item.item_id.clone();
    let sidecar = session_store
        .upsert_current_turn_item(session_id, item)
        .ok()
        .flatten()?;
    published_session_turn_item_from_sidecar(sidecar, &item_id, task_store)
}

pub fn publish_session_turn_item_event(
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    published: &PublishedSessionTurnItem,
) {
    publish_session_turn_item_event_with_stream_update(
        event_bus,
        session_id,
        workspace_id,
        published,
        None,
    );
}

pub fn publish_session_turn_item_event_with_stream_update(
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    published: &PublishedSessionTurnItem,
    stream_update: Option<&SessionTurnStreamUpdate>,
) {
    let mut payload = serde_json::json!({
        "session_id": session_id.to_string(),
        "workspace_id": workspace_id.as_ref().map(ToString::to_string),
        "turn_id": published.turn_id,
        "turn_seq": published.turn_seq,
        "item": published.item,
        "current_turn": published.current_turn,
        "turn_items": published.turn_items,
        "canonical_schema_version": CANONICAL_TURN_SCHEMA_VERSION,
        "canonical_event_kind": canonical_event_kind(published.canonical_turn.as_ref()),
        "canonical_turn": published.canonical_turn,
        "canonical_item": published.canonical_item,
    });
    if let Some(stream_update) = stream_update
        && let Some(object) = payload.as_object_mut()
    {
        object.insert(
            "stream_delta".to_string(),
            Value::String(stream_update.delta.clone()),
        );
        object.insert(
            "stream_content_length".to_string(),
            Value::from(stream_update.content_length as u64),
        );
        object.insert("stream_reset".to_string(), Value::Bool(stream_update.reset));
    }

    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-session-turn-item-{}", UtcMillis::now().0)),
            "session.turn.item",
            payload,
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            ..EventContext::default()
        }),
    );
}

pub fn publish_current_session_turn_item_event(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    item_id: &str,
    task_store: Option<&TaskStore>,
) {
    let Some(sidecar) = session_store.runtime_sidecar(session_id) else {
        return;
    };
    let Some(published) = published_session_turn_item_from_sidecar(sidecar, item_id, task_store)
    else {
        return;
    };
    publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
}

pub fn append_session_turn_error_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    task_id: Option<&TaskId>,
    request_id: Option<&str>,
    user_message_id: Option<&str>,
    placeholder_message_id: Option<&str>,
    error_text: &str,
    _streaming_entry_id: Option<&str>,
    source_thread_id: ThreadId,
    persist_session_state: Option<&SessionStatePersistCallback>,
) {
    let mut error_item = session_turn_item(
        "assistant_error",
        "failed",
        Some("回复生成失败".to_string()),
        Some(error_text.to_string()),
        Some(format!("turn-item-assistant-error-{}", UtcMillis::now().0)),
        source_thread_id,
    );
    let error_item_id = error_item.item_id.clone();
    if let Some(task_id) = task_id {
        error_item.task_id = Some(task_id.clone());
    }
    error_item.request_id = request_id.map(str::to_string);
    error_item.user_message_id = user_message_id.map(str::to_string);
    error_item.placeholder_message_id = placeholder_message_id.map(str::to_string);
    let _ = append_session_turn_item(session_store, session_id, error_item);
    let _ = session_store.update_current_turn_status(session_id, "failed");
    persist_session_state_checkpoint(persist_session_state, "session_turn_failed");
    publish_current_session_turn_item_event(
        event_bus,
        session_store,
        session_id,
        workspace_id,
        &error_item_id,
        None,
    );
}

fn task_role_id(task_store: Option<&TaskStore>, task_id: &TaskId) -> Option<String> {
    task_store
        .and_then(|store| store.get_task(task_id))
        .and_then(|task| task.executor_binding_target_role().map(str::to_string))
}

fn to_turn_item_summary(
    item: &ActiveExecutionTurnItem,
    task_store: Option<&TaskStore>,
) -> SessionRuntimeTurnItemSummaryEntry {
    let role_id = item.role_id.clone().or_else(|| {
        item.task_id
            .as_ref()
            .and_then(|task_id| task_role_id(task_store, task_id))
    });
    SessionRuntimeTurnItemSummaryEntry {
        item_id: item.item_id.clone(),
        item_seq: item.item_seq,
        kind: item.kind.clone(),
        status: item.status.clone(),
        source: item.source.clone(),
        title: item.title.clone(),
        content: item.content.clone(),
        task_id: item.task_id.as_ref().map(ToString::to_string),
        worker_id: item.worker_id.as_ref().map(ToString::to_string),
        role_id,
        tool_call_id: item.tool_call_id.clone(),
        tool_name: item.tool_name.clone(),
        tool_status: item.tool_status.clone(),
        tool_arguments: item.tool_arguments.clone(),
        tool_result: item.tool_result.clone(),
        tool_error: item.tool_error.clone(),
        request_id: item.request_id.clone(),
        user_message_id: item.user_message_id.clone(),
        placeholder_message_id: item.placeholder_message_id.clone(),
        timeline_entry_id: item.timeline_entry_id.clone(),
        source_thread_id: item.source_thread_id.to_string(),
    }
}

pub fn append_session_tool_call_items_batch(
    session_store: &SessionStore,
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&SkillRuntime>,
    skill_dispatch_runtime: Option<&SkillDispatchRuntime>,
    skill_name: Option<&str>,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    workspace_root_path: Option<PathBuf>,
    access_profile: magi_core::AccessProfile,
    tool_calls: &[ChatToolCall],
    messages: &mut Vec<ChatMessage>,
    snapshot_session: Option<Arc<SnapshotSession>>,
    execution_group_id: Option<String>,
    source_thread_id: &ThreadId,
    persist_session_state: Option<&SessionStatePersistCallback>,
    write_allowed: impl Fn() -> bool,
) -> bool {
    for tool_call in tool_calls {
        if !write_allowed() {
            return false;
        }
        let mut started_item = session_turn_item(
            "tool_call_started",
            "running",
            Some(tool_call.function.name.clone()),
            Some(format!("正在调用工具：{}", tool_call.function.name)),
            Some(format!("turn-item-tool-{}", tool_call.id)),
            source_thread_id.clone(),
        );
        started_item.source = "tool".to_string();
        started_item.tool_call_id = Some(tool_call.id.clone());
        started_item.tool_name = Some(tool_call.function.name.clone());
        started_item.tool_status = Some("running".to_string());
        started_item.tool_arguments = Some(tool_call.function.arguments.clone());
        if let Some(published) = upsert_session_turn_item(session_store, session_id, started_item) {
            publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
        }
    }

    let hook_contexts: Vec<ToolHookCtx> = tool_calls
        .iter()
        .map(|tool_call| ToolHookCtx {
            tool_call_id: tool_call.id.clone(),
            worker_id: None,
            execution_group_id: execution_group_id.clone(),
            declared_paths: derive_declared_paths(tool_call),
        })
        .collect();

    let tool_results = execute_session_turn_tool_call_batch(
        event_bus,
        tool_registry,
        skill_runtime,
        skill_dispatch_runtime,
        skill_name,
        safety_gate,
        tool_calls,
        session_id,
        workspace_id,
        workspace_root_path.as_ref(),
        access_profile,
        snapshot_session.as_ref(),
        &hook_contexts,
    );

    if let Some(snapshot) = snapshot_session.as_deref()
        && let Err(err) = snapshot.reconcile()
    {
        tracing::warn!(
            session_id = %session_id.as_str(),
            error = %err,
            "snapshot reconcile after tool batch failed"
        );
    }

    for (tool_call, (tool_result, tool_status)) in tool_calls.iter().zip(tool_results.into_iter()) {
        if !write_allowed() {
            return false;
        }
        upsert_session_tool_call_result_item(
            session_store,
            event_bus,
            session_id,
            workspace_id,
            tool_call,
            &tool_result,
            tool_status,
            source_thread_id,
            persist_session_state,
        );
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(tool_result),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call.id.clone()),
        });
    }
    true
}

fn upsert_session_tool_call_result_item(
    session_store: &SessionStore,
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    tool_call: &ChatToolCall,
    tool_result: &str,
    tool_status: ExecutionResultStatus,
    source_thread_id: &ThreadId,
    persist_session_state: Option<&SessionStatePersistCallback>,
) {
    let status_label = tool_execution_status_label(tool_status);
    let mut result_item = session_turn_item(
        "tool_call_result",
        turn_item_status_for_tool_result(tool_status),
        Some(tool_call.function.name.clone()),
        Some(summarize_tool_result(tool_result)),
        Some(format!("turn-item-tool-{}", tool_call.id)),
        source_thread_id.clone(),
    );
    result_item.source = "tool".to_string();
    result_item.tool_call_id = Some(tool_call.id.clone());
    result_item.tool_name = Some(tool_call.function.name.clone());
    result_item.tool_status = Some(status_label.to_string());
    result_item.tool_arguments = Some(tool_call.function.arguments.clone());
    result_item.tool_result = Some(tool_result.to_string());
    if !matches!(tool_status, ExecutionResultStatus::Succeeded) {
        result_item.tool_error = Some(tool_result.to_string());
    }
    if let Some(published) = upsert_session_turn_item(session_store, session_id, result_item) {
        persist_session_state_checkpoint(persist_session_state, "session_turn_tool_result");
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
}

fn execute_session_turn_tool_call_batch(
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&SkillRuntime>,
    skill_dispatch_runtime: Option<&SkillDispatchRuntime>,
    skill_name: Option<&str>,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    tool_calls: &[ChatToolCall],
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    workspace_root_path: Option<&PathBuf>,
    access_profile: magi_core::AccessProfile,
    snapshot_session: Option<&Arc<SnapshotSession>>,
    hook_contexts: &[ToolHookCtx],
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
                    let mut hook_ctx = hook_contexts[tool_index].clone();
                    if let Some(snapshot) = snapshot_session {
                        snapshot.before_tool(&hook_ctx);
                    }
                    let result = execute_session_turn_tool_call(
                        event_bus,
                        tool_registry,
                        skill_runtime,
                        skill_dispatch_runtime,
                        skill_name,
                        safety_gate,
                        &tool_calls[tool_index],
                        session_id,
                        workspace_id,
                        workspace_root_path,
                        access_profile,
                    );
                    append_result_declared_paths(&mut hook_ctx.declared_paths, &result.0);
                    if let Some(snapshot) = snapshot_session {
                        snapshot.after_tool(&hook_ctx);
                    }
                    results[tool_index] = Some(result);
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
                            let mut hook_ctx = hook_contexts[tool_index].clone();
                            let snapshot_session = snapshot_session.cloned();
                            (
                                tool_index,
                                scope.spawn(move || {
                                    let result = execute_session_turn_tool_call(
                                        event_bus,
                                        tool_registry,
                                        skill_runtime,
                                        skill_dispatch_runtime,
                                        skill_name,
                                        safety_gate,
                                        tool_call,
                                        session_id,
                                        workspace_id,
                                        workspace_root_path,
                                        access_profile,
                                    );
                                    append_result_declared_paths(
                                        &mut hook_ctx.declared_paths,
                                        &result.0,
                                    );
                                    if let Some(snapshot) = snapshot_session.as_deref() {
                                        snapshot.after_tool(&hook_ctx);
                                    }
                                    result
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
                                    "error": "工具执行线程异常"
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
                        "error": "工具执行结果缺失"
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                )
            })
        })
        .collect()
}

fn execute_session_turn_tool_call(
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&SkillRuntime>,
    skill_dispatch_runtime: Option<&SkillDispatchRuntime>,
    skill_name: Option<&str>,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    tool_call: &ChatToolCall,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    workspace_root_path: Option<&PathBuf>,
    access_profile: magi_core::AccessProfile,
) -> (String, ExecutionResultStatus) {
    let Some(registry) = tool_registry else {
        return (
            serde_json::json!({ "error": "tool registry not available" }).to_string(),
            ExecutionResultStatus::Failed,
        );
    };

    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-session-turn-tool-{}", UtcMillis::now().0)),
            "session.turn.tool.invoked",
            serde_json::json!({
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "tool_name": tool_call.function.name,
                "tool_call_id": tool_call.id,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            ..EventContext::default()
        }),
    );

    if tool_call.function.name == SKILL_APPLY_TOOL_NAME {
        return execute_skill_apply_from_runtime(&tool_call.function.arguments, skill_runtime);
    }

    if let Some((tool_skill_name, binding_id)) =
        parse_skill_custom_tool_name(&tool_call.function.name)
    {
        return execute_skill_custom_tool(
            tool_call,
            &tool_skill_name,
            &binding_id,
            skill_name,
            skill_runtime,
            skill_dispatch_runtime,
            ToolExecutionContext {
                worker_id: None,
                task_id: None,
                session_id: Some(session_id.clone()),
                workspace_id: workspace_id.clone(),
                working_directory: workspace_root_path.cloned(),
            },
            workspace_root_path
                .as_ref()
                .map(|path| path.display().to_string()),
        );
    }

    if let Some(rejection) = internal_builtin_tool_rejection_payload(&tool_call.function.name) {
        return (rejection, ExecutionResultStatus::Failed);
    }

    let access_profile_decision = access_profile_tool_decision(
        access_profile,
        "",
        &[],
        &[],
        &tool_call.function.name,
        &tool_call.function.arguments,
    );
    let safety_gate_decision = safety_gate.and_then(|gate| {
        safety_gate_tool_decision(
            gate,
            access_profile,
            &tool_call.function.name,
            &tool_call.function.arguments,
        )
    });
    if let Some(decision) = select_preflight_decision(access_profile_decision, safety_gate_decision)
    {
        return (decision.payload, decision.status);
    }

    let output = registry.execute_with_policy(
        ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new(&tool_call.id),
            &tool_call.function.name,
            tool_call.function.arguments.clone(),
        ),
        ToolExecutionContext {
            worker_id: None,
            task_id: None,
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.clone(),
            working_directory: workspace_root_path.cloned(),
        },
        &ToolExecutionPolicy {
            access_profile,
            ..ToolExecutionPolicy::default()
        },
    );
    (output.payload, output.status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::{
        BridgeBindingKind, BridgeDispatchAction, BridgeDispatchRuntime, BridgeResponse,
        ChatToolFunction, McpBridgeClient, McpToolCallRequest,
    };
    use magi_core::{ApprovalRequirement, MissionId, RiskLevel, ThreadId};
    use magi_governance::GovernanceService;
    use magi_session_store::{
        ActiveExecutionChain, ActiveExecutionDispatchContext, ActiveExecutionTurn,
    };
    use magi_skill_runtime::{
        SkillDefinition, SkillDispatchRuntime, SkillMetadata, SkillRegistry, SkillRuntime,
    };
    use magi_tool_runtime::{BuiltinTool, BuiltinToolSpec};
    use std::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };

    struct ConcurrentToolProbe {
        active: AtomicUsize,
        max_active: AtomicUsize,
        delay: Duration,
    }

    #[test]
    fn stream_update_reports_append_delta_and_reset() {
        let appended =
            session_turn_stream_update("你好", "你好，世界").expect("append update should exist");
        assert_eq!(appended.delta, "，世界");
        assert_eq!(appended.content_length, 5);
        assert!(!appended.reset);

        let reset = session_turn_stream_update("旧内容", "新内容").expect("reset should exist");
        assert_eq!(reset.delta, "新内容");
        assert_eq!(reset.content_length, 3);
        assert!(reset.reset);

        assert!(session_turn_stream_update("same", "same").is_none());
    }

    impl ConcurrentToolProbe {
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

    struct ProbeBuiltinTool {
        name: &'static str,
        probe: Arc<ConcurrentToolProbe>,
    }

    #[derive(Clone, Default)]
    struct RecordingMcpClient {
        calls: Arc<Mutex<Vec<McpToolCallRequest>>>,
    }

    impl ProbeBuiltinTool {
        fn new(name: &'static str, probe: Arc<ConcurrentToolProbe>) -> Self {
            Self { name, probe }
        }
    }

    impl BuiltinTool for ProbeBuiltinTool {
        fn name(&self) -> &'static str {
            self.name
        }

        fn execute(
            &self,
            input: &str,
            _context: &ToolExecutionContext,
            _resources: &magi_tool_runtime::ToolRuntimeResources,
        ) -> String {
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

    impl McpBridgeClient for RecordingMcpClient {
        fn call_tool(
            &self,
            request: McpToolCallRequest,
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            self.calls
                .lock()
                .expect("recording mcp calls lock poisoned")
                .push(request.clone());
            Ok(BridgeResponse {
                ok: true,
                payload: serde_json::json!({
                    "tool": request.tool_name,
                    "server": request.server_name,
                    "input": request.input,
                    "ok": true,
                })
                .to_string(),
            })
        }
    }

    #[test]
    fn session_turn_item_uses_expected_defaults() {
        let orchestrator_thread = magi_core::ThreadId::new("thread-test-orchestrator-defaults");
        let item = session_turn_item(
            "assistant_phase",
            "running",
            Some("理解请求".to_string()),
            Some("准备中".to_string()),
            None,
            orchestrator_thread.clone(),
        );

        assert!(item.item_id.starts_with("turn-item-assistant_phase-"));
        assert_eq!(item.kind, "assistant_phase");
        assert_eq!(item.status, "running");
        assert_eq!(item.source, "orchestrator");
        assert_eq!(item.source_thread_id, orchestrator_thread);
    }

    #[test]
    fn canonical_turn_hides_agent_wait_tool_call() {
        let session_id = SessionId::new("session-agent-wait-hidden");
        let thread_id = ThreadId::new("thread-agent-wait-hidden");
        let now = UtcMillis::now();
        let mut item = session_turn_item(
            "tool_call_result",
            "completed",
            Some("agent_wait".to_string()),
            Some("{\"status\":\"succeeded\"}".to_string()),
            Some("turn-item-agent-wait".to_string()),
            thread_id,
        );
        item.tool_call_id = Some("tool-call-agent-wait".to_string());
        item.tool_name = Some("agent_wait".to_string());
        item.tool_arguments = Some("{\"task_ids\":[\"task-1\"]}".to_string());
        item.tool_result = Some("{\"status\":\"succeeded\"}".to_string());
        let turn = ActiveExecutionTurn {
            turn_id: "turn-agent-wait-hidden".to_string(),
            turn_seq: 1,
            accepted_at: now,
            completed_at: None,
            status: "running".to_string(),
            user_message: None,
            items: vec![item.clone()],
        };

        let canonical = to_canonical_turn_item(&session_id, &turn, &item)
            .expect("agent_wait should still be kept in canonical audit log");

        assert!(
            !canonical.visibility.renderable,
            "agent_wait 是编排协议回执，不能进入用户可见时间线"
        );
    }

    #[test]
    fn canonical_turn_keeps_agent_spawn_tool_call_renderable() {
        let session_id = SessionId::new("session-agent-spawn-visible");
        let thread_id = ThreadId::new("thread-agent-spawn-visible");
        let now = UtcMillis::now();
        let mut item = session_turn_item(
            "tool_call_result",
            "completed",
            Some("agent_spawn".to_string()),
            Some("{\"status\":\"started\"}".to_string()),
            Some("turn-item-agent-spawn".to_string()),
            thread_id,
        );
        item.tool_call_id = Some("tool-call-agent-spawn".to_string());
        item.tool_name = Some("agent_spawn".to_string());
        item.tool_arguments = Some(
            serde_json::json!({
                "role": "explorer",
                "display_name": "目录探查代理",
                "goal": "读取目录结构"
            })
            .to_string(),
        );
        item.tool_result = Some("{\"status\":\"started\"}".to_string());
        let turn = ActiveExecutionTurn {
            turn_id: "turn-agent-spawn-visible".to_string(),
            turn_seq: 1,
            accepted_at: now,
            completed_at: None,
            status: "running".to_string(),
            user_message: None,
            items: vec![item.clone()],
        };

        let canonical = to_canonical_turn_item(&session_id, &turn, &item)
            .expect("agent_spawn should be canonicalized");

        assert!(
            canonical.visibility.renderable,
            "agent_spawn 是主线代理卡片入口，必须保持可渲染"
        );
    }

    #[test]
    fn execute_session_turn_tool_call_requires_registry_before_runtime_tools() {
        let event_bus = InMemoryEventBus::new(8);
        let call = ChatToolCall {
            id: "tool-call-1".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: SKILL_APPLY_TOOL_NAME.to_string(),
                arguments: serde_json::json!({ "skill_name": "code-review" }).to_string(),
            },
        };

        let (_, status) = execute_session_turn_tool_call(
            &event_bus,
            None,
            None,
            None,
            None,
            None,
            &call,
            &SessionId::new("session-1"),
            &None,
            None,
            magi_core::AccessProfile::Restricted,
        );

        assert_eq!(status, ExecutionResultStatus::Failed);
        assert!(event_bus.snapshot().recent_events.is_empty());
    }

    #[test]
    fn execute_session_turn_tool_call_uses_skill_runtime_after_registry_check() {
        let registry = SkillRegistry::new();
        registry.register(SkillDefinition {
            skill_id: "code-review".to_string(),
            title: "代码审查".to_string(),
            instruction: "检查稳定性风险。".to_string(),
            metadata: SkillMetadata {
                category: "quality".to_string(),
                tags: vec!["review".to_string()],
            },
            allowed_tools: vec![],
            custom_tool_bindings: vec![],
            prompt_priority: 50,
        });
        let skill_runtime = SkillRuntime::new(registry);
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let call = ChatToolCall {
            id: "tool-call-1".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: SKILL_APPLY_TOOL_NAME.to_string(),
                arguments: serde_json::json!({ "skill_name": "code-review" }).to_string(),
            },
        };

        let (payload, status) = execute_session_turn_tool_call(
            &event_bus,
            Some(&tool_registry),
            Some(&skill_runtime),
            None,
            None,
            None,
            &call,
            &SessionId::new("session-1"),
            &None,
            None,
            magi_core::AccessProfile::Restricted,
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["skill_name"], "code-review");
        assert_eq!(event_bus.snapshot().recent_events.len(), 1);
    }

    #[test]
    fn execute_session_turn_tool_call_dispatches_custom_skill_binding() {
        let registry = SkillRegistry::new();
        registry.register(SkillDefinition {
            skill_id: "code-review".to_string(),
            title: "代码审查".to_string(),
            instruction: "检查稳定性风险。".to_string(),
            metadata: magi_skill_runtime::SkillMetadata {
                category: "quality".to_string(),
                tags: vec!["review".to_string()],
            },
            allowed_tools: vec![],
            custom_tool_bindings: vec![magi_skill_runtime::CustomToolBinding {
                binding_id: "review-mcp".to_string(),
                tool_name: "echo.describe".to_string(),
                description: "回显描述".to_string(),
                bridge_kind: BridgeBindingKind::Mcp,
                dispatch_action: BridgeDispatchAction::McpToolCall,
                bridge_target: "loopback-mcp".to_string(),
            }],
            prompt_priority: 50,
        });
        let skill_runtime = SkillRuntime::new(registry);
        let tool_registry = magi_tool_runtime::ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(magi_event_bus::InMemoryEventBus::new(8)),
        );
        let mcp_client = RecordingMcpClient::default();
        let mcp_calls = mcp_client.calls.clone();
        let skill_dispatch_runtime = SkillDispatchRuntime::new(
            tool_registry.clone(),
            BridgeDispatchRuntime::new().with_mcp_client(Arc::new(mcp_client)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let call = ChatToolCall {
            id: "tool-call-1".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "skill__code-review__review-mcp".to_string(),
                arguments: serde_json::json!({ "payload": "hello mcp" }).to_string(),
            },
        };

        let (payload, status) = execute_session_turn_tool_call(
            &event_bus,
            Some(&tool_registry),
            Some(&skill_runtime),
            Some(&skill_dispatch_runtime),
            Some("code-review"),
            None,
            &call,
            &SessionId::new("session-1"),
            &None,
            None,
            magi_core::AccessProfile::Restricted,
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "echo.describe");
        assert_eq!(parsed["server"], "loopback-mcp");
        assert_eq!(parsed["input"], "hello mcp");
        let recorded = mcp_calls.lock().expect("mcp calls lock");
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].server_name, "loopback-mcp");
        assert_eq!(recorded[0].tool_name, "echo.describe");
        assert_eq!(recorded[0].input, "hello mcp");
        assert_eq!(event_bus.snapshot().recent_events.len(), 1);
    }

    #[test]
    fn execute_session_turn_tool_call_rejects_internal_process_launch_surface() {
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let call = ChatToolCall {
            id: "tool-call-process-launch".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "process_launch".to_string(),
                arguments: serde_json::json!({ "command": "sleep 60" }).to_string(),
            },
        };

        let (payload, status) = execute_session_turn_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            None,
            None,
            None,
            &call,
            &SessionId::new("session-1"),
            &None,
            None,
            magi_core::AccessProfile::Restricted,
        );

        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "process_launch");
        assert_eq!(parsed["status"], "failed");
        assert!(
            parsed["error"]
                .as_str()
                .expect("error should be string")
                .contains("shell_exec")
        );
    }

    #[test]
    fn execute_session_turn_tool_call_rejects_write_tool_in_read_only_access_profile() {
        let mut tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_default_builtins();
        let event_bus = InMemoryEventBus::new(8);
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().to_path_buf();
        let target = root.join("blocked.txt");
        let call = ChatToolCall {
            id: "tool-call-read-only-file-write".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "file_write".to_string(),
                arguments: serde_json::json!({
                    "path": target.to_string_lossy(),
                    "content": "should not write"
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_session_turn_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            None,
            None,
            None,
            &call,
            &SessionId::new("session-read-only-tool"),
            &None,
            Some(&root),
            magi_core::AccessProfile::ReadOnly,
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        assert!(!target.exists(), "只读访问模式下 file_write 不能落盘");
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "file_write");
        assert_eq!(parsed["status"], "rejected");
        assert_eq!(parsed["access_profile"], "read_only");
    }

    #[test]
    fn execute_session_turn_tool_call_requires_approval_for_write_shell_in_restricted_profile() {
        let mut tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_default_builtins();
        let event_bus = InMemoryEventBus::new(8);
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().to_path_buf();
        let target = root.join("blocked-shell.txt");
        let call = ChatToolCall {
            id: "tool-call-restricted-shell".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "shell_exec".to_string(),
                arguments: serde_json::json!({
                    "command": format!("printf restricted > {}", target.display())
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_session_turn_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            None,
            None,
            None,
            &call,
            &SessionId::new("session-restricted-shell"),
            &None,
            Some(&root),
            magi_core::AccessProfile::Restricted,
        );

        assert_eq!(status, ExecutionResultStatus::NeedsApproval);
        assert!(
            !target.exists(),
            "受限模式下写类 shell 需要审批，不能提前执行"
        );
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "shell_exec");
        assert_eq!(parsed["status"], "needs_approval");
        assert_eq!(parsed["access_profile"], "restricted");
    }

    #[test]
    fn execute_session_turn_tool_call_allows_read_only_shell_in_read_only_profile() {
        let mut tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_default_builtins();
        let event_bus = InMemoryEventBus::new(8);
        let call = ChatToolCall {
            id: "tool-call-read-only-shell".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "shell_exec".to_string(),
                arguments: serde_json::json!({
                    "command": "printf readonly",
                    "access_mode": "read_only"
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_session_turn_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            None,
            None,
            None,
            &call,
            &SessionId::new("session-read-only-shell"),
            &None,
            None,
            magi_core::AccessProfile::ReadOnly,
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "shell_exec");
        assert_eq!(parsed["access_mode"], "read_only");
        assert_eq!(parsed["stdout"], "readonly");
    }

    #[test]
    fn execute_session_turn_tool_call_applies_safety_gate() {
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let safety_gate = magi_safety_gate::SafetyGate::with_builtin_defaults();
        let call = ChatToolCall {
            id: "tool-call-dangerous-shell".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "shell_exec".to_string(),
                arguments: serde_json::json!({
                    "command": "rm -rf /tmp/magi-safety-gate-probe"
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_session_turn_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            None,
            None,
            Some(&safety_gate),
            &call,
            &SessionId::new("session-1"),
            &None,
            None,
            magi_core::AccessProfile::Restricted,
        );

        assert_eq!(status, ExecutionResultStatus::NeedsApproval);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "shell_exec");
        assert_eq!(parsed["status"], "needs_approval");
        assert_eq!(parsed["safety_gate"]["pattern"], "rm -rf");
    }

    #[test]
    fn execute_session_turn_tool_call_skips_restricted_safety_gate_in_full_access() {
        let mut tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_default_builtins();
        let event_bus = InMemoryEventBus::new(8);
        let safety_gate =
            magi_safety_gate::SafetyGate::new(vec![magi_safety_gate::SafetyRule::new(
                "printf full-access-ok",
                magi_safety_gate::SafetyCategory::Custom,
            )]);
        let call = ChatToolCall {
            id: "tool-call-full-access-shell".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "shell_exec".to_string(),
                arguments: serde_json::json!({
                    "command": "printf full-access-ok"
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_session_turn_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            None,
            None,
            Some(&safety_gate),
            &call,
            &SessionId::new("session-1"),
            &None,
            None,
            magi_core::AccessProfile::FullAccess,
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["stdout"], "full-access-ok");
    }

    #[test]
    fn execute_session_turn_tool_call_applies_builtin_invocation_policy() {
        let mut tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_default_builtins();
        let event_bus = InMemoryEventBus::new(8);
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("nested");
        std::fs::create_dir_all(target.join("child")).expect("create nested dir");
        std::fs::write(target.join("child").join("probe.txt"), "probe").expect("write probe");
        let call = ChatToolCall {
            id: "tool-call-recursive-remove".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "file_remove".to_string(),
                arguments: serde_json::json!({
                    "path": target.to_string_lossy(),
                    "recursive": true
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_session_turn_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            None,
            None,
            None,
            &call,
            &SessionId::new("session-1"),
            &None,
            None,
            magi_core::AccessProfile::Restricted,
        );

        assert_eq!(status, ExecutionResultStatus::NeedsApproval);
        assert!(payload.contains("高风险工具必须人工审批"));
        assert!(target.exists(), "需要审批的递归删除不能提前执行");
    }

    #[test]
    fn session_turn_shell_tool_batch_executes_concurrently_and_preserves_order() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(32);
        let session_id = SessionId::new("session-turn-shell-batch");
        let workspace_id = Some(WorkspaceId::new("workspace-turn-shell-batch"));
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "shell batch session",
                workspace_id.as_ref().map(ToString::to_string),
            )
            .expect("session should be creatable");
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-shell-batch".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    status: "running".to_string(),
                    user_message: Some("并发执行 shell".to_string()),
                    items: Vec::new(),
                    completed_at: None,
                },
            )
            .expect("turn should be creatable");

        let probe = Arc::new(ConcurrentToolProbe::new(Duration::from_millis(180)));
        let mut tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_builtin(Arc::new(ProbeBuiltinTool::new(
            "shell_exec",
            Arc::clone(&probe),
        )));

        let tool_calls = vec![
            ChatToolCall {
                id: "tool-call-shell-a".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: "shell_exec".to_string(),
                    arguments: serde_json::json!({
                        "command": "printf a",
                        "access_mode": "read_only"
                    })
                    .to_string(),
                },
            },
            ChatToolCall {
                id: "tool-call-shell-b".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: "shell_exec".to_string(),
                    arguments: serde_json::json!({
                        "command": "printf b",
                        "access_mode": "read_only"
                    })
                    .to_string(),
                },
            },
        ];
        let mut messages = Vec::new();

        append_session_tool_call_items_batch(
            &session_store,
            &event_bus,
            Some(&tool_registry),
            None,
            None,
            None,
            None,
            &session_id,
            &workspace_id,
            None,
            magi_core::AccessProfile::Restricted,
            &tool_calls,
            &mut messages,
            None,
            None,
            &ThreadId::new("thread-shell-batch"),
            None,
            || true,
        );

        assert!(
            probe.max_active() > 1,
            "session turn 中的多个 shell 工具调用必须并发执行"
        );
        assert_eq!(
            messages
                .iter()
                .map(|message| message.tool_call_id.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("tool-call-shell-a"), Some("tool-call-shell-b")]
        );
        assert_eq!(
            messages
                .iter()
                .map(|message| message.role.as_str())
                .collect::<Vec<_>>(),
            vec!["tool", "tool"]
        );

        let sidecar = session_store
            .runtime_sidecar(&session_id)
            .expect("sidecar should exist");
        let turn = sidecar.current_turn.expect("turn should exist");
        assert_eq!(
            turn.items
                .iter()
                .map(|item| (
                    item.kind.as_str(),
                    item.status.as_str(),
                    item.item_seq,
                    item.tool_call_id.as_deref(),
                    item.tool_result.is_some(),
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    "tool_call_result",
                    "completed",
                    1,
                    Some("tool-call-shell-a"),
                    true
                ),
                (
                    "tool_call_result",
                    "completed",
                    2,
                    Some("tool-call-shell-b"),
                    true
                ),
            ]
        );
        let canonical_turn = session_store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| turn.turn_id == "turn-shell-batch")
            .expect("canonical turn should be stored");
        assert_eq!(
            canonical_turn
                .items
                .iter()
                .map(|item| (
                    item.kind,
                    item.status,
                    item.item_seq,
                    item.tool.as_ref().map(|tool| tool.call_id.as_str()),
                    item.tool
                        .as_ref()
                        .and_then(|tool| tool.result.as_ref())
                        .is_some(),
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    CanonicalTurnItemKind::ToolCall,
                    CanonicalTurnItemStatus::Completed,
                    1,
                    Some("tool-call-shell-a"),
                    true
                ),
                (
                    CanonicalTurnItemKind::ToolCall,
                    CanonicalTurnItemStatus::Completed,
                    2,
                    Some("tool-call-shell-b"),
                    true
                ),
            ],
            "工具 started/result 必须收敛为同一批 canonical tool item，且保持模型调用顺序"
        );

        let snapshot_event = event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .rev()
            .find(|event| event.event_type == "session.turn.item")
            .expect("session.turn.item event should be published");
        assert!(
            snapshot_event.payload.get("current_turn").is_some(),
            "实时 turn item 事件必须携带完整 current_turn 快照"
        );
        assert_eq!(
            snapshot_event
                .payload
                .get("turn_items")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(2),
            "实时 turn item 事件必须携带当前 turn 的全部 item"
        );
    }

    #[test]
    fn completed_plain_turn_summary_does_not_inherit_previous_execution_chain() {
        let session_store = SessionStore::new();
        let session_id = SessionId::new("session-plain-after-task");
        let mission_id = MissionId::new("mission-previous-task");
        let root_task_id = TaskId::new("task-previous-root");
        let now = UtcMillis::now();
        session_store
            .create_session(session_id.clone(), "plain after task")
            .expect("session should be creatable");
        let (_, orchestrator_thread_id) =
            session_store.ensure_session_mission(&session_id, now, || mission_id.clone());
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id,
                    root_task_id,
                    execution_chain_ref: "chain-previous-task".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: Vec::new(),
                    active_worker_bindings: Vec::new(),
                    branches: Vec::new(),
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-previous-task".to_string(),
                        trimmed_text: Some("之前的任务".to_string()),
                        skill_name: None,
                    },
                    current_turn: None,
                },
            )
            .expect("chain should be stored");

        let final_item = session_turn_item(
            "assistant_final",
            "completed",
            Some("最终回复".to_string()),
            Some("OK-普通流式".to_string()),
            Some("turn-item-plain-final".to_string()),
            orchestrator_thread_id.clone(),
        );
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-session-plain".to_string(),
                    turn_seq: 2,
                    accepted_at: now,
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("普通流式验证".to_string()),
                    items: vec![final_item],
                },
            )
            .expect("plain turn should be stored");
        session_store
            .update_current_turn_status(&session_id, "completed")
            .expect("plain turn should complete");

        let sidecar = session_store
            .runtime_sidecar(&session_id)
            .expect("runtime sidecar should exist");
        let published =
            published_session_turn_item_from_sidecar(sidecar, "turn-item-plain-final", None)
                .expect("plain final item should publish");

        assert_eq!(
            published.current_turn.mission_id, None,
            "普通 turn summary 不能继承上一轮任务 mission_id"
        );
        assert_eq!(
            published.current_turn.root_task_id, None,
            "普通 turn summary 不能继承上一轮任务 root_task_id"
        );
        assert_eq!(
            published.current_turn.execution_chain_ref, None,
            "普通 turn summary 不能继承上一轮任务 execution_chain_ref"
        );
    }
}
