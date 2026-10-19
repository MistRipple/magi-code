//! 人工交互写回与执行领取复用 Session canonical 事务；不另建授权日志。
use super::{CanonicalItemCommitNotice, CanonicalTurnMutation, SessionStore};
use crate::SessionStoreState;
use magi_core::{DomainResult, SessionId};

impl SessionStore {
    pub(super) fn commit_tool_interaction<T>(
        &self,
        session: &SessionId,
        prepare: impl FnOnce(&SessionStoreState) -> DomainResult<(Vec<CanonicalTurnMutation>, T)>,
    ) -> DomainResult<T> {
        let (value, notices) = self.commit_canonical_transaction(
            session,
            |state| {
                let (mut mutations, value) = prepare(state)?;
                mutations.retain(|mutation| mutation.previous.as_ref() != Some(&mutation.next));
                let workspace_id = state
                    .sessions
                    .iter()
                    .find(|record| &record.session_id == session)
                    .and_then(|record| record.workspace_id.clone());
                let mut notices = Vec::new();
                for mutation in &mutations {
                    if let Some(previous) = &mutation.previous {
                        mutation.next.validate_update_from(previous)?;
                    }
                    for item in &mutation.next.items {
                        let previous = mutation.previous.as_ref().and_then(|turn| {
                            turn.items.iter().find(|old| old.item_id == item.item_id)
                        });
                        if let Some(previous) = previous {
                            item.validate_update_from(previous)?;
                        }
                        if previous != Some(item) {
                            notices.push(CanonicalItemCommitNotice {
                                turn: mutation.next.clone(),
                                item: item.clone(),
                                workspace_id: workspace_id.clone(),
                            });
                        }
                    }
                }
                let mut projection = state.execution_sidecar_store.runtime_sidecar(session);
                if let Some(sidecar) = projection.as_mut() {
                    let current_id = sidecar
                        .current_turn
                        .as_ref()
                        .map(|turn| turn.turn_id.as_str());
                    if let Some(mutation) = mutations
                        .iter()
                        .find(|mutation| Some(mutation.next.turn_id.as_str()) == current_id)
                    {
                        super::sidecar::rebuild_sidecar_projection_from_canonical(
                            sidecar,
                            std::slice::from_ref(&mutation.next),
                        )?;
                    }
                }
                Ok(super::sidecar::CanonicalCommitPlan {
                    mutations,
                    acceptance: None,
                    value: (value, notices, projection),
                })
            },
            |state, (value, notices, projection)| {
                if let Some(projection) = projection
                    && let Some(sidecar) = state
                        .execution_sidecar_store
                        .runtime_sidecars
                        .iter_mut()
                        .find(|sidecar| &sidecar.session_id == session)
                {
                    sidecar.current_turn = projection.current_turn;
                    if let Some(chain) = sidecar.active_execution_chain.as_mut() {
                        chain.current_turn = sidecar.current_turn.clone();
                        chain.normalize();
                    }
                }
                (value, notices)
            },
        )?;
        let observer = self
            .canonical_item_commit_observer
            .read()
            .expect("canonical observer lock")
            .clone();
        if let Some(observer) = observer {
            for notice in notices {
                observer(notice);
            }
        }
        Ok(value)
    }
}
