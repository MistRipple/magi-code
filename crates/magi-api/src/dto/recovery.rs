use magi_core::{EventId, UtcMillis, WorkerId};
use magi_orchestrator::RecoveryExecutionResult;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryResumeRequestDto {
    pub recovery_id: String,
    pub worker_id: Option<String>,
}

impl RecoveryResumeRequestDto {
    pub fn requested_recovery_id(&self) -> Option<String> {
        trimmed_non_empty(Some(self.recovery_id.as_str()))
    }

    pub fn requested_worker_id(&self) -> Option<WorkerId> {
        trimmed_non_empty(self.worker_id.as_deref()).map(WorkerId::new)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RecoveryResumeResponseDto {
    pub recovery_id: String,
    pub snapshot_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub mission_id: String,
    pub assignment_id: String,
    pub task_id: String,
    pub worker_id: String,
    pub event_id: String,
    pub resumed_at: UtcMillis,
    pub memory_writeback_applied: bool,
}

impl RecoveryResumeResponseDto {
    pub fn new(
        result: &RecoveryExecutionResult,
        event_id: EventId,
        resumed_at: UtcMillis,
        memory_writeback_applied: bool,
    ) -> Self {
        let worker_id = result
            .decision
            .worker_id
            .clone()
            .or_else(|| result.recovery_input.ownership.worker_id.clone())
            .expect("recovery resume response requires worker id");
        Self {
            recovery_id: result.recovery_input.recovery_id.clone(),
            snapshot_id: result.recovery_input.snapshot_id.clone(),
            session_id: result
                .recovery_input
                .ownership
                .session_id
                .as_ref()
                .map(ToString::to_string),
            workspace_id: result
                .recovery_input
                .ownership
                .workspace_id
                .as_ref()
                .map(ToString::to_string),
            mission_id: result.decision.mission_id.to_string(),
            assignment_id: result.decision.assignment_id.to_string(),
            task_id: result.decision.task_id.to_string(),
            worker_id: worker_id.to_string(),
            event_id: event_id.to_string(),
            resumed_at,
            memory_writeback_applied,
        }
    }
}

fn trimmed_non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_resume_request_trims_recovery_and_worker_ids() {
        let request = RecoveryResumeRequestDto {
            recovery_id: "  recovery-1  ".to_string(),
            worker_id: Some("  worker-1  ".to_string()),
        };

        assert_eq!(
            request.requested_recovery_id().as_deref(),
            Some("recovery-1")
        );
        assert_eq!(
            request.requested_worker_id().as_ref().map(ToString::to_string),
            Some("worker-1".to_string())
        );
    }

    #[test]
    fn recovery_resume_request_rejects_blank_recovery_id() {
        let request = RecoveryResumeRequestDto {
            recovery_id: "   ".to_string(),
            worker_id: Some("   ".to_string()),
        };

        assert!(request.requested_recovery_id().is_none());
        assert!(request.requested_worker_id().is_none());
    }
}
