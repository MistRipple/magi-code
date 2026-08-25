//! 会话级提示词装配的快照测试。
//!
//! `compose_developer_instructions` 决定了 developer 前缀里 user_rules /
//! safeguard 两个文本小节如何稳定拼接；
//! `workspace_context_system_prompt` 决定了工作区上下文的整段措辞。
//! 把这些拼接结果落到 `.snap`，避免分隔符 / 段头 / 文本顺序被无意调整。

use insta::assert_snapshot;
use magi_conversation_runtime::prompt_utils::{
    compose_developer_instructions, workspace_context_system_prompt_for_platform,
};

#[test]
fn snapshot_compose_all_sections() {
    let prompt = compose_developer_instructions(
        Some("执行当前任务"),
        Some("用户偏好简洁回答"),
        Some("禁止执行破坏性命令"),
        None,
    )
    .expect("developer prompt");
    assert_snapshot!(prompt);
}

#[test]
fn snapshot_compose_user_rules_only() {
    let prompt =
        compose_developer_instructions(Some("执行当前任务"), Some("保持中文回复"), None, None)
            .expect("developer prompt");
    assert_snapshot!(prompt);
}

#[test]
fn snapshot_compose_safeguard_only() {
    let prompt = compose_developer_instructions(
        Some("执行当前任务"),
        None,
        Some("禁止读取 .env 文件"),
        None,
    )
    .expect("developer prompt");
    assert_snapshot!(prompt);
}

#[test]
fn snapshot_compose_base_only() {
    let prompt = compose_developer_instructions(Some("执行当前任务"), Some("   "), Some(""), None)
        .expect("base developer prompt");
    assert_snapshot!(prompt);
}

#[test]
fn snapshot_workspace_context_system_prompt() {
    let prompt = workspace_context_system_prompt_for_platform("/workspace/demo", "linux");
    assert_snapshot!(prompt);
}
