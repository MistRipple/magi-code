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

pub(crate) fn workspace_context_system_prompt(root_path: &str) -> String {
    format!(
        "当前工作区根目录是 `{root_path}`。当用户、任务或 worker 提到“当前项目”、“当前工程”、“当前仓库”、“本项目”或 current project/repo/codebase 时，默认指这个工作区。需要分析当前项目时，必须优先使用可用工具读取该工作区的目录、README、配置和关键源码，不要要求用户手动粘贴项目结构。工具未显式传 cwd/root/path 的相对路径均应按该工作区根目录理解。不要假设工作区一定是 Git 仓库；执行 `git status`、`git diff` 等 Git 状态命令前，必须先用只读、受保护的条件命令确认 Git worktree，例如 `git -C <root> rev-parse --is-inside-work-tree >/dev/null 2>&1` 只能出现在 if 条件中，非 Git 目录应输出 `NOT_GIT_WORKTREE` 并保持 shell 命令成功。只读 shell 探测必须显式传 `access_mode=read_only`。如果工作区不是 Git 仓库，应明确说明 Git 状态不可用，不要继续重复 Git 状态命令，也不要把 Git 不可用等同于已完成文件变更检测。"
    )
}

pub(crate) fn normalize_model_visible_content(content: String) -> String {
    let content = content
        .strip_prefix("loopback-model::")
        .unwrap_or(content.as_str())
        .trim()
        .to_string();
    strip_dispatch_text(&content).trim().to_string()
}

pub(crate) fn normalize_model_stream_preview_content(content: &str) -> String {
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
