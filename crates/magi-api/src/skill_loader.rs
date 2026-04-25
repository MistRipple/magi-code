use crate::settings_store::SettingsStore;
use magi_skill_runtime::{SkillDefinition, SkillMetadata, SkillRegistry, SkillRuntime};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const SKILLS_CONFIG_SECTION: &str = "skillsConfig";
const TOP_LEVEL_CUSTOM_TOOLS_SECTION: &str = "customTools";
const TOP_LEVEL_INSTRUCTION_SKILLS_SECTION: &str = "skills";

fn read_skill_instruction(dir_path: &Path) -> String {
    for filename in ["prompt.md", "SKILL.md", "README.md"] {
        let path = dir_path.join(filename);
        if path.exists() {
            return fs::read_to_string(path).unwrap_or_default();
        }
    }
    String::new()
}

fn normalize_wrapped_section_value(value: &mut Value) {
    let Some(object) = value.as_object() else {
        return;
    };
    let nested = object.get("config").or_else(|| object.get("data")).cloned();
    if let Some(nested) = nested {
        *value = nested;
    }
}

fn normalize_skills_config_value(value: Value) -> Map<String, Value> {
    let mut value = value;
    normalize_wrapped_section_value(&mut value);
    value.as_object().cloned().unwrap_or_default()
}

fn ensure_array_entry_mut<'a>(map: &'a mut Map<String, Value>, key: &str) -> &'a mut Vec<Value> {
    let value = map
        .entry(key.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !value.is_array() {
        *value = Value::Array(Vec::new());
    }
    value.as_array_mut().expect("array value just inserted")
}

fn migrate_top_level_custom_tools_into_skills_config(
    config: &mut Map<String, Value>,
    top_level_custom_tools: Option<Value>,
) {
    let top_level_custom_tools = top_level_custom_tools
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    if top_level_custom_tools.is_empty() {
        return;
    }
    let custom_tools_array = ensure_array_entry_mut(config, TOP_LEVEL_CUSTOM_TOOLS_SECTION);
    for entry in top_level_custom_tools {
        let Some(mut object) = entry.as_object().cloned() else {
            continue;
        };
        let tool_name = object
            .get("name")
            .and_then(|value| value.as_str())
            .or_else(|| object.get("toolName").and_then(|value| value.as_str()))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_default()
            .to_string();
        if tool_name.is_empty() {
            continue;
        }
        object.insert("name".to_string(), serde_json::json!(tool_name));
        object.insert("toolName".to_string(), serde_json::json!(tool_name));
        if let Some(position) = custom_tools_array.iter().position(|item| {
            ["toolName", "name"].iter().any(|field| {
                item.get(*field)
                    .and_then(|value| value.as_str())
                    .is_some_and(|value| value == tool_name)
            })
        }) {
            custom_tools_array[position] = Value::Object(object);
        } else {
            custom_tools_array.push(Value::Object(object));
        }
    }
}

fn migrate_top_level_instruction_skills_into_skills_config(
    config: &mut Map<String, Value>,
    top_level_skills: Option<Value>,
) {
    let top_level_skills = top_level_skills
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    if top_level_skills.is_empty() {
        return;
    }
    let instruction_skills_array = ensure_array_entry_mut(config, "instructionSkills");
    for entry in top_level_skills {
        let Some(mut object) = entry.as_object().cloned() else {
            continue;
        };
        let skill_name = object
            .get("name")
            .and_then(|value| value.as_str())
            .or_else(|| object.get("skillName").and_then(|value| value.as_str()))
            .or_else(|| object.get("skillId").and_then(|value| value.as_str()))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_default()
            .to_string();
        if skill_name.is_empty() {
            continue;
        }
        object.insert("name".to_string(), serde_json::json!(skill_name));
        object.insert("skillName".to_string(), serde_json::json!(skill_name));
        object.insert("skillId".to_string(), serde_json::json!(skill_name));
        if object
            .get("fullName")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
        {
            object.insert("fullName".to_string(), serde_json::json!(skill_name));
        }
        if let Some(position) = instruction_skills_array.iter().position(|item| {
            ["skillId", "skillName", "name"].iter().any(|field| {
                item.get(*field)
                    .and_then(|value| value.as_str())
                    .is_some_and(|value| value == skill_name)
            })
        }) {
            instruction_skills_array[position] = Value::Object(object);
        } else {
            instruction_skills_array.push(Value::Object(object));
        }
    }
}

fn canonical_skills_config_from_snapshot(
    snapshot: &mut HashMap<String, Value>,
) -> Map<String, Value> {
    let mut config = snapshot
        .remove(SKILLS_CONFIG_SECTION)
        .map(normalize_skills_config_value)
        .unwrap_or_default();
    migrate_top_level_custom_tools_into_skills_config(
        &mut config,
        snapshot.remove(TOP_LEVEL_CUSTOM_TOOLS_SECTION),
    );
    migrate_top_level_instruction_skills_into_skills_config(
        &mut config,
        snapshot.remove(TOP_LEVEL_INSTRUCTION_SKILLS_SECTION),
    );
    config
}

pub fn normalize_skills_config_sections(snapshot: &mut HashMap<String, Value>) {
    let config = canonical_skills_config_from_snapshot(snapshot);
    snapshot.insert(SKILLS_CONFIG_SECTION.to_string(), Value::Object(config));
}

pub fn skills_config_object(store: &SettingsStore) -> Map<String, Value> {
    let mut snapshot = store.public_snapshot();
    normalize_skills_config_sections(&mut snapshot);
    snapshot
        .remove(SKILLS_CONFIG_SECTION)
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

pub fn save_skills_config_object(store: &SettingsStore, config: Map<String, Value>) {
    let mut snapshot = HashMap::from([(SKILLS_CONFIG_SECTION.to_string(), Value::Object(config))]);
    normalize_skills_config_sections(&mut snapshot);
    let canonical = snapshot
        .remove(SKILLS_CONFIG_SECTION)
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    store.set_section(SKILLS_CONFIG_SECTION, Value::Object(canonical));
    store.remove_section(TOP_LEVEL_INSTRUCTION_SKILLS_SECTION);
    store.remove_section(TOP_LEVEL_CUSTOM_TOOLS_SECTION);
}

fn canonicalize_skills_config_store(store: &SettingsStore) {
    let canonical = skills_config_object(store);
    let current = store.get_section(SKILLS_CONFIG_SECTION);
    let needs_update = current.as_object() != Some(&canonical)
        || store.get(TOP_LEVEL_INSTRUCTION_SKILLS_SECTION).is_some()
        || store.get(TOP_LEVEL_CUSTOM_TOOLS_SECTION).is_some();
    if needs_update {
        save_skills_config_object(store, canonical);
    }
}

fn build_skill_registry_from_config(config: &Map<String, Value>) -> SkillRegistry {
    let registry = SkillRegistry::new();
    if let Some(skills) = config
        .get("instructionSkills")
        .and_then(|value| value.as_array())
    {
        for skill_val in skills {
            if let Some(skill_id) = skill_val.get("skillId").and_then(|v| v.as_str()) {
                if let Some(dir_path) = skill_val.get("directoryPath").and_then(|v| v.as_str()) {
                    let skill_dir = PathBuf::from(dir_path);
                    let instruction = read_skill_instruction(&skill_dir);

                    let mut allowed_tools = Vec::new();
                    let custom_tool_bindings = Vec::new();

                    let config_path = skill_dir.join("config.json");
                    if let Ok(content) = fs::read_to_string(config_path) {
                        if let Ok(parsed) = serde_json::from_str::<Value>(&content) {
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

pub fn load_skills_into_registry(store: &SettingsStore) -> SkillRegistry {
    let config = skills_config_object(store);
    build_skill_registry_from_config(&config)
}

pub fn build_skill_runtime_from_settings(store: &SettingsStore) -> SkillRuntime {
    canonicalize_skills_config_store(store);
    SkillRuntime::new(load_skills_into_registry(store))
}

pub fn reload_skill_runtime_from_settings(skill_runtime: &SkillRuntime, store: &SettingsStore) {
    canonicalize_skills_config_store(store);
    let loaded = load_skills_into_registry(store);
    let registry = skill_runtime.registry();
    registry.clear();
    for skill in loaded.list() {
        registry.register(skill);
    }
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

    fn make_local_skill_dir(name: &str, skill_body: &str) -> PathBuf {
        let skill_dir = unique_test_dir(name);
        std::fs::create_dir_all(&skill_dir).expect("temp skill dir should be created");
        std::fs::write(skill_dir.join("SKILL.md"), skill_body)
            .expect("skill markdown should be written");
        skill_dir
    }

    #[test]
    fn normalize_skills_config_sections_merges_legacy_sections_into_canonical_config() {
        let skill_dir =
            make_local_skill_dir("legacy-merge", "# 合并测试\n\n请输出 legacy-skill。\n");
        let mut snapshot = HashMap::from([
            (
                "skills".to_string(),
                serde_json::json!([
                    {
                        "skillId": "legacy-skill",
                        "name": "legacy-skill",
                        "directoryPath": skill_dir.to_string_lossy().to_string()
                    }
                ]),
            ),
            (
                "customTools".to_string(),
                serde_json::json!([
                    {
                        "name": "legacy-tool",
                        "bindingId": "legacy-tool"
                    }
                ]),
            ),
        ]);

        normalize_skills_config_sections(&mut snapshot);

        assert!(snapshot.get("skills").is_none());
        assert!(snapshot.get("customTools").is_none());
        assert_eq!(
            snapshot["skillsConfig"]["instructionSkills"][0]["skillId"],
            serde_json::json!("legacy-skill")
        );
        assert_eq!(
            snapshot["skillsConfig"]["customTools"][0]["name"],
            serde_json::json!("legacy-tool")
        );

        std::fs::remove_dir_all(&skill_dir).expect("temp skill dir should be removed");
    }

    #[test]
    fn load_skills_into_registry_falls_back_to_skill_markdown() {
        let skill_dir =
            make_local_skill_dir("skill-md", "# 中文工程规范\n\n请输出 skill-loader-e2e。\n");

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

    #[test]
    fn build_skill_runtime_from_settings_canonicalizes_legacy_sections() {
        let skill_dir = make_local_skill_dir(
            "runtime-build",
            "# Runtime build\n\n请输出 runtime-build。\n",
        );
        let store = SettingsStore::new();
        store.set_section(
            "skills",
            serde_json::json!([
                {
                    "skillId": "runtime-skill",
                    "name": "runtime-skill",
                    "directoryPath": skill_dir.to_string_lossy().to_string()
                }
            ]),
        );

        let runtime = build_skill_runtime_from_settings(&store);
        let registry = runtime.registry();
        assert!(registry.get("runtime-skill").is_some());
        assert!(store.get("skills").is_none());
        assert!(store.get("skillsConfig").is_some());

        std::fs::remove_dir_all(&skill_dir).expect("temp skill dir should be removed");
    }

    #[test]
    fn reload_skill_runtime_from_settings_replaces_existing_registry() {
        let first_dir = make_local_skill_dir("reload-first", "# First\n\n请输出 first-skill。\n");
        let second_dir =
            make_local_skill_dir("reload-second", "# Second\n\n请输出 second-skill。\n");

        let store = SettingsStore::new();
        store.set_section(
            "skillsConfig",
            serde_json::json!({
                "instructionSkills": [
                    {
                        "skillId": "first-skill",
                        "name": "first-skill",
                        "directoryPath": first_dir.to_string_lossy().to_string()
                    }
                ]
            }),
        );

        let runtime = build_skill_runtime_from_settings(&store);
        assert!(runtime.registry().get("first-skill").is_some());

        store.set_section(
            "skillsConfig",
            serde_json::json!({
                "instructionSkills": [
                    {
                        "skillId": "second-skill",
                        "name": "second-skill",
                        "directoryPath": second_dir.to_string_lossy().to_string()
                    }
                ]
            }),
        );

        reload_skill_runtime_from_settings(&runtime, &store);
        let registry = runtime.registry();
        assert!(registry.get("first-skill").is_none());
        assert!(registry.get("second-skill").is_some());

        std::fs::remove_dir_all(&first_dir).expect("temp skill dir should be removed");
        std::fs::remove_dir_all(&second_dir).expect("temp skill dir should be removed");
    }

    #[test]
    fn save_skills_config_object_canonicalizes_wrapped_input_and_removes_legacy_sections() {
        let store = SettingsStore::new();
        store.set_section("skills", serde_json::json!([{ "skillId": "legacy-skill" }]));
        store.set_section(
            "customTools",
            serde_json::json!([{ "name": "legacy-tool" }]),
        );

        save_skills_config_object(
            &store,
            serde_json::json!({
                "config": {
                    "instructionSkills": [
                        {
                            "skillId": "saved-skill",
                            "name": "saved-skill"
                        }
                    ]
                }
            })
            .as_object()
            .cloned()
            .expect("wrapped config should be an object"),
        );

        assert!(store.get("skills").is_none());
        assert!(store.get("customTools").is_none());
        assert_eq!(
            store.get_section("skillsConfig")["instructionSkills"][0]["skillId"],
            serde_json::json!("saved-skill")
        );
    }
}
