use crate::ids::{AssignmentId, MissionId, SessionId, TaskId, WorkerId, WorkspaceId};
use crate::status::DispatchReason;
use crate::value_objects::UtcMillis;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExecutionOwnership {
    pub session_id: Option<SessionId>,
    pub workspace_id: Option<WorkspaceId>,
    pub mission_id: Option<MissionId>,
    pub task_id: Option<TaskId>,
    pub worker_id: Option<WorkerId>,
    pub execution_chain_ref: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskExecutionTarget {
    pub mission_id: MissionId,
    pub root_task_id: TaskId,
    pub task_id: TaskId,
    pub requested_worker_id: Option<WorkerId>,
    pub recovery_id: Option<String>,
    pub execution_chain_ref: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryResumeInput {
    pub recovery_id: String,
    pub snapshot_id: String,
    pub ownership: ExecutionOwnership,
    pub diagnostic_summary: Option<String>,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResumeDispatchDecision {
    pub mission_id: MissionId,
    pub assignment_id: AssignmentId,
    pub task_id: TaskId,
    pub worker_id: Option<WorkerId>,
    pub dispatch_reason: DispatchReason,
    pub recovery_id: String,
    pub execution_chain_ref: Option<String>,
}
