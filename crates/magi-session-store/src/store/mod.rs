mod queries;
mod sidecar;

#[cfg(test)]
mod tests;

use crate::models::{
    NotificationRecord, SessionDurableState, SessionExecutionSidecarStoreState,
    SessionRecord, SessionSidecarFlushReason, SessionStoreState, TimelineEntry,
    TimelineEntryKind,
};
use magi_core::{
    DomainError, DomainResult, SessionId, SessionLifecycleStatus, UtcMillis,
};
use std::sync::{Arc, RwLock};

#[derive(Clone, Debug, Default)]
struct SidecarFlushState {
    current_version: u64,
    flushed_version: u64,
    last_dirty_at: Option<UtcMillis>,
    last_dirty_reason: Option<SessionSidecarFlushReason>,
    last_flush_at: Option<UtcMillis>,
    next_flush_hint: Option<UtcMillis>,
}

#[derive(Clone, Debug)]
pub struct SessionStore {
    state: Arc<RwLock<SessionStoreState>>,
    sidecar_flush_state: Arc<RwLock<SidecarFlushState>>,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self {
            state: Arc::new(RwLock::new(SessionStoreState::default())),
            sidecar_flush_state: Arc::new(RwLock::new(SidecarFlushState::default())),
        }
    }
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_state(state: SessionStoreState) -> Self {
        Self {
            state: Arc::new(RwLock::new(state)),
            sidecar_flush_state: Arc::new(RwLock::new(SidecarFlushState::default())),
        }
    }

    pub fn from_persisted_parts(
        durable_state: SessionDurableState,
        execution_sidecar_store: SessionExecutionSidecarStoreState,
    ) -> Self {
        Self::from_state(SessionStoreState::from_persisted_parts(
            durable_state,
            execution_sidecar_store,
        ))
    }

    fn mark_sidecar_dirty(&self, reason: SessionSidecarFlushReason) {
        let mut flush_state = self
            .sidecar_flush_state
            .write()
            .expect("session sidecar flush state write lock poisoned");
        flush_state.current_version = flush_state.current_version.saturating_add(1);
        let now = UtcMillis::now();
        flush_state.last_dirty_at = Some(now);
        flush_state.last_dirty_reason = Some(reason);
        flush_state.next_flush_hint = Some(now);
    }

    pub fn create_session(
        &self,
        session_id: SessionId,
        title: impl Into<String>,
    ) -> DomainResult<SessionRecord> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if state.sessions.iter().any(|session| session.session_id == session_id) {
            return Err(DomainError::AlreadyExists { entity: "session" });
        }

        let title = title.into();
        let now = UtcMillis::now();
        let session = SessionRecord {
            session_id: session_id.clone(),
            title: title.clone(),
            status: SessionLifecycleStatus::Active,
            created_at: now,
            updated_at: now,
            message_count: None,
        };
        state.sessions.push(session.clone());
        state.current_session_id = Some(session_id.clone());
        state.timeline.push(TimelineEntry {
            entry_id: format!("timeline-session-created-{}", session_id),
            session_id,
            kind: TimelineEntryKind::SessionCreated,
            message: format!("会话已创建: {}", title),
            occurred_at: now,
        });
        Ok(session)
    }

    pub fn rename_session(
        &self,
        session_id: &SessionId,
        title: impl Into<String>,
    ) -> DomainResult<SessionRecord> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let new_title = title.into();
        let session = state
            .sessions
            .iter_mut()
            .find(|session| &session.session_id == session_id)
            .ok_or(DomainError::NotFound { entity: "session" })?;
        session.title = new_title.clone();
        session.updated_at = UtcMillis::now();
        let updated = session.clone();
        state.timeline.push(TimelineEntry {
            entry_id: format!("timeline-session-renamed-{}", session_id),
            session_id: session_id.clone(),
            kind: TimelineEntryKind::SessionRenamed,
            message: format!("会话已重命名: {}", new_title),
            occurred_at: updated.updated_at,
        });
        Ok(updated)
    }

    pub fn archive_session(&self, session_id: &SessionId) -> DomainResult<SessionRecord> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let session = state
            .sessions
            .iter_mut()
            .find(|session| &session.session_id == session_id)
            .ok_or(DomainError::NotFound { entity: "session" })?;
        session.status = SessionLifecycleStatus::Archived;
        session.updated_at = UtcMillis::now();
        let archived = session.clone();
        state.timeline.push(TimelineEntry {
            entry_id: format!("timeline-session-archived-{}", session_id),
            session_id: session_id.clone(),
            kind: TimelineEntryKind::SessionArchived,
            message: "会话已归档".to_string(),
            occurred_at: archived.updated_at,
        });
        if state.current_session_id.as_ref() == Some(session_id) {
            state.current_session_id = state
                .sessions
                .iter()
                .filter(|session| session.status == SessionLifecycleStatus::Active)
                .map(|session| session.session_id.clone())
                .min_by(|left, right| left.as_str().cmp(right.as_str()));
        }
        Ok(archived)
    }

    pub fn delete_session(&self, session_id: &SessionId) -> DomainResult<()> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let before_len = state.sessions.len();
        state.sessions.retain(|session| &session.session_id != session_id);
        if state.sessions.len() == before_len {
            return Err(DomainError::NotFound { entity: "session" });
        }
        state.timeline.retain(|entry| &entry.session_id != session_id);
        state
            .notifications
            .retain(|notification| &notification.session_id != session_id);
        let removed_sidecar = state.execution_sidecar_store.runtime_sidecar(session_id).is_some();
        state.execution_sidecar_store.remove_runtime_sidecar(session_id);
        if state.current_session_id.as_ref() == Some(session_id) {
            state.current_session_id = state
                .sessions
                .iter()
                .map(|session| session.session_id.clone())
                .min_by(|left, right| left.as_str().cmp(right.as_str()));
        }
        drop(state);
        if removed_sidecar {
            self.mark_sidecar_dirty(SessionSidecarFlushReason::DeleteSession);
        }
        Ok(())
    }

    pub fn switch_session(&self, session_id: &SessionId) -> DomainResult<()> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if !state.sessions.iter().any(|session| &session.session_id == session_id) {
            return Err(DomainError::NotFound { entity: "session" });
        }
        let occurred_at = UtcMillis::now();
        state.current_session_id = Some(session_id.clone());
        state.timeline.push(TimelineEntry {
            entry_id: format!("timeline-session-switched-{}-{}", session_id, occurred_at.0),
            session_id: session_id.clone(),
            kind: TimelineEntryKind::SessionSwitched,
            message: "当前会话已切换".to_string(),
            occurred_at,
        });
        Ok(())
    }

    pub fn append_timeline_entry(
        &self,
        session_id: SessionId,
        kind: TimelineEntryKind,
        message: impl Into<String>,
    ) {
        self.state
            .write()
            .expect("session state write lock poisoned")
            .timeline
            .push(TimelineEntry {
                entry_id: format!("timeline-{}-{}", session_id, UtcMillis::now().0),
                session_id,
                kind,
                message: message.into(),
                occurred_at: UtcMillis::now(),
            });
    }

    pub fn append_notification(
        &self,
        session_id: SessionId,
        notification_id: impl Into<String>,
        kind: impl Into<String>,
        message: impl Into<String>,
    ) {
        let notification = NotificationRecord {
            notification_id: notification_id.into(),
            session_id: session_id.clone(),
            kind: kind.into(),
            message: message.into(),
            created_at: UtcMillis::now(),
            handled: false,
        };
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        state.notifications.push(notification.clone());
        state.timeline.push(TimelineEntry {
            entry_id: format!("timeline-notification-{}", notification.notification_id),
            session_id,
            kind: TimelineEntryKind::NotificationPublished,
            message: format!("通知已生成: {}", notification.kind),
            occurred_at: notification.created_at,
        });
    }

    pub fn mark_notification_handled(&self, notification_id: &str) -> DomainResult<()> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let notification = state
            .notifications
            .iter_mut()
            .find(|notification| notification.notification_id == notification_id)
            .ok_or(DomainError::NotFound {
                entity: "notification",
            })?;
        notification.handled = true;
        Ok(())
    }

    pub fn remove_notification(&self, notification_id: &str) -> DomainResult<()> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let removed = state
            .notifications
            .iter()
            .position(|notification| notification.notification_id == notification_id)
            .ok_or(DomainError::NotFound {
                entity: "notification",
            })?;
        state.notifications.remove(removed);
        Ok(())
    }

    pub fn clear_notifications_for_session(&self, session_id: &SessionId) -> usize {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let before = state.notifications.len();
        state
            .notifications
            .retain(|notification| &notification.session_id != session_id);
        before.saturating_sub(state.notifications.len())
    }

    pub fn mark_notifications_handled_for_session(&self, session_id: &SessionId) {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        for notification in state
            .notifications
            .iter_mut()
            .filter(|notification| &notification.session_id == session_id)
        {
            notification.handled = true;
        }
    }

    pub fn remove_notification_for_session(
        &self,
        session_id: &SessionId,
        notification_id: &str,
    ) -> DomainResult<()> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let removed = state
            .notifications
            .iter()
            .position(|notification| {
                &notification.session_id == session_id
                    && notification.notification_id == notification_id
            })
            .ok_or(DomainError::NotFound {
                entity: "notification",
            })?;
        state.notifications.remove(removed);
        Ok(())
    }
}
