use magi_core::{
    ExecutionOwnership, LeaseId, MissionId, SessionId, SessionLifecycleStatus, TaskId, UtcMillis,
    WorkerId, WorkspaceId,
};
use serde::{Deserialize, Serialize};
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

impl SessionDurableState {
    pub fn is_empty(&self) -> bool {
        self.current_session_id.is_none()
            && self.sessions.is_empty()
            && self.timeline.is_empty()
            && self.notifications.is_empty()
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
