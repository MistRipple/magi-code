//! 会话级提示词装配的快照测试。
//!
//! `prepend_session_instructions` 决定了 17 段 system message 里 user_rules /
//! safeguard 两个文本小节如何串在用户原始 prompt 之前；
//! `workspace_context_system_prompt` 决定了工作区上下文的整段措辞。
//! 把这些拼接结果落到 `.snap`，避免分隔符 / 段头 / 文本顺序被无意调整。

use insta::assert_snapshot;
use magi_conversation_runtime::prompt_utils::{
    prepend_session_instructions, workspace_context_system_prompt,
};

#[test]
fn snapshot_prepend_all_sections() {
    let prompt = prepend_session_instructions(
        Some("用户偏好简洁回答"),
        Some("禁止执行破坏性命令"),
        "执行当前任务",
    );
    assert_snapshot!(prompt);
}

#[test]
fn snapshot_prepend_user_rules_only() {
    let prompt = prepend_session_instructions(Some("保持中文回复"), None, "执行当前任务");
    assert_snapshot!(prompt);
}

#[test]
fn snapshot_prepend_safeguard_only() {
    let prompt = prepend_session_instructions(None, Some("禁止读取 .env 文件"), "执行当前任务");
    assert_snapshot!(prompt);
}

#[test]
fn snapshot_prepend_no_sections_returns_prompt_only() {
    // 两个小节全部为空时，输出应与原 prompt 字节一致。
    let prompt = prepend_session_instructions(Some("   "), Some(""), "执行当前任务");
    assert_snapshot!(prompt);
}

#[test]
fn snapshot_workspace_context_system_prompt() {
    let prompt = workspace_context_system_prompt("/workspace/demo");
    assert_snapshot!(prompt);
}
