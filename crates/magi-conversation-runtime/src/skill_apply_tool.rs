use magi_bridge_client::{ChatToolDefinition, ChatToolFunctionDefinition};
use magi_core::ExecutionResultStatus;
use magi_skill_runtime::SkillRuntime;

pub const SKILL_APPLY_TOOL_NAME: &str = "skill_apply";

const SKILL_APPLY_TOOL_DESCRIPTION: &str =
    "Load and apply a named skill for specialized task execution";

pub fn skill_apply_tool_definition() -> ChatToolDefinition {
    ChatToolDefinition {
        kind: "function".to_string(),
        function: ChatToolFunctionDefinition {
            name: SKILL_APPLY_TOOL_NAME.to_string(),
            description: SKILL_APPLY_TOOL_DESCRIPTION.to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": "Name of the skill to apply"
                    },
                    "context": {
                        "type": "string",
                        "description": "Additional context for the skill execution"
                    }
                },
                "required": ["skill_name"]
            }),
        },
    }
}

pub fn execute_skill_apply_from_runtime(
    arguments: &str,
    skill_runtime: Option<&SkillRuntime>,
) -> (String, ExecutionResultStatus) {
    let Some(skill_runtime) = skill_runtime else {
        return skill_apply_failed("SkillRuntime 未配置，无法应用 skill", None, Vec::new());
    };
    let parsed = match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(value) => value,
        Err(error) => {
            return skill_apply_failed(
                format!("skill_apply 参数不是合法 JSON: {error}"),
                None,
                Vec::new(),
            );
        }
    };
    let skill_name = match parsed
        .get("skill_name")
        .or_else(|| parsed.get("name"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => value,
        None => {
            return skill_apply_failed("缺少 skill_name 字段", None, Vec::new());
        }
    };
    let context = parsed
        .get("context")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);
    let registry = skill_runtime.registry();
    let available_skills = registry
        .list()
        .into_iter()
        .map(|skill| skill.skill_id)
        .collect::<Vec<_>>();
    let Some(skill) = registry.get(skill_name) else {
        return skill_apply_failed(
            format!("未找到已注册 skill: {skill_name}"),
            Some(skill_name),
            available_skills,
        );
    };
    let skill_id = skill.skill_id.clone();
    let title = skill.title.clone();
    let custom_tool_bindings = skill
        .custom_tool_bindings
        .iter()
        .map(|binding| {
            serde_json::json!({
                "binding_id": binding.binding_id,
                "tool_name": binding.tool_name,
                "description": binding.description,
                "bridge_kind": binding.bridge_kind,
                "dispatch_action": binding.dispatch_action,
                "bridge_target": binding.bridge_target,
            })
        })
        .collect::<Vec<_>>();
    (
        serde_json::json!({
            "tool": SKILL_APPLY_TOOL_NAME,
            "status": "succeeded",
            "skill_name": skill.skill_id,
            "title": skill.title,
            "instruction": skill.instruction,
            "allowed_tools": skill.allowed_tools,
            "custom_tool_bindings": custom_tool_bindings,
            "metadata": {
                "category": skill.metadata.category,
                "tags": skill.metadata.tags,
            },
            "context": context,
            "summary": format!("已加载已注册 skill: {skill_id} ({title})")
        })
        .to_string(),
        ExecutionResultStatus::Succeeded,
    )
}

fn skill_apply_failed(
    error: impl Into<String>,
    skill_name: Option<&str>,
    available_skills: Vec<String>,
) -> (String, ExecutionResultStatus) {
    (
        serde_json::json!({
            "tool": SKILL_APPLY_TOOL_NAME,
            "status": "failed",
            "error": error.into(),
            "skill_name": skill_name,
            "available_skills": available_skills,
        })
        .to_string(),
        ExecutionResultStatus::Failed,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_skill_runtime::{SkillDefinition, SkillMetadata, SkillRegistry};

    fn make_skill_runtime() -> SkillRuntime {
        let registry = SkillRegistry::new();
        registry.register(SkillDefinition {
            skill_id: "code-review".to_string(),
            title: "代码审查".to_string(),
            instruction: "从产品稳定性角度检查关键缺陷。".to_string(),
            metadata: SkillMetadata {
                category: "quality".to_string(),
                tags: vec!["review".to_string()],
            },
            allowed_tools: vec!["file_read".to_string()],
            custom_tool_bindings: vec![],
            prompt_priority: 50,
        });
        SkillRuntime::new(registry)
    }

    #[test]
    fn skill_apply_uses_registered_skill_runtime() {
        let runtime = make_skill_runtime();
        let (payload, status) = execute_skill_apply_from_runtime(
            &serde_json::json!({
                "skill_name": "code-review",
                "context": "检查主链路"
            })
            .to_string(),
            Some(&runtime),
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], SKILL_APPLY_TOOL_NAME);
        assert_eq!(parsed["status"], "succeeded");
        assert_eq!(parsed["skill_name"], "code-review");
        assert_eq!(parsed["title"], "代码审查");
        assert_eq!(parsed["context"], "检查主链路");
        assert!(
            parsed["instruction"]
                .as_str()
                .unwrap()
                .contains("产品稳定性")
        );
    }

    #[test]
    fn skill_apply_reports_missing_registered_skill_without_filesystem_scan_fields() {
        let runtime = SkillRuntime::new(SkillRegistry::new());
        let (payload, status) = execute_skill_apply_from_runtime(
            &serde_json::json!({ "skill_name": "auto-review" }).to_string(),
            Some(&runtime),
        );

        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], SKILL_APPLY_TOOL_NAME);
        assert_eq!(parsed["status"], "failed");
        assert_eq!(parsed["skill_name"], "auto-review");
        assert!(parsed["error"].as_str().unwrap().contains("auto-review"));
        assert!(parsed.get("search_paths").is_none());
        assert!(parsed.get("hint").is_none());
    }
}
