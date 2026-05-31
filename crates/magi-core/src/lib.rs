pub mod errors;
pub mod execution;
pub mod ids;
pub mod paths;
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
    MissionLifecyclePhase, MissionLifecycleStatus, RiskLevel, SessionLifecycleStatus,
    TaskResultKind, TerminationReason, VerificationStatus, WorkerLifecycleStatus,
    WorkspaceLifecycleStatus,
};
pub use task::{
    AccessProfile, ProgressSummary, TASK_RUNTIME_FAILURE_PUBLIC_OUTPUT, Task, TaskKind, TaskPolicy,
    TaskProjection, TaskRuntimePayload, TaskStatus, TaskTier, public_task_output_refs,
    task_output_ref_is_internal_runtime_failure,
};
pub use value_objects::{AbsolutePath, UtcMillis, WorkspaceRootPath, WorktreeRootPath};
