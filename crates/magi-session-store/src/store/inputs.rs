use super::{CanonicalTurnMutation, SessionInputMutation, SessionStore};
use crate::{CanonicalTurnItemKind, SessionInput, SessionInputState};
use magi_core::{DomainError, DomainResult, SessionId};

/// 从同一事务的用户 item 建立输入绑定；不会根据文本相似度猜测消费。
pub(super) fn attached_inputs(
    inputs: &[SessionInput],
    mutations: &[CanonicalTurnMutation],
) -> DomainResult<Vec<SessionInputMutation>> {
    for input in inputs
        .iter()
        .filter(|input| input.state == SessionInputState::Pending && input.interruption.is_some())
    {
        for mutation in mutations.iter().filter(|mutation| {
            mutation.previous.is_none() && mutation.next.session_id == input.session_id
        }) {
            let consumes_replacement = mutation.next.items.iter().any(|item| {
                item.kind == CanonicalTurnItemKind::UserMessage
                    && item
                        .metadata
                        .get("requestId")
                        .and_then(|value| value.as_str())
                        == Some(input.request_id.as_str())
            });
            if !consumes_replacement
                || input
                    .interruption
                    .as_ref()
                    .is_some_and(|value| value.settled_at.is_none())
            {
                return Err(DomainError::InvalidState {
                    message: "停止并改做输入尚未结算或应先接纳，不能启动其他 Turn".into(),
                });
            }
        }
    }
    let mut updates: Vec<SessionInputMutation> = inputs
        .iter()
        .filter(|input| input.state == SessionInputState::Pending)
        .filter_map(|input| {
            let matches = mutations
                .iter()
                .filter(|mutation| {
                    mutation.next.session_id == input.session_id
                        && mutation.next.items.iter().any(|item| {
                            item.kind == CanonicalTurnItemKind::UserMessage
                                && item.metadata.get("requestId").and_then(|v| v.as_str())
                                    == Some(input.request_id.as_str())
                                && item.metadata.get("userMessageId").and_then(|v| v.as_str())
                                    == Some(input.user_message_id.as_str())
                        })
                })
                .collect::<Vec<_>>();
            if matches.is_empty() {
                return None;
            }
            Some((|| {
                if matches.len() != 1 {
                    return Err(DomainError::InvalidState {
                        message: "同一输入不能绑定多个 Turn".into(),
                    });
                }
                let mut next = input.clone();
                next.revision =
                    next.revision
                        .checked_add(1)
                        .ok_or_else(|| DomainError::InvalidState {
                            message: "输入 revision 溢出".into(),
                        })?;
                next.state = SessionInputState::Attached;
                next.attached_turn_id = Some(matches[0].next.turn_id.clone());
                next.validate_update_from(Some(input))?;
                Ok(SessionInputMutation {
                    previous: Some(input.clone()),
                    next,
                })
            })())
        })
        .collect::<DomainResult<Vec<_>>>()?;
    for mutation in mutations {
        for item in &mutation.next.items {
            if item.kind != CanonicalTurnItemKind::UserMessage {
                continue;
            }
            let Some(request_id) = item.metadata.get("requestId").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(user_message_id) = item.metadata.get("userMessageId").and_then(|v| v.as_str())
            else {
                continue;
            };
            let Some(fingerprint) = item
                .metadata
                .get("requestFingerprint")
                .and_then(|v| v.as_str())
            else {
                continue;
            };
            if let Some(existing) = inputs.iter().find(|input| input.request_id == request_id) {
                if existing.session_id != item.session_id
                    || existing.user_message_id != user_message_id
                    || existing.request_fingerprint != fingerprint
                    || existing.state == SessionInputState::Cancelled
                    || (matches!(
                        existing.state,
                        SessionInputState::Attached | SessionInputState::Consumed
                    ) && existing.attached_turn_id.as_deref() != Some(item.turn_id.as_str()))
                {
                    return Err(DomainError::InvalidState {
                        message: "用户 item 与原始输入身份冲突".into(),
                    });
                }
                continue;
            }
            if updates
                .iter()
                .any(|input| input.next.request_id == request_id)
            {
                continue;
            }
            let next = SessionInput {
                input_id: format!("input-{request_id}"),
                session_id: item.session_id.clone(),
                request_id: request_id.into(),
                user_message_id: user_message_id.into(),
                request_fingerprint: fingerprint.into(),
                accepted_at: item.created_at,
                revision: 1,
                state: SessionInputState::Attached,
                attached_turn_id: Some(item.turn_id.clone()),
                consumption: None,
                interruption: None,
                payload: serde_json::to_value(item).map_err(|error| DomainError::Validation {
                    message: error.to_string(),
                })?,
            };
            next.validate_update_from(None)?;
            updates.push(SessionInputMutation {
                previous: None,
                next,
            });
        }
    }
    Ok(updates)
}

pub(super) fn apply_input(inputs: &mut Vec<SessionInput>, input: SessionInput) {
    if let Some(existing) = inputs.iter_mut().find(|existing| {
        existing.session_id == input.session_id && existing.input_id == input.input_id
    }) {
        *existing = input;
    } else {
        inputs.push(input);
    }
}

impl SessionStore {
    pub fn input_request_ids_for_turn(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> std::collections::BTreeSet<String> {
        self.state
            .read()
            .expect("session state lock poisoned")
            .inputs
            .iter()
            .filter(|input| {
                &input.session_id == session_id
                    && input.attached_turn_id.as_deref() == Some(turn_id)
            })
            .map(|input| input.request_id.clone())
            .collect()
    }

    pub fn unconsumed_input_request_ids(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> std::collections::BTreeSet<String> {
        self.state
            .read()
            .expect("session state lock poisoned")
            .inputs
            .iter()
            .filter(|input| {
                &input.session_id == session_id
                    && input.attached_turn_id.as_deref() == Some(turn_id)
                    && input.state == SessionInputState::Attached
            })
            .map(|input| input.request_id.clone())
            .collect()
    }

    pub fn install_input_commit_observer(
        &self,
        observer: std::sync::Arc<dyn Fn(super::SessionInputCommitNotice) + Send + Sync>,
    ) {
        *self
            .input_commit_observer
            .write()
            .expect("input observer lock poisoned") = Some(observer);
    }

    pub(super) fn notify_input_commit(&self, session_id: &SessionId) {
        let observer = self
            .input_commit_observer
            .read()
            .expect("input observer lock poisoned")
            .clone();
        let Some(observer) = observer else {
            return;
        };
        let notice = {
            let state = self.state.read().expect("session state lock poisoned");
            let Some(session) = state
                .sessions
                .iter()
                .find(|session| &session.session_id == session_id)
            else {
                return;
            };
            super::SessionInputCommitNotice {
                session_id: session_id.clone(),
                workspace_id: session.workspace_id.clone(),
                epoch: format!("{}:{}", session_id, session.created_at.0),
                through_sequence: state
                    .inputs
                    .iter()
                    .filter(|input| &input.session_id == session_id)
                    .map(|input| input.revision)
                    .sum(),
            }
        };
        observer(notice);
    }

    /// Inbox 热路径只复制 pending 请求；已消费输入的正文不随队列刷新重复深拷贝。
    pub fn inbox_snapshot(&self, session_id: &SessionId) -> (Vec<SessionInput>, u64) {
        let state = self.state.read().expect("session state lock poisoned");
        let mut pending = Vec::new();
        let mut sequence = 0;
        for input in state
            .inputs
            .iter()
            .filter(|input| &input.session_id == session_id)
        {
            sequence += input.revision;
            if input.state == SessionInputState::Pending {
                pending.push(input.clone());
            }
        }
        (pending, sequence)
    }
    pub fn input_for_request(&self, request_id: &str) -> Option<SessionInput> {
        self.state
            .read()
            .expect("session state lock poisoned")
            .inputs
            .iter()
            .find(|input| input.request_id == request_id)
            .cloned()
    }
    pub fn input_for_id(&self, session_id: &SessionId, input_id: &str) -> Option<SessionInput> {
        self.state
            .read()
            .expect("session state lock poisoned")
            .inputs
            .iter()
            .find(|input| &input.session_id == session_id && input.input_id == input_id)
            .cloned()
    }
    pub fn pending_input_count(&self, session_id: &SessionId) -> usize {
        self.state
            .read()
            .expect("session state lock poisoned")
            .inputs
            .iter()
            .filter(|input| {
                &input.session_id == session_id && input.state == SessionInputState::Pending
            })
            .count()
    }
    pub fn pending_input_session_ids(&self) -> Vec<SessionId> {
        let mut ids = self
            .state
            .read()
            .expect("session state lock poisoned")
            .inputs
            .iter()
            .filter(|input| input.state == SessionInputState::Pending)
            .map(|input| input.session_id.clone())
            .collect::<Vec<_>>();
        ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        ids.dedup();
        ids
    }

    pub fn inputs_for_session(&self, session_id: &SessionId) -> Vec<SessionInput> {
        self.state
            .read()
            .expect("session state lock poisoned")
            .inputs
            .iter()
            .filter(|input| &input.session_id == session_id)
            .cloned()
            .collect()
    }

    pub fn all_session_inputs(&self) -> Vec<SessionInput> {
        self.state
            .read()
            .expect("session state lock poisoned")
            .inputs
            .clone()
    }

    /// CAS 与 canonical 提交使用同一会话锁，写盘失败不会推进内存。
    pub fn commit_input(
        &self,
        input: SessionInput,
        expected_revision: Option<u64>,
    ) -> DomainResult<()> {
        let _barrier = self
            .canonical_commit_barrier
            .read()
            .expect("canonical barrier poisoned");
        let lock = self.canonical_commit_lock_for(&input.session_id);
        let _guard = lock.lock().expect("canonical session lock poisoned");
        let previous = {
            let state = self.state.read().expect("session state lock poisoned");
            if !state
                .sessions
                .iter()
                .any(|session| session.session_id == input.session_id)
            {
                return Err(DomainError::NotFound { entity: "session" });
            }
            if state.inputs.iter().any(|existing| {
                existing.input_id != input.input_id && existing.request_id == input.request_id
            }) {
                return Err(DomainError::InvalidState {
                    message: "requestId 已属于另一输入".into(),
                });
            }
            if let Some(interruption) = &input.interruption {
                let turn = state
                    .canonical_turns
                    .iter()
                    .find(|turn| {
                        turn.session_id == input.session_id && turn.turn_id == interruption.turn_id
                    })
                    .ok_or(DomainError::NotFound {
                        entity: "interrupted_turn",
                    })?;
                if expected_revision.is_none() {
                    let sidecar = state
                        .execution_sidecar_store
                        .runtime_sidecars
                        .iter()
                        .find(|sidecar| sidecar.session_id == input.session_id);
                    let source_root = sidecar
                        .and_then(|sidecar| sidecar.active_execution_chain.as_ref())
                        .filter(|chain| {
                            chain
                                .current_turn
                                .as_ref()
                                .is_some_and(|current| current.turn_id == interruption.turn_id)
                        })
                        .map(|chain| &chain.root_task_id);
                    if input.state != SessionInputState::Pending
                        || interruption.settled_at.is_some()
                        || turn.status.is_terminal()
                        || turn.completed_at.is_some()
                        || sidecar
                            .and_then(|sidecar| sidecar.current_turn.as_ref())
                            .is_none_or(|current| current.turn_id != interruption.turn_id)
                        || source_root != interruption.root_task_id.as_ref()
                        || state.inputs.iter().any(|candidate| {
                            candidate.session_id == input.session_id
                                && candidate.state == SessionInputState::Pending
                                && candidate.interruption.is_some()
                        })
                    {
                        return Err(DomainError::InvalidState {
                            message: "停止并改做目标已变化、来源不匹配或已有待结算替代输入".into(),
                        });
                    }
                } else if interruption.settled_at.is_some() && !turn.status.is_terminal() {
                    return Err(DomainError::InvalidState {
                        message: "原 Turn 尚未停止，不能确认替代输入的资源结算".into(),
                    });
                }
            }
            state
                .inputs
                .iter()
                .find(|existing| {
                    existing.session_id == input.session_id && existing.input_id == input.input_id
                })
                .cloned()
        };
        if previous.as_ref().map(|input| input.revision) != expected_revision {
            return Err(DomainError::InvalidState {
                message: "输入 revision 已变化".into(),
            });
        }
        input.validate_update_from(previous.as_ref())?;
        self.persist_session_transaction(
            &input.session_id,
            &[],
            &[SessionInputMutation {
                previous,
                next: input.clone(),
            }],
            None,
        )?;
        let session_id = input.session_id.clone();
        apply_input(
            &mut self
                .state
                .write()
                .expect("session state lock poisoned")
                .inputs,
            input,
        );
        self.notify_input_commit(&session_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SessionAcceptanceRecord, SessionEventWriter};
    use magi_core::{Task, UtcMillis};
    use std::sync::Arc;

    struct RejectingWriter;
    impl SessionEventWriter for RejectingWriter {
        fn append_session_transaction(
            &self,
            _: &SessionId,
            _: &[CanonicalTurnMutation],
            _: &[SessionInputMutation],
            _: Option<(&SessionAcceptanceRecord, Option<&Task>)>,
        ) -> DomainResult<()> {
            Err(DomainError::Persistence {
                message: "injected before commit".into(),
            })
        }
    }
    fn input(session_id: &SessionId) -> SessionInput {
        SessionInput {
            input_id: "input-1".into(),
            session_id: session_id.clone(),
            request_id: "request-1".into(),
            user_message_id: "user-1".into(),
            request_fingerprint: "fingerprint-1".into(),
            accepted_at: UtcMillis(10),
            revision: 1,
            state: SessionInputState::Pending,
            attached_turn_id: None,
            consumption: None,
            interruption: None,
            payload: serde_json::json!({"text":"work"}),
        }
    }
    #[test]
    fn failed_input_commit_does_not_publish_or_change_memory() {
        let store = SessionStore::new();
        let session = SessionId::new("session-input-failure");
        store.create_session(session.clone(), "failure").unwrap();
        store.install_session_event_writer(Arc::new(RejectingWriter));
        assert!(store.commit_input(input(&session), None).is_err());
        assert!(store.inputs_for_session(&session).is_empty());

        let store = SessionStore::new();
        store
            .create_session(session.clone(), "edit failure")
            .unwrap();
        let pending = input(&session);
        store.commit_input(pending.clone(), None).unwrap();
        store.install_session_event_writer(Arc::new(RejectingWriter));
        let mut edit = pending.clone();
        edit.revision += 1;
        edit.payload = serde_json::json!({"text":"edited work"});
        assert!(store.commit_input(edit, Some(pending.revision)).is_err());
        assert_eq!(
            store.input_for_id(&session, &pending.input_id),
            Some(pending)
        );
    }
    #[test]
    fn input_cas_prevents_revival_and_competing_claims() {
        let store = SessionStore::new();
        let session = SessionId::new("session-input-cas");
        store.create_session(session.clone(), "CAS").unwrap();
        let pending = input(&session);
        store.commit_input(pending.clone(), None).unwrap();
        let mut cancelled = pending.clone();
        cancelled.revision = 2;
        cancelled.state = SessionInputState::Cancelled;
        store.commit_input(cancelled.clone(), Some(1)).unwrap();
        assert!(store.commit_input(pending, None).is_err());
        let mut competing = cancelled.clone();
        competing.revision = 2;
        competing.state = SessionInputState::Attached;
        competing.attached_turn_id = Some("turn-1".into());
        assert!(store.commit_input(competing, Some(1)).is_err());
        assert_eq!(store.inputs_for_session(&session), vec![cancelled]);
    }
}
