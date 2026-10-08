use thiserror::Error;

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("实体未找到: {entity}")]
    NotFound { entity: &'static str },
    #[error("实体已存在: {entity}")]
    AlreadyExists { entity: &'static str },
    #[error("非法状态转换: {message}")]
    InvalidState { message: String },
    /// 目标（Goal）操作被当前状态拒绝。显示文本与 `InvalidState` 一致；
    /// `reason` 让工具层按类别给出失败说明，而不必解析文案。
    #[error("非法状态转换: {message}")]
    GoalRejected {
        reason: GoalRejection,
        message: String,
    },
    #[error("会话 {session_id} 已有活动轮次 {active_turn_id}")]
    CurrentTurnConflict {
        session_id: String,
        active_turn_id: String,
    },
    #[error("校验失败: {message}")]
    Validation { message: String },
    #[error("持久化失败: {message}")]
    Persistence { message: String },
}

/// 目标操作被拒绝的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalRejection {
    /// 目标已经存在未结束的实例，不能再创建。
    AlreadyUnfinished,
    /// 目标不是进行中状态，不能完成或观察阻塞。
    NotActive,
    /// 目标已结束，不能再修改。
    Terminal,
    /// 只有目标所属的轮次可以完成或标记阻塞。
    NotOwnedByTurn,
    /// 传入的目标版本与当前不一致。
    ControlRevisionConflict,
    /// 目标绑定的计划已不存在。
    PlanMissing,
    /// 目标绑定了计划，但没有传计划版本。
    PlanRevisionRequired,
    /// 传入的计划版本与当前不一致。
    PlanRevisionConflict,
    /// 计划里还有未完成或阻塞的项。
    PlanUnfinished,
    /// 计划绑定的任务仍在运行。
    PlanTasksActive,
    /// 完成带计划的目标需要证据引用。
    EvidenceRequired,
    /// 目标状态机不允许这次转换。
    IllegalTransition,
    /// 恢复回滚时发现目标或计划已被并发修改。
    ConcurrentModification,
    /// 会话还没有可承载目标的 orchestrator 线程。
    NoOrchestratorThread,
}

impl GoalRejection {
    /// 稳定的类别名，工具层拼成 `{tool}_{code}` 形式的错误码。
    pub fn code(self) -> &'static str {
        match self {
            Self::AlreadyUnfinished => "already_unfinished",
            Self::NotActive => "not_active",
            Self::Terminal => "terminal",
            Self::NotOwnedByTurn => "not_owned_by_turn",
            Self::ControlRevisionConflict => "revision_conflict",
            Self::PlanMissing => "plan_missing",
            Self::PlanRevisionRequired => "plan_revision_required",
            Self::PlanRevisionConflict => "plan_revision_conflict",
            Self::PlanUnfinished => "plan_unfinished",
            Self::PlanTasksActive => "plan_tasks_active",
            Self::EvidenceRequired => "evidence_required",
            Self::IllegalTransition => "illegal_transition",
            Self::ConcurrentModification => "concurrent_modification",
            Self::NoOrchestratorThread => "no_orchestrator_thread",
        }
    }
}

pub type DomainResult<T> = Result<T, DomainError>;
