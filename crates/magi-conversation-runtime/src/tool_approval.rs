use magi_core::{SessionId, TaskId, UtcMillis};
use magi_session_store::SessionStore;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, mpsc};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalDecision {
    AllowOnce,
    AllowForTurn,
    Deny,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingToolApproval {
    pub approval_id: String,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub turn_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub reason: String,
    pub requested_at: UtcMillis,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct TurnToolGrant {
    session_id: SessionId,
    turn_id: String,
    tool_name: String,
}

fn grant_for(request: &PendingToolApproval) -> TurnToolGrant {
    TurnToolGrant {
        session_id: request.session_id.clone(),
        turn_id: request.turn_id.clone(),
        tool_name: magi_tool_runtime::canonical_builtin_tool_name(&request.tool_name)
            .unwrap_or_else(|| request.tool_name.trim().to_ascii_lowercase()),
    }
}

#[derive(Debug)]
struct PendingApprovalEntry {
    request: PendingToolApproval,
    decision_tx: mpsc::Sender<ToolApprovalDecision>,
}

#[derive(Debug, Default)]
struct ToolApprovalState {
    pending: HashMap<String, PendingApprovalEntry>,
    turn_tool_grants: HashSet<TurnToolGrant>,
}

pub struct ToolApprovalWaiter {
    pub request: PendingToolApproval,
    pub decision_rx: mpsc::Receiver<ToolApprovalDecision>,
}

pub enum ToolApprovalRequestOutcome {
    AlreadyAllowed,
    Pending(ToolApprovalWaiter),
}

#[derive(Clone, Debug, Default)]
pub struct ToolApprovalRegistry {
    state: Arc<Mutex<ToolApprovalState>>,
}

pub(crate) fn session_turn_is_active(
    session_store: &SessionStore,
    session_id: &SessionId,
    turn_id: &str,
) -> bool {
    session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .is_some_and(|turn| {
            turn.turn_id == turn_id
                && matches!(
                    turn.status.trim().to_ascii_lowercase().as_str(),
                    "pending"
                        | "queued"
                        | "accepted"
                        | "running"
                        | "started"
                        | "streaming"
                        | "awaiting_approval"
                        | "review_required"
                        | "repairing"
                        | "verifying"
                )
        })
}

impl ToolApprovalRegistry {
    pub fn request(
        &self,
        request: PendingToolApproval,
    ) -> Result<ToolApprovalRequestOutcome, String> {
        let grant = grant_for(&request);
        let mut state = self
            .state
            .lock()
            .map_err(|_| "工具授权状态锁已损坏".to_string())?;
        state.turn_tool_grants.retain(|existing| {
            existing.session_id != grant.session_id || existing.turn_id == grant.turn_id
        });
        if state.turn_tool_grants.contains(&grant) {
            return Ok(ToolApprovalRequestOutcome::AlreadyAllowed);
        }
        if state.pending.contains_key(&request.approval_id) {
            return Err(format!("工具授权请求已存在: {}", request.approval_id));
        }
        let (decision_tx, decision_rx) = mpsc::channel();
        state.pending.insert(
            request.approval_id.clone(),
            PendingApprovalEntry {
                request: request.clone(),
                decision_tx,
            },
        );
        Ok(ToolApprovalRequestOutcome::Pending(ToolApprovalWaiter {
            request,
            decision_rx,
        }))
    }

    pub fn resolve(
        &self,
        session_id: &SessionId,
        approval_id: &str,
        decision: ToolApprovalDecision,
    ) -> Result<PendingToolApproval, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "工具授权状态锁已损坏".to_string())?;
        let Some(entry) = state.pending.remove(approval_id) else {
            return Err("工具授权请求不存在或已经处理".to_string());
        };
        if entry.request.session_id != *session_id {
            state.pending.insert(approval_id.to_string(), entry);
            return Err("工具授权请求不属于当前会话".to_string());
        }
        entry
            .decision_tx
            .send(decision)
            .map_err(|_| "工具授权等待任务已经结束".to_string())?;
        if decision == ToolApprovalDecision::AllowForTurn {
            let grant = grant_for(&entry.request);
            state.turn_tool_grants.insert(grant.clone());
            let matching_approval_ids = state
                .pending
                .iter()
                .filter_map(|(candidate_id, candidate)| {
                    (grant_for(&candidate.request) == grant).then(|| candidate_id.clone())
                })
                .collect::<Vec<_>>();
            for candidate_id in matching_approval_ids {
                if let Some(candidate) = state.pending.remove(&candidate_id) {
                    let _ = candidate
                        .decision_tx
                        .send(ToolApprovalDecision::AllowForTurn);
                }
            }
        }
        Ok(entry.request)
    }

    pub fn cancel(&self, approval_id: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.pending.remove(approval_id);
        }
    }

    pub fn pending_for_session(&self, session_id: &SessionId) -> Vec<PendingToolApproval> {
        let Ok(state) = self.state.lock() else {
            return Vec::new();
        };
        let mut requests = state
            .pending
            .values()
            .filter(|entry| entry.request.session_id == *session_id)
            .map(|entry| entry.request.clone())
            .collect::<Vec<_>>();
        requests.sort_by_key(|request| request.requested_at);
        requests
    }

    pub fn remove_turn(&self, session_id: &SessionId, turn_id: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.pending.retain(|_, entry| {
                entry.request.session_id != *session_id || entry.request.turn_id != turn_id
            });
            state
                .turn_tool_grants
                .retain(|grant| grant.session_id != *session_id || grant.turn_id != turn_id);
        }
    }

    pub fn remove_task(&self, session_id: &SessionId, task_id: &TaskId) {
        if let Ok(mut state) = self.state.lock() {
            state.pending.retain(|_, entry| {
                entry.request.session_id != *session_id || entry.request.task_id != *task_id
            });
        }
    }

    pub fn remove_session(&self, session_id: &SessionId) {
        if let Ok(mut state) = self.state.lock() {
            state
                .pending
                .retain(|_, entry| entry.request.session_id != *session_id);
            state
                .turn_tool_grants
                .retain(|grant| grant.session_id != *session_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(approval_id: &str) -> PendingToolApproval {
        PendingToolApproval {
            approval_id: approval_id.to_string(),
            session_id: SessionId::new("session-approval"),
            task_id: TaskId::new("task-approval"),
            turn_id: "turn-approval".to_string(),
            tool_call_id: "call-approval".to_string(),
            tool_name: "file_write".to_string(),
            reason: "需要写入文件".to_string(),
            requested_at: UtcMillis(1),
        }
    }

    #[test]
    fn allow_for_turn_reuses_only_matching_turn_tool_scope() {
        let registry = ToolApprovalRegistry::default();
        let ToolApprovalRequestOutcome::Pending(waiter) = registry
            .request(request("approval-1"))
            .expect("request approval")
        else {
            panic!("first request must wait");
        };
        registry
            .resolve(
                &SessionId::new("session-approval"),
                "approval-1",
                ToolApprovalDecision::AllowForTurn,
            )
            .expect("resolve approval");
        assert_eq!(
            waiter.decision_rx.recv().expect("receive decision"),
            ToolApprovalDecision::AllowForTurn
        );

        let mut same_scope = request("approval-2");
        same_scope.tool_call_id = "call-approval-2".to_string();
        assert!(matches!(
            registry.request(same_scope).expect("reuse turn grant"),
            ToolApprovalRequestOutcome::AlreadyAllowed
        ));

        let mut next_turn = request("approval-3");
        next_turn.turn_id = "turn-next".to_string();
        assert!(matches!(
            registry.request(next_turn).expect("next turn request"),
            ToolApprovalRequestOutcome::Pending(_)
        ));
    }

    #[test]
    fn allow_for_turn_releases_matching_pending_calls_across_tasks() {
        let registry = ToolApprovalRegistry::default();
        let ToolApprovalRequestOutcome::Pending(first) = registry
            .request(request("approval-first"))
            .expect("first approval")
        else {
            panic!("first request must wait");
        };
        let mut sibling_request = request("approval-sibling");
        sibling_request.task_id = TaskId::new("task-sibling");
        sibling_request.tool_call_id = "call-sibling".to_string();
        let ToolApprovalRequestOutcome::Pending(sibling) =
            registry.request(sibling_request).expect("sibling approval")
        else {
            panic!("sibling request must initially wait");
        };

        registry
            .resolve(
                &SessionId::new("session-approval"),
                "approval-first",
                ToolApprovalDecision::AllowForTurn,
            )
            .expect("resolve turn approval");

        assert_eq!(
            first.decision_rx.recv().expect("first decision"),
            ToolApprovalDecision::AllowForTurn
        );
        assert_eq!(
            sibling.decision_rx.recv().expect("sibling decision"),
            ToolApprovalDecision::AllowForTurn
        );
        assert!(
            registry
                .pending_for_session(&SessionId::new("session-approval"))
                .is_empty()
        );
    }

    #[test]
    fn failed_allow_for_turn_delivery_does_not_install_grant() {
        let registry = ToolApprovalRegistry::default();
        let ToolApprovalRequestOutcome::Pending(waiter) = registry
            .request(request("approval-stale"))
            .expect("stale approval")
        else {
            panic!("request must wait");
        };
        drop(waiter);

        assert!(
            registry
                .resolve(
                    &SessionId::new("session-approval"),
                    "approval-stale",
                    ToolApprovalDecision::AllowForTurn,
                )
                .is_err()
        );

        let mut retry = request("approval-retry");
        retry.tool_call_id = "call-retry".to_string();
        assert!(matches!(
            registry.request(retry).expect("retry approval"),
            ToolApprovalRequestOutcome::Pending(_)
        ));
    }

    #[test]
    fn removing_turn_cancels_pending_and_grants() {
        let registry = ToolApprovalRegistry::default();
        let ToolApprovalRequestOutcome::Pending(waiter) = registry
            .request(request("approval-turn"))
            .expect("turn approval")
        else {
            panic!("request must wait");
        };
        registry.remove_turn(&SessionId::new("session-approval"), "turn-approval");
        assert!(waiter.decision_rx.recv().is_err());
        assert!(
            registry
                .pending_for_session(&SessionId::new("session-approval"))
                .is_empty()
        );
    }
}
