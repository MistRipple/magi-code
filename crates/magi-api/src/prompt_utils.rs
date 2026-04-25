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
    content
        .strip_prefix("shadow-model::")
        .unwrap_or(content.as_str())
        .trim()
        .to_string()
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
}
