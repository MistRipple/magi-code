use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SkillDirectoryMetadata {
    pub(super) file_count: usize,
    pub(super) last_modified_epoch_ms: Option<u64>,
    pub(super) content_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GithubRepositoryRef {
    pub(super) owner: String,
    pub(super) name: String,
    pub(super) canonical_url: String,
}

impl GithubRepositoryRef {
    pub(super) fn clone_url(&self) -> String {
        format!("{}.git", self.canonical_url)
    }

    pub(super) fn cache_key(&self) -> String {
        format!("{}__{}", self.owner, self.name)
    }

    pub(super) fn skill_id(&self, relative_path: &Path) -> Result<String, ApiError> {
        let relative = relative_path_to_slash_string(relative_path)?;
        if relative.is_empty() {
            Ok(format!("{}/{}", self.owner, self.name))
        } else {
            Ok(format!("{}/{}/{}", self.owner, self.name, relative))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RepositorySkill {
    pub(super) skill_id: String,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) author: Option<String>,
    pub(super) version: Option<String>,
    pub(super) category: Option<String>,
    pub(super) content_hash: String,
    pub(super) relative_path: PathBuf,
}

pub(super) fn parse_github_repository_url(raw_url: &str) -> Result<GithubRepositoryRef, ApiError> {
    let parsed = reqwest::Url::parse(raw_url.trim())
        .map_err(|_| ApiError::InvalidInput(SKILL_REPOSITORY_URL_PUBLIC_ERROR.to_string()))?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("github.com")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(ApiError::InvalidInput(
            SKILL_REPOSITORY_URL_PUBLIC_ERROR.to_string(),
        ));
    }

    let segments = parsed
        .path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if segments.len() != 2 {
        return Err(ApiError::InvalidInput(
            SKILL_REPOSITORY_URL_PUBLIC_ERROR.to_string(),
        ));
    }
    let owner = segments[0].trim();
    let name = segments[1].trim_end_matches(".git").trim();
    if !is_safe_github_repository_segment(owner) || !is_safe_github_repository_segment(name) {
        return Err(ApiError::InvalidInput(
            SKILL_REPOSITORY_URL_PUBLIC_ERROR.to_string(),
        ));
    }

    Ok(GithubRepositoryRef {
        owner: owner.to_string(),
        name: name.to_string(),
        canonical_url: format!("https://github.com/{owner}/{name}"),
    })
}

pub(super) fn is_safe_github_repository_segment(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
}

pub(super) fn relative_path_to_slash_string(path: &Path) -> Result<String, ApiError> {
    let mut segments = Vec::new();
    for component in path.components() {
        let std::path::Component::Normal(segment) = component else {
            return Err(ApiError::InvalidInput("Skill 仓库目录结构无效".to_string()));
        };
        let value = segment
            .to_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ApiError::InvalidInput("Skill 仓库目录结构无效".to_string()))?;
        segments.push(value);
    }
    Ok(segments.join("/"))
}

pub(super) fn read_skill_frontmatter_value(dir: &Path, key: &str) -> Option<String> {
    let content = std::fs::read_to_string(dir.join("SKILL.md")).ok()?;
    let mut lines = content.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let prefix = format!("{key}:");
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some(value) = trimmed.strip_prefix(&prefix) {
            let value = value
                .trim()
                .trim_matches(|character| character == '"' || character == '\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

pub(super) fn scan_cached_repository_skills(
    repository_root: &Path,
    repository: &GithubRepositoryRef,
) -> Result<Vec<RepositorySkill>, ApiError> {
    if !repository_root.is_dir() {
        return Err(ApiError::InvalidInput(
            SKILL_REPOSITORY_PUBLIC_ERROR.to_string(),
        ));
    }
    let mut skills = Vec::new();
    collect_repository_skills(repository_root, repository_root, repository, 0, &mut skills)?;
    skills.sort_by(|left, right| left.skill_id.cmp(&right.skill_id));
    Ok(skills)
}

pub(super) fn collect_repository_skills(
    repository_root: &Path,
    current_dir: &Path,
    repository: &GithubRepositoryRef,
    depth: usize,
    skills: &mut Vec<RepositorySkill>,
) -> Result<(), ApiError> {
    if depth > 8 {
        return Ok(());
    }
    let skill_file = current_dir.join("SKILL.md");
    if skill_file.is_file() {
        let relative_path = current_dir
            .strip_prefix(repository_root)
            .map_err(|_| ApiError::InvalidInput("Skill 仓库目录结构无效".to_string()))?
            .to_path_buf();
        let directory_name = current_dir
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(&repository.name)
            .to_string();
        let metadata = read_skill_directory_metadata(current_dir)?;
        skills.push(RepositorySkill {
            skill_id: repository.skill_id(&relative_path)?,
            name: read_skill_frontmatter_value(current_dir, "name").unwrap_or(directory_name),
            description: read_skill_frontmatter_value(current_dir, "description")
                .unwrap_or_else(|| skill_loader::read_skill_description(current_dir)),
            author: read_skill_frontmatter_value(current_dir, "author"),
            version: read_skill_frontmatter_value(current_dir, "version"),
            category: read_skill_frontmatter_value(current_dir, "category"),
            content_hash: metadata.content_hash,
            relative_path,
        });
        return Ok(());
    }

    let mut entries = std::fs::read_dir(current_dir)
        .map_err(|error| skill_cache_error("读取 Skill 仓库目录失败", current_dir, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| skill_cache_error("读取 Skill 仓库目录失败", current_dir, error))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || matches!(name.as_ref(), "node_modules" | "target") {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| skill_cache_error("读取 Skill 仓库条目失败", &entry.path(), error))?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_repository_skills(
                repository_root,
                &entry.path(),
                repository,
                depth + 1,
                skills,
            )?;
        }
    }
    Ok(())
}

pub(super) fn copy_repository_skill_directory(
    source: &Path,
    target: &Path,
) -> Result<(), ApiError> {
    if !source.is_dir() || !source.join("SKILL.md").is_file() {
        return Err(ApiError::InvalidInput(
            "该 Skill 已不在仓库中，请刷新 Skill 库".to_string(),
        ));
    }
    let parent = target
        .parent()
        .ok_or_else(|| ApiError::InvalidInput(SKILL_CACHE_PUBLIC_ERROR.to_string()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| skill_cache_error("创建 Skill 缓存目录失败", parent, error))?;
    let file_name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("skill");
    let staging = parent.join(format!(
        ".{file_name}.installing-{}-{}",
        std::process::id(),
        epoch_ms_now()
    ));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|error| skill_cache_error("清理 Skill 临时目录失败", &staging, error))?;
    }
    if let Err(error) = copy_directory_tree(source, &staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    replace_directory_atomically(&staging, target, "替换 Skill 缓存失败")
}

pub(super) fn update_repository_skill_directory(
    source: &Path,
    target: &Path,
    backup: &Path,
) -> Result<(), ApiError> {
    if !source.is_dir() || !source.join("SKILL.md").is_file() {
        return Err(ApiError::InvalidInput(
            "该 Skill 已不在仓库中，请刷新 Skill 库".to_string(),
        ));
    }
    let parent = target
        .parent()
        .ok_or_else(|| ApiError::InvalidInput(SKILL_CACHE_PUBLIC_ERROR.to_string()))?;
    let staging = parent.join(format!(
        ".{}.updating-{}-{}",
        target
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("skill"),
        std::process::id(),
        epoch_ms_now()
    ));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|error| skill_cache_error("清理 Skill 更新临时目录失败", &staging, error))?;
    }
    if let Err(error) = copy_directory_tree(source, &staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }

    if let Some(backup_parent) = backup.parent() {
        std::fs::create_dir_all(backup_parent)
            .map_err(|error| skill_cache_error("创建 Skill 备份目录失败", backup_parent, error))?;
    }
    if backup.exists() {
        std::fs::remove_dir_all(backup)
            .map_err(|error| skill_cache_error("清理旧 Skill 备份失败", backup, error))?;
    }
    if target.exists() {
        std::fs::rename(target, backup)
            .map_err(|error| skill_cache_error("备份当前 Skill 失败", target, error))?;
    }
    if let Err(error) = std::fs::rename(&staging, target) {
        if backup.exists() {
            let _ = std::fs::rename(backup, target);
        }
        return Err(skill_cache_error("替换 Skill 缓存失败", target, error));
    }
    Ok(())
}

pub(super) fn rollback_repository_skill_directory(
    target: &Path,
    backup: &Path,
) -> Result<(), ApiError> {
    if !backup.is_dir() {
        return Err(ApiError::InvalidInput(
            "该 Skill 没有可回滚版本".to_string(),
        ));
    }
    let parent = target
        .parent()
        .ok_or_else(|| ApiError::InvalidInput(SKILL_CACHE_PUBLIC_ERROR.to_string()))?;
    let current = parent.join(format!(
        ".{}.rollback-{}-{}",
        target
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("skill"),
        std::process::id(),
        epoch_ms_now()
    ));
    if target.exists() {
        std::fs::rename(target, &current)
            .map_err(|error| skill_cache_error("准备 Skill 回滚失败", target, error))?;
    }
    if let Err(error) = std::fs::rename(backup, target) {
        if current.exists() {
            let _ = std::fs::rename(&current, target);
        }
        return Err(skill_cache_error("恢复 Skill 备份失败", target, error));
    }
    if current.exists() {
        std::fs::rename(&current, backup)
            .map_err(|error| skill_cache_error("保留 Skill 回滚版本失败", backup, error))?;
    }
    Ok(())
}

pub(super) fn copy_directory_tree(source: &Path, target: &Path) -> Result<(), ApiError> {
    std::fs::create_dir_all(target)
        .map_err(|error| skill_cache_error("创建 Skill 目录失败", target, error))?;
    let entries = std::fs::read_dir(source)
        .map_err(|error| skill_cache_error("读取 Skill 目录失败", source, error))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| skill_cache_error("读取 Skill 目录条目失败", source, error))?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|error| skill_cache_error("读取 Skill 文件类型失败", &source_path, error))?;
        if file_type.is_symlink() {
            return Err(ApiError::InvalidInput(
                "Skill 包含不支持的符号链接".to_string(),
            ));
        }
        if file_type.is_dir() {
            copy_directory_tree(&source_path, &target_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&source_path, &target_path)
                .map_err(|error| skill_cache_error("复制 Skill 文件失败", &target_path, error))?;
        }
    }
    Ok(())
}

pub(super) fn replace_directory_atomically(
    staging: &Path,
    target: &Path,
    action: &'static str,
) -> Result<(), ApiError> {
    let parent = target
        .parent()
        .ok_or_else(|| ApiError::InvalidInput(SKILL_CACHE_PUBLIC_ERROR.to_string()))?;
    let backup = parent.join(format!(
        ".{}.backup-{}-{}",
        target
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("cache"),
        std::process::id(),
        epoch_ms_now()
    ));
    if target.exists() {
        std::fs::rename(target, &backup)
            .map_err(|error| skill_cache_error(action, target, error))?;
    }
    if let Err(error) = std::fs::rename(staging, target) {
        if backup.exists() {
            let _ = std::fs::rename(&backup, target);
        }
        return Err(skill_cache_error(action, target, error));
    }
    if backup.exists() {
        std::fs::remove_dir_all(&backup)
            .map_err(|error| skill_cache_error("清理旧 Skill 缓存失败", &backup, error))?;
    }
    Ok(())
}

pub(super) fn read_skill_directory_metadata(
    dir: &Path,
) -> Result<SkillDirectoryMetadata, ApiError> {
    if !dir.is_dir() {
        return Err(ApiError::InvalidInput(
            "技能源不可用，请重新导入该 Skill".to_string(),
        ));
    }

    let mut files = Vec::new();
    collect_skill_directory_files(dir, dir, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));

    let mut hasher = Sha256::new();
    let mut latest_modified: Option<std::time::SystemTime> = None;
    for (relative_path, absolute_path, modified) in &files {
        let relative = relative_path_to_slash_string(relative_path)?;
        hasher.update((relative.len() as u64).to_le_bytes());
        hasher.update(relative.as_bytes());

        let mut file = std::fs::File::open(absolute_path)
            .map_err(|error| skill_cache_error("读取 Skill 文件失败", absolute_path, error))?;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|error| skill_cache_error("读取 Skill 文件失败", absolute_path, error))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        latest_modified =
            Some(latest_modified.map_or(*modified, |previous| previous.max(*modified)));
    }

    Ok(SkillDirectoryMetadata {
        file_count: files.len(),
        last_modified_epoch_ms: latest_modified.and_then(|time| {
            time.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|duration| duration.as_millis() as u64)
        }),
        content_hash: magi_core::hex_lower(hasher.finalize()),
    })
}

pub(super) fn collect_skill_directory_files(
    root: &Path,
    current: &Path,
    files: &mut Vec<(PathBuf, PathBuf, std::time::SystemTime)>,
) -> Result<(), ApiError> {
    let mut entries = std::fs::read_dir(current)
        .map_err(|error| skill_cache_error("读取 Skill 目录失败", current, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| skill_cache_error("读取 Skill 目录失败", current, error))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        if entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| skill_cache_error("读取 Skill 文件类型失败", &path, error))?;
        if file_type.is_symlink() {
            return Err(ApiError::InvalidInput(
                "Skill 包含不支持的符号链接".to_string(),
            ));
        }
        if file_type.is_dir() {
            collect_skill_directory_files(root, &path, files)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|error| skill_cache_error("读取 Skill 文件元数据失败", &path, error))?;
        let relative = path
            .strip_prefix(root)
            .map_err(|_| ApiError::InvalidInput("Skill 目录结构无效".to_string()))?
            .to_path_buf();
        files.push((
            relative,
            path,
            metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
        ));
    }
    Ok(())
}

pub(super) fn normalize_repository_entry(
    request: &serde_json::Value,
    allow_updates: bool,
) -> Result<serde_json::Value, ApiError> {
    let mut entry = request.as_object().cloned().unwrap_or_default();
    if allow_updates
        && let Some(updates) = request.get("updates").and_then(|value| value.as_object())
    {
        for (key, value) in updates {
            entry.insert(key.clone(), value.clone());
        }
    }
    let repository_url = entry
        .get("url")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(parse_github_repository_url)
        .transpose()?
        .map(|repository| repository.canonical_url);
    let repository_id = request
        .get("repositoryId")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| repository_url.clone())
        .ok_or_else(|| ApiError::InvalidInput("repositoryId 或 url 不能为空".to_string()))?;
    strip_scope_binding_fields_from_map(&mut entry);
    entry.remove("updates");
    entry.insert("repositoryId".to_string(), serde_json::json!(repository_id));
    if let Some(url) = repository_url {
        entry.insert("url".to_string(), serde_json::json!(url));
    }
    Ok(serde_json::Value::Object(entry))
}

pub(super) fn upsert_named_object_array_entry(
    items: &mut Vec<serde_json::Value>,
    entry: serde_json::Value,
    field_names: &[&str],
) {
    let Some(entry_name) = field_names
        .iter()
        .find_map(|field| entry.get(*field).and_then(|value| value.as_str()))
        .map(str::to_string)
    else {
        items.push(entry);
        return;
    };
    if let Some(position) = items.iter().position(|item| {
        field_names.iter().any(|field| {
            item.get(*field)
                .and_then(|value| value.as_str())
                .is_some_and(|value| value == entry_name)
        })
    }) {
        items[position] = entry;
    } else {
        items.push(entry);
    }
}
