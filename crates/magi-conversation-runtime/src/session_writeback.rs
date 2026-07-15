use crate::tool_declared_paths::{append_result_declared_paths, derive_declared_paths};
use crate::tool_result_utils::{
    model_visible_tool_result, summarize_tool_result, tool_execution_failed_result,
    tool_execution_status_label, turn_item_status_for_tool_result,
};
use crate::tool_surface_state::activated_skill_id_from_tool_result;
use crate::{
    SKILL_APPLY_TOOL_NAME, active_skill_tool_execution_policy, execute_skill_apply_from_runtime,
    execute_skill_custom_tool, internal_builtin_tool_rejection_payload,
    parse_skill_custom_tool_name,
    tool_batch::{
        access_profile_tool_decision, execute_goal_tool, safety_gate_tool_decision,
        select_preflight_decision,
    },
    tool_execution_policy_scope,
};
use magi_bridge_client::{
    ChatMessage, ChatToolCall, ModelRetryRuntimeEvent, ModelRetryRuntimePhase,
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
use magi_tool_runtime::{BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolRegistry};
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
    pub base_content_length: usize,
    pub content_length: usize,
    pub reset: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionToolCallBatchOutcome {
    pub completed: bool,
    pub succeeded_tool_names: Vec<String>,
    pub activated_skill_id: Option<String>,
}

const STREAM_ITEM_PUBLISH_MIN_INTERVAL_MS: u64 = 80;
const STREAM_ITEM_PUBLISH_MIN_CHARS: usize = 24;

#[derive(Clone, Debug, Default)]
pub struct SessionTurnStreamPublishGate {
    last_published_at: Option<UtcMillis>,
    last_published_content_length: usize,
    last_published_content: String,
    published_version: u64,
}

impl SessionTurnStreamPublishGate {
    fn should_publish_at(&self, update: &SessionTurnStreamUpdate, now: UtcMillis) -> bool {
        self.last_published_at.is_none()
            || update.reset
            || self.last_published_at.is_some_and(|last| {
                now.0.saturating_sub(last.0) >= STREAM_ITEM_PUBLISH_MIN_INTERVAL_MS
            })
            || update
                .content_length
                .saturating_sub(self.last_published_content_length)
                >= STREAM_ITEM_PUBLISH_MIN_CHARS
    }

    fn prepare_publish_at(
        &mut self,
        candidate: &SessionTurnStreamUpdate,
        current_content: &str,
        now: UtcMillis,
    ) -> Option<(u64, SessionTurnStreamUpdate)> {
        if !self.should_publish_at(candidate, now) {
            return None;
        }
        let update = session_turn_stream_update(&self.last_published_content, current_content)?;
        self.last_published_at = Some(now);
        self.last_published_content_length = update.content_length;
        self.last_published_content.clear();
        self.last_published_content.push_str(current_content);
        self.published_version = self
            .published_version
            .checked_add(1)
            .expect("stream publish version overflow");
        Some((self.published_version, update))
    }

    fn prepare_publish(
        &mut self,
        candidate: &SessionTurnStreamUpdate,
        current_content: &str,
    ) -> Option<(u64, SessionTurnStreamUpdate)> {
        self.prepare_publish_at(candidate, current_content, UtcMillis::now())
    }
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
        base_content_length: previous_content.chars().count(),
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
        "cancelled" | "canceled" | "killed" => Some(CanonicalTurnStatus::Cancelled),
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
    let mut metadata = item.metadata.clone();
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
    if let Some(renderable) = item.requested_renderable() {
        return renderable;
    }
    if kind == CanonicalTurnItemKind::ToolCall
        && item
            .tool_name
            .as_deref()
            .and_then(BuiltinToolName::from_name)
            .is_some_and(|tool| !tool.is_session_timeline_renderable_tool_call())
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
        metadata: Default::default(),
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
    let payload = serde_json::json!({
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
    publish_session_turn_item_payload(event_bus, session_id, workspace_id, payload);
}

pub fn publish_model_retry_runtime_event(
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    message_id: &str,
    task_id: Option<&TaskId>,
    event: &ModelRetryRuntimeEvent,
) {
    let phase = match event.phase {
        ModelRetryRuntimePhase::Scheduled => "scheduled",
        ModelRetryRuntimePhase::AttemptStarted => "attempt_started",
        ModelRetryRuntimePhase::Settled => "settled",
    };
    let payload = serde_json::json!({
        "session_id": session_id.to_string(),
        "workspace_id": workspace_id.as_ref().map(ToString::to_string),
        "message_id": message_id,
        "phase": phase,
        "attempt": event.attempt,
        "max_attempts": event.max_attempts,
        "delay_ms": event.delay_ms,
    });
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!(
                "model-retry-runtime-{}-{}",
                message_id,
                UtcMillis::now().0
            )),
            "model.retry.runtime",
            payload,
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            task_id: task_id.cloned(),
            ..EventContext::default()
        }),
    );
}

pub fn publish_session_turn_item_stream_event(
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    published: &PublishedSessionTurnItem,
    stream_update: &SessionTurnStreamUpdate,
    publish_gate: &mut SessionTurnStreamPublishGate,
) {
    let Some(canonical_item) = published.canonical_item.as_ref() else {
        return;
    };
    let current_content = canonical_item.content.as_deref().unwrap_or_default();
    let Some((item_version, stream_update)) =
        publish_gate.prepare_publish(stream_update, current_content)
    else {
        return;
    };
    let mut payload = serde_json::json!({
        "session_id": session_id.to_string(),
        "workspace_id": workspace_id.as_ref().map(ToString::to_string),
        "turn_id": published.turn_id,
        "turn_seq": published.turn_seq,
        "canonical_schema_version": CANONICAL_TURN_SCHEMA_VERSION,
        "canonical_event_kind": CanonicalTurnEventKind::TurnItemUpsert,
        "canonical_item_id": canonical_item.item_id,
        "canonical_item_version": item_version,
        "canonical_item_status": canonical_item.status,
        "stream_base_content_length": stream_update.base_content_length,
        "stream_delta": stream_update.delta,
        "stream_content_length": stream_update.content_length,
        "stream_reset": stream_update.reset,
    });
    if item_version == 1 {
        let mut canonical_item = canonical_item.clone();
        canonical_item.item_version = Some(item_version);
        payload
            .as_object_mut()
            .expect("stream event payload must be an object")
            .insert(
                "canonical_item".to_string(),
                serde_json::to_value(canonical_item)
                    .expect("canonical stream item must be serializable"),
            );
    }
    publish_session_turn_item_payload(event_bus, session_id, workspace_id, payload);
}

fn publish_session_turn_item_payload(
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    payload: Value,
) {
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

pub struct SessionTurnErrorInput<'a> {
    pub session_id: &'a SessionId,
    pub workspace_id: &'a Option<WorkspaceId>,
    pub task_id: Option<&'a TaskId>,
    pub request_id: Option<&'a str>,
    pub user_message_id: Option<&'a str>,
    pub placeholder_message_id: Option<&'a str>,
    pub error_text: &'a str,
    pub streaming_entry_id: Option<&'a str>,
    pub source_thread_id: ThreadId,
    pub persist_session_state: Option<&'a SessionStatePersistCallback>,
}

pub fn append_session_turn_error_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    input: SessionTurnErrorInput<'_>,
) {
    let SessionTurnErrorInput {
        session_id,
        workspace_id,
        task_id,
        request_id,
        user_message_id,
        placeholder_message_id,
        error_text,
        streaming_entry_id: _,
        source_thread_id,
        persist_session_state,
    } = input;
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

#[cfg(test)]
struct SessionToolCallBatchTestContext<'a> {
    session_store: &'a SessionStore,
    event_bus: &'a InMemoryEventBus,
    tool_registry: Option<&'a ToolRegistry>,
    skill_runtime: Option<&'a SkillRuntime>,
    skill_dispatch_runtime: Option<&'a SkillDispatchRuntime>,
    skill_name: Option<&'a str>,
    safety_gate: Option<&'a magi_safety_gate::SafetyGate>,
    session_id: &'a SessionId,
    workspace_id: &'a Option<WorkspaceId>,
    workspace_root_path: Option<PathBuf>,
    access_profile: magi_core::AccessProfile,
    snapshot_session: Option<Arc<SnapshotSession>>,
    execution_group_id: Option<String>,
    source_thread_id: &'a ThreadId,
    persist_session_state: Option<&'a SessionStatePersistCallback>,
}

#[cfg(test)]
fn append_session_tool_call_items_batch(
    context: SessionToolCallBatchTestContext<'_>,
    tool_calls: &[ChatToolCall],
    messages: &mut Vec<ChatMessage>,
    write_allowed: impl Fn() -> bool,
) -> SessionToolCallBatchOutcome {
    let SessionToolCallBatchTestContext {
        session_store,
        event_bus,
        tool_registry,
        skill_runtime,
        skill_dispatch_runtime,
        skill_name,
        safety_gate,
        session_id,
        workspace_id,
        workspace_root_path,
        access_profile,
        snapshot_session,
        execution_group_id,
        source_thread_id,
        persist_session_state,
    } = context;
    let todo_ledger = crate::test_todo_ledger("test-todo-ledger");
    let mission_id = magi_core::MissionId::new(format!("mission-{session_id}"));
    append_session_tool_call_items_batch_with_context(
        SessionToolCallBatchContext {
            session_store,
            event_bus,
            tool_registry,
            skill_runtime,
            skill_dispatch_runtime,
            skill_name,
            safety_gate,
            todo_ledger: &todo_ledger,
            mission_id: &mission_id,
            session_id,
            workspace_id,
            workspace_root_path,
            context_references: &[],
            access_profile,
            snapshot_session,
            execution_group_id,
            source_thread_id,
            persist_session_state,
        },
        tool_calls,
        messages,
        write_allowed,
    )
}

pub struct SessionToolCallBatchContext<'a> {
    pub session_store: &'a SessionStore,
    pub event_bus: &'a InMemoryEventBus,
    pub tool_registry: Option<&'a ToolRegistry>,
    pub skill_runtime: Option<&'a SkillRuntime>,
    pub skill_dispatch_runtime: Option<&'a SkillDispatchRuntime>,
    pub skill_name: Option<&'a str>,
    pub safety_gate: Option<&'a magi_safety_gate::SafetyGate>,
    pub todo_ledger: &'a magi_todo_ledger::TodoLedger,
    pub mission_id: &'a magi_core::MissionId,
    pub session_id: &'a SessionId,
    pub workspace_id: &'a Option<WorkspaceId>,
    pub workspace_root_path: Option<PathBuf>,
    pub context_references: &'a [crate::context_reference::SessionContextReference],
    pub access_profile: magi_core::AccessProfile,
    pub snapshot_session: Option<Arc<SnapshotSession>>,
    pub execution_group_id: Option<String>,
    pub source_thread_id: &'a ThreadId,
    pub persist_session_state: Option<&'a SessionStatePersistCallback>,
}

pub fn append_session_tool_call_items_batch_with_context(
    context: SessionToolCallBatchContext<'_>,
    tool_calls: &[ChatToolCall],
    messages: &mut Vec<ChatMessage>,
    write_allowed: impl Fn() -> bool,
) -> SessionToolCallBatchOutcome {
    let SessionToolCallBatchContext {
        session_store,
        event_bus,
        tool_registry,
        skill_runtime,
        skill_dispatch_runtime,
        skill_name,
        safety_gate,
        todo_ledger,
        mission_id,
        session_id,
        workspace_id,
        workspace_root_path,
        context_references,
        access_profile,
        snapshot_session,
        execution_group_id,
        source_thread_id,
        persist_session_state,
    } = context;
    for tool_call in tool_calls {
        if !write_allowed() {
            return SessionToolCallBatchOutcome::default();
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
        SessionToolExecutionContext {
            session_store,
            event_bus,
            tool_registry,
            skill_runtime,
            skill_dispatch_runtime,
            skill_name,
            safety_gate,
            todo_ledger,
            mission_id,
            session_id,
            workspace_id,
            workspace_root_path: workspace_root_path.as_ref(),
            context_references,
            access_profile,
        },
        tool_calls,
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

    let mut succeeded_tool_names = Vec::new();
    let mut activated_skill_id = None;
    for (tool_call, (tool_result, tool_status)) in tool_calls.iter().zip(tool_results) {
        if !write_allowed() {
            return SessionToolCallBatchOutcome::default();
        }
        if is_session_goal_write_tool(&tool_call.function.name)
            && matches!(tool_status, ExecutionResultStatus::Succeeded)
        {
            persist_session_state_checkpoint(persist_session_state, "session_goal_tool");
        }
        upsert_session_tool_call_result_item(
            SessionToolResultWritebackContext {
                session_store,
                event_bus,
                session_id,
                workspace_id,
                source_thread_id,
                persist_session_state,
            },
            tool_call,
            &tool_result,
            tool_status,
        );
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(model_visible_tool_result(&tool_result, tool_status)),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call.id.clone()),
        });
        if matches!(tool_status, ExecutionResultStatus::Succeeded) {
            succeeded_tool_names.push(tool_call.function.name.clone());
        }
        if let Some(skill_id) =
            activated_skill_id_from_tool_result(&tool_call.function.name, &tool_result, tool_status)
        {
            activated_skill_id = Some(skill_id);
        }
    }
    SessionToolCallBatchOutcome {
        completed: true,
        succeeded_tool_names,
        activated_skill_id,
    }
}

#[derive(Clone, Copy)]
struct SessionToolResultWritebackContext<'a> {
    session_store: &'a SessionStore,
    event_bus: &'a InMemoryEventBus,
    session_id: &'a SessionId,
    workspace_id: &'a Option<WorkspaceId>,
    source_thread_id: &'a ThreadId,
    persist_session_state: Option<&'a SessionStatePersistCallback>,
}

fn upsert_session_tool_call_result_item(
    context: SessionToolResultWritebackContext<'_>,
    tool_call: &ChatToolCall,
    tool_result: &str,
    tool_status: ExecutionResultStatus,
) {
    let status_label = tool_execution_status_label(tool_status);
    let mut result_item = session_turn_item(
        "tool_call_result",
        turn_item_status_for_tool_result(tool_status),
        Some(tool_call.function.name.clone()),
        Some(summarize_tool_result(tool_result)),
        Some(format!("turn-item-tool-{}", tool_call.id)),
        context.source_thread_id.clone(),
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
    if let Some(published) =
        upsert_session_turn_item(context.session_store, context.session_id, result_item)
    {
        persist_session_state_checkpoint(context.persist_session_state, "session_turn_tool_result");
        publish_session_turn_item_event(
            context.event_bus,
            context.session_id,
            context.workspace_id,
            &published,
        );
    }
}

#[derive(Clone, Copy)]
struct SessionToolExecutionContext<'a> {
    session_store: &'a SessionStore,
    event_bus: &'a InMemoryEventBus,
    tool_registry: Option<&'a ToolRegistry>,
    skill_runtime: Option<&'a SkillRuntime>,
    skill_dispatch_runtime: Option<&'a SkillDispatchRuntime>,
    skill_name: Option<&'a str>,
    safety_gate: Option<&'a magi_safety_gate::SafetyGate>,
    todo_ledger: &'a magi_todo_ledger::TodoLedger,
    mission_id: &'a magi_core::MissionId,
    session_id: &'a SessionId,
    workspace_id: &'a Option<WorkspaceId>,
    workspace_root_path: Option<&'a PathBuf>,
    context_references: &'a [crate::context_reference::SessionContextReference],
    access_profile: magi_core::AccessProfile,
}

fn execute_session_turn_tool_call_batch(
    context: SessionToolExecutionContext<'_>,
    tool_calls: &[ChatToolCall],
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
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        execute_session_turn_tool_call_scoped(context, &tool_calls[tool_index])
                    }))
                    .unwrap_or_else(|_| {
                        tracing::warn!(
                            tool_name = %tool_calls[tool_index].function.name,
                            tool_call_id = %tool_calls[tool_index].id,
                            session_id = %context.session_id.as_str(),
                            "session turn tool execution panicked"
                        );
                        tool_execution_failed_result(&tool_calls[tool_index].function.name)
                    });
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
                                    if let Some(snapshot) = snapshot_session.as_deref() {
                                        snapshot.before_tool(&hook_ctx);
                                    }
                                    let result = std::panic::catch_unwind(
                                        std::panic::AssertUnwindSafe(|| {
                                            execute_session_turn_tool_call_scoped(
                                                context, tool_call,
                                            )
                                        }),
                                    );
                                    let result = result.unwrap_or_else(|_| {
                                        tracing::warn!(
                                            tool_name = %tool_call.function.name,
                                            tool_call_id = %tool_call.id,
                                            session_id = %context.session_id.as_str(),
                                            "session turn tool execution panicked"
                                        );
                                        tool_execution_failed_result(&tool_call.function.name)
                                    });
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
                            tracing::warn!(
                                tool_name = %tool_calls[tool_index].function.name,
                                tool_call_id = %tool_calls[tool_index].id,
                                session_id = %context.session_id.as_str(),
                                "session turn tool execution thread panicked"
                            );
                            tool_execution_failed_result(&tool_calls[tool_index].function.name)
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
                tool_execution_failed_result(&tool_calls[tool_index].function.name)
            })
        })
        .collect()
}

#[cfg(test)]
struct SessionToolCallTestContext<'a> {
    session_store: &'a SessionStore,
    event_bus: &'a InMemoryEventBus,
    tool_registry: Option<&'a ToolRegistry>,
    skill_runtime: Option<&'a SkillRuntime>,
    skill_dispatch_runtime: Option<&'a SkillDispatchRuntime>,
    skill_name: Option<&'a str>,
    safety_gate: Option<&'a magi_safety_gate::SafetyGate>,
    session_id: &'a SessionId,
    workspace_id: &'a Option<WorkspaceId>,
    workspace_root_path: Option<&'a PathBuf>,
    access_profile: magi_core::AccessProfile,
}

#[cfg(test)]
fn execute_session_turn_tool_call(
    context: SessionToolCallTestContext<'_>,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    let SessionToolCallTestContext {
        session_store,
        event_bus,
        tool_registry,
        skill_runtime,
        skill_dispatch_runtime,
        skill_name,
        safety_gate,
        session_id,
        workspace_id,
        workspace_root_path,
        access_profile,
    } = context;
    let todo_ledger = crate::test_todo_ledger("test-todo-ledger");
    let mission_id = magi_core::MissionId::new(format!("mission-{session_id}"));
    execute_session_turn_tool_call_scoped(
        SessionToolExecutionContext {
            session_store,
            event_bus,
            tool_registry,
            skill_runtime,
            skill_dispatch_runtime,
            skill_name,
            safety_gate,
            todo_ledger: &todo_ledger,
            mission_id: &mission_id,
            session_id,
            workspace_id,
            workspace_root_path,
            context_references: &[],
            access_profile,
        },
        tool_call,
    )
}

fn execute_session_turn_tool_call_scoped(
    context: SessionToolExecutionContext<'_>,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    let SessionToolExecutionContext {
        session_store,
        event_bus,
        tool_registry,
        skill_runtime,
        skill_dispatch_runtime,
        skill_name,
        safety_gate,
        todo_ledger,
        mission_id,
        session_id,
        workspace_id,
        workspace_root_path,
        context_references,
        access_profile,
    } = context;
    if let Some(canonical) = BuiltinToolName::from_name(tool_call.function.name.as_str())
        && matches!(
            canonical,
            BuiltinToolName::GetGoal | BuiltinToolName::CreateGoal | BuiltinToolName::UpdateGoal
        )
    {
        let Some(thread_id) = session_store
            .orchestrator_thread_for_session(session_id)
            .map(|thread| thread.thread_id)
        else {
            return (
                serde_json::json!({
                    "tool": canonical.as_str(),
                    "status": "failed",
                    "error": "当前会话缺少主线 thread，无法执行 goal 工具",
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        };
        return execute_goal_tool(
            session_store,
            session_id,
            thread_id,
            access_profile,
            canonical,
            &tool_call.function.arguments,
        );
    }

    if matches!(
        BuiltinToolName::from_name(tool_call.function.name.as_str()),
        Some(BuiltinToolName::TodoWrite)
    ) {
        return magi_todo_ledger::execute_session_todo_write_tool(
            event_bus,
            todo_ledger,
            session_id,
            workspace_id.as_ref(),
            mission_id,
            &tool_call.function.arguments,
        );
    }

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

    let reference_policy = crate::context_reference::session_context_reference_policy(
        context_references,
        workspace_root_path
            .map(|path| path.to_string_lossy())
            .as_deref(),
        access_profile,
    );

    if let Some((tool_skill_name, binding_id)) =
        parse_skill_custom_tool_name(&tool_call.function.name)
    {
        let mut tool_policy =
            tool_execution_policy_scope(access_profile, "", &reference_policy.allowed_paths, &[]);
        tool_policy.read_only_paths = reference_policy.read_only_paths.clone();
        return execute_skill_custom_tool(
            tool_call,
            &tool_skill_name,
            &binding_id,
            skill_name,
            tool_policy,
            safety_gate,
            skill_runtime,
            skill_dispatch_runtime,
            ToolExecutionContext {
                worker_id: None,
                task_id: None,
                session_id: Some(session_id.clone()),
                workspace_id: workspace_id.clone(),
                access_profile,
                working_directory: workspace_root_path.cloned(),
            },
            workspace_root_path
                .as_ref()
                .map(|path| path.display().to_string()),
        );
    }

    if let Some(result) = registry.execute_external_mcp_tool(
        &tool_call.function.name,
        &tool_call.function.arguments,
        access_profile,
    ) {
        return result;
    }

    if let Some(rejection) = internal_builtin_tool_rejection_payload(&tool_call.function.name) {
        return (rejection, ExecutionResultStatus::Failed);
    }

    let access_profile_decision =
        access_profile_tool_decision(crate::tool_batch::AccessProfileToolDecisionInput {
            access_profile,
            command_mode: "",
            allowed_tools: &[],
            denied_tools: &[],
            allowed_paths: &reference_policy.allowed_paths,
            denied_paths: &[],
            read_only_paths: &reference_policy.read_only_paths,
            requested_tool_name: &tool_call.function.name,
            arguments: &tool_call.function.arguments,
            workspace_root_path,
        });
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

    let mut tool_policy =
        active_skill_tool_execution_policy(access_profile, skill_runtime, skill_name);
    tool_policy.allowed_paths = reference_policy.allowed_paths;
    tool_policy.read_only_paths = reference_policy.read_only_paths;
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
            access_profile: tool_policy.access_profile,
            working_directory: workspace_root_path.cloned(),
        },
        &tool_policy,
    );
    (output.payload, output.status)
}

fn is_session_goal_write_tool(tool_name: &str) -> bool {
    BuiltinToolName::from_name(tool_name).is_some_and(|tool| {
        matches!(
            tool,
            BuiltinToolName::CreateGoal | BuiltinToolName::UpdateGoal
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::{
        BridgeBindingKind, BridgeDispatchAction, BridgeDispatchRuntime, BridgeResponse,
        ChatToolFunction, McpBridgeClient, McpToolCallRequest, ModelRetryRuntimeEvent,
        ModelRetryRuntimePhase,
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
    fn model_retry_runtime_event_is_session_scoped() {
        let event_bus = InMemoryEventBus::new(8);
        let session_id = SessionId::new("session-model-retry-runtime");
        let workspace_id = Some(WorkspaceId::new("workspace-model-retry-runtime"));

        publish_model_retry_runtime_event(
            &event_bus,
            &session_id,
            &workspace_id,
            "assistant-message-retry",
            Some(&TaskId::new("task-model-retry-runtime")),
            &ModelRetryRuntimeEvent {
                phase: ModelRetryRuntimePhase::Scheduled,
                attempt: 2,
                max_attempts: 5,
                delay_ms: Some(15_000),
            },
        );

        let snapshot = event_bus.snapshot();
        let event = snapshot.recent_events.last().expect("retry event");
        assert_eq!(event.event_type, "model.retry.runtime");
        assert_eq!(event.session_id.as_ref(), Some(&session_id));
        assert_eq!(event.workspace_id.as_ref(), workspace_id.as_ref());
        assert_eq!(
            event.task_id.as_ref().map(TaskId::as_str),
            Some("task-model-retry-runtime")
        );
        assert_eq!(event.payload["message_id"], "assistant-message-retry");
        assert_eq!(event.payload["phase"], "scheduled");
        assert_eq!(event.payload["attempt"], 2);
        assert_eq!(event.payload["max_attempts"], 5);
        assert_eq!(event.payload["delay_ms"], 15_000);
    }

    #[test]
    fn stream_update_reports_append_delta_and_reset() {
        let appended =
            session_turn_stream_update("你好", "你好，世界").expect("append update should exist");
        assert_eq!(appended.delta, "，世界");
        assert_eq!(appended.base_content_length, 2);
        assert_eq!(appended.content_length, 5);
        assert!(!appended.reset);

        let reset = session_turn_stream_update("旧内容", "新内容").expect("reset should exist");
        assert_eq!(reset.delta, "新内容");
        assert_eq!(reset.base_content_length, 3);
        assert_eq!(reset.content_length, 3);
        assert!(reset.reset);

        assert!(session_turn_stream_update("same", "same").is_none());
    }

    #[test]
    fn stream_publish_gate_keeps_first_frame_and_coalesces_bursts() {
        let mut gate = SessionTurnStreamPublishGate::default();
        let first = SessionTurnStreamUpdate {
            delta: "a".to_string(),
            base_content_length: 0,
            content_length: 1,
            reset: false,
        };
        let (version, published) = gate
            .prepare_publish_at(&first, "a", UtcMillis(1_000))
            .expect("first frame should publish");
        assert_eq!(version, 1);
        assert_eq!(published.delta, "a");

        let burst = SessionTurnStreamUpdate {
            delta: "b".to_string(),
            base_content_length: 1,
            content_length: 2,
            reset: false,
        };
        assert!(
            gate.prepare_publish_at(&burst, "ab", UtcMillis(1_001))
                .is_none()
        );

        let enough_chars = SessionTurnStreamUpdate {
            delta: "c".repeat(STREAM_ITEM_PUBLISH_MIN_CHARS),
            base_content_length: 2,
            content_length: STREAM_ITEM_PUBLISH_MIN_CHARS + 2,
            reset: false,
        };
        let enough_content = format!("ab{}", "c".repeat(STREAM_ITEM_PUBLISH_MIN_CHARS));
        let (version, published) = gate
            .prepare_publish_at(&enough_chars, &enough_content, UtcMillis(1_002))
            .expect("coalesced content should publish");
        assert_eq!(version, 2);
        assert_eq!(
            published.delta,
            format!("b{}", "c".repeat(STREAM_ITEM_PUBLISH_MIN_CHARS))
        );

        let delayed = SessionTurnStreamUpdate {
            delta: "d".to_string(),
            base_content_length: STREAM_ITEM_PUBLISH_MIN_CHARS + 2,
            content_length: STREAM_ITEM_PUBLISH_MIN_CHARS + 3,
            reset: false,
        };
        let delayed_content = format!("{enough_content}d");
        let (version, published) = gate
            .prepare_publish_at(
                &delayed,
                &delayed_content,
                UtcMillis(1_002 + STREAM_ITEM_PUBLISH_MIN_INTERVAL_MS),
            )
            .expect("delayed frame should publish");
        assert_eq!(version, 3);
        assert_eq!(published.delta, "d");

        let reset = SessionTurnStreamUpdate {
            delta: "reset".to_string(),
            base_content_length: STREAM_ITEM_PUBLISH_MIN_CHARS + 3,
            content_length: 5,
            reset: true,
        };
        let (version, published) = gate
            .prepare_publish_at(&reset, "reset", UtcMillis(1_003))
            .expect("reset frame should publish");
        assert_eq!(version, 4);
        assert!(published.reset);
    }

    #[test]
    fn stream_events_publish_one_item_snapshot_then_delta_only_frames() {
        let session_store = SessionStore::new();
        let session_id = SessionId::new("session-stream-delta-payload");
        session_store
            .create_session(session_id.clone(), "stream delta payload")
            .expect("session should be creatable");
        let now = UtcMillis::now();
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-stream-delta-payload".to_string(),
                    turn_seq: 1,
                    accepted_at: now,
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: None,
                    items: Vec::new(),
                },
            )
            .expect("running turn should be stored");

        let event_bus = InMemoryEventBus::new(8);
        let workspace_id = None;
        let source_thread_id = ThreadId::new("thread-stream-delta-payload");
        let item_id = "assistant-stream";
        let mut gate = SessionTurnStreamPublishGate::default();

        let first_content = "你";
        let first_item = session_turn_item(
            "assistant_stream",
            "running",
            Some("生成回复".to_string()),
            Some(first_content.to_string()),
            Some(item_id.to_string()),
            source_thread_id.clone(),
        );
        let first_published = upsert_session_turn_item(&session_store, &session_id, first_item)
            .expect("first stream item should be published");
        let first_update = session_turn_stream_update("", first_content)
            .expect("first stream update should exist");
        publish_session_turn_item_stream_event(
            &event_bus,
            &session_id,
            &workspace_id,
            &first_published,
            &first_update,
            &mut gate,
        );

        let suppressed_content = "你好";
        let suppressed_item = session_turn_item(
            "assistant_stream",
            "running",
            Some("生成回复".to_string()),
            Some(suppressed_content.to_string()),
            Some(item_id.to_string()),
            source_thread_id.clone(),
        );
        let suppressed_published =
            upsert_session_turn_item(&session_store, &session_id, suppressed_item)
                .expect("suppressed stream item should be stored");
        let suppressed_update = session_turn_stream_update(first_content, suppressed_content)
            .expect("suppressed stream update should exist");
        publish_session_turn_item_stream_event(
            &event_bus,
            &session_id,
            &workspace_id,
            &suppressed_published,
            &suppressed_update,
            &mut gate,
        );

        let second_content = format!("你好{}", "呀".repeat(STREAM_ITEM_PUBLISH_MIN_CHARS));
        let second_item = session_turn_item(
            "assistant_stream",
            "running",
            Some("生成回复".to_string()),
            Some(second_content.clone()),
            Some(item_id.to_string()),
            source_thread_id,
        );
        let second_published = upsert_session_turn_item(&session_store, &session_id, second_item)
            .expect("second stream item should be published");
        let second_update = session_turn_stream_update(suppressed_content, &second_content)
            .expect("second stream update should exist");
        publish_session_turn_item_stream_event(
            &event_bus,
            &session_id,
            &workspace_id,
            &second_published,
            &second_update,
            &mut gate,
        );

        let events = event_bus.snapshot().recent_events;
        assert_eq!(events.len(), 2);
        let first_payload = &events[0].payload;
        assert!(first_payload.get("canonical_turn").is_none());
        assert!(first_payload.get("item").is_none());
        assert!(first_payload.get("current_turn").is_none());
        assert!(first_payload.get("turn_items").is_none());
        assert_eq!(
            first_payload["canonical_item"]["itemId"],
            Value::String(item_id.to_string())
        );
        assert_eq!(
            first_payload["canonical_item"]["itemVersion"],
            Value::from(1_u64)
        );
        assert_eq!(first_payload["canonical_item_version"], Value::from(1_u64));
        assert_eq!(
            first_payload["stream_base_content_length"],
            Value::from(0_u64)
        );
        assert_eq!(first_payload["stream_content_length"], Value::from(1_u64));
        assert_eq!(first_payload["stream_reset"], Value::Bool(false));

        let second_payload = &events[1].payload;
        assert!(second_payload.get("canonical_turn").is_none());
        assert!(second_payload.get("canonical_item").is_none());
        assert!(second_payload.get("item").is_none());
        assert!(second_payload.get("current_turn").is_none());
        assert!(second_payload.get("turn_items").is_none());
        assert_eq!(
            second_payload["canonical_item_id"],
            Value::String(item_id.to_string())
        );
        assert_eq!(second_payload["canonical_item_version"], Value::from(2_u64));
        assert_eq!(
            second_payload["stream_base_content_length"],
            Value::from(1_u64)
        );
        assert_eq!(
            second_payload["canonical_item_status"],
            Value::String("running".to_string())
        );
        assert_eq!(
            second_payload["stream_delta"],
            Value::String(format!("好{}", "呀".repeat(STREAM_ITEM_PUBLISH_MIN_CHARS)))
        );
        assert_eq!(
            second_payload["stream_content_length"],
            Value::from(second_content.chars().count() as u64)
        );
        assert_eq!(second_payload["stream_reset"], Value::Bool(false));

        let snapshot_event_bus = InMemoryEventBus::new(1);
        publish_session_turn_item_event(
            &snapshot_event_bus,
            &session_id,
            &workspace_id,
            &second_published,
        );
        let snapshot_payload = &snapshot_event_bus.snapshot().recent_events[0].payload;
        assert!(snapshot_payload.get("canonical_turn").is_some());
        assert!(snapshot_payload.get("canonical_item").is_some());
        assert!(snapshot_payload.get("item").is_some());
        assert!(snapshot_payload.get("current_turn").is_some());
        assert!(snapshot_payload.get("turn_items").is_some());
        assert!(snapshot_payload.get("stream_delta").is_none());
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

    struct PanicBuiltinTool {
        name: &'static str,
    }

    struct SnapshotReconcileProbeTool {
        name: &'static str,
        snapshot: Arc<SnapshotSession>,
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

    impl BuiltinTool for PanicBuiltinTool {
        fn name(&self) -> &'static str {
            self.name
        }

        fn execute(
            &self,
            _input: &str,
            _context: &ToolExecutionContext,
            _resources: &magi_tool_runtime::ToolRuntimeResources,
        ) -> String {
            panic!("internal panic detail must stay out of public tool output")
        }

        fn spec(&self) -> BuiltinToolSpec {
            BuiltinToolSpec {
                name: self.name.to_string(),
                risk_level: RiskLevel::Low,
                approval_requirement: ApprovalRequirement::None,
            }
        }
    }

    impl SnapshotReconcileProbeTool {
        fn new(name: &'static str, snapshot: Arc<SnapshotSession>) -> Self {
            Self { name, snapshot }
        }
    }

    impl BuiltinTool for SnapshotReconcileProbeTool {
        fn name(&self) -> &'static str {
            self.name
        }

        fn execute(
            &self,
            input: &str,
            context: &ToolExecutionContext,
            _resources: &magi_tool_runtime::ToolRuntimeResources,
        ) -> String {
            let arguments = serde_json::from_str::<serde_json::Value>(input).unwrap_or_default();
            let path = arguments
                .get("changed_paths")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| items.first())
                .and_then(serde_json::Value::as_str)
                .expect("probe changed path");
            let workspace_root = context
                .working_directory
                .as_ref()
                .expect("probe working directory");
            std::fs::write(workspace_root.join(path), format!("probe {path}"))
                .expect("probe file write");
            self.snapshot.reconcile().expect("probe reconcile");
            serde_json::json!({
                "tool": self.name,
                "status": "succeeded",
                "stdout": "snapshot reconciled"
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
    fn canonical_turn_respects_explicit_non_renderable_model_output() {
        let session_id = SessionId::new("session-hidden-goal-progress");
        let thread_id = ThreadId::new("thread-hidden-goal-progress");
        let now = UtcMillis::now();
        let mut item = session_turn_item(
            "assistant_final",
            "completed",
            Some("最终回复".to_string()),
            Some("目标仍在推进".to_string()),
            Some("turn-item-hidden-goal-progress".to_string()),
            thread_id,
        );
        item.metadata
            .insert("renderable".to_string(), serde_json::Value::Bool(false));
        let turn = ActiveExecutionTurn {
            turn_id: "turn-hidden-goal-progress".to_string(),
            turn_seq: 1,
            accepted_at: now,
            completed_at: Some(now),
            status: "completed".to_string(),
            user_message: None,
            items: vec![item.clone()],
        };

        let canonical = to_canonical_turn_item(&session_id, &turn, &item)
            .expect("目标中间输出应保留在 canonical 审计记录");

        assert!(!canonical.visibility.renderable);
        assert_eq!(canonical.content.as_deref(), Some("目标仍在推进"));
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
            SessionToolCallTestContext {
                session_store: &SessionStore::new(),
                event_bus: &event_bus,
                tool_registry: None,
                skill_runtime: None,
                skill_dispatch_runtime: None,
                skill_name: None,
                safety_gate: None,
                session_id: &SessionId::new("session-1"),
                workspace_id: &None,
                workspace_root_path: None,
                access_profile: magi_core::AccessProfile::Restricted,
            },
            &call,
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
            restrict_standard_tools: true,
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
            SessionToolCallTestContext {
                session_store: &SessionStore::new(),
                event_bus: &event_bus,
                tool_registry: Some(&tool_registry),
                skill_runtime: Some(&skill_runtime),
                skill_dispatch_runtime: None,
                skill_name: None,
                safety_gate: None,
                session_id: &SessionId::new("session-1"),
                workspace_id: &None,
                workspace_root_path: None,
                access_profile: magi_core::AccessProfile::Restricted,
            },
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["skill_name"], "code-review");
        assert_eq!(event_bus.snapshot().recent_events.len(), 1);
    }

    #[test]
    fn execute_session_turn_tool_call_routes_live_mcp_tool_through_registry() {
        let event_bus = InMemoryEventBus::new(8);
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        )
        .with_external_tool_catalog_provider(Arc::new(|| {
            magi_tool_runtime::ExternalToolCatalogSnapshot {
                instruction_skill_count: 0,
                mcp_tools: vec![magi_tool_runtime::ExternalMcpToolCatalogEntry {
                    server_id: "repo-tools".to_string(),
                    server_name: "Repository Tools".to_string(),
                    model_tool_name: "mcp__repo-tools__inspect".to_string(),
                    tool_name: "inspect".to_string(),
                    description: "Inspect repository".to_string(),
                    read_only: true,
                    input_schema: serde_json::json!({ "type": "object", "properties": {} }),
                }],
                ..magi_tool_runtime::ExternalToolCatalogSnapshot::default()
            }
        }))
        .with_external_mcp_tool_executor(Arc::new(|server_id, tool_name, arguments| {
            assert_eq!(server_id, "repo-tools");
            assert_eq!(tool_name, "inspect");
            assert_eq!(arguments, "{}");
            (
                serde_json::json!({ "status": "succeeded", "files": 3 }).to_string(),
                ExecutionResultStatus::Succeeded,
            )
        }));
        let call = ChatToolCall {
            id: "tool-call-live-mcp".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "mcp__repo-tools__inspect".to_string(),
                arguments: "{}".to_string(),
            },
        };

        let (payload, status) = execute_session_turn_tool_call(
            SessionToolCallTestContext {
                session_store: &SessionStore::new(),
                event_bus: &event_bus,
                tool_registry: Some(&tool_registry),
                skill_runtime: None,
                skill_dispatch_runtime: None,
                skill_name: None,
                safety_gate: None,
                session_id: &SessionId::new("session-live-mcp"),
                workspace_id: &None,
                workspace_root_path: None,
                access_profile: magi_core::AccessProfile::Restricted,
            },
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        assert!(payload.contains("\"files\":3"));
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
            restrict_standard_tools: true,
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
            SessionToolCallTestContext {
                session_store: &SessionStore::new(),
                event_bus: &event_bus,
                tool_registry: Some(&tool_registry),
                skill_runtime: Some(&skill_runtime),
                skill_dispatch_runtime: Some(&skill_dispatch_runtime),
                skill_name: Some("code-review"),
                safety_gate: None,
                session_id: &SessionId::new("session-1"),
                workspace_id: &None,
                workspace_root_path: None,
                access_profile: magi_core::AccessProfile::FullAccess,
            },
            &call,
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
    fn restricted_mcp_skill_binding_requires_approval() {
        let registry = SkillRegistry::new();
        registry.register(SkillDefinition {
            skill_id: "code-review".to_string(),
            title: "代码审查".to_string(),
            instruction: "检查稳定性风险。".to_string(),
            metadata: magi_skill_runtime::SkillMetadata {
                category: "quality".to_string(),
                tags: vec!["review".to_string()],
            },
            restrict_standard_tools: true,
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
            id: "tool-call-mcp-restricted".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "skill__code-review__review-mcp".to_string(),
                arguments: serde_json::json!({ "payload": "hello mcp" }).to_string(),
            },
        };

        let (payload, status) = execute_session_turn_tool_call(
            SessionToolCallTestContext {
                session_store: &SessionStore::new(),
                event_bus: &event_bus,
                tool_registry: Some(&tool_registry),
                skill_runtime: Some(&skill_runtime),
                skill_dispatch_runtime: Some(&skill_dispatch_runtime),
                skill_name: Some("code-review"),
                safety_gate: None,
                session_id: &SessionId::new("session-1"),
                workspace_id: &None,
                workspace_root_path: None,
                access_profile: magi_core::AccessProfile::Restricted,
            },
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::NeedsApproval);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["status"], "needs_approval");
        assert_eq!(parsed["tool"], "skill__code-review__review-mcp");
        assert_eq!(parsed["error_code"], "skill_tool_needs_approval");
        assert_eq!(
            parsed["error"],
            "受限访问已拦截该 Skill 工具，请切换为完全访问权限后重试"
        );
        assert_eq!(parsed.get("bridge_kind"), None);
        assert_eq!(parsed.get("bridge_target"), None);
        assert_eq!(parsed.get("risk_level"), None);
        assert!(mcp_calls.lock().expect("mcp calls lock").is_empty());
        let invocations = tool_registry.invocations();
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].tool_kind, magi_governance::ToolKind::Mcp);
        assert_eq!(invocations[0].status, ExecutionResultStatus::NeedsApproval);
    }

    #[test]
    fn execute_session_turn_tool_call_applies_safety_gate_to_mcp_skill_payload() {
        let registry = SkillRegistry::new();
        registry.register(SkillDefinition {
            skill_id: "shell-mcp".to_string(),
            title: "Shell MCP".to_string(),
            instruction: "调用外接 shell 能力。".to_string(),
            metadata: magi_skill_runtime::SkillMetadata {
                category: "ops".to_string(),
                tags: vec!["mcp".to_string()],
            },
            restrict_standard_tools: true,
            allowed_tools: vec![],
            custom_tool_bindings: vec![magi_skill_runtime::CustomToolBinding {
                binding_id: "shell-mcp-binding".to_string(),
                tool_name: "shell.run".to_string(),
                description: "运行 shell".to_string(),
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
        let safety_gate =
            magi_safety_gate::SafetyGate::new(vec![magi_safety_gate::SafetyRule::with_action(
                "rm -rf",
                magi_safety_gate::SafetyCategory::BulkDelete,
                magi_safety_gate::SafetyAction::HardBlock,
            )]);
        let event_bus = InMemoryEventBus::new(8);
        let call = ChatToolCall {
            id: "tool-call-mcp-safety".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "skill__shell-mcp__shell-mcp-binding".to_string(),
                arguments: serde_json::json!({ "payload": r#"{"command":"rm -rf /tmp/demo"}"# })
                    .to_string(),
            },
        };

        let (payload, status) = execute_session_turn_tool_call(
            SessionToolCallTestContext {
                session_store: &SessionStore::new(),
                event_bus: &event_bus,
                tool_registry: Some(&tool_registry),
                skill_runtime: Some(&skill_runtime),
                skill_dispatch_runtime: Some(&skill_dispatch_runtime),
                skill_name: Some("shell-mcp"),
                safety_gate: Some(&safety_gate),
                session_id: &SessionId::new("session-1"),
                workspace_id: &None,
                workspace_root_path: None,
                access_profile: magi_core::AccessProfile::FullAccess,
            },
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["status"], "rejected");
        assert_eq!(parsed["error_code"], "tool_safety_rejected");
        assert_eq!(parsed["error"], "该操作已被安全防护阻止");
        assert_eq!(parsed.get("safety_gate"), None);
        assert!(!payload.contains("rm -rf"));
        assert!(!payload.contains("bulk_delete"));
        assert!(!payload.contains("hard_block"));
        assert!(mcp_calls.lock().expect("mcp calls lock").is_empty());
    }

    #[test]
    fn execute_session_turn_tool_call_enforces_active_skill_allowed_tools() {
        let registry = SkillRegistry::new();
        registry.register(SkillDefinition {
            skill_id: "search-only".to_string(),
            title: "只允许搜索".to_string(),
            instruction: "只能检索文本。".to_string(),
            metadata: SkillMetadata {
                category: "quality".to_string(),
                tags: vec!["search".to_string()],
            },
            restrict_standard_tools: true,
            allowed_tools: vec!["search_text".to_string()],
            custom_tool_bindings: vec![],
            prompt_priority: 50,
        });
        let skill_runtime = SkillRuntime::new(registry);
        let mut tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_default_builtins();
        let event_bus = InMemoryEventBus::new(8);
        let call = ChatToolCall {
            id: "tool-call-file-read".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "file_read".to_string(),
                arguments: serde_json::json!({ "path": "Cargo.toml" }).to_string(),
            },
        };

        let (payload, status) = execute_session_turn_tool_call(
            SessionToolCallTestContext {
                session_store: &SessionStore::new(),
                event_bus: &event_bus,
                tool_registry: Some(&tool_registry),
                skill_runtime: Some(&skill_runtime),
                skill_dispatch_runtime: None,
                skill_name: Some("search-only"),
                safety_gate: None,
                session_id: &SessionId::new("session-1"),
                workspace_id: &None,
                workspace_root_path: None,
                access_profile: magi_core::AccessProfile::Restricted,
            },
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "file_read");
        assert_eq!(parsed["status"], "rejected");
        assert_eq!(parsed["error_code"], "tool_policy_rejected");
        assert_eq!(parsed["error"], "该工具在当前上下文中不可用");
        assert!(!payload.contains("search-only"));
        assert!(!payload.contains("skill runtime"));
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
            SessionToolCallTestContext {
                session_store: &SessionStore::new(),
                event_bus: &event_bus,
                tool_registry: Some(&tool_registry),
                skill_runtime: None,
                skill_dispatch_runtime: None,
                skill_name: None,
                safety_gate: None,
                session_id: &SessionId::new("session-1"),
                workspace_id: &None,
                workspace_root_path: None,
                access_profile: magi_core::AccessProfile::Restricted,
            },
            &call,
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
            SessionToolCallTestContext {
                session_store: &SessionStore::new(),
                event_bus: &event_bus,
                tool_registry: Some(&tool_registry),
                skill_runtime: None,
                skill_dispatch_runtime: None,
                skill_name: None,
                safety_gate: None,
                session_id: &SessionId::new("session-read-only-tool"),
                workspace_id: &None,
                workspace_root_path: Some(&root),
                access_profile: magi_core::AccessProfile::ReadOnly,
            },
            &call,
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
            SessionToolCallTestContext {
                session_store: &SessionStore::new(),
                event_bus: &event_bus,
                tool_registry: Some(&tool_registry),
                skill_runtime: None,
                skill_dispatch_runtime: None,
                skill_name: None,
                safety_gate: None,
                session_id: &SessionId::new("session-restricted-shell"),
                workspace_id: &None,
                workspace_root_path: Some(&root),
                access_profile: magi_core::AccessProfile::Restricted,
            },
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::NeedsApproval);
        assert!(
            !target.exists(),
            "受限模式下写类 shell 被拦截，不能提前执行"
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
            SessionToolCallTestContext {
                session_store: &SessionStore::new(),
                event_bus: &event_bus,
                tool_registry: Some(&tool_registry),
                skill_runtime: None,
                skill_dispatch_runtime: None,
                skill_name: None,
                safety_gate: None,
                session_id: &SessionId::new("session-read-only-shell"),
                workspace_id: &None,
                workspace_root_path: None,
                access_profile: magi_core::AccessProfile::ReadOnly,
            },
            &call,
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
            SessionToolCallTestContext {
                session_store: &SessionStore::new(),
                event_bus: &event_bus,
                tool_registry: Some(&tool_registry),
                skill_runtime: None,
                skill_dispatch_runtime: None,
                skill_name: None,
                safety_gate: Some(&safety_gate),
                session_id: &SessionId::new("session-1"),
                workspace_id: &None,
                workspace_root_path: None,
                access_profile: magi_core::AccessProfile::Restricted,
            },
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::NeedsApproval);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "shell_exec");
        assert_eq!(parsed["status"], "needs_approval");
        assert_eq!(parsed["error_code"], "tool_safety_needs_approval");
        assert_eq!(
            parsed["error"],
            "安全防护已在受限访问下拦截该操作，请切换为完全访问权限后重试"
        );
        assert_eq!(parsed.get("safety_gate"), None);
        assert!(!payload.contains("rm -rf"));
        assert!(!payload.contains("bulk_delete"));
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
            SessionToolCallTestContext {
                session_store: &SessionStore::new(),
                event_bus: &event_bus,
                tool_registry: Some(&tool_registry),
                skill_runtime: None,
                skill_dispatch_runtime: None,
                skill_name: None,
                safety_gate: Some(&safety_gate),
                session_id: &SessionId::new("session-1"),
                workspace_id: &None,
                workspace_root_path: None,
                access_profile: magi_core::AccessProfile::FullAccess,
            },
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["stdout"], "full-access-ok");
    }

    #[test]
    fn execute_session_turn_tool_call_requires_approval_for_file_remove_in_restricted_profile() {
        let mut tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_default_builtins();
        let event_bus = InMemoryEventBus::new(8);
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("probe.txt");
        std::fs::write(&target, "probe").expect("write probe");
        let call = ChatToolCall {
            id: "tool-call-file-remove".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "file_remove".to_string(),
                arguments: serde_json::json!({
                    "path": target.to_string_lossy()
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_session_turn_tool_call(
            SessionToolCallTestContext {
                session_store: &SessionStore::new(),
                event_bus: &event_bus,
                tool_registry: Some(&tool_registry),
                skill_runtime: None,
                skill_dispatch_runtime: None,
                skill_name: None,
                safety_gate: None,
                session_id: &SessionId::new("session-1"),
                workspace_id: &None,
                workspace_root_path: None,
                access_profile: magi_core::AccessProfile::Restricted,
            },
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::NeedsApproval);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "file_remove");
        assert_eq!(parsed["status"], "needs_approval");
        assert_eq!(parsed["error_code"], "tool_policy_needs_approval");
        assert!(target.exists(), "受限访问拦截的删除不能提前执行");
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
            SessionToolCallBatchTestContext {
                session_store: &session_store,
                event_bus: &event_bus,
                tool_registry: Some(&tool_registry),
                skill_runtime: None,
                skill_dispatch_runtime: None,
                skill_name: None,
                safety_gate: None,
                session_id: &session_id,
                workspace_id: &workspace_id,
                workspace_root_path: None,
                access_profile: magi_core::AccessProfile::Restricted,
                snapshot_session: None,
                execution_group_id: None,
                source_thread_id: &ThreadId::new("thread-shell-batch"),
                persist_session_state: None,
            },
            &tool_calls,
            &mut messages,
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
    fn session_goal_tools_write_goal_state_and_request_persistence() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(32);
        let session_id = SessionId::new("session-goal-tool-writeback");
        let workspace_id = Some(WorkspaceId::new("workspace-goal-tool-writeback"));
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "goal writeback session",
                workspace_id.as_ref().map(ToString::to_string),
            )
            .expect("session should be creatable");
        let (_, thread_id) =
            session_store.ensure_session_mission(&session_id, UtcMillis::now(), || {
                MissionId::new("mission-goal-tool-writeback")
            });
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-goal-tool-writeback".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    status: "running".to_string(),
                    user_message: Some("创建并完成目标".to_string()),
                    items: Vec::new(),
                    completed_at: None,
                },
            )
            .expect("turn should be creatable");
        let checkpoints = Arc::new(Mutex::new(Vec::<String>::new()));
        let checkpoints_for_callback = Arc::clone(&checkpoints);
        let persist = move |checkpoint: &str| {
            checkpoints_for_callback
                .lock()
                .expect("checkpoint lock")
                .push(checkpoint.to_string());
        };
        let mut messages = Vec::new();
        let create_call = ChatToolCall {
            id: "tool-call-create-goal".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::CreateGoal.as_str().to_string(),
                arguments: serde_json::json!({
                    "objective": "验证 goal 工具写回"
                })
                .to_string(),
            },
        };

        assert!(
            append_session_tool_call_items_batch(
                SessionToolCallBatchTestContext {
                    session_store: &session_store,
                    event_bus: &event_bus,
                    tool_registry: None,
                    skill_runtime: None,
                    skill_dispatch_runtime: None,
                    skill_name: None,
                    safety_gate: None,
                    session_id: &session_id,
                    workspace_id: &workspace_id,
                    workspace_root_path: None,
                    access_profile: magi_core::AccessProfile::Restricted,
                    snapshot_session: None,
                    execution_group_id: None,
                    source_thread_id: &thread_id,
                    persist_session_state: Some(&persist),
                },
                &[create_call],
                &mut messages,
                || true,
            )
            .completed
        );

        let created_goal = session_store
            .active_goal(&session_id)
            .expect("create_goal should create an active session goal");
        assert_eq!(created_goal.objective, "验证 goal 工具写回");
        assert_eq!(created_goal.token_budget, None);
        assert_eq!(session_store.durable_state().goals.len(), 1);
        assert!(
            checkpoints
                .lock()
                .expect("checkpoint lock")
                .iter()
                .any(|checkpoint| checkpoint == "session_goal_tool"),
            "create_goal 成功后必须立即请求 durable state 持久化"
        );

        let update_call = ChatToolCall {
            id: "tool-call-update-goal".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::UpdateGoal.as_str().to_string(),
                arguments: serde_json::json!({
                    "goal_id": created_goal.goal_id,
                    "status": "complete"
                })
                .to_string(),
            },
        };

        assert!(
            append_session_tool_call_items_batch(
                SessionToolCallBatchTestContext {
                    session_store: &session_store,
                    event_bus: &event_bus,
                    tool_registry: None,
                    skill_runtime: None,
                    skill_dispatch_runtime: None,
                    skill_name: None,
                    safety_gate: None,
                    session_id: &session_id,
                    workspace_id: &workspace_id,
                    workspace_root_path: None,
                    access_profile: magi_core::AccessProfile::Restricted,
                    snapshot_session: None,
                    execution_group_id: None,
                    source_thread_id: &thread_id,
                    persist_session_state: Some(&persist),
                },
                &[update_call],
                &mut messages,
                || true,
            )
            .completed
        );

        let updated_goal = session_store
            .current_goal(&session_id)
            .expect("update_goal should keep current goal readable");
        assert_eq!(
            updated_goal.status,
            magi_session_store::GoalStatus::Complete
        );
        assert_eq!(
            session_store.durable_state().goals[0].status,
            updated_goal.status
        );
        assert!(
            checkpoints
                .lock()
                .expect("checkpoint lock")
                .iter()
                .filter(|checkpoint| checkpoint.as_str() == "session_goal_tool")
                .count()
                >= 2,
            "create_goal 和 update_goal 成功后都必须请求 durable state 持久化"
        );
    }

    #[test]
    fn session_turn_todo_write_uses_shared_ledger_and_only_reports_success() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(32);
        let session_id = SessionId::new("session-mainline-todo-write");
        let workspace_id = Some(WorkspaceId::new("workspace-mainline-todo-write"));
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "mainline todo write",
                workspace_id.as_ref().map(ToString::to_string),
            )
            .expect("session should be creatable");
        let (mission_id, thread_id) =
            session_store.ensure_session_mission(&session_id, UtcMillis::now(), || {
                MissionId::new("mission-mainline-todo-write")
            });
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-mainline-todo-write".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    status: "running".to_string(),
                    user_message: Some("写入任务清单".to_string()),
                    items: Vec::new(),
                    completed_at: None,
                },
            )
            .expect("turn should be creatable");
        let todo_ledger = crate::test_todo_ledger("test-todo-ledger");
        let mut messages = Vec::new();
        let valid_call = ChatToolCall {
            id: "tool-call-mainline-todo-valid".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::TodoWrite.as_str().to_string(),
                arguments: serde_json::json!({
                    "todos": [
                        {"content": "完成第一项", "activeForm": "正在完成第一项", "status": "completed"},
                        {"content": "推进第二项", "activeForm": "正在推进第二项", "status": "in_progress"}
                    ]
                })
                .to_string(),
            },
        };

        let valid_outcome = append_session_tool_call_items_batch_with_context(
            SessionToolCallBatchContext {
                session_store: &session_store,
                event_bus: &event_bus,
                tool_registry: None,
                skill_runtime: None,
                skill_dispatch_runtime: None,
                skill_name: None,
                safety_gate: None,
                todo_ledger: &todo_ledger,
                mission_id: &mission_id,
                session_id: &session_id,
                workspace_id: &workspace_id,
                workspace_root_path: None,
                context_references: &[],
                access_profile: magi_core::AccessProfile::Restricted,
                snapshot_session: None,
                execution_group_id: None,
                source_thread_id: &thread_id,
                persist_session_state: None,
            },
            &[valid_call],
            &mut messages,
            || true,
        );

        assert!(valid_outcome.completed);
        assert_eq!(valid_outcome.succeeded_tool_names, vec!["todo_write"]);
        assert_eq!(todo_ledger.snapshot().len(), 2);

        let invalid_call = ChatToolCall {
            id: "tool-call-mainline-todo-invalid".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::TodoWrite.as_str().to_string(),
                arguments: serde_json::json!({
                    "todos": [
                        {"content": "错误状态", "activeForm": "正在写入错误状态", "status": "unknown"}
                    ]
                })
                .to_string(),
            },
        };
        let invalid_outcome = append_session_tool_call_items_batch_with_context(
            SessionToolCallBatchContext {
                session_store: &session_store,
                event_bus: &event_bus,
                tool_registry: None,
                skill_runtime: None,
                skill_dispatch_runtime: None,
                skill_name: None,
                safety_gate: None,
                todo_ledger: &todo_ledger,
                mission_id: &mission_id,
                session_id: &session_id,
                workspace_id: &workspace_id,
                workspace_root_path: None,
                context_references: &[],
                access_profile: magi_core::AccessProfile::Restricted,
                snapshot_session: None,
                execution_group_id: None,
                source_thread_id: &thread_id,
                persist_session_state: None,
            },
            &[invalid_call],
            &mut messages,
            || true,
        );

        assert!(invalid_outcome.completed);
        assert!(invalid_outcome.succeeded_tool_names.is_empty());
        assert_eq!(todo_ledger.snapshot().len(), 2);
    }

    #[test]
    fn session_tool_batch_reports_canonical_skill_activation_to_next_model_round() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(16);
        let session_id = SessionId::new("session-skill-activation");
        session_store
            .create_session(session_id.clone(), "skill activation")
            .expect("session should be creatable");
        let (mission_id, thread_id) =
            session_store.ensure_session_mission(&session_id, UtcMillis::now(), || {
                MissionId::new("mission-skill-activation")
            });
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-skill-activation".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    status: "running".to_string(),
                    user_message: Some("使用 code-review".to_string()),
                    items: Vec::new(),
                    completed_at: None,
                },
            )
            .expect("turn should be creatable");
        let skill_registry = SkillRegistry::new();
        skill_registry.register(SkillDefinition {
            skill_id: "owner/repo/skills/code-review".to_string(),
            title: "代码审查".to_string(),
            instruction: "检查稳定性。".to_string(),
            metadata: SkillMetadata {
                category: "quality".to_string(),
                tags: vec![],
            },
            restrict_standard_tools: true,
            allowed_tools: vec!["file_read".to_string()],
            custom_tool_bindings: vec![],
            prompt_priority: 50,
        });
        let skill_runtime = SkillRuntime::new(skill_registry);
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let todo_ledger = crate::test_todo_ledger("skill-activation-ledger");
        let mut messages = Vec::new();
        let call = ChatToolCall {
            id: "tool-call-skill-activation".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: SKILL_APPLY_TOOL_NAME.to_string(),
                arguments: serde_json::json!({ "skill_name": "code-review" }).to_string(),
            },
        };

        let outcome = append_session_tool_call_items_batch_with_context(
            SessionToolCallBatchContext {
                session_store: &session_store,
                event_bus: &event_bus,
                tool_registry: Some(&tool_registry),
                skill_runtime: Some(&skill_runtime),
                skill_dispatch_runtime: None,
                skill_name: None,
                safety_gate: None,
                todo_ledger: &todo_ledger,
                mission_id: &mission_id,
                session_id: &session_id,
                workspace_id: &None,
                workspace_root_path: None,
                context_references: &[],
                access_profile: magi_core::AccessProfile::Restricted,
                snapshot_session: None,
                execution_group_id: None,
                source_thread_id: &thread_id,
                persist_session_state: None,
            },
            &[call],
            &mut messages,
            || true,
        );

        assert_eq!(
            outcome.activated_skill_id.as_deref(),
            Some("owner/repo/skills/code-review")
        );
    }

    #[test]
    fn session_turn_approval_required_tool_is_terminal_error_item() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(32);
        let session_id = SessionId::new("session-turn-approval-tool");
        let workspace_id = Some(WorkspaceId::new("workspace-turn-approval-tool"));
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().to_path_buf();
        let target = root.join("approval-required.txt");
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "approval tool session",
                workspace_id.as_ref().map(ToString::to_string),
            )
            .expect("session should be creatable");
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-approval-tool".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    status: "running".to_string(),
                    user_message: Some("受限模式执行写入 shell".to_string()),
                    items: Vec::new(),
                    completed_at: None,
                },
            )
            .expect("turn should be creatable");

        let mut tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_default_builtins();
        let tool_calls = vec![ChatToolCall {
            id: "tool-call-approval-shell".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "shell_exec".to_string(),
                arguments: serde_json::json!({
                    "command": format!("printf approval > {}", target.display())
                })
                .to_string(),
            },
        }];
        let mut messages = Vec::new();

        append_session_tool_call_items_batch(
            SessionToolCallBatchTestContext {
                session_store: &session_store,
                event_bus: &event_bus,
                tool_registry: Some(&tool_registry),
                skill_runtime: None,
                skill_dispatch_runtime: None,
                skill_name: None,
                safety_gate: None,
                session_id: &session_id,
                workspace_id: &workspace_id,
                workspace_root_path: Some(root.clone()),
                access_profile: magi_core::AccessProfile::Restricted,
                snapshot_session: None,
                execution_group_id: None,
                source_thread_id: &ThreadId::new("thread-approval-tool"),
                persist_session_state: None,
            },
            &tool_calls,
            &mut messages,
            || true,
        );

        assert!(!target.exists(), "受限访问拦截的工具调用不能提前执行");
        let sidecar = session_store
            .runtime_sidecar(&session_id)
            .expect("sidecar should exist");
        let turn = sidecar.current_turn.expect("turn should exist");
        let item = turn.items.first().expect("tool item should exist");
        assert_eq!(item.status, "failed");
        assert_eq!(item.tool_status.as_deref(), Some("needs_approval"));
        assert!(item.tool_error.is_some(), "受限访问拦截必须作为错误槽写回");
        assert_eq!(
            messages
                .first()
                .and_then(|message| message.tool_call_id.as_deref()),
            Some("tool-call-approval-shell")
        );
        assert_eq!(
            messages
                .first()
                .and_then(|message| message.content.as_deref()),
            Some("受限访问已拦截该操作，请切换为完全访问权限后重试")
        );

        let canonical_turn = session_store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| turn.turn_id == "turn-approval-tool")
            .expect("canonical turn should be stored");
        let canonical_item = canonical_turn
            .items
            .first()
            .expect("canonical tool item should exist");
        assert_eq!(canonical_item.status, CanonicalTurnItemStatus::Failed);
        let canonical_tool = canonical_item.tool.as_ref().expect("canonical tool");
        assert!(canonical_tool.result.is_some());
        assert!(canonical_tool.error.is_some());
    }

    #[test]
    fn session_turn_serial_tool_panic_is_written_as_terminal_public_failure() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(32);
        let session_id = SessionId::new("session-turn-panic-tool");
        let workspace_id = Some(WorkspaceId::new("workspace-turn-panic-tool"));
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "panic tool session",
                workspace_id.as_ref().map(ToString::to_string),
            )
            .expect("session should be creatable");
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-panic-tool".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    status: "running".to_string(),
                    user_message: Some("执行会 panic 的串行工具".to_string()),
                    items: Vec::new(),
                    completed_at: None,
                },
            )
            .expect("turn should be creatable");

        let mut tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_builtin(Arc::new(PanicBuiltinTool {
            name: "unstable_tool",
        }));
        let tool_calls = vec![ChatToolCall {
            id: "tool-call-panic".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "unstable_tool".to_string(),
                arguments: "{}".to_string(),
            },
        }];
        let mut messages = Vec::new();

        assert!(
            append_session_tool_call_items_batch(
                SessionToolCallBatchTestContext {
                    session_store: &session_store,
                    event_bus: &event_bus,
                    tool_registry: Some(&tool_registry),
                    skill_runtime: None,
                    skill_dispatch_runtime: None,
                    skill_name: None,
                    safety_gate: None,
                    session_id: &session_id,
                    workspace_id: &workspace_id,
                    workspace_root_path: None,
                    access_profile: magi_core::AccessProfile::Restricted,
                    snapshot_session: None,
                    execution_group_id: None,
                    source_thread_id: &ThreadId::new("thread-panic-tool"),
                    persist_session_state: None,
                },
                &tool_calls,
                &mut messages,
                || true,
            )
            .completed
        );

        let sidecar = session_store
            .runtime_sidecar(&session_id)
            .expect("sidecar should exist");
        let turn = sidecar.current_turn.expect("turn should exist");
        assert_eq!(
            turn.items.len(),
            1,
            "running item must be replaced by result"
        );
        let item = turn.items.first().expect("tool item should exist");
        assert_eq!(item.status, "failed");
        assert_eq!(item.tool_status.as_deref(), Some("failed"));
        let public_payload = item
            .tool_result
            .as_deref()
            .expect("panic result should be written");
        let payload: serde_json::Value =
            serde_json::from_str(public_payload).expect("panic result should be json");
        assert_eq!(payload["status"], "failed");
        assert_eq!(payload["error_code"], "tool_execution_failed");
        assert_eq!(payload["error"], "工具执行失败，请稍后重试");
        assert!(!public_payload.contains("panic"));
        assert!(!public_payload.contains("线程"));
        assert_eq!(
            messages
                .first()
                .and_then(|message| message.tool_call_id.as_deref()),
            Some("tool-call-panic")
        );

        let canonical_turn = session_store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| turn.turn_id == "turn-panic-tool")
            .expect("canonical turn should be stored");
        let canonical_item = canonical_turn
            .items
            .first()
            .expect("canonical tool item should exist");
        assert_eq!(canonical_item.status, CanonicalTurnItemStatus::Failed);
        assert!(canonical_item.visibility.renderable);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_turn_concurrent_tool_batch_keeps_snapshot_context_during_execution() {
        let dir = tempfile::tempdir().expect("temp dir");
        let workspace_root = dir.path().to_path_buf();
        let snapshot = magi_snapshot::SnapshotManager::new()
            .start_session(
                "session-turn-concurrent-snapshot".to_string(),
                workspace_root.clone(),
            )
            .await
            .expect("snapshot session should start");

        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(32);
        let session_id = SessionId::new("session-turn-concurrent-snapshot");
        let workspace_id = Some(WorkspaceId::new("workspace-turn-concurrent-snapshot"));
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "snapshot batch session",
                workspace_id.as_ref().map(ToString::to_string),
            )
            .expect("session should be creatable");
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-concurrent-snapshot".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    status: "running".to_string(),
                    user_message: Some("并发工具快照归因".to_string()),
                    items: Vec::new(),
                    completed_at: None,
                },
            )
            .expect("turn should be creatable");

        let mut tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_builtin(Arc::new(SnapshotReconcileProbeTool::new(
            BuiltinToolName::ShellExec.as_str(),
            snapshot.clone(),
        )));
        let tool_calls = vec![
            ChatToolCall {
                id: "tool-call-session-snapshot-a".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: BuiltinToolName::ShellExec.as_str().to_string(),
                    arguments: serde_json::json!({
                        "command": "printf a",
                        "access_mode": "read_only",
                        "changed_paths": ["session-a.txt"]
                    })
                    .to_string(),
                },
            },
            ChatToolCall {
                id: "tool-call-session-snapshot-b".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: BuiltinToolName::ShellExec.as_str().to_string(),
                    arguments: serde_json::json!({
                        "command": "printf b",
                        "access_mode": "read_only",
                        "changed_paths": ["session-b.txt"]
                    })
                    .to_string(),
                },
            },
        ];
        let mut messages = Vec::new();

        append_session_tool_call_items_batch(
            SessionToolCallBatchTestContext {
                session_store: &session_store,
                event_bus: &event_bus,
                tool_registry: Some(&tool_registry),
                skill_runtime: None,
                skill_dispatch_runtime: None,
                skill_name: None,
                safety_gate: None,
                session_id: &session_id,
                workspace_id: &workspace_id,
                workspace_root_path: Some(workspace_root.clone()),
                access_profile: magi_core::AccessProfile::FullAccess,
                snapshot_session: Some(snapshot.clone()),
                execution_group_id: Some("session-turn-group".to_string()),
                source_thread_id: &ThreadId::new("thread-session-snapshot"),
                persist_session_state: None,
            },
            &tool_calls,
            &mut messages,
            || true,
        );

        assert_eq!(
            messages
                .iter()
                .map(|message| message.tool_call_id.as_deref())
                .collect::<Vec<_>>(),
            vec![
                Some("tool-call-session-snapshot-a"),
                Some("tool-call-session-snapshot-b")
            ]
        );
        let pending = snapshot.pending_changes().expect("pending changes");
        for (path, call_id) in [
            ("session-a.txt", "tool-call-session-snapshot-a"),
            ("session-b.txt", "tool-call-session-snapshot-b"),
        ] {
            let change = pending
                .iter()
                .find(|change| change.path == path)
                .expect("session turn concurrent tool change should be tracked");
            assert_eq!(change.source, magi_snapshot::SourceKind::Tool);
            assert_eq!(change.tool_call_id.as_deref(), Some(call_id));
            assert_eq!(change.worker_id, None);
            assert_eq!(
                change.execution_group_id.as_deref(),
                Some("session-turn-group")
            );
        }
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
