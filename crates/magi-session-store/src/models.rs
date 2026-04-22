use magi_core::{
    ExecutionOwnership, LeaseId, MissionId, SessionId, SessionLifecycleStatus, TaskId, UtcMillis,
    WorkerId, WorkspaceId,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    #[serde(default, alias = "message_count", skip_serializing_if = "Option::is_none")]
    pub message_count: Option<usize>,
    #[serde(default, alias = "workspace_id", skip_serializing_if = "Option::is_none")]
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
    pub last_checkpoint_at: UtcMillis,
    pub use_tools: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_name: Option<String>,
    #[serde(default)]
    pub is_primary: bool,
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
}

impl ActiveExecutionChain {
    pub fn normalize(&mut self) {
        self.active_branch_task_ids
            .sort_by(|left, right| left.as_str().cmp(right.as_str()));
        self.active_branch_task_ids.dedup_by(|left, right| left == right);
        self.active_worker_bindings
            .sort_by(|left, right| left.as_str().cmp(right.as_str()));
        self.active_worker_bindings.dedup_by(|left, right| left == right);
        self.branches.sort_by(|left, right| {
            left.task_id
                .as_str()
                .cmp(right.task_id.as_str())
                .then_with(|| left.worker_id.as_str().cmp(right.worker_id.as_str()))
        });
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionRuntimeSidecar {
    pub session_id: SessionId,
    pub ownership: ExecutionOwnership,
    #[serde(default, alias = "recovery_ref")]
    pub recovery_id: Option<String>,
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
        runtime_sidecars.sort_by(|left, right| left.session_id.as_str().cmp(right.session_id.as_str()));
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationRecord {
    #[serde(alias = "notification_id")]
    pub notification_id: String,
    #[serde(alias = "session_id")]
    pub session_id: SessionId,
    pub kind: String,
    pub message: String,
    #[serde(alias = "created_at")]
    pub created_at: UtcMillis,
    pub handled: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionStoreState {
    pub current_session_id: Option<SessionId>,
    pub sessions: Vec<SessionRecord>,
    pub timeline: Vec<TimelineEntry>,
    pub notifications: Vec<NotificationRecord>,
    #[serde(default, flatten)]
    pub execution_sidecar_store: SessionExecutionSidecarStoreState,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionDurableState {
    pub current_session_id: Option<SessionId>,
    pub sessions: Vec<SessionRecord>,
    pub timeline: Vec<TimelineEntry>,
    pub notifications: Vec<NotificationRecord>,
}

impl SessionStoreState {
    fn normalize_timeline_entry_ids(timeline: &mut [TimelineEntry]) {
        let mut seen = HashMap::<String, usize>::new();
        for entry in timeline.iter_mut() {
            let original = entry.entry_id.clone();
            let duplicate_index = seen.entry(original.clone()).or_insert(0);
            if *duplicate_index > 0 {
                entry.entry_id = format!(
                    "{}-{}-{}",
                    original,
                    entry.occurred_at.0,
                    duplicate_index
                );
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
        Self {
            current_session_id: durable_state.current_session_id,
            sessions: durable_state.sessions,
            timeline,
            notifications: durable_state.notifications,
            execution_sidecar_store,
        }
    }

    pub fn durable_state(&self) -> SessionDurableState {
        SessionDurableState {
            current_session_id: self.current_session_id.clone(),
            sessions: self.sessions.clone(),
            timeline: self.timeline.clone(),
            notifications: self.notifications.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionProjectionInput {
    pub current_session_id: Option<SessionId>,
    pub sessions: Vec<SessionRecord>,
    pub timeline: Vec<TimelineEntry>,
    pub notifications: Vec<NotificationRecord>,
}
