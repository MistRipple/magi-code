pub mod execution;
pub mod errors;
pub mod ids;
pub mod status;
pub mod task;
pub mod value_objects;

#[cfg(test)]
mod tests;

pub use execution::{
    ExecutionOwnership, RecoveryResumeInput, TaskExecutionTarget,
};
pub use errors::{DomainError, DomainResult};
pub use ids::{
    AssignmentId, EventId, LeaseId, MissionId, SessionId, TaskId, ToolCallId, WorkerId, WorkspaceId,
};
pub use status::{
    ApprovalRequirement, AssignmentLifecycleStatus, ExecutionResultStatus,
    DispatchReason, MissionLifecycleStatus, RiskLevel, SessionLifecycleStatus,
    TaskResultKind, TerminationReason, VerificationStatus,
    WorkerLifecycleStatus, WorkspaceLifecycleStatus,
};
pub use task::{
    AssignmentLease, DecisionOption, DecisionTaskPayload, ExecutorBinding, LeaseStatus,
    ProgressSummary, Task, TaskKind, TaskPolicy, TaskProjection, TaskStatus,
    WorkPackageSummary,
};
pub use value_objects::{AbsolutePath, UtcMillis, WorkspaceRootPath, WorktreeRootPath};
