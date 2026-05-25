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
    /// branch 关联的 thread id。
    ///
    /// session resume 时 rebuild dispatch plan 需要取回 sub-task 的 thread；
    /// `ensure_thread_for_role` 用 `now.0` 拼 id 不可重放，必须持久化在 branch。
    pub thread_id: ThreadId,
}

pub const CANONICAL_TURN_SCHEMA_VERSION: &str = "canonical-turn.v1";

/// `source_thread_id` 的可见性判定结果：
/// - `Main`：对应 session 的 orchestrator thread，item 归属主线时间线
/// - `TaskDetail`：对应某条子代理 task thread，item 归属该 task 详情
///
/// 由 `SessionStore::resolve_thread_visibility` 返回，是后端路由可见性的唯一出口。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadVisibility {
    Main,
    TaskDetail {
        role_id: String,
        worker_id: WorkerId,
    },
}

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
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
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
            Self::Blocked => matches!(
                next,
                Self::Running | Self::Completed | Self::Failed | Self::Cancelled
            ),
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
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
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
            Self::Blocked => matches!(
                next,
                Self::Running | Self::Completed | Self::Failed | Self::Cancelled
            ),
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
    /// 该 item 是否值得被 UI 投射为卡片。与 source_thread_id 正交：
    /// renderable=false 的 item 仍然参与 canonical log（用于审计、撤销等），
    /// 只是前端 projection 在渲染时跳过。主线 / drawer 路由一律交给
    /// `source_thread_id` + thread_registry 判定，不再靠 visibility 决定归属。
    #[serde(default = "default_true")]
    pub renderable: bool,
}

impl Default for CanonicalTurnVisibility {
    fn default() -> Self {
        Self { renderable: true }
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
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<CanonicalToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<CanonicalWorkerRef>,
    /// item 归属的 thread_id。orchestrator 主线 item 为 session 级 orchestrator thread，
    /// 子代理 item 为对应 task thread。前端 projection 用它作为单一路由键。
    pub source_thread_id: ThreadId,
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
    /// item 归属的 thread。orchestrator 主线 item 为 session 级 orchestrator thread；
    /// 子代理 item 为对应 task thread。单一路由键，前端按此 + thread 的 `role_id`
    /// 判定主线 / task 详情归属。
    pub source_thread_id: ThreadId,
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
}

fn default_true() -> bool {
    true
}

impl ActiveExecutionTurn {
    pub fn normalize(&mut self) {
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
    ArchiveActiveExecutionChain,
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
    /// P6 Thread 原语注册表：按 session 聚合 `ExecutionThread`。orchestrator thread
    /// 随 session 常驻；worker thread 绑定单个 task 执行，不跨 task 复用。
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
/// - `Idle`：该 thread 当前无 in-flight lease；worker thread 到达终态后保留为审计事实。
/// - `Retired`：mission 结束或显式回收，不再可被复用；保留为只读历史。
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionThreadStatus {
    Active,
    Idle,
    Retired,
}

/// Thread 实体：承载 task 执行归属与 UI 可见性锚点。
///
/// orchestrator thread 绑定 session 主线；worker thread 绑定单次 task + worker 实例，
/// 不按 role 复用。这样当前 task 的执行事实不会被历史 tool-call 上下文污染。
///
/// `mission_id` 为必填。Session 首次接收 user 输入时通过 `ensure_session_mission`
/// 创建该 session 的常驻 mission，并同时 spawn `role_id = ORCHESTRATOR_ROLE_ID`
/// 的主线 thread；后续每次任务派发也复用这同一个 mission。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionThread {
    pub thread_id: ThreadId,
    pub session_id: SessionId,
    pub mission_id: MissionId,
    pub role_id: String,
    pub worker_instance_id: WorkerId,
    pub status: ExecutionThreadStatus,
    pub created_at: UtcMillis,
    pub last_used_at: UtcMillis,
    /// 该 thread 处理过的 task 序列，用于调试 / UI 呈现时间线；worker thread 通常只有一个。
    #[serde(default)]
    pub handled_task_ids: Vec<TaskId>,
    /// thread 内部的 LLM 对话审计 / 恢复记录。它只属于当前 thread，不能作为同 role
    /// 下一 task 的执行上下文。
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
