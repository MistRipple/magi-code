use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionLifecycleStatus {
    Active,
    Archived,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceLifecycleStatus {
    Registered,
    Active,
    Released,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MissionLifecycleStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// Mission 派生生命周期阶段（区别于 [`MissionLifecycleStatus`] 这个最终结果态，
/// `Phase` 描述 mission 当下正在“干嘛”——charter 起草、人审挂起、执行中、收尾）。
///
/// 派生量，**不存盘**；由 `magi_mission::MissionAggregate::lifecycle_phase` 计算，
/// 由 `magi_mission_metrics` 持久化最近一次观测值。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionLifecyclePhase {
    /// charter 仍处于 Draft，mission 尚未真正进入执行阶段。
    CharterDraft,
    /// 存在 Pending 的人审节点，需要人工决策才能继续。
    AwaitingHumanCheckpoint,
    /// charter 已冻结、无人审挂起，但执行尚未开始（plan 全 Pending 或空）。
    PlanReady,
    /// 至少有一个 plan step 处于 InProgress 或已 Completed，但整体未完成。
    Executing,
    /// 全部 plan step 均为 Completed/Cancelled，mission 流程性收尾。
    AllStepsCompleted,
}

impl MissionLifecyclePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CharterDraft => "charter_draft",
            Self::AwaitingHumanCheckpoint => "awaiting_human_checkpoint",
            Self::PlanReady => "plan_ready",
            Self::Executing => "executing",
            Self::AllStepsCompleted => "all_steps_completed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssignmentLifecycleStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerLifecycleStatus {
    Idle,
    Running,
    Reviewing,
    Verifying,
    Repairing,
    Finished,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalRequirement {
    None,
    Required,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionResultStatus {
    Succeeded,
    Failed,
    Rejected,
    NeedsApproval,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispatchReason {
    InitialDispatch,
    RetryAfterFailure,
    RepairFollowUp,
    ManualResume,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminationReason {
    Completed,
    Failed,
    Cancelled,
    Blocked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskResultKind {
    Success,
    Failure,
    Partial,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationStatus {
    Pending,
    Passed,
    Failed,
}
