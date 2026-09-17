//! Turn 领域合同。
//!
//! 这些类型把一次响应的身份、执行代际、任务关联和事件序号放在同一个可
//! 序列化合同中。正文仍由 SessionStore 的 canonical Turn 保存；这里不复制
//! 正文，只描述允许跨模块传递的事实边界。

use crate::session_turn_coordinator::{
    CoordinatorTurnStatus, ExecutionProfile, TurnAdmission, TurnAttempt,
};
use magi_core::{LeaseId, SessionId, TaskId, UtcMillis};
use magi_session_store::{CanonicalTurn, CanonicalTurnItem};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// canonical Turn 的跨模块身份记录。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TurnRecord {
    pub session_id: SessionId,
    pub turn_id: String,
    pub turn_seq: u64,
    pub request_id: String,
    pub request_fingerprint: String,
    pub execution_profile: ExecutionProfile,
    pub attempt_id: String,
    pub status: CoordinatorTurnStatus,
    pub event_sequence: u64,
    pub accepted_at: UtcMillis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<UtcMillis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

impl TurnRecord {
    pub fn admission(&self) -> TurnAdmission {
        TurnAdmission {
            turn_id: self.turn_id.clone(),
            request_id: self.request_id.clone(),
            request_fingerprint: self.request_fingerprint.clone(),
            profile: self.execution_profile,
        }
    }

    pub fn attempt(&self) -> TurnAttempt {
        TurnAttempt {
            turn_id: self.turn_id.clone(),
            attempt_id: self.attempt_id.clone(),
            profile: self.execution_profile,
        }
    }

    /// 从 canonical Turn 提取稳定身份。缺少 requestId/attemptId 的旧事实不会
    /// 被伪造为可重放身份，调用方必须在迁移边界补齐这些字段后再接纳。
    pub fn from_canonical(turn: &CanonicalTurn, event_sequence: u64) -> Option<Self> {
        let string_metadata = |key: &str| {
            turn.metadata
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        let request_id = string_metadata("requestId").or_else(|| string_metadata("request_id"))?;
        let attempt_id = string_metadata("attemptId").or_else(|| string_metadata("attempt_id"))?;
        let request_fingerprint = string_metadata("requestFingerprint")
            .or_else(|| string_metadata("request_fingerprint"))?;
        let execution_profile = match string_metadata("executionProfile")
            .or_else(|| string_metadata("execution_profile"))
            .as_deref()
        {
            Some("task") => ExecutionProfile::Task,
            _ => ExecutionProfile::Conversation,
        };
        let status = match turn.status {
            magi_session_store::CanonicalTurnStatus::Pending => CoordinatorTurnStatus::Accepted,
            magi_session_store::CanonicalTurnStatus::Running => CoordinatorTurnStatus::Running,
            magi_session_store::CanonicalTurnStatus::Blocked => CoordinatorTurnStatus::Blocked,
            magi_session_store::CanonicalTurnStatus::Completed => CoordinatorTurnStatus::Completed,
            magi_session_store::CanonicalTurnStatus::Failed => CoordinatorTurnStatus::Failed,
            magi_session_store::CanonicalTurnStatus::Interrupted
            | magi_session_store::CanonicalTurnStatus::Cancelled
            | magi_session_store::CanonicalTurnStatus::Superseded => {
                CoordinatorTurnStatus::Cancelled
            }
        };
        let task_id = turn.items.iter().find_map(|item| {
            item.worker
                .as_ref()
                .and_then(|worker| worker.task_id.clone())
        });
        let root_task_id = turn.metadata.get("rootTaskId").and_then(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(TaskId::new)
        });
        Some(Self {
            session_id: turn.session_id.clone(),
            turn_id: turn.turn_id.clone(),
            turn_seq: turn.turn_seq,
            request_id,
            request_fingerprint,
            execution_profile,
            attempt_id,
            status,
            event_sequence,
            accepted_at: turn.accepted_at,
            completed_at: turn.completed_at,
            task_id,
            root_task_id,
            causation_id: string_metadata("causationId"),
            trace_id: string_metadata("traceId"),
        })
    }
}

/// TaskStore 事实在 Turn 侧的最小关联记录。TaskStore 仍是 task status 的唯一事实源。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskRunRecord {
    pub session_id: SessionId,
    pub turn_id: String,
    pub task_id: TaskId,
    pub root_task_id: TaskId,
    pub attempt_id: String,
    pub status: String,
    pub event_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<LeaseId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

/// Turn 实时通知的统一 envelope。delta 是传输事实，完整 item/turn 是可恢复事实。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TurnEventEnvelope {
    pub event_id: String,
    pub event_type: String,
    pub event_sequence: u64,
    pub session_id: SessionId,
    pub turn_id: String,
    pub turn_seq: u64,
    pub occurred_at: UtcMillis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_content_length: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_length: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_run: Option<TaskRunRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<CanonicalTurnItem>,
}

/// Coordinator 的唯一命令边界。命令只验证生命周期身份；canonical 内容必须由
/// TurnEventSink 在 durable mutation 成功后产生。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnCommand {
    Start(TurnAdmission),
    SetStatus {
        attempt: TurnAttempt,
        status: CoordinatorTurnStatus,
    },
    Steer {
        attempt: TurnAttempt,
        request_id: String,
    },
    Continue {
        previous: TurnAttempt,
        previous_status: CoordinatorTurnStatus,
        next: TurnAdmission,
    },
    Cancel {
        attempt: TurnAttempt,
    },
    Recover {
        admission: TurnAdmission,
        attempt_id: String,
        status: CoordinatorTurnStatus,
        /// canonical Turn 的持久化顺序，用于恢复时拒绝迟到快照。
        turn_seq: u64,
    },
    Finish {
        attempt: TurnAttempt,
        status: CoordinatorTurnStatus,
    },
    Abort {
        attempt: TurnAttempt,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::SessionId;

    fn canonical(status: magi_session_store::CanonicalTurnStatus) -> CanonicalTurn {
        CanonicalTurn {
            session_id: SessionId::new("turn-contract-session"),
            turn_id: "turn-contract".to_string(),
            turn_seq: 7,
            accepted_at: UtcMillis(10),
            completed_at: status.is_terminal().then_some(UtcMillis(20)),
            status,
            response_duration_ms: None,
            usage: None,
            items: Vec::new(),
            metadata: serde_json::json!({
                "requestId": "request-contract",
                "requestFingerprint": "sha256:contract",
                "executionProfile": "task",
                "attemptId": "attempt-contract",
                "rootTaskId": "task-contract"
            })
            .as_object()
            .cloned()
            .unwrap()
            .into_iter()
            .collect(),
        }
    }

    #[test]
    fn turn_record_preserves_canonical_identity_and_task_link() {
        let record = TurnRecord::from_canonical(
            &canonical(magi_session_store::CanonicalTurnStatus::Completed),
            42,
        )
        .expect("complete canonical turn should expose its identity");
        assert_eq!(record.session_id, SessionId::new("turn-contract-session"));
        assert_eq!(record.turn_id, "turn-contract");
        assert_eq!(record.turn_seq, 7);
        assert_eq!(record.request_id, "request-contract");
        assert_eq!(record.request_fingerprint, "sha256:contract");
        assert_eq!(record.execution_profile, ExecutionProfile::Task);
        assert_eq!(record.attempt_id, "attempt-contract");
        assert_eq!(record.status, CoordinatorTurnStatus::Completed);
        assert_eq!(record.event_sequence, 42);
        assert_eq!(record.root_task_id, Some(TaskId::new("task-contract")));
        assert_eq!(record.admission().request_id, "request-contract");
        assert_eq!(record.attempt().attempt_id, "attempt-contract");
    }

    #[test]
    fn legacy_canonical_turn_without_request_identity_is_not_replayable() {
        let mut turn = canonical(magi_session_store::CanonicalTurnStatus::Running);
        turn.metadata.clear();
        assert!(TurnRecord::from_canonical(&turn, 1).is_none());
    }
}
