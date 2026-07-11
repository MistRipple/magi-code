use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_bridge_client::{McpToolInfo, StdioMcpBridgeClient};
use std::collections::HashMap;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::{
    errors::ApiError,
    mcp_config::{
        build_mcp_config_from_entry, mcp_server_entry_enabled, mcp_server_entry_id,
        normalize_mcp_server_request_entry as normalize_mcp_server_entry,
        normalize_mcp_server_snapshot_entry, preserve_redacted_mcp_env_values,
        redact_mcp_server_public_entry,
    },
    scope_binding::strip_scope_binding_fields_from_map,
    skill_loader,
    state::ApiState,
};

const MCP_CONNECTION_FAILED_MARKER: &str = "mcp_connection_failed";
const MCP_DISABLED_HEALTH: &str = "disabled";
const MCP_DISCONNECTED_HEALTH: &str = "disconnected";
const SKILL_REPOSITORY_PUBLIC_ERROR: &str = "Skill 仓库暂不可读取";
const SKILL_CACHE_PUBLIC_ERROR: &str = "Skill 缓存不可保存，请检查本地权限";
const SKILL_REPOSITORY_URL_PUBLIC_ERROR: &str = "仅支持标准 GitHub HTTPS 仓库地址";
static SKILL_REPOSITORY_SYNC_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

async fn list_mcp_tools(client: Arc<StdioMcpBridgeClient>) -> Result<Vec<McpToolInfo>, String> {
    tokio::task::spawn_blocking(move || client.list_tools())
        .await
        .map_err(|error| format!("MCP worker join failed: {error}"))?
        .map_err(|error| error.to_string())
}

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/settings/mcp", get(list_mcp_servers))
        .route("/settings/mcp/add", post(add_mcp_server))
        .route("/settings/mcp/update", post(update_mcp_server))
        .route("/settings/mcp/delete", post(delete_mcp_server))
        .route("/settings/mcp/tools", post(get_mcp_tools))
        .route("/settings/mcp/tools/refresh", post(refresh_mcp_tools))
        .route("/settings/mcp/connect", post(connect_mcp_server))
        .route("/settings/mcp/disconnect", post(disconnect_mcp_server))
        .route("/settings/repositories", get(list_repositories))
        .route("/settings/repositories/add", post(add_repository))
        .route("/settings/repositories/update", post(update_repository))
        .route("/settings/repositories/delete", post(delete_repository))
        .route("/settings/repositories/refresh", post(refresh_repository))
        .route("/settings/skills/library", get(list_skills))
        .route("/settings/skills/install", post(install_skill))
        .route("/settings/skills/install-local", post(install_local_skill))
        .route(
            "/settings/skills/instruction-preview",
            get(get_instruction_skill_preview),
        )
        .route(
            "/settings/skills/scan-local",
            post(scan_local_skill_directory),
        )
        .route("/settings/skills/config/save", post(save_skills_config))
        .route("/settings/skills/custom-tool/add", post(add_custom_tool))
        .route("/settings/skills/remove", post(remove_installed_skill))
        .route("/settings/skills/update", post(update_skill))
        .route("/settings/skills/update-all", post(update_all_skills))
}

fn load_skills_config_object(state: &ApiState) -> serde_json::Map<String, serde_json::Value> {
    skill_loader::skills_config_object(&state.settings_store)
}

fn persist_skills_config_object(
    state: &ApiState,
    config: serde_json::Map<String, serde_json::Value>,
) {
    skill_loader::save_skills_config_object(&state.settings_store, config);
    reload_skill_registry(state);
}

fn reload_skill_registry(state: &ApiState) {
    if let Some(ref skill_rt) = state.skill_runtime {
        skill_loader::reload_skill_runtime_from_settings(skill_rt, &state.settings_store);
    }
}

fn load_instruction_skills(state: &ApiState) -> Vec<serde_json::Value> {
    load_skills_config_object(state)
        .remove("instructionSkills")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
}

fn persist_instruction_skills(state: &ApiState, instruction_skills: Vec<serde_json::Value>) {
    let mut config = load_skills_config_object(state);
    config.insert(
        "instructionSkills".to_string(),
        serde_json::Value::Array(instruction_skills),
    );
    persist_skills_config_object(state, config);
}

fn normalize_instruction_skill_entry(
    request: &serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    if request.get("skill").is_some() || request.get("updates").is_some() {
        return Err(ApiError::InvalidInput(
            "Skill 安装请求必须使用顶层 skillId，不能包裹在 skill/updates 中".to_string(),
        ));
    }
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

fn is_local_skill_dir(dir: &std::path::Path) -> bool {
    dir.join("prompt.md").is_file()
        || dir.join("SKILL.md").is_file()
        || dir.join("README.md").is_file()
        || dir.join("config.json").is_file()
}

fn build_local_instruction_skill_entry(
    dir: &std::path::Path,
) -> Result<serde_json::Value, ApiError> {
    let directory_path = dir
        .to_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("本地 Skill 目录不可用".to_string()))?;
    let skill_name = dir
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("本地 Skill 名称不可用".to_string()))?;
    let metadata = read_local_skill_metadata(dir);
    let description = read_local_skill_description(dir);
    Ok(serde_json::json!({
        "name": skill_name,
        "skillId": skill_name,
        "fullName": skill_name,
        "directoryPath": directory_path,
        "description": description,
        "source": "local",
        "fileCount": metadata.file_count,
        "lastRefreshed": epoch_ms_now(),
        "lastModified": metadata.last_modified_epoch_ms,
    }))
}

fn read_local_skill_description(dir: &std::path::Path) -> String {
    let config_path = dir.join("config.json");
    if config_path.is_file()
        && let Ok(content) = std::fs::read_to_string(&config_path)
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(desc) = json.get("description").and_then(|v| v.as_str())
    {
        let desc = desc.trim();
        if !desc.is_empty() {
            return desc.chars().take(200).collect();
        }
    }
    for filename in &["SKILL.md", "prompt.md", "README.md"] {
        let path = dir.join(filename);
        if path.is_file()
            && let Ok(content) = std::fs::read_to_string(&path)
        {
            let mut lines = content.lines();
            let first_line = lines.next();
            if first_line.is_some_and(|line| line.trim() == "---") {
                for line in lines.by_ref() {
                    let trimmed = line.trim();
                    if trimmed == "---" {
                        break;
                    }
                    if let Some(description) = trimmed.strip_prefix("description:") {
                        let description = description
                            .trim()
                            .trim_matches(|ch| ch == '"' || ch == '\'');
                        if !description.is_empty() {
                            return description.chars().take(200).collect();
                        }
                    }
                }
            } else if let Some(line) = first_line {
                let trimmed = line.trim().trim_start_matches('#').trim();
                if !trimmed.is_empty() && trimmed != "---" {
                    return trimmed.chars().take(200).collect();
                }
            }
            for line in lines {
                let trimmed = line.trim().trim_start_matches('#').trim();
                if !trimmed.is_empty() && trimmed != "---" {
                    return trimmed.chars().take(200).collect();
                }
            }
        }
    }
    String::new()
}

fn scan_local_instruction_skill_entries(
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

fn remove_instruction_skill_from_list(
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

fn instruction_skill_id(item: &serde_json::Value) -> Option<String> {
    item.get("skillId")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn instruction_skill_matches(item: &serde_json::Value, skill_name: &str) -> bool {
    item.get("skillId")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value == skill_name)
}

fn normalize_local_instruction_skill_request_path(
    request: &serde_json::Value,
) -> Result<std::path::PathBuf, ApiError> {
    let directory_path = request
        .get("directoryPath")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("请选择本地 Skill 目录".to_string()))?;
    Ok(std::path::PathBuf::from(directory_path))
}

fn normalize_local_instruction_skill_request_skill_id(
    request: &serde_json::Value,
) -> Option<String> {
    request
        .get("skillId")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn public_local_instruction_skill_entry(entry: &serde_json::Value) -> serde_json::Value {
    let mut obj = entry.as_object().cloned().unwrap_or_default();
    obj.remove("directoryPath");
    if !obj.contains_key("localSkillId")
        && let Some(skill_id) = instruction_skill_id(entry)
    {
        obj.insert("localSkillId".to_string(), serde_json::json!(skill_id));
    }
    serde_json::Value::Object(obj)
}

fn epoch_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

struct LocalSkillMetadata {
    file_count: usize,
    last_modified_epoch_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GithubRepositoryRef {
    owner: String,
    name: String,
    canonical_url: String,
}

impl GithubRepositoryRef {
    fn clone_url(&self) -> String {
        format!("{}.git", self.canonical_url)
    }

    fn cache_key(&self) -> String {
        format!("{}__{}", self.owner, self.name)
    }

    fn skill_id(&self, relative_path: &Path) -> Result<String, ApiError> {
        let relative = relative_path_to_slash_string(relative_path)?;
        if relative.is_empty() {
            Ok(format!("{}/{}", self.owner, self.name))
        } else {
            Ok(format!("{}/{}/{}", self.owner, self.name, relative))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RepositorySkill {
    skill_id: String,
    name: String,
    description: String,
    relative_path: PathBuf,
}

fn parse_github_repository_url(raw_url: &str) -> Result<GithubRepositoryRef, ApiError> {
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

fn is_safe_github_repository_segment(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
}

fn relative_path_to_slash_string(path: &Path) -> Result<String, ApiError> {
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

fn read_skill_frontmatter_value(dir: &Path, key: &str) -> Option<String> {
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

fn scan_cached_repository_skills(
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

fn collect_repository_skills(
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
        let fallback_name = current_dir
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(&repository.name)
            .to_string();
        skills.push(RepositorySkill {
            skill_id: repository.skill_id(&relative_path)?,
            name: read_skill_frontmatter_value(current_dir, "name").unwrap_or(fallback_name),
            description: read_skill_frontmatter_value(current_dir, "description")
                .unwrap_or_else(|| read_local_skill_description(current_dir)),
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

fn copy_repository_skill_directory(source: &Path, target: &Path) -> Result<(), ApiError> {
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
    copy_directory_tree(source, &staging)?;
    replace_directory_atomically(&staging, target, "替换 Skill 缓存失败")
}

fn copy_directory_tree(source: &Path, target: &Path) -> Result<(), ApiError> {
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

fn replace_directory_atomically(
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

fn read_local_skill_metadata(dir: &std::path::Path) -> LocalSkillMetadata {
    let mut file_count = 0usize;
    let mut latest_modified: Option<std::time::SystemTime> = None;

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_file() {
                file_count += 1;
            }
            if let Ok(modified) = meta.modified() {
                latest_modified = Some(
                    latest_modified
                        .map_or(modified, |prev: std::time::SystemTime| prev.max(modified)),
                );
            }
        }
    }

    let last_modified_epoch_ms = latest_modified.and_then(|t| {
        t.duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as u64)
    });

    LocalSkillMetadata {
        file_count,
        last_modified_epoch_ms,
    }
}

fn normalize_repository_entry(
    request: &serde_json::Value,
    allow_updates: bool,
) -> Result<serde_json::Value, ApiError> {
    if request.get("repository").is_some() {
        return Err(ApiError::InvalidInput(
            "仓库配置必须使用顶层 url/repositoryId，不能包裹在 repository 中".to_string(),
        ));
    }
    if request.get("updates").is_some() && !allow_updates {
        return Err(ApiError::InvalidInput(
            "新增仓库必须使用顶层 url，不能包裹在 updates 中".to_string(),
        ));
    }
    let mut entry = request.as_object().cloned().unwrap_or_default();
    if let Some(updates) = request.get("updates").and_then(|value| value.as_object()) {
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

fn upsert_named_object_array_entry(
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

// ─── MCP Servers ────────────────────────────────────────────────────────────

fn canonical_mcp_servers(state: &ApiState) -> Vec<serde_json::Value> {
    stored_mcp_servers(state)
        .into_iter()
        .map(redact_mcp_server_public_entry)
        .map(|mut entry| {
            let enabled = mcp_server_entry_enabled(&entry);
            let server_id = mcp_server_entry_id(&entry).map(str::to_string);
            let connected = enabled
                && server_id.as_deref().is_some_and(|server_id| {
                    state
                        .mcp_connections()
                        .read()
                        .expect("mcp connections read lock poisoned")
                        .contains_key(server_id)
                });
            entry["connected"] = serde_json::json!(connected);
            entry["health"] = serde_json::json!(if !enabled {
                MCP_DISABLED_HEALTH
            } else if connected {
                "connected"
            } else {
                MCP_DISCONNECTED_HEALTH
            });
            if !enabled {
                entry.as_object_mut().map(|object| object.remove("error"));
            }
            entry
        })
        .collect()
}

fn stored_mcp_servers(state: &ApiState) -> Vec<serde_json::Value> {
    state
        .settings_store
        .public_snapshot()
        .get("mcpServers")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(normalize_mcp_server_snapshot_entry)
        .collect()
}

fn stored_mcp_server_entry(state: &ApiState, server_id: &str) -> Option<serde_json::Value> {
    stored_mcp_servers(state)
        .into_iter()
        .find(|entry| mcp_server_entry_id(entry).is_some_and(|id| id == server_id))
}

async fn list_mcp_servers(State(state): State<ApiState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "servers": canonical_mcp_servers(&state) }))
}

async fn add_mcp_server(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let normalized = normalize_mcp_server_entry(&request)?;
    state
        .settings_store
        .upsert_array_entry("mcpServers", "id", &normalized);
    Ok(Json(serde_json::json!({ "added": true })))
}

async fn update_mcp_server(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let requested_server_id = mcp_server_entry_id(&request).map(str::to_string);
    let mut normalized = normalize_mcp_server_entry(&request)?;
    let server_id = requested_server_id
        .or_else(|| mcp_server_entry_id(&normalized).map(str::to_string))
        .unwrap_or_default()
        .trim()
        .to_string();
    let existing = stored_mcp_server_entry(&state, &server_id);
    preserve_redacted_mcp_env_values(&mut normalized, existing.as_ref());
    state
        .settings_store
        .upsert_array_entry("mcpServers", "id", &normalized);
    if !mcp_server_entry_enabled(&normalized) {
        remove_mcp_connection(&state, &server_id);
    }
    Ok(Json(serde_json::json!({ "updated": true })))
}

async fn delete_mcp_server(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let server_id = request
        .get("serverId")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    state
        .settings_store
        .remove_array_entry("mcpServers", "id", server_id);
    {
        let mut pool = state
            .mcp_connections()
            .write()
            .expect("mcp connections write lock poisoned");
        pool.remove(server_id);
    }
    Ok(Json(serde_json::json!({ "deleted": true })))
}

fn find_server_entry(state: &ApiState, server_id: &str) -> Option<serde_json::Value> {
    stored_mcp_server_entry(state, server_id)
}

fn remove_mcp_connection(state: &ApiState, server_id: &str) {
    let mut pool = state
        .mcp_connections()
        .write()
        .expect("mcp connections write lock poisoned");
    pool.remove(server_id);
}

fn mcp_tools_unavailable_response(server_id: &str) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "tools": [],
        "connected": false,
        "health": MCP_DISCONNECTED_HEALTH,
        "error": MCP_CONNECTION_FAILED_MARKER,
        "serverId": server_id,
        "toolCount": 0,
    }))
}

fn mcp_tools_disabled_response(server_id: &str) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "tools": [],
        "connected": false,
        "health": MCP_DISABLED_HEALTH,
        "serverId": server_id,
        "toolCount": 0,
    }))
}

async fn connect_mcp_server(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let server_id = request
        .get("serverId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::InvalidInput("serverId 不能为空".to_string()))?
        .to_string();

    let entry = find_server_entry(&state, &server_id)
        .ok_or_else(|| ApiError::not_found("MCP server 配置不存在", &server_id))?;

    if !mcp_server_entry_enabled(&entry) {
        remove_mcp_connection(&state, &server_id);
        return Ok(mcp_tools_disabled_response(&server_id));
    }

    let config = build_mcp_config_from_entry(&entry)
        .ok_or_else(|| ApiError::InvalidInput("MCP server 配置中缺少 command".to_string()))?;

    let client = Arc::new(StdioMcpBridgeClient::new(config));
    let tools = list_mcp_tools(client.clone()).await.map_err(|err| {
        tracing::warn!(
            server_id = %server_id,
            error = ?err,
            "MCP server connect failed"
        );
        ApiError::InvalidInput("MCP server 连接失败".to_string())
    })?;

    {
        let mut pool = state
            .mcp_connections()
            .write()
            .expect("mcp connections write lock poisoned");
        pool.insert(server_id.clone(), client);
    }

    Ok(Json(serde_json::json!({
        "connected": true,
        "serverId": server_id,
        "toolCount": tools.len(),
    })))
}

async fn disconnect_mcp_server(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let server_id = request
        .get("serverId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::InvalidInput("serverId 不能为空".to_string()))?;

    let removed = {
        let mut pool = state
            .mcp_connections()
            .write()
            .expect("mcp connections write lock poisoned");
        pool.remove(server_id).is_some()
    };

    Ok(Json(serde_json::json!({
        "disconnected": removed,
        "serverId": server_id,
    })))
}

async fn get_mcp_tools(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let server_id = request
        .get("serverId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::InvalidInput("serverId 不能为空".to_string()))?;

    if let Some(entry) = stored_mcp_server_entry(&state, server_id)
        && !mcp_server_entry_enabled(&entry)
    {
        remove_mcp_connection(&state, server_id);
        return Ok(mcp_tools_disabled_response(server_id));
    }

    let client = {
        let pool = state
            .mcp_connections()
            .read()
            .expect("mcp connections read lock poisoned");
        pool.get(server_id).cloned()
    };

    let Some(client) = client else {
        return Ok(mcp_tools_unavailable_response(server_id));
    };

    let tools = match list_mcp_tools(client).await {
        Ok(tools) => tools,
        Err(err) => {
            tracing::warn!(
                server_id = %server_id,
                error = ?err,
                "MCP tools fetch failed"
            );
            remove_mcp_connection(&state, server_id);
            return Ok(mcp_tools_unavailable_response(server_id));
        }
    };

    Ok(Json(serde_json::json!({
        "tools": tools,
        "connected": true,
        "serverId": server_id,
    })))
}

async fn refresh_mcp_tools(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let server_id = request
        .get("serverId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::InvalidInput("serverId 不能为空".to_string()))?;

    if let Some(entry) = stored_mcp_server_entry(&state, server_id)
        && !mcp_server_entry_enabled(&entry)
    {
        remove_mcp_connection(&state, server_id);
        return Ok(mcp_tools_disabled_response(server_id));
    }

    let client = {
        let pool = state
            .mcp_connections()
            .read()
            .expect("mcp connections read lock poisoned");
        pool.get(server_id).cloned()
    };

    let Some(client) = client else {
        return Ok(mcp_tools_unavailable_response(server_id));
    };

    let tools = match list_mcp_tools(client).await {
        Ok(tools) => tools,
        Err(err) => {
            tracing::warn!(
                server_id = %server_id,
                error = ?err,
                "MCP tools refresh failed"
            );
            remove_mcp_connection(&state, server_id);
            return Ok(mcp_tools_unavailable_response(server_id));
        }
    };

    Ok(Json(serde_json::json!({
        "tools": tools,
        "connected": true,
        "serverId": server_id,
    })))
}

// ─── Repositories ───────────────────────────────────────────────────────────

fn skill_repository_cache_root(state: &ApiState) -> Result<PathBuf, ApiError> {
    state
        .runtime_persistence()
        .and_then(|persistence| persistence.state_root())
        .map(|root| root.join("skill_repositories"))
        .ok_or_else(|| ApiError::InvalidInput(SKILL_CACHE_PUBLIC_ERROR.to_string()))
}

fn installed_skill_cache_root(state: &ApiState) -> Result<PathBuf, ApiError> {
    state
        .runtime_persistence()
        .and_then(|persistence| persistence.state_root())
        .map(|root| root.join("skills_cache"))
        .ok_or_else(|| ApiError::InvalidInput(SKILL_CACHE_PUBLIC_ERROR.to_string()))
}

fn installed_skill_cache_key(skill_id: &str) -> Result<String, ApiError> {
    if skill_id.trim().is_empty() {
        return Err(ApiError::InvalidInput("skillId 无效".to_string()));
    }
    Ok(urlencoding::encode(skill_id).into_owned())
}

async fn ensure_cached_github_repository(
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

    let clone_future = tokio::process::Command::new("git")
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

fn repository_skill_public_entry(
    skill: RepositorySkill,
    repository_id: &str,
    repository_url: &str,
    installed: bool,
) -> serde_json::Value {
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
    })
}

async fn find_repository_skill(
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

async fn list_repositories(State(state): State<ApiState>) -> Json<serde_json::Value> {
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
    if repos != stored_repos {
        state
            .settings_store
            .set_section("repositories", serde_json::Value::Array(repos.clone()));
    }
    Json(serde_json::json!({ "repositories": repos }))
}

fn clean_repository_scope_fields(mut entry: serde_json::Value) -> serde_json::Value {
    if let Some(object) = entry.as_object_mut() {
        strip_scope_binding_fields_from_map(object);
    }
    entry
}

async fn add_repository(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let normalized = normalize_repository_entry(&request, false)?;
    state
        .settings_store
        .upsert_array_entry("repositories", "repositoryId", &normalized);
    Ok(Json(serde_json::json!({ "added": true })))
}

async fn update_repository(
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
        .upsert_array_entry("repositories", "repositoryId", &normalized);
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

async fn delete_repository(
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
        .remove_array_entry("repositories", "repositoryId", repo_id);
    if let Some(repository_url) = repository_url
        && let Ok(repository) = parse_github_repository_url(&repository_url)
        && let Ok(cache_root) = skill_repository_cache_root(&state)
    {
        let _ = std::fs::remove_dir_all(cache_root.join(repository.cache_key()));
    }
    Ok(Json(serde_json::json!({ "deleted": true })))
}

async fn refresh_repository(
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

    let repository_url = repos[pos]
        .get("url")
        .and_then(|value| value.as_str())
        .ok_or_else(|| ApiError::InvalidInput("仓库 URL 不能为空".to_string()))?;
    let (repository, repository_root) =
        ensure_cached_github_repository(&state, repository_url, true).await?;
    let skill_count = scan_cached_repository_skills(&repository_root, &repository)?.len();

    let mut updated_repos = repos;
    let mut entry = updated_repos[pos].as_object().cloned().unwrap_or_default();
    entry.insert(
        "lastRefreshed".to_string(),
        serde_json::json!(epoch_ms_now()),
    );
    entry.insert("skillCount".to_string(), serde_json::json!(skill_count));
    updated_repos[pos] = serde_json::Value::Object(entry);
    state
        .settings_store
        .set_section("repositories", serde_json::Value::Array(updated_repos));

    Ok(Json(serde_json::json!({
        "refreshed": true,
        "skillCount": skill_count,
    })))
}

// ─── Skills ─────────────────────────────────────────────────────────────────

async fn list_skills(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let installed_skills = load_instruction_skills(&state);
    let installed_skill_ids: std::collections::HashSet<String> = installed_skills
        .iter()
        .filter(|skill| skill_loader::instruction_skill_source_available(skill))
        .filter_map(instruction_skill_id)
        .collect();
    let mut all_skills: Vec<serde_json::Value> = installed_skills
        .into_iter()
        .filter(skill_loader::instruction_skill_source_available)
        .map(|skill| {
            let mut entry = skill.as_object().cloned().unwrap_or_default();
            entry.remove("directoryPath");
            entry.insert("installed".to_string(), serde_json::json!(true));
            serde_json::Value::Object(entry)
        })
        .collect();
    let repos = state
        .settings_store
        .get_section("repositories")
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut failed_repo_count = 0usize;
    for repo in &repos {
        let repo_url = repo.get("url").and_then(|v| v.as_str()).unwrap_or_default();
        let repo_id = repo
            .get("repositoryId")
            .and_then(|v| v.as_str())
            .unwrap_or(repo_url);
        if repo_url.is_empty() {
            continue;
        }
        match ensure_cached_github_repository(&state, repo_url, false).await {
            Ok((repository, repository_root)) => {
                match scan_cached_repository_skills(&repository_root, &repository) {
                    Ok(remote_skills) => {
                        for skill in remote_skills {
                            let installed = installed_skill_ids.contains(&skill.skill_id);
                            if !installed {
                                all_skills.push(repository_skill_public_entry(
                                    skill, repo_id, repo_url, false,
                                ));
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            repository_id = %repo_id,
                            url = %repo_url,
                            error = ?error,
                            "Skill repository scan failed"
                        );
                        failed_repo_count += 1;
                    }
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
    Json(serde_json::json!({
        "skills": all_skills,
        "failedRepositoryCount": failed_repo_count,
    }))
}

async fn install_skill(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let normalized_request = normalize_instruction_skill_entry(&request)?;
    let skill_id = normalized_request
        .get("skillId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let (repository, repository_root, skill, repository_id) =
        find_repository_skill(&state, &skill_id).await?;
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

    let mut instruction_skills = load_instruction_skills(&state);
    upsert_named_object_array_entry(&mut instruction_skills, normalized, &["skillId"]);
    persist_instruction_skills(&state, instruction_skills);
    Ok(Json(serde_json::json!({
        "installed": true,
        "skillId": skill_id,
    })))
}

async fn install_local_skill(
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
    persist_instruction_skills(&state, instruction_skills);

    Ok(Json(serde_json::json!({
        "installed": true,
        "count": entries.len(),
    })))
}

async fn scan_local_skill_directory(
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

async fn save_skills_config(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if request.get("config").is_some() || request.get("data").is_some() {
        return Err(ApiError::InvalidInput(
            "skillsConfig 必须作为顶层对象提交，不能包裹在 config/data 中".to_string(),
        ));
    }
    let config = request
        .as_object()
        .cloned()
        .ok_or_else(|| ApiError::InvalidInput("skillsConfig 必须是对象".to_string()))?;
    persist_skills_config_object(&state, config);
    Ok(Json(serde_json::json!({ "saved": true })))
}

async fn add_custom_tool(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if request.get("tool").is_some() {
        return Err(ApiError::InvalidInput(
            "自定义工具必须作为顶层对象提交，不能包裹在 tool 中".to_string(),
        ));
    }
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
    persist_skills_config_object(&state, config);
    Ok(Json(serde_json::json!({ "added": true })))
}

async fn remove_installed_skill(
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
            persist_skills_config_object(&state, config);
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
            let removed = remove_instruction_skill_from_list(&mut instruction_skills, skill_id);
            if !removed {
                return Err(ApiError::not_found("技能未安装", skill_id));
            }
            persist_instruction_skills(&state, instruction_skills);
            if removed_skill
                .as_ref()
                .and_then(|skill| skill.get("source"))
                .and_then(|value| value.as_str())
                == Some("repository")
                && let Some(directory_path) = removed_skill
                    .as_ref()
                    .and_then(|skill| skill.get("directoryPath"))
                    .and_then(|value| value.as_str())
            {
                let directory = PathBuf::from(directory_path);
                if directory.is_dir() {
                    std::fs::remove_dir_all(&directory).map_err(|error| {
                        skill_cache_error("删除 Skill 缓存失败", &directory, error)
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

async fn update_skill(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let skill_id = request
        .get("skillId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("skillId 不能为空".to_string()))?;

    let mut instruction_skills = load_instruction_skills(&state);
    let position = instruction_skills
        .iter()
        .position(|item| instruction_skill_matches(item, skill_id));

    let Some(pos) = position else {
        return Err(ApiError::not_found("技能未安装", skill_id));
    };

    let skill = &instruction_skills[pos];
    if let Some(dir_path) = skill.get("directoryPath").and_then(|v| v.as_str()) {
        let path = std::path::Path::new(dir_path);
        if !path.is_dir() {
            return Err(ApiError::InvalidInput(
                "技能源不可用，请重新导入该 Skill".to_string(),
            ));
        }
        let mut updated_entry = skill.as_object().cloned().unwrap_or_default();
        let meta = read_local_skill_metadata(path);
        updated_entry.insert("fileCount".to_string(), serde_json::json!(meta.file_count));
        updated_entry.insert(
            "lastRefreshed".to_string(),
            serde_json::json!(epoch_ms_now()),
        );
        if let Some(ts) = meta.last_modified_epoch_ms {
            updated_entry.insert("lastModified".to_string(), serde_json::json!(ts));
        }
        instruction_skills[pos] = serde_json::Value::Object(updated_entry);
        persist_instruction_skills(&state, instruction_skills);
    }

    Ok(Json(serde_json::json!({ "updated": true })))
}

async fn update_all_skills(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut instruction_skills = load_instruction_skills(&state);
    let mut updated_count = 0u32;

    for skill in instruction_skills.iter_mut() {
        let Some(dir_path) = skill.get("directoryPath").and_then(|v| v.as_str()) else {
            continue;
        };
        let path = std::path::Path::new(dir_path);
        if !path.is_dir() {
            continue;
        }
        let mut entry = skill.as_object().cloned().unwrap_or_default();
        let meta = read_local_skill_metadata(path);
        entry.insert("fileCount".to_string(), serde_json::json!(meta.file_count));
        entry.insert(
            "lastRefreshed".to_string(),
            serde_json::json!(epoch_ms_now()),
        );
        if let Some(ts) = meta.last_modified_epoch_ms {
            entry.insert("lastModified".to_string(), serde_json::json!(ts));
        }
        *skill = serde_json::Value::Object(entry);
        updated_count += 1;
    }

    if updated_count > 0 {
        persist_instruction_skills(&state, instruction_skills);
    }

    Ok(Json(serde_json::json!({
        "updated": true,
        "count": updated_count,
    })))
}

fn skill_repository_error(
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

fn skill_cache_error(action: &'static str, path: &Path, error: impl Display) -> ApiError {
    tracing::warn!(
        action,
        path = %path.display(),
        error = %error,
        "Skill cache write failed"
    );
    ApiError::InvalidInput(SKILL_CACHE_PUBLIC_ERROR.to_string())
}

async fn get_instruction_skill_preview(
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

    let directory_path = matched
        .get("directoryPath")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("技能源不可用，请重新导入该 Skill".to_string()))?;

    let dir = PathBuf::from(directory_path);
    let instruction = skill_loader::read_available_skill_instruction(&dir)
        .ok_or_else(|| ApiError::InvalidInput("技能源不可用，请重新导入该 Skill".to_string()))?;
    let preview: String = instruction.chars().take(200).collect();

    Ok(Json(serde_json::json!({
        "skillId": skill_id,
        "preview": preview,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope_binding::SCOPE_BINDING_FIELDS;
    use crate::state::RuntimeStatePersistence;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_state() -> ApiState {
        ApiState::new(
            "magi-mcp-skill-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
    }

    fn test_state_with_persistence(root: &Path) -> ApiState {
        test_state().with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
            root.join("sessions.json"),
            root.join("workspaces.json"),
            root.join("knowledge.json"),
        )))
    }

    #[test]
    fn local_skill_description_reads_yaml_frontmatter_description() {
        let dir = tempfile::tempdir().expect("temp skill dir should create");
        std::fs::write(
            dir.path().join("SKILL.md"),
            "---\nname: demo-skill\ndescription: 用于验证 Skill 快捷引用\n---\n\n# Demo\n",
        )
        .expect("skill markdown should write");

        assert_eq!(
            read_local_skill_description(dir.path()),
            "用于验证 Skill 快捷引用"
        );
    }

    #[test]
    fn github_repository_url_normalizes_to_stable_identity() {
        let repository =
            parse_github_repository_url("https://github.com/stellarlinkco/myclaude.git/")
                .expect("GitHub repository URL should parse");

        assert_eq!(repository.owner, "stellarlinkco");
        assert_eq!(repository.name, "myclaude");
        assert_eq!(
            repository.canonical_url,
            "https://github.com/stellarlinkco/myclaude"
        );
    }

    #[test]
    fn repository_skill_scan_uses_skill_directory_and_frontmatter_metadata() {
        let repository_root = tempfile::tempdir().expect("repository root should create");
        let skill_dir = repository_root.path().join("skills").join("omo");
        std::fs::create_dir_all(&skill_dir).expect("skill directory should create");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: omo\ndescription: 多代理编排\n---\n\n# OmO\n",
        )
        .expect("skill markdown should write");

        let repository = parse_github_repository_url("https://github.com/stellarlinkco/myclaude")
            .expect("GitHub repository URL should parse");
        let skills = scan_cached_repository_skills(repository_root.path(), &repository)
            .expect("repository skills should scan");

        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].skill_id, "stellarlinkco/myclaude/skills/omo");
        assert_eq!(skills[0].name, "omo");
        assert_eq!(skills[0].description, "多代理编排");
        assert_eq!(skills[0].relative_path, PathBuf::from("skills/omo"));
    }

    #[test]
    fn repository_skill_copy_preserves_complete_skill_directory() {
        let source_root = tempfile::tempdir().expect("source root should create");
        let target_root = tempfile::tempdir().expect("target root should create");
        let source = source_root.path().join("skill");
        let target = target_root.path().join("installed");
        std::fs::create_dir_all(source.join("scripts")).expect("source tree should create");
        std::fs::write(source.join("SKILL.md"), "# demo\n").expect("skill should write");
        std::fs::write(source.join("scripts").join("run.sh"), "echo ok\n")
            .expect("script should write");

        copy_repository_skill_directory(&source, &target)
            .expect("complete skill directory should copy");

        assert_eq!(
            std::fs::read_to_string(target.join("SKILL.md")).expect("skill should read"),
            "# demo\n"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("scripts").join("run.sh"))
                .expect("script should read"),
            "echo ok\n"
        );
    }

    #[tokio::test]
    async fn repository_skill_library_and_install_share_cached_repository_source() {
        let state_root = tempfile::tempdir().expect("state root should create");
        let repository_root = state_root
            .path()
            .join("skill_repositories")
            .join("stellarlinkco__myclaude");
        let skill_dir = repository_root.join("skills").join("omo");
        std::fs::create_dir_all(repository_root.join(".git"))
            .expect("repository marker should create");
        std::fs::create_dir_all(skill_dir.join("scripts")).expect("skill tree should create");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: omo\ndescription: 多代理编排\n---\n\n# OmO\n",
        )
        .expect("skill markdown should write");
        std::fs::write(skill_dir.join("scripts").join("run.sh"), "echo ok\n")
            .expect("skill script should write");

        let state = test_state_with_persistence(state_root.path());
        state.settings_store.set_section(
            "repositories",
            serde_json::json!([{
                "repositoryId": "https://github.com/stellarlinkco/myclaude",
                "url": "https://github.com/stellarlinkco/myclaude"
            }]),
        );

        let library = get_json(
            Router::new().merge(routes()).with_state(state.clone()),
            "/settings/skills/library",
        )
        .await;
        assert_eq!(library["failedRepositoryCount"], serde_json::json!(0));
        assert_eq!(library["skills"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            library["skills"][0]["skillId"],
            serde_json::json!("stellarlinkco/myclaude/skills/omo")
        );
        assert_eq!(library["skills"][0]["name"], serde_json::json!("omo"));

        let installed = post_json(
            Router::new().merge(routes()).with_state(state.clone()),
            "/settings/skills/install",
            serde_json::json!({
                "skillId": "stellarlinkco/myclaude/skills/omo"
            }),
        )
        .await;
        assert_eq!(installed["installed"], serde_json::json!(true));

        let stored = state.settings_store.get_section("skillsConfig");
        let skill = &stored["instructionSkills"][0];
        assert_eq!(
            skill["skillId"],
            serde_json::json!("stellarlinkco/myclaude/skills/omo")
        );
        assert_eq!(skill["name"], serde_json::json!("omo"));
        assert_eq!(skill["description"], serde_json::json!("多代理编排"));
        assert_eq!(skill["source"], serde_json::json!("repository"));
        let installed_dir = PathBuf::from(
            skill["directoryPath"]
                .as_str()
                .expect("installed directory should exist"),
        );
        assert_eq!(
            std::fs::read_to_string(installed_dir.join("scripts").join("run.sh"))
                .expect("installed script should read"),
            "echo ok\n"
        );

        let installed_library = get_json(
            Router::new().merge(routes()).with_state(state),
            "/settings/skills/library",
        )
        .await;
        assert_eq!(
            installed_library["skills"][0]["repositoryName"],
            serde_json::json!("https://github.com/stellarlinkco/myclaude")
        );
    }

    async fn post_json(app: Router, path: &str, payload: serde_json::Value) -> serde_json::Value {
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(payload.to_string()))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should read");
        serde_json::from_slice(&bytes).expect("response should be json")
    }

    async fn post_json_with_status(
        app: Router,
        path: &str,
        payload: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(payload.to_string()))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should read");
        let body = serde_json::from_slice(&bytes).expect("response should be json");
        (status, body)
    }

    async fn get_json(app: Router, path: &str) -> serde_json::Value {
        let response = app
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should read");
        serde_json::from_slice(&bytes).expect("response should be json")
    }

    #[tokio::test]
    async fn mcp_server_list_redacts_env_values() {
        let state = test_state();
        state.settings_store.upsert_array_entry(
            "mcpServers",
            "id",
            &serde_json::json!({
                "id": "server-redacted",
                "name": "server-redacted",
                "command": "node",
                "enabled": false,
                "env": {
                    "TOKEN": "secret-token"
                }
            }),
        );
        let app = Router::new().merge(routes()).with_state(state);

        let body = get_json(app, "/settings/mcp").await;

        assert_eq!(
            body["servers"][0]["env"]["TOKEN"],
            serde_json::json!(crate::mcp_config::REDACTED_MCP_ENV_VALUE)
        );
    }

    #[tokio::test]
    async fn mcp_server_update_preserves_redacted_env_values() {
        let state = test_state();
        state.settings_store.upsert_array_entry(
            "mcpServers",
            "id",
            &serde_json::json!({
                "id": "server-preserve-env",
                "name": "server-preserve-env",
                "command": "node",
                "env": {
                    "TOKEN": "secret-token",
                    "MODE": "old"
                }
            }),
        );
        let app = Router::new().merge(routes()).with_state(state.clone());

        let body = post_json(
            app,
            "/settings/mcp/update",
            serde_json::json!({
                "id": "server-preserve-env",
                "name": "server-preserve-env",
                "command": "node",
                "env": {
                    "TOKEN": crate::mcp_config::REDACTED_MCP_ENV_VALUE,
                    "MODE": "new"
                },
                "enabled": false
            }),
        )
        .await;

        assert_eq!(body["updated"], true);
        let stored = stored_mcp_server_entry(&state, "server-preserve-env")
            .expect("updated server should remain stored");
        assert_eq!(stored["env"]["TOKEN"], serde_json::json!("secret-token"));
        assert_eq!(stored["env"]["MODE"], serde_json::json!("new"));
        assert_eq!(stored["enabled"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn mcp_server_save_strips_workspace_session_scope_fields() {
        let state = test_state();
        let app = Router::new().merge(routes()).with_state(state.clone());

        let body = post_json(
            app,
            "/settings/mcp/add",
            serde_json::json!({
                "id": "server-scope-clean",
                "name": "server-scope-clean",
                "command": "node",
                "enabled": false,
                "workspaceId": "workspace-old",
                "workspacePath": "/tmp/old",
                "sessionId": "session-old"
            }),
        )
        .await;

        assert_eq!(body["added"], true);
        let stored = stored_mcp_server_entry(&state, "server-scope-clean")
            .expect("added server should remain stored");
        for key in ["workspaceId", "workspacePath", "sessionId"] {
            assert!(
                stored.get(key).is_none(),
                "MCP server settings are global and must not persist {key}"
            );
        }
    }

    #[tokio::test]
    async fn mcp_server_save_rejects_wrapped_requests() {
        for (path, wrapper) in [
            ("/settings/mcp/add", "server"),
            ("/settings/mcp/add", "updates"),
            ("/settings/mcp/update", "server"),
            ("/settings/mcp/update", "updates"),
        ] {
            let app = Router::new().merge(routes()).with_state(test_state());

            let (status, body) = post_json_with_status(
                app,
                path,
                serde_json::json!({
                    wrapper: {
                        "id": "wrapped-server",
                        "command": "node"
                    }
                }),
            )
            .await;

            assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
            assert!(
                body["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("不能包裹在 server/updates 中"),
                "unexpected body: {body}"
            );
        }
    }

    #[tokio::test]
    async fn mcp_server_list_filters_legacy_wrapped_entries() {
        let state = test_state();
        state.settings_store.set_section(
            "mcpServers",
            serde_json::json!([
                {
                    "server": {
                        "id": "wrapped-server",
                        "command": "node"
                    }
                },
                {
                    "id": "canonical-server",
                    "command": "node",
                    "enabled": false
                }
            ]),
        );
        let app = Router::new().merge(routes()).with_state(state);

        let body = get_json(app, "/settings/mcp").await;
        let servers = body["servers"].as_array().expect("servers should be array");

        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0]["id"], serde_json::json!("canonical-server"));
    }

    fn assert_scope_fields_absent(value: &serde_json::Value) {
        for key in SCOPE_BINDING_FIELDS {
            assert!(
                value.get(key).is_none(),
                "global settings must not persist scope field {key}"
            );
        }
    }

    #[tokio::test]
    async fn repository_save_strips_workspace_session_scope_fields() {
        let state = test_state();
        let app = Router::new().merge(routes()).with_state(state.clone());

        let body = post_json(
            app,
            "/settings/repositories/add",
            serde_json::json!({
                "url": "https://github.com/example/skills",
                "workspaceId": "workspace-old",
                "workspace_path": "/tmp/old",
                "sessionId": "session-old"
            }),
        )
        .await;

        assert_eq!(body["added"], true);
        let stored = state.settings_store.get_section("repositories");
        assert_scope_fields_absent(&stored[0]);
    }

    #[tokio::test]
    async fn repository_add_rejects_wrapped_requests() {
        for payload in [
            serde_json::json!({
                "repository": {
                    "url": "https://github.com/example/skills"
                }
            }),
            serde_json::json!({
                "updates": {
                    "url": "https://github.com/example/skills"
                }
            }),
        ] {
            let app = Router::new().merge(routes()).with_state(test_state());

            let (status, body) =
                post_json_with_status(app, "/settings/repositories/add", payload).await;

            assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
            let message = body["message"].as_str().unwrap_or_default();
            assert!(
                message.contains("不能包裹在 repository 中")
                    || message.contains("不能包裹在 updates 中"),
                "unexpected body: {body}"
            );
        }
    }

    #[tokio::test]
    async fn repository_update_accepts_top_level_id_and_updates_patch() {
        let state = test_state();
        let app = Router::new().merge(routes()).with_state(state.clone());

        let body = post_json(
            app,
            "/settings/repositories/update",
            serde_json::json!({
                "repositoryId": "https://github.com/example/skills",
                "updates": {
                    "url": "https://github.com/example/skills-renamed"
                }
            }),
        )
        .await;

        assert_eq!(body["updated"], true);
        let stored = state.settings_store.get_section("repositories");
        assert_eq!(
            stored[0]["repositoryId"],
            serde_json::json!("https://github.com/example/skills")
        );
        assert_eq!(
            stored[0]["url"],
            serde_json::json!("https://github.com/example/skills-renamed")
        );
        assert!(stored[0].get("updates").is_none());
    }

    #[tokio::test]
    async fn repository_list_cleans_legacy_scope_fields() {
        let state = test_state();
        state.settings_store.set_section(
            "repositories",
            serde_json::json!([
                {
                    "repositoryId": "legacy-repo",
                    "url": "https://github.com/example/skills",
                    "workspaceId": "workspace-old",
                    "workspace_path": "/tmp/old",
                    "sessionId": "session-old"
                }
            ]),
        );
        let app = Router::new().merge(routes()).with_state(state.clone());

        let body = get_json(app, "/settings/repositories").await;

        assert_scope_fields_absent(&body["repositories"][0]);
        let stored = state.settings_store.get_section("repositories");
        assert_scope_fields_absent(&stored[0]);
    }

    #[test]
    fn instruction_skill_normalization_strips_workspace_session_scope_fields() {
        let normalized = normalize_instruction_skill_entry(&serde_json::json!({
            "skillId": "example/skill",
            "workspaceId": "workspace-old",
            "workspace_path": "/tmp/old",
            "sessionId": "session-old"
        }))
        .expect("skill should normalize");

        assert_scope_fields_absent(&normalized);
        assert_eq!(normalized["skillId"], serde_json::json!("example/skill"));
        assert_eq!(normalized["name"], serde_json::json!("example/skill"));
        assert!(normalized.get("skillName").is_none());
    }

    #[tokio::test]
    async fn install_skill_rejects_wrapped_requests() {
        for wrapper in ["skill", "updates"] {
            let app = Router::new().merge(routes()).with_state(test_state());

            let (status, body) = post_json_with_status(
                app,
                "/settings/skills/install",
                serde_json::json!({
                    wrapper: {
                        "skillId": "example/skill"
                    }
                }),
            )
            .await;

            assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
            assert!(
                body["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("不能包裹在 skill/updates 中"),
                "unexpected body: {body}"
            );
        }
    }

    #[tokio::test]
    async fn skills_config_save_strips_workspace_session_scope_fields() {
        let state = test_state();
        let app = Router::new().merge(routes()).with_state(state.clone());

        let body = post_json(
            app,
            "/settings/skills/config/save",
            serde_json::json!({
                "workspaceId": "workspace-old",
                "workspace_path": "/tmp/old",
                "sessionId": "session-old",
                "instructionSkills": [
                    {
                        "skillId": "example/skill",
                        "skillName": "legacy-skill-name",
                        "workspaceId": "workspace-old",
                        "session_id": "session-old"
                    },
                    {
                        "skillName": "legacy-skill-name-only"
                    }
                ],
                "customTools": [
                    {
                        "name": "example-tool",
                        "workspacePath": "/tmp/old",
                        "sessionId": "session-old"
                    }
                ]
            }),
        )
        .await;

        assert_eq!(body["saved"], true);
        let stored = state.settings_store.get_section("skillsConfig");
        assert_scope_fields_absent(&stored);
        assert_scope_fields_absent(&stored["instructionSkills"][0]);
        assert_scope_fields_absent(&stored["customTools"][0]);
        assert_eq!(
            stored["instructionSkills"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(
            stored["instructionSkills"][0]["skillId"],
            serde_json::json!("example/skill")
        );
        assert!(stored["instructionSkills"][0].get("skillName").is_none());
    }

    #[tokio::test]
    async fn skills_config_save_rejects_config_data_wrappers() {
        for wrapper in ["config", "data"] {
            let state = test_state();
            let app = Router::new().merge(routes()).with_state(state);

            let (status, body) = post_json_with_status(
                app,
                "/settings/skills/config/save",
                serde_json::json!({
                    wrapper: {
                        "instructionSkills": [
                            {
                                "skillId": "wrapped-skill"
                            }
                        ]
                    }
                }),
            )
            .await;

            assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
            assert!(
                body["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("不能包裹在 config/data 中"),
                "unexpected body: {body}"
            );
        }
    }

    #[tokio::test]
    async fn skill_library_filters_unavailable_local_instruction_skills() {
        let state = test_state();
        let valid_dir = tempfile::tempdir().expect("temp skill dir should create");
        std::fs::write(
            valid_dir.path().join("SKILL.md"),
            "# valid-skill\n\n请输出 valid-skill。\n",
        )
        .expect("skill markdown should write");
        let missing_dir = valid_dir.path().join("missing-skill");
        state.settings_store.set_section(
            "skillsConfig",
            serde_json::json!({
                "instructionSkills": [
                    {
                        "skillId": "valid-skill",
                        "name": "valid-skill",
                        "directoryPath": valid_dir.path().to_string_lossy().to_string()
                    },
                    {
                        "skillId": "missing-skill",
                        "name": "missing-skill",
                        "directoryPath": missing_dir.to_string_lossy().to_string()
                    }
                ]
            }),
        );
        let app = Router::new().merge(routes()).with_state(state);

        let body = get_json(app, "/settings/skills/library").await;
        let skills = body["skills"]
            .as_array()
            .expect("skills should be an array");

        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0]["skillId"], serde_json::json!("valid-skill"));
        assert_eq!(skills[0]["installed"], serde_json::json!(true));
        assert!(skills[0].get("directoryPath").is_none());
    }

    #[tokio::test]
    async fn instruction_skill_preview_rejects_unavailable_source() {
        let state = test_state();
        let missing_dir =
            std::env::temp_dir().join(format!("magi-missing-skill-{}", epoch_ms_now()));
        state.settings_store.set_section(
            "skillsConfig",
            serde_json::json!({
                "instructionSkills": [
                    {
                        "skillId": "missing-skill",
                        "name": "missing-skill",
                        "directoryPath": missing_dir.to_string_lossy().to_string()
                    }
                ]
            }),
        );

        let error = get_instruction_skill_preview(
            State(state),
            Query(HashMap::from([(
                "skillId".to_string(),
                "missing-skill".to_string(),
            )])),
        )
        .await
        .expect_err("missing skill source should be rejected");

        assert_eq!(error.message(), "技能源不可用，请重新导入该 Skill");
        assert!(!error.message().contains("magi-missing-skill"));
    }

    #[tokio::test]
    async fn instruction_skill_operations_reject_legacy_name_fields() {
        let app = Router::new().merge(routes()).with_state(test_state());

        let (update_status, update_body) = post_json_with_status(
            app,
            "/settings/skills/update",
            serde_json::json!({ "skillName": "legacy-skill" }),
        )
        .await;
        assert_eq!(
            update_status,
            StatusCode::BAD_REQUEST,
            "unexpected body: {update_body}"
        );
        assert!(
            update_body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("skillId 不能为空"),
            "unexpected body: {update_body}"
        );

        let preview_error = get_instruction_skill_preview(
            State(test_state()),
            Query(HashMap::from([(
                "skillName".to_string(),
                "legacy-skill".to_string(),
            )])),
        )
        .await
        .expect_err("legacy preview key should be rejected");
        assert_eq!(preview_error.message(), "skillId 不能为空");
    }

    #[tokio::test]
    async fn custom_tool_add_strips_workspace_session_scope_fields() {
        let state = test_state();
        let app = Router::new().merge(routes()).with_state(state.clone());

        let body = post_json(
            app,
            "/settings/skills/custom-tool/add",
            serde_json::json!({
                "name": "example-tool",
                "workspaceId": "workspace-old",
                "workspace_path": "/tmp/old",
                "sessionId": "session-old"
            }),
        )
        .await;

        assert_eq!(body["added"], true);
        let stored = state.settings_store.get_section("skillsConfig");
        assert_scope_fields_absent(&stored["customTools"][0]);
        assert_eq!(
            stored["customTools"][0]["name"],
            serde_json::json!("example-tool")
        );
        assert!(stored["customTools"][0].get("toolName").is_none());
    }

    #[tokio::test]
    async fn custom_tool_add_rejects_tool_wrapper() {
        let app = Router::new().merge(routes()).with_state(test_state());

        let (status, body) = post_json_with_status(
            app,
            "/settings/skills/custom-tool/add",
            serde_json::json!({
                "tool": {
                    "name": "example-tool"
                }
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("不能包裹在 tool 中"),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn custom_tool_add_rejects_legacy_tool_name_field() {
        let app = Router::new().merge(routes()).with_state(test_state());

        let (status, body) = post_json_with_status(
            app,
            "/settings/skills/custom-tool/add",
            serde_json::json!({
                "toolName": "legacy-tool-name"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("name 不能为空"),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn installed_skill_remove_requires_skill_id_contract() {
        for payload in [
            serde_json::json!({
                "source": "custom",
                "toolName": "legacy-tool"
            }),
            serde_json::json!({
                "source": "custom",
                "name": "legacy-tool"
            }),
            serde_json::json!({
                "source": "instruction",
                "skillName": "legacy-skill"
            }),
            serde_json::json!({
                "source": "instruction",
                "name": "legacy-skill"
            }),
        ] {
            let app = Router::new().merge(routes()).with_state(test_state());

            let (status, body) =
                post_json_with_status(app, "/settings/skills/remove", payload).await;

            assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
            assert!(
                body["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("skillId 不能为空"),
                "unexpected body: {body}"
            );
        }
    }

    #[tokio::test]
    async fn mcp_tools_routes_return_recoverable_disconnected_marker() {
        for path in ["/settings/mcp/tools", "/settings/mcp/tools/refresh"] {
            let app = Router::new().merge(routes()).with_state(test_state());
            let body = post_json(app, path, serde_json::json!({ "serverId": "missing-mcp" })).await;

            assert_eq!(body["serverId"], "missing-mcp");
            assert_eq!(body["connected"], false);
            assert_eq!(body["health"], "disconnected");
            assert_eq!(body["error"], MCP_CONNECTION_FAILED_MARKER);
            assert_eq!(body["toolCount"], 0);
            assert_eq!(body["tools"].as_array().map(Vec::len), Some(0));
        }
    }

    #[tokio::test]
    async fn disabled_mcp_server_routes_do_not_report_connection_error() {
        let state = test_state();
        state.settings_store.upsert_array_entry(
            "mcpServers",
            "id",
            &serde_json::json!({
                "id": "disabled-mcp",
                "name": "disabled-mcp",
                "command": "node",
                "enabled": false,
                "error": "/private/path/raw transport error"
            }),
        );

        let app = Router::new().merge(routes()).with_state(state.clone());
        let list = get_json(app, "/settings/mcp").await;
        assert_eq!(list["servers"][0]["connected"], false);
        assert_eq!(list["servers"][0]["health"], MCP_DISABLED_HEALTH);
        assert!(list["servers"][0].get("error").is_none());

        for path in ["/settings/mcp/tools", "/settings/mcp/tools/refresh"] {
            let app = Router::new().merge(routes()).with_state(state.clone());
            let body =
                post_json(app, path, serde_json::json!({ "serverId": "disabled-mcp" })).await;

            assert_eq!(body["serverId"], "disabled-mcp");
            assert_eq!(body["connected"], false);
            assert_eq!(body["health"], MCP_DISABLED_HEALTH);
            assert!(body.get("error").is_none());
            assert_eq!(body["toolCount"], 0);
            assert_eq!(body["tools"].as_array().map(Vec::len), Some(0));
        }
    }

    #[test]
    fn skill_runtime_errors_keep_private_details_out_of_response() {
        let repository_error =
            skill_repository_error("https://github.com/private/repo", "读取仓库失败", "raw 404");
        let cache_error = skill_cache_error(
            "缓存失败",
            std::path::Path::new("/private/cache/path"),
            "permission denied",
        );

        assert_eq!(repository_error.message(), SKILL_REPOSITORY_PUBLIC_ERROR);
        assert_eq!(cache_error.message(), SKILL_CACHE_PUBLIC_ERROR);
        assert!(!repository_error.message().contains("github.com"));
        assert!(!cache_error.message().contains("/private/cache/path"));
    }
}
