use super::{SessionStore, cmp_sessions_newest_first, with_session_message_count};
use crate::models::{
    ActiveExecutionChain, NotificationRecord, SessionDurableState,
    SessionExecutionSidecarStoreState, SessionProjectionInput, SessionRecord,
    SessionRuntimeSidecar, SessionRuntimeSidecarExport, SessionSidecarFlushMetadata, TimelineEntry,
};
use magi_core::{ExecutionOwnership, SessionId};
use std::collections::HashSet;

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
        state.sessions.sort_by(cmp_sessions_newest_first);
        state.timeline.sort_by(|left, right| {
            left.occurred_at
                .0
                .cmp(&right.occurred_at.0)
                .then_with(|| left.entry_id.cmp(&right.entry_id))
        });
        state.canonical_turns.sort_by(|left, right| {
            left.turn_seq
                .cmp(&right.turn_seq)
                .then_with(|| left.turn_id.cmp(&right.turn_id))
        });
        state.notifications.sort_by(|left, right| {
            left.created_at
                .0
                .cmp(&right.created_at.0)
                .then_with(|| left.notification_id.cmp(&right.notification_id))
        });
        let timeline = state.timeline.clone();
        let sessions = state
            .sessions
            .into_iter()
            .map(|session| with_session_message_count(session, &timeline))
            .collect();
        SessionProjectionInput {
            current_session_id: state.current_session_id,
            sessions,
            timeline,
            canonical_turns: state.canonical_turns,
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

    /// 汇总 session 历史与当前运行态引用过的全部 TaskId。canonical turn 是持久化事实，
    /// 因此 daemon 重启后即使进程内 thread registry 已清空，删除会话仍能定位任务树。
    pub fn execution_task_ids_for_session(&self, session_id: &SessionId) -> Vec<magi_core::TaskId> {
        let state = self.state.read().expect("session state read lock poisoned");
        let mut task_ids = HashSet::new();
        for turn in state
            .canonical_turns
            .iter()
            .filter(|turn| &turn.session_id == session_id)
        {
            task_ids.extend(
                turn.items
                    .iter()
                    .filter_map(|item| item.worker.as_ref()?.task_id.clone()),
            );
        }
        for thread in state
            .thread_registry
            .iter()
            .filter(|thread| &thread.session_id == session_id)
        {
            task_ids.extend(thread.handled_task_ids.iter().cloned());
        }
        if let Some(sidecar) = state.execution_sidecar_store.runtime_sidecar(session_id) {
            task_ids.extend(sidecar.ownership.task_id.iter().cloned());
            if let Some(chain) = sidecar.active_execution_chain.as_ref() {
                task_ids.insert(chain.root_task_id.clone());
                task_ids.extend(chain.branches.iter().map(|branch| branch.task_id.clone()));
            }
        }
        let mut task_ids = task_ids.into_iter().collect::<Vec<_>>();
        task_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        task_ids
    }

    pub fn current_session(&self) -> Option<SessionRecord> {
        let state = self.state.read().expect("session state read lock poisoned");
        state.current_session_id.as_ref().and_then(|session_id| {
            state
                .sessions
                .iter()
                .find(|session| &session.session_id == session_id)
                .cloned()
                .map(|session| with_session_message_count(session, &state.timeline))
        })
    }

    pub fn session(&self, session_id: &SessionId) -> Option<SessionRecord> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .sessions
            .iter()
            .find(|session| &session.session_id == session_id)
            .cloned()
            .map(|session| with_session_message_count(session, &state.timeline))
    }

    pub fn sessions(&self) -> Vec<SessionRecord> {
        let state = self.state.read().expect("session state read lock poisoned");
        let mut sessions = state.sessions.clone();
        sessions.sort_by(cmp_sessions_newest_first);
        sessions
            .into_iter()
            .map(|session| with_session_message_count(session, &state.timeline))
            .collect()
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

    pub fn canonical_turns_for_session(
        &self,
        session_id: &SessionId,
    ) -> Vec<crate::models::CanonicalTurn> {
        let mut turns = self
            .state
            .read()
            .expect("session state read lock poisoned")
            .canonical_turns
            .iter()
            .filter(|turn| &turn.session_id == session_id)
            .cloned()
            .collect::<Vec<_>>();
        for turn in &mut turns {
            turn.normalize();
        }
        turns.sort_by(|left, right| {
            left.turn_seq
                .cmp(&right.turn_seq)
                .then_with(|| left.turn_id.cmp(&right.turn_id))
        });
        turns
    }

    pub fn recent_turn_messages(&self, session_id: &SessionId, limit: usize) -> Vec<String> {
        let mut entries = self.timeline_for_session(session_id);
        entries.reverse();
        let mut messages = entries
            .into_iter()
            .take(limit)
            .filter_map(|entry| crate::timeline_entry_visible_text(&entry.message))
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

    pub fn notifications_for_context(
        &self,
        workspace_id: &str,
        session_id: Option<&SessionId>,
    ) -> Vec<NotificationRecord> {
        let mut notifications = self
            .state
            .read()
            .expect("session state read lock poisoned")
            .notifications
            .iter()
            .filter(|notification| notification.visible_in_context(workspace_id, session_id))
            .cloned()
            .collect::<Vec<_>>();
        notifications.sort_by(|left, right| {
            right
                .created_at
                .0
                .cmp(&left.created_at.0)
                .then_with(|| right.notification_id.cmp(&left.notification_id))
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
        self.runtime_sidecar(session_id)
            .map(|sidecar| sidecar.export_view())
    }

    pub fn active_execution_chain(&self, session_id: &SessionId) -> Option<ActiveExecutionChain> {
        self.runtime_sidecar(session_id)
            .and_then(|sidecar| sidecar.active_execution_chain)
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
