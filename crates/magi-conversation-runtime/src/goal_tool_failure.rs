//! 目标工具（get_goal / create_goal / update_goal）的失败载荷。
//!
//! 约定见 `docs/builtin-tool-failure-contract.md`：稳定的 `error_code`（`{tool}_{类别}`）、
//! 面向模型的类别说明、下一步指引；失败取决于当前目标或计划时，附带它们的当前状态，
//! 让模型据此重新提交（版本号、未完成的计划项），而不是凭过期记忆反复试。

use magi_core::{DomainError, ExecutionResultStatus, GoalRejection, SessionId, ToolFailure};
use magi_session_store::SessionStore;

type ToolOutput = (String, ExecutionResultStatus);

/// 参数缺失或形状不对。
pub(crate) fn invalid_input(tool: &str, error: impl Into<String>, instruction: &str) -> ToolOutput {
    (
        ToolFailure::new(tool, "invalid_input", error)
            .instruction(instruction)
            .into_payload(),
        ExecutionResultStatus::Failed,
    )
}

/// 目标存储拒绝了这次操作。
pub(crate) fn store_failure(
    tool: &str,
    error: DomainError,
    session_store: &SessionStore,
    session_id: &SessionId,
) -> ToolOutput {
    let failure = match &error {
        DomainError::GoalRejected { reason, message } => {
            tracing::warn!(tool, reason = reason.code(), %message, "goal tool rejected by store");
            let (text, instruction) = describe(*reason);
            let mut failure = ToolFailure::new(tool, reason.code(), text).instruction(instruction);
            if depends_on_current_state(*reason) {
                failure = with_current_state(failure, session_store, session_id);
            }
            failure
        }
        DomainError::NotFound { entity } => {
            tracing::warn!(tool, entity, "goal tool target not found");
            with_current_state(
                ToolFailure::new(tool, "not_found", "目标不存在").instruction(
                    "用 get_goal 确认当前目标；goal_id 必须取自 get_goal 或 create_goal 的返回。",
                ),
                session_store,
                session_id,
            )
        }
        DomainError::Validation { message } => {
            tracing::warn!(tool, %message, "goal tool input rejected by store");
            ToolFailure::new(tool, "invalid_input", "参数没有通过校验")
                .instruction("按工具参数要求修正后重新调用。")
        }
        other => {
            tracing::warn!(tool, error = %other, "goal tool failed");
            ToolFailure::new(tool, "failed", "目标操作失败")
                .instruction("原因已记录到日志；先用 get_goal 确认状态，不要用相同参数重复调用。")
        }
    };
    (failure.into_payload(), ExecutionResultStatus::Failed)
}

fn depends_on_current_state(reason: GoalRejection) -> bool {
    !matches!(
        reason,
        GoalRejection::NoOrchestratorThread | GoalRejection::EvidenceRequired
    )
}

/// 附带当前目标与它绑定的计划：版本冲突、计划未完成这类失败，模型需要的就是它们。
fn with_current_state(
    failure: ToolFailure,
    session_store: &SessionStore,
    session_id: &SessionId,
) -> ToolFailure {
    let goal = session_store.current_goal(session_id);
    let plan = goal.as_ref().and_then(|goal| {
        session_store
            .plan(session_id)
            .filter(|plan| plan.goal_id.as_ref() == Some(&goal.goal_id))
    });
    failure
        .with("goal", serde_json::json!(goal))
        .with("plan", serde_json::json!(plan))
}

fn describe(reason: GoalRejection) -> (&'static str, &'static str) {
    match reason {
        GoalRejection::AlreadyUnfinished => (
            "当前会话已有未结束的目标",
            "不能再创建新目标；先看返回的 goal。它完成或标记阻塞之前，不要重复调用 create_goal。",
        ),
        GoalRejection::NotActive => (
            "目标不是进行中状态",
            "已完成、已阻塞或已暂停的目标不能再完成或标记阻塞；以返回的 goal.status 为准，不要重试。",
        ),
        GoalRejection::Terminal => (
            "目标已经结束，不能再修改",
            "以返回的 goal 为准；需要新目标时在当前目标结束后用 create_goal 创建。",
        ),
        GoalRejection::NotOwnedByTurn => (
            "只有目标所属的轮次可以完成或标记阻塞",
            "该目标由其他轮次持有，本轮不要完成它；向用户说明情况，不要重试。",
        ),
        GoalRejection::ControlRevisionConflict => (
            "目标版本已变化",
            "用返回的 goal.control_revision 作为 expected_revision 重新调用，并先确认 goal 的当前状态仍允许这次操作。",
        ),
        GoalRejection::PlanMissing => (
            "目标绑定的计划已不存在",
            "返回的 plan 为 null：expected_plan_revision 传 null 后重新调用。",
        ),
        GoalRejection::PlanRevisionRequired => (
            "目标绑定了计划，必须提供计划版本",
            "把返回的 plan.revision 作为 expected_plan_revision 重新调用。",
        ),
        GoalRejection::PlanRevisionConflict => (
            "计划版本已变化",
            "用返回的 plan.revision 作为 expected_plan_revision 重新调用。",
        ),
        GoalRejection::PlanUnfinished => (
            "计划里还有未完成或阻塞的项",
            "先用 update_plan 把返回的 plan 里所有项标为 completed 或 cancelled（沿用其 planId、revision 与 itemId），计划收尾后再调用 update_goal。",
        ),
        GoalRejection::PlanTasksActive => (
            "计划绑定的任务仍在运行",
            "先用 agent_wait 等这些任务结束，或用 agent_cancel 取消不再需要的任务，再重新调用。",
        ),
        GoalRejection::EvidenceRequired => (
            "完成带计划的目标需要证据引用",
            "在 evidence_refs 里列出可核对的证据（如读回的文件内容、测试输出）后重新调用。",
        ),
        GoalRejection::IllegalTransition => (
            "目标状态机不允许这次转换",
            "用 get_goal 查看当前状态，按允许的方向操作；不要重复同一个调用。",
        ),
        GoalRejection::ConcurrentModification => (
            "目标或计划已被并发修改",
            "用 get_goal 刷新状态后再决定下一步。",
        ),
        GoalRejection::NoOrchestratorThread => (
            "会话还没有可承载目标的主线",
            "不要重试；告知用户该会话暂时不能创建目标。",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn parse(output: &ToolOutput) -> Value {
        serde_json::from_str(&output.0).expect("failure payload is json")
    }

    #[test]
    fn invalid_input_is_a_failed_result_with_code_and_instruction() {
        let output = invalid_input("update_goal", "缺少 goal_id", "先 get_goal");

        assert_eq!(output.1, ExecutionResultStatus::Failed);
        let payload = parse(&output);
        assert_eq!(payload["status"], "failed");
        assert_eq!(payload["error_code"], "update_goal_invalid_input");
        assert_eq!(payload["instruction"], "先 get_goal");
    }

    #[test]
    fn every_rejection_reason_has_a_distinct_code_text_and_instruction() {
        use std::collections::HashSet;
        let reasons = [
            GoalRejection::AlreadyUnfinished,
            GoalRejection::NotActive,
            GoalRejection::Terminal,
            GoalRejection::NotOwnedByTurn,
            GoalRejection::ControlRevisionConflict,
            GoalRejection::PlanMissing,
            GoalRejection::PlanRevisionRequired,
            GoalRejection::PlanRevisionConflict,
            GoalRejection::PlanUnfinished,
            GoalRejection::PlanTasksActive,
            GoalRejection::EvidenceRequired,
            GoalRejection::IllegalTransition,
            GoalRejection::ConcurrentModification,
            GoalRejection::NoOrchestratorThread,
        ];
        let codes: HashSet<_> = reasons.iter().map(|reason| reason.code()).collect();
        assert_eq!(codes.len(), reasons.len());
        for reason in reasons {
            let (text, instruction) = describe(reason);
            assert!(!text.is_empty() && !instruction.is_empty(), "{reason:?}");
            assert!(!instruction.contains("稍后重试"), "{reason:?}");
        }
    }
}
