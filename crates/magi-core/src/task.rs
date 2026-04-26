use serde::{Deserialize, Serialize};

use crate::ids::{LeaseId, MissionId, TaskId, WorkerId};
use crate::value_objects::UtcMillis;

// ---------------------------------------------------------------------------
// TaskKind
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskKind {
    // Plan nodes (can appear in initial graph)
    Objective,
    Phase,
    WorkPackage,
    Action,
    Validation,
    // Runtime nodes (only created during execution)
    Repair,
    Decision,
}

// ---------------------------------------------------------------------------
// TaskStatus
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Draft,
    Ready,
    Running,
    Blocked,
    AwaitingApproval,
    Verifying,
    Repairing,
    Completed,
    Failed,
    Cancelled,
    Skipped,
}

// ---------------------------------------------------------------------------
// ExecutorBinding
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutorBinding {
    pub target_role: String,
    pub capability_requirements: Vec<String>,
    pub parallelism_group: Option<String>,
    pub exclusive_scope: Option<String>,
    pub worker_selector: Option<String>,
}

// ---------------------------------------------------------------------------
// PolicyDispatchDecision
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyDispatchDecision {
    /// 允许派发。
    Allow,
    /// 拒绝派发，附带原因。
    Reject(String),
    /// 需要审批，创建 Decision Task。
    NeedsApproval(String),
}

// ---------------------------------------------------------------------------
// TaskPolicy
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskPolicy {
    pub autonomy_level: String,
    pub approval_mode: String,
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
    pub allowed_paths: Vec<String>,
    pub denied_paths: Vec<String>,
    pub network_mode: String,
    pub command_mode: String,
    pub retry_limit: u32,
    pub repair_limit: u32,
    pub validation_profile: Option<String>,
    pub checkpoint_mode: String,
    pub background_allowed: bool,
    pub escalation_conditions: Vec<String>,
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub task_id: TaskId,
    pub mission_id: MissionId,
    pub root_task_id: TaskId,
    pub parent_task_id: Option<TaskId>,
    pub kind: TaskKind,
    pub title: String,
    pub goal: String,
    pub status: TaskStatus,
    pub dependency_ids: Vec<TaskId>,
    pub required_children: Vec<TaskId>,
    pub policy_snapshot: Option<TaskPolicy>,
    pub executor_binding: Option<ExecutorBinding>,
    pub context_refs: Vec<String>,
    pub knowledge_refs: Vec<String>,
    pub workspace_scope: Option<String>,
    pub write_scope: Option<String>,
    pub input_refs: Vec<String>,
    pub output_refs: Vec<String>,
    pub evidence_refs: Vec<String>,
    pub retry_count: u32,
    pub repair_count: u32,
    /// Payload for Decision tasks (design 12).
    pub decision_payload: Option<DecisionTaskPayload>,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

// ---------------------------------------------------------------------------
// AssignmentLease
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssignmentLease {
    pub lease_id: LeaseId,
    pub task_id: TaskId,
    pub root_task_id: TaskId,
    pub worker_id: WorkerId,
    pub role: String,
    pub granted_at: UtcMillis,
    pub expires_at: UtcMillis,
    pub heartbeat_at: UtcMillis,
    pub lease_status: LeaseStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeaseStatus {
    Active,
    Completed,
    Expired,
    Revoked,
}

// ---------------------------------------------------------------------------
// DecisionTaskPayload
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DecisionTaskPayload {
    pub decision_context: String,
    pub blocked_reason: String,
    pub target_task_id: Option<TaskId>,
    pub options: Vec<DecisionOption>,
    pub risk_notes: Vec<String>,
    pub recommended_option: Option<String>,
    pub required_user_input: bool,
    pub decision_evidence: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DecisionOption {
    pub option_id: String,
    pub label: String,
    pub description: String,
}

// ---------------------------------------------------------------------------
// TaskProjection (view contract)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskProjection {
    pub root_task: Task,
    pub tasks: Vec<Task>,
    pub current_phase: Option<String>,
    pub running_tasks: Vec<TaskId>,
    pub blocked_tasks: Vec<TaskId>,
    pub pending_decisions: Vec<TaskId>,
    pub workpackage_summaries: Vec<WorkPackageSummary>,
    pub validation_summary: Option<String>,
    pub progress_summary: ProgressSummary,
    pub aggregate_status: TaskStatus,
    pub display_status: String,
    pub execution_mode: String,
    pub runner_status: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProgressSummary {
    pub total_tasks: u32,
    pub completed_tasks: u32,
    pub settled_tasks: u32,
    pub failed_tasks: u32,
    pub running_tasks: u32,
    pub blocked_tasks: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkPackageSummary {
    pub task_id: String,
    pub title: String,
    pub aggregate_status: TaskStatus,
    pub display_status: String,
    pub progress_ratio: f32,
    pub recent_evidence: Vec<String>,
    pub recent_issues: Vec<String>,
}
