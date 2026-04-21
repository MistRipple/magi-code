use crate::settings_store::SettingsStore;
use magi_skill_runtime::{SkillDefinition, SkillMetadata, SkillRegistry};
use std::fs;
use std::path::PathBuf;

pub fn load_skills_into_registry(store: &SettingsStore) -> SkillRegistry {
    let registry = SkillRegistry::new();
    let config = store.get_section("skillsConfig");
    if let Some(skills) = config.get("instructionSkills").and_then(|v| v.as_array()) {
        for skill_val in skills {
            if let Some(skill_id) = skill_val.get("skillId").and_then(|v| v.as_str()) {
                if let Some(dir_path) = skill_val.get("directoryPath").and_then(|v| v.as_str()) {
                    let mut instruction = String::new();
                    let prompt_path = PathBuf::from(dir_path).join("prompt.md");
                    if prompt_path.exists() {
                        instruction = fs::read_to_string(prompt_path).unwrap_or_default();
                    } else {
                        let readme_path = PathBuf::from(dir_path).join("README.md");
                        if readme_path.exists() {
                            instruction = fs::read_to_string(readme_path).unwrap_or_default();
                        }
                    }

                    let mut allowed_tools = Vec::new();
                    let custom_tool_bindings = Vec::new(); // Support custom bindings later if needed

                    let config_path = PathBuf::from(dir_path).join("config.json");
                    if let Ok(content) = fs::read_to_string(config_path) {
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                            if let Some(allowed) = parsed.get("allowed_tools").and_then(|v| v.as_array()) {
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
                        title: skill_val.get("name").and_then(|v| v.as_str()).unwrap_or(skill_id).to_string(),
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
