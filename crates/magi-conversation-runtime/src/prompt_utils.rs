use magi_bridge_client::assignment_dispatch::{strip_dispatch_preview_text, strip_dispatch_text};

/// 17 段 system prompt 装配中，文本级小节之间的固定分隔串。
///
/// 用于 [`prepend_session_instructions`] 把 user_rules / safeguard / lifecycle
/// 等小节用空行隔开拼到同一条 system message 里。消息级（按 `ChatMessage`
/// 切分）的分段不使用此常量。
pub const SEGMENT_SEP: &str = "\n\n";

/// 段头模板：`--- <title> ---`。统一所有 inline 小节标题，方便检索与调整文案。
pub const SEGMENT_HEADER_USER_RULES: &str = "--- 用户规则 ---";
pub const SEGMENT_HEADER_SAFEGUARD: &str = "--- 安全防护 ---";
pub const SEGMENT_HEADER_LIFECYCLE: &str = "--- 生命周期通知 ---";

pub fn prepend_session_instructions(
    user_rules: Option<&str>,
    safeguard_rules: Option<&str>,
    lifecycle_notice: Option<&str>,
    prompt: &str,
) -> String {
    let mut sections = Vec::new();
    if let Some(rules) = user_rules.map(str::trim).filter(|rules| !rules.is_empty()) {
        sections.push(format!("{SEGMENT_HEADER_USER_RULES}\n{rules}"));
    }
    if let Some(rules) = safeguard_rules
        .map(str::trim)
        .filter(|rules| !rules.is_empty())
    {
        sections.push(format!("{SEGMENT_HEADER_SAFEGUARD}\n{rules}"));
    }
    if let Some(notice) = lifecycle_notice
        .map(str::trim)
        .filter(|notice| !notice.is_empty())
    {
        sections.push(format!("{SEGMENT_HEADER_LIFECYCLE}\n{notice}"));
    }
    if sections.is_empty() {
        return prompt.to_string();
    }
    format!("{}{SEGMENT_SEP}{}", sections.join(SEGMENT_SEP), prompt)
}

/// 工作区上下文 system prompt 模板。运行时只替换 `{{root_path}}`。
const TPL_WORKSPACE_CONTEXT: &str = include_str!("../templates/workspace_context.md");

pub fn workspace_context_system_prompt(root_path: &str) -> String {
    TPL_WORKSPACE_CONTEXT
        .replace("{{root_path}}", root_path)
        .trim_end()
        .to_string()
}

pub fn normalize_model_visible_content(content: String) -> String {
    let content = content
        .strip_prefix("loopback-model::")
        .unwrap_or(content.as_str())
        .trim()
        .to_string();
    strip_dispatch_text(&content).trim().to_string()
}

pub fn normalize_model_stream_preview_content(content: &str) -> String {
    let content = content
        .strip_prefix("loopback-model::")
        .unwrap_or(content)
        .trim();
    let stripped = strip_dispatch_text(content);
    if stripped != content {
        return stripped.trim().to_string();
    }
    strip_dispatch_preview_text(content).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepend_session_instructions_keeps_plain_prompt_when_rules_empty() {
        assert_eq!(
            prepend_session_instructions(Some("  "), None, None, "执行任务"),
            "执行任务"
        );
    }

    #[test]
    fn prepend_session_instructions_adds_user_and_safeguard_rules() {
        let prompt =
            prepend_session_instructions(Some("保持稳定"), Some("禁止危险操作"), None, "执行任务");

        assert!(prompt.contains("--- 用户规则 ---\n保持稳定"));
        assert!(prompt.contains("--- 安全防护 ---\n禁止危险操作"));
        assert!(prompt.ends_with("执行任务"));
    }

    #[test]
    fn prepend_session_instructions_appends_lifecycle_notice_in_order() {
        let prompt = prepend_session_instructions(
            Some("用户规则文本"),
            Some("安全文本"),
            Some("Mission M-1 已恢复"),
            "执行任务",
        );

        let user_pos = prompt.find("--- 用户规则 ---").expect("用户规则段存在");
        let safe_pos = prompt.find("--- 安全防护 ---").expect("安全段存在");
        let life_pos = prompt
            .find("--- 生命周期通知 ---")
            .expect("生命周期通知段存在");
        assert!(user_pos < safe_pos);
        assert!(safe_pos < life_pos);
        assert!(prompt.ends_with("执行任务"));
    }

    #[test]
    fn prepend_session_instructions_ignores_empty_lifecycle_notice() {
        let prompt = prepend_session_instructions(Some("u"), None, Some("   "), "执行任务");
        assert!(!prompt.contains("--- 生命周期通知 ---"));
    }

    #[test]
    fn normalize_model_visible_content_removes_loopback_prefix() {
        assert_eq!(
            normalize_model_visible_content(" loopback-model::结果 \n".trim_start().to_string()),
            "结果"
        );
    }

    #[test]
    fn workspace_context_system_prompt_requires_git_probe_before_status() {
        let prompt = workspace_context_system_prompt("/tmp/workspace");

        assert!(prompt.contains("/tmp/workspace"));
        assert!(prompt.contains("不要假设工作区一定是 Git 仓库"));
        assert!(prompt.contains("rev-parse --is-inside-work-tree"));
        assert!(prompt.contains("NOT_GIT_WORKTREE"));
        assert!(prompt.contains("access_mode=read_only"));
        assert!(prompt.contains("不要继续重复 Git 状态命令"));
    }

    #[test]
    fn normalize_model_visible_content_strips_assignment_dispatch_payload() {
        let content = r#"分析完成。
我将安排以下任务：
```json
{
  "mission_title": "实现用户认证",
  "tasks": [{
    "task_name": "实现 JWT 验证",
    "ownership_hint": "backend",
    "mode_hint": "implement",
    "goal": "实现 JWT token 验证中间件",
    "acceptance": ["通过单元测试"],
    "constraints": ["使用现有模块"],
    "context": ["auth"],
    "requires_modification": true
  }]
}
```"#;
        assert_eq!(
            normalize_model_visible_content(content.to_string()),
            "分析完成。"
        );
    }

    #[test]
    fn normalize_model_stream_preview_content_hides_partial_assignment_dispatch_payload() {
        let content = r#"分析完成。
我将安排以下任务：
```json
{
  "mission_title": "实现用户认证",
  "tasks": [{"#;
        assert_eq!(
            normalize_model_stream_preview_content(content),
            "分析完成。"
        );
    }
}
