use serde::{Deserialize, Serialize};

use magi_core::{AssignmentId, MissionId, SessionId, UtcMillis, WorkerId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoType {
    Discovery,
    Design,
    Implementation,
    Verification,
    Integration,
    Fix,
    Refactor,
}

impl TodoType {
    pub const ALL: [TodoType; 7] = [
        Self::Discovery,
        Self::Design,
        Self::Implementation,
        Self::Verification,
        Self::Integration,
        Self::Fix,
        Self::Refactor,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
    Cancelled,
}

impl TodoStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Skipped | Self::Cancelled
        )
    }

    pub const ALL: [TodoStatus; 6] = [
        Self::Pending,
        Self::Running,
        Self::Completed,
        Self::Failed,
        Self::Skipped,
        Self::Cancelled,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoProjectionStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
    Cancelled,
    Blocked,
    Ready,
}

impl TodoProjectionStatus {
    pub fn canonicalize(self) -> TodoStatus {
        match self {
            Self::Blocked | Self::Ready | Self::Pending => TodoStatus::Pending,
            Self::Running => TodoStatus::Running,
            Self::Completed => TodoStatus::Completed,
            Self::Failed => TodoStatus::Failed,
            Self::Skipped => TodoStatus::Skipped,
            Self::Cancelled => TodoStatus::Cancelled,
        }
    }

    pub fn is_execution_candidate(self) -> bool {
        matches!(self, Self::Pending | Self::Blocked | Self::Ready)
    }

    pub fn is_skippable(self) -> bool {
        matches!(self, Self::Pending | Self::Blocked | Self::Ready)
    }
}

impl From<TodoStatus> for TodoProjectionStatus {
    fn from(s: TodoStatus) -> Self {
        match s {
            TodoStatus::Pending => Self::Pending,
            TodoStatus::Running => Self::Running,
            TodoStatus::Completed => Self::Completed,
            TodoStatus::Failed => Self::Failed,
            TodoStatus::Skipped => Self::Skipped,
            TodoStatus::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoExecutionBlocker {
    Dependencies,
    Contracts,
    Approval,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Rejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalSeverity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoSource {
    PlannerMacro,
    WorkerSplit,
    OrchestratorAdjustment,
    ReviewFix,
    SystemRepair,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Approved,
    NeedsRevision,
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TodoOutput {
    pub success: bool,
    pub summary: String,
    pub modified_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_contracts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issues: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<TokenUsage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnifiedTodo {
    pub id: String,
    pub session_id: SessionId,
    pub mission_id: MissionId,
    pub assignment_id: AssignmentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub source: TodoSource,

    pub content: String,
    pub reasoning: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,

    pub todo_type: TodoType,
    pub worker_id: WorkerId,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default = "default_effort_weight")]
    pub effort_weight: f64,
    #[serde(default)]
    pub waiver_approved: bool,
    #[serde(default = "default_priority")]
    pub priority: u8,

    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub required_contracts: Vec<String>,
    #[serde(default)]
    pub produces_contracts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_blocker: Option<TodoExecutionBlocker>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,

    #[serde(default)]
    pub out_of_scope: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_status: Option<ApprovalStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_severity: Option<ApprovalSeverity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_status: Option<ReviewStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_feedback: Option<String>,

    pub status: TodoStatus,
    #[serde(default)]
    pub progress: u8,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_at: Option<UtcMillis>,
    #[serde(default)]
    pub retry_count: u32,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<TodoOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_files: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_files: Option<Vec<String>>,

    pub created_at: UtcMillis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<UtcMillis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<UtcMillis>,
}

fn default_true() -> bool {
    true
}
fn default_effort_weight() -> f64 {
    1.0
}
fn default_priority() -> u8 {
    3
}
fn default_max_retries() -> u32 {
    3
}

#[derive(Clone, Debug)]
pub struct CreateTodoParams {
    pub session_id: Option<SessionId>,
    pub mission_id: MissionId,
    pub assignment_id: AssignmentId,
    pub parent_id: Option<String>,
    pub source: Option<TodoSource>,
    pub content: String,
    pub reasoning: String,
    pub todo_type: TodoType,
    pub worker_id: WorkerId,
    pub priority: Option<u8>,
    pub expected_output: Option<String>,
    pub prompt: Option<String>,
    pub depends_on: Option<Vec<String>>,
    pub required_contracts: Option<Vec<String>>,
    pub produces_contracts: Option<Vec<String>>,
    pub target_files: Option<Vec<String>>,
    pub required: Option<bool>,
    pub effort_weight: Option<f64>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct UpdateTodoParams {
    pub content: Option<String>,
    pub reasoning: Option<String>,
    pub expected_output: Option<String>,
    pub priority: Option<u8>,
    pub depends_on: Option<Vec<String>>,
    pub required_contracts: Option<Vec<String>>,
    pub produces_contracts: Option<Vec<String>>,
    pub required: Option<bool>,
    pub effort_weight: Option<f64>,
    pub waiver_approved: Option<bool>,
    pub review_status: Option<ReviewStatus>,
    pub review_feedback: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct TodoQuery {
    pub session_id: Option<SessionId>,
    pub mission_id: Option<MissionId>,
    pub assignment_id: Option<AssignmentId>,
    pub worker_id: Option<WorkerId>,
    pub status: Option<Vec<TodoProjectionStatus>>,
    pub todo_type: Option<Vec<TodoType>>,
    pub out_of_scope: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TodoStats {
    pub total: usize,
    pub by_status: TodoStatusCounts,
    pub by_type: TodoTypeCounts,
    pub by_worker: Vec<(String, usize)>,
    pub completion_rate: f64,
    pub average_duration_ms: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TodoStatusCounts {
    pub pending: usize,
    pub running: usize,
    pub completed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub cancelled: usize,
}

impl TodoStatusCounts {
    pub fn increment(&mut self, status: TodoStatus) {
        match status {
            TodoStatus::Pending => self.pending += 1,
            TodoStatus::Running => self.running += 1,
            TodoStatus::Completed => self.completed += 1,
            TodoStatus::Failed => self.failed += 1,
            TodoStatus::Skipped => self.skipped += 1,
            TodoStatus::Cancelled => self.cancelled += 1,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TodoTypeCounts {
    pub discovery: usize,
    pub design: usize,
    pub implementation: usize,
    pub verification: usize,
    pub integration: usize,
    pub fix: usize,
    pub refactor: usize,
}

impl TodoTypeCounts {
    pub fn increment(&mut self, todo_type: TodoType) {
        match todo_type {
            TodoType::Discovery => self.discovery += 1,
            TodoType::Design => self.design += 1,
            TodoType::Implementation => self.implementation += 1,
            TodoType::Verification => self.verification += 1,
            TodoType::Integration => self.integration += 1,
            TodoType::Fix => self.fix += 1,
            TodoType::Refactor => self.refactor += 1,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PlanReviewFeedback {
    pub status: ReviewStatus,
    pub todos_to_add: Vec<CreateTodoParams>,
    pub todos_to_remove: Vec<String>,
    pub todos_to_modify: Vec<TodoModification>,
    pub comments: Option<String>,
    pub rejection_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TodoModification {
    pub todo_id: String,
    pub updates: UpdateTodoParams,
}
