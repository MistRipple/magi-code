use crate::{SkillDefinition, SkillPolicyDecision};
use std::collections::HashSet;

pub(crate) fn normalize_skill(mut skill: SkillDefinition) -> SkillDefinition {
    skill.allowed_tools.sort();
    skill.allowed_tools.dedup();
    skill.metadata.tags.sort();
    skill.metadata.tags.dedup();
    skill.custom_tool_bindings.sort_by(|left, right| {
        left.binding_id
            .cmp(&right.binding_id)
            .then_with(|| left.tool_name.cmp(&right.tool_name))
            .then_with(|| left.bridge_target.cmp(&right.bridge_target))
    });
    skill
        .custom_tool_bindings
        .dedup_by(|left, right| left.binding_id == right.binding_id);
    skill
}

pub(crate) fn evaluate_policy(
    selected_skills: &[SkillDefinition],
    requested_tools: &[String],
) -> SkillPolicyDecision {
    let mut allowlist = selected_skills
        .iter()
        .flat_map(|skill| skill.allowed_tools.iter().cloned())
        .collect::<HashSet<_>>();
    let mut allowed_tools = Vec::new();
    let mut denied_tools = Vec::new();

    if requested_tools.is_empty() {
        let mut tools = allowlist.into_iter().collect::<Vec<_>>();
        tools.sort();
        return SkillPolicyDecision {
            allowed_tools: tools,
            denied_tools,
        };
    }

    for tool in requested_tools {
        if allowlist.remove(tool)
            || selected_skills
                .iter()
                .any(|skill| skill.allowed_tools.iter().any(|item| item == tool))
        {
            allowed_tools.push(tool.clone());
        } else {
            denied_tools.push(tool.clone());
        }
    }

    allowed_tools.sort();
    allowed_tools.dedup();
    denied_tools.sort();
    denied_tools.dedup();

    SkillPolicyDecision {
        allowed_tools,
        denied_tools,
    }
}
