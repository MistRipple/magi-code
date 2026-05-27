use crate::execution_outcome::{
    EXECUTION_OUTCOME_END, EXECUTION_OUTCOME_START, ExecutionOutcomeStatus,
};
use crate::orchestrator_termination::{OrchestratorTerminationReason, TerminationSnapshot};

// ============================================================================
// Prompt templates · 通过 `include_str!` 在编译期内联到二进制，
// 运行时只做 `{{key}}` 字符串替换（与 magi-lifecycle-notice 同一 pattern）。
//
// 为什么不用 format!：模板里大量 JSON / 角括号 / Markdown 字面量包含 `{` `}`，
// 与 format! 的 `{{` `}}` escape 互相干扰；引入 handlebars 对 11 个简单模板
// 是过度工程。这里保持最小依赖。
// ============================================================================

const TPL_CONTINUE_NO_TASKS: &str = include_str!("../templates/round_policy/continue_no_tasks.md");
const TPL_CONTINUE_WITH_PROGRESS: &str =
    include_str!("../templates/round_policy/continue_with_progress.md");
const TPL_OUTCOME_BLOCK_REQUEST: &str =
    include_str!("../templates/round_policy/outcome_block_request.md");
const TPL_NO_TASK_TOOL_LOOP: &str = include_str!("../templates/round_policy/no_task_tool_loop.md");
const TPL_THINKING_ONLY_RECOVERY: &str =
    include_str!("../templates/round_policy/thinking_only_recovery.md");
const TPL_SUMMARY_HIJACK_LVL1: &str =
    include_str!("../templates/round_policy/summary_hijack_lvl1.md");
const TPL_SUMMARY_HIJACK_LVL2: &str =
    include_str!("../templates/round_policy/summary_hijack_lvl2.md");
const TPL_SUMMARY_HIJACK_LVL3PLUS: &str =
    include_str!("../templates/round_policy/summary_hijack_lvl3plus.md");
const TPL_TERMINAL_SYNTHESIS_COMPLETED: &str =
    include_str!("../templates/round_policy/terminal_synthesis_completed.md");
const TPL_TERMINAL_SYNTHESIS_FAILED: &str =
    include_str!("../templates/round_policy/terminal_synthesis_failed.md");

/// `{{key}}` 替换的最小渲染器。模板末尾的换行会被 trim 掉，方便直接拼到上下文。
fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (key, value) in vars {
        let token = format!("{{{{{key}}}}}");
        out = out.replace(&token, value);
    }
    out.trim_end().to_string()
}

pub fn build_continue_prompt(snapshot: &TerminationSnapshot) -> String {
    let p = &snapshot.progress_vector;
    if snapshot.required_total == 0 {
        return render(
            TPL_CONTINUE_NO_TASKS,
            &[
                ("outcome_start", EXECUTION_OUTCOME_START),
                ("outcome_end", EXECUTION_OUTCOME_END),
            ],
        );
    }
    let remain = snapshot
        .required_total
        .saturating_sub(p.terminal_required_tasks);
    render(
        TPL_CONTINUE_WITH_PROGRESS,
        &[
            ("required_total", &snapshot.required_total.to_string()),
            ("terminal_required", &p.terminal_required_tasks.to_string()),
            ("remain", &remain.to_string()),
            ("unresolved_blockers", &p.unresolved_blockers.to_string()),
        ],
    )
}

pub fn build_outcome_block_request_prompt() -> String {
    render(
        TPL_OUTCOME_BLOCK_REQUEST,
        &[
            ("outcome_start", EXECUTION_OUTCOME_START),
            ("outcome_end", EXECUTION_OUTCOME_END),
        ],
    )
}

pub fn build_no_task_tool_loop_prompt(
    no_task_tool_round_streak: u32,
    repeated_signature_streak: u32,
) -> String {
    render(
        TPL_NO_TASK_TOOL_LOOP,
        &[
            (
                "no_task_tool_round_streak",
                &no_task_tool_round_streak.to_string(),
            ),
            (
                "repeated_signature_streak",
                &repeated_signature_streak.to_string(),
            ),
        ],
    )
}

pub fn build_thinking_only_orchestration_recovery_prompt() -> &'static str {
    TPL_THINKING_ONLY_RECOVERY.trim_end()
}

pub struct SummaryHijackCorrection {
    pub prompt: String,
    pub force_no_tools_next_round: bool,
    pub normalized_rounds: u32,
}

pub fn build_summary_hijack_correction(rounds: u32) -> SummaryHijackCorrection {
    if rounds <= 1 {
        return SummaryHijackCorrection {
            prompt: TPL_SUMMARY_HIJACK_LVL1.trim_end().to_string(),
            force_no_tools_next_round: false,
            normalized_rounds: 1,
        };
    }

    if rounds == 2 {
        return SummaryHijackCorrection {
            prompt: TPL_SUMMARY_HIJACK_LVL2.trim_end().to_string(),
            force_no_tools_next_round: true,
            normalized_rounds: 2,
        };
    }

    SummaryHijackCorrection {
        prompt: TPL_SUMMARY_HIJACK_LVL3PLUS.trim_end().to_string(),
        force_no_tools_next_round: true,
        normalized_rounds: 2,
    }
}

#[derive(Clone, Debug)]
pub enum NoTaskPlainResponseDecision {
    TerminateCompleted { next_missing_outcome_streak: u32 },
    TerminateFailed { next_missing_outcome_streak: u32 },
    RequestOutcomeBlock { next_missing_outcome_streak: u32 },
    ContinueWithPrompt { next_missing_outcome_streak: u32 },
}

#[derive(Clone, Debug)]
pub enum PendingTerminalSynthesisDecision {
    Retry { next_retry_count: u32 },
    Finalize,
}

pub fn decide_pending_terminal_synthesis_action(
    assistant_text: &str,
    has_outcome_signal: bool,
    has_dispatch_attempt: bool,
    retry_count: u32,
    max_retry_count: u32,
) -> PendingTerminalSynthesisDecision {
    let missing_terminal_text = assistant_text.trim().is_empty();
    let missing_terminal_outcome = !has_outcome_signal;
    if retry_count < max_retry_count
        && (has_dispatch_attempt || missing_terminal_text || missing_terminal_outcome)
    {
        return PendingTerminalSynthesisDecision::Retry {
            next_retry_count: retry_count + 1,
        };
    }
    PendingTerminalSynthesisDecision::Finalize
}

pub struct NoTaskToolLoopEscalation {
    pub force_no_tools_next_round: bool,
    pub repeated_signature_streak: u32,
    pub last_signature: String,
    pub should_escalate: bool,
}

pub fn evaluate_no_task_tool_loop_escalation(
    round_signature: &str,
    last_signature: &str,
    no_task_tool_round_streak: u32,
    repeated_signature_streak: u32,
    force_no_tools_next_round: bool,
    repeat_threshold: Option<u32>,
    round_threshold: Option<u32>,
) -> NoTaskToolLoopEscalation {
    let repeat_threshold = repeat_threshold.unwrap_or(2);
    let round_threshold = round_threshold.unwrap_or(4);

    let new_repeated = if !round_signature.is_empty() && round_signature == last_signature {
        repeated_signature_streak + 1
    } else {
        1
    };

    let should_escalate = !force_no_tools_next_round
        && (no_task_tool_round_streak >= round_threshold || new_repeated >= repeat_threshold);

    NoTaskToolLoopEscalation {
        force_no_tools_next_round: if should_escalate {
            true
        } else {
            force_no_tools_next_round
        },
        repeated_signature_streak: new_repeated,
        last_signature: round_signature.to_string(),
        should_escalate,
    }
}

pub fn decide_no_task_plain_response_action(
    assistant_text: &str,
    total_tool_result_count: u32,
    explicit_orchestration_request: bool,
    outcome_status: Option<ExecutionOutcomeStatus>,
    normalized_outcome_step_count: u32,
    no_task_outcome_missing_streak: u32,
) -> NoTaskPlainResponseDecision {
    let has_tool_evidence = total_tool_result_count > 0;
    let has_outcome_signal = normalized_outcome_step_count > 0 || outcome_status.is_some();
    let requires_governed_outcome = explicit_orchestration_request || has_tool_evidence;

    if explicit_orchestration_request && !has_tool_evidence {
        let next = no_task_outcome_missing_streak + 1;
        return if next >= 2 {
            NoTaskPlainResponseDecision::TerminateFailed {
                next_missing_outcome_streak: next,
            }
        } else {
            NoTaskPlainResponseDecision::ContinueWithPrompt {
                next_missing_outcome_streak: next,
            }
        };
    }

    if assistant_text.trim().is_empty() {
        return NoTaskPlainResponseDecision::RequestOutcomeBlock {
            next_missing_outcome_streak: no_task_outcome_missing_streak + 1,
        };
    }

    if !requires_governed_outcome && !has_outcome_signal {
        return NoTaskPlainResponseDecision::TerminateCompleted {
            next_missing_outcome_streak: 0,
        };
    }

    if has_outcome_signal {
        if outcome_status == Some(ExecutionOutcomeStatus::Failed) {
            return NoTaskPlainResponseDecision::TerminateFailed {
                next_missing_outcome_streak: 0,
            };
        }
        if outcome_status == Some(ExecutionOutcomeStatus::Running)
            && normalized_outcome_step_count == 0
        {
            let next = no_task_outcome_missing_streak + 1;
            return if next >= 2 {
                NoTaskPlainResponseDecision::TerminateCompleted {
                    next_missing_outcome_streak: next,
                }
            } else {
                NoTaskPlainResponseDecision::RequestOutcomeBlock {
                    next_missing_outcome_streak: next,
                }
            };
        }
        return NoTaskPlainResponseDecision::TerminateCompleted {
            next_missing_outcome_streak: 0,
        };
    }

    let next = no_task_outcome_missing_streak + 1;
    if next >= 2 {
        NoTaskPlainResponseDecision::TerminateCompleted {
            next_missing_outcome_streak: next,
        }
    } else {
        NoTaskPlainResponseDecision::RequestOutcomeBlock {
            next_missing_outcome_streak: next,
        }
    }
}

pub fn should_request_terminal_synthesis_after_tool_round(
    reason: OrchestratorTerminationReason,
    tool_call_count: u32,
) -> bool {
    if tool_call_count == 0 {
        return false;
    }
    reason == OrchestratorTerminationReason::Completed
        || reason == OrchestratorTerminationReason::Failed
}

pub fn build_terminal_synthesis_prompt(
    reason: OrchestratorTerminationReason,
    snapshot: &TerminationSnapshot,
    enforce_outcome_block: bool,
) -> String {
    let remain = snapshot
        .required_total
        .saturating_sub(snapshot.progress_vector.terminal_required_tasks);

    let outcome_contract = format!(
        "输出末尾必须追加控制块：\n{}\n{}\n{}",
        EXECUTION_OUTCOME_START,
        r#"{"status":"running|completed|failed","next_steps":["..."]}"#,
        EXECUTION_OUTCOME_END,
    );

    if reason == OrchestratorTerminationReason::Completed {
        let enforce_line = if enforce_outcome_block {
            "\n- 本轮禁止省略上述控制块；若无法判定，请至少给出 status=completed 和 next_steps=[]。"
        } else {
            ""
        };
        return render(
            TPL_TERMINAL_SYNTHESIS_COMPLETED,
            &[
                ("required_total", &snapshot.required_total.to_string()),
                (
                    "terminal_required",
                    &snapshot.progress_vector.terminal_required_tasks.to_string(),
                ),
                ("remain", &remain.to_string()),
                ("outcome_contract", &outcome_contract),
                ("enforce_line", enforce_line),
            ],
        );
    }

    let enforce_line = if enforce_outcome_block {
        "\n- 本轮禁止省略上述控制块；失败后若仍需继续修复，请使用 status=failed 并写出 next_steps。"
    } else {
        ""
    };
    render(
        TPL_TERMINAL_SYNTHESIS_FAILED,
        &[
            ("required_total", &snapshot.required_total.to_string()),
            (
                "terminal_required",
                &snapshot.progress_vector.terminal_required_tasks.to_string(),
            ),
            ("failed_required", &snapshot.failed_required.to_string()),
            ("outcome_contract", &outcome_contract),
            ("enforce_line", enforce_line),
        ],
    )
}

pub fn build_termination_fallback_text(reason: OrchestratorTerminationReason) -> &'static str {
    match reason {
        OrchestratorTerminationReason::Completed => {
            "任务已满足终止条件，但未收到最终总结文本。请参考上方工具结果。"
        }
        OrchestratorTerminationReason::Failed => {
            "任务进入失败终态，但未收到失败总结文本。请参考上方工具结果与错误信息。"
        }
        _ => "任务已终止。",
    }
}
