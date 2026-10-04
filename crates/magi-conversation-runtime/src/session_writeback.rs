use crate::context_authority::{ContextCompactionProgress, ContextCompactionRecord};
use crate::tool_result_utils::{summarize_tool_result, turn_item_status_for_tool_result};
use crate::turn_contract::{TurnEventEnvelope, TurnRecord};
#[cfg(test)]
use crate::{
    CoordinatorAdmission, CoordinatorCommandResult, CoordinatorTurnStatus, ExecutionProfile,
    SessionTurnCoordinator, TurnAdmission, TurnCommand,
};
use crate::{TaskTurnVisibility, apply_task_worker_detail_visibility};
use magi_bridge_client::{ModelRetryRuntimeEvent, ModelRetryRuntimePhase};
use magi_core::{
    EventId, ExecutionResultStatus, SessionId, TaskId, ThreadId, UtcMillis, WorkspaceId,
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
    TimelineEntryInput, active_execution_turn_request_id,
};
use magi_tool_runtime::BuiltinToolName;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
};

pub type SessionStatePersistCallback = dyn Fn(&str) -> Result<(), String> + Send + Sync;
static CONTEXT_COMPACTION_ITEM_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
pub(crate) struct ContextCompactionWritebackContext<'a> {
    pub(crate) event_bus: &'a InMemoryEventBus,
    pub(crate) session_store: &'a SessionStore,
    pub(crate) session_id: &'a SessionId,
    pub(crate) workspace_id: &'a Option<WorkspaceId>,
    pub(crate) thread_id: &'a ThreadId,
    pub(crate) item_id: &'a str,
    pub(crate) phase: &'static str,
    pub(crate) persist_session_state: Option<&'a SessionStatePersistCallback>,
    pub(crate) task: Option<&'a magi_core::Task>,
    pub(crate) turn_visibility: Option<&'a TaskTurnVisibility>,
    pub(crate) expected_turn_id: Option<&'a str>,
}

pub(crate) fn new_context_compaction_item_id(
    owner_id: &str,
    thread_id: &ThreadId,
    phase: &str,
) -> String {
    format!(
        "turn-item-context-compaction-{owner_id}-{thread_id}-{phase}-{}-{}",
        UtcMillis::now().0,
        CONTEXT_COMPACTION_ITEM_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

pub(crate) fn upsert_context_compaction_progress_notice(
    context: ContextCompactionWritebackContext<'_>,
    progress: ContextCompactionProgress,
) -> Result<(), String> {
    let (status, state, notice_type, stage, completed_chunks, total_chunks) = match progress {
        ContextCompactionProgress::Started {
            stage,
            total_chunks,
        } => (
            "running",
            "running",
            "info",
            Some(stage),
            Some(0),
            Some(total_chunks),
        ),
        ContextCompactionProgress::Advanced {
            stage,
            completed_chunks,
            total_chunks,
        } => (
            "running",
            "running",
            "info",
            Some(stage),
            Some(completed_chunks),
            Some(total_chunks),
        ),
        ContextCompactionProgress::Skipped => ("completed", "skipped", "info", None, None, None),
        ContextCompactionProgress::Cancelled => {
            ("cancelled", "cancelled", "info", None, None, None)
        }
        ContextCompactionProgress::Deferred => {
            ("completed", "deferred", "warning", None, None, None)
        }
        ContextCompactionProgress::Failed => ("failed", "failed", "warning", None, None, None),
    };
    let mut item = session_turn_item(
        "assistant_phase",
        status,
        Some("Context compaction".to_string()),
        Some("Context compaction".to_string()),
        Some(context.item_id.to_string()),
        context.thread_id.clone(),
    );
    if let (Some(task), Some(turn_visibility)) = (context.task, context.turn_visibility) {
        apply_task_worker_detail_visibility(&mut item, task, turn_visibility);
    }
    item.metadata.insert(
        "noticeKind".to_string(),
        serde_json::Value::String("context_compaction".to_string()),
    );
    item.metadata.insert(
        "noticeType".to_string(),
        serde_json::Value::String(notice_type.to_string()),
    );
    item.metadata.insert(
        "compactionState".to_string(),
        serde_json::Value::String(state.to_string()),
    );
    item.metadata.insert(
        "phase".to_string(),
        serde_json::Value::String(context.phase.to_string()),
    );
    if let Some(stage) = stage {
        item.metadata.insert(
            "compactionStage".to_string(),
            serde_json::Value::String(stage.to_string()),
        );
    }
    if let Some(completed_chunks) = completed_chunks {
        item.metadata.insert(
            "completedChunks".to_string(),
            serde_json::json!(completed_chunks),
        );
    }
    if let Some(total_chunks) = total_chunks {
        item.metadata
            .insert("totalChunks".to_string(), serde_json::json!(total_chunks));
    }
    match upsert_session_turn_item_for_turn(
        context.session_store,
        context.session_id,
        context.expected_turn_id,
        item,
        None,
    ) {
        Ok(Some(published)) => {
            persist_session_state_checkpoint(
                context.persist_session_state,
                "context_compaction_notice",
            )?;
            publish_session_turn_item_event(
                context.event_bus,
                context.session_id,
                context.workspace_id,
                &published,
            );
            Ok(())
        }
        Ok(None) => Err(format!(
            "会话 {} 没有可写入上下文压缩进度的当前 Turn",
            context.session_id
        )),
        Err(error) => Err(format!("上下文压缩进度事实写回失败：{error}")),
    }
}

pub(crate) fn upsert_context_compaction_completed_notice(
    context: ContextCompactionWritebackContext<'_>,
    record: &ContextCompactionRecord,
) -> Result<(), String> {
    let mut item = session_turn_item(
        "assistant_phase",
        "completed",
        Some("Context compacted".to_string()),
        Some("Context compacted".to_string()),
        Some(context.item_id.to_string()),
        context.thread_id.clone(),
    );
    if let (Some(task), Some(turn_visibility)) = (context.task, context.turn_visibility) {
        apply_task_worker_detail_visibility(&mut item, task, turn_visibility);
    }
    item.metadata.insert(
        "noticeKind".to_string(),
        serde_json::Value::String("context_compaction".to_string()),
    );
    item.metadata.insert(
        "noticeType".to_string(),
        serde_json::Value::String("info".to_string()),
    );
    item.metadata.insert(
        "compactionState".to_string(),
        serde_json::Value::String("completed".to_string()),
    );
    item.metadata.insert(
        "phase".to_string(),
        serde_json::Value::String(context.phase.to_string()),
    );
    item.metadata.insert(
        "reason".to_string(),
        serde_json::Value::String(record.reason.to_string()),
    );
    item.metadata.insert(
        "originalMessageCount".to_string(),
        serde_json::json!(record.original_message_count),
    );
    item.metadata.insert(
        "compactedMessageCount".to_string(),
        serde_json::json!(record.compacted_message_count),
    );
    item.metadata.insert(
        "originalTokenEstimate".to_string(),
        serde_json::json!(record.original_token_estimate),
    );
    item.metadata.insert(
        "compactedTokenEstimate".to_string(),
        serde_json::json!(record.compacted_token_estimate),
    );
    item.metadata.insert(
        "compactedAt".to_string(),
        serde_json::json!(record.compacted_at.0),
    );
    match upsert_session_turn_item_for_turn(
        context.session_store,
        context.session_id,
        context.expected_turn_id,
        item,
        None,
    ) {
        Ok(Some(published)) => {
            persist_session_state_checkpoint(
                context.persist_session_state,
                "context_compaction_notice",
            )?;
            publish_session_turn_item_event(
                context.event_bus,
                context.session_id,
                context.workspace_id,
                &published,
            );
            Ok(())
        }
        Ok(None) => Err(format!(
            "会话 {} 没有可写入上下文压缩完成事实的当前 Turn",
            context.session_id
        )),
        Err(error) => Err(format!("上下文压缩完成事实写回失败：{error}")),
    }
}

/// 同一模型调用轮次的思考与正文必须共享这个稳定关联键。
/// 呈现层据此只在同一轮内重排思考与正文，不能把后续轮次错误并入前一段思考。
pub const MODEL_RESPONSE_ROUND_METADATA_KEY: &str = "modelRound";

pub fn apply_model_response_round(item: &mut ActiveExecutionTurnItem, round: usize) {
    item.metadata.insert(
        MODEL_RESPONSE_ROUND_METADATA_KEY.to_string(),
        Value::from(round as u64),
    );
}

pub fn persist_session_state_checkpoint(
    callback: Option<&SessionStatePersistCallback>,
    checkpoint: &'static str,
) -> Result<(), String> {
    callback.map_or(Ok(()), |callback| callback(checkpoint))
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

const STREAM_ITEM_PUBLISH_MIN_INTERVAL_MS: u64 = 80;
const STREAM_ITEM_PUBLISH_MIN_CHARS: usize = 24;

/// 流式发布节流闸门：只决定“这一次要不要发布”以及相对上次发布的增量。
///
/// 事件携带的 itemVersion 必须是 SessionStore 里该 item 的版本——它也是快照、
/// 整条 upsert 和恢复使用的版本。闸门若自己计数，会因为节流跳过的刷新而落后于
/// 存储版本，客户端从快照恢复后就会把后续增量全部当成旧版本丢弃。
#[derive(Clone, Debug, Default)]
pub struct SessionTurnStreamPublishGate {
    last_published_at: Option<UtcMillis>,
    last_published_content_length: usize,
    last_published_content: String,
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

    /// 返回 `(是否为第一帧, 相对上次发布的增量)`；第一帧需要附带完整 item。
    fn prepare_publish_at(
        &mut self,
        candidate: &SessionTurnStreamUpdate,
        current_content: &str,
        now: UtcMillis,
    ) -> Option<(bool, SessionTurnStreamUpdate)> {
        if !self.should_publish_at(candidate, now) {
            return None;
        }
        let update = session_turn_stream_update(&self.last_published_content, current_content)?;
        let first_frame = self.last_published_at.is_none();
        self.last_published_at = Some(now);
        self.last_published_content_length = update.content_length;
        self.last_published_content.clear();
        self.last_published_content.push_str(current_content);
        Some((first_frame, update))
    }

    fn prepare_publish(
        &mut self,
        candidate: &SessionTurnStreamUpdate,
        current_content: &str,
    ) -> Option<(bool, SessionTurnStreamUpdate)> {
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
    session_store: &SessionStore,
    sidecar: SessionRuntimeSidecar,
    item_id: &str,
    task_store: Option<&TaskStore>,
) -> Result<Option<PublishedSessionTurnItem>, String> {
    let Some(turn) = sidecar.current_turn.as_ref() else {
        return Ok(None);
    };
    let item = turn
        .items
        .iter()
        .find(|candidate| candidate.item_id == item_id)
        .ok_or_else(|| {
            format!(
                "会话 {} 的 Turn {} 已写入 item {}，但无法从当前 Turn 生成发布快照",
                sidecar.session_id, turn.turn_id, item_id
            )
        })
        .cloned()?;
    let chain = chain_for_turn_summary(&sidecar, turn);
    let response_duration_ms = turn
        .completed_at
        .map(|completed_at| completed_at.0.saturating_sub(turn.accepted_at.0));
    let canonical_turn = session_store
        .canonical_turns_for_session(&sidecar.session_id)
        .into_iter()
        .find(|canonical| canonical.turn_id == turn.turn_id)
        .or_else(|| to_canonical_turn(&sidecar.session_id, turn))
        .ok_or_else(|| {
            format!(
                "会话 {} 的 Turn {} 无法转换为 canonical 发布快照",
                sidecar.session_id, turn.turn_id
            )
        })?;
    let canonical_item = canonical_turn
        .items
        .iter()
        .find(|candidate| candidate.item_id == item.item_id)
        .cloned()
        .or_else(|| to_canonical_turn_item(&sidecar.session_id, turn, &item))
        .ok_or_else(|| {
            format!(
                "会话 {} 的 Turn {} item {} 无法转换为 canonical item",
                sidecar.session_id, turn.turn_id, item_id
            )
        })?;
    Ok(Some(PublishedSessionTurnItem {
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
        canonical_turn: Some(canonical_turn),
        canonical_item: Some(canonical_item),
    }))
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
        "interrupted" => Some(CanonicalTurnStatus::Interrupted),
        "cancelled" | "canceled" | "killed" => Some(CanonicalTurnStatus::Cancelled),
        "superseded" => Some(CanonicalTurnStatus::Superseded),
        _ => None,
    }
}

fn canonical_item_status(status: &str) -> Option<CanonicalTurnItemStatus> {
    if status.trim().eq_ignore_ascii_case("indeterminate") {
        return Some(CanonicalTurnItemStatus::Indeterminate);
    }
    match canonical_turn_status(status)? {
        CanonicalTurnStatus::Pending => Some(CanonicalTurnItemStatus::Pending),
        CanonicalTurnStatus::Running => Some(CanonicalTurnItemStatus::Running),
        CanonicalTurnStatus::Completed => Some(CanonicalTurnItemStatus::Completed),
        CanonicalTurnStatus::Blocked => Some(CanonicalTurnItemStatus::Blocked),
        CanonicalTurnStatus::Failed => Some(CanonicalTurnItemStatus::Failed),
        CanonicalTurnStatus::Interrupted => Some(CanonicalTurnItemStatus::Cancelled),
        CanonicalTurnStatus::Cancelled => Some(CanonicalTurnItemStatus::Cancelled),
        CanonicalTurnStatus::Superseded => Some(CanonicalTurnItemStatus::Cancelled),
    }
}

fn terminal_item_status_for_turn_status(
    status: CanonicalTurnStatus,
) -> Option<CanonicalTurnItemStatus> {
    match status {
        CanonicalTurnStatus::Completed => Some(CanonicalTurnItemStatus::Completed),
        CanonicalTurnStatus::Blocked => Some(CanonicalTurnItemStatus::Blocked),
        CanonicalTurnStatus::Failed => Some(CanonicalTurnItemStatus::Failed),
        CanonicalTurnStatus::Interrupted => Some(CanonicalTurnItemStatus::Cancelled),
        CanonicalTurnStatus::Cancelled => Some(CanonicalTurnItemStatus::Cancelled),
        CanonicalTurnStatus::Superseded => Some(CanonicalTurnItemStatus::Cancelled),
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
    if let Some(output_kind) = match item.kind.as_str() {
        "assistant_stream" => Some("progress"),
        "assistant_final" => Some("final"),
        "assistant_error" => Some("error"),
        _ => None,
    } {
        metadata.insert(
            "assistantOutputKind".to_string(),
            Value::String(output_kind.to_string()),
        );
    }
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
        // 与 SessionStore 的 canonical 写模型保持一致：新 item 的事实版本从 1 开始。
        item_version: Some(1),
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
    let mut metadata = HashMap::new();
    if let Some(request_id) = active_execution_turn_request_id(turn) {
        metadata.insert("requestId".to_string(), Value::String(request_id));
    }
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
        metadata,
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

/// Turn 事实写回的唯一边界。
///
/// `SessionStore` 负责 canonical durable mutation，`InMemoryEventBus` 只负责在
/// mutation 成功后发布投影通知。这个对象不拥有任何业务状态，因此可以按一次
/// command 创建，避免把 SessionStore 的可变事实复制到执行器中。
pub struct CanonicalTurnEventSink<'a> {
    session_store: Option<&'a SessionStore>,
    event_bus: Option<&'a InMemoryEventBus>,
    task_store: Option<&'a TaskStore>,
}

impl<'a> CanonicalTurnEventSink<'a> {
    pub fn new(
        session_store: &'a SessionStore,
        event_bus: &'a InMemoryEventBus,
        task_store: Option<&'a TaskStore>,
    ) -> Self {
        Self {
            session_store: Some(session_store),
            event_bus: Some(event_bus),
            task_store,
        }
    }

    pub fn for_store(session_store: &'a SessionStore, task_store: Option<&'a TaskStore>) -> Self {
        Self {
            session_store: Some(session_store),
            event_bus: None,
            task_store,
        }
    }

    pub fn for_events(event_bus: &'a InMemoryEventBus) -> Self {
        Self {
            session_store: None,
            event_bus: Some(event_bus),
            task_store: None,
        }
    }

    pub fn envelope_for_item(
        &self,
        published: &PublishedSessionTurnItem,
        event_sequence: u64,
    ) -> Option<TurnEventEnvelope> {
        let turn = published
            .canonical_turn
            .as_ref()
            .and_then(|turn| TurnRecord::from_canonical(turn, event_sequence));
        let canonical_item = published.canonical_item.clone();
        let turn_record = turn.as_ref()?;
        Some(TurnEventEnvelope {
            event_id: format!("session-turn-item-{}-{}", published.turn_id, event_sequence),
            event_type: "session.turn.item".to_string(),
            event_sequence,
            session_id: turn_record.session_id.clone(),
            turn_id: turn_record.turn_id.clone(),
            turn_seq: turn_record.turn_seq,
            occurred_at: UtcMillis::now(),
            causation_id: turn_record.causation_id.clone(),
            trace_id: turn_record.trace_id.clone(),
            item_version: canonical_item.as_ref().and_then(|item| item.item_version),
            base_content_length: None,
            content_length: canonical_item
                .as_ref()
                .and_then(|item| item.content.as_deref())
                .map(|content| content.chars().count()),
            delta: None,
            reset: None,
            turn: Some(turn_record.clone()),
            task_run: None,
            item: canonical_item,
        })
    }
}

impl<'a> CanonicalTurnEventSink<'a> {
    pub fn append_item(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
        item: ActiveExecutionTurnItem,
    ) -> Result<Option<PublishedSessionTurnItem>, String> {
        append_session_turn_item_for_turn_raw(
            self.session_store
                .ok_or_else(|| "CanonicalTurnEventSink 缺少 SessionStore".to_string())?,
            session_id,
            expected_turn_id,
            item,
            self.task_store,
        )
    }

    pub fn upsert_item(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
        item: ActiveExecutionTurnItem,
    ) -> Result<Option<PublishedSessionTurnItem>, String> {
        upsert_session_turn_item_for_turn_raw(
            self.session_store
                .ok_or_else(|| "CanonicalTurnEventSink 缺少 SessionStore".to_string())?,
            session_id,
            expected_turn_id,
            item,
            self.task_store,
        )
    }

    pub fn set_status(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
        status: &str,
    ) -> Result<Option<(SessionRuntimeSidecar, bool)>, String> {
        self.set_status_domain(session_id, expected_turn_id, status)
            .map_err(|error| error.to_string())
    }

    pub fn publish_item(
        &self,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        published: &PublishedSessionTurnItem,
    ) -> u64 {
        if let Some(event_bus) = self.event_bus {
            return publish_session_turn_item_event_raw(
                event_bus,
                session_id,
                workspace_id,
                published,
            );
        }
        0
    }

    pub fn publish_stream(
        &self,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        published: &PublishedSessionTurnItem,
        stream_update: &SessionTurnStreamUpdate,
        publish_gate: &mut SessionTurnStreamPublishGate,
    ) -> Option<u64> {
        if let Some(event_bus) = self.event_bus {
            return publish_session_turn_item_stream_event_raw(
                event_bus,
                session_id,
                workspace_id,
                published,
                stream_update,
                publish_gate,
            );
        }
        None
    }

    /// 需要保留 SessionStore 原始 DomainError 的边界（例如 API 要把
    /// CurrentTurnConflict 映射为 409）时使用这些方法。它们仍通过当前
    /// `CanonicalTurnEventSink` 对象执行 canonical mutation。
    pub fn append_item_sidecar(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
        item: ActiveExecutionTurnItem,
    ) -> magi_core::DomainResult<Option<SessionRuntimeSidecar>> {
        self.session_store
            .ok_or_else(|| magi_core::DomainError::InvalidState {
                message: "CanonicalTurnEventSink 缺少 SessionStore".to_string(),
            })?
            .append_current_turn_item_for_turn(session_id, expected_turn_id, item)
    }

    pub fn upsert_item_sidecar(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
        item: ActiveExecutionTurnItem,
    ) -> magi_core::DomainResult<Option<SessionRuntimeSidecar>> {
        self.session_store
            .ok_or_else(|| magi_core::DomainError::InvalidState {
                message: "CanonicalTurnEventSink 缺少 SessionStore".to_string(),
            })?
            .upsert_current_turn_item_for_turn(session_id, expected_turn_id, item)
    }

    pub fn set_status_domain(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
        status: &str,
    ) -> magi_core::DomainResult<Option<(SessionRuntimeSidecar, bool)>> {
        self.session_store
            .ok_or_else(|| magi_core::DomainError::InvalidState {
                message: "CanonicalTurnEventSink 缺少 SessionStore".to_string(),
            })?
            .update_current_turn_status_for_turn_with_change(session_id, expected_turn_id, status)
    }

    /// 接纳普通 Conversation Turn 的 canonical 事实。
    pub fn accept_conversation_turn_with_timeline_entry(
        &self,
        session_id: SessionId,
        workspace_id: Option<WorkspaceId>,
        timeline_entry: TimelineEntryInput,
        turn: ActiveExecutionTurn,
    ) -> magi_core::DomainResult<(String, SessionRuntimeSidecar, CanonicalTurn)> {
        self.session_store
            .ok_or_else(|| magi_core::DomainError::InvalidState {
                message: "CanonicalTurnEventSink 缺少 SessionStore".to_string(),
            })?
            .accept_conversation_turn_with_timeline_entry(
                session_id,
                workspace_id,
                timeline_entry,
                turn,
            )
    }

    /// 接纳带有 Task execution chain 的 Turn，并把 Task 关联写进同一笔 canonical 事务。
    pub fn accept_active_execution_chain_with_timeline_entry_and_task(
        &self,
        session_id: SessionId,
        timeline_entry: TimelineEntryInput,
        active_execution_chain: ActiveExecutionChain,
        task: &magi_core::Task,
    ) -> magi_core::DomainResult<(String, SessionRuntimeSidecar, Option<CanonicalTurn>)> {
        self.session_store
            .ok_or_else(|| magi_core::DomainError::InvalidState {
                message: "CanonicalTurnEventSink 缺少 SessionStore".to_string(),
            })?
            .accept_active_execution_chain_with_timeline_entry_and_task(
                session_id,
                timeline_entry,
                active_execution_chain,
                task,
            )
    }

    /// 接纳 Goal continuation Turn，并保留 Goal continuation 的 canonical 关联。
    pub fn accept_goal_continuation_with_timeline_entry_and_task(
        &self,
        session_id: SessionId,
        goal_id: &magi_core::GoalId,
        timeline_entry: TimelineEntryInput,
        active_execution_chain: ActiveExecutionChain,
        task: &magi_core::Task,
    ) -> magi_core::DomainResult<(String, SessionRuntimeSidecar, Option<CanonicalTurn>)> {
        self.session_store
            .ok_or_else(|| magi_core::DomainError::InvalidState {
                message: "CanonicalTurnEventSink 缺少 SessionStore".to_string(),
            })?
            .accept_goal_continuation_with_timeline_entry_and_task(
                session_id,
                goal_id,
                timeline_entry,
                active_execution_chain,
                task,
            )
    }

    /// 原子替换旧 Turn 并接纳新的 Task execution chain。
    pub fn replace_current_turn_with_active_execution_chain_and_timeline_entry_and_task(
        &self,
        session_id: SessionId,
        replace_turn_id: &str,
        timeline_entry: TimelineEntryInput,
        active_execution_chain: ActiveExecutionChain,
        task: &magi_core::Task,
    ) -> magi_core::DomainResult<(String, SessionRuntimeSidecar, CanonicalTurn, CanonicalTurn)>
    {
        self.session_store
            .ok_or_else(|| magi_core::DomainError::InvalidState {
                message: "CanonicalTurnEventSink 缺少 SessionStore".to_string(),
            })?
            .replace_current_turn_with_active_execution_chain_and_timeline_entry_and_task(
                session_id,
                replace_turn_id,
                timeline_entry,
                active_execution_chain,
                task,
            )
    }

    /// Continue 创建新 Turn 前收口旧 current Turn，保持 replacement 归属校验集中在 sink。
    pub fn finalize_turn_for_continue(
        &self,
        session_id: &SessionId,
        expected_turn_id: &str,
    ) -> magi_core::DomainResult<Option<SessionRuntimeSidecar>> {
        self.session_store
            .ok_or_else(|| magi_core::DomainError::InvalidState {
                message: "CanonicalTurnEventSink 缺少 SessionStore".to_string(),
            })?
            .finalize_current_turn_for_continue(session_id, expected_turn_id)
    }

    /// 接纳 Continue 的普通 canonical Turn。
    pub fn accept_turn_with_timeline_entry(
        &self,
        session_id: SessionId,
        timeline_entry: TimelineEntryInput,
        turn: ActiveExecutionTurn,
    ) -> magi_core::DomainResult<(String, SessionRuntimeSidecar)> {
        self.session_store
            .ok_or_else(|| magi_core::DomainError::InvalidState {
                message: "CanonicalTurnEventSink 缺少 SessionStore".to_string(),
            })?
            .accept_current_turn_with_timeline_entry(session_id, timeline_entry, turn)
    }

    pub fn cancel_turn(
        &self,
        session_id: &SessionId,
    ) -> magi_core::DomainResult<Option<SessionRuntimeSidecar>> {
        self.session_store
            .ok_or_else(|| magi_core::DomainError::InvalidState {
                message: "CanonicalTurnEventSink 缺少 SessionStore".to_string(),
            })?
            .cancel_current_turn(session_id)
    }

    /// 按用户来源中断当前 Turn，并保留 canonical interruption metadata。
    ///
    /// 用户取消和内部资源清理都结束 Turn，但前者还必须写入
    /// `interruptionSource=user` 与时间戳，供恢复路由和审计区分来源。
    pub fn interrupt_turn_by_user(
        &self,
        session_id: &SessionId,
    ) -> magi_core::DomainResult<Option<SessionRuntimeSidecar>> {
        self.session_store
            .ok_or_else(|| magi_core::DomainError::InvalidState {
                message: "CanonicalTurnEventSink 缺少 SessionStore".to_string(),
            })?
            .interrupt_current_turn_by_user(session_id)
    }

    /// 将 daemon 重启遗留的执行 Turn 收口为可恢复的中断事实。
    pub fn interrupt_turn_by_daemon_restart(
        &self,
        session_id: &SessionId,
    ) -> magi_core::DomainResult<Option<SessionRuntimeSidecar>> {
        self.session_store
            .ok_or_else(|| magi_core::DomainError::InvalidState {
                message: "CanonicalTurnEventSink 缺少 SessionStore".to_string(),
            })?
            .interrupt_current_turn_by_daemon_restart(session_id)
    }

    pub fn complete_from_root_task(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
    ) -> magi_core::DomainResult<Option<SessionRuntimeSidecar>> {
        self.session_store
            .ok_or_else(|| magi_core::DomainError::InvalidState {
                message: "CanonicalTurnEventSink 缺少 SessionStore".to_string(),
            })?
            .complete_current_turn_from_completed_root_task_for_turn(session_id, expected_turn_id)
    }

    pub fn complete_from_root_task_with_change(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
    ) -> magi_core::DomainResult<Option<(SessionRuntimeSidecar, bool)>> {
        self.session_store
            .ok_or_else(|| magi_core::DomainError::InvalidState {
                message: "CanonicalTurnEventSink 缺少 SessionStore".to_string(),
            })?
            .complete_current_turn_from_completed_root_task_for_turn_with_change(
                session_id,
                expected_turn_id,
            )
    }
}

pub fn append_session_turn_item_for_turn(
    session_store: &SessionStore,
    session_id: &SessionId,
    expected_turn_id: Option<&str>,
    item: ActiveExecutionTurnItem,
    task_store: Option<&TaskStore>,
) -> Result<Option<PublishedSessionTurnItem>, String> {
    CanonicalTurnEventSink::for_store(session_store, task_store).append_item(
        session_id,
        expected_turn_id,
        item,
    )
}

pub fn upsert_session_turn_item_for_turn(
    session_store: &SessionStore,
    session_id: &SessionId,
    expected_turn_id: Option<&str>,
    item: ActiveExecutionTurnItem,
    task_store: Option<&TaskStore>,
) -> Result<Option<PublishedSessionTurnItem>, String> {
    CanonicalTurnEventSink::for_store(session_store, task_store).upsert_item(
        session_id,
        expected_turn_id,
        item,
    )
}

fn append_session_turn_item_for_turn_raw(
    session_store: &SessionStore,
    session_id: &SessionId,
    expected_turn_id: Option<&str>,
    item: ActiveExecutionTurnItem,
    task_store: Option<&TaskStore>,
) -> Result<Option<PublishedSessionTurnItem>, String> {
    let item_id = item.item_id.clone();
    let sidecar = session_store
        .append_current_turn_item_for_turn(session_id, expected_turn_id, item)
        .map_err(|error| error.to_string())?;
    let Some(sidecar) = sidecar else {
        return Ok(None);
    };
    published_session_turn_item_from_sidecar(session_store, sidecar, &item_id, task_store)
}

fn upsert_session_turn_item_for_turn_raw(
    session_store: &SessionStore,
    session_id: &SessionId,
    expected_turn_id: Option<&str>,
    item: ActiveExecutionTurnItem,
    task_store: Option<&TaskStore>,
) -> Result<Option<PublishedSessionTurnItem>, String> {
    let item_id = item.item_id.clone();
    let sidecar = session_store
        .upsert_current_turn_item_for_turn(session_id, expected_turn_id, item)
        .map_err(|error| error.to_string())?;
    let Some(sidecar) = sidecar else {
        return Ok(None);
    };
    published_session_turn_item_from_sidecar(session_store, sidecar, &item_id, task_store)
}

pub fn publish_session_turn_item_event(
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    published: &PublishedSessionTurnItem,
) {
    let event_sequence = CanonicalTurnEventSink::for_events(event_bus).publish_item(
        session_id,
        workspace_id,
        published,
    );
    record_event_bus_item_timing(session_id, published, event_sequence);
}

fn publish_session_turn_item_event_raw(
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    published: &PublishedSessionTurnItem,
) -> u64 {
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
    publish_session_turn_item_payload(event_bus, session_id, workspace_id, payload)
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
    event_bus.publish(
        EventEnvelope::domain(
            EventId::unique(format!("model-retry-runtime-{}", message_id)),
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
    let event_sequence = CanonicalTurnEventSink::for_events(event_bus).publish_stream(
        session_id,
        workspace_id,
        published,
        stream_update,
        publish_gate,
    );
    if let Some(event_sequence) = event_sequence {
        record_event_bus_item_timing(session_id, published, event_sequence);
    }
}

/// 记录模型/工具产生的第一类 session.turn.item 发布事实。
///
/// accepted 用户 item 也会经过同一 EventBus，但它属于接纳阶段；这里跳过 user
/// source，只记录执行面产生的 item，供 Electron timing 证据按 turn_id 关联。
fn record_event_bus_item_timing(
    session_id: &SessionId,
    published: &PublishedSessionTurnItem,
    event_sequence: u64,
) {
    if published.item.source == "user" || published.item.kind == "user_message" {
        return;
    }
    let trace_id = published
        .item
        .request_id
        .as_deref()
        .or_else(|| {
            published
                .item
                .metadata
                .get("traceId")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            published
                .item
                .metadata
                .get("requestId")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            published.canonical_turn.as_ref().and_then(|turn| {
                turn.items.iter().find_map(|item| {
                    item.metadata
                        .get("traceId")
                        .or_else(|| item.metadata.get("requestId"))
                        .and_then(Value::as_str)
                })
            })
        });
    let Some(trace_id) = trace_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    tracing::info!(
        target: "magi.performance",
        trace_id,
        request_id = trace_id,
        session_id = %session_id,
        turn_id = %published.turn_id,
        provider_call_id = "",
        event_sequence,
        stage = "event_bus_item_published",
        elapsed_ms = 0u64,
        "conversation response timing"
    );
    if published
        .canonical_turn
        .as_ref()
        .is_some_and(|turn| turn.status.is_terminal())
    {
        tracing::info!(
            target: "magi.performance",
            trace_id,
            request_id = trace_id,
            session_id = %session_id,
            turn_id = %published.turn_id,
            provider_call_id = "",
            event_sequence,
            finalized = true,
            elapsed_ms = 0u64,
            stage = "canonical_terminal_published",
            "conversation response timing"
        );
    }
}

fn publish_session_turn_item_stream_event_raw(
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    published: &PublishedSessionTurnItem,
    stream_update: &SessionTurnStreamUpdate,
    publish_gate: &mut SessionTurnStreamPublishGate,
) -> Option<u64> {
    let canonical_item = published.canonical_item.as_ref()?;
    // 存储写入后的版本就是这份内容的事实版本；没有版本的 item 不能作为增量事实发布。
    let item_version = canonical_item.item_version?;
    let current_content = canonical_item.content.as_deref().unwrap_or_default();
    let (first_frame, stream_update) =
        publish_gate.prepare_publish(stream_update, current_content)?;
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
        // 对外协议使用 camelCase；保留现有 snake_case 字段供旧事件回放解析器读取，
        // 两组字段在同一次事实事件中始终表达同一版本和长度。
        "itemVersion": item_version,
        "baseContentLength": stream_update.base_content_length,
        "contentLength": stream_update.content_length,
        "delta": stream_update.delta,
        "reset": stream_update.reset,
    });
    if first_frame {
        payload
            .as_object_mut()
            .expect("stream event payload must be an object")
            .insert(
                "canonical_item".to_string(),
                serde_json::to_value(canonical_item)
                    .expect("canonical stream item must be serializable"),
            );
    }
    Some(publish_session_turn_item_payload(
        event_bus,
        session_id,
        workspace_id,
        payload,
    ))
}

fn publish_session_turn_item_payload(
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    payload: Value,
) -> u64 {
    event_bus.publish(
        EventEnvelope::domain(
            EventId::unique("event-session-turn-item"),
            "session.turn.item",
            payload,
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            ..EventContext::default()
        }),
    )
}

pub fn publish_current_session_turn_item_event(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    item_id: &str,
    task_store: Option<&TaskStore>,
) -> Result<(), String> {
    let sidecar = session_store
        .runtime_sidecar(session_id)
        .ok_or_else(|| format!("会话 {} 没有可发布的运行时 sidecar", session_id))?;
    let published =
        published_session_turn_item_from_sidecar(session_store, sidecar, item_id, task_store)?
            .ok_or_else(|| {
                format!(
                    "会话 {} 当前 Turn 没有可发布的 item {}",
                    session_id, item_id
                )
            })?;
    publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    Ok(())
}

pub struct SessionTurnErrorInput<'a> {
    pub session_id: &'a SessionId,
    pub workspace_id: &'a Option<WorkspaceId>,
    pub task_id: Option<&'a TaskId>,
    pub request_id: Option<&'a str>,
    pub user_message_id: Option<&'a str>,
    pub placeholder_message_id: Option<&'a str>,
    pub error_text: &'a str,
    pub model_failure: Option<&'a crate::model_error::ModelFailureDiagnostic>,
    pub streaming_entry_id: Option<&'a str>,
    pub source_thread_id: ThreadId,
    pub persist_session_state: Option<&'a SessionStatePersistCallback>,
    pub expected_turn_id: Option<&'a str>,
}

pub fn append_session_turn_error_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    input: SessionTurnErrorInput<'_>,
) -> Result<(), String> {
    append_session_turn_error_item_with_terminal_commit(event_bus, session_store, input, true)
}

/// 写入失败 item，但把 Turn 终态留给 SessionTurnCoordinator 提交。
///
/// Conversation profile 使用这个入口，保证执行器只产生内容事实，Coordinator
/// 才能在同一个生命周期边界提交 failed/cancelled/completed。
pub fn append_session_turn_error_item_without_terminal_commit(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    input: SessionTurnErrorInput<'_>,
) -> Result<(), String> {
    append_session_turn_error_item_with_terminal_commit(event_bus, session_store, input, false)
}

fn append_session_turn_error_item_with_terminal_commit(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    input: SessionTurnErrorInput<'_>,
    commit_terminal: bool,
) -> Result<(), String> {
    let SessionTurnErrorInput {
        session_id,
        workspace_id,
        task_id,
        request_id,
        user_message_id,
        placeholder_message_id,
        error_text,
        model_failure,
        streaming_entry_id: _,
        source_thread_id,
        persist_session_state,
        expected_turn_id,
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
    if let Some(model_failure) = model_failure {
        error_item.metadata.insert(
            "modelFailure".to_string(),
            serde_json::to_value(model_failure).expect("model failure diagnostic must serialize"),
        );
    }
    let item_published = append_session_turn_item_for_turn(
        session_store,
        session_id,
        expected_turn_id,
        error_item,
        None,
    )?
    .ok_or_else(|| {
        format!(
            "会话 {} 没有可写入的当前 Turn，无法记录失败事实",
            session_id
        )
    })?;
    if commit_terminal {
        CanonicalTurnEventSink::for_store(session_store, None)
            .set_status_domain(session_id, expected_turn_id, "failed")
            .map_err(|error| format!("更新会话 {} 的 Turn failed 状态失败: {error}", session_id))?
            .ok_or_else(|| {
                format!(
                    "会话 {} 没有可更新的当前 Turn，无法提交 failed 状态",
                    session_id
                )
            })?;
        persist_session_state_checkpoint(persist_session_state, "session_turn_failed")?;
    } else {
        // 失败 item 本身仍需可靠落盘，终态由 Coordinator 在随后同一执行边界提交。
        // 生产 Task 路径不能在这里发布 item：TaskStore finalizer 可能并发提交
        // canonical failed 状态，此处若重新读取 current Turn 会把“失败 item 写回”
        // 伪装成 terminal event，并先于 Coordinator settlement 唤醒下一轮接纳。
        // finalizer 会以稳定 item ID 做幂等 upsert，并在 Coordinator 收口后发布完整
        // canonical terminal snapshot；持久化事实仍可在断线/重启恢复。
        persist_session_state_checkpoint(persist_session_state, "session_turn_error_item")?;
        return Ok(());
    }
    let _ = item_published;
    publish_current_session_turn_item_event(
        event_bus,
        session_store,
        session_id,
        workspace_id,
        &error_item_id,
        None,
    )?;
    Ok(())
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

/// 外部（MCP）工具调用在会话当前 Turn 里的内联展示。
///
/// GPT Web 的工具由网页模型经 MCP 调用，不经过 Magi 自己的工具循环；它们仍是 Magi 的执行事实，
/// 必须作为普通工具项写进 canonical 并发布 turn item 事件（W9、§8.4）。写入的 Turn 是会话
/// **当前进行中的 Turn**：没有进行中的 Turn 时返回错误，调用方应拒绝该调用。
pub struct ExternalToolItemWriter<'a> {
    pub session_store: &'a SessionStore,
    pub event_bus: &'a InMemoryEventBus,
    pub session_id: &'a SessionId,
    pub workspace_id: &'a Option<WorkspaceId>,
}

impl ExternalToolItemWriter<'_> {
    fn current_turn_id(&self) -> Result<String, String> {
        self.session_store
            .runtime_sidecar(self.session_id)
            .and_then(|sidecar| sidecar.current_turn.map(|turn| turn.turn_id))
            .ok_or_else(|| format!("会话 {} 当前没有进行中的 Turn", self.session_id))
    }

    fn source_thread(&self) -> ThreadId {
        self.session_store
            .ensure_session_mission(self.session_id, UtcMillis::now(), || {
                magi_core::MissionId::new(format!("mission-{}", self.session_id))
            })
            .1
    }

    fn write(&self, turn_id: &str, item: ActiveExecutionTurnItem) -> Result<(), String> {
        let published = upsert_session_turn_item_for_turn(
            self.session_store,
            self.session_id,
            Some(turn_id),
            item,
            None,
        )?
        .ok_or_else(|| format!("会话 {} 的当前 Turn 已不可写", self.session_id))?;
        publish_session_turn_item_event(
            self.event_bus,
            self.session_id,
            self.workspace_id,
            &published,
        );
        Ok(())
    }

    pub fn started(
        &self,
        call_id: &str,
        tool_name: &str,
        arguments_json: &str,
    ) -> Result<(), String> {
        let turn_id = self.current_turn_id()?;
        let mut item = session_turn_item(
            "tool_call_started",
            "running",
            Some(tool_name.to_string()),
            Some(format!("正在调用工具：{tool_name}")),
            Some(format!("turn-item-tool-{call_id}")),
            self.source_thread(),
        );
        item.source = "tool".to_string();
        item.tool_call_id = Some(call_id.to_string());
        item.tool_name = Some(tool_name.to_string());
        item.tool_status = Some("running".to_string());
        item.tool_arguments = Some(arguments_json.to_string());
        self.write(&turn_id, item)
    }

    pub fn finished(
        &self,
        call_id: &str,
        tool_name: &str,
        arguments_json: &str,
        result: &str,
        status: ExecutionResultStatus,
    ) -> Result<(), String> {
        let turn_id = self.current_turn_id()?;
        let mut item = session_turn_item(
            "tool_call_result",
            turn_item_status_for_tool_result(status),
            Some(tool_name.to_string()),
            Some(summarize_tool_result(result)),
            Some(format!("turn-item-tool-{call_id}")),
            self.source_thread(),
        );
        item.source = "tool".to_string();
        item.tool_call_id = Some(call_id.to_string());
        item.tool_name = Some(tool_name.to_string());
        item.tool_status = Some(status.wire_label().to_string());
        item.tool_arguments = Some(arguments_json.to_string());
        item.tool_result = Some(result.to_string());
        if matches!(
            status,
            ExecutionResultStatus::Failed
                | ExecutionResultStatus::Rejected
                | ExecutionResultStatus::NeedsApproval
                | ExecutionResultStatus::Cancelled
        ) {
            item.tool_error = Some(result.to_string());
        }
        self.write(&turn_id, item)
    }
}

#[cfg(test)]
fn seed_test_conversation_turn(
    session_store: &SessionStore,
    session_id: &SessionId,
    mut turn: ActiveExecutionTurn,
) {
    let coordinator = SessionTurnCoordinator::new();
    let turn_id = turn.turn_id.clone();
    let accepted_at = turn.accepted_at;
    let request_id = format!("request-{turn_id}");
    let request_fingerprint = format!("fingerprint-{turn_id}");
    let attempt = match coordinator
        .execute_command(
            session_id,
            TurnCommand::Start(TurnAdmission {
                turn_id: turn_id.clone(),
                request_id,
                request_fingerprint,
                profile: ExecutionProfile::Conversation,
            }),
        )
        .expect("session writeback test Turn should be admitted")
    {
        CoordinatorCommandResult::Admission(CoordinatorAdmission::Accepted(attempt)) => attempt,
        other => panic!("unexpected session writeback test admission: {other:?}"),
    };
    let workspace_id = session_store
        .execution_ownership(session_id)
        .and_then(|ownership| ownership.workspace_id);
    let message = turn.user_message.clone().unwrap_or_default();
    turn.status = "accepted".to_string();
    CanonicalTurnEventSink::for_store(session_store, None)
        .accept_conversation_turn_with_timeline_entry(
            session_id.clone(),
            workspace_id,
            TimelineEntryInput::new(
                format!("timeline-{turn_id}"),
                magi_session_store::TimelineEntryKind::UserMessage,
                message,
                accepted_at,
            ),
            turn,
        )
        .expect("session writeback test Turn should be persisted through sink");
    coordinator
        .execute_command(
            session_id,
            TurnCommand::SetStatus {
                attempt: attempt.clone(),
                status: CoordinatorTurnStatus::Preparing,
            },
        )
        .expect("session writeback test Turn should enter preparing");
    coordinator
        .execute_command(
            session_id,
            TurnCommand::SetStatus {
                attempt,
                status: CoordinatorTurnStatus::Running,
            },
        )
        .expect("session writeback test Turn should enter running");
    CanonicalTurnEventSink::for_store(session_store, None)
        .set_status_domain(session_id, Some(turn_id.as_str()), "running")
        .expect("session writeback test running status should persist");
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::{ModelRetryRuntimeEvent, ModelRetryRuntimePhase};
    use magi_core::{MissionId, ThreadId};
    use magi_session_store::{
        ActiveExecutionChain, ActiveExecutionDispatchContext, ActiveExecutionTurn,
    };

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
        let (first_frame, published) = gate
            .prepare_publish_at(&first, "a", UtcMillis(1_000))
            .expect("first frame should publish");
        assert!(first_frame, "第一次发布必须标记为第一帧，以便附带完整 item");
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
        let (first_frame, published) = gate
            .prepare_publish_at(&enough_chars, &enough_content, UtcMillis(1_002))
            .expect("coalesced content should publish");
        assert!(!first_frame);
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
        let (first_frame, published) = gate
            .prepare_publish_at(
                &delayed,
                &delayed_content,
                UtcMillis(1_002 + STREAM_ITEM_PUBLISH_MIN_INTERVAL_MS),
            )
            .expect("delayed frame should publish");
        assert!(!first_frame);
        assert_eq!(published.delta, "d");

        let reset = SessionTurnStreamUpdate {
            delta: "reset".to_string(),
            base_content_length: STREAM_ITEM_PUBLISH_MIN_CHARS + 3,
            content_length: 5,
            reset: true,
        };
        let (first_frame, published) = gate
            .prepare_publish_at(&reset, "reset", UtcMillis(1_003))
            .expect("reset frame should publish");
        assert!(!first_frame);
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
        seed_test_conversation_turn(
            &session_store,
            &session_id,
            ActiveExecutionTurn {
                turn_id: "turn-stream-delta-payload".to_string(),
                turn_seq: 1,
                accepted_at: now,
                completed_at: None,
                status: "running".to_string(),
                user_message: None,
                items: Vec::new(),
            },
        );

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
        let first_published = upsert_session_turn_item_for_turn(
            &session_store,
            &session_id,
            Some("turn-stream-delta-payload"),
            first_item,
            None,
        )
        .expect("first stream item writeback should succeed")
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
        let suppressed_published = upsert_session_turn_item_for_turn(
            &session_store,
            &session_id,
            Some("turn-stream-delta-payload"),
            suppressed_item,
            None,
        )
        .expect("suppressed stream item writeback should succeed")
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
        let second_published = upsert_session_turn_item_for_turn(
            &session_store,
            &session_id,
            Some("turn-stream-delta-payload"),
            second_item,
            None,
        )
        .expect("second stream item writeback should succeed")
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
        assert_eq!(first_payload["itemVersion"], Value::from(1_u64));
        assert_eq!(first_payload["baseContentLength"], Value::from(0_u64));
        assert_eq!(first_payload["contentLength"], Value::from(1_u64));
        assert_eq!(first_payload["delta"], Value::String("你".to_string()));
        assert_eq!(first_payload["reset"], Value::Bool(false));
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
        // 中间那次写入被节流、没有发布，但存储版本照样前进到 3。事件必须携带存储版本：
        // 它也是快照和恢复使用的版本，否则客户端从快照恢复后会把后续增量当成旧版本丢弃。
        let stored_version = second_published
            .canonical_item
            .as_ref()
            .and_then(|item| item.item_version)
            .expect("stored item should carry a version");
        assert_eq!(stored_version, 3);
        assert_eq!(
            second_payload["canonical_item_version"],
            Value::from(stored_version)
        );
        assert_eq!(second_payload["itemVersion"], Value::from(stored_version));
        assert_eq!(second_payload["baseContentLength"], Value::from(1_u64));
        assert_eq!(
            second_payload["contentLength"],
            Value::from(second_content.chars().count() as u64)
        );
        assert_eq!(second_payload["delta"], second_payload["stream_delta"]);
        assert_eq!(second_payload["reset"], Value::Bool(false));
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
    fn canonical_turn_carries_request_identity_for_terminal_shell() {
        let session_id = SessionId::new("session-terminal-shell-runtime");
        let thread_id = ThreadId::new("thread-terminal-shell-runtime");
        let now = UtcMillis(10);
        let mut user_item = session_turn_item(
            "user_message",
            "completed",
            None,
            Some("终态 shell 请求身份".to_string()),
            Some("user-terminal-shell-runtime".to_string()),
            thread_id,
        );
        user_item.item_seq = 1;
        user_item.request_id = Some("request-terminal-shell-runtime".to_string());
        let turn = ActiveExecutionTurn {
            turn_id: "turn-terminal-shell-runtime".to_string(),
            turn_seq: 1,
            accepted_at: now,
            completed_at: Some(UtcMillis(20)),
            status: "completed".to_string(),
            user_message: Some("终态 shell 请求身份".to_string()),
            items: vec![user_item],
        };

        let canonical = to_canonical_turn(&session_id, &turn).expect("turn should canonicalize");
        assert_eq!(
            canonical.metadata.get("requestId"),
            Some(&Value::String("request-terminal-shell-runtime".to_string()))
        );

        let mut shell = canonical;
        shell.items.clear();
        assert_eq!(
            shell.metadata.get("requestId"),
            Some(&Value::String("request-terminal-shell-runtime".to_string()))
        );
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
    fn canonical_assistant_output_metadata_distinguishes_progress_final_and_error() {
        let session_id = SessionId::new("session-assistant-output-kind");
        let thread_id = ThreadId::new("thread-assistant-output-kind");
        let now = UtcMillis::now();

        for (kind, expected) in [
            ("assistant_stream", "progress"),
            ("assistant_final", "final"),
            ("assistant_error", "error"),
        ] {
            let item = session_turn_item(
                kind,
                "completed",
                Some("模型输出".to_string()),
                Some("内容".to_string()),
                Some(format!("turn-item-{expected}")),
                thread_id.clone(),
            );
            let turn = ActiveExecutionTurn {
                turn_id: "turn-assistant-output-kind".to_string(),
                turn_seq: 1,
                accepted_at: now,
                completed_at: Some(now),
                status: "completed".to_string(),
                user_message: None,
                items: vec![item.clone()],
            };

            let canonical = to_canonical_turn_item(&session_id, &turn, &item)
                .expect("assistant output should remain canonical");
            assert_eq!(
                canonical
                    .metadata
                    .get("assistantOutputKind")
                    .and_then(serde_json::Value::as_str),
                Some(expected)
            );
        }
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
        seed_test_conversation_turn(
            &session_store,
            &session_id,
            ActiveExecutionTurn {
                turn_id: "turn-session-plain".to_string(),
                turn_seq: 2,
                accepted_at: now,
                completed_at: None,
                status: "running".to_string(),
                user_message: Some("普通流式验证".to_string()),
                items: vec![final_item],
            },
        );
        CanonicalTurnEventSink::for_store(&session_store, None)
            .set_status_domain(&session_id, Some("turn-session-plain"), "completed")
            .expect("plain turn should complete through sink");

        let sidecar = session_store
            .runtime_sidecar(&session_id)
            .expect("runtime sidecar should exist");
        let published = published_session_turn_item_from_sidecar(
            &session_store,
            sidecar,
            "turn-item-plain-final",
            None,
        )
        .expect("plain final item writeback should succeed")
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
