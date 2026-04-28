use magi_bridge_client::assignment_dispatch::{strip_dispatch_preview_text, strip_dispatch_text};

pub(crate) fn prepend_session_instructions(
    user_rules: Option<&str>,
    safeguard_rules: Option<&str>,
    prompt: &str,
) -> String {
    let mut sections = Vec::new();
    if let Some(rules) = user_rules.map(str::trim).filter(|rules| !rules.is_empty()) {
        sections.push(format!("--- 用户规则 ---\n{rules}"));
    }
    if let Some(rules) = safeguard_rules
        .map(str::trim)
        .filter(|rules| !rules.is_empty())
    {
        sections.push(format!("--- 安全防护 ---\n{rules}"));
    }
    if sections.is_empty() {
        return prompt.to_string();
    }
    format!("{}\n\n{}", sections.join("\n\n"), prompt)
}

pub(crate) fn normalize_model_visible_content(content: String) -> String {
    let content = content
        .strip_prefix("shadow-model::")
        .unwrap_or(content.as_str())
        .trim()
        .to_string();
    strip_dispatch_text(&content).trim().to_string()
}

pub(crate) fn normalize_model_stream_preview_content(content: &str) -> String {
    let content = content
        .strip_prefix("shadow-model::")
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
            prepend_session_instructions(Some("  "), None, "执行任务"),
            "执行任务"
        );
    }

    #[test]
    fn prepend_session_instructions_adds_user_and_safeguard_rules() {
        let prompt =
            prepend_session_instructions(Some("保持稳定"), Some("禁止危险操作"), "执行任务");

        assert!(prompt.contains("--- 用户规则 ---\n保持稳定"));
        assert!(prompt.contains("--- 安全防护 ---\n禁止危险操作"));
        assert!(prompt.ends_with("执行任务"));
    }

    #[test]
    fn normalize_model_visible_content_removes_shadow_prefix() {
        assert_eq!(
            normalize_model_visible_content(" shadow-model::结果 \n".trim_start().to_string()),
            "结果"
        );
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
