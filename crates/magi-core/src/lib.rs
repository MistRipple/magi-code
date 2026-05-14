pub mod errors;
pub mod execution;
pub mod ids;
pub mod status;
pub mod task;
pub mod value_objects;

#[cfg(test)]
mod tests;

pub use errors::{DomainError, DomainResult};
pub use execution::{ExecutionOwnership, RecoveryResumeInput, TaskExecutionTarget};
pub use ids::{
    AssignmentId, EventId, LeaseId, MissionId, SessionId, TaskId, ThreadId, ToolCallId, WorkerId,
    WorkspaceId,
};
pub use status::{
    ApprovalRequirement, AssignmentLifecycleStatus, DispatchReason, ExecutionResultStatus,
    MissionLifecycleStatus, RiskLevel, SessionLifecycleStatus, TaskResultKind, TerminationReason,
    VerificationStatus, WorkerLifecycleStatus, WorkspaceLifecycleStatus,
};
pub use task::{
    AssignmentLease, DecisionOption, DecisionTaskPayload, ExecutorBinding, LeaseStatus,
    PolicyDispatchDecision, ProgressSummary, Task, TaskKind, TaskPolicy, TaskProjection,
    TaskStatus, TaskVariant, WorkPackageSummary,
};
pub use value_objects::{AbsolutePath, UtcMillis, WorkspaceRootPath, WorktreeRootPath};
