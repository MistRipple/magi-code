use super::*;

pub(super) fn load_skills_config_object(
    state: &ApiState,
) -> serde_json::Map<String, serde_json::Value> {
    skill_loader::skills_config_object(&state.settings_store)
}

pub(super) fn persist_skills_config_object(
    state: &ApiState,
    config: serde_json::Map<String, serde_json::Value>,
) -> Result<(), ApiError> {
    let mut config = config;
    crate::builtin_skills::protect_builtin_entries(&load_skills_config_object(state), &mut config);
    skill_loader::save_skills_config_object(&state.settings_store, config)
        .map_err(settings_persistence_error)?;
    reload_skill_registry(state);
    Ok(())
}

pub(super) fn reload_skill_registry(state: &ApiState) {
    if let Some(ref skill_rt) = state.skill_runtime {
        skill_loader::reload_skill_runtime_from_settings(skill_rt, &state.settings_store);
    }
}

pub(super) fn load_instruction_skills(state: &ApiState) -> Vec<serde_json::Value> {
    load_skills_config_object(state)
        .remove("instructionSkills")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
}

pub(super) fn persist_instruction_skills(
    state: &ApiState,
    instruction_skills: Vec<serde_json::Value>,
) -> Result<(), ApiError> {
    let mut config = load_skills_config_object(state);
    config.insert(
        "instructionSkills".to_string(),
        serde_json::Value::Array(instruction_skills),
    );
    persist_skills_config_object(state, config)
}

pub(super) fn normalize_instruction_skill_entry(
    request: &serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    let skill_id = request
        .get("skillId")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("skillId 不能为空".to_string()))?;
    let mut entry = request.as_object().cloned().unwrap_or_default();
    strip_scope_binding_fields_from_map(&mut entry);
    entry.insert("skillId".to_string(), serde_json::json!(skill_id));
    entry.insert("name".to_string(), serde_json::json!(skill_id));
    if entry
        .get("fullName")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        entry.insert("fullName".to_string(), serde_json::json!(skill_id));
    }
    Ok(serde_json::Value::Object(entry))
}

pub(super) fn is_local_skill_dir(dir: &std::path::Path) -> bool {
    dir.join("prompt.md").is_file()
        || dir.join("SKILL.md").is_file()
        || dir.join("README.md").is_file()
        || dir.join("config.json").is_file()
}

pub(super) fn build_local_instruction_skill_entry(
    dir: &std::path::Path,
) -> Result<serde_json::Value, ApiError> {
    let skill_name = dir
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("本地 Skill 名称不可用".to_string()))?;
    let metadata = read_skill_directory_metadata(dir)?;
    let description = skill_loader::read_skill_description(dir);
    Ok(serde_json::json!({
        "name": skill_name,
        "skillId": skill_name,
        "fullName": skill_name,
        "directoryPath": dir.to_string_lossy(),
        "directoryPathRef": magi_core::HostPath::from_path(dir.to_path_buf()).to_path_ref().as_str(),
        "description": description,
        "source": "local",
        "fileCount": metadata.file_count,
        "lastRefreshed": epoch_ms_now(),
        "lastModified": metadata.last_modified_epoch_ms,
        "loadedContentHash": metadata.content_hash,
        "installedAt": epoch_ms_now(),
        "updatedAt": epoch_ms_now(),
    }))
}

pub(super) fn scan_local_instruction_skill_entries(
    root_dir: &std::path::Path,
) -> Result<Vec<serde_json::Value>, ApiError> {
    if !root_dir.exists() {
        return Err(ApiError::InvalidInput("所选目录不存在".to_string()));
    }
    if !root_dir.is_dir() {
        return Err(ApiError::InvalidInput("所选路径不是目录".to_string()));
    }

    if is_local_skill_dir(root_dir) {
        return Ok(vec![build_local_instruction_skill_entry(root_dir)?]);
    }

    let mut candidates = std::fs::read_dir(root_dir)
        .map_err(|_| ApiError::InvalidInput("无法读取所选目录".to_string()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .filter(|path| is_local_skill_dir(path))
        .collect::<Vec<_>>();
    candidates.sort();

    let mut entries = Vec::new();
    for dir in candidates {
        entries.push(build_local_instruction_skill_entry(&dir)?);
    }
    Ok(entries)
}

pub(super) fn remove_instruction_skill_from_list(
    instruction_skills: &mut Vec<serde_json::Value>,
    skill_name: &str,
) -> bool {
    let before_len = instruction_skills.len();
    instruction_skills.retain(|item| {
        item.get("skillId")
            .and_then(|value| value.as_str())
            .is_none_or(|value| value != skill_name)
    });
    instruction_skills.len() != before_len
}

pub(super) fn instruction_skill_id(item: &serde_json::Value) -> Option<String> {
    item.get("skillId")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub(super) fn instruction_skill_matches(item: &serde_json::Value, skill_name: &str) -> bool {
    item.get("skillId")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value == skill_name)
}

pub(super) fn normalize_local_instruction_skill_request_path(
    request: &serde_json::Value,
) -> Result<std::path::PathBuf, ApiError> {
    let directory_path = request
        .get("directoryPath")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("请选择本地 Skill 目录".to_string()))?;
    magi_core::HostPath::resolve_native_input(
        directory_path,
        std::env::current_dir().ok().as_deref(),
        dirs::home_dir().as_deref(),
    )
    .map(magi_core::HostPath::into_path_buf)
    .map_err(|error| ApiError::InvalidInput(error.to_string()))
}

pub(super) fn normalize_local_instruction_skill_request_skill_id(
    request: &serde_json::Value,
) -> Option<String> {
    request
        .get("skillId")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(super) fn public_local_instruction_skill_entry(entry: &serde_json::Value) -> serde_json::Value {
    let mut obj = entry.as_object().cloned().unwrap_or_default();
    obj.remove("directoryPath");
    obj.remove("directoryPathRef");
    if !obj.contains_key("localSkillId")
        && let Some(skill_id) = instruction_skill_id(entry)
    {
        obj.insert("localSkillId".to_string(), serde_json::json!(skill_id));
    }
    serde_json::Value::Object(obj)
}

pub(super) fn epoch_ms_now() -> u64 {
    magi_core::UtcMillis::now().0
}
