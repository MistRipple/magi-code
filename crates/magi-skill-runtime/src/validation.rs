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
    let restricted = selected_skills
        .iter()
        .filter(|skill| skill.standard_tool_access().is_restricted())
        .collect::<Vec<_>>();
    let candidates = if requested_tools.is_empty() {
        selected_skills
            .iter()
            .flat_map(|skill| skill.allowed_tools.iter().cloned())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    } else {
        requested_tools.to_vec()
    };
    let (mut allowed_tools, mut denied_tools): (Vec<_>, Vec<_>) =
        candidates.into_iter().partition(|tool| {
            restricted
                .iter()
                .all(|skill| skill.standard_tool_access().allows(tool))
        });
    allowed_tools.sort();
    allowed_tools.dedup();
    denied_tools.sort();
    denied_tools.dedup();
    SkillPolicyDecision {
        allowed_tools,
        denied_tools,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn skill(restrict: bool, allowed: &[&str]) -> SkillDefinition {
        SkillDefinition {
            skill_id: format!("skill-{restrict}-{}", allowed.join("-")),
            title: "test".into(),
            instruction: String::new(),
            metadata: crate::SkillMetadata {
                category: "test".into(),
                tags: vec![],
            },
            restrict_standard_tools: restrict,
            allowed_tools: allowed.iter().map(|s| (*s).into()).collect(),
            custom_tool_bindings: vec![],
            prompt_priority: 0,
        }
    }
    #[test]
    fn skill_recommendations_cannot_expand_a_closed_workflow() {
        let skills = vec![
            skill(true, &["file_read", "search_text"]),
            skill(true, &["file_read"]),
            skill(false, &["shell_exec"]),
        ];
        let policy = evaluate_policy(&skills, &[]);
        assert_eq!(policy.allowed_tools, ["file_read"]);
        assert!(policy.denied_tools.contains(&"shell_exec".into()));
        assert!(!skills[0].standard_tool_access().allows("shell_exec"));
    }
    #[test]
    fn empty_inherit_and_empty_deny_all_remain_distinct() {
        let requested = vec!["file_read".into()];
        assert_eq!(
            evaluate_policy(&[skill(false, &[])], &requested).allowed_tools,
            requested
        );
        assert!(
            evaluate_policy(&[skill(true, &[])], &requested)
                .allowed_tools
                .is_empty()
        );
    }
}
