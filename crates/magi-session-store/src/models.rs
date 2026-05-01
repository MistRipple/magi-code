use magi_core::{
    DomainError, DomainResult, ExecutionOwnership, LeaseId, MissionId, SessionId,
    SessionLifecycleStatus, TaskId, UtcMillis, WorkerId, WorkspaceId,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionExecutionSidecarStatus {
    #[default]
    Detached,
    Bound,
    RecoveryLinked,
    Resumed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    #[serde(alias = "session_id")]
    pub session_id: SessionId,
    pub title: String,
    pub status: SessionLifecycleStatus,
    #[serde(alias = "created_at")]
    pub created_at: UtcMillis,
    #[serde(alias = "updated_at")]
    pub updated_at: UtcMillis,
    #[serde(
        default,
        alias = "message_count",
        skip_serializing_if = "Option::is_none"
    )]
    pub message_count: Option<usize>,
    #[serde(
        default,
        alias = "workspace_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub workspace_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveExecutionDispatchContext {
    pub accepted_at: UtcMillis,
    pub entry_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trimmed_text: Option<String>,
    pub deep_task: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveExecutionBranch {
    pub task_id: TaskId,
    pub worker_id: WorkerId,
    pub stage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<LeaseId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_intent_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_lifecycle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_step_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_at: Option<UtcMillis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_token: Option<String>,
    pub use_tools: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_name: Option<String>,
    #[serde(default)]
    pub is_primary: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveExecutionTurnLane {
    pub lane_id: String,
    pub lane_seq: usize,
    pub task_id: TaskId,
    pub worker_id: WorkerId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub is_primary: bool,
}

pub const CANONICAL_TURN_SCHEMA_VERSION: &str = "canonical-turn.v1";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalTurnStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl CanonicalTurnStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn allows_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        match self {
            Self::Pending => matches!(
                next,
                Self::Running | Self::Completed | Self::Failed | Self::Cancelled
            ),
            Self::Running => matches!(next, Self::Completed | Self::Failed | Self::Cancelled),
            Self::Completed | Self::Failed | Self::Cancelled => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalTurnItemKind {
    UserMessage,
    AssistantText,
    AssistantThinking,
    ToolCall,
    WorkerDispatch,
    WorkerStatus,
    WorkerResult,
    TaskStatus,
    SystemNotice,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalTurnItemStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl CanonicalTurnItemStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn allows_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        match self {
            Self::Pending => matches!(
                next,
                Self::Running | Self::Completed | Self::Failed | Self::Cancelled
            ),
            Self::Running => matches!(next, Self::Completed | Self::Failed | Self::Cancelled),
            Self::Completed | Self::Failed | Self::Cancelled => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalTurnEventKind {
    TurnStarted,
    TurnItemUpsert,
    TurnCompleted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalTurnVisibility {
    #[serde(default = "default_true")]
    pub thread_visible: bool,
    #[serde(default)]
    pub worker_visible: bool,
    #[serde(default = "default_true")]
    pub renderable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub worker_tab_ids: Vec<String>,
}

impl Default for CanonicalTurnVisibility {
    fn default() -> Self {
        Self {
            thread_visible: true,
            worker_visible: false,
            renderable: true,
            worker_tab_ids: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalToolCall {
    pub call_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalWorkerRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<WorkerId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalTurnItem {
    pub session_id: SessionId,
    pub turn_id: String,
    pub turn_seq: u64,
    pub item_id: String,
    pub item_seq: usize,
    pub kind: CanonicalTurnItemKind,
    pub created_at: UtcMillis,
    pub status: CanonicalTurnItemStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_version: Option<u64>,
    pub updated_at: UtcMillis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane_seq: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<CanonicalToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<CanonicalWorkerRef>,
    #[serde(default)]
    pub visibility: CanonicalTurnVisibility,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, Value>,
}

impl CanonicalTurnItem {
    pub fn validate_update_from(&self, existing: &Self) -> DomainResult<()> {
        reject_changed_field(
            "sessionId",
            self.session_id == existing.session_id,
            &self.item_id,
        )?;
        reject_changed_field("turnId", self.turn_id == existing.turn_id, &self.item_id)?;
        reject_changed_field("turnSeq", self.turn_seq == existing.turn_seq, &self.item_id)?;
        reject_changed_field("itemSeq", self.item_seq == existing.item_seq, &self.item_id)?;
        reject_changed_field("kind", self.kind == existing.kind, &self.item_id)?;
        reject_changed_field(
            "createdAt",
            self.created_at == existing.created_at,
            &self.item_id,
        )?;
        reject_changed_field("laneId", self.lane_id == existing.lane_id, &self.item_id)?;
        reject_changed_field("laneSeq", self.lane_seq == existing.lane_seq, &self.item_id)?;
        reject_changed_field(
            "tool.callId",
            self.tool_call_id() == existing.tool_call_id(),
            &self.item_id,
        )?;
        if !existing.status.allows_transition_to(self.status) {
            return Err(DomainError::InvalidState {
                message: format!(
                    "canonical turn item {} illegal status transition: {:?} -> {:?}",
                    self.item_id, existing.status, self.status
                ),
            });
        }
        Ok(())
    }

    fn tool_call_id(&self) -> Option<&str> {
        self.tool.as_ref().map(|tool| tool.call_id.as_str())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalTurn {
    pub session_id: SessionId,
    pub turn_id: String,
    pub turn_seq: u64,
    pub accepted_at: UtcMillis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<UtcMillis>,
    pub status: CanonicalTurnStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    #[serde(default)]
    pub items: Vec<CanonicalTurnItem>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, Value>,
}

impl CanonicalTurn {
    pub fn normalize(&mut self) {
        self.items.sort_by(|left, right| {
            left.item_seq
                .cmp(&right.item_seq)
                .then_with(|| left.item_id.cmp(&right.item_id))
        });
    }

    pub fn validate_update_from(&self, existing: &Self) -> DomainResult<()> {
        reject_changed_field(
            "sessionId",
            self.session_id == existing.session_id,
            &self.turn_id,
        )?;
        reject_changed_field("turnId", self.turn_id == existing.turn_id, &self.turn_id)?;
        reject_changed_field("turnSeq", self.turn_seq == existing.turn_seq, &self.turn_id)?;
        reject_changed_field(
            "acceptedAt",
            self.accepted_at == existing.accepted_at,
            &self.turn_id,
        )?;
        if !existing.status.allows_transition_to(self.status) {
            return Err(DomainError::InvalidState {
                message: format!(
                    "canonical turn {} illegal status transition: {:?} -> {:?}",
                    self.turn_id, existing.status, self.status
                ),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalTurnEvent {
    pub schema_version: String,
    pub event_id: String,
    pub event_seq: u64,
    pub kind: CanonicalTurnEventKind,
    pub session_id: SessionId,
    pub turn_id: String,
    pub turn_seq: u64,
    pub occurred_at: UtcMillis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<CanonicalTurn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<CanonicalTurnItem>,
}

fn reject_changed_field(field: &'static str, unchanged: bool, identity: &str) -> DomainResult<()> {
    if unchanged {
        return Ok(());
    }
    Err(DomainError::InvalidState {
        message: format!(
            "canonical turn fact {identity} attempted to change immutable field {field}"
        ),
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveExecutionTurnItem {
    pub item_id: String,
    pub item_seq: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane_seq: Option<usize>,
    pub kind: String,
    pub status: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<WorkerId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline_entry_id: Option<String>,
    #[serde(default = "default_true")]
    pub thread_visible: bool,
    #[serde(default)]
    pub worker_visible: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveExecutionTurn {
    pub turn_id: String,
    pub turn_seq: u64,
    pub accepted_at: UtcMillis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<UtcMillis>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_message: Option<String>,
    #[serde(default)]
    pub items: Vec<ActiveExecutionTurnItem>,
    #[serde(default)]
    pub worker_lanes: Vec<ActiveExecutionTurnLane>,
}

fn default_true() -> bool {
    true
}

impl ActiveExecutionTurn {
    pub fn normalize(&mut self) {
        self.worker_lanes.sort_by(|left, right| {
            left.lane_seq
                .cmp(&right.lane_seq)
                .then_with(|| left.lane_id.cmp(&right.lane_id))
        });
        self.items.sort_by(|left, right| {
            left.item_seq
                .cmp(&right.item_seq)
                .then_with(|| left.item_id.cmp(&right.item_id))
        });
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveExecutionChain {
    pub session_id: SessionId,
    pub mission_id: MissionId,
    pub root_task_id: TaskId,
    pub execution_chain_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default)]
    pub active_branch_task_ids: Vec<TaskId>,
    #[serde(default)]
    pub active_worker_bindings: Vec<WorkerId>,
    #[serde(default)]
    pub branches: Vec<ActiveExecutionBranch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_ref: Option<String>,
    pub dispatch_context: ActiveExecutionDispatchContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_turn: Option<ActiveExecutionTurn>,
}

impl ActiveExecutionChain {
    pub fn normalize(&mut self) {
        self.active_branch_task_ids
            .sort_by(|left, right| left.as_str().cmp(right.as_str()));
        self.active_branch_task_ids
            .dedup_by(|left, right| left == right);
        self.active_worker_bindings
            .sort_by(|left, right| left.as_str().cmp(right.as_str()));
        self.active_worker_bindings
            .dedup_by(|left, right| left == right);
        self.branches.sort_by(|left, right| {
            left.task_id
                .as_str()
                .cmp(right.task_id.as_str())
                .then_with(|| left.worker_id.as_str().cmp(right.worker_id.as_str()))
        });
        if let Some(turn) = self.current_turn.as_mut() {
            turn.normalize();
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionRuntimeSidecar {
    pub session_id: SessionId,
    pub ownership: ExecutionOwnership,
    #[serde(default, alias = "recovery_ref")]
    pub recovery_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_turn: Option<ActiveExecutionTurn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_execution_chain: Option<ActiveExecutionChain>,
    #[serde(default)]
    pub status: SessionExecutionSidecarStatus,
    pub updated_at: UtcMillis,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionRuntimeSidecarExport {
    pub session_id: SessionId,
    #[serde(alias = "status")]
    pub current_status: SessionExecutionSidecarStatus,
    #[serde(alias = "updated_at")]
    pub last_update: UtcMillis,
    pub ownership: ExecutionOwnership,
    pub execution_chain_ref: Option<String>,
    #[serde(default, alias = "recovery_id")]
    pub recovery_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_turn: Option<ActiveExecutionTurn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_execution_chain: Option<ActiveExecutionChain>,
}

impl SessionRuntimeSidecar {
    pub fn export_view(&self) -> SessionRuntimeSidecarExport {
        SessionRuntimeSidecarExport {
            session_id: self.session_id.clone(),
            current_status: self.status.clone(),
            last_update: self.updated_at,
            ownership: self.ownership.clone(),
            execution_chain_ref: self.ownership.execution_chain_ref.clone(),
            recovery_ref: self.recovery_id.clone(),
            current_turn: self.current_turn.clone(),
            active_execution_chain: self.active_execution_chain.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionSidecarFlushReason {
    UpsertRuntimeSidecar,
    BindExecutionOwnership,
    ApplyRecoveryResumeInput,
    ApplyResumeExecutionTarget,
    UpsertActiveExecutionChain,
    UpsertCurrentTurn,
    UpdateActiveExecutionBranchSnapshot,
    AppendCurrentTurnItem,
    UpdateCurrentTurnStatus,
    AttachRecoveryRef,
    ClearExecutionOwnership,
    DeleteSession,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSidecarFlushMetadata {
    pub current_version: u64,
    pub flushed_version: u64,
    pub last_dirty_at: Option<UtcMillis>,
    pub last_dirty_reason: Option<SessionSidecarFlushReason>,
    pub last_flush_at: Option<UtcMillis>,
    pub next_flush_hint: Option<UtcMillis>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionExecutionSidecarStoreState {
    pub runtime_sidecars: Vec<SessionRuntimeSidecar>,
}

impl SessionExecutionSidecarStoreState {
    fn sort_runtime_sidecars(runtime_sidecars: &mut Vec<SessionRuntimeSidecar>) {
        runtime_sidecars
            .sort_by(|left, right| left.session_id.as_str().cmp(right.session_id.as_str()));
    }

    pub fn upsert_runtime_sidecar(&mut self, sidecar: SessionRuntimeSidecar) {
        if let Some(existing) = self
            .runtime_sidecars
            .iter_mut()
            .find(|existing| existing.session_id == sidecar.session_id)
        {
            *existing = sidecar;
        } else {
            self.runtime_sidecars.push(sidecar);
        }
        Self::sort_runtime_sidecars(&mut self.runtime_sidecars);
    }

    pub fn remove_runtime_sidecar(&mut self, session_id: &SessionId) {
        self.runtime_sidecars
            .retain(|sidecar| &sidecar.session_id != session_id);
    }

    pub fn runtime_sidecar(&self, session_id: &SessionId) -> Option<SessionRuntimeSidecar> {
        self.runtime_sidecars
            .iter()
            .find(|sidecar| &sidecar.session_id == session_id)
            .cloned()
    }

    pub fn runtime_sidecars(&self) -> Vec<SessionRuntimeSidecar> {
        let mut sidecars = self.runtime_sidecars.clone();
        Self::sort_runtime_sidecars(&mut sidecars);
        sidecars
    }

    pub fn active_runtime_sidecars(&self) -> Vec<SessionRuntimeSidecar> {
        self.runtime_sidecars()
            .into_iter()
            .filter(|sidecar| {
                sidecar.ownership.execution_chain_ref.is_some()
                    || sidecar.ownership.workspace_id.is_some()
                    || sidecar.ownership.mission_id.is_some()
                    || sidecar.ownership.task_id.is_some()
            })
            .collect()
    }

    pub fn export_views(&self) -> Vec<SessionRuntimeSidecarExport> {
        let mut exports = self
            .runtime_sidecars()
            .into_iter()
            .map(|sidecar| sidecar.export_view())
            .collect::<Vec<_>>();
        exports.sort_by(|left, right| left.session_id.as_str().cmp(right.session_id.as_str()));
        exports
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TimelineEntryKind {
    SessionCreated,
    SessionRenamed,
    SessionSwitched,
    SessionArchived,
    NotificationPublished,
    SystemNote,
    UserMessage,
    AssistantMessage,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineEntry {
    #[serde(alias = "entry_id")]
    pub entry_id: String,
    #[serde(alias = "session_id")]
    pub session_id: SessionId,
    pub kind: TimelineEntryKind,
    pub message: String,
    #[serde(alias = "occurred_at")]
    pub occurred_at: UtcMillis,
}

pub fn timeline_entry_visible_text(message: &str) -> Option<String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationRecord {
    #[serde(alias = "notification_id")]
    pub notification_id: String,
    #[serde(alias = "session_id")]
    pub session_id: SessionId,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(alias = "created_at")]
    pub created_at: UtcMillis,
    pub handled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist_to_center: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count_unread: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionStoreState {
    pub current_session_id: Option<SessionId>,
    pub sessions: Vec<SessionRecord>,
    pub timeline: Vec<TimelineEntry>,
    #[serde(default)]
    pub canonical_turns: Vec<CanonicalTurn>,
    pub notifications: Vec<NotificationRecord>,
    #[serde(default, flatten)]
    pub execution_sidecar_store: SessionExecutionSidecarStoreState,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionDurableState {
    pub current_session_id: Option<SessionId>,
    pub sessions: Vec<SessionRecord>,
    pub timeline: Vec<TimelineEntry>,
    #[serde(default)]
    pub canonical_turns: Vec<CanonicalTurn>,
    pub notifications: Vec<NotificationRecord>,
}

impl SessionDurableState {
    pub fn is_empty(&self) -> bool {
        self.current_session_id.is_none()
            && self.sessions.is_empty()
            && self.timeline.is_empty()
            && self.canonical_turns.is_empty()
            && self.notifications.is_empty()
    }

    pub fn append_state(&mut self, other: SessionDurableState) {
        if self.current_session_id.is_none() {
            self.current_session_id = other.current_session_id;
        }
        self.sessions.extend(other.sessions);
        self.timeline.extend(other.timeline);
        self.canonical_turns.extend(other.canonical_turns);
        self.notifications.extend(other.notifications);
    }

    pub fn partition_by_workspace(
        &self,
    ) -> (SessionDurableState, HashMap<String, SessionDurableState>) {
        let mut global_sessions = Vec::new();
        let mut workspace_sessions = HashMap::<String, Vec<SessionRecord>>::new();

        for session in &self.sessions {
            if let Some(workspace_id) = session.workspace_id.as_deref() {
                workspace_sessions
                    .entry(workspace_id.to_string())
                    .or_default()
                    .push(session.clone());
            } else {
                global_sessions.push(session.clone());
            }
        }

        let global_session_ids = global_sessions
            .iter()
            .map(|session| session.session_id.clone())
            .collect::<HashSet<_>>();

        let mut workspace_states = HashMap::<String, SessionDurableState>::new();
        let mut workspace_session_ids = HashMap::<String, HashSet<SessionId>>::new();
        for (workspace_id, sessions) in workspace_sessions {
            let session_ids = sessions
                .iter()
                .map(|session| session.session_id.clone())
                .collect::<HashSet<_>>();
            workspace_session_ids.insert(workspace_id.clone(), session_ids);
            workspace_states.insert(
                workspace_id,
                SessionDurableState {
                    current_session_id: None,
                    sessions,
                    timeline: Vec::new(),
                    canonical_turns: Vec::new(),
                    notifications: Vec::new(),
                },
            );
        }

        let mut global_state = SessionDurableState {
            current_session_id: self
                .current_session_id
                .clone()
                .filter(|session_id| global_session_ids.contains(session_id)),
            sessions: global_sessions,
            timeline: Vec::new(),
            canonical_turns: Vec::new(),
            notifications: Vec::new(),
        };

        for entry in &self.timeline {
            if global_session_ids.contains(&entry.session_id) {
                global_state.timeline.push(entry.clone());
                continue;
            }
            for (workspace_id, session_ids) in &workspace_session_ids {
                if session_ids.contains(&entry.session_id) {
                    workspace_states
                        .get_mut(workspace_id)
                        .expect("workspace durable state should exist")
                        .timeline
                        .push(entry.clone());
                    break;
                }
            }
        }

        for turn in &self.canonical_turns {
            if global_session_ids.contains(&turn.session_id) {
                global_state.canonical_turns.push(turn.clone());
                continue;
            }
            for (workspace_id, session_ids) in &workspace_session_ids {
                if session_ids.contains(&turn.session_id) {
                    workspace_states
                        .get_mut(workspace_id)
                        .expect("workspace durable state should exist")
                        .canonical_turns
                        .push(turn.clone());
                    break;
                }
            }
        }

        for notification in &self.notifications {
            if global_session_ids.contains(&notification.session_id) {
                global_state.notifications.push(notification.clone());
                continue;
            }
            for (workspace_id, session_ids) in &workspace_session_ids {
                if session_ids.contains(&notification.session_id) {
                    workspace_states
                        .get_mut(workspace_id)
                        .expect("workspace durable state should exist")
                        .notifications
                        .push(notification.clone());
                    break;
                }
            }
        }

        if let Some(current_session_id) = self.current_session_id.as_ref() {
            for state in workspace_states.values_mut() {
                if state
                    .sessions
                    .iter()
                    .any(|session| &session.session_id == current_session_id)
                {
                    state.current_session_id = Some(current_session_id.clone());
                    break;
                }
            }
        }

        (global_state, workspace_states)
    }
}

impl SessionStoreState {
    fn normalize_timeline_entry_ids(timeline: &mut [TimelineEntry]) {
        let mut seen = HashMap::<String, usize>::new();
        for entry in timeline.iter_mut() {
            let original = entry.entry_id.clone();
            let duplicate_index = seen.entry(original.clone()).or_insert(0);
            if *duplicate_index > 0 {
                entry.entry_id =
                    format!("{}-{}-{}", original, entry.occurred_at.0, duplicate_index);
            }
            *duplicate_index += 1;
        }
    }

    pub fn from_persisted_parts(
        durable_state: SessionDurableState,
        execution_sidecar_store: SessionExecutionSidecarStoreState,
    ) -> Self {
        let mut timeline = durable_state.timeline;
        Self::normalize_timeline_entry_ids(&mut timeline);
        let mut canonical_turns = durable_state.canonical_turns;
        for turn in &mut canonical_turns {
            turn.normalize();
        }
        canonical_turns.sort_by(|left, right| {
            left.turn_seq
                .cmp(&right.turn_seq)
                .then_with(|| left.turn_id.cmp(&right.turn_id))
        });
        Self {
            current_session_id: durable_state.current_session_id,
            sessions: durable_state.sessions,
            timeline,
            canonical_turns,
            notifications: durable_state.notifications,
            execution_sidecar_store,
        }
    }

    pub fn durable_state(&self) -> SessionDurableState {
        SessionDurableState {
            current_session_id: self.current_session_id.clone(),
            sessions: self.sessions.clone(),
            timeline: self.timeline.clone(),
            canonical_turns: self.canonical_turns.clone(),
            notifications: self.notifications.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionProjectionInput {
    pub current_session_id: Option<SessionId>,
    pub sessions: Vec<SessionRecord>,
    pub timeline: Vec<TimelineEntry>,
    pub canonical_turns: Vec<CanonicalTurn>,
    pub notifications: Vec<NotificationRecord>,
}
