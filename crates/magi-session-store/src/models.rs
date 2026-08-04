use magi_core::{
    AccessProfile, DomainError, DomainResult, ExecutionOwnership, GoalId, LeaseId, MissionId,
    PlanId, PlanItem, PlanState, SessionId, SessionLifecycleStatus, TaskId, ThreadId, UtcMillis,
    WorkerId, WorkspaceId,
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionRecord {
    pub session_id: SessionId,
    pub title: String,
    pub status: SessionLifecycleStatus,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completed_at: Option<UtcMillis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_viewed_at: Option<UtcMillis>,
}

impl SessionRecord {
    pub fn has_unread_completion(&self) -> bool {
        self.last_completed_at.is_some_and(|completed_at| {
            self.last_viewed_at
                .is_none_or(|viewed_at| completed_at > viewed_at)
        })
    }
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

pub struct ActiveExecutionBranchSnapshotUpdate {
    pub task_id: TaskId,
    pub worker_id: WorkerId,
    pub stage: String,
    pub lease_id: Option<LeaseId>,
    pub execution_intent_ref: Option<String>,
    pub binding_lifecycle: Option<String>,
    pub checkpoint_stage: Option<String>,
    pub next_step_index: Option<usize>,
    pub checkpoint_at: Option<UtcMillis>,
    pub resume_mode: Option<String>,
    pub resume_token: Option<String>,
}

pub const CANONICAL_TURN_SCHEMA_VERSION: &str = "canonical-turn.v1";

/// `source_thread_id` 的可见性判定结果：
/// - `Main`：对应 session 的 orchestrator thread，item 归属主线时间线
/// - `TaskDetail`：对应某条代理 task thread，item 归属该 task 详情
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
    Interrupted,
    Cancelled,
    Superseded,
}

impl CanonicalTurnStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Interrupted | Self::Cancelled | Self::Superseded
        )
    }

    pub fn allows_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        match self {
            Self::Pending => matches!(
                next,
                Self::Running
                    | Self::Completed
                    | Self::Blocked
                    | Self::Failed
                    | Self::Interrupted
                    | Self::Cancelled
            ),
            Self::Running => matches!(
                next,
                Self::Completed
                    | Self::Blocked
                    | Self::Failed
                    | Self::Interrupted
                    | Self::Cancelled
            ),
            Self::Blocked => matches!(
                next,
                Self::Running
                    | Self::Completed
                    | Self::Failed
                    | Self::Interrupted
                    | Self::Cancelled
            ),
            Self::Cancelled => matches!(next, Self::Superseded),
            Self::Completed | Self::Failed | Self::Interrupted | Self::Superseded => false,
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
    TurnSuperseded,
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
    /// 代理 item 为对应 task thread。前端 projection 用它作为单一路由键。
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
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline_entry_id: Option<String>,
    /// item 归属的 thread。orchestrator 主线 item 为 session 级 orchestrator thread；
    /// 代理 item 为对应 task thread。单一路由键，前端按此 + thread 的 `role_id`
    /// 判定主线 / task 详情归属。
    pub source_thread_id: ThreadId,
}

impl ActiveExecutionTurnItem {
    pub fn requested_renderable(&self) -> Option<bool> {
        self.metadata.get("renderable").and_then(Value::as_bool)
    }
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
#[serde(deny_unknown_fields)]
pub struct SessionRuntimeSidecar {
    pub session_id: SessionId,
    pub ownership: ExecutionOwnership,
    #[serde(default)]
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
    pub current_status: SessionExecutionSidecarStatus,
    pub last_update: UtcMillis,
    pub ownership: ExecutionOwnership,
    pub execution_chain_ref: Option<String>,
    #[serde(default)]
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
    UpdatePlan,
    ClearPlan,
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
    fn sort_runtime_sidecars(runtime_sidecars: &mut [SessionRuntimeSidecar]) {
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
    SessionArchived,
    NotificationPublished,
    SystemNote,
    UserMessage,
    AssistantMessage,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TimelineEntry {
    pub entry_id: String,
    pub session_id: SessionId,
    pub kind: TimelineEntryKind,
    pub message: String,
    pub occurred_at: UtcMillis,
}

pub fn timeline_entry_visible_text(message: &str) -> Option<String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(Default)]
pub enum NotificationScope {
    App,
    Workspace,
    #[default]
    Session,
}

fn default_notification_occurrence_count() -> u32 {
    1
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotificationRecord {
    pub notification_id: String,
    #[serde(default)]
    pub scope: NotificationScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 经字段级脱敏后的直接错误文本。
    pub message: String,
    /// 可选的附加诊断详情，例如调用栈；不能替代 `message` 中的直接错误。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub created_at: UtcMillis,
    pub handled: bool,
    #[serde(default = "default_true")]
    pub action_required: bool,
    #[serde(default = "default_true")]
    pub count_unread: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fingerprint: String,
    #[serde(default = "default_notification_occurrence_count")]
    pub occurrence_count: u32,
    #[serde(default)]
    pub resolved: bool,
}

impl NotificationRecord {
    pub fn is_incident(&self) -> bool {
        self.kind == "incident"
    }

    pub fn normalize_incident(&mut self) {
        self.kind = "incident".to_string();
        self.occurrence_count = self.occurrence_count.max(1);
        match self.scope {
            NotificationScope::App => {
                self.workspace_id = None;
                self.session_id = None;
            }
            NotificationScope::Workspace => {
                self.session_id = None;
            }
            NotificationScope::Session => {}
        }
    }

    pub fn visible_in_context(&self, workspace_id: &str, session_id: Option<&SessionId>) -> bool {
        if !self.is_incident() {
            return false;
        }
        match self.scope {
            NotificationScope::App => true,
            NotificationScope::Workspace => self.workspace_id.as_deref() == Some(workspace_id),
            NotificationScope::Session => {
                self.workspace_id.as_deref() == Some(workspace_id)
                    && self.session_id.as_ref() == session_id
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

impl GoalStatus {
    pub fn is_unfinished(self) -> bool {
        matches!(
            self,
            Self::Active | Self::Paused | Self::Blocked | Self::UsageLimited | Self::BudgetLimited
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalBlockerState {
    pub blocker_key: String,
    pub reason: String,
    pub consecutive_turns: u32,
    pub last_observed_turn_id: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoalContinuationPhase {
    #[default]
    Idle,
    Running,
    Waiting,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalContinuationState {
    #[serde(default)]
    pub phase: GoalContinuationPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalCompletionRecord {
    pub turn_id: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_revision: Option<u64>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub completed_at: UtcMillis,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionGoal {
    pub goal_id: GoalId,
    pub session_id: SessionId,
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_turn_id: Option<String>,
    pub objective: String,
    pub status: GoalStatus,
    #[serde(default = "default_goal_control_revision")]
    pub control_revision: u64,
    #[serde(default)]
    pub access_profile: AccessProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub tokens_used: u64,
    #[serde(default)]
    pub time_used_seconds: u64,
    #[serde(default)]
    pub time_used_millis: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing_started_at: Option<UtcMillis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<GoalBlockerState>,
    #[serde(default)]
    pub continuation: GoalContinuationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<GoalCompletionRecord>,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

#[derive(Clone, Debug)]
pub struct InterruptedGoalResumeCheckpoint {
    pub(crate) goal_before: SessionGoal,
    pub(crate) plan_before: Option<SessionPlan>,
    pub(crate) applied_goal_revision: u64,
    pub(crate) applied_plan_revision: Option<u64>,
    pub(crate) resumed_turn_id: String,
}

#[derive(Clone, Debug)]
pub struct GoalResumeCheckpoint {
    pub(crate) goal_before: SessionGoal,
    pub(crate) plan_before: Option<SessionPlan>,
    pub(crate) applied_goal_revision: u64,
    pub(crate) applied_plan_revision: Option<u64>,
}

fn default_goal_control_revision() -> u64 {
    1
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionPlan {
    #[serde(default = "empty_plan_id")]
    pub plan_id: PlanId,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<GoalId>,
    #[serde(default = "default_plan_revision")]
    pub revision: u64,
    #[serde(default = "default_plan_language")]
    pub language: String,
    #[serde(default)]
    pub state: PlanState,
    pub items: Vec<PlanItem>,
    #[serde(default)]
    pub task_bindings: HashMap<TaskId, magi_core::PlanItemId>,
    #[serde(default)]
    pub task_statuses: HashMap<TaskId, magi_core::TaskStatus>,
    pub updated_at: UtcMillis,
}

fn empty_plan_id() -> PlanId {
    PlanId::new("")
}

fn default_plan_revision() -> u64 {
    1
}

fn default_plan_language() -> String {
    "zh-CN".to_string()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionStoreState {
    pub current_session_id: Option<SessionId>,
    pub sessions: Vec<SessionRecord>,
    pub timeline: Vec<TimelineEntry>,
    #[serde(default)]
    pub canonical_turns: Vec<CanonicalTurn>,
    pub notifications: Vec<NotificationRecord>,
    #[serde(default)]
    pub goals: Vec<SessionGoal>,
    #[serde(default, alias = "todo_lists")]
    pub plans: Vec<SessionPlan>,
    #[serde(default, flatten)]
    pub execution_sidecar_store: SessionExecutionSidecarStoreState,
    /// P6 Thread 原语注册表：按 session 聚合 `ExecutionThread`。orchestrator thread
    /// 随 session 常驻；worker thread 绑定单个 task 执行，不跨 task 复用。
    #[serde(skip, default)]
    pub thread_registry: Vec<ExecutionThread>,
    #[serde(skip, default)]
    pub thread_context_checkpoints: Vec<ThreadContextCheckpoint>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionDurableState {
    pub current_session_id: Option<SessionId>,
    pub sessions: Vec<SessionRecord>,
    pub timeline: Vec<TimelineEntry>,
    #[serde(default)]
    pub canonical_turns: Vec<CanonicalTurn>,
    pub notifications: Vec<NotificationRecord>,
    #[serde(default)]
    pub goals: Vec<SessionGoal>,
    #[serde(default, alias = "todo_lists")]
    pub plans: Vec<SessionPlan>,
    #[serde(default)]
    pub thread_registry: Vec<ExecutionThread>,
    #[serde(default)]
    pub thread_context_checkpoints: Vec<ThreadContextCheckpoint>,
}

impl SessionDurableState {
    pub fn is_empty(&self) -> bool {
        self.current_session_id.is_none()
            && self.sessions.is_empty()
            && self.timeline.is_empty()
            && self.canonical_turns.is_empty()
            && self.notifications.is_empty()
            && self.goals.is_empty()
            && self.plans.is_empty()
            && self.thread_registry.is_empty()
            && self.thread_context_checkpoints.is_empty()
    }

    pub fn append_state(&mut self, other: SessionDurableState) {
        if self.current_session_id.is_none() {
            self.current_session_id = other.current_session_id.clone();
        }
        self.append_state_without_current(other);
    }

    pub fn append_state_without_current(&mut self, other: SessionDurableState) {
        self.sessions.extend(other.sessions);
        self.timeline.extend(other.timeline);
        self.canonical_turns.extend(other.canonical_turns);
        self.notifications.extend(other.notifications);
        self.goals.extend(other.goals);
        self.plans.extend(other.plans);
        self.thread_registry.extend(other.thread_registry);
        self.thread_context_checkpoints
            .extend(other.thread_context_checkpoints);
    }

    pub fn clear_current_session_if_owned_by_workspace_states(
        &mut self,
        workspace_states: &HashMap<String, SessionDurableState>,
    ) {
        let Some(current_session_id) = self.current_session_id.as_ref() else {
            return;
        };
        if workspace_states.values().any(|state| {
            state
                .sessions
                .iter()
                .any(|session| &session.session_id == current_session_id)
        }) {
            self.current_session_id = None;
        }
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
                    goals: Vec::new(),
                    plans: Vec::new(),
                    thread_registry: Vec::new(),
                    thread_context_checkpoints: Vec::new(),
                },
            );
        }

        let mut global_state = SessionDurableState {
            // 当前打开的会话是 daemon 级唯一导航选择，不属于任一 workspace 的业务历史。
            // 统一写入全局状态，避免多个 workspace 文件各自携带旧选择并在重启时互相覆盖。
            current_session_id: self.current_session_id.clone(),
            sessions: global_sessions,
            timeline: Vec::new(),
            canonical_turns: Vec::new(),
            notifications: Vec::new(),
            goals: Vec::new(),
            plans: Vec::new(),
            thread_registry: Vec::new(),
            thread_context_checkpoints: Vec::new(),
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

        for notification in self.notifications.iter().filter(|item| item.is_incident()) {
            match notification.scope {
                NotificationScope::App => global_state.notifications.push(notification.clone()),
                NotificationScope::Workspace => {
                    if let Some(workspace_id) = notification.workspace_id.as_deref() {
                        workspace_states
                            .entry(workspace_id.to_string())
                            .or_default()
                            .notifications
                            .push(notification.clone());
                    }
                }
                NotificationScope::Session => {
                    let Some(session_id) = notification.session_id.as_ref() else {
                        continue;
                    };
                    if global_session_ids.contains(session_id) {
                        global_state.notifications.push(notification.clone());
                        continue;
                    }
                    for (workspace_id, session_ids) in &workspace_session_ids {
                        if session_ids.contains(session_id) {
                            workspace_states
                                .get_mut(workspace_id)
                                .expect("workspace durable state should exist")
                                .notifications
                                .push(notification.clone());
                            break;
                        }
                    }
                }
            }
        }

        for goal in &self.goals {
            if global_session_ids.contains(&goal.session_id) {
                global_state.goals.push(goal.clone());
                continue;
            }
            for (workspace_id, session_ids) in &workspace_session_ids {
                if session_ids.contains(&goal.session_id) {
                    workspace_states
                        .get_mut(workspace_id)
                        .expect("workspace durable state should exist")
                        .goals
                        .push(goal.clone());
                    break;
                }
            }
        }

        for plan in &self.plans {
            if global_session_ids.contains(&plan.session_id) {
                global_state.plans.push(plan.clone());
                continue;
            }
            for (workspace_id, session_ids) in &workspace_session_ids {
                if session_ids.contains(&plan.session_id) {
                    workspace_states
                        .get_mut(workspace_id)
                        .expect("workspace durable state should exist")
                        .plans
                        .push(plan.clone());
                    break;
                }
            }
        }

        for thread in &self.thread_registry {
            if global_session_ids.contains(&thread.session_id) {
                global_state.thread_registry.push(thread.clone());
                continue;
            }
            for (workspace_id, session_ids) in &workspace_session_ids {
                if session_ids.contains(&thread.session_id) {
                    workspace_states
                        .get_mut(workspace_id)
                        .expect("workspace durable state should exist")
                        .thread_registry
                        .push(thread.clone());
                    break;
                }
            }
        }

        let global_thread_ids = global_state
            .thread_registry
            .iter()
            .map(|thread| thread.thread_id.clone())
            .collect::<HashSet<_>>();
        for checkpoint in &self.thread_context_checkpoints {
            if global_thread_ids.contains(&checkpoint.thread_id) {
                global_state
                    .thread_context_checkpoints
                    .push(checkpoint.clone());
                continue;
            }
            let workspace_id = workspace_states
                .iter()
                .find(|(_, state)| {
                    state
                        .thread_registry
                        .iter()
                        .any(|thread| thread.thread_id == checkpoint.thread_id)
                })
                .map(|(workspace_id, _)| workspace_id.clone());
            if let Some(workspace_id) = workspace_id {
                workspace_states
                    .get_mut(&workspace_id)
                    .expect("workspace durable state should exist")
                    .thread_context_checkpoints
                    .push(checkpoint.clone());
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
        let mut notifications = durable_state
            .notifications
            .into_iter()
            .filter(NotificationRecord::is_incident)
            .collect::<Vec<_>>();
        for notification in &mut notifications {
            notification.normalize_incident();
        }
        let mut goals = normalize_session_goals(durable_state.goals);
        let mut plans = durable_state.plans;
        normalize_session_plans(&mut plans);
        bind_unfinished_goal_plans(&goals, &mut plans);
        normalize_goal_plan_control_states(&mut goals, &mut plans);
        Self {
            current_session_id: durable_state.current_session_id,
            sessions: durable_state.sessions,
            timeline,
            canonical_turns,
            notifications,
            goals,
            plans,
            execution_sidecar_store,
            thread_registry: durable_state.thread_registry,
            thread_context_checkpoints: durable_state.thread_context_checkpoints,
        }
    }

    pub fn durable_state(&self) -> SessionDurableState {
        SessionDurableState {
            current_session_id: self.current_session_id.clone(),
            sessions: self.sessions.clone(),
            timeline: self.timeline.clone(),
            canonical_turns: self.canonical_turns.clone(),
            notifications: self.notifications.clone(),
            goals: self.goals.clone(),
            plans: self.plans.clone(),
            thread_registry: self.thread_registry.clone(),
            thread_context_checkpoints: self.thread_context_checkpoints.clone(),
        }
    }
}

fn normalize_goal_plan_control_states(goals: &mut [SessionGoal], plans: &mut [SessionPlan]) {
    for goal in goals.iter_mut().filter(|goal| goal.status.is_unfinished()) {
        let Some(plan) = plans.iter_mut().find(|plan| {
            plan.session_id == goal.session_id && plan.goal_id.as_ref() == Some(&goal.goal_id)
        }) else {
            continue;
        };
        if goal.status == GoalStatus::Active && plan.state != PlanState::Active {
            goal.status = GoalStatus::Paused;
            goal.blocker = None;
            goal.continuation = GoalContinuationState::default();
            goal.control_revision = goal.control_revision.saturating_add(1);
            goal.updated_at = goal.updated_at.max(plan.updated_at);
        } else if goal.status != GoalStatus::Active && plan.state == PlanState::Active {
            plan.state = PlanState::Paused;
            plan.revision = plan.revision.saturating_add(1);
            plan.updated_at = plan.updated_at.max(goal.updated_at);
        }
    }
}

fn normalize_session_goals(goals: Vec<SessionGoal>) -> Vec<SessionGoal> {
    let mut by_session = HashMap::<SessionId, Vec<SessionGoal>>::new();
    for mut goal in goals {
        if goal.control_revision == 0 {
            goal.control_revision = 1;
        }
        by_session
            .entry(goal.session_id.clone())
            .or_default()
            .push(goal);
    }

    let mut current = Vec::with_capacity(by_session.len());
    for (_, mut candidates) in by_session {
        candidates.sort_by(|left, right| {
            left.updated_at
                .0
                .cmp(&right.updated_at.0)
                .then_with(|| left.goal_id.as_str().cmp(right.goal_id.as_str()))
        });
        let selected = candidates.pop();
        if let Some(selected) = selected {
            current.push(selected);
        }
    }
    current.sort_by(|left, right| left.session_id.as_str().cmp(right.session_id.as_str()));
    current
}

fn bind_unfinished_goal_plans(goals: &[SessionGoal], plans: &mut [SessionPlan]) {
    for plan in plans {
        if let Some(goal) = goals.iter().find(|goal| goal.session_id == plan.session_id)
            && (plan.goal_id.is_some() || goal.status.is_unfinished())
        {
            plan.goal_id = Some(goal.goal_id.clone());
        }
    }
}

fn normalize_session_plans(plans: &mut [SessionPlan]) {
    for plan in plans {
        if plan.plan_id.as_str().trim().is_empty() {
            plan.plan_id = PlanId::new(format!("plan-{}", plan.session_id));
        }
        if plan.revision == 0 {
            plan.revision = 1;
        }
        if plan.language.trim().is_empty() {
            plan.language = default_plan_language();
        }
        for (index, item) in plan.items.iter_mut().enumerate() {
            if item.item_id.as_str().trim().is_empty() {
                item.item_id =
                    magi_core::PlanItemId::new(format!("{}-item-{}", plan.plan_id, index + 1));
            }
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
// --- P6 Thread 原语（Y 方案）

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
    /// 当前模型在该 thread 上已确认的上下文窗口；用于跨轮次和重启后的预算门禁。
    #[serde(default)]
    pub observed_context_window_tokens: Option<u64>,
    /// 该 thread 处理过的 task 序列，用于调试 / UI 呈现时间线；worker thread 通常只有一个。
    #[serde(default)]
    pub handled_task_ids: Vec<TaskId>,
    /// thread 内部的 LLM 对话审计 / 恢复记录。它只属于当前 thread，不能作为同 role
    /// 下一 task 的执行上下文。
    #[serde(default)]
    pub message_history: Vec<ThreadChatMessage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadContextCheckpoint {
    pub thread_id: ThreadId,
    pub checkpoint_id: String,
    /// 检查点摘要覆盖的原始 transcript 消息数量。
    pub source_message_count: usize,
    pub summary_message: ThreadChatMessage,
    pub reason: String,
    pub original_token_estimate: usize,
    pub checkpoint_token_estimate: usize,
    pub created_at: UtcMillis,
    #[serde(default)]
    pub file_fact_versions: Vec<ThreadFileFactVersion>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ThreadFileFactVersion {
    pub path: String,
    pub content_hash: String,
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
    pub images: Vec<ThreadChatImageSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ThreadChatToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// 模型提供方要求随 assistant 消息跨轮持久化并原样回放的私有上下文。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_context: Vec<ThreadModelProviderContext>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadModelProviderContext {
    pub provider: String,
    pub kind: String,
    pub data: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ThreadChatImageSource {
    #[serde(rename = "type")]
    pub kind: String,
    pub media_type: String,
    pub data: String,
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
