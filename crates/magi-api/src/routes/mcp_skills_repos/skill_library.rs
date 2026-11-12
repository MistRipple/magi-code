use super::*;

#[derive(Clone, Debug)]
pub(super) struct AvailableRepositorySkill {
    pub(super) skill: RepositorySkill,
    pub(super) repository_root: PathBuf,
    pub(super) repository_id: String,
    pub(super) repository_url: String,
    pub(super) commit: Option<String>,
    pub(super) checked_at: u64,
}

pub(super) fn installed_skill_public_entry(
    state: &ApiState,
    skill: serde_json::Value,
    available: Option<&AvailableRepositorySkill>,
) -> serde_json::Value {
    let mut entry = skill.as_object().cloned().unwrap_or_default();
    entry.remove("directoryPath");
    entry.remove("directoryPathRef");
    entry.insert("installed".to_string(), serde_json::json!(true));
    let enabled = entry.get("enabled").and_then(serde_json::Value::as_bool) != Some(false);
    entry.insert("enabled".to_string(), serde_json::json!(enabled));

    let source = entry
        .get("source")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("local")
        .to_string();
    let directory = skill_loader::instruction_skill_directory_path(&skill);
    let metadata = directory
        .as_deref()
        .filter(|path| path.is_dir())
        .and_then(|path| read_skill_directory_metadata(path).ok());
    let current_hash = metadata
        .as_ref()
        .map(|metadata| metadata.content_hash.clone());
    let loaded_hash = entry
        .get("loadedContentHash")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            entry
                .get("installedContentHash")
                .and_then(serde_json::Value::as_str)
        })
        .map(ToOwned::to_owned);

    if let Some(metadata) = &metadata {
        entry.insert(
            "fileCount".to_string(),
            serde_json::json!(metadata.file_count),
        );
        entry.insert(
            "lastModified".to_string(),
            serde_json::json!(metadata.last_modified_epoch_ms),
        );
        entry.insert(
            "currentContentHash".to_string(),
            serde_json::json!(metadata.content_hash),
        );
        entry.insert("sourceStatus".to_string(), serde_json::json!("available"));
    } else {
        entry.insert("sourceStatus".to_string(), serde_json::json!("missing"));
        entry.insert(
            "updateStatus".to_string(),
            serde_json::json!("source_missing"),
        );
        entry.insert("updateAvailable".to_string(), serde_json::json!(false));
        return serde_json::Value::Object(entry);
    }

    if source == "repository" {
        let skill_id = instruction_skill_id(&skill).unwrap_or_default();
        let backup_available = installed_skill_backup_root(state)
            .ok()
            .and_then(|root| {
                installed_skill_cache_key(&skill_id)
                    .ok()
                    .map(|key| root.join(key))
            })
            .is_some_and(|path| path.is_dir());
        entry.insert(
            "rollbackAvailable".to_string(),
            serde_json::json!(backup_available),
        );

        let installed_hash = entry
            .get("installedContentHash")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| loaded_hash.clone());
        if current_hash != installed_hash {
            entry.insert(
                "updateStatus".to_string(),
                serde_json::json!("local_modified"),
            );
            entry.insert("updateAvailable".to_string(), serde_json::json!(false));
        } else if let Some(available) = available {
            let update_available =
                installed_hash.as_deref() != Some(available.skill.content_hash.as_str());
            entry.insert(
                "availableContentHash".to_string(),
                serde_json::json!(available.skill.content_hash.clone()),
            );
            entry.insert(
                "availableVersion".to_string(),
                serde_json::json!(available.skill.version.clone()),
            );
            entry.insert(
                "availableCommit".to_string(),
                serde_json::json!(available.commit.clone()),
            );
            entry.insert(
                "lastCheckedAt".to_string(),
                serde_json::json!(available.checked_at),
            );
            entry.insert(
                "updateStatus".to_string(),
                serde_json::json!(if update_available {
                    "update_available"
                } else {
                    "up_to_date"
                }),
            );
            entry.insert(
                "updateAvailable".to_string(),
                serde_json::json!(update_available),
            );
        } else {
            entry.insert("sourceStatus".to_string(), serde_json::json!("removed"));
            entry.insert(
                "updateStatus".to_string(),
                serde_json::json!("source_removed"),
            );
            entry.insert("updateAvailable".to_string(), serde_json::json!(false));
        }
    } else {
        let changed = current_hash != loaded_hash;
        entry.insert(
            "updateStatus".to_string(),
            serde_json::json!(if changed {
                "local_changed"
            } else {
                "up_to_date"
            }),
        );
        entry.insert("updateAvailable".to_string(), serde_json::json!(changed));
    }
    serde_json::Value::Object(entry)
}

pub(super) async fn build_skill_library(
    state: &ApiState,
    refresh_stale: bool,
) -> serde_json::Value {
    let installed_skills = load_instruction_skills(state);
    let installed_skill_ids: std::collections::HashSet<String> = installed_skills
        .iter()
        .filter_map(instruction_skill_id)
        .collect();
    let repos = state
        .settings_store
        .get_section("repositories")
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut failed_repo_count = 0usize;
    let mut available_skills = HashMap::<String, AvailableRepositorySkill>::new();
    for repo in &repos {
        let repo_url = repo.get("url").and_then(|v| v.as_str()).unwrap_or_default();
        let repo_id = repo
            .get("repositoryId")
            .and_then(|v| v.as_str())
            .unwrap_or(repo_url);
        if repo_url.is_empty() {
            continue;
        }
        let should_refresh =
            refresh_stale && repository_update_check_is_stale(repo, epoch_ms_now());
        let snapshot = match load_repository_snapshot(state, repo, should_refresh).await {
            Ok(snapshot) => Ok(snapshot),
            Err(error) if should_refresh => {
                tracing::warn!(
                    repository_id = %repo_id,
                    url = %repo_url,
                    error = ?error,
                    "Skill repository refresh failed; using existing cache"
                );
                load_repository_snapshot(state, repo, false).await
            }
            Err(error) => Err(error),
        };
        match snapshot {
            Ok(snapshot) => {
                for skill in snapshot.skills {
                    available_skills.insert(
                        skill.skill_id.clone(),
                        AvailableRepositorySkill {
                            skill,
                            repository_root: snapshot.root.clone(),
                            repository_id: repo_id.to_string(),
                            repository_url: repo_url.to_string(),
                            commit: snapshot.commit.clone(),
                            checked_at: snapshot.checked_at,
                        },
                    );
                }
            }
            Err(error) => {
                tracing::warn!(
                    repository_id = %repo_id,
                    url = %repo_url,
                    error = ?error,
                    "Skill repository load failed"
                );
                failed_repo_count += 1;
            }
        }
    }

    let mut all_skills = installed_skills
        .into_iter()
        .map(|skill| {
            let available =
                instruction_skill_id(&skill).and_then(|skill_id| available_skills.get(&skill_id));
            installed_skill_public_entry(state, skill, available)
        })
        .collect::<Vec<_>>();
    for available in available_skills.into_values() {
        if installed_skill_ids.contains(&available.skill.skill_id) {
            continue;
        }
        all_skills.push(repository_skill_public_entry(
            available.skill,
            &available.repository_id,
            &available.repository_url,
            false,
            available.commit.as_deref(),
            Some(available.checked_at),
        ));
    }
    all_skills.sort_by(|left, right| {
        let left_installed = left
            .get("installed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let right_installed = right
            .get("installed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        right_installed.cmp(&left_installed).then_with(|| {
            left.get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .cmp(
                    right
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                )
        })
    });
    let update_available_count = all_skills
        .iter()
        .filter(|skill| {
            skill
                .get("updateAvailable")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
        })
        .count();
    serde_json::json!({
        "skills": all_skills,
        "failedRepositoryCount": failed_repo_count,
        "updateAvailableCount": update_available_count,
    })
}

pub(super) async fn list_skills(State(state): State<ApiState>) -> Json<serde_json::Value> {
    Json(build_skill_library(&state, true).await)
}

pub(super) async fn install_skill(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if request.get("skill").is_some() || request.get("updates").is_some() {
        return Err(ApiError::InvalidInput(
            "安装 Skill 请求不能包裹在 skill/updates 中".to_string(),
        ));
    }
    let normalized_request = normalize_instruction_skill_entry(&request)?;
    let skill_id = normalized_request
        .get("skillId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let (repository, repository_root, skill, repository_id) =
        find_repository_skill(&state, &skill_id).await?;
    let repository_commit = cached_repository_commit(&repository_root).await;
    let source_dir = repository_root.join(&skill.relative_path);
    let cache_root = installed_skill_cache_root(&state)?;
    let target_dir = cache_root.join(installed_skill_cache_key(&skill_id)?);
    copy_repository_skill_directory(&source_dir, &target_dir)?;

    let mut normalized = build_local_instruction_skill_entry(&target_dir)?;
    let object = normalized
        .as_object_mut()
        .ok_or_else(|| ApiError::InvalidInput(SKILL_CACHE_PUBLIC_ERROR.to_string()))?;
    object.insert("skillId".to_string(), serde_json::json!(skill_id));
    object.insert("fullName".to_string(), serde_json::json!(skill_id));
    object.insert("name".to_string(), serde_json::json!(skill.name));
    object.insert(
        "description".to_string(),
        serde_json::json!(skill.description),
    );
    object.insert("source".to_string(), serde_json::json!("repository"));
    object.insert("repositoryId".to_string(), serde_json::json!(repository_id));
    object.insert(
        "repositoryUrl".to_string(),
        serde_json::json!(repository.canonical_url),
    );
    object.insert(
        "repositoryName".to_string(),
        serde_json::json!(repository.canonical_url),
    );
    object.insert(
        "repositoryPath".to_string(),
        serde_json::json!(relative_path_to_slash_string(&skill.relative_path)?),
    );
    object.insert(
        "installedContentHash".to_string(),
        serde_json::json!(skill.content_hash.clone()),
    );
    object.insert(
        "loadedContentHash".to_string(),
        serde_json::json!(skill.content_hash),
    );
    object.insert(
        "installedCommit".to_string(),
        serde_json::json!(repository_commit),
    );
    object.insert("version".to_string(), serde_json::json!(skill.version));
    object.insert("author".to_string(), serde_json::json!(skill.author));
    object.insert("category".to_string(), serde_json::json!(skill.category));
    object.insert(
        "lastCheckedAt".to_string(),
        serde_json::json!(epoch_ms_now()),
    );
    object.insert("updatedAt".to_string(), serde_json::json!(epoch_ms_now()));

    let mut instruction_skills = load_instruction_skills(&state);
    upsert_named_object_array_entry(&mut instruction_skills, normalized, &["skillId"]);
    persist_instruction_skills(&state, instruction_skills)?;
    Ok(Json(serde_json::json!({
        "installed": true,
        "skillId": skill_id,
    })))
}

pub(super) async fn install_local_skill(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let root_dir = normalize_local_instruction_skill_request_path(&request)?;
    let mut entries = scan_local_instruction_skill_entries(&root_dir)?;
    if entries.is_empty() {
        return Err(ApiError::InvalidInput(
            "所选目录下未发现可导入的 Skill".to_string(),
        ));
    }

    if let Some(skill_id) = normalize_local_instruction_skill_request_skill_id(&request) {
        entries.retain(|entry| instruction_skill_matches(entry, &skill_id));
        if entries.is_empty() {
            return Err(ApiError::InvalidInput(
                "所选目录中未找到该 Skill".to_string(),
            ));
        }
    }

    let mut instruction_skills = load_instruction_skills(&state);
    for entry in &entries {
        upsert_named_object_array_entry(&mut instruction_skills, entry.clone(), &["skillId"]);
    }
    persist_instruction_skills(&state, instruction_skills)?;

    Ok(Json(serde_json::json!({
        "installed": true,
        "count": entries.len(),
    })))
}

pub(super) async fn scan_local_skill_directory(
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let root_dir = normalize_local_instruction_skill_request_path(&request)?;
    let entries = scan_local_instruction_skill_entries(&root_dir)?;
    let public_entries = entries
        .iter()
        .map(public_local_instruction_skill_entry)
        .collect::<Vec<_>>();
    Ok(Json(serde_json::json!({
        "skills": public_entries,
        "count": public_entries.len(),
    })))
}

pub(super) async fn save_skills_config(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let config = request
        .as_object()
        .cloned()
        .ok_or_else(|| ApiError::InvalidInput("skillsConfig 必须是对象".to_string()))?;
    persist_skills_config_object(&state, config)?;
    Ok(Json(serde_json::json!({ "saved": true })))
}

pub(super) async fn toggle_skill(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let skill_id = request
        .get("skillId")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("skillId 不能为空".to_string()))?;
    let enabled = request
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| ApiError::InvalidInput("enabled 必须是布尔值".to_string()))?;

    let mut instruction_skills = load_instruction_skills(&state);
    let skill = instruction_skills
        .iter_mut()
        .find(|item| instruction_skill_matches(item, skill_id))
        .ok_or_else(|| ApiError::not_found("技能未安装", skill_id))?;
    let object = skill
        .as_object_mut()
        .ok_or_else(|| ApiError::InvalidInput("Skill 配置无效".to_string()))?;
    object.insert("enabled".to_string(), serde_json::json!(enabled));
    persist_instruction_skills(&state, instruction_skills)?;

    Ok(Json(serde_json::json!({
        "updated": true,
        "skillId": skill_id,
        "enabled": enabled,
    })))
}

pub(super) async fn add_custom_tool(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut entry = request.as_object().cloned().unwrap_or_default();
    strip_scope_binding_fields_from_map(&mut entry);
    let tool_name = entry
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("name 不能为空".to_string()))?
        .to_string();
    entry.insert("name".to_string(), serde_json::json!(tool_name));

    let mut config = load_skills_config_object(&state);
    let mut custom_tools = config
        .remove("customTools")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    upsert_named_object_array_entry(
        &mut custom_tools,
        serde_json::Value::Object(entry),
        &["name"],
    );
    config.insert(
        "customTools".to_string(),
        serde_json::Value::Array(custom_tools),
    );
    persist_skills_config_object(&state, config)?;
    Ok(Json(serde_json::json!({ "added": true })))
}

pub(super) async fn remove_installed_skill(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let source = request
        .get("source")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            ApiError::InvalidInput("source 必须为 'custom' 或 'instruction'".to_string())
        })?;
    let skill_id = request
        .get("skillId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("skillId 不能为空".to_string()))?;
    match source {
        "custom" => {
            let mut config = load_skills_config_object(&state);
            let mut custom_tools = config
                .remove("customTools")
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default();
            custom_tools.retain(|item| {
                item.get("name")
                    .and_then(|value| value.as_str())
                    .is_none_or(|value| value != skill_id)
            });
            config.insert(
                "customTools".to_string(),
                serde_json::Value::Array(custom_tools),
            );
            persist_skills_config_object(&state, config)?;
            Ok(Json(serde_json::json!({
                "removed": true,
                "source": "custom",
                "skillId": skill_id,
            })))
        }
        "instruction" => {
            let mut instruction_skills = load_instruction_skills(&state);
            let removed_skill = instruction_skills
                .iter()
                .find(|item| instruction_skill_matches(item, skill_id))
                .cloned();
            let removed_skill_id = removed_skill
                .as_ref()
                .and_then(instruction_skill_id)
                .unwrap_or_else(|| skill_id.to_string());
            if removed_skill
                .as_ref()
                .is_some_and(crate::builtin_skills::is_builtin)
            {
                return Err(ApiError::InvalidInput(
                    "内置技能不能删除，如不需要可以停用".to_string(),
                ));
            }
            let removed = remove_instruction_skill_from_list(&mut instruction_skills, skill_id);
            if !removed {
                return Err(ApiError::not_found("技能未安装", skill_id));
            }
            persist_instruction_skills(&state, instruction_skills)?;
            if removed_skill
                .as_ref()
                .and_then(|skill| skill.get("source"))
                .and_then(|value| value.as_str())
                == Some("repository")
                && let Some(directory) = removed_skill
                    .as_ref()
                    .and_then(skill_loader::instruction_skill_directory_path)
                && directory.is_dir()
            {
                std::fs::remove_dir_all(&directory)
                    .map_err(|error| skill_cache_error("删除 Skill 缓存失败", &directory, error))?;
            }
            if removed_skill
                .as_ref()
                .and_then(|skill| skill.get("source"))
                .and_then(|value| value.as_str())
                == Some("repository")
                && let Ok(backup_root) = installed_skill_backup_root(&state)
                && let Ok(cache_key) = installed_skill_cache_key(&removed_skill_id)
            {
                let backup = backup_root.join(cache_key);
                if backup.exists() {
                    std::fs::remove_dir_all(&backup).map_err(|error| {
                        skill_cache_error("删除 Skill 备份失败", &backup, error)
                    })?;
                }
            }
            Ok(Json(serde_json::json!({
                "removed": true,
                "source": "instruction",
                "skillId": removed_skill_id,
            })))
        }
        other => Err(ApiError::InvalidInput(format!(
            "未知 source: {other}（应为 'custom' 或 'instruction'）"
        ))),
    }
}

pub(super) fn reload_local_skill_entry(
    skill: &serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    let path = skill_loader::instruction_skill_directory_path(skill)
        .filter(|path| path.is_dir())
        .ok_or_else(|| ApiError::InvalidInput("技能源不可用，请重新导入该 Skill".to_string()))?;
    let metadata = read_skill_directory_metadata(&path)?;
    let mut entry = skill.as_object().cloned().unwrap_or_default();
    entry.insert(
        "fileCount".to_string(),
        serde_json::json!(metadata.file_count),
    );
    entry.insert(
        "lastModified".to_string(),
        serde_json::json!(metadata.last_modified_epoch_ms),
    );
    entry.insert(
        "loadedContentHash".to_string(),
        serde_json::json!(metadata.content_hash),
    );
    entry.insert(
        "lastRefreshed".to_string(),
        serde_json::json!(epoch_ms_now()),
    );
    entry.insert("updatedAt".to_string(), serde_json::json!(epoch_ms_now()));
    Ok(serde_json::Value::Object(entry))
}

pub(super) async fn refresh_available_repository_skill(
    state: &ApiState,
    installed: &serde_json::Value,
) -> Result<AvailableRepositorySkill, ApiError> {
    let skill_id = instruction_skill_id(installed)
        .ok_or_else(|| ApiError::InvalidInput("skillId 不能为空".to_string()))?;
    let repository_url = installed
        .get("repositoryUrl")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("Skill 仓库来源不可用".to_string()))?;
    let repository_id = installed
        .get("repositoryId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(repository_url);
    let repository_entry = state
        .settings_store
        .get_section("repositories")
        .as_array()
        .and_then(|repositories| {
            repositories.iter().find(|repository| {
                repository
                    .get("repositoryId")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| value == repository_id)
            })
        })
        .cloned()
        .ok_or_else(|| ApiError::InvalidInput("Skill 来源仓库未配置".to_string()))?;
    let snapshot = load_repository_snapshot(state, &repository_entry, true).await?;
    let skill = snapshot
        .skills
        .iter()
        .find(|candidate| candidate.skill_id == skill_id)
        .cloned()
        .ok_or_else(|| ApiError::InvalidInput("该 Skill 已从来源仓库移除".to_string()))?;
    Ok(AvailableRepositorySkill {
        skill,
        repository_root: snapshot.root,
        repository_id: repository_id.to_string(),
        repository_url: repository_url.to_string(),
        commit: snapshot.commit,
        checked_at: snapshot.checked_at,
    })
}

pub(super) fn apply_repository_skill_update(
    state: &ApiState,
    installed: &serde_json::Value,
    available: &AvailableRepositorySkill,
) -> Result<(serde_json::Value, bool), ApiError> {
    let skill_id = instruction_skill_id(installed)
        .ok_or_else(|| ApiError::InvalidInput("skillId 不能为空".to_string()))?;
    let target = skill_loader::instruction_skill_directory_path(installed)
        .ok_or_else(|| ApiError::InvalidInput("Skill 安装目录不可用".to_string()))?;
    let source = available
        .repository_root
        .join(&available.skill.relative_path);
    let backup = installed_skill_backup_root(state)?.join(installed_skill_cache_key(&skill_id)?);
    let installed_hash = installed
        .get("installedContentHash")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            installed
                .get("loadedContentHash")
                .and_then(serde_json::Value::as_str)
        });
    let current_hash = target
        .is_dir()
        .then(|| read_skill_directory_metadata(&target))
        .transpose()?
        .map(|metadata| metadata.content_hash);
    if installed_hash == Some(available.skill.content_hash.as_str())
        && current_hash.as_deref() == installed_hash
    {
        let mut unchanged = installed.as_object().cloned().unwrap_or_default();
        unchanged.insert(
            "lastCheckedAt".to_string(),
            serde_json::json!(available.checked_at),
        );
        return Ok((serde_json::Value::Object(unchanged), false));
    }

    update_repository_skill_directory(&source, &target, &backup)?;
    let metadata = read_skill_directory_metadata(&target)?;
    let mut entry = installed.as_object().cloned().unwrap_or_default();
    for (source_key, backup_key) in [
        ("installedContentHash", "backupContentHash"),
        ("installedCommit", "backupCommit"),
        ("version", "backupVersion"),
        ("name", "backupName"),
        ("description", "backupDescription"),
        ("author", "backupAuthor"),
        ("category", "backupCategory"),
        ("updatedAt", "backupUpdatedAt"),
    ] {
        if let Some(value) = entry.get(source_key).cloned() {
            entry.insert(backup_key.to_string(), value);
        }
    }
    entry.insert("name".to_string(), serde_json::json!(available.skill.name));
    entry.insert(
        "description".to_string(),
        serde_json::json!(available.skill.description),
    );
    entry.insert(
        "author".to_string(),
        serde_json::json!(available.skill.author),
    );
    entry.insert(
        "version".to_string(),
        serde_json::json!(available.skill.version),
    );
    entry.insert(
        "category".to_string(),
        serde_json::json!(available.skill.category),
    );
    entry.insert(
        "installedContentHash".to_string(),
        serde_json::json!(available.skill.content_hash),
    );
    entry.insert(
        "loadedContentHash".to_string(),
        serde_json::json!(metadata.content_hash),
    );
    entry.insert(
        "installedCommit".to_string(),
        serde_json::json!(available.commit),
    );
    entry.insert(
        "fileCount".to_string(),
        serde_json::json!(metadata.file_count),
    );
    entry.insert(
        "lastModified".to_string(),
        serde_json::json!(metadata.last_modified_epoch_ms),
    );
    entry.insert(
        "lastCheckedAt".to_string(),
        serde_json::json!(available.checked_at),
    );
    entry.insert(
        "lastRefreshed".to_string(),
        serde_json::json!(epoch_ms_now()),
    );
    entry.insert("updatedAt".to_string(), serde_json::json!(epoch_ms_now()));
    Ok((serde_json::Value::Object(entry), true))
}

pub(super) async fn check_skill_updates(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let repositories = state
        .settings_store
        .get_section("repositories")
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut refreshed_count = 0usize;
    let mut failed = Vec::new();
    for repository in repositories {
        let repository_id = repository
            .get("repositoryId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        match load_repository_snapshot(&state, &repository, true).await {
            Ok(_) => refreshed_count += 1,
            Err(error) => failed.push(serde_json::json!({
                "repositoryId": repository_id,
                "message": error.message(),
            })),
        }
    }
    let library = build_skill_library(&state, false).await;
    Ok(Json(serde_json::json!({
        "checked": true,
        "refreshedRepositoryCount": refreshed_count,
        "failedRepositories": failed,
        "updateAvailableCount": library["updateAvailableCount"],
        "skills": library["skills"],
    })))
}

pub(super) async fn update_skill(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let skill_id = request
        .get("skillId")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("skillId 不能为空".to_string()))?;
    let mut instruction_skills = load_instruction_skills(&state);
    let position = instruction_skills
        .iter()
        .position(|item| instruction_skill_matches(item, skill_id))
        .ok_or_else(|| ApiError::not_found("技能未安装", skill_id))?;
    let installed = instruction_skills[position].clone();
    let source = installed
        .get("source")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("local");
    let (updated_entry, changed, action) = if source == "repository" {
        let available = refresh_available_repository_skill(&state, &installed).await?;
        let (entry, changed) = apply_repository_skill_update(&state, &installed, &available)?;
        (entry, changed, "updated")
    } else {
        (reload_local_skill_entry(&installed)?, true, "reloaded")
    };
    instruction_skills[position] = updated_entry;
    persist_instruction_skills(&state, instruction_skills)?;
    Ok(Json(serde_json::json!({
        "updated": true,
        "changed": changed,
        "action": action,
        "skillId": skill_id,
    })))
}

pub(super) async fn update_all_skills(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let repositories = state
        .settings_store
        .get_section("repositories")
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut available_skills = HashMap::<String, AvailableRepositorySkill>::new();
    let mut failures = Vec::new();
    for repository in repositories {
        let repository_id = repository
            .get("repositoryId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        match load_repository_snapshot(&state, &repository, true).await {
            Ok(snapshot) => {
                let repository_url = repository
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                for skill in snapshot.skills {
                    available_skills.insert(
                        skill.skill_id.clone(),
                        AvailableRepositorySkill {
                            skill,
                            repository_root: snapshot.root.clone(),
                            repository_id: repository_id.clone(),
                            repository_url: repository_url.clone(),
                            commit: snapshot.commit.clone(),
                            checked_at: snapshot.checked_at,
                        },
                    );
                }
            }
            Err(error) => failures.push(serde_json::json!({
                "repositoryId": repository_id,
                "message": error.message(),
            })),
        }
    }

    let mut instruction_skills = load_instruction_skills(&state);
    let mut updated_count = 0usize;
    let mut reloaded_count = 0usize;
    let mut unchanged_count = 0usize;
    for installed in &mut instruction_skills {
        let skill_id = instruction_skill_id(installed).unwrap_or_default();
        let source = installed
            .get("source")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("local");
        let result = if source == "repository" {
            available_skills
                .get(&skill_id)
                .ok_or_else(|| ApiError::InvalidInput("该 Skill 已从来源仓库移除".to_string()))
                .and_then(|available| apply_repository_skill_update(&state, installed, available))
                .map(|(entry, changed)| {
                    if changed {
                        updated_count += 1;
                    } else {
                        unchanged_count += 1;
                    }
                    entry
                })
        } else {
            reload_local_skill_entry(installed).inspect(|_| {
                reloaded_count += 1;
            })
        };
        match result {
            Ok(entry) => *installed = entry,
            Err(error) => failures.push(serde_json::json!({
                "skillId": skill_id,
                "message": error.message(),
            })),
        }
    }
    persist_instruction_skills(&state, instruction_skills)?;
    Ok(Json(serde_json::json!({
        "updated": true,
        "updatedCount": updated_count,
        "reloadedCount": reloaded_count,
        "unchangedCount": unchanged_count,
        "failures": failures,
    })))
}

pub(super) async fn rollback_skill(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let skill_id = request
        .get("skillId")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("skillId 不能为空".to_string()))?;
    let mut instruction_skills = load_instruction_skills(&state);
    let position = instruction_skills
        .iter()
        .position(|item| instruction_skill_matches(item, skill_id))
        .ok_or_else(|| ApiError::not_found("技能未安装", skill_id))?;
    let installed = instruction_skills[position].clone();
    if installed.get("source").and_then(serde_json::Value::as_str) != Some("repository") {
        return Err(ApiError::InvalidInput(
            "本地 Skill 不支持版本回滚".to_string(),
        ));
    }
    let target = skill_loader::instruction_skill_directory_path(&installed)
        .ok_or_else(|| ApiError::InvalidInput("Skill 安装目录不可用".to_string()))?;
    let backup = installed_skill_backup_root(&state)?.join(installed_skill_cache_key(skill_id)?);
    rollback_repository_skill_directory(&target, &backup)?;
    let metadata = read_skill_directory_metadata(&target)?;
    let mut entry = installed.as_object().cloned().unwrap_or_default();
    for (current_key, backup_key) in [
        ("installedContentHash", "backupContentHash"),
        ("installedCommit", "backupCommit"),
        ("version", "backupVersion"),
        ("name", "backupName"),
        ("description", "backupDescription"),
        ("author", "backupAuthor"),
        ("category", "backupCategory"),
        ("updatedAt", "backupUpdatedAt"),
    ] {
        let current = entry.get(current_key).cloned();
        let backup_value = entry.get(backup_key).cloned();
        if let Some(value) = backup_value {
            entry.insert(current_key.to_string(), value);
        } else {
            entry.remove(current_key);
        }
        if let Some(value) = current {
            entry.insert(backup_key.to_string(), value);
        } else {
            entry.remove(backup_key);
        }
    }
    entry.insert(
        "loadedContentHash".to_string(),
        serde_json::json!(metadata.content_hash),
    );
    entry.insert(
        "fileCount".to_string(),
        serde_json::json!(metadata.file_count),
    );
    entry.insert(
        "lastModified".to_string(),
        serde_json::json!(metadata.last_modified_epoch_ms),
    );
    entry.insert(
        "lastRefreshed".to_string(),
        serde_json::json!(epoch_ms_now()),
    );
    instruction_skills[position] = serde_json::Value::Object(entry);
    persist_instruction_skills(&state, instruction_skills)?;
    Ok(Json(serde_json::json!({
        "rolledBack": true,
        "skillId": skill_id,
    })))
}

pub(super) fn skill_repository_error(
    repository_url: &str,
    action: &'static str,
    error: impl Display,
) -> ApiError {
    tracing::warn!(
        action,
        repository_url = %repository_url,
        error = %error,
        "Skill repository request failed"
    );
    ApiError::InvalidInput(SKILL_REPOSITORY_PUBLIC_ERROR.to_string())
}

pub(super) fn skill_cache_error(
    action: &'static str,
    path: &Path,
    error: impl Display,
) -> ApiError {
    tracing::warn!(
        action,
        path = %path.display(),
        error = %error,
        "Skill cache write failed"
    );
    ApiError::InvalidInput(SKILL_CACHE_PUBLIC_ERROR.to_string())
}

/// Skill 详情：说明文件全文与目录里的文件清单，供设置页“查看详情”展示。
/// 只返回相对路径与大小，不暴露本机绝对路径。
pub(super) const SKILL_DETAIL_MAX_INSTRUCTION_BYTES: usize = 256 * 1024;

pub(super) const SKILL_DETAIL_MAX_FILES: usize = 60;

pub(super) const SKILL_DETAIL_MAX_DEPTH: usize = 3;

pub(super) fn skill_detail_file_list(root: &Path) -> Vec<serde_json::Value> {
    fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(String, u64)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if out.len() >= SKILL_DETAIL_MAX_FILES {
                return;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                if depth < SKILL_DETAIL_MAX_DEPTH {
                    walk(root, &path, depth + 1, out);
                }
            } else if file_type.is_file()
                && let Ok(relative) = path.strip_prefix(root)
            {
                let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                out.push((relative.to_string_lossy().replace('\\', "/"), size));
            }
        }
    }
    let mut files = Vec::new();
    walk(root, root, 1, &mut files);
    files
        .into_iter()
        .map(|(path, size)| serde_json::json!({ "path": path, "size": size }))
        .collect()
}

pub(super) async fn get_instruction_skill_detail(
    State(state): State<ApiState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let skill_id = params
        .get("skillId")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("skillId 不能为空".to_string()))?;

    let instruction_skills = load_instruction_skills(&state);
    let matched = instruction_skills
        .iter()
        .find(|item| instruction_skill_matches(item, skill_id))
        .ok_or_else(|| ApiError::not_found("技能未安装", skill_id))?;

    let dir = skill_loader::instruction_skill_directory_path(matched)
        .ok_or_else(|| ApiError::InvalidInput("技能源不可用，请重新导入该 Skill".to_string()))?;
    let instruction = skill_loader::read_available_skill_instruction(&dir)
        .ok_or_else(|| ApiError::InvalidInput("技能源不可用，请重新导入该 Skill".to_string()))?;
    let instruction_file = ["prompt.md", "SKILL.md", "README.md"]
        .into_iter()
        .find(|name| dir.join(name).is_file())
        .unwrap_or("SKILL.md");
    let total_bytes = instruction.len();
    let truncated = total_bytes > SKILL_DETAIL_MAX_INSTRUCTION_BYTES;
    let instruction = if truncated {
        let mut end = SKILL_DETAIL_MAX_INSTRUCTION_BYTES;
        while !instruction.is_char_boundary(end) {
            end -= 1;
        }
        instruction[..end].to_string()
    } else {
        instruction
    };

    Ok(Json(serde_json::json!({
        "skillId": skill_id,
        "instructionFile": instruction_file,
        "instruction": instruction,
        "instructionBytes": total_bytes,
        "truncated": truncated,
        "files": skill_detail_file_list(&dir),
    })))
}
