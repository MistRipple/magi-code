use super::SessionStore;
use crate::models::{
    NotificationRecord, SessionDurableState, SessionExecutionSidecarStoreState,
    SessionProjectionInput, SessionRecord, SessionRuntimeSidecar, SessionRuntimeSidecarExport,
    SessionSidecarFlushMetadata, TimelineEntry,
};
use magi_core::{ExecutionOwnership, SessionId};

impl SessionStore {
    pub fn export_state(&self) -> crate::models::SessionStoreState {
        self.state
            .read()
            .expect("session state read lock poisoned")
            .clone()
    }

    pub fn durable_state(&self) -> SessionDurableState {
        self.export_state().durable_state()
    }

    pub fn projection_input(&self) -> SessionProjectionInput {
        let mut state = self.export_state();
        state
            .sessions
            .sort_by(|left, right| left.session_id.as_str().cmp(right.session_id.as_str()));
        state.timeline.sort_by(|left, right| {
            left.occurred_at
                .0
                .cmp(&right.occurred_at.0)
                .then_with(|| left.entry_id.cmp(&right.entry_id))
        });
        state.notifications.sort_by(|left, right| {
            left.created_at
                .0
                .cmp(&right.created_at.0)
                .then_with(|| left.notification_id.cmp(&right.notification_id))
        });
        SessionProjectionInput {
            current_session_id: state.current_session_id,
            sessions: state.sessions,
            timeline: state.timeline,
            notifications: state.notifications,
        }
    }

    pub fn session_index(&self) -> Vec<SessionId> {
        let mut session_ids = self
            .state
            .read()
            .expect("session state read lock poisoned")
            .sessions
            .iter()
            .map(|session| session.session_id.clone())
            .collect::<Vec<_>>();
        session_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        session_ids.dedup();
        session_ids
    }

    pub fn current_session(&self) -> Option<SessionRecord> {
        let state = self
            .state
            .read()
            .expect("session state read lock poisoned");
        state.current_session_id.as_ref().and_then(|session_id| {
            state
                .sessions
                .iter()
                .find(|session| &session.session_id == session_id)
                .cloned()
        })
    }

    pub fn session(&self, session_id: &SessionId) -> Option<SessionRecord> {
        self.state
            .read()
            .expect("session state read lock poisoned")
            .sessions
            .iter()
            .find(|session| &session.session_id == session_id)
            .cloned()
    }

    pub fn sessions(&self) -> Vec<SessionRecord> {
        let mut sessions = self
            .state
            .read()
            .expect("session state read lock poisoned")
            .sessions
            .clone();
        sessions.sort_by(|left, right| left.session_id.as_str().cmp(right.session_id.as_str()));
        sessions
    }

    pub fn timeline(&self) -> Vec<TimelineEntry> {
        let mut timeline = self
            .state
            .read()
            .expect("session state read lock poisoned")
            .timeline
            .clone();
        timeline.sort_by(|left, right| {
            left.occurred_at
                .0
                .cmp(&right.occurred_at.0)
                .then_with(|| left.entry_id.cmp(&right.entry_id))
        });
        timeline
    }

    pub fn timeline_for_session(&self, session_id: &SessionId) -> Vec<TimelineEntry> {
        let mut entries = self
            .state
            .read()
            .expect("session state read lock poisoned")
            .timeline
            .iter()
            .filter(|entry| &entry.session_id == session_id)
            .cloned()
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.occurred_at.0);
        entries
    }

    pub fn recent_turn_messages(&self, session_id: &SessionId, limit: usize) -> Vec<String> {
        let mut entries = self.timeline_for_session(session_id);
        entries.reverse();
        let mut messages = entries
            .into_iter()
            .take(limit)
            .map(|entry| entry.message)
            .collect::<Vec<_>>();
        messages.reverse();
        messages
    }

    pub fn notifications(&self) -> Vec<NotificationRecord> {
        let mut notifications = self
            .state
            .read()
            .expect("session state read lock poisoned")
            .notifications
            .clone();
        notifications.sort_by(|left, right| {
            left.created_at
                .0
                .cmp(&right.created_at.0)
                .then_with(|| left.notification_id.cmp(&right.notification_id))
        });
        notifications
    }

    pub fn notifications_for_session(&self, session_id: &SessionId) -> Vec<NotificationRecord> {
        let mut notifications = self
            .state
            .read()
            .expect("session state read lock poisoned")
            .notifications
            .iter()
            .filter(|notification| &notification.session_id == session_id)
            .cloned()
            .collect::<Vec<_>>();
        notifications.sort_by(|left, right| {
            left.created_at
                .0
                .cmp(&right.created_at.0)
                .then_with(|| left.notification_id.cmp(&right.notification_id))
        });
        notifications
    }

    pub fn is_empty(&self) -> bool {
        self.state
            .read()
            .expect("session state read lock poisoned")
            .sessions
            .is_empty()
    }

    pub fn runtime_sidecar(&self, session_id: &SessionId) -> Option<SessionRuntimeSidecar> {
        self.state
            .read()
            .expect("session state read lock poisoned")
            .execution_sidecar_store
            .runtime_sidecar(session_id)
    }

    pub fn active_execution_sidecars(&self) -> Vec<SessionRuntimeSidecar> {
        self.state
            .read()
            .expect("session state read lock poisoned")
            .execution_sidecar_store
            .active_runtime_sidecars()
    }

    pub fn execution_sidecar_exports(&self) -> Vec<SessionRuntimeSidecarExport> {
        self.state
            .read()
            .expect("session state read lock poisoned")
            .execution_sidecar_store
            .export_views()
    }

    pub fn execution_sidecar_export(
        &self,
        session_id: &SessionId,
    ) -> Option<SessionRuntimeSidecarExport> {
        self.runtime_sidecar(session_id).map(|sidecar| sidecar.export_view())
    }

    pub fn execution_ownership(&self, session_id: &SessionId) -> Option<ExecutionOwnership> {
        self.runtime_sidecar(session_id)
            .map(|sidecar| sidecar.ownership)
    }

    pub fn recovery_id(&self, session_id: &SessionId) -> Option<String> {
        self.runtime_sidecar(session_id)
            .and_then(|sidecar| sidecar.recovery_id)
    }

    pub fn recovery_ref(&self, session_id: &SessionId) -> Option<String> {
        self.recovery_id(session_id)
    }

    pub fn execution_sidecar_store_state(&self) -> SessionExecutionSidecarStoreState {
        self.state
            .read()
            .expect("session state read lock poisoned")
            .execution_sidecar_store
            .clone()
    }

    pub fn execution_sidecar_flush_metadata(&self) -> SessionSidecarFlushMetadata {
        let flush_state = self
            .sidecar_flush_state
            .read()
            .expect("session sidecar flush state read lock poisoned");
        SessionSidecarFlushMetadata {
            current_version: flush_state.current_version,
            flushed_version: flush_state.flushed_version,
            last_dirty_at: flush_state.last_dirty_at,
            last_dirty_reason: flush_state.last_dirty_reason.clone(),
            last_flush_at: flush_state.last_flush_at,
            next_flush_hint: if flush_state.current_version == flush_state.flushed_version {
                None
            } else {
                flush_state.next_flush_hint.or(flush_state.last_dirty_at)
            },
        }
    }

    pub fn runtime_sidecars(&self) -> Vec<SessionRuntimeSidecar> {
        self.state
            .read()
            .expect("session state read lock poisoned")
            .execution_sidecar_store
            .runtime_sidecars()
    }
}
