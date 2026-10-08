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
    /// 写操作已经发出，但无法确认是否生效（例如执行中途与 Host 断开）。
    /// 它是终态但不是成功；副作用可能已经发生，所以不得自动重试，也不计入重复失败判定。
    Indeterminate,
}

impl ExecutionResultStatus {
    /// 工具结果 payload、事件与日志中使用的状态标签；全仓只此一份映射。
    pub fn wire_label(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Rejected => "rejected",
            Self::NeedsApproval => "needs_approval",
            Self::Cancelled => "cancelled",
            Self::Indeterminate => "indeterminate",
        }
    }

    /// `wire_label` 的逆映射：只认规范标签。别名与近义词不在这里兜底，
    /// 产生 payload 的一方必须直接使用规范标签。
    pub fn from_wire_label(label: &str) -> Option<Self> {
        match label {
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "rejected" => Some(Self::Rejected),
            "needs_approval" => Some(Self::NeedsApproval),
            "cancelled" => Some(Self::Cancelled),
            "indeterminate" => Some(Self::Indeterminate),
            _ => None,
        }
    }
}

#[cfg(test)]
mod execution_result_status_tests {
    use super::ExecutionResultStatus;

    #[test]
    fn wire_labels_round_trip_and_aliases_are_not_accepted() {
        for status in [
            ExecutionResultStatus::Succeeded,
            ExecutionResultStatus::Failed,
            ExecutionResultStatus::Rejected,
            ExecutionResultStatus::NeedsApproval,
            ExecutionResultStatus::Cancelled,
            ExecutionResultStatus::Indeterminate,
        ] {
            assert_eq!(
                ExecutionResultStatus::from_wire_label(status.wire_label()),
                Some(status)
            );
        }
        assert_eq!(ExecutionResultStatus::from_wire_label("ok"), None);
        assert_eq!(ExecutionResultStatus::from_wire_label("Succeeded"), None);
    }
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
