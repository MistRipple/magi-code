use super::*;

impl SessionStore {
    /// 注册项目时从 daemon 的同一投影/事件恢复入口读取历史。与正常保存共用锁序，
    /// 先校验完整候选状态再发布，不能覆盖已有会话、输入身份或当前导航。
    pub fn restore_workspace_history(
        &self,
        workspace_id: &str,
        restore: impl FnOnce() -> DomainResult<(
            SessionDurableState,
            SessionExecutionSidecarStoreState,
            Vec<UnavailableSession>,
        )>,
    ) -> DomainResult<()> {
        let _persistence_guard = self
            .durable_persistence_lock
            .lock()
            .expect("session durable persistence lock poisoned");
        let _canonical_guard = self
            .canonical_commit_barrier
            .write()
            .expect("session canonical commit barrier poisoned");
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        // 项目移除后保留在原 Store 中的历史始终优先，不能被旧备份重置。
        if state
            .sessions
            .iter()
            .any(|session| session.workspace_id.as_deref() == Some(workspace_id))
        {
            return Ok(());
        }
        let (incoming, sidecars, unavailable) = restore()?;
        if incoming.current_session_id.is_some()
            || incoming.sessions.iter().any(|session| {
                session.workspace_id.as_deref() != Some(workspace_id)
                    || state
                        .sessions
                        .iter()
                        .any(|existing| existing.session_id == session.session_id)
            })
        {
            return Err(DomainError::InvalidState {
                message: "项目历史归属冲突，拒绝覆盖已有会话或当前导航".into(),
            });
        }
        let imported = Self::from_persisted_parts(incoming, sidecars)?.export_state();
        if imported.inputs.iter().any(|input| {
            state
                .inputs
                .iter()
                .any(|existing| existing.request_id == input.request_id)
        }) || imported.canonical_turns.iter().any(|turn| {
            state
                .canonical_turns
                .iter()
                .any(|existing| existing.turn_id == turn.turn_id)
        }) || imported.thread_registry.iter().any(|thread| {
            state
                .thread_registry
                .iter()
                .any(|existing| existing.thread_id == thread.thread_id)
        }) || imported.goals.iter().any(|goal| {
            state
                .goals
                .iter()
                .any(|existing| existing.goal_id == goal.goal_id)
        }) || imported.plans.iter().any(|plan| {
            state
                .plans
                .iter()
                .any(|existing| existing.plan_id == plan.plan_id)
        }) {
            return Err(DomainError::InvalidState {
                message: "项目历史与已有执行身份冲突，拒绝自动合并".into(),
            });
        }
        // 只发布刚校验的项目切片，不能对其它正在执行的会话运行重启归一化。
        state.sessions.extend(imported.sessions);
        state.timeline.extend(imported.timeline);
        state.canonical_turns.extend(imported.canonical_turns);
        state.inputs.extend(imported.inputs);
        state.notifications.extend(imported.notifications);
        state.goals.extend(imported.goals);
        state.plans.extend(imported.plans);
        state.thread_registry.extend(imported.thread_registry);
        state
            .thread_context_checkpoints
            .extend(imported.thread_context_checkpoints);
        state
            .execution_sidecar_store
            .runtime_sidecars
            .extend(imported.execution_sidecar_store.runtime_sidecars);
        let mut known = self
            .unavailable_sessions
            .write()
            .expect("unavailable sessions lock poisoned");
        known.retain(|session| session.workspace_id.as_deref() != Some(workspace_id));
        known.extend(
            unavailable
                .into_iter()
                .filter(|session| session.workspace_id.as_deref() == Some(workspace_id)),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_import_preserves_existing_navigation_and_does_not_reimport_live_project() {
        let store = SessionStore::new();
        let current = SessionId::new("current-personal");
        store
            .create_session(current.clone(), "keep current")
            .unwrap();
        let before = store.export_state();
        let old = SessionStore::new();
        let old_session = SessionId::new("portable-history");
        old.create_session_for_workspace(old_session.clone(), "history", Some("portable".into()))
            .unwrap();
        old.clear_current_session();
        store
            .restore_workspace_history("portable", || {
                Ok((
                    old.durable_state(),
                    old.execution_sidecar_store_state(),
                    vec![],
                ))
            })
            .unwrap();
        assert_eq!(store.current_session_id(), Some(current.clone()));
        assert_eq!(
            serde_json::to_value(
                store
                    .export_state()
                    .sessions
                    .into_iter()
                    .find(|session| session.session_id == current)
            )
            .unwrap(),
            serde_json::to_value(
                before
                    .sessions
                    .into_iter()
                    .find(|session| session.session_id == current)
            )
            .unwrap()
        );
        assert_eq!(
            store.session(&old_session).unwrap().workspace_id.as_deref(),
            Some("portable")
        );
        store
            .restore_workspace_history("portable", || panic!("已有内存历史不能重新读取备份"))
            .unwrap();
        assert_eq!(store.export_state().sessions.len(), 2);
    }

    #[test]
    fn conflicting_history_never_overwrites_existing_session_or_navigation() {
        let store = SessionStore::new();
        let current = SessionId::new("same-session");
        store.create_session(current.clone(), "personal").unwrap();
        let before = serde_json::to_value(store.export_state()).unwrap();
        let old = SessionStore::new();
        old.create_session_for_workspace(current, "foreign", Some("portable".into()))
            .unwrap();
        old.clear_current_session();
        assert!(
            store
                .restore_workspace_history("portable", || Ok((
                    old.durable_state(),
                    old.execution_sidecar_store_state(),
                    vec![]
                )))
                .is_err()
        );
        assert_eq!(serde_json::to_value(store.export_state()).unwrap(), before);
        assert!(
            store
                .restore_workspace_history("portable", || Err(DomainError::Persistence {
                    message: "write failed".into()
                }))
                .is_err()
        );
        assert_eq!(serde_json::to_value(store.export_state()).unwrap(), before);
    }
}
