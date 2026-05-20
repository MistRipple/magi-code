//! Round-policy 提示词模板快照测试。
//!
//! 目的：把模板内容固化为 `.snap`，避免文案被无意修改后悄悄通过；
//! 文案确需调整时由 `cargo insta accept` 显式确认，使提示词调优进入 review 流。
//!
//! 输入构造统一从 `ProgressVector::default()` 出发，只设置参与渲染的字段；
//! 其他字段的默认值与最终模板输出无关。

use insta::assert_snapshot;
use magi_bridge_client::orchestrator_termination::{
    OrchestratorTerminationReason, ProgressVector, TerminationSnapshot,
};
use magi_bridge_client::round_policy::{
    build_continue_prompt, build_no_task_tool_loop_prompt, build_outcome_block_request_prompt,
    build_pseudo_tool_call_recovery_prompt, build_summary_hijack_correction,
    build_terminal_synthesis_prompt, build_thinking_only_orchestration_recovery_prompt,
};

fn snapshot_fixture(
    required_total: u32,
    terminal_required: u32,
    unresolved_blockers: u32,
    failed_required: u32,
) -> TerminationSnapshot {
    TerminationSnapshot {
        snapshot_id: "snap-test".to_string(),
        plan_id: "plan-test".to_string(),
        attempt_seq: 1,
        progress_vector: ProgressVector {
            terminal_required_tasks: terminal_required,
            accepted_criteria: 0,
            critical_path_resolved: 0,
            unresolved_blockers,
        },
        review_state: Default::default(),
        blocker_state: Default::default(),
        budget_state: Default::default(),
        cache_state: None,
        cp_version: 0,
        required_total,
        failed_required,
        running_or_pending_required: 0,
        running_required: None,
        source_event_ids: vec![],
        computed_at: 0,
    }
}

#[test]
fn snapshot_continue_prompt_zero_required() {
    let snap = snapshot_fixture(0, 0, 0, 0);
    assert_snapshot!(build_continue_prompt(&snap));
}

#[test]
fn snapshot_continue_prompt_with_progress() {
    // 5 个必做任务，已完成 2 个，剩余 3 个，遗留 1 个阻塞。
    let snap = snapshot_fixture(5, 2, 1, 0);
    assert_snapshot!(build_continue_prompt(&snap));
}

#[test]
fn snapshot_outcome_block_request_prompt() {
    assert_snapshot!(build_outcome_block_request_prompt());
}

#[test]
fn snapshot_no_task_tool_loop_prompt() {
    // 连续 5 轮不用工具，且最近 3 轮签名重复。
    assert_snapshot!(build_no_task_tool_loop_prompt(5, 3));
}

#[test]
fn snapshot_pseudo_tool_call_recovery_prompt() {
    assert_snapshot!(build_pseudo_tool_call_recovery_prompt());
}

#[test]
fn snapshot_thinking_only_recovery_prompt() {
    assert_snapshot!(build_thinking_only_orchestration_recovery_prompt());
}

#[test]
fn snapshot_summary_hijack_correction_level1() {
    let correction = build_summary_hijack_correction(1);
    assert_eq!(correction.normalized_rounds, 1);
    assert!(!correction.force_no_tools_next_round);
    assert_snapshot!(correction.prompt);
}

#[test]
fn snapshot_summary_hijack_correction_level2() {
    let correction = build_summary_hijack_correction(2);
    assert_eq!(correction.normalized_rounds, 2);
    assert!(correction.force_no_tools_next_round);
    assert_snapshot!(correction.prompt);
}

#[test]
fn snapshot_summary_hijack_correction_level3plus() {
    let correction = build_summary_hijack_correction(5);
    assert_eq!(correction.normalized_rounds, 2);
    assert!(correction.force_no_tools_next_round);
    assert_snapshot!(correction.prompt);
}

#[test]
fn snapshot_terminal_synthesis_completed_with_enforce() {
    let snap = snapshot_fixture(4, 4, 0, 0);
    let prompt = build_terminal_synthesis_prompt(
        OrchestratorTerminationReason::Completed,
        &snap,
        true,
    );
    assert_snapshot!(prompt);
}

#[test]
fn snapshot_terminal_synthesis_completed_without_enforce() {
    let snap = snapshot_fixture(4, 4, 0, 0);
    let prompt = build_terminal_synthesis_prompt(
        OrchestratorTerminationReason::Completed,
        &snap,
        false,
    );
    assert_snapshot!(prompt);
}

#[test]
fn snapshot_terminal_synthesis_failed_with_enforce() {
    let snap = snapshot_fixture(4, 3, 1, 2);
    let prompt = build_terminal_synthesis_prompt(
        OrchestratorTerminationReason::Failed,
        &snap,
        true,
    );
    assert_snapshot!(prompt);
}
