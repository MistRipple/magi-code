mod goals;
mod queries;
mod sidecar;

#[cfg(test)]
mod tests;

use crate::lifecycle::SessionLifecycleObserver;
use crate::models::{
    CanonicalTurn, NotificationContext, NotificationRecord, NotificationScope,
    SessionAcceptanceRecord, SessionDurableState, SessionExecutionSidecarStoreState, SessionPlan,
    SessionRecord, SessionSidecarFlushReason, SessionStoreState, TimelineEntry, TimelineEntryKind,
};
use magi_core::{DomainError, DomainResult, SessionId, SessionLifecycleStatus, Task, UtcMillis};
use std::sync::{Arc, Mutex, RwLock};

/// canonical 写模型在一次 SessionStore mutation 中准备的原子变更。
///
/// `previous` 必须与事件日志当前重放结果一致；持久化实现据此生成逐 item 事件，
/// 成功后 SessionStore 才会提交 `next` 到内存 projection。
#[derive(Clone, Debug)]
pub struct CanonicalTurnMutation {
    pub previous: Option<CanonicalTurn>,
    pub next: CanonicalTurn,
}

/// Session 生命周期 mutation 事务的失败类型。
///
/// durable projection 写入失败时，SessionStore 不提交候选状态，因此调用方可以安全地
/// 保留原会话并向用户返回错误；只有 projection 成功后才会提交内存状态和生命周期事件。
#[derive(Debug)]
pub enum SessionMutationTransactionError<E> {
    Domain(DomainError),
    Persistence(E),
}

pub trait CanonicalTurnEventWriter: Send + Sync {
    fn append_canonical_turn_transaction(
        &self,
        session_id: &SessionId,
        mutations: &[CanonicalTurnMutation],
    ) -> DomainResult<()>;

    /// 提交一次新的执行轮 accepted 事实。
    ///
    /// 该入口由持久化实现把 accepted 事实与 canonical event 写入同一个事务，
    /// 让 event segment 成为一次提交的唯一 durable 边界。
    fn append_canonical_turn_transaction_with_acceptance(
        &self,
        session_id: &SessionId,
        mutations: &[CanonicalTurnMutation],
        acceptance: &SessionAcceptanceRecord,
        task: &Task,
    ) -> DomainResult<()>;
}

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
    /// 串行化 canonical 事务的准备、事件写入和内存提交，但不占用 session state 写锁。
    ///
    /// canonical event writer 可能执行 fsync；这把锁只阻止另一笔 canonical 事务进入，
    /// 不阻止普通 session 读取或不相关的内存状态操作。
    pub(crate) canonical_commit_lock: Arc<Mutex<()>>,
    sidecar_flush_state: Arc<RwLock<SidecarFlushState>>,
    sidecar_flush_lock: Arc<Mutex<()>>,
    lifecycle_observer: Arc<RwLock<Option<Arc<dyn SessionLifecycleObserver>>>>,
    canonical_event_writer: Arc<RwLock<Option<Arc<dyn CanonicalTurnEventWriter>>>>,
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

fn prepare_session_deletion(
    state: &mut SessionStoreState,
    session_id: &SessionId,
    replacement_session_id: Option<&SessionId>,
) -> DomainResult<bool> {
    let target = state
        .sessions
        .iter()
        .find(|session| &session.session_id == session_id)
        .ok_or(DomainError::NotFound { entity: "session" })?
        .clone();
    let deleting_current = state.current_session_id.as_ref() == Some(session_id);

    if !deleting_current && replacement_session_id.is_some() {
        return Err(DomainError::InvalidState {
            message: format!("删除非 current 会话时不能改写 current: {session_id}"),
        });
    }
    if let Some(replacement_id) = replacement_session_id {
        if replacement_id == session_id {
            return Err(DomainError::InvalidState {
                message: "删除会话的替代 current 不能指向被删除会话".to_string(),
            });
        }
        let replacement = state
            .sessions
            .iter()
            .find(|session| &session.session_id == replacement_id)
            .ok_or(DomainError::NotFound {
                entity: "replacement session",
            })?;
        if replacement.workspace_id != target.workspace_id {
            return Err(DomainError::InvalidState {
                message: "删除会话的替代 current 必须属于同一 workspace".to_string(),
            });
        }
    }

    state
        .sessions
        .retain(|session| &session.session_id != session_id);
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
    if deleting_current {
        state.current_session_id = replacement_session_id.cloned();
    }
    Ok(removed_sidecar)
}

fn prepare_session_rename(
    state: &mut SessionStoreState,
    session_id: &SessionId,
    new_title: &str,
) -> DomainResult<Option<SessionRecord>> {
    let session = state
        .sessions
        .iter_mut()
        .find(|session| &session.session_id == session_id)
        .ok_or(DomainError::NotFound { entity: "session" })?;
    if session.title == new_title {
        return Ok(None);
    }
    session.title = new_title.to_string();
    session.updated_at = UtcMillis::now();
    let updated = session.clone();
    let entry_id = unique_timeline_entry_id(
        &state.timeline,
        format!("timeline-session-renamed-{session_id}"),
    );
    state.timeline.push(TimelineEntry {
        entry_id,
        session_id: session_id.clone(),
        kind: TimelineEntryKind::SessionRenamed,
        message: format!("会话已重命名: {new_title}"),
        occurred_at: updated.updated_at,
    });
    Ok(Some(updated))
}

impl Default for SessionStore {
    fn default() -> Self {
        Self {
            state: Arc::new(RwLock::new(SessionStoreState::default())),
            durable_persistence_lock: Arc::new(Mutex::new(())),
            canonical_commit_lock: Arc::new(Mutex::new(())),
            sidecar_flush_state: Arc::new(RwLock::new(SidecarFlushState::default())),
            sidecar_flush_lock: Arc::new(Mutex::new(())),
            lifecycle_observer: Arc::new(RwLock::new(None)),
            canonical_event_writer: Arc::new(RwLock::new(None)),
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
            canonical_commit_lock: Arc::new(Mutex::new(())),
            sidecar_flush_state: Arc::new(RwLock::new(SidecarFlushState::default())),
            sidecar_flush_lock: Arc::new(Mutex::new(())),
            lifecycle_observer: Arc::new(RwLock::new(None)),
            canonical_event_writer: Arc::new(RwLock::new(None)),
        }
    }

    pub fn from_persisted_parts(
        durable_state: SessionDurableState,
        execution_sidecar_store: SessionExecutionSidecarStoreState,
    ) -> DomainResult<Self> {
        let mut state =
            SessionStoreState::from_persisted_parts(durable_state, execution_sidecar_store);
        prune_incident_notifications(&mut state.notifications);
        sidecar::reconcile_sidecars_from_canonical(&mut state)?;
        sidecar::reconcile_terminal_goal_continuations(&mut state);
        sidecar::reconcile_goal_response_duration_scopes(&mut state);
        Ok(Self::from_state(state))
    }

    pub fn rebuild_sidecar_projection_from_canonical(
        sidecar: &mut crate::models::SessionRuntimeSidecar,
        canonical_turns: &[CanonicalTurn],
    ) -> DomainResult<()> {
        sidecar::rebuild_sidecar_projection_from_canonical(sidecar, canonical_turns)
    }

    /// 一次性 v1 -> v2 转换。旧 timeline/sidecar 兼容只允许存在于此边界。
    pub fn convert_v1_persisted_parts(
        durable_state: SessionDurableState,
        execution_sidecar_store: SessionExecutionSidecarStoreState,
    ) -> DomainResult<Self> {
        let mut state =
            SessionStoreState::from_persisted_parts(durable_state, execution_sidecar_store);
        prune_incident_notifications(&mut state.notifications);
        sidecar::convert_v1_conversation_facts(&mut state)?;
        sidecar::reconcile_sidecars_from_canonical(&mut state)?;
        sidecar::reconcile_terminal_goal_continuations(&mut state);
        sidecar::reconcile_goal_response_duration_scopes(&mut state);
        Ok(Self::from_state(state))
    }

    /// 安装 canonical 权威事件 writer。daemon 恢复完成后、接受任何新 mutation 前安装。
    pub fn install_canonical_event_writer(&self, writer: Arc<dyn CanonicalTurnEventWriter>) {
        *self
            .canonical_event_writer
            .write()
            .expect("canonical event writer lock poisoned") = Some(writer);
    }

    pub(super) fn persist_canonical_mutations(
        &self,
        session_id: &SessionId,
        mutations: &[CanonicalTurnMutation],
    ) -> DomainResult<()> {
        let writer = self
            .canonical_event_writer
            .read()
            .expect("canonical event writer lock poisoned")
            .clone();
        if let Some(writer) = writer {
            writer.append_canonical_turn_transaction(session_id, mutations)?;
        }
        Ok(())
    }

    pub(super) fn persist_canonical_mutations_with_acceptance(
        &self,
        session_id: &SessionId,
        mutations: &[CanonicalTurnMutation],
        acceptance: &SessionAcceptanceRecord,
        task: &Task,
    ) -> DomainResult<()> {
        let writer = self
            .canonical_event_writer
            .read()
            .expect("canonical event writer lock poisoned")
            .clone();
        if let Some(writer) = writer {
            writer.append_canonical_turn_transaction_with_acceptance(
                session_id, mutations, acceptance, task,
            )?;
        }
        Ok(())
    }

    /// 将旧版本 accepted journal 合并回内存状态。
    ///
    /// 旧 journal 只在完整 snapshot 成功后删除，因此这里必须允许它与旧 snapshot
    /// 重复出现；已有终态事实优先，不能被一条过期的恢复记录覆盖。
    pub fn restore_session_acceptance_records(
        &self,
        records: impl IntoIterator<Item = SessionAcceptanceRecord>,
    ) -> DomainResult<usize> {
        // 所有 canonical 事实提交都先取得这把锁，再取得 state 锁。
        // 恢复也必须遵守同一顺序，避免 state -> canonical 与 canonical -> state 互相等待。
        let _canonical_guard = self
            .canonical_commit_lock
            .lock()
            .expect("canonical commit lock poisoned");
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let mut restored = 0;
        for record in records {
            let SessionAcceptanceRecord {
                session,
                timeline_entry,
                superseded_turn,
                canonical_turn,
                sidecar,
            } = record;
            let session_id = session.session_id.clone();
            let turn_id = canonical_turn.turn_id.clone();
            let mut changed = false;

            let mut mutations = Vec::new();
            if let Some(superseded_turn) = superseded_turn.as_ref() {
                let previous = state
                    .canonical_turns
                    .iter()
                    .find(|existing| {
                        existing.session_id == session_id
                            && existing.turn_id == superseded_turn.turn_id
                    })
                    .cloned()
                    .ok_or_else(|| DomainError::InvalidState {
                        message: format!(
                            "replacement accepted journal 缺少被替换 turn {}",
                            superseded_turn.turn_id
                        ),
                    })?;
                if previous != *superseded_turn {
                    superseded_turn.validate_update_from(&previous)?;
                    mutations.push(CanonicalTurnMutation {
                        previous: Some(previous),
                        next: superseded_turn.clone(),
                    });
                }
            }
            let existing_accepted = state
                .canonical_turns
                .iter()
                .find(|existing| existing.session_id == session_id && existing.turn_id == turn_id);
            if let Some(existing) = existing_accepted {
                existing.validate_update_from(&canonical_turn)?;
            } else {
                mutations.push(CanonicalTurnMutation {
                    previous: None,
                    next: canonical_turn.clone(),
                });
            }
            if !mutations.is_empty() {
                self.persist_canonical_mutations(&session_id, &mutations)?;
            }

            if !state
                .sessions
                .iter()
                .any(|existing| existing.session_id == session_id)
            {
                state.sessions.push(session);
                changed = true;
            }
            if !state
                .timeline
                .iter()
                .any(|existing| existing.entry_id == timeline_entry.entry_id)
            {
                state.timeline.push(timeline_entry);
                changed = true;
            }

            if let Some(superseded_turn) = superseded_turn {
                let index = state
                    .canonical_turns
                    .iter()
                    .position(|existing| {
                        existing.session_id == session_id
                            && existing.turn_id == superseded_turn.turn_id
                    })
                    .expect("replacement predecessor was validated");
                if state.canonical_turns[index] != superseded_turn {
                    state.canonical_turns[index] = superseded_turn;
                    changed = true;
                }
            }
            let canonical_index = state.canonical_turns.iter().position(|existing| {
                existing.session_id == session_id && existing.turn_id == turn_id
            });
            if canonical_index.is_none() {
                state.canonical_turns.push(canonical_turn.clone());
                changed = true;
            }

            let sidecar_index = state
                .execution_sidecar_store
                .runtime_sidecars
                .iter()
                .position(|existing| existing.session_id == session_id);
            match sidecar_index {
                Some(index) => {
                    let should_replace = state.execution_sidecar_store.runtime_sidecars[index]
                        .current_turn
                        .as_ref()
                        .is_none_or(|current| {
                            current.turn_id != turn_id
                                && Self::acceptance_turn_is_newer(&canonical_turn, current)
                        });
                    if should_replace {
                        state.execution_sidecar_store.runtime_sidecars[index] = sidecar;
                        changed = true;
                    }
                }
                None => {
                    state.execution_sidecar_store.runtime_sidecars.push(sidecar);
                    changed = true;
                }
            }
            if state.current_session_id.is_none() {
                state.current_session_id = Some(session_id);
                changed = true;
            }
            if changed {
                restored += 1;
            }
        }
        if restored > 0 {
            state
                .execution_sidecar_store
                .runtime_sidecars
                .sort_by(|left, right| left.session_id.as_str().cmp(right.session_id.as_str()));
            state.canonical_turns.sort_by(|left, right| {
                left.turn_seq
                    .cmp(&right.turn_seq)
                    .then_with(|| left.turn_id.cmp(&right.turn_id))
            });
            self.mark_sidecar_dirty(SessionSidecarFlushReason::UpsertActiveExecutionChain);
        }
        Ok(restored)
    }

    /// 比较 accepted journal 中的 Turn 与当前 sidecar Turn，保证旧 journal
    /// 不能覆盖更新的活动轮次。`turn_id` 是同一时间戳下的最终稳定排序键。
    fn acceptance_turn_is_newer(
        candidate: &crate::models::CanonicalTurn,
        current: &crate::models::ActiveExecutionTurn,
    ) -> bool {
        (
            candidate.accepted_at.0,
            candidate.turn_seq,
            candidate.turn_id.as_str(),
        ) > (
            current.accepted_at.0,
            current.turn_seq,
            current.turn_id.as_str(),
        )
    }

    /// 串行化完整 session projection 持久化事务。durable、sidecar 与 current pointer
    /// 必须在同一把 state 读锁覆盖的持久化事务中生成并写出，canonical mutation 不能
    /// 在 projection 回调期间穿透进来。
    pub fn persist_projection_with<T, E>(
        &self,
        persist: impl FnMut(&SessionDurableState, &SessionExecutionSidecarStoreState) -> Result<T, E>,
    ) -> Result<T, E> {
        let _persistence_guard = self
            .durable_persistence_lock
            .lock()
            .expect("session durable persistence lock poisoned");
        // canonical event writer 会先更新事件缓存，再提交内存 projection。
        // 如果只持有 state 读锁，写入事件与内存提交之间的窗口会让持久化回调
        // 同时看到旧 canonical 和新 event，产生无法解释的投影冲突。把完整
        // snapshot 事务置于 canonical commit lock 之下，保证两者使用同一代事实。
        let _canonical_guard = self
            .canonical_commit_lock
            .lock()
            .expect("session canonical commit lock poisoned");
        let mut persist = persist;
        let state = self.state.read().expect("session state read lock poisoned");
        let durable = state.durable_state();
        let sidecars = state.execution_sidecar_store.clone();
        persist(&durable, &sidecars)
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

    /// 进入未发送的新会话草稿。该操作只清除导航选择，不创建空会话，
    /// 不写 timeline，也不改变任何既有会话的业务时间。
    pub fn clear_current_session(&self) {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        state.current_session_id = None;
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
        self.rename_session_with_persistence(session_id, new_title, |_, _| {
            Ok::<(), std::convert::Infallible>(())
        })
        .map_err(|error| match error {
            SessionMutationTransactionError::Domain(error) => error,
            SessionMutationTransactionError::Persistence(error) => match error {},
        })
    }

    /// 在 durable projection 成功后提交 session 标题和重命名 timeline 事件。
    ///
    /// 候选状态在 state 写锁覆盖期间生成、持久化并提交，避免重命名后刷新时出现旧标题
    /// 覆盖新标题，或事件已经发布但 durable projection 仍是旧版本。
    pub fn rename_session_with_persistence<E>(
        &self,
        session_id: &SessionId,
        title: impl Into<String>,
        persist: impl FnOnce(&SessionDurableState, &SessionExecutionSidecarStoreState) -> Result<(), E>,
    ) -> Result<SessionRecord, SessionMutationTransactionError<E>> {
        let new_title = normalize_session_title(title.into())
            .map_err(SessionMutationTransactionError::Domain)?;
        let _persistence_guard = self
            .durable_persistence_lock
            .lock()
            .expect("session durable persistence lock poisoned");
        let _canonical_guard = self
            .canonical_commit_lock
            .lock()
            .expect("session canonical commit lock poisoned");
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let mut candidate = state.clone();
        let Some(updated) = prepare_session_rename(&mut candidate, session_id, &new_title)
            .map_err(SessionMutationTransactionError::Domain)?
        else {
            return candidate
                .sessions
                .into_iter()
                .find(|session| &session.session_id == session_id)
                .ok_or(SessionMutationTransactionError::Domain(
                    DomainError::NotFound { entity: "session" },
                ));
        };
        let durable = candidate.durable_state();
        let sidecars = candidate.execution_sidecar_store.clone();
        persist(&durable, &sidecars).map_err(SessionMutationTransactionError::Persistence)?;
        *state = candidate;
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

    /// 删除 session 的唯一状态提交入口。
    ///
    /// 调用方必须在生命周期锁内先完成运行资源的收口，再把预先确定的替代 current
    /// 传入。这里持有 durable、canonical 和 state 写锁直到 projection 回调结束，
    /// 所以回调失败时不会出现“磁盘已删、内存仍在”或“内存已删、磁盘仍在”的分歧。
    pub fn delete_session_with_persistence<E>(
        &self,
        session_id: &SessionId,
        replacement_session_id: Option<&SessionId>,
        persist: impl FnOnce(&SessionDurableState, &SessionExecutionSidecarStoreState) -> Result<(), E>,
    ) -> Result<(), SessionMutationTransactionError<E>> {
        let _persistence_guard = self
            .durable_persistence_lock
            .lock()
            .expect("session durable persistence lock poisoned");
        let _canonical_guard = self
            .canonical_commit_lock
            .lock()
            .expect("session canonical commit lock poisoned");
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let mut candidate = state.clone();
        let removed_sidecar =
            prepare_session_deletion(&mut candidate, session_id, replacement_session_id)
                .map_err(SessionMutationTransactionError::Domain)?;
        let durable = candidate.durable_state();
        let sidecars = candidate.execution_sidecar_store.clone();
        persist(&durable, &sidecars).map_err(SessionMutationTransactionError::Persistence)?;

        *state = candidate;
        drop(state);
        if removed_sidecar {
            self.mark_sidecar_dirty(SessionSidecarFlushReason::DeleteSession);
        }
        if let Some(observer) = self.lifecycle_observer() {
            observer.on_session_deleted(session_id);
        }
        Ok(())
    }

    /// 仅供不接入 durable repository 的领域级调用方使用。生产 API 删除路径必须调用
    /// `delete_session_with_persistence`，并传入真正的 projection 持久化回调。
    pub fn delete_session(&self, session_id: &SessionId) -> DomainResult<()> {
        let replacement = {
            let state = self.state.read().expect("session state read lock poisoned");
            if state.current_session_id.as_ref() != Some(session_id) {
                None
            } else {
                state
                    .sessions
                    .iter()
                    .filter(|session| &session.session_id != session_id)
                    .max_by(|left, right| cmp_sessions_newest_first(left, right))
                    .map(|session| session.session_id.clone())
            }
        };
        self.delete_session_with_persistence(session_id, replacement.as_ref(), |_, _| {
            Ok::<(), std::convert::Infallible>(())
        })
        .map_err(|error| match error {
            SessionMutationTransactionError::Domain(error) => error,
            SessionMutationTransactionError::Persistence(error) => match error {},
        })
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

    /// 在确认当前 revision 仍属于本次装配后恢复计划快照。
    ///
    /// `original_plan = None` 表示本次装配新建的计划，应直接删除；传入原快照
    /// 时恢复其完整内容。revision 校验避免回滚覆盖 materialize 之后发生的真实
    /// 用户或执行更新。
    pub fn restore_plan_if_current(
        &self,
        session_id: &SessionId,
        expected_current_revision: u64,
        original_plan: Option<SessionPlan>,
    ) -> DomainResult<()> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let Some(index) = state
            .plans
            .iter()
            .position(|plan| &plan.session_id == session_id)
        else {
            return Err(DomainError::NotFound { entity: "plan" });
        };
        if state.plans[index].revision != expected_current_revision {
            return Err(DomainError::InvalidState {
                message: format!(
                    "计划已被其他执行修改，拒绝回滚：期望 revision={}，当前 revision={}",
                    expected_current_revision, state.plans[index].revision
                ),
            });
        }
        match original_plan {
            Some(mut original) => {
                if original.session_id != *session_id {
                    return Err(DomainError::Validation {
                        message: "计划恢复快照 session_id 与写入作用域不一致".to_string(),
                    });
                }
                // revision 是并发控制代际，回滚业务字段不能让它倒退，否则旧的
                // 客户端可以用已消费过的 revision 再次写入，破坏 optimistic lock。
                original.revision = expected_current_revision;
                original.updated_at = UtcMillis::now();
                state.plans[index] = original;
            }
            None => {
                state.plans.remove(index);
            }
        }
        drop(state);
        self.mark_sidecar_dirty(SessionSidecarFlushReason::UpdatePlan);
        Ok(())
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
            NotificationScope::Session if notification.session_id.is_none() => {
                return Err(DomainError::Validation {
                    message: "session incident requires session_id".to_string(),
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

    pub fn clear_notifications_for_context(&self, context: &NotificationContext) -> usize {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let before = state.notifications.len();
        state
            .notifications
            .retain(|notification| !notification.visible_in_context(context));
        before.saturating_sub(state.notifications.len())
    }

    pub fn mark_notifications_handled_for_context(&self, context: &NotificationContext) {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        for notification in state
            .notifications
            .iter_mut()
            .filter(|notification| notification.visible_in_context(context))
        {
            notification.handled = true;
            notification.count_unread = false;
        }
    }

    pub fn remove_notification_for_context(
        &self,
        context: &NotificationContext,
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
                notification.visible_in_context(context)
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
        context: &NotificationContext,
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
                    && notification.visible_in_context(context)
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
