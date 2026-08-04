mod goals;
mod queries;
mod sidecar;

#[cfg(test)]
mod tests;

use crate::lifecycle::SessionLifecycleObserver;
use crate::models::{
    NotificationRecord, NotificationScope, SessionDurableState, SessionExecutionSidecarStoreState,
    SessionPlan, SessionRecord, SessionSidecarFlushReason, SessionStoreState, TimelineEntry,
    TimelineEntryKind,
};
use magi_core::{DomainError, DomainResult, SessionId, SessionLifecycleStatus, UtcMillis};
use std::sync::{Arc, Mutex, RwLock};

/// orchestrator 主线 thread 的稳定 role 标识。
///
/// Session 首次接收 user 输入时通过 `ensure_session_mission` 创建 mission
/// 并 spawn 一条 `role_id = ORCHESTRATOR_ROLE_ID` 的常驻 thread，作为"主线
/// 对话"身份锚点。与 worker role 体系正交 —— 这是产品级的主干角色，
/// 不会被 `DynamicWorkerCatalog` 识别为可派发 worker。
pub const ORCHESTRATOR_ROLE_ID: &str = "orchestrator";
pub const SESSION_TITLE_MAX_CHARS: usize = 40;
const MAX_INCIDENT_NOTIFICATION_RECORDS: usize = 1_000;

#[derive(Clone, Debug, Default)]
struct SidecarFlushState {
    current_version: u64,
    flushed_version: u64,
    last_dirty_at: Option<UtcMillis>,
    last_dirty_reason: Option<SessionSidecarFlushReason>,
    last_flush_at: Option<UtcMillis>,
    next_flush_hint: Option<UtcMillis>,
}

fn normalize_session_title(title: String) -> DomainResult<String> {
    let title = title.trim();
    if title.is_empty() {
        return Err(DomainError::Validation {
            message: "会话名称不能为空".to_string(),
        });
    }
    if title.chars().any(char::is_control) {
        return Err(DomainError::Validation {
            message: "会话名称不能包含换行或控制字符".to_string(),
        });
    }
    if title.chars().count() > SESSION_TITLE_MAX_CHARS {
        return Err(DomainError::Validation {
            message: format!("会话名称不能超过 {SESSION_TITLE_MAX_CHARS} 个字符"),
        });
    }
    Ok(title.to_string())
}

#[derive(Clone)]
pub struct SessionStore {
    state: Arc<RwLock<SessionStoreState>>,
    durable_persistence_lock: Arc<Mutex<()>>,
    sidecar_flush_state: Arc<RwLock<SidecarFlushState>>,
    sidecar_flush_lock: Arc<Mutex<()>>,
    lifecycle_observer: Arc<RwLock<Option<Arc<dyn SessionLifecycleObserver>>>>,
}

#[derive(Clone, Debug)]
pub struct TimelineEntryInput {
    pub entry_id: String,
    pub kind: TimelineEntryKind,
    pub message: String,
    pub occurred_at: UtcMillis,
}

impl TimelineEntryInput {
    pub fn new(
        entry_id: impl Into<String>,
        kind: TimelineEntryKind,
        message: impl Into<String>,
        occurred_at: UtcMillis,
    ) -> Self {
        Self {
            entry_id: entry_id.into(),
            kind,
            message: message.into(),
            occurred_at,
        }
    }
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

fn prune_incident_notifications(notifications: &mut Vec<NotificationRecord>) {
    while notifications.len() > MAX_INCIDENT_NOTIFICATION_RECORDS {
        let removal_index = notifications
            .iter()
            .enumerate()
            .filter(|(_, record)| record.resolved)
            .min_by_key(|(_, record)| record.created_at.0)
            .map(|(index, _)| index)
            .or_else(|| {
                notifications
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, record)| record.created_at.0)
                    .map(|(index, _)| index)
            })
            .expect("notification retention requires a non-empty collection");
        notifications.remove(removal_index);
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

/// 会话列表统一按"更新时间倒序"排序：最近活跃的会话排在最前。
/// updated_at 相同则回退到 created_at 倒序，最后才用 session_id 倒序作为
/// 稳定的 tie-breaker，保证测试期确定性。
pub(crate) fn cmp_sessions_newest_first(
    left: &SessionRecord,
    right: &SessionRecord,
) -> std::cmp::Ordering {
    right
        .updated_at
        .0
        .cmp(&left.updated_at.0)
        .then_with(|| right.created_at.0.cmp(&left.created_at.0))
        .then_with(|| right.session_id.as_str().cmp(left.session_id.as_str()))
}

impl Default for SessionStore {
    fn default() -> Self {
        Self {
            state: Arc::new(RwLock::new(SessionStoreState::default())),
            durable_persistence_lock: Arc::new(Mutex::new(())),
            sidecar_flush_state: Arc::new(RwLock::new(SidecarFlushState::default())),
            sidecar_flush_lock: Arc::new(Mutex::new(())),
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
            durable_persistence_lock: Arc::new(Mutex::new(())),
            sidecar_flush_state: Arc::new(RwLock::new(SidecarFlushState::default())),
            sidecar_flush_lock: Arc::new(Mutex::new(())),
            lifecycle_observer: Arc::new(RwLock::new(None)),
        }
    }

    pub fn from_persisted_parts(
        durable_state: SessionDurableState,
        execution_sidecar_store: SessionExecutionSidecarStoreState,
    ) -> Self {
        let mut state =
            SessionStoreState::from_persisted_parts(durable_state, execution_sidecar_store);
        prune_incident_notifications(&mut state.notifications);
        sidecar::restore_canonical_turns_from_sidecars(&mut state)
            .expect("persisted sidecar current turn should be canonical-compatible");
        sidecar::reconcile_terminal_goal_continuations(&mut state);
        sidecar::reconcile_goal_response_duration_scopes(&mut state);
        Self::from_state(state)
    }

    /// 串行化完整 durable snapshot 持久化事务。快照必须在锁内生成，避免较早请求
    /// 延迟写入旧快照，覆盖之后已经完成的会话选择或业务状态。
    pub fn persist_durable_state_with<T, E>(
        &self,
        persist: impl FnOnce(SessionDurableState) -> Result<T, E>,
    ) -> Result<T, E> {
        let _persistence_guard = self
            .durable_persistence_lock
            .lock()
            .expect("session durable persistence lock poisoned");
        persist(self.durable_state())
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
        self.create_session_for_workspace_at(session_id, title, workspace_id, UtcMillis::now())
    }

    pub fn create_session_for_workspace_at(
        &self,
        session_id: SessionId,
        title: impl Into<String>,
        workspace_id: Option<String>,
        created_at: UtcMillis,
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
        let session = SessionRecord {
            session_id: session_id.clone(),
            title: title.clone(),
            status: SessionLifecycleStatus::Active,
            created_at,
            updated_at: created_at,
            message_count: None,
            workspace_id: workspace_id.clone(),
            last_completed_at: None,
            last_viewed_at: None,
        };
        state.sessions.push(session.clone());
        state.current_session_id = Some(session_id.clone());
        state.timeline.push(TimelineEntry {
            entry_id: format!("timeline-session-created-{}", session_id),
            session_id: session_id.clone(),
            kind: TimelineEntryKind::SessionCreated,
            message: format!("会话已创建: {}", title),
            occurred_at: created_at,
        });
        drop(state);
        if let Some(observer) = self.lifecycle_observer() {
            observer.on_session_created(&session_id, workspace_id.as_deref());
        }
        Ok(session)
    }

    pub fn mark_session_viewed(&self, session_id: &SessionId) -> DomainResult<SessionRecord> {
        self.mark_session_viewed_at(session_id, UtcMillis::now())
    }

    /// 更新用户当前打开的会话。该操作只改变导航选择，不写 timeline，
    /// 也不修改会话业务时间，避免浏览行为影响会话排序。
    pub fn select_current_session(&self, session_id: &SessionId) -> DomainResult<SessionRecord> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let session = state
            .sessions
            .iter()
            .find(|session| &session.session_id == session_id)
            .cloned()
            .ok_or(DomainError::NotFound { entity: "session" })?;
        state.current_session_id = Some(session_id.clone());
        Ok(with_session_message_count(session, &state.timeline))
    }

    pub fn mark_session_viewed_at(
        &self,
        session_id: &SessionId,
        viewed_at: UtcMillis,
    ) -> DomainResult<SessionRecord> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let session = state
            .sessions
            .iter_mut()
            .find(|session| &session.session_id == session_id)
            .ok_or(DomainError::NotFound { entity: "session" })?;
        if session
            .last_viewed_at
            .is_none_or(|last_viewed_at| viewed_at > last_viewed_at)
        {
            session.last_viewed_at = Some(viewed_at);
        }
        Ok(session.clone())
    }

    /// 按 workspace_id 过滤返回会话列表，按更新时间倒序排序（最近活跃在前）。
    pub fn sessions_for_workspace(&self, workspace_id: &str) -> Vec<SessionRecord> {
        let state = self.state.read().expect("session state read lock poisoned");
        let mut sessions: Vec<SessionRecord> = state
            .sessions
            .iter()
            .filter(|s| s.workspace_id.as_deref() == Some(workspace_id))
            .cloned()
            .map(|session| with_session_message_count(session, &state.timeline))
            .collect();
        sessions.sort_by(cmp_sessions_newest_first);
        sessions
    }

    pub fn rename_session(
        &self,
        session_id: &SessionId,
        title: impl Into<String>,
    ) -> DomainResult<SessionRecord> {
        let new_title = normalize_session_title(title.into())?;
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let session = state
            .sessions
            .iter_mut()
            .find(|session| &session.session_id == session_id)
            .ok_or(DomainError::NotFound { entity: "session" })?;
        if session.title == new_title {
            return Ok(session.clone());
        }
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
            .retain(|notification| notification.session_id.as_ref() != Some(session_id));
        state
            .canonical_turns
            .retain(|turn| &turn.session_id != session_id);
        state.goals.retain(|goal| &goal.session_id != session_id);
        state.plans.retain(|plan| &plan.session_id != session_id);
        state
            .thread_registry
            .retain(|thread| &thread.session_id != session_id);
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

    pub fn upsert_plan(
        &self,
        session_id: &SessionId,
        plan: SessionPlan,
        expected_revision: Option<u64>,
    ) -> DomainResult<SessionPlan> {
        self.upsert_plan_inner(session_id, plan, expected_revision, None)
    }

    pub fn upsert_plan_for_goal_progress(
        &self,
        session_id: &SessionId,
        plan: SessionPlan,
        expected_revision: Option<u64>,
        expected_goal_id: Option<magi_core::GoalId>,
        expected_goal_control_revision: Option<u64>,
    ) -> DomainResult<SessionPlan> {
        self.upsert_plan_inner(
            session_id,
            plan,
            expected_revision,
            Some((expected_goal_id, expected_goal_control_revision)),
        )
    }

    fn upsert_plan_inner(
        &self,
        session_id: &SessionId,
        mut plan: SessionPlan,
        expected_revision: Option<u64>,
        goal_progress_guard: Option<(Option<magi_core::GoalId>, Option<u64>)>,
    ) -> DomainResult<SessionPlan> {
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
        if &plan.session_id != session_id {
            return Err(DomainError::Validation {
                message: "计划 session_id 与写入作用域不一致".to_string(),
            });
        }
        let unfinished_goal = state
            .goals
            .iter()
            .find(|goal| &goal.session_id == session_id && goal.status.is_unfinished());
        if let Some(goal) = unfinished_goal {
            if plan.goal_id.as_ref() != Some(&goal.goal_id) {
                return Err(DomainError::InvalidState {
                    message: "unfinished goal plan must be bound to the current goal".to_string(),
                });
            }
            if plan.state == magi_core::PlanState::Active
                && goal.status != crate::models::GoalStatus::Active
            {
                return Err(DomainError::InvalidState {
                    message: "non-active goal cannot have an active plan".to_string(),
                });
            }
            if goal.status == crate::models::GoalStatus::Active
                && plan.state == magi_core::PlanState::Paused
            {
                return Err(DomainError::InvalidState {
                    message: "active goal plan cannot be paused independently".to_string(),
                });
            }
        }
        if let Some((expected_goal_id, expected_goal_control_revision)) = goal_progress_guard {
            match (
                unfinished_goal,
                expected_goal_id.as_ref(),
                expected_goal_control_revision,
            ) {
                (None, None, None) if plan.goal_id.is_none() => {}
                (None, _, _) => {
                    return Err(DomainError::InvalidState {
                        message: "goal no longer exists".to_string(),
                    });
                }
                (Some(goal), None, _) => {
                    return Err(DomainError::InvalidState {
                        message: format!("goal id is required; current goal is {}", goal.goal_id),
                    });
                }
                (Some(goal), Some(expected_goal_id), _) if &goal.goal_id != expected_goal_id => {
                    return Err(DomainError::InvalidState {
                        message: format!(
                            "goal id conflict: expected {}, current {}",
                            expected_goal_id, goal.goal_id
                        ),
                    });
                }
                (Some(goal), Some(_), None) => {
                    return Err(DomainError::InvalidState {
                        message: format!(
                            "goal control revision is required; current revision is {}",
                            goal.control_revision
                        ),
                    });
                }
                (Some(goal), Some(_), Some(expected_goal_control_revision)) => {
                    if goal.control_revision != expected_goal_control_revision {
                        return Err(DomainError::InvalidState {
                            message: format!(
                                "goal revision conflict: expected {}, current {}",
                                expected_goal_control_revision, goal.control_revision
                            ),
                        });
                    }
                    if goal.status != crate::models::GoalStatus::Active {
                        return Err(DomainError::InvalidState {
                            message: format!(
                                "goal is not active and cannot advance its plan: {:?}",
                                goal.status
                            ),
                        });
                    }
                }
            }
        }
        if let Some(plan_goal_id) = plan.goal_id.as_ref()
            && !state
                .goals
                .iter()
                .any(|goal| &goal.session_id == session_id && &goal.goal_id == plan_goal_id)
        {
            return Err(DomainError::InvalidState {
                message: "plan references a non-current goal".to_string(),
            });
        }
        let now = UtcMillis::now();
        if let Some(current) = state
            .plans
            .iter_mut()
            .find(|candidate| &candidate.session_id == session_id)
        {
            if let Some(expected_revision) = expected_revision
                && current.revision != expected_revision
            {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "计划版本冲突：期望 revision={}，当前 revision={}",
                        expected_revision, current.revision
                    ),
                });
            }
            plan.revision = current.revision.saturating_add(1);
            plan.updated_at = now;
            *current = plan.clone();
        } else {
            if expected_revision.is_some_and(|revision| revision != 0) {
                return Err(DomainError::InvalidState {
                    message: "计划不存在，expectedRevision 必须为 0 或省略".to_string(),
                });
            }
            plan.revision = 1;
            plan.updated_at = now;
            state.plans.push(plan.clone());
        }
        if let Some(session) = state
            .sessions
            .iter_mut()
            .find(|session| &session.session_id == session_id)
        {
            session.updated_at = now;
        }
        drop(state);
        self.mark_sidecar_dirty(SessionSidecarFlushReason::UpdatePlan);
        Ok(plan)
    }

    pub fn clear_plan(
        &self,
        session_id: &SessionId,
        expected_revision: Option<u64>,
    ) -> DomainResult<bool> {
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
        if let Some(expected_revision) = expected_revision
            && let Some(current) = state
                .plans
                .iter()
                .find(|plan| &plan.session_id == session_id)
            && current.revision != expected_revision
        {
            return Err(DomainError::InvalidState {
                message: format!(
                    "计划版本冲突：期望 revision={}，当前 revision={}",
                    expected_revision, current.revision
                ),
            });
        }
        let before = state.plans.len();
        state.plans.retain(|plan| &plan.session_id != session_id);
        let changed = state.plans.len() != before;
        drop(state);
        if changed {
            self.mark_sidecar_dirty(SessionSidecarFlushReason::ClearPlan);
        }
        Ok(changed)
    }

    pub fn plan(&self, session_id: &SessionId) -> Option<SessionPlan> {
        self.state
            .read()
            .expect("session state read lock poisoned")
            .plans
            .iter()
            .find(|plan| &plan.session_id == session_id)
            .cloned()
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

    pub fn append_incident_record(&self, mut notification: NotificationRecord) -> DomainResult<()> {
        notification.normalize_incident();
        match notification.scope {
            NotificationScope::App => {}
            NotificationScope::Workspace if notification.workspace_id.is_none() => {
                return Err(DomainError::Validation {
                    message: "workspace incident requires workspace_id".to_string(),
                });
            }
            NotificationScope::Session
                if notification.workspace_id.is_none() || notification.session_id.is_none() =>
            {
                return Err(DomainError::Validation {
                    message: "session incident requires workspace_id and session_id".to_string(),
                });
            }
            NotificationScope::Workspace | NotificationScope::Session => {}
        }
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        // 通知中心承担错误日志职责：每次发生都独立留痕，不能按 fingerprint
        // 覆盖旧记录，否则用户无法追溯当时的直接错误与发生时间。
        state.notifications.push(notification);
        prune_incident_notifications(&mut state.notifications);
        Ok(())
    }

    pub fn clear_notifications_for_context(
        &self,
        workspace_id: &str,
        session_id: Option<&SessionId>,
    ) -> usize {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let before = state.notifications.len();
        state
            .notifications
            .retain(|notification| !notification.visible_in_context(workspace_id, session_id));
        before.saturating_sub(state.notifications.len())
    }

    pub fn mark_notifications_handled_for_context(
        &self,
        workspace_id: &str,
        session_id: Option<&SessionId>,
    ) {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        for notification in state
            .notifications
            .iter_mut()
            .filter(|notification| notification.visible_in_context(workspace_id, session_id))
        {
            notification.handled = true;
            notification.count_unread = false;
        }
    }

    pub fn remove_notification_for_context(
        &self,
        workspace_id: &str,
        session_id: Option<&SessionId>,
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
                notification.visible_in_context(workspace_id, session_id)
                    && notification.notification_id == notification_id
            })
            .ok_or(DomainError::NotFound {
                entity: "notification",
            })?;
        state.notifications.remove(removed);
        Ok(())
    }

    pub fn resolve_notification_for_context(
        &self,
        workspace_id: &str,
        session_id: Option<&SessionId>,
        notification_id: &str,
    ) -> DomainResult<()> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let notification = state
            .notifications
            .iter_mut()
            .find(|notification| {
                notification.notification_id == notification_id
                    && notification.visible_in_context(workspace_id, session_id)
            })
            .ok_or(DomainError::NotFound {
                entity: "notification",
            })?;
        notification.resolved = true;
        notification.handled = true;
        notification.count_unread = false;
        Ok(())
    }
}
