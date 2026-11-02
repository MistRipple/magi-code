//! 会话停止后的“继续”：唯一的判定。
//!
//! 异常停止、用户主动停止、daemon 中断……对用户都是同一个动作：重新激活并继续推进。
//! 能不能继续、从哪里继续，只由这里决定，并同时服务两处：
//! - 读模型把结果投影给客户端，客户端只据此渲染“继续”，不再自行推断；
//! - 继续动作（会话 Turn 的 `resume`）用同一个判定选择执行路径。
//!
//! 两处共用一个函数，Web 与 daemon 因此不可能对“能不能继续”给出不同答案。

use magi_orchestrator::task_store::TaskStore;
use magi_session_store::MainlineResumeCandidate;

/// 继续的方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContinuationKind {
    /// 沿原执行链从可恢复的检查点接着做。
    ResumeChain,
    /// 主线最近一轮被停止：新的执行继承它已有的消息与工具证据。
    ResumeTurn,
}

impl ContinuationKind {
    /// 读模型里的稳定标识。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ResumeChain => "resume_chain",
            Self::ResumeTurn => "resume_turn",
        }
    }
}

/// 继续的来源任务必须仍存在于任务存储中，新的执行才能继承它的契约和证据。
pub(crate) fn candidate_task_exists(
    task_store: Option<&TaskStore>,
    candidate: &MainlineResumeCandidate,
) -> bool {
    task_store.is_some_and(|store| store.get_task(&candidate.source_task_id).is_some())
}

/// 先看有没有可恢复的执行链，其次看主线最近一轮是否被停止且来源完整；都没有则不可继续。
pub(crate) fn decide_continuation(
    recoverable_branch_count: usize,
    candidate: Option<&MainlineResumeCandidate>,
    task_store: Option<&TaskStore>,
) -> Option<ContinuationKind> {
    if recoverable_branch_count > 0 {
        return Some(ContinuationKind::ResumeChain);
    }
    candidate
        .filter(|candidate| candidate_task_exists(task_store, candidate))
        .map(|_| ContinuationKind::ResumeTurn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{TaskId, ThreadId};

    fn candidate() -> MainlineResumeCandidate {
        MainlineResumeCandidate {
            turn_id: "turn-1".into(),
            original_user_message: "做点事".into(),
            source_task_id: TaskId::new("task-1"),
            source_thread_id: ThreadId::new("thread-1"),
        }
    }

    #[test]
    fn a_recoverable_chain_wins_over_a_stopped_turn() {
        assert_eq!(
            decide_continuation(1, Some(&candidate()), None),
            Some(ContinuationKind::ResumeChain)
        );
        assert_eq!(ContinuationKind::ResumeChain.as_str(), "resume_chain");
    }

    #[test]
    fn nothing_to_continue_is_not_continuable() {
        assert_eq!(decide_continuation(0, None, None), None);
        // 来源任务不在任务存储里：新的执行没法继承它，不能声称可以继续。
        assert_eq!(decide_continuation(0, Some(&candidate()), None), None);
    }
}
