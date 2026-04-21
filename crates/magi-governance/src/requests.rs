use magi_core::{ApprovalRequirement, AssignmentId, MissionId, RiskLevel, TaskId, WorkerId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolKind {
    Builtin,
    Mcp,
    SkillBound,
    HostBound,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolExecutionRequest {
    pub tool_name: String,
    pub tool_kind: ToolKind,
    pub risk_level: RiskLevel,
    pub approval_requirement: ApprovalRequirement,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxRequest {
    pub command: String,
    pub working_directory: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathAccessRequest {
    pub absolute_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerControlKind {
    Execute,
    Review,
    Verify,
    Repair,
    RepairRetry,
    Finish,
    Fail,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerControlRequest {
    pub worker_id: Option<WorkerId>,
    pub mission_id: Option<MissionId>,
    pub assignment_id: Option<AssignmentId>,
    pub task_id: Option<TaskId>,
    pub action: WorkerControlKind,
    pub risk_level: RiskLevel,
    pub approval_requirement: ApprovalRequirement,
    pub retry_count: usize,
    pub blocked: bool,
    pub reason: Option<String>,
}
