//! 随 Magi 发布的内置 Skill。
//!
//! 内置 Skill 与用户安装的 Skill 走同一套机制：说明文件写到 state root 下的目录，并在
//! `skillsConfig.instructionSkills` 里登记一个普通条目，所以能在设置里启停、在输入框里用 `/` 唤起。
//! 内置 Skill 可以停用，但**不能删除**：移除请求会被拒绝，整体保存配置时缺失的内置条目会被补回，
//! 启动时也会补齐。安装记录单独保存在 `builtin-skills/.installed.json`：版本没变就不重写说明文件；
//! Magi 升级带来新版本时才会更新说明文件，并保留用户的启停选择。

use std::collections::HashMap;
use std::path::Path;

use magi_settings_store::SettingsStore;
use serde_json::{Map, Value, json};

use crate::skill_loader::{
    read_skill_description, save_skills_config_object, skills_config_object,
};

struct BuiltinSkill {
    id: &'static str,
    /// 说明文件有改动时递增。
    version: u32,
    body: &'static str,
    /// 技能的 `config.json`：声明它要用的工具。带这个声明的技能被选中时，本轮带工具执行，
    /// 并且只开放声明的这些工具。
    config: &'static str,
}

const BUILTIN_SKILLS: &[BuiltinSkill] = &[BuiltinSkill {
    id: "magi-cloudflare-tunnel",
    version: 3,
    body: include_str!("../assets/builtin-skills/magi-cloudflare-tunnel/SKILL.md"),
    config: include_str!("../assets/builtin-skills/magi-cloudflare-tunnel/config.json"),
}];

const INSTALLED_FILE: &str = ".installed.json";

/// 安装或升级内置 Skill。失败只影响内置 Skill 本身，调用方记日志即可。
pub fn install_builtin_skills(store: &SettingsStore, state_root: &Path) -> std::io::Result<()> {
    let root = state_root.join("builtin-skills");
    let installed_path = root.join(INSTALLED_FILE);
    let mut installed: HashMap<String, u32> = std::fs::read(&installed_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let mut changed = false;
    for skill in BUILTIN_SKILLS {
        let dir = root.join(skill.id);
        let up_to_date = installed.get(skill.id) == Some(&skill.version)
            && dir.join("SKILL.md").is_file()
            && has_entry(store, skill.id);
        if up_to_date {
            continue;
        }
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("SKILL.md"), skill.body)?;
        std::fs::write(dir.join("config.json"), skill.config)?;
        upsert_entry(store, skill, &dir)?;
        installed.insert(skill.id.to_string(), skill.version);
        changed = true;
    }
    if changed {
        let body = serde_json::to_vec_pretty(&installed)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        std::fs::write(installed_path, body)?;
    }
    Ok(())
}

fn has_entry(store: &SettingsStore, skill_id: &str) -> bool {
    skills_config_object(store)
        .get("instructionSkills")
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry.get("skillId").and_then(Value::as_str) == Some(skill_id) && is_builtin(entry)
            })
        })
}

/// 条目是否是内置 Skill。
pub fn is_builtin(entry: &Value) -> bool {
    entry.get("builtin").and_then(Value::as_bool) == Some(true)
}

/// 整体保存 Skill 配置时保护内置条目：被漏掉的补回，`builtin` 标记不能被去掉（启停选择以提交的为准）。
pub fn protect_builtin_entries(current: &Map<String, Value>, incoming: &mut Map<String, Value>) {
    let current_builtin: Vec<&Value> = current
        .get("instructionSkills")
        .and_then(Value::as_array)
        .map(|entries| entries.iter().filter(|entry| is_builtin(entry)).collect())
        .unwrap_or_default();
    if current_builtin.is_empty() {
        return;
    }
    let entries = incoming
        .entry("instructionSkills".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(entries) = entries.as_array_mut() else {
        return;
    };
    for builtin in current_builtin {
        let id = builtin.get("skillId").and_then(Value::as_str);
        match entries
            .iter_mut()
            .find(|entry| entry.get("skillId").and_then(Value::as_str) == id)
        {
            Some(submitted) => {
                if let Some(object) = submitted.as_object_mut() {
                    object.insert("builtin".to_string(), Value::Bool(true));
                }
            }
            None => entries.push(builtin.clone()),
        }
    }
}

fn upsert_entry(store: &SettingsStore, skill: &BuiltinSkill, dir: &Path) -> std::io::Result<()> {
    let mut config = skills_config_object(store);
    let mut entries = config
        .remove("instructionSkills")
        .and_then(|value| match value {
            Value::Array(entries) => Some(entries),
            _ => None,
        })
        .unwrap_or_default();
    let now = magi_core::UtcMillis::now().0;
    // 描述从已写入的 Skill 目录解析（config.json → SKILL.md front matter），
    // 与本地 / 仓库 Skill 同一套规则，避免内置 Skill 再维护一份描述事实源。
    let description = read_skill_description(dir);
    let mut entry = json!({
        "name": skill.id,
        "skillId": skill.id,
        "fullName": skill.id,
        "directoryPath": dir.to_string_lossy(),
        "directoryPathRef": magi_core::HostPath::from_path(dir.to_path_buf()).to_path_ref().as_str(),
        "description": description,
        "source": "local",
        "builtin": true,
        "updatedAt": now,
    });
    match entries
        .iter_mut()
        .find(|existing| existing.get("skillId").and_then(Value::as_str) == Some(skill.id))
    {
        Some(existing) => {
            // 升级只刷新路径与说明，保留用户对启停的选择。
            if let (Some(target), Some(source)) = (existing.as_object_mut(), entry.as_object_mut())
            {
                for (key, value) in std::mem::take(source) {
                    target.insert(key, value);
                }
            }
        }
        None => {
            entry["installedAt"] = json!(now);
            entry["enabled"] = json!(true);
            entries.push(entry);
        }
    }
    config.insert("instructionSkills".to_string(), Value::Array(entries));
    save_skills_config_object(store, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_in(dir: &Path) -> SettingsStore {
        let _ = dir;
        SettingsStore::new()
    }

    #[test]
    fn builtin_skills_are_installed_and_restored_but_the_users_enable_choice_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        install_builtin_skills(&store, dir.path()).unwrap();

        let skill_md = dir
            .path()
            .join("builtin-skills/magi-cloudflare-tunnel/SKILL.md");
        assert!(
            std::fs::read_to_string(&skill_md)
                .unwrap()
                .contains("命名隧道")
        );
        let config = skills_config_object(&store);
        let entries = config["instructionSkills"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["skillId"], "magi-cloudflare-tunnel");
        assert_eq!(entries[0]["enabled"], true);
        // 展示描述来自 SKILL.md 的 front matter，而不是 Rust 常量或 config.json。
        assert_eq!(
            entries[0]["description"],
            "协助用户配置 Cloudflare 命名隧道，让 Magi MCP 的公网地址固定、重启后不变。"
        );
        assert!(is_builtin(&entries[0]));

        // 停用是允许的，再次启动不会改回启用。
        let mut config = skills_config_object(&store);
        config["instructionSkills"][0]["enabled"] = json!(false);
        save_skills_config_object(&store, config).unwrap();
        install_builtin_skills(&store, dir.path()).unwrap();
        assert_eq!(
            skills_config_object(&store)["instructionSkills"][0]["enabled"],
            false
        );

        // 条目被弄丢（旧数据、手工改配置）时，启动会补回来。
        let mut config = skills_config_object(&store);
        config.insert("instructionSkills".to_string(), json!([]));
        save_skills_config_object(&store, config).unwrap();
        install_builtin_skills(&store, dir.path()).unwrap();
        assert!(has_entry(&store, "magi-cloudflare-tunnel"));
    }

    #[test]
    fn saving_the_whole_config_cannot_drop_or_unmark_a_builtin_skill() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        install_builtin_skills(&store, dir.path()).unwrap();
        let current = skills_config_object(&store);

        let mut dropped = Map::new();
        dropped.insert(
            "instructionSkills".to_string(),
            json!([{ "skillId": "user-skill", "name": "user-skill" }]),
        );
        protect_builtin_entries(&current, &mut dropped);
        let ids: Vec<&str> = dropped["instructionSkills"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry["skillId"].as_str())
            .collect();
        assert_eq!(ids, ["user-skill", "magi-cloudflare-tunnel"]);

        let mut unmarked = Map::new();
        unmarked.insert(
            "instructionSkills".to_string(),
            json!([{ "skillId": "magi-cloudflare-tunnel", "enabled": false }]),
        );
        protect_builtin_entries(&current, &mut unmarked);
        let entry = &unmarked["instructionSkills"][0];
        assert!(is_builtin(entry));
        assert_eq!(entry["enabled"], false, "启停以提交的为准");
    }

    #[test]
    fn an_installed_builtin_skill_is_loaded_into_the_skill_registry() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        install_builtin_skills(&store, dir.path()).unwrap();
        let registry = crate::skill_loader::load_skills_into_registry(&store);
        let skill = registry
            .get("magi-cloudflare-tunnel")
            .expect("内置技能必须出现在技能注册表里，斜杠选中后才有内容可注入");
        assert!(skill.instruction.contains("命名隧道"));
    }

    #[test]
    fn the_cloudflare_skill_declares_only_the_browser_tools_it_needs() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        install_builtin_skills(&store, dir.path()).unwrap();
        let registry = crate::skill_loader::load_skills_into_registry(&store);
        let skill = registry.get("magi-cloudflare-tunnel").unwrap();
        assert!(skill.restrict_standard_tools, "声明了工具就只开放这些工具");
        for needed in [
            "browser_navigate",
            "browser_snapshot",
            "browser_click",
            "browser_type",
        ] {
            assert!(
                skill.allowed_tools.iter().any(|tool| tool == needed),
                "{needed}"
            );
        }
        // 不开放执行脚本 / 读网络与控制台 / 文件与命令工具，避免读到控制台里的令牌或改动本机。
        for forbidden in [
            "browser_evaluate",
            "browser_network",
            "browser_console",
            "file_write",
            "shell_exec",
        ] {
            assert!(
                !skill.allowed_tools.iter().any(|tool| tool == forbidden),
                "{forbidden}"
            );
        }
        let builtin = &BUILTIN_SKILLS[0];
        let parsed: Value = serde_json::from_str(builtin.config).unwrap();
        assert!(parsed["allowed_tools"].is_array());
        assert!(
            parsed.get("description").is_none(),
            "描述只保留在 SKILL.md front matter 一处"
        );
    }

    #[test]
    fn the_skill_never_asks_the_model_to_read_or_type_secrets() {
        let body = BUILTIN_SKILLS[0].body;
        assert!(body.contains("不要截图"));
        assert!(body.contains("绝不要替用户输入账号或密码"));
        // 令牌页必须点名禁止会读出页面内容的工具，否则令牌会进入模型上下文。
        assert!(body.contains("不要在这一页调用 `browser_snapshot`"));
        assert!(body.contains("`browser_read`"));
        assert!(body.contains("`browser_screenshot`"));
    }

    #[test]
    fn the_skill_requires_handing_browser_control_back_after_manual_steps() {
        let body = BUILTIN_SKILLS[0].body;
        // 用户手动操作会暂停 Agent 的浏览器控制，说明必须引导“交还控制”并给出安全的页面确认方式。
        assert!(body.contains("交还控制"));
        assert!(body.contains("browser_wait_for"));
    }
}
