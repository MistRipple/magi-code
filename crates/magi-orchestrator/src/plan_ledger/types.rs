use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanMode {
    Standard,
    Deep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Draft,
    AwaitingConfirmation,
    Approved,
    Rejected,
    Executing,
    PartiallyCompleted,
    Completed,
    Failed,
    Cancelled,
    Superseded,
}

impl PlanStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Rejected | Self::Superseded
        )
    }

    pub fn allowed_transitions(self) -> &'static [PlanStatus] {
        match self {
            Self::Draft => &[
                Self::AwaitingConfirmation, Self::Approved, Self::Executing,
                Self::Rejected, Self::Failed, Self::Cancelled, Self::Superseded, Self::Completed,
            ],
            Self::AwaitingConfirmation => &[
                Self::Approved, Self::Rejected, Self::Executing,
                Self::Failed, Self::Cancelled, Self::Superseded, Self::Completed,
            ],
            Self::Approved => &[
                Self::Executing, Self::Failed, Self::Cancelled, Self::Superseded, Self::Completed,
            ],
            Self::Executing => &[
                Self::PartiallyCompleted, Self::Completed, Self::Failed, Self::Cancelled,
            ],
            Self::PartiallyCompleted => &[
                Self::Executing, Self::Completed, Self::Failed, Self::Cancelled,
            ],
            _ => &[],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanItemStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
    Cancelled,
}

impl PlanItemStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Skipped | Self::Cancelled
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanAttemptScope {
    Orchestrator,
    Assignment,
    Task,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanAttemptStatus {
    Created,
    Inflight,
    Succeeded,
    Failed,
    Timeout,
    Cancelled,
}

impl PlanAttemptStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Timeout | Self::Cancelled
        )
    }

    pub fn allowed_transitions(self) -> &'static [PlanAttemptStatus] {
        match self {
            Self::Created => &[Self::Inflight],
            Self::Inflight => &[Self::Succeeded, Self::Failed, Self::Timeout, Self::Cancelled],
            _ => &[],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanAttemptRecord {
    pub attempt_id: String,
    pub scope: PlanAttemptScope,
    pub target_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub sequence: u32,
    pub status: PlanAttemptStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<u64>,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanReview {
    pub status: PlanReviewStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub reviewed_at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanReviewStatus {
    Approved,
    Rejected,
    Skipped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanAcceptanceSummary {
    Pending,
    Partial,
    Passed,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptanceCriterion {
    pub description: String,
    pub met: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRuntimeAcceptance {
    pub criteria: Vec<AcceptanceCriterion>,
    pub summary: PlanAcceptanceSummary,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRuntimeReviewState {
    pub round: u32,
    pub state: ReviewState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reviewed_at: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Idle,
    Running,
    Accepted,
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRuntimeReplanState {
    pub state: ReplanState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplanState {
    None,
    Required,
    AwaitingConfirmation,
    Applied,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRuntimeWaitState {
    pub state: WaitState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitState {
    None,
    ExternalWaiting,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRuntimePhaseState {
    pub state: PhaseState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_title: Option<String>,
    #[serde(default)]
    pub remaining_phases: Vec<String>,
    pub continuation_intent: ContinuationIntent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseState {
    Idle,
    Running,
    AwaitingNextPhase,
    Completed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationIntent {
    Continue,
    Stop,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRuntimeTerminationState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRuntimeState {
    pub acceptance: PlanRuntimeAcceptance,
    pub review: PlanRuntimeReviewState,
    pub replan: PlanRuntimeReplanState,
    pub wait: PlanRuntimeWaitState,
    pub phase: PlanRuntimePhaseState,
    pub termination: PlanRuntimeTerminationState,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanLinks {
    pub assignment_ids: Vec<String>,
    pub task_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    pub item_id: String,
    pub title: String,
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_hints: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_files: Option<Vec<String>>,
    #[serde(default)]
    pub requires_modification: bool,
    pub status: PlanItemStatus,
    pub progress: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment_id: Option<String>,
    #[serde(default)]
    pub task_ids: Vec<String>,
    #[serde(default)]
    pub task_statuses: HashMap<String, String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRecord {
    pub plan_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<String>,
    pub turn_id: String,
    pub schema_version: u32,
    pub revision: u32,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_plan_id: Option<String>,
    pub mode: PlanMode,
    pub status: PlanStatus,
    pub prompt_digest: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis: Option<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<PlanReview>,
    pub runtime: PlanRuntimeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formatted_plan: Option<String>,
    pub items: Vec<PlanItem>,
    pub attempts: Vec<PlanAttemptRecord>,
    pub links: PlanLinks,
    #[serde(default)]
    pub recovery_protected: bool,
    pub created_at: u64,
    pub updated_at: u64,
}

pub struct CreatePlanDraftInput {
    pub session_id: String,
    pub turn_id: String,
    pub mission_id: Option<String>,
    pub mode: PlanMode,
    pub prompt: String,
    pub summary: Option<String>,
    pub analysis: Option<String>,
    pub acceptance_criteria: Option<Vec<String>>,
    pub constraints: Option<Vec<String>>,
    pub risk_level: Option<String>,
    pub formatted_plan: Option<String>,
}

pub struct DispatchPlanItemInput {
    pub item_id: String,
    pub title: String,
    pub worker: String,
    pub category: Option<String>,
    pub depends_on: Option<Vec<String>>,
    pub scope_hints: Option<Vec<String>>,
    pub target_files: Option<Vec<String>>,
    pub requires_modification: Option<bool>,
}

pub struct PlanAttemptStartInput {
    pub scope: PlanAttemptScope,
    pub target_id: Option<String>,
    pub assignment_id: Option<String>,
    pub task_id: Option<String>,
    pub reason: Option<String>,
}

pub struct PlanAttemptCompleteInput {
    pub scope: PlanAttemptScope,
    pub target_id: Option<String>,
    pub assignment_id: Option<String>,
    pub task_id: Option<String>,
    pub status: PlanAttemptStatus,
    pub error: Option<String>,
    pub evidence_ids: Option<Vec<String>>,
}
