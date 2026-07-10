pub mod errors;
pub mod execution;
pub mod fs_atomic;
pub mod ids;
pub mod paths;
pub mod public_text;
pub mod status;
pub mod task;
pub mod value_objects;

#[cfg(test)]
mod tests;

pub use errors::{DomainError, DomainResult};
pub use execution::{ExecutionOwnership, RecoveryResumeInput, TaskExecutionTarget};
pub use ids::{
    AssignmentId, EventId, GoalId, LeaseId, MissionId, SessionId, TaskId, ThreadId, ToolCallId,
    WorkerId, WorkspaceId,
};
pub use public_text::{
    PUBLIC_REDACTED_PATH, PUBLIC_REDACTED_VALUE, PUBLIC_RUNTIME_SUMMARY_MAX_CHARS,
    public_runtime_excerpt, public_runtime_summary, public_runtime_text,
};
pub use status::{
    ApprovalRequirement, AssignmentLifecycleStatus, DispatchReason, ExecutionResultStatus,
    MissionLifecycleStatus, RiskLevel, SessionLifecycleStatus, TaskResultKind, TerminationReason,
    VerificationStatus, WorkerLifecycleStatus, WorkspaceLifecycleStatus,
};
pub use task::{
    AccessProfile, AgentRunProjection, ProgressSummary, TASK_RUNTIME_FAILURE_PUBLIC_OUTPUT, Task,
    TaskExecutorBinding, TaskKind, TaskPolicy, TaskRuntimePayload, TaskStatus, TaskTier, TodoItem,
    TodoStatus, public_task_output_refs, task_output_ref_is_internal_runtime_failure,
    text_prohibits_agent_spawn, text_requires_agent_spawn,
};
pub use value_objects::{AbsolutePath, UtcMillis, WorkspaceRootPath, WorktreeRootPath};
