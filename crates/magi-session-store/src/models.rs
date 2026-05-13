use magi_core::{
    DomainError, DomainResult, ExecutionOwnership, LeaseId, MissionId, SessionId,
    SessionLifecycleStatus, TaskId, ThreadId, UtcMillis, WorkerId, WorkspaceId,
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
/// P6d 概念定位：`ActiveExecutionTurnLane` 是 Thread 在某个 turn 的"派发快照"——
/// thread 是跨 turn / 跨 task 的稳定身份（见 [`ExecutionThread`]），
/// lane 是 thread 在当前 turn 中实际承担工作的视图记录。lane.thread_id 必须能回溯到
/// 同一 mission 下注册过的 ExecutionThread；当 thread_id = None 时仅限 P6 迁移期
/// 的历史数据，新写入路径都应设置该字段。
pub struct ActiveExecutionTurnLane {
    pub lane_id: String,
    pub lane_seq: usize,
    pub task_id: TaskId,
    pub worker_id: WorkerId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_id: Option<String>,
    /// P6a：lane 绑定到其隶属的 Thread。`None` 表示旧路径（P6 未完成迁移的 lane），
    /// P6d 收口后会去掉 Option。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
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
    Blocked,
    Failed,
    Cancelled,
}

impl CanonicalTurnStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Blocked | Self::Failed | Self::Cancelled
        )
    }

    pub fn allows_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        match self {
            Self::Pending => matches!(
                next,
                Self::Running | Self::Completed | Self::Blocked | Self::Failed | Self::Cancelled
            ),
            Self::Running => matches!(
                next,
                Self::Completed | Self::Blocked | Self::Failed | Self::Cancelled
            ),
            Self::Completed | Self::Blocked | Self::Failed | Self::Cancelled => false,
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
    Blocked,
    Failed,
    Cancelled,
}

impl CanonicalTurnItemStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Blocked | Self::Failed | Self::Cancelled
        )
    }

    pub fn allows_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        match self {
            Self::Pending => matches!(
                next,
                Self::Running | Self::Completed | Self::Blocked | Self::Failed | Self::Cancelled
            ),
            Self::Running => matches!(
                next,
                Self::Completed | Self::Blocked | Self::Failed | Self::Cancelled
            ),
            Self::Completed | Self::Blocked | Self::Failed | Self::Cancelled => false,
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
    /// P6c：item 归属的 thread_id。orchestrator 主线 item 为 session 级 orchestrator thread，
    /// worker sidechain item 为对应 worker thread。前端 projection 用它作为单一路由键。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_thread_id: Option<ThreadId>,
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
    /// P6c：item 归属的 thread。orchestrator 主线 item 为 session 级 orchestrator thread；
    /// worker sidechain item 为对应 worker thread。为 None 仅限于 P6 迁移期的历史数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_thread_id: Option<ThreadId>,
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
    /// P6 Thread 原语注册表：按 session 聚合 `ExecutionThread`，支持同 role 跨 task
    /// 复用。不进入 durable snapshot（P6a 仅运行时维护；P6b 引入持久化后再迁移）。
    #[serde(skip, default)]
    pub thread_registry: Vec<ExecutionThread>,
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
            thread_registry: Vec::new(),
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

// ---------------------------------------------------------------------------
// P6 Thread 原语（Y 方案）
// ---------------------------------------------------------------------------

/// Thread 的生命周期状态。
///
/// - `Active`：当前正在处理某个 task（有 in-flight lease）。
/// - `Idle`：上一个 task 已完成，context 保留，可被下一 task 复用。
/// - `Retired`：mission 结束或显式回收，不再可被复用；保留为只读历史。
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionThreadStatus {
    Active,
    Idle,
    Retired,
}

/// P6 Thread 实体：承载"同 mission + 同 role = 同一条 thread"的产品语义。
///
/// 一个 Thread 绑定到具体的 worker 实例（`worker_instance_id`），跨多个 task
/// 累积上下文（`message_history` 在 P6b 启用）。Thread 在 mission 生命周期内
/// 可被派发多次；mission 结束后整体进入 `Retired`，不跨 mission 复用。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionThread {
    pub thread_id: ThreadId,
    pub session_id: SessionId,
    /// P6c：session 级 thread（如 orchestrator 主线 thread）在 mission 未绑定时
    /// 可以没有 mission。worker thread 在 ensure_thread_for_role 时总会带上。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<MissionId>,
    pub role_id: String,
    pub worker_instance_id: WorkerId,
    pub status: ExecutionThreadStatus,
    pub created_at: UtcMillis,
    pub last_used_at: UtcMillis,
    /// 该 thread 处理过的 task 序列，用于调试 / UI 呈现时间线。
    /// P6a 仅记录 task_id；P6b 在此基础上引入独立的 message_history 字段。
    #[serde(default)]
    pub handled_task_ids: Vec<TaskId>,
    /// P6b：跨 task 累积的 LLM 对话历史。存 user/assistant/tool 消息的串行记录，
    /// 下一 task 启动时会把这段历史作为上文前置到新一轮 prompt 中，形成 Codex 式的
    /// "同 role 持续性对话"。mission 结束时随 thread 一起 Retired，不跨 mission 复用。
    #[serde(default)]
    pub message_history: Vec<ThreadChatMessage>,
}

/// ExecutionThread 消息历史的最小存储格式：与 magi_bridge_client::ChatMessage 同构，
/// 但保留独立定义避免 session-store 反向依赖 bridge-client。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadChatMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ThreadChatToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadChatToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ThreadChatToolFunction,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadChatToolFunction {
    pub name: String,
    pub arguments: String,
}
