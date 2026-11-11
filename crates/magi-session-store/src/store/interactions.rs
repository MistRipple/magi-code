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
                Ok(super::sidecar::CanonicalCommitPlan {
                    mutations,
                    acceptance: None,
                    value: (value, notices),
                })
            },
            |_state, value| value,
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
