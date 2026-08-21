use super::{SessionStore, cmp_sessions_newest_first, with_session_message_count};
use crate::models::{
    ActiveExecutionChain, NotificationContext, NotificationRecord, SessionDurableState,
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

    /// 为 bootstrap 构造工作区内单个会话的投影。
    ///
    /// 首屏只需要当前 workspace 的会话列表和当前会话历史。这里直接在读锁
    /// 下按范围筛选，避免先克隆整个 daemon 的所有会话、timeline 和 canonical
    /// turns，再由 API 层裁剪造成不必要的 CPU 与内存峰值。
    pub fn projection_input_for_workspace_session(
        &self,
        workspace_id: &str,
        requested_session_id: Option<&SessionId>,
    ) -> SessionProjectionInput {
        self.projection_input_for_optional_workspace_session(
            Some(workspace_id.trim()),
            requested_session_id,
        )
    }

    /// 为未绑定项目的个人会话构造投影。
    ///
    /// `workspace_id = None` 是一个明确的会话作用域，而不是“没有筛选条件”。
    /// 因此这里不能复用全局 projection：全局 projection 会把项目会话混入“最近”。
    pub fn projection_input_for_personal_session(
        &self,
        requested_session_id: Option<&SessionId>,
    ) -> SessionProjectionInput {
        self.projection_input_for_optional_workspace_session(None, requested_session_id)
    }

    fn projection_input_for_optional_workspace_session(
        &self,
        workspace_id: Option<&str>,
        requested_session_id: Option<&SessionId>,
    ) -> SessionProjectionInput {
        let workspace_id = workspace_id.map(str::trim);
        let state = self.state.read().expect("session state read lock poisoned");
        let mut sessions = state
            .sessions
            .iter()
            .filter(|session| session.workspace_id.as_deref() == workspace_id)
            .cloned()
            .map(|session| with_session_message_count(session, &state.timeline))
            .filter(|session| {
                session.message_count.unwrap_or(0) > 0
                    || requested_session_id == Some(&session.session_id)
                    || state.current_session_id.as_ref() == Some(&session.session_id)
            })
            .collect::<Vec<_>>();
        sessions.sort_by(cmp_sessions_newest_first);

        let selected_session_id = requested_session_id
            .filter(|session_id| {
                sessions
                    .iter()
                    .any(|session| &session.session_id == *session_id)
            })
            .cloned()
            .or_else(|| {
                state
                    .current_session_id
                    .as_ref()
                    .filter(|session_id| {
                        sessions
                            .iter()
                            .any(|session| &session.session_id == *session_id)
                    })
                    .cloned()
            });

        // 多取一个 timeline entry 作为分页哨兵，避免 bootstrap 为了裁剪首屏
        // 先复制当前会话的完整消息历史。
        let mut timeline_refs = selected_session_id
            .as_ref()
            .map(|session_id| {
                state
                    .timeline
                    .iter()
                    .filter(|entry| &entry.session_id == session_id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        timeline_refs.sort_by(|left, right| {
            left.occurred_at
                .0
                .cmp(&right.occurred_at.0)
                .then_with(|| left.entry_id.cmp(&right.entry_id))
        });
        let timeline_start = timeline_refs.len().saturating_sub(51);
        let timeline = timeline_refs
            .drain(timeline_start..)
            .cloned()
            .collect::<Vec<_>>();

        // 多取一个 turn 作为分页哨兵。DTO 会把它裁掉，但可以据此准确判断
        // 是否还有更早历史，同时不会为了首屏复制整个 canonical history。
        let mut canonical_turn_refs = selected_session_id
            .as_ref()
            .map(|session_id| {
                let mut turns = state
                    .canonical_turns
                    .iter()
                    .filter(|turn| &turn.session_id == session_id)
                    .collect::<Vec<_>>();
                turns.sort_by(|left, right| {
                    left.turn_seq
                        .cmp(&right.turn_seq)
                        .then_with(|| left.turn_id.cmp(&right.turn_id))
                });
                turns
            })
            .unwrap_or_default();
        let canonical_start = canonical_turn_refs.len().saturating_sub(21);
        let canonical_turns = canonical_turn_refs
            .drain(canonical_start..)
            .map(|turn| {
                let mut turn = turn.clone();
                turn.normalize();
                turn
            })
            .collect();

        SessionProjectionInput {
            current_session_id: selected_session_id,
            sessions,
            timeline,
            canonical_turns,
            notifications: Vec::new(),
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
    /// 因此删除会话时不依赖 thread registry 的完整性也能定位任务树。
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

    /// 按协议 requestId 找回已经持久化的 canonical Turn。
    ///
    /// App Server 客户端可能在服务端已经接受请求、但响应尚未送达时重试同一个
    /// requestId。这个查询是重试幂等的唯一持久事实来源，不能依赖连接内存状态。
    pub fn canonical_turn_for_request_id(
        &self,
        request_id: &str,
    ) -> Option<crate::models::CanonicalTurn> {
        let request_id = request_id.trim();
        if request_id.is_empty() {
            return None;
        }
        let state = self.state.read().expect("session state read lock poisoned");
        let mut turn = state
            .canonical_turns
            .iter()
            .find(|turn| {
                turn.items.iter().any(|item| {
                    item.metadata
                        .get("requestId")
                        .or_else(|| item.metadata.get("request_id"))
                        .and_then(serde_json::Value::as_str)
                        == Some(request_id)
                })
            })?
            .clone();
        turn.normalize();
        Some(turn)
    }

    /// 返回当前会话 canonical turn 的倒序分页窗口。
    ///
    /// canonical turn 是主对话的权威事实，不能在每次 bootstrap 时完整复制。
    /// 游标使用稳定的 turn_id，和 timeline 的 entry_id 分开，避免一条 turn
    /// 包含大量 item 时把完整历史重新编码进首屏响应。
    pub fn canonical_turn_page_for_session(
        &self,
        session_id: &SessionId,
        before_cursor: Option<&str>,
        limit: usize,
    ) -> Option<(Vec<crate::models::CanonicalTurn>, bool, Option<String>)> {
        let state = self.state.read().expect("session state read lock poisoned");
        let mut turns = state
            .canonical_turns
            .iter()
            .filter(|turn| &turn.session_id == session_id)
            .collect::<Vec<_>>();
        turns.sort_by(|left, right| {
            left.turn_seq
                .cmp(&right.turn_seq)
                .then_with(|| left.turn_id.cmp(&right.turn_id))
        });
        let end = match before_cursor
            .map(str::trim)
            .filter(|cursor| !cursor.is_empty())
        {
            Some(cursor) => turns.iter().position(|turn| turn.turn_id == cursor)?,
            None => turns.len(),
        };
        let start = end.saturating_sub(limit.max(1));
        let page = turns[start..end]
            .iter()
            .map(|turn| {
                let mut turn = (*turn).clone();
                turn.normalize();
                turn
            })
            .collect::<Vec<_>>();
        let cursor = page.first().map(|turn| turn.turn_id.clone());
        Some((page, start > 0, cursor))
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
        context: &NotificationContext,
    ) -> Vec<NotificationRecord> {
        let mut notifications = self
            .state
            .read()
            .expect("session state read lock poisoned")
            .notifications
            .iter()
            .filter(|notification| notification.visible_in_context(context))
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
