use crate::scope_binding::{strip_scope_binding_fields, strip_scope_binding_fields_from_map};
use magi_bridge_client::{BridgeBindingKind, BridgeDispatchAction};
use magi_settings_store::SettingsStore;
use magi_skill_runtime::{
    CustomToolBinding, SkillDefinition, SkillMetadata, SkillRegistry, SkillRuntime,
};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const SKILLS_CONFIG_SECTION: &str = "skillsConfig";
const TOP_LEVEL_CUSTOM_TOOLS_SECTION: &str = "customTools";
const TOP_LEVEL_INSTRUCTION_SKILLS_SECTION: &str = "skills";

pub fn read_skill_instruction(dir_path: &Path) -> String {
    read_available_skill_instruction(dir_path).unwrap_or_default()
}

pub fn read_available_skill_instruction(dir_path: &Path) -> Option<String> {
    for filename in ["prompt.md", "SKILL.md", "README.md"] {
        let path = dir_path.join(filename);
        if path.is_file() {
            let instruction = fs::read_to_string(path).ok()?;
            if instruction.trim().is_empty() {
                return None;
            }
            return Some(instruction);
        }
    }
    None
}

pub fn instruction_skill_source_available(skill: &Value) -> bool {
    let Some(dir_path) = skill
        .get("directoryPath")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    read_available_skill_instruction(Path::new(dir_path)).is_some()
}

fn normalize_skills_config_value(value: Value) -> Map<String, Value> {
    let mut value = value;
    strip_scope_binding_fields(&mut value);
    let mut config = value.as_object().cloned().unwrap_or_default();
    normalize_skills_config_entries(&mut config);
    config
}

fn normalize_skills_config_entries(config: &mut Map<String, Value>) {
    strip_scope_binding_fields_from_map(config);
    if let Some(entries) = config
        .get_mut("instructionSkills")
        .and_then(Value::as_array_mut)
    {
        normalize_instruction_skill_entries(entries);
    }
    if let Some(entries) = config.get_mut("customTools").and_then(Value::as_array_mut) {
        normalize_custom_tool_entries(entries);
    }
}

fn normalize_instruction_skill_entries(entries: &mut Vec<Value>) {
    entries.retain_mut(|entry| {
        strip_scope_binding_fields(entry);
        let Some(object) = entry.as_object_mut() else {
            return false;
        };
        object.remove("skillName");
        let Some(skill_id) = object
            .get("skillId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
        else {
            return false;
        };
        if object
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
        {
            object.insert("name".to_string(), Value::String(skill_id));
        }
        true
    });
}

fn normalize_custom_tool_entries(entries: &mut Vec<Value>) {
    entries.retain_mut(|entry| {
        strip_scope_binding_fields(entry);
        let Some(object) = entry.as_object_mut() else {
            return false;
        };
        object.remove("toolName");
        object
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .is_some_and(|name| !name.is_empty())
    });
}

fn normalize_token(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !matches!(ch, '_' | '-' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

fn read_string_field<'a>(object: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn parse_bridge_kind(value: &str) -> Option<BridgeBindingKind> {
    match normalize_token(value).as_str() {
        "model" => Some(BridgeBindingKind::Model),
        "mcp" => Some(BridgeBindingKind::Mcp),
        _ => None,
    }
}

fn parse_dispatch_action(value: &str) -> Option<BridgeDispatchAction> {
    match normalize_token(value).as_str() {
        "modelprompt" | "prompt" => Some(BridgeDispatchAction::ModelPrompt),
        "mcptoolcall" | "toolcall" | "call" => Some(BridgeDispatchAction::McpToolCall),
        _ => None,
    }
}

fn parse_custom_tool_bindings(value: &Value) -> Vec<CustomToolBinding> {
    let Some(entries) = value.as_array() else {
        return Vec::new();
    };

    entries
        .iter()
        .filter_map(|entry| {
            let object = entry.as_object()?;
            let tool_name = read_string_field(object, &["tool_name", "toolName", "name"])?;
            let bridge_target = read_string_field(
                object,
                &["bridge_target", "bridgeTarget", "target", "serverId"],
            )?
            .to_string();
            let binding_id = read_string_field(object, &["binding_id", "bindingId"])
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("{tool_name}:{bridge_target}"));
            let description = read_string_field(object, &["description"])
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| tool_name.to_string());
            let bridge_kind = object
                .get("bridge_kind")
                .or_else(|| object.get("bridgeKind"))
                .and_then(Value::as_str)
                .and_then(parse_bridge_kind)
                .unwrap_or(BridgeBindingKind::Mcp);
            let dispatch_action = object
                .get("dispatch_action")
                .or_else(|| object.get("dispatchAction"))
                .and_then(Value::as_str)
                .and_then(parse_dispatch_action)
                .unwrap_or(match bridge_kind {
                    BridgeBindingKind::Model => BridgeDispatchAction::ModelPrompt,
                    BridgeBindingKind::Mcp => BridgeDispatchAction::McpToolCall,
                });

            Some(CustomToolBinding {
                binding_id,
                tool_name: tool_name.to_string(),
                description,
                bridge_kind,
                dispatch_action,
                bridge_target,
            })
        })
        .collect()
}

fn canonical_skills_config_from_snapshot(
    snapshot: &mut HashMap<String, Value>,
) -> Map<String, Value> {
    let mut config = snapshot
        .remove(SKILLS_CONFIG_SECTION)
        .map(normalize_skills_config_value)
        .unwrap_or_default();
    snapshot.remove(TOP_LEVEL_CUSTOM_TOOLS_SECTION);
    snapshot.remove(TOP_LEVEL_INSTRUCTION_SKILLS_SECTION);
    normalize_skills_config_entries(&mut config);
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
            if let Some(skill_id) = skill_val.get("skillId").and_then(|v| v.as_str())
                && let Some(dir_path) = skill_val.get("directoryPath").and_then(|v| v.as_str())
            {
                let skill_dir = PathBuf::from(dir_path);
                let Some(instruction) = read_available_skill_instruction(&skill_dir) else {
                    continue;
                };
                let instruction = skill_instruction_with_runtime_context(&skill_dir, &instruction);

                let mut allowed_tools = Vec::new();
                let mut custom_tool_bindings = Vec::new();
                let mut restrict_standard_tools = false;

                let config_path = skill_dir.join("config.json");
                if let Ok(content) = fs::read_to_string(config_path)
                    && let Ok(parsed) = serde_json::from_str::<Value>(&content)
                {
                    restrict_standard_tools = parsed.get("allowed_tools").is_some();
                    if let Some(allowed) = parsed.get("allowed_tools").and_then(|v| v.as_array()) {
                        for t in allowed {
                            if let Some(t_str) = t.as_str() {
                                allowed_tools.push(t_str.to_string());
                            }
                        }
                    }
                    if let Some(bindings) = parsed
                        .get("custom_tool_bindings")
                        .or_else(|| parsed.get("customToolBindings"))
                        .or_else(|| parsed.get("customTools"))
                    {
                        custom_tool_bindings = parse_custom_tool_bindings(bindings);
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
                    restrict_standard_tools,
                    allowed_tools,
                    custom_tool_bindings,
                    prompt_priority: 50,
                });
            }
        }
    }
    registry
}

fn skill_instruction_with_runtime_context(skill_dir: &Path, instruction: &str) -> String {
    format!(
        "--- Magi Skill 运行上下文 ---\n\
Magi Skill 根目录：{}\n\
所有相对路径、脚本和资源文件都必须以该目录为基准解析。\n\
回复中引用 Skill 内文件时必须使用完整绝对路径，不能只输出文件名或相对路径，以确保文件预览指向真实资源。\n\
不得改用 Claude、Codex 或其他来源平台的固定安装路径，也不得在这些路径中重新创建或安装 Skill。\n\
若说明中引用的资产在该目录内不存在，应明确报告缺失，不得自行重建替代实现。\n\
--- Skill 原始说明 ---\n{}",
        skill_dir.display(),
        instruction
    )
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
    fn instruction_skill_without_tool_config_inherits_standard_tool_surface() {
        let skill_dir = make_local_skill_dir(
            "inherit-standard-tools",
            "# Prompt Only Skill\n\n先取证，再回答。\n",
        );
        let store = SettingsStore::default();
        store.set_section(
            SKILLS_CONFIG_SECTION,
            serde_json::json!({
                "instructionSkills": [{
                    "skillId": "prompt-only",
                    "name": "Prompt Only",
                    "directoryPath": skill_dir.to_string_lossy().to_string()
                }]
            }),
        );

        let runtime = build_skill_runtime_from_settings(&store);
        let plan = runtime.build_tool_runtime_plan(SkillSelection {
            skill_ids: vec!["prompt-only".to_string()],
            requested_tools: Vec::new(),
        });

        assert!(
            plan.tool_policy.source_skill_ids.is_empty(),
            "未声明 allowed_tools 的 instruction Skill 不应创建标准工具白名单"
        );
        std::fs::remove_dir_all(&skill_dir).expect("skill dir should be removed");
    }

    #[test]
    fn normalize_skills_config_sections_discards_obsolete_top_level_sections() {
        let skill_dir = make_local_skill_dir(
            "obsolete-top-level",
            "# 合并测试\n\n请输出 obsolete-skill。\n",
        );
        let mut snapshot = HashMap::from([
            (
                "skills".to_string(),
                serde_json::json!([
                    {
                        "skillId": "obsolete-skill",
                        "name": "obsolete-skill",
                        "directoryPath": skill_dir.to_string_lossy().to_string()
                    }
                ]),
            ),
            (
                "customTools".to_string(),
                serde_json::json!([
                    {
                        "name": "obsolete-tool",
                        "bindingId": "obsolete-tool"
                    }
                ]),
            ),
        ]);

        normalize_skills_config_sections(&mut snapshot);

        assert!(!snapshot.contains_key("skills"));
        assert!(!snapshot.contains_key("customTools"));
        assert_eq!(snapshot["skillsConfig"], serde_json::json!({}));

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
        assert!(
            plan.prompt_injections[0]
                .body
                .contains(&format!("Magi Skill 根目录：{}", skill_dir.display()))
        );
        assert!(
            plan.prompt_injections[0]
                .body
                .contains("不得改用 Claude、Codex 或其他来源平台的固定安装路径")
        );
        assert!(
            plan.prompt_injections[0]
                .body
                .contains("引用 Skill 内文件时必须使用完整绝对路径")
        );

        std::fs::remove_dir_all(&skill_dir).expect("temp skill dir should be removed");
    }

    #[test]
    fn load_skills_into_registry_skips_unavailable_instruction_skills() {
        let valid_dir =
            make_local_skill_dir("valid-skill", "# 可用 Skill\n\n请输出 available-skill。\n");
        let empty_dir = unique_test_dir("empty-skill");
        std::fs::create_dir_all(&empty_dir).expect("empty skill dir should be created");
        std::fs::write(empty_dir.join("SKILL.md"), "   \n")
            .expect("empty skill markdown should be written");
        let missing_dir = unique_test_dir("missing-skill");

        let store = SettingsStore::new();
        store.set_section(
            "skillsConfig",
            serde_json::json!({
                "instructionSkills": [
                    {
                        "skillId": "valid-skill",
                        "name": "valid-skill",
                        "directoryPath": valid_dir.to_string_lossy().to_string()
                    },
                    {
                        "skillId": "empty-skill",
                        "name": "empty-skill",
                        "directoryPath": empty_dir.to_string_lossy().to_string()
                    },
                    {
                        "skillId": "missing-skill",
                        "name": "missing-skill",
                        "directoryPath": missing_dir.to_string_lossy().to_string()
                    }
                ]
            }),
        );

        let registry = load_skills_into_registry(&store);
        let plan = registry.build_tool_runtime_plan(&SkillSelection {
            skill_ids: vec![
                "valid-skill".to_string(),
                "empty-skill".to_string(),
                "missing-skill".to_string(),
            ],
            requested_tools: vec![],
        });

        assert_eq!(plan.prompt_injections.len(), 1);
        assert!(plan.prompt_injections[0].body.contains("available-skill"));

        std::fs::remove_dir_all(&valid_dir).expect("valid temp skill dir should be removed");
        std::fs::remove_dir_all(&empty_dir).expect("empty temp skill dir should be removed");
    }

    #[test]
    fn load_skills_into_registry_reads_custom_tool_bindings() {
        let skill_dir = make_local_skill_dir("custom-binding", "# Custom binding\n\n");
        std::fs::write(
            skill_dir.join("config.json"),
            serde_json::json!({
                "allowed_tools": ["file_read"],
                "custom_tool_bindings": [
                    {
                        "binding_id": "openai-prompter",
                        "tool_name": "model.prompt",
                        "description": "让辅助模型润色一段提示词",
                        "bridge_kind": "model",
                        "dispatch_action": "model_prompt",
                        "bridge_target": "openai"
                    },
                    {
                        "bindingId": "anthropic-reviewer",
                        "toolName": "mcp.review",
                        "description": "调用 MCP 进行审查",
                        "bridgeKind": "mcp",
                        "dispatchAction": "mcp_tool_call",
                        "bridgeTarget": "review-server"
                    }
                ]
            })
            .to_string(),
        )
        .expect("skill config should be written");

        let store = SettingsStore::new();
        store.set_section(
            "skillsConfig",
            serde_json::json!({
                "instructionSkills": [
                    {
                        "skillId": "custom-skill",
                        "name": "custom-skill",
                        "directoryPath": skill_dir.to_string_lossy().to_string()
                    }
                ]
            }),
        );

        let registry = load_skills_into_registry(&store);
        let skill = registry.get("custom-skill").expect("skill should exist");
        assert_eq!(skill.allowed_tools, vec!["file_read".to_string()]);
        assert_eq!(skill.custom_tool_bindings.len(), 2);
        assert_eq!(
            skill.custom_tool_bindings[0].binding_id,
            "anthropic-reviewer"
        );
        assert_eq!(skill.custom_tool_bindings[0].tool_name, "mcp.review");
        assert_eq!(
            skill.custom_tool_bindings[0].bridge_kind,
            BridgeBindingKind::Mcp
        );
        assert_eq!(skill.custom_tool_bindings[1].binding_id, "openai-prompter");
        assert_eq!(
            skill.custom_tool_bindings[1].bridge_kind,
            BridgeBindingKind::Model
        );

        std::fs::remove_dir_all(&skill_dir).expect("temp skill dir should be removed");
    }

    #[test]
    fn build_skill_runtime_from_settings_does_not_load_obsolete_top_level_sections() {
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
        assert!(registry.get("runtime-skill").is_none());
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
    fn save_skills_config_object_removes_obsolete_sections_without_loading_them() {
        let store = SettingsStore::new();
        store.set_section(
            "skills",
            serde_json::json!([{ "skillId": "obsolete-skill" }]),
        );
        store.set_section(
            "customTools",
            serde_json::json!([{ "name": "obsolete-tool" }]),
        );

        save_skills_config_object(
            &store,
            serde_json::json!({
                "instructionSkills": [
                    {
                        "skillId": "saved-skill",
                        "name": "saved-skill"
                    }
                ]
            })
            .as_object()
            .cloned()
            .expect("skills config should be an object"),
        );

        assert!(store.get("skills").is_none());
        assert!(store.get("customTools").is_none());
        assert_eq!(
            store.get_section("skillsConfig")["instructionSkills"][0]["skillId"],
            serde_json::json!("saved-skill")
        );
    }

    #[test]
    fn save_skills_config_object_strips_scope_binding_fields() {
        let store = SettingsStore::new();

        save_skills_config_object(
            &store,
            serde_json::json!({
                "workspaceId": "workspace-old",
                "workspace_path": "/tmp/old",
                "sessionId": "session-old",
                "instructionSkills": [
                    {
                        "skillId": "saved-skill",
                        "skillName": "legacy-saved-skill",
                        "workspaceId": "workspace-old",
                        "session_id": "session-old"
                    },
                    {
                        "skillName": "legacy-skill-name-only"
                    },
                    "invalid-instruction-skill"
                ],
                "customTools": [
                    {
                        "name": "saved-tool",
                        "toolName": "legacy-saved-tool",
                        "workspacePath": "/tmp/old",
                        "sessionId": "session-old"
                    },
                    {
                        "toolName": "legacy-tool-name-only"
                    },
                    "invalid-custom-tool"
                ]
            })
            .as_object()
            .cloned()
            .expect("skills config should be an object"),
        );

        let saved = store.get_section("skillsConfig");
        for key in [
            "workspaceId",
            "workspace_path",
            "sessionId",
            "workspacePath",
            "session_id",
        ] {
            assert!(saved.get(key).is_none());
        }
        assert!(saved["instructionSkills"][0].get("workspaceId").is_none());
        assert!(saved["instructionSkills"][0].get("session_id").is_none());
        assert_eq!(
            saved["instructionSkills"]
                .as_array()
                .expect("instructionSkills should be array")
                .len(),
            1
        );
        assert_eq!(
            saved["instructionSkills"][0]["skillId"],
            serde_json::json!("saved-skill")
        );
        assert_eq!(
            saved["instructionSkills"][0]["name"],
            serde_json::json!("saved-skill")
        );
        assert!(saved["instructionSkills"][0].get("skillName").is_none());
        assert_eq!(
            saved["customTools"]
                .as_array()
                .expect("customTools should be array")
                .len(),
            1
        );
        assert_eq!(
            saved["customTools"][0]["name"],
            serde_json::json!("saved-tool")
        );
        assert!(saved["customTools"][0].get("toolName").is_none());
        assert!(saved["customTools"][0].get("workspacePath").is_none());
        assert!(saved["customTools"][0].get("sessionId").is_none());
    }
}
