mod queries;
mod sidecar;

#[cfg(test)]
mod tests;

use crate::lifecycle::SessionLifecycleObserver;
use crate::models::{
    ExecutionThread, ExecutionThreadStatus, NotificationRecord, SessionDurableState,
    SessionExecutionSidecarStoreState, SessionRecord, SessionSidecarFlushReason,
    SessionStoreState, TimelineEntry, TimelineEntryKind,
};
use magi_core::{
    DomainError, DomainResult, SessionId, SessionLifecycleStatus, ThreadId, UtcMillis, WorkerId,
};
use std::sync::{Arc, RwLock};

/// P6c：orchestrator 主线 thread 的稳定 role 标识。
///
/// Session 创建即 spawn 一条 `role_id = ORCHESTRATOR_ROLE_ID` 的常驻 thread，
/// 作为"主线对话"身份锚点。与 worker role 体系正交 —— 这是产品级的主干角色，
/// 不会被 `DynamicWorkerCatalog` 识别为可派发 worker。
pub const ORCHESTRATOR_ROLE_ID: &str = "orchestrator";

#[derive(Clone, Debug, Default)]
struct SidecarFlushState {
    current_version: u64,
    flushed_version: u64,
    last_dirty_at: Option<UtcMillis>,
    last_dirty_reason: Option<SessionSidecarFlushReason>,
    last_flush_at: Option<UtcMillis>,
    next_flush_hint: Option<UtcMillis>,
}

#[derive(Clone)]
pub struct SessionStore {
    state: Arc<RwLock<SessionStoreState>>,
    sidecar_flush_state: Arc<RwLock<SidecarFlushState>>,
    lifecycle_observer: Arc<RwLock<Option<Arc<dyn SessionLifecycleObserver>>>>,
}

impl std::fmt::Debug for SessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionStore")
            .field("state", &"<state>")
            .finish()
    }
}

fn unique_timeline_entry_id(existing: &[TimelineEntry], base: String) -> String {
    if !existing.iter().any(|entry| entry.entry_id == base) {
        return base;
    }

    let mut suffix = 1usize;
    loop {
        let candidate = format!("{base}-{suffix}");
        if !existing.iter().any(|entry| entry.entry_id == candidate) {
            return candidate;
        }
        suffix = suffix.saturating_add(1);
    }
}

fn user_message_count_for_session(timeline: &[TimelineEntry], session_id: &SessionId) -> usize {
    timeline
        .iter()
        .filter(|entry| {
            &entry.session_id == session_id && matches!(entry.kind, TimelineEntryKind::UserMessage)
        })
        .count()
}

fn with_session_message_count(
    mut session: SessionRecord,
    timeline: &[TimelineEntry],
) -> SessionRecord {
    session.message_count = Some(user_message_count_for_session(
        timeline,
        &session.session_id,
    ));
    session
}

impl Default for SessionStore {
    fn default() -> Self {
        Self {
            state: Arc::new(RwLock::new(SessionStoreState::default())),
            sidecar_flush_state: Arc::new(RwLock::new(SidecarFlushState::default())),
            lifecycle_observer: Arc::new(RwLock::new(None)),
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
            lifecycle_observer: Arc::new(RwLock::new(None)),
        }
    }

    pub fn from_persisted_parts(
        durable_state: SessionDurableState,
        execution_sidecar_store: SessionExecutionSidecarStoreState,
    ) -> Self {
        let mut state =
            SessionStoreState::from_persisted_parts(durable_state, execution_sidecar_store);
        sidecar::restore_canonical_turns_from_sidecars(&mut state)
            .expect("persisted sidecar current turn should be canonical-compatible");
        Self::from_state(state)
    }

    /// 安装 session 生命周期 observer。每个 store 同一时间只挂一个 observer，
    /// magi-api 启动时由 wiring 层装配；后挂的会替换前一个。
    pub fn set_lifecycle_observer(&self, observer: Arc<dyn SessionLifecycleObserver>) {
        *self
            .lifecycle_observer
            .write()
            .expect("session lifecycle observer write lock poisoned") = Some(observer);
    }

    fn lifecycle_observer(&self) -> Option<Arc<dyn SessionLifecycleObserver>> {
        self.lifecycle_observer
            .read()
            .expect("session lifecycle observer read lock poisoned")
            .clone()
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
        self.create_session_for_workspace(session_id, title, None)
    }

    pub fn create_session_for_workspace(
        &self,
        session_id: SessionId,
        title: impl Into<String>,
        workspace_id: Option<String>,
    ) -> DomainResult<SessionRecord> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if state
            .sessions
            .iter()
            .any(|session| session.session_id == session_id)
        {
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
            workspace_id: workspace_id.clone(),
        };
        state.sessions.push(session.clone());
        state.current_session_id = Some(session_id.clone());
        state.timeline.push(TimelineEntry {
            entry_id: format!("timeline-session-created-{}", session_id),
            session_id: session_id.clone(),
            kind: TimelineEntryKind::SessionCreated,
            message: format!("会话已创建: {}", title),
            occurred_at: now,
        });
        // P6c：session 创建时即 spawn 一条 orchestrator 主线 thread。
        // 其 mission_id = None 表示跨 mission 存在，作为"主线对话"身份锚点。
        // 所有 thread-visible 主线 item 语义上都归属这条 thread，
        // 未来主线 LLM 也可通过 ExecutionThread.message_history 累积跨 turn 上下文。
        state.thread_registry.push(ExecutionThread {
            thread_id: ThreadId::new(format!("thread-orchestrator-{}", session_id)),
            session_id: session_id.clone(),
            mission_id: None,
            role_id: ORCHESTRATOR_ROLE_ID.to_string(),
            worker_instance_id: WorkerId::new(format!("worker-orchestrator-{}", session_id)),
            status: ExecutionThreadStatus::Idle,
            created_at: now,
            last_used_at: now,
            handled_task_ids: Vec::new(),
            message_history: Vec::new(),
        });
        drop(state);
        if let Some(observer) = self.lifecycle_observer() {
            observer.on_session_created(&session_id, workspace_id.as_deref());
        }
        Ok(session)
    }

    /// 按 workspace_id 过滤返回会话列表
    pub fn sessions_for_workspace(&self, workspace_id: &str) -> Vec<SessionRecord> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .sessions
            .iter()
            .filter(|s| s.workspace_id.as_deref() == Some(workspace_id))
            .cloned()
            .map(|session| with_session_message_count(session, &state.timeline))
            .collect()
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
        let entry_id = unique_timeline_entry_id(
            &state.timeline,
            format!("timeline-session-renamed-{}", session_id),
        );
        state.timeline.push(TimelineEntry {
            entry_id,
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
        drop(state);
        if let Some(observer) = self.lifecycle_observer() {
            observer.on_session_archived(session_id);
        }
        Ok(archived)
    }

    pub fn delete_session(&self, session_id: &SessionId) -> DomainResult<()> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let before_len = state.sessions.len();
        state
            .sessions
            .retain(|session| &session.session_id != session_id);
        if state.sessions.len() == before_len {
            return Err(DomainError::NotFound { entity: "session" });
        }
        state
            .timeline
            .retain(|entry| &entry.session_id != session_id);
        state
            .notifications
            .retain(|notification| &notification.session_id != session_id);
        let removed_sidecar = state
            .execution_sidecar_store
            .runtime_sidecar(session_id)
            .is_some();
        state
            .execution_sidecar_store
            .remove_runtime_sidecar(session_id);
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
        if let Some(observer) = self.lifecycle_observer() {
            observer.on_session_deleted(session_id);
        }
        Ok(())
    }

    pub fn switch_session(&self, session_id: &SessionId) -> DomainResult<()> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if !state
            .sessions
            .iter()
            .any(|session| &session.session_id == session_id)
        {
            return Err(DomainError::NotFound { entity: "session" });
        }
        let occurred_at = UtcMillis::now();
        state.current_session_id = Some(session_id.clone());
        let entry_id = unique_timeline_entry_id(
            &state.timeline,
            format!("timeline-session-switched-{}-{}", session_id, occurred_at.0),
        );
        state.timeline.push(TimelineEntry {
            entry_id,
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
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let occurred_at = UtcMillis::now();
        let entry_id = unique_timeline_entry_id(
            &state.timeline,
            format!("timeline-{}-{}", session_id, occurred_at.0),
        );
        state.timeline.push(TimelineEntry {
            entry_id,
            session_id: session_id.clone(),
            kind,
            message: message.into(),
            occurred_at,
        });
        if let Some(session) = state
            .sessions
            .iter_mut()
            .find(|session| session.session_id == session_id)
        {
            session.updated_at = occurred_at;
        }
    }

    /// 按 entry_id 更新已有 timeline 条目的内容，不存在则插入新条目。
    /// 用于流式 LLM 输出时增量更新 timeline 中的 AssistantMessage。
    pub fn upsert_timeline_entry(
        &self,
        session_id: SessionId,
        entry_id: &str,
        kind: TimelineEntryKind,
        message: impl Into<String>,
    ) {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let now = UtcMillis::now();
        let message_str = message.into();

        if let Some(entry) = state
            .timeline
            .iter_mut()
            .find(|entry| entry.entry_id == entry_id)
        {
            entry.message = message_str;
            entry.kind = kind;
        } else {
            state.timeline.push(TimelineEntry {
                entry_id: entry_id.to_string(),
                session_id: session_id.clone(),
                kind,
                message: message_str,
                occurred_at: now,
            });
        }

        if let Some(session) = state
            .sessions
            .iter_mut()
            .find(|session| session.session_id == session_id)
        {
            session.updated_at = now;
        }
    }

    pub fn remove_timeline_entry(&self, session_id: &SessionId, entry_id: &str) -> bool {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let before_len = state.timeline.len();
        state
            .timeline
            .retain(|entry| !(entry.session_id == *session_id && entry.entry_id == entry_id));
        before_len != state.timeline.len()
    }

    pub fn append_notification(
        &self,
        session_id: SessionId,
        notification_id: impl Into<String>,
        kind: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.append_notification_record(NotificationRecord {
            notification_id: notification_id.into(),
            session_id: session_id.clone(),
            kind: kind.into(),
            level: None,
            title: None,
            message: message.into(),
            source: None,
            created_at: UtcMillis::now(),
            handled: false,
            persist_to_center: Some(true),
            action_required: None,
            count_unread: None,
            display_mode: None,
            duration: None,
        });
    }

    pub fn append_notification_record(&self, notification: NotificationRecord) {
        let session_id = notification.session_id.clone();
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        state.notifications.push(notification.clone());
        let entry_id = unique_timeline_entry_id(
            &state.timeline,
            format!("timeline-notification-{}", notification.notification_id),
        );
        state.timeline.push(TimelineEntry {
            entry_id,
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
