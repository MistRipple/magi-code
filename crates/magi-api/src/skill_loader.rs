use crate::settings_store::SettingsStore;
use magi_skill_runtime::{SkillDefinition, SkillMetadata, SkillRegistry};
use std::fs;
use std::path::{Path, PathBuf};

fn read_skill_instruction(dir_path: &Path) -> String {
    for filename in ["prompt.md", "SKILL.md", "README.md"] {
        let path = dir_path.join(filename);
        if path.exists() {
            return fs::read_to_string(path).unwrap_or_default();
        }
    }
    String::new()
}

pub fn load_skills_into_registry(store: &SettingsStore) -> SkillRegistry {
    let registry = SkillRegistry::new();
    let config = store.get_section("skillsConfig");
    if let Some(skills) = config.get("instructionSkills").and_then(|v| v.as_array()) {
        for skill_val in skills {
            if let Some(skill_id) = skill_val.get("skillId").and_then(|v| v.as_str()) {
                if let Some(dir_path) = skill_val.get("directoryPath").and_then(|v| v.as_str()) {
                    let skill_dir = PathBuf::from(dir_path);
                    let instruction = read_skill_instruction(&skill_dir);

                    let mut allowed_tools = Vec::new();
                    let custom_tool_bindings = Vec::new(); // Support custom bindings later if needed

                    let config_path = skill_dir.join("config.json");
                    if let Ok(content) = fs::read_to_string(config_path) {
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                            if let Some(allowed) =
                                parsed.get("allowed_tools").and_then(|v| v.as_array())
                            {
                                for t in allowed {
                                    if let Some(t_str) = t.as_str() {
                                        allowed_tools.push(t_str.to_string());
                                    }
                                }
                            }
                        }
                    }

                    registry.register(SkillDefinition {
                        skill_id: skill_id.to_string(),
                        title: skill_val
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or(skill_id)
                            .to_string(),
                        instruction,
                        metadata: SkillMetadata {
                            category: "local".to_string(),
                            tags: vec![],
                        },
                        allowed_tools,
                        custom_tool_bindings,
                        prompt_priority: 50,
                    });
                }
            }
        }
    }
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_skill_runtime::SkillSelection;

    fn unique_test_dir(name: &str) -> PathBuf {
        let epoch_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        std::env::temp_dir().join(format!("magi-skill-loader-{name}-{epoch_ms}"))
    }

    #[test]
    fn load_skills_into_registry_falls_back_to_skill_markdown() {
        let skill_dir = unique_test_dir("skill-md");
        std::fs::create_dir_all(&skill_dir).expect("temp skill dir should be created");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "# 中文工程规范\n\n请输出 skill-loader-e2e。\n",
        )
        .expect("skill markdown should be written");

        let store = SettingsStore::new();
        store.set_section(
            "skillsConfig",
            serde_json::json!({
                "instructionSkills": [
                    {
                        "skillId": "cn-engineering-standard",
                        "name": "cn-engineering-standard",
                        "directoryPath": skill_dir.to_string_lossy().to_string()
                    }
                ]
            }),
        );

        let registry = load_skills_into_registry(&store);
        let plan = registry.build_tool_runtime_plan(&SkillSelection {
            skill_ids: vec!["cn-engineering-standard".to_string()],
            requested_tools: vec![],
        });

        assert_eq!(plan.prompt_injections.len(), 1);
        assert!(plan.prompt_injections[0].body.contains("skill-loader-e2e"));

        std::fs::remove_dir_all(&skill_dir).expect("temp skill dir should be removed");
    }
}
