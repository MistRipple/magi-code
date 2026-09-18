use magi_core::{ExecutionResultStatus, SessionId, TaskId, UtcMillis};
use magi_session_store::SessionStore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, mpsc};

/// 用户审批的最长等待时间。过期后不会执行原始工具调用，也不会写入拒绝记忆；
/// 后续新的模型调用可以重新请求同一组参数的审批。
pub const TOOL_APPROVAL_TTL_MILLIS: u64 = 5 * 60 * 1_000;

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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SessionToolCallFingerprint {
    session_id: SessionId,
    turn_id: String,
    tool_name: String,
    normalized_arguments: String,
}

fn grant_for(request: &PendingToolApproval) -> TurnToolGrant {
    TurnToolGrant {
        session_id: request.session_id.clone(),
        turn_id: request.turn_id.clone(),
        tool_name: magi_tool_runtime::canonical_builtin_tool_name(&request.tool_name)
            .unwrap_or_else(|| request.tool_name.trim().to_ascii_lowercase()),
    }
}

fn fingerprint_for(request: &PendingToolApproval, arguments: &str) -> SessionToolCallFingerprint {
    SessionToolCallFingerprint {
        session_id: request.session_id.clone(),
        turn_id: request.turn_id.clone(),
        tool_name: magi_tool_runtime::canonical_builtin_tool_name(&request.tool_name)
            .unwrap_or_else(|| request.tool_name.trim().to_ascii_lowercase()),
        normalized_arguments: normalize_arguments(arguments),
    }
}

fn normalize_arguments(arguments: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(arguments) else {
        return arguments.trim().to_string();
    };
    serde_json::to_string(&canonicalize_json(&value))
        .unwrap_or_else(|_| arguments.trim().to_string())
}

fn canonicalize_json(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_json).collect()),
        Value::Object(object) => {
            let sorted = object
                .iter()
                .map(|(key, value)| (key.clone(), canonicalize_json(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        }
        _ => value.clone(),
    }
}

#[derive(Debug)]
struct PendingApprovalEntry {
    request: PendingToolApproval,
    fingerprint: SessionToolCallFingerprint,
    decision_tx: mpsc::Sender<ToolApprovalDecision>,
}

#[derive(Debug)]
struct ExpiredToolApproval {
    task_id: TaskId,
    turn_id: String,
}

#[derive(Debug, Default)]
struct ToolApprovalState {
    pending: HashMap<String, PendingApprovalEntry>,
    turn_tool_grants: HashSet<TurnToolGrant>,
    denied_session_tool_calls: HashSet<SessionToolCallFingerprint>,
    expired: HashMap<(SessionId, String), ExpiredToolApproval>,
}

pub struct ToolApprovalWaiter {
    pub request: PendingToolApproval,
    pub decision_rx: mpsc::Receiver<ToolApprovalDecision>,
}

pub enum ToolApprovalRequestOutcome {
    AlreadyAllowed,
    PreviouslyDenied,
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
    fn expire_stale_locked(state: &mut ToolApprovalState, now: UtcMillis) {
        let expired = state
            .pending
            .iter()
            .filter(|(_, entry)| {
                entry
                    .request
                    .requested_at
                    .0
                    .saturating_add(TOOL_APPROVAL_TTL_MILLIS)
                    <= now.0
            })
            .map(|(approval_id, entry)| (approval_id.clone(), entry.request.session_id.clone()))
            .collect::<Vec<_>>();
        for (approval_id, session_id) in expired {
            if let Some(entry) = state.pending.remove(&approval_id) {
                state.expired.insert(
                    (session_id, approval_id),
                    ExpiredToolApproval {
                        task_id: entry.request.task_id,
                        turn_id: entry.request.turn_id,
                    },
                );
            }
        }
    }

    /// 清理指定时间之前已过期的审批，并关闭对应等待通道。
    pub fn expire_stale(&self, now: UtcMillis) -> usize {
        let Ok(mut state) = self.state.lock() else {
            return 0;
        };
        let before = state.pending.len();
        Self::expire_stale_locked(&mut state, now);
        before.saturating_sub(state.pending.len())
    }

    /// 判断审批是否刚因超时被清理。等待线程用该事实区分“过期”与运行时通道故障。
    pub fn is_expired(&self, session_id: &SessionId, approval_id: &str) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        Self::expire_stale_locked(&mut state, UtcMillis::now());
        state
            .expired
            .contains_key(&(session_id.clone(), approval_id.to_string()))
    }

    pub fn request(
        &self,
        request: PendingToolApproval,
    ) -> Result<ToolApprovalRequestOutcome, String> {
        self.request_with_arguments(request, "")
    }

    pub fn request_with_arguments(
        &self,
        request: PendingToolApproval,
        arguments: &str,
    ) -> Result<ToolApprovalRequestOutcome, String> {
        let grant = grant_for(&request);
        let fingerprint = fingerprint_for(&request, arguments);
        let mut state = self
            .state
            .lock()
            .map_err(|_| "工具授权状态锁已损坏".to_string())?;
        Self::expire_stale_locked(&mut state, UtcMillis::now());
        state.expired.retain(|(session_id, approval_id), _| {
            session_id != &request.session_id || approval_id != &request.approval_id
        });
        state.turn_tool_grants.retain(|existing| {
            existing.session_id != grant.session_id || existing.turn_id == grant.turn_id
        });
        if state.turn_tool_grants.contains(&grant) {
            return Ok(ToolApprovalRequestOutcome::AlreadyAllowed);
        }
        if state.denied_session_tool_calls.contains(&fingerprint) {
            return Ok(ToolApprovalRequestOutcome::PreviouslyDenied);
        }
        if state.pending.contains_key(&request.approval_id) {
            return Err(format!("工具授权请求已存在: {}", request.approval_id));
        }
        let (decision_tx, decision_rx) = mpsc::channel();
        state.pending.insert(
            request.approval_id.clone(),
            PendingApprovalEntry {
                request: request.clone(),
                fingerprint,
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
        Self::expire_stale_locked(&mut state, UtcMillis::now());
        let Some(entry) = state.pending.remove(approval_id) else {
            if state
                .expired
                .contains_key(&(session_id.clone(), approval_id.to_string()))
            {
                return Err("工具授权请求已过期".to_string());
            }
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
        if decision == ToolApprovalDecision::Deny {
            state.denied_session_tool_calls.insert(entry.fingerprint);
        } else if decision == ToolApprovalDecision::AllowForTurn {
            let grant = grant_for(&entry.request);
            state.turn_tool_grants.insert(grant.clone());
            let matching_approval_ids = state
                .pending
                .iter()
                .filter(|(_, candidate)| grant_for(&candidate.request) == grant)
                .map(|(candidate_id, _)| candidate_id.clone())
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
        let Ok(mut state) = self.state.lock() else {
            return Vec::new();
        };
        Self::expire_stale_locked(&mut state, UtcMillis::now());
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
            state.denied_session_tool_calls.retain(|fingerprint| {
                fingerprint.session_id != *session_id || fingerprint.turn_id != turn_id
            });
            state.expired.retain(|(expired_session, _), entry| {
                expired_session != session_id || entry.turn_id != turn_id
            });
        }
    }

    /// 开始一次新的用户请求，清理上一次请求留下的授权决定。
    ///
    /// 模型在同一次用户请求内可能经历多个内部执行子轮，不能在子轮边界清理拒绝记忆；
    /// 只有新的用户请求开始时，才允许相同调用再次进入授权流程。
    pub fn begin_turn(&self, session_id: &SessionId) {
        if let Ok(mut state) = self.state.lock() {
            state
                .pending
                .retain(|_, entry| entry.request.session_id != *session_id);
            state
                .turn_tool_grants
                .retain(|grant| grant.session_id != *session_id);
            state
                .denied_session_tool_calls
                .retain(|fingerprint| fingerprint.session_id != *session_id);
            state
                .expired
                .retain(|(expired_session, _), _| expired_session != session_id);
        }
    }

    pub fn remove_task(&self, session_id: &SessionId, task_id: &TaskId) {
        if let Ok(mut state) = self.state.lock() {
            state.pending.retain(|_, entry| {
                entry.request.session_id != *session_id || entry.request.task_id != *task_id
            });
            state.expired.retain(|(expired_session, _), entry| {
                expired_session != session_id || entry.task_id != *task_id
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
            state
                .denied_session_tool_calls
                .retain(|fingerprint| fingerprint.session_id != *session_id);
            state
                .expired
                .retain(|(expired_session, _), _| expired_session != session_id);
        }
    }
}

pub(crate) fn rejected_tool_approval_result(
    tool_name: &str,
    approval_id: &str,
    repeated: bool,
) -> (String, ExecutionResultStatus) {
    (
        serde_json::json!({
            "tool": tool_name,
            "status": "rejected",
            "error_code": "tool_approval_denied",
            "error": if repeated {
                "用户已拒绝相同的工具操作，本轮不会再次请求授权"
            } else {
                "用户拒绝了本次工具操作"
            },
            "approval_id": approval_id,
            "retryable_with_same_arguments": false,
            "instruction": "不要重复相同调用；如需继续，请修改操作范围或参数后再尝试。",
        })
        .to_string(),
        ExecutionResultStatus::Rejected,
    )
}

pub(crate) fn expired_tool_approval_result(
    tool_name: &str,
    approval_id: &str,
) -> (String, ExecutionResultStatus) {
    (
        serde_json::json!({
            "tool": tool_name,
            "status": "rejected",
            "error_code": "tool_approval_expired",
            "error": "工具授权请求已过期，原始操作未执行",
            "approval_id": approval_id,
            "retryable_with_same_arguments": false,
            "instruction": "授权窗口已过期；如需继续，请重新发起操作。",
        })
        .to_string(),
        ExecutionResultStatus::Rejected,
    )
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
            requested_at: UtcMillis::now(),
        }
    }

    #[test]
    fn stale_approval_closes_waiter_and_can_be_requested_again() {
        let registry = ToolApprovalRegistry::default();
        let stale = request("approval-expired");
        let requested_at = stale.requested_at;
        let ToolApprovalRequestOutcome::Pending(waiter) = registry
            .request(stale.clone())
            .expect("approval should become pending")
        else {
            panic!("approval must initially wait");
        };

        assert_eq!(
            registry.expire_stale(UtcMillis(requested_at.0 + TOOL_APPROVAL_TTL_MILLIS + 1,)),
            1
        );
        assert!(registry.pending_for_session(&stale.session_id).is_empty());
        assert!(waiter.decision_rx.recv().is_err());
        assert!(registry.is_expired(&stale.session_id, &stale.approval_id));
        assert_eq!(
            registry
                .resolve(
                    &stale.session_id,
                    &stale.approval_id,
                    ToolApprovalDecision::AllowOnce,
                )
                .expect_err("expired approval must not resolve"),
            "工具授权请求已过期"
        );

        let mut retry = stale;
        retry.requested_at = UtcMillis::now();
        let ToolApprovalRequestOutcome::Pending(retry_waiter) = registry
            .request_with_arguments(retry, r#"{"path":"src/a.txt"}"#)
            .expect("same approval id should be requestable again")
        else {
            panic!("reissued approval must wait");
        };
        registry
            .resolve(
                &SessionId::new("session-approval"),
                "approval-expired",
                ToolApprovalDecision::Deny,
            )
            .expect("retry approval should resolve");
        assert_eq!(
            retry_waiter
                .decision_rx
                .recv()
                .expect("retry decision should arrive"),
            ToolApprovalDecision::Deny
        );
        assert!(
            registry
                .pending_for_session(&SessionId::new("session-approval"))
                .is_empty()
        );
    }

    #[test]
    fn expired_approval_does_not_create_deny_memory() {
        let registry = ToolApprovalRegistry::default();
        let stale = request("approval-expired-no-deny");
        let requested_at = stale.requested_at;
        let ToolApprovalRequestOutcome::Pending(waiter) = registry
            .request_with_arguments(stale.clone(), r#"{"path":"src/a.txt"}"#)
            .expect("approval should become pending")
        else {
            panic!("approval must initially wait");
        };
        assert_eq!(
            registry.expire_stale(UtcMillis(requested_at.0 + TOOL_APPROVAL_TTL_MILLIS + 1,)),
            1
        );
        assert!(waiter.decision_rx.recv().is_err());

        let mut retry = stale;
        retry.approval_id = "approval-expired-no-deny-retry".to_string();
        retry.requested_at = UtcMillis::now();
        assert!(matches!(
            registry
                .request_with_arguments(retry, r#"{"path":"src/a.txt"}"#)
                .expect("expiry must not be remembered as denial"),
            ToolApprovalRequestOutcome::Pending(_)
        ));
    }

    #[test]
    fn removing_task_clears_expired_approval_metadata() {
        let registry = ToolApprovalRegistry::default();
        let stale = request("approval-expired-task-cleanup");
        let requested_at = stale.requested_at;
        let task_id = stale.task_id.clone();
        let ToolApprovalRequestOutcome::Pending(waiter) = registry
            .request(stale.clone())
            .expect("approval should become pending")
        else {
            panic!("approval must initially wait");
        };
        assert_eq!(
            registry.expire_stale(UtcMillis(requested_at.0 + TOOL_APPROVAL_TTL_MILLIS + 1,)),
            1
        );
        assert!(waiter.decision_rx.recv().is_err());
        assert!(registry.is_expired(&stale.session_id, &stale.approval_id));

        registry.remove_task(&stale.session_id, &task_id);

        assert!(!registry.is_expired(&stale.session_id, &stale.approval_id));
    }

    #[test]
    fn removing_other_turn_keeps_expired_approval_for_original_turn() {
        let registry = ToolApprovalRegistry::default();
        let mut stale = request("approval-expired-turn-cleanup");
        stale.turn_id = "turn-expired".to_string();
        let requested_at = stale.requested_at;
        let ToolApprovalRequestOutcome::Pending(waiter) = registry
            .request(stale.clone())
            .expect("approval should become pending")
        else {
            panic!("approval must initially wait");
        };
        assert_eq!(
            registry.expire_stale(UtcMillis(requested_at.0 + TOOL_APPROVAL_TTL_MILLIS + 1,)),
            1
        );
        assert!(waiter.decision_rx.recv().is_err());

        registry.remove_turn(&stale.session_id, "turn-other");

        assert!(registry.is_expired(&stale.session_id, &stale.approval_id));

        registry.remove_turn(&stale.session_id, "turn-expired");

        assert!(!registry.is_expired(&stale.session_id, &stale.approval_id));
    }

    #[test]
    fn removing_session_clears_expired_approval_metadata() {
        let registry = ToolApprovalRegistry::default();
        let stale = request("approval-expired-session-cleanup");
        let requested_at = stale.requested_at;
        let ToolApprovalRequestOutcome::Pending(waiter) = registry
            .request(stale.clone())
            .expect("approval should become pending")
        else {
            panic!("approval must initially wait");
        };
        assert_eq!(
            registry.expire_stale(UtcMillis(requested_at.0 + TOOL_APPROVAL_TTL_MILLIS + 1,)),
            1
        );
        assert!(waiter.decision_rx.recv().is_err());
        assert!(registry.is_expired(&stale.session_id, &stale.approval_id));

        registry.remove_session(&stale.session_id);

        assert!(!registry.is_expired(&stale.session_id, &stale.approval_id));
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
    fn resolving_from_another_session_keeps_the_pending_request() {
        let registry = ToolApprovalRegistry::default();
        let ToolApprovalRequestOutcome::Pending(waiter) = registry
            .request(request("approval-session-scope"))
            .expect("session-scoped approval")
        else {
            panic!("request must wait");
        };

        let error = registry
            .resolve(
                &SessionId::new("another-session"),
                "approval-session-scope",
                ToolApprovalDecision::AllowOnce,
            )
            .expect_err("another session must not resolve the approval");
        assert!(error.contains("不属于当前会话"));
        assert_eq!(
            registry.pending_for_session(&SessionId::new("session-approval")),
            vec![waiter.request.clone()]
        );

        registry
            .resolve(
                &SessionId::new("session-approval"),
                "approval-session-scope",
                ToolApprovalDecision::AllowOnce,
            )
            .expect("owning session should resolve the approval");
        assert_eq!(
            waiter.decision_rx.recv().expect("receive approval"),
            ToolApprovalDecision::AllowOnce
        );
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

    #[test]
    fn deny_remembers_only_the_same_call_within_the_turn() {
        let registry = ToolApprovalRegistry::default();
        let first = request("approval-deny-first");
        let ToolApprovalRequestOutcome::Pending(waiter) = registry
            .request_with_arguments(first, r#"{"path":"src/a.txt","content":"a"}"#)
            .expect("first approval")
        else {
            panic!("first request must wait");
        };
        registry
            .resolve(
                &SessionId::new("session-approval"),
                "approval-deny-first",
                ToolApprovalDecision::Deny,
            )
            .expect("deny approval");
        assert_eq!(
            waiter.decision_rx.recv().expect("receive denial"),
            ToolApprovalDecision::Deny
        );

        let mut same_call = request("approval-deny-repeat");
        same_call.tool_call_id = "call-repeat".to_string();
        assert!(matches!(
            registry
                .request_with_arguments(same_call, r#"{ "content": "a", "path": "src/a.txt" }"#,)
                .expect("same call should be remembered"),
            ToolApprovalRequestOutcome::PreviouslyDenied
        ));

        let mut changed_call = request("approval-deny-changed");
        changed_call.tool_call_id = "call-changed".to_string();
        let ToolApprovalRequestOutcome::Pending(changed_waiter) = registry
            .request_with_arguments(changed_call, r#"{"path":"src/b.txt","content":"a"}"#)
            .expect("changed call can ask again")
        else {
            panic!("changed arguments must create a new approval request");
        };
        registry
            .resolve(
                &SessionId::new("session-approval"),
                &changed_waiter.request.approval_id,
                ToolApprovalDecision::AllowOnce,
            )
            .expect("changed arguments should be independently resolvable");
        assert_eq!(
            changed_waiter
                .decision_rx
                .recv()
                .expect("receive changed-argument decision"),
            ToolApprovalDecision::AllowOnce
        );
    }

    #[test]
    fn deny_memory_is_removed_with_the_turn() {
        let registry = ToolApprovalRegistry::default();
        let ToolApprovalRequestOutcome::Pending(waiter) = registry
            .request_with_arguments(request("approval-deny-cleanup"), r#"{"path":"a"}"#)
            .expect("approval")
        else {
            panic!("request must wait");
        };
        registry
            .resolve(
                &SessionId::new("session-approval"),
                "approval-deny-cleanup",
                ToolApprovalDecision::Deny,
            )
            .expect("deny approval");
        assert_eq!(
            waiter.decision_rx.recv().expect("receive denial"),
            ToolApprovalDecision::Deny
        );

        registry.remove_turn(&SessionId::new("session-approval"), "turn-approval");
        assert!(matches!(
            registry
                .request_with_arguments(request("approval-after-cleanup"), r#"{"path":"a"}"#)
                .expect("request after turn cleanup"),
            ToolApprovalRequestOutcome::Pending(_)
        ));
    }

    #[test]
    fn removing_an_older_turn_does_not_clear_a_new_turn_denial() {
        let registry = ToolApprovalRegistry::default();
        let mut old_turn = request("approval-old-turn");
        old_turn.turn_id = "turn-old".to_string();
        let ToolApprovalRequestOutcome::Pending(old_waiter) = registry
            .request_with_arguments(old_turn, r#"{"path":"a"}"#)
            .expect("old turn approval")
        else {
            panic!("old turn request must wait");
        };
        registry
            .resolve(
                &SessionId::new("session-approval"),
                "approval-old-turn",
                ToolApprovalDecision::Deny,
            )
            .expect("old turn denial");
        assert_eq!(
            old_waiter.decision_rx.recv().expect("old denial"),
            ToolApprovalDecision::Deny
        );

        let mut new_turn = request("approval-new-turn");
        new_turn.turn_id = "turn-new".to_string();
        let ToolApprovalRequestOutcome::Pending(new_waiter) = registry
            .request_with_arguments(new_turn, r#"{"path":"a"}"#)
            .expect("new turn approval")
        else {
            panic!("new turn request must wait");
        };
        registry
            .resolve(
                &SessionId::new("session-approval"),
                "approval-new-turn",
                ToolApprovalDecision::Deny,
            )
            .expect("new turn denial");
        assert_eq!(
            new_waiter.decision_rx.recv().expect("new denial"),
            ToolApprovalDecision::Deny
        );

        registry.remove_turn(&SessionId::new("session-approval"), "turn-old");
        let mut retry = request("approval-new-turn-retry");
        retry.turn_id = "turn-new".to_string();
        assert!(matches!(
            registry
                .request_with_arguments(retry, r#"{"path":"a"}"#)
                .expect("new turn denial should remain scoped"),
            ToolApprovalRequestOutcome::PreviouslyDenied
        ));
    }
}
