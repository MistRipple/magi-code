use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use magi_bridge_client::{McpServerConfig, StdioMcpBridgeClient};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::{errors::ApiError, skill_loader, state::ApiState};

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
            "/settings/skills/scan-local",
            post(scan_local_skill_directory),
        )
        .route("/settings/skills/config/save", post(save_skills_config))
        .route("/settings/skills/custom-tool/add", post(add_custom_tool))
        .route(
            "/settings/skills/custom-tool/remove",
            post(remove_custom_tool),
        )
        .route(
            "/settings/skills/instruction/remove",
            post(remove_instruction_skill),
        )
        .route("/settings/skills/update", post(update_skill))
        .route("/settings/skills/update-all", post(update_all_skills))
}

fn unwrap_request_value<'a>(
    request: &'a serde_json::Value,
    keys: &[&str],
) -> &'a serde_json::Value {
    for key in keys {
        if let Some(value) = request.get(*key) {
            return value;
        }
    }
    request
}

fn server_entry_id(entry: &serde_json::Value) -> Option<&str> {
    entry
        .get("id")
        .and_then(|v| v.as_str())
        .or_else(|| entry.get("serverId").and_then(|v| v.as_str()))
}

fn normalize_mcp_server_entry(request: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    let raw = unwrap_request_value(request, &["server", "updates"]);
    let server_id = raw
        .get("id")
        .and_then(|v| v.as_str())
        .or_else(|| raw.get("serverId").and_then(|v| v.as_str()))
        .or_else(|| request.get("serverId").and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("serverId 不能为空".to_string()))?;
    let mut entry = raw.as_object().cloned().unwrap_or_default();
    entry.insert("id".to_string(), serde_json::json!(server_id));
    entry.insert("serverId".to_string(), serde_json::json!(server_id));
    if entry
        .get("name")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        entry.insert("name".to_string(), serde_json::json!(server_id));
    }
    let command = entry
        .get("command")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| ApiError::InvalidInput("MCP server 配置中缺少 command".to_string()))?;
    entry.insert("command".to_string(), serde_json::json!(command));
    entry.insert("type".to_string(), serde_json::json!("stdio"));
    entry.remove("url");
    entry.remove("headers");
    Ok(serde_json::Value::Object(entry))
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
    let raw = unwrap_request_value(request, &["skill", "updates"]);
    let skill_id = raw
        .get("skillId")
        .and_then(|value| value.as_str())
        .or_else(|| raw.get("skillName").and_then(|value| value.as_str()))
        .or_else(|| raw.get("name").and_then(|value| value.as_str()))
        .or_else(|| request.get("skillId").and_then(|value| value.as_str()))
        .or_else(|| request.get("skillName").and_then(|value| value.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("skillId 不能为空".to_string()))?;
    let mut entry = raw.as_object().cloned().unwrap_or_default();
    entry.insert("name".to_string(), serde_json::json!(skill_id));
    entry.insert("skillName".to_string(), serde_json::json!(skill_id));
    entry.insert("skillId".to_string(), serde_json::json!(skill_id));
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
        .ok_or_else(|| ApiError::InvalidInput("directoryPath 无法解析技能名称".to_string()))?;
    let skill_name = dir
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("directoryPath 无法解析技能名称".to_string()))?;
    let metadata = read_local_skill_metadata(dir);
    let description = read_local_skill_description(dir);
    Ok(serde_json::json!({
        "name": skill_name,
        "skillName": skill_name,
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
    if config_path.is_file() {
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(desc) = json.get("description").and_then(|v| v.as_str()) {
                    let desc = desc.trim();
                    if !desc.is_empty() {
                        return desc.chars().take(200).collect();
                    }
                }
            }
        }
    }
    for filename in &["SKILL.md", "prompt.md", "README.md"] {
        let path = dir.join(filename);
        if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                for line in content.lines() {
                    let trimmed = line.trim().trim_start_matches('#').trim();
                    if !trimmed.is_empty() && trimmed != "---" {
                        return trimmed.chars().take(200).collect();
                    }
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
        return Err(ApiError::InvalidInput(format!(
            "目录不存在: {}",
            root_dir.display()
        )));
    }
    if !root_dir.is_dir() {
        return Err(ApiError::InvalidInput(format!(
            "路径不是目录: {}",
            root_dir.display()
        )));
    }

    if is_local_skill_dir(root_dir) {
        return Ok(vec![build_local_instruction_skill_entry(root_dir)?]);
    }

    let mut candidates = std::fs::read_dir(root_dir)
        .map_err(|e| ApiError::internal_assembly("读取本地技能目录失败", e))?
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
        !["skillName", "name", "skillId"].iter().any(|field| {
            item.get(*field)
                .and_then(|value| value.as_str())
                .is_some_and(|value| value == skill_name)
        })
    });
    instruction_skills.len() != before_len
}

fn instruction_skill_name(item: &serde_json::Value) -> Option<String> {
    ["skillName", "name", "skillId"]
        .iter()
        .find_map(|field| item.get(*field).and_then(|value| value.as_str()))
        .map(|value| value.to_string())
}

fn instruction_skill_matches(item: &serde_json::Value, skill_name: &str) -> bool {
    ["skillName", "name", "skillId"].iter().any(|field| {
        item.get(*field)
            .and_then(|value| value.as_str())
            .is_some_and(|value| value == skill_name)
    })
}

fn normalize_local_instruction_skill_request_path(
    request: &serde_json::Value,
) -> Result<std::path::PathBuf, ApiError> {
    let directory_path = request
        .get("directoryPath")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("directoryPath 不能为空".to_string()))?;
    Ok(std::path::PathBuf::from(directory_path))
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

fn normalize_repository_entry(request: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    let raw = unwrap_request_value(request, &["repository", "updates"]);
    let repository_url = raw
        .get("url")
        .and_then(|value| value.as_str())
        .or_else(|| request.get("url").and_then(|value| value.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let repository_id = raw
        .get("repositoryId")
        .and_then(|value| value.as_str())
        .or_else(|| request.get("repositoryId").and_then(|value| value.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or(repository_url)
        .ok_or_else(|| ApiError::InvalidInput("repositoryId 或 url 不能为空".to_string()))?;
    let mut entry = raw.as_object().cloned().unwrap_or_default();
    entry.insert("repositoryId".to_string(), serde_json::json!(repository_id));
    if entry
        .get("url")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        if let Some(url) = repository_url {
            entry.insert("url".to_string(), serde_json::json!(url));
        }
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
    state
        .settings_snapshot_json()
        .get("mcpServers")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
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
    let normalized = normalize_mcp_server_entry(&request)?;
    state
        .settings_store
        .upsert_array_entry("mcpServers", "id", &normalized);
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

fn build_mcp_config(entry: &serde_json::Value) -> Option<McpServerConfig> {
    let command = entry.get("command")?.as_str()?.to_string();
    if command.is_empty() {
        return None;
    }
    let args: Vec<String> = entry
        .get("args")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let working_directory = entry
        .get("workingDirectory")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from);
    let env: BTreeMap<String, String> = entry
        .get("env")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Some(McpServerConfig {
        command,
        args,
        working_directory,
        env,
    })
}

fn find_server_entry(state: &ApiState, server_id: &str) -> Option<serde_json::Value> {
    canonical_mcp_servers(state)
        .into_iter()
        .find(|entry| server_entry_id(entry).is_some_and(|id| id == server_id))
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

    let config = build_mcp_config(&entry)
        .ok_or_else(|| ApiError::InvalidInput("MCP server 配置中缺少 command".to_string()))?;

    let client = StdioMcpBridgeClient::new(config);
    let tools = client
        .list_tools()
        .map_err(|e| ApiError::internal_assembly("连接 MCP server 失败", format!("{e:?}")))?;

    let client = Arc::new(client);
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

    let client = {
        let pool = state
            .mcp_connections()
            .read()
            .expect("mcp connections read lock poisoned");
        pool.get(server_id).cloned()
    };

    let Some(client) = client else {
        return Ok(Json(serde_json::json!({
            "tools": [],
            "connected": false,
            "serverId": server_id,
        })));
    };

    let tools = client
        .list_tools()
        .map_err(|e| ApiError::internal_assembly("获取 MCP tools 失败", format!("{e:?}")))?;

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

    let client = {
        let pool = state
            .mcp_connections()
            .read()
            .expect("mcp connections read lock poisoned");
        pool.get(server_id).cloned()
    };

    let Some(client) = client else {
        return Ok(Json(serde_json::json!({
            "tools": [],
            "connected": false,
            "serverId": server_id,
        })));
    };

    let tools = client
        .list_tools()
        .map_err(|e| ApiError::internal_assembly("刷新 MCP tools 失败", format!("{e:?}")))?;

    Ok(Json(serde_json::json!({
        "tools": tools,
        "connected": true,
        "serverId": server_id,
    })))
}

// ─── Repositories ───────────────────────────────────────────────────────────

async fn list_repositories(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let repos = state
        .settings_store
        .get_section("repositories")
        .as_array()
        .cloned()
        .unwrap_or_default();
    Json(serde_json::json!({ "repositories": repos }))
}

async fn add_repository(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let normalized = normalize_repository_entry(&request)?;
    state
        .settings_store
        .upsert_array_entry("repositories", "repositoryId", &normalized);
    Ok(Json(serde_json::json!({ "added": true })))
}

async fn update_repository(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let normalized = normalize_repository_entry(&request)?;
    state
        .settings_store
        .upsert_array_entry("repositories", "repositoryId", &normalized);
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
    state
        .settings_store
        .remove_array_entry("repositories", "repositoryId", repo_id);
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

    let mut updated_repos = repos;
    let mut entry = updated_repos[pos].as_object().cloned().unwrap_or_default();
    entry.insert(
        "lastRefreshed".to_string(),
        serde_json::json!(epoch_ms_now()),
    );
    updated_repos[pos] = serde_json::Value::Object(entry);
    state
        .settings_store
        .set_section("repositories", serde_json::Value::Array(updated_repos));

    Ok(Json(serde_json::json!({ "refreshed": true })))
}

// ─── Skills ─────────────────────────────────────────────────────────────────

async fn list_skills(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let installed_skills = load_instruction_skills(&state);
    let installed_names: std::collections::HashSet<String> = installed_skills
        .iter()
        .filter_map(|s| instruction_skill_name(s))
        .collect();
    let mut all_skills: Vec<serde_json::Value> = installed_skills
        .into_iter()
        .map(|skill| {
            let mut entry = skill.as_object().cloned().unwrap_or_default();
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
    let mut failed_repos: Vec<serde_json::Value> = Vec::new();
    for repo in &repos {
        let repo_url = repo.get("url").and_then(|v| v.as_str()).unwrap_or_default();
        let repo_id = repo
            .get("repositoryId")
            .and_then(|v| v.as_str())
            .unwrap_or(repo_url);
        if repo_url.is_empty() {
            continue;
        }
        match fetch_github_repo_skills(repo_url).await {
            Ok(remote) => {
                for mut rs in remote {
                    let nm = rs
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let fl = rs
                        .get("fullName")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let inst = installed_names.contains(&nm) || installed_names.contains(&fl);
                    if let Some(o) = rs.as_object_mut() {
                        o.insert("installed".into(), serde_json::json!(inst));
                        o.insert("repositoryId".into(), serde_json::json!(repo_id));
                        o.insert("repositoryName".into(), serde_json::json!(repo_url));
                    }
                    if !inst {
                        all_skills.push(rs);
                    }
                }
            }
            Err(e) => {
                failed_repos.push(serde_json::json!({"repositoryId": repo_id, "url": repo_url, "error": format!("{:?}", e)}));
            }
        }
    }
    Json(serde_json::json!({"skills": all_skills, "failedRepositories": failed_repos}))
}

async fn fetch_github_repo_skills(repo_url: &str) -> Result<Vec<serde_json::Value>, ApiError> {
    let path = repo_url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit("github.com/")
        .next()
        .unwrap_or_default();
    if path.is_empty() || !path.contains('/') {
        return Ok(vec![]);
    }
    let api_url = format!("https://api.github.com/repos/{}/contents/skills", path);
    let client = reqwest::Client::builder()
        .user_agent("magi-daemon")
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| ApiError::internal_assembly("HTTP client error", e))?;
    let resp = client
        .get(&api_url)
        .send()
        .await
        .map_err(|e| ApiError::internal_assembly("GitHub API failed", e))?;
    if !resp.status().is_success() {
        return Err(ApiError::InvalidInput(format!(
            "GitHub API {} for {}",
            resp.status(),
            api_url
        )));
    }
    let items: Vec<serde_json::Value> = resp
        .json()
        .await
        .map_err(|e| ApiError::internal_assembly("Parse GitHub response failed", e))?;
    let mut skills = Vec::new();
    for item in items {
        let t = item
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let n = item
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if t == "dir" && !n.starts_with('.') {
            let full = format!("{}/{}", path, n);
            skills.push(serde_json::json!({"name": n, "skillName": n, "skillId": full, "fullName": full, "source": "repository"}));
        }
    }
    Ok(skills)
}

async fn install_skill(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut normalized = normalize_instruction_skill_entry(&request)?;

    let skill_id = normalized
        .get("skillId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let cache_dir = state
        .runtime_persistence()
        .and_then(|p| p.state_root())
        .map(|root| root.join("skills_cache"))
        .unwrap_or_else(|| std::env::temp_dir().join("magi_skills_cache"));

    let target_dir = cache_dir.join(skill_id.replace('/', "_"));

    // Download the skill from github
    download_github_skill(&skill_id, &target_dir).await?;

    // Inject the directoryPath so the local loader can read it
    if let Some(obj) = normalized.as_object_mut() {
        obj.insert(
            "directoryPath".to_string(),
            serde_json::json!(target_dir.to_string_lossy().to_string()),
        );
    }

    let mut instruction_skills = load_instruction_skills(&state);
    upsert_named_object_array_entry(
        &mut instruction_skills,
        normalized,
        &["skillId", "skillName", "name"],
    );
    persist_instruction_skills(&state, instruction_skills);
    Ok(Json(serde_json::json!({ "installed": true })))
}

async fn install_local_skill(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let root_dir = normalize_local_instruction_skill_request_path(&request)?;
    let entries = scan_local_instruction_skill_entries(&root_dir)?;
    if entries.is_empty() {
        return Err(ApiError::InvalidInput(format!(
            "所选目录下未发现可导入的技能子目录: {}",
            root_dir.display()
        )));
    }

    let mut instruction_skills = load_instruction_skills(&state);
    for entry in &entries {
        upsert_named_object_array_entry(
            &mut instruction_skills,
            entry.clone(),
            &["skillId", "skillName", "name"],
        );
    }
    persist_instruction_skills(&state, instruction_skills);

    Ok(Json(serde_json::json!({
        "installed": true,
        "count": entries.len(),
        "skills": entries,
    })))
}

async fn scan_local_skill_directory(
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let root_dir = normalize_local_instruction_skill_request_path(&request)?;
    let entries = scan_local_instruction_skill_entries(&root_dir)?;
    Ok(Json(serde_json::json!({
        "directoryPath": root_dir.to_string_lossy(),
        "skills": entries,
        "count": entries.len(),
    })))
}

async fn save_skills_config(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let section = unwrap_request_value(&request, &["config"]).clone();
    let config = section.as_object().cloned().unwrap_or_default();
    persist_skills_config_object(&state, config);
    Ok(Json(serde_json::json!({ "saved": true })))
}

async fn add_custom_tool(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let raw = unwrap_request_value(&request, &["tool"]);
    let mut entry = raw.as_object().cloned().unwrap_or_default();
    let tool_name = entry
        .get("name")
        .and_then(|v| v.as_str())
        .or_else(|| entry.get("toolName").and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("toolName 不能为空".to_string()))?
        .to_string();
    entry.insert("name".to_string(), serde_json::json!(tool_name));
    entry.insert("toolName".to_string(), serde_json::json!(tool_name));

    let mut config = load_skills_config_object(&state);
    let mut custom_tools = config
        .remove("customTools")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    upsert_named_object_array_entry(
        &mut custom_tools,
        serde_json::Value::Object(entry),
        &["toolName", "name"],
    );
    config.insert(
        "customTools".to_string(),
        serde_json::Value::Array(custom_tools),
    );
    persist_skills_config_object(&state, config);
    Ok(Json(serde_json::json!({ "added": true })))
}

async fn remove_custom_tool(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let tool_name = request
        .get("toolName")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let mut config = load_skills_config_object(&state);
    let mut custom_tools = config
        .remove("customTools")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    custom_tools.retain(|item| {
        !["toolName", "name"].iter().any(|field| {
            item.get(*field)
                .and_then(|value| value.as_str())
                .is_some_and(|value| value == tool_name)
        })
    });
    config.insert(
        "customTools".to_string(),
        serde_json::Value::Array(custom_tools),
    );
    persist_skills_config_object(&state, config);
    Ok(Json(serde_json::json!({ "removed": true })))
}

async fn remove_instruction_skill(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let skill_name = request
        .get("skillName")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("skillName 不能为空".to_string()))?;
    let mut instruction_skills = load_instruction_skills(&state);
    let removed_skill_name = instruction_skills
        .iter()
        .find(|item| instruction_skill_matches(item, skill_name))
        .and_then(instruction_skill_name)
        .unwrap_or_else(|| skill_name.to_string());
    let removed = remove_instruction_skill_from_list(&mut instruction_skills, skill_name);
    if !removed {
        return Err(ApiError::not_found("技能未安装", skill_name));
    }
    persist_instruction_skills(&state, instruction_skills);
    Ok(Json(serde_json::json!({
        "removed": true,
        "skillName": removed_skill_name,
    })))
}

async fn update_skill(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let skill_name = request
        .get("skillName")
        .and_then(|v| v.as_str())
        .or_else(|| request.get("skillId").and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("skillName 不能为空".to_string()))?;

    let mut instruction_skills = load_instruction_skills(&state);
    let position = instruction_skills.iter().position(|item| {
        ["skillName", "name", "skillId"].iter().any(|field| {
            item.get(*field)
                .and_then(|v| v.as_str())
                .is_some_and(|v| v == skill_name)
        })
    });

    let Some(pos) = position else {
        return Err(ApiError::not_found("技能未安装", skill_name));
    };

    let skill = &instruction_skills[pos];
    if let Some(dir_path) = skill.get("directoryPath").and_then(|v| v.as_str()) {
        let path = std::path::Path::new(dir_path);
        if !path.is_dir() {
            return Err(ApiError::InvalidInput(format!(
                "技能目录不存在: {dir_path}"
            )));
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

async fn download_github_skill(
    skill_id: &str,
    target_dir: &std::path::Path,
) -> Result<(), ApiError> {
    let client = reqwest::Client::builder()
        .user_agent("magi-agent")
        .build()
        .map_err(|e| ApiError::internal_assembly("HTTP client build failed", e))?;

    let branches = vec!["main", "master"];

    // Create target directory
    if !target_dir.exists() {
        std::fs::create_dir_all(target_dir)
            .map_err(|e| ApiError::internal_assembly("Failed to create skill cache dir", e))?;
    }

    let mut prompt_downloaded = false;

    // Try fetching prompt.md or README.md
    for branch in &branches {
        let prompt_url = format!(
            "https://raw.githubusercontent.com/{}/{}/prompt.md",
            skill_id, branch
        );
        if let Ok(resp) = client.get(&prompt_url).send().await {
            if resp.status().is_success() {
                if let Ok(text) = resp.text().await {
                    std::fs::write(target_dir.join("prompt.md"), text)
                        .map_err(|e| ApiError::internal_assembly("Failed to write prompt.md", e))?;
                    prompt_downloaded = true;

                    // Also try to download config.json from the same branch
                    let config_url = format!(
                        "https://raw.githubusercontent.com/{}/{}/config.json",
                        skill_id, branch
                    );
                    if let Ok(c_resp) = client.get(&config_url).send().await {
                        if c_resp.status().is_success() {
                            if let Ok(c_text) = c_resp.text().await {
                                let _ = std::fs::write(target_dir.join("config.json"), c_text);
                            }
                        }
                    }
                    break;
                }
            }
        }

        let readme_url = format!(
            "https://raw.githubusercontent.com/{}/{}/README.md",
            skill_id, branch
        );
        if let Ok(resp) = client.get(&readme_url).send().await {
            if resp.status().is_success() {
                if let Ok(text) = resp.text().await {
                    std::fs::write(target_dir.join("README.md"), text)
                        .map_err(|e| ApiError::internal_assembly("Failed to write README.md", e))?;
                    prompt_downloaded = true;

                    let config_url = format!(
                        "https://raw.githubusercontent.com/{}/{}/config.json",
                        skill_id, branch
                    );
                    if let Ok(c_resp) = client.get(&config_url).send().await {
                        if c_resp.status().is_success() {
                            if let Ok(c_text) = c_resp.text().await {
                                let _ = std::fs::write(target_dir.join("config.json"), c_text);
                            }
                        }
                    }
                    break;
                }
            }
        }
    }

    if !prompt_downloaded {
        return Err(ApiError::InvalidInput(format!(
            "Could not find prompt.md or README.md in repository {}",
            skill_id
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_mcp_server_entry_rejects_url_only_server() {
        let error = normalize_mcp_server_entry(&serde_json::json!({
            "id": "remote-server",
            "url": "https://example.test/mcp"
        }))
        .expect_err("当前运行时没有 HTTP MCP client，不应保存 URL-only 配置");

        match error {
            ApiError::InvalidInput(message) => {
                assert!(message.contains("缺少 command"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn normalize_mcp_server_entry_canonicalizes_stdio_server() {
        let entry = normalize_mcp_server_entry(&serde_json::json!({
            "id": "stdio-server",
            "command": " npx ",
            "url": "https://example.test/mcp",
            "headers": { "Authorization": "Bearer test" },
            "type": "streamable-http"
        }))
        .expect("stdio MCP server should normalize");

        assert_eq!(entry["id"], serde_json::json!("stdio-server"));
        assert_eq!(entry["serverId"], serde_json::json!("stdio-server"));
        assert_eq!(entry["command"], serde_json::json!("npx"));
        assert_eq!(entry["type"], serde_json::json!("stdio"));
        assert!(entry.get("url").is_none());
        assert!(entry.get("headers").is_none());
    }
}
