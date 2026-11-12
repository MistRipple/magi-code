use super::*;

pub(super) fn skill_repository_cache_root(state: &ApiState) -> Result<PathBuf, ApiError> {
    state
        .runtime_persistence()
        .and_then(|persistence| persistence.state_root())
        .map(|root| root.join("skill_repositories"))
        .ok_or_else(|| ApiError::InvalidInput(SKILL_CACHE_PUBLIC_ERROR.to_string()))
}

pub(super) fn installed_skill_cache_root(state: &ApiState) -> Result<PathBuf, ApiError> {
    state
        .runtime_persistence()
        .and_then(|persistence| persistence.state_root())
        .map(|root| root.join("skills_cache"))
        .ok_or_else(|| ApiError::InvalidInput(SKILL_CACHE_PUBLIC_ERROR.to_string()))
}

pub(super) fn installed_skill_backup_root(state: &ApiState) -> Result<PathBuf, ApiError> {
    state
        .runtime_persistence()
        .and_then(|persistence| persistence.state_root())
        .map(|root| root.join("skills_backups"))
        .ok_or_else(|| ApiError::InvalidInput(SKILL_CACHE_PUBLIC_ERROR.to_string()))
}

pub(super) fn installed_skill_cache_key(skill_id: &str) -> Result<String, ApiError> {
    if skill_id.trim().is_empty() {
        return Err(ApiError::InvalidInput("skillId 无效".to_string()));
    }
    Ok(urlencoding::encode(skill_id).into_owned())
}

pub(super) async fn ensure_cached_github_repository(
    state: &ApiState,
    repository_url: &str,
    force_refresh: bool,
) -> Result<(GithubRepositoryRef, PathBuf), ApiError> {
    let repository = parse_github_repository_url(repository_url)?;
    let cache_root = skill_repository_cache_root(state)?;
    let target = cache_root.join(repository.cache_key());
    let sync_lock = SKILL_REPOSITORY_SYNC_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _guard = sync_lock.lock().await;
    if !force_refresh && target.join(".git").is_dir() {
        return Ok((repository, target));
    }

    std::fs::create_dir_all(&cache_root)
        .map_err(|error| skill_cache_error("创建 Skill 仓库缓存目录失败", &cache_root, error))?;
    let staging = cache_root.join(format!(
        ".{}.syncing-{}-{}",
        repository.cache_key(),
        std::process::id(),
        epoch_ms_now()
    ));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|error| skill_cache_error("清理 Skill 仓库临时目录失败", &staging, error))?;
    }

    let clone_future = magi_process::tokio_command("git")
        .args(["clone", "--depth", "1", "--no-tags", "--quiet", "--"])
        .arg(repository.clone_url())
        .arg(&staging)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output();
    let output = tokio::time::timeout(std::time::Duration::from_secs(90), clone_future)
        .await
        .map_err(|_| {
            tracing::warn!(
                repository_url = %repository.canonical_url,
                "Skill repository clone timed out"
            );
            ApiError::InvalidInput(SKILL_REPOSITORY_PUBLIC_ERROR.to_string())
        })?
        .map_err(|error| {
            skill_repository_error(
                &repository.canonical_url,
                "启动 GitHub Skill 仓库同步失败",
                error,
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            repository_url = %repository.canonical_url,
            status = ?output.status.code(),
            error = %stderr.trim(),
            "GitHub Skill repository clone failed"
        );
        let _ = std::fs::remove_dir_all(&staging);
        return Err(ApiError::InvalidInput(
            SKILL_REPOSITORY_PUBLIC_ERROR.to_string(),
        ));
    }
    replace_directory_atomically(&staging, &target, "替换 Skill 仓库缓存失败")?;
    Ok((repository, target))
}

pub(super) async fn cached_repository_commit(repository_root: &Path) -> Option<String> {
    let output = magi_process::tokio_command("git")
        .arg("-C")
        .arg(repository_root)
        .args(["rev-parse", "HEAD"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!commit.is_empty()).then_some(commit)
}

pub(super) fn repository_skill_public_entry(
    skill: RepositorySkill,
    repository_id: &str,
    repository_url: &str,
    installed: bool,
    available_commit: Option<&str>,
    last_checked_at: Option<u64>,
) -> serde_json::Value {
    let available_version = skill.version.clone();
    let available_content_hash = skill.content_hash.clone();
    serde_json::json!({
        "name": skill.name,
        "skillId": skill.skill_id,
        "fullName": skill.skill_id,
        "description": skill.description,
        "source": "repository",
        "repositoryId": repository_id,
        "repositoryName": repository_url,
        "repositoryPath": relative_path_to_slash_string(&skill.relative_path).unwrap_or_default(),
        "installed": installed,
        "author": skill.author,
        "version": skill.version,
        "category": skill.category,
        "availableVersion": available_version,
        "availableContentHash": available_content_hash,
        "availableCommit": available_commit,
        "lastCheckedAt": last_checked_at,
        "sourceStatus": "available",
        "updateStatus": if installed { "up_to_date" } else { "not_installed" },
        "updateAvailable": false,
    })
}

#[derive(Clone, Debug)]
pub(super) struct CachedRepositorySnapshot {
    pub(super) root: PathBuf,
    pub(super) skills: Vec<RepositorySkill>,
    pub(super) commit: Option<String>,
    pub(super) checked_at: u64,
}

pub(super) fn repository_update_check_is_stale(entry: &serde_json::Value, now: u64) -> bool {
    entry
        .get("lastCheckedAt")
        .and_then(serde_json::Value::as_u64)
        .is_none_or(|last_checked| now.saturating_sub(last_checked) >= SKILL_UPDATE_CHECK_TTL_MS)
}

pub(super) async fn load_repository_snapshot(
    state: &ApiState,
    entry: &serde_json::Value,
    force_refresh: bool,
) -> Result<CachedRepositorySnapshot, ApiError> {
    let repository_url = entry
        .get("url")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("仓库 URL 不能为空".to_string()))?;
    let repository_id = entry
        .get("repositoryId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(repository_url);
    let now = epoch_ms_now();
    let previous_checked_at = entry
        .get("lastCheckedAt")
        .and_then(serde_json::Value::as_u64);
    let checked_at = if force_refresh || previous_checked_at.is_none() {
        now
    } else {
        previous_checked_at.unwrap_or(now)
    };
    let (repository, root) =
        ensure_cached_github_repository(state, repository_url, force_refresh).await?;
    let skills = scan_cached_repository_skills(&root, &repository)?;
    let commit = cached_repository_commit(&root).await;

    let mut updated = entry.as_object().cloned().unwrap_or_default();
    updated.insert("repositoryId".to_string(), serde_json::json!(repository_id));
    updated.insert(
        "url".to_string(),
        serde_json::json!(repository.canonical_url.clone()),
    );
    updated.insert("skillCount".to_string(), serde_json::json!(skills.len()));
    updated.insert("lastCheckedAt".to_string(), serde_json::json!(checked_at));
    if force_refresh || previous_checked_at.is_none() {
        updated.insert("lastRefreshed".to_string(), serde_json::json!(checked_at));
    }
    updated.insert("syncStatus".to_string(), serde_json::json!("ready"));
    if let Some(commit) = &commit {
        updated.insert("commit".to_string(), serde_json::json!(commit));
    }
    state
        .settings_store
        .upsert_array_entry(
            "repositories",
            "repositoryId",
            &serde_json::Value::Object(updated),
        )
        .map_err(settings_persistence_error)?;

    Ok(CachedRepositorySnapshot {
        root,
        skills,
        commit,
        checked_at,
    })
}

pub(super) async fn find_repository_skill(
    state: &ApiState,
    skill_id: &str,
) -> Result<(GithubRepositoryRef, PathBuf, RepositorySkill, String), ApiError> {
    let repositories = state
        .settings_store
        .get_section("repositories")
        .as_array()
        .cloned()
        .unwrap_or_default();
    for entry in repositories {
        let repository_url = entry
            .get("url")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if repository_url.is_empty() {
            continue;
        }
        let parsed = parse_github_repository_url(repository_url)?;
        let prefix = format!("{}/{}/", parsed.owner, parsed.name);
        if skill_id != format!("{}/{}", parsed.owner, parsed.name) && !skill_id.starts_with(&prefix)
        {
            continue;
        }
        let repository_id = entry
            .get("repositoryId")
            .and_then(|value| value.as_str())
            .unwrap_or(repository_url)
            .to_string();
        let (repository, root) =
            ensure_cached_github_repository(state, repository_url, false).await?;
        let skill = scan_cached_repository_skills(&root, &repository)?
            .into_iter()
            .find(|skill| skill.skill_id == skill_id)
            .ok_or_else(|| {
                ApiError::InvalidInput("该 Skill 已不在仓库中，请刷新 Skill 库".to_string())
            })?;
        return Ok((repository, root, skill, repository_id));
    }
    Err(ApiError::InvalidInput(
        "该 Skill 不属于已配置的仓库".to_string(),
    ))
}

pub(super) async fn list_repositories(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let stored_repos = state
        .settings_store
        .get_section("repositories")
        .as_array()
        .cloned()
        .unwrap_or_default();
    let repos = stored_repos
        .iter()
        .cloned()
        .map(clean_repository_scope_fields)
        .collect::<Vec<_>>();
    Json(serde_json::json!({ "repositories": repos }))
}

pub(super) fn clean_repository_scope_fields(mut entry: serde_json::Value) -> serde_json::Value {
    if let Some(object) = entry.as_object_mut() {
        strip_scope_binding_fields_from_map(object);
    }
    entry
}

pub(super) async fn add_repository(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let normalized = normalize_repository_entry(&request, false)?;
    state
        .settings_store
        .upsert_array_entry("repositories", "repositoryId", &normalized)
        .map_err(settings_persistence_error)?;
    Ok(Json(serde_json::json!({ "added": true })))
}

pub(super) async fn update_repository(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let normalized = normalize_repository_entry(&request, true)?;
    let repository_id = normalized
        .get("repositoryId")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let previous_url = state
        .settings_store
        .get_section("repositories")
        .as_array()
        .and_then(|repositories| {
            repositories.iter().find(|repository| {
                repository
                    .get("repositoryId")
                    .and_then(|value| value.as_str())
                    .is_some_and(|value| value == repository_id)
            })
        })
        .and_then(|repository| repository.get("url"))
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);
    state
        .settings_store
        .upsert_array_entry("repositories", "repositoryId", &normalized)
        .map_err(settings_persistence_error)?;
    if let Some(previous_url) = previous_url
        && normalized
            .get("url")
            .and_then(|value| value.as_str())
            .is_some_and(|url| url != previous_url)
        && let Ok(previous_repository) = parse_github_repository_url(&previous_url)
        && let Ok(cache_root) = skill_repository_cache_root(&state)
    {
        let _ = std::fs::remove_dir_all(cache_root.join(previous_repository.cache_key()));
    }
    Ok(Json(serde_json::json!({ "updated": true })))
}

pub(super) async fn delete_repository(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let repo_id = request
        .get("repositoryId")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let repository_url = state
        .settings_store
        .get_section("repositories")
        .as_array()
        .and_then(|repositories| {
            repositories.iter().find(|repository| {
                repository
                    .get("repositoryId")
                    .and_then(|value| value.as_str())
                    .is_some_and(|value| value == repo_id)
            })
        })
        .and_then(|repository| repository.get("url"))
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);
    state
        .settings_store
        .remove_array_entry("repositories", "repositoryId", repo_id)
        .map_err(settings_persistence_error)?;
    if let Some(repository_url) = repository_url
        && let Ok(repository) = parse_github_repository_url(&repository_url)
        && let Ok(cache_root) = skill_repository_cache_root(&state)
    {
        let _ = std::fs::remove_dir_all(cache_root.join(repository.cache_key()));
    }
    Ok(Json(serde_json::json!({ "deleted": true })))
}

pub(super) async fn refresh_repository(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let repo_id = request
        .get("repositoryId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("repositoryId 不能为空".to_string()))?;

    let repos = state
        .settings_store
        .get_section("repositories")
        .as_array()
        .cloned()
        .unwrap_or_default();

    let position = repos.iter().position(|item| {
        item.get("repositoryId")
            .and_then(|v| v.as_str())
            .is_some_and(|v| v == repo_id)
    });

    let Some(pos) = position else {
        return Err(ApiError::not_found("仓库不存在", repo_id));
    };

    let snapshot = load_repository_snapshot(&state, &repos[pos], true).await?;

    Ok(Json(serde_json::json!({
        "refreshed": true,
        "skillCount": snapshot.skills.len(),
        "commit": snapshot.commit,
        "lastCheckedAt": snapshot.checked_at,
    })))
}
