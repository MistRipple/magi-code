use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_bridge_client::StdioMcpBridgeClient;
use std::collections::HashMap;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
const SKILL_DOWNLOAD_PUBLIC_ERROR: &str = "Skill 下载暂不可用，请稍后重试";
const SKILL_CACHE_PUBLIC_ERROR: &str = "Skill 缓存不可保存，请检查本地权限";

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
    strip_scope_binding_fields_from_map(&mut entry);
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
        .ok_or_else(|| ApiError::InvalidInput("请选择本地 Skill 目录".to_string()))?;
    Ok(std::path::PathBuf::from(directory_path))
}

fn normalize_local_instruction_skill_request_skill_id(
    request: &serde_json::Value,
) -> Option<String> {
    ["skillId", "skillName", "name"]
        .iter()
        .find_map(|field| request.get(*field).and_then(|value| value.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn public_local_instruction_skill_entry(entry: &serde_json::Value) -> serde_json::Value {
    let mut obj = entry.as_object().cloned().unwrap_or_default();
    obj.remove("directoryPath");
    if !obj.contains_key("localSkillId") {
        if let Some(skill_name) = instruction_skill_name(entry) {
            obj.insert("localSkillId".to_string(), serde_json::json!(skill_name));
        }
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
    strip_scope_binding_fields_from_map(&mut entry);
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

    let client = StdioMcpBridgeClient::new(config);
    let tools = client.list_tools().map_err(|err| {
        tracing::warn!(
            server_id = %server_id,
            error = ?err,
            "MCP server connect failed"
        );
        ApiError::InvalidInput("MCP server 连接失败".to_string())
    })?;

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

    let tools = match client.list_tools() {
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

    let tools = match client.list_tools() {
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
        .filter(|skill| skill_loader::instruction_skill_source_available(skill))
        .filter_map(|s| instruction_skill_name(s))
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
                tracing::warn!(
                    repository_id = %repo_id,
                    url = %repo_url,
                    error = ?e,
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
        .map_err(|e| skill_repository_error(repo_url, "构建 GitHub 客户端失败", e))?;
    let resp = client
        .get(&api_url)
        .send()
        .await
        .map_err(|e| skill_repository_error(repo_url, "读取 GitHub Skill 仓库失败", e))?;
    if !resp.status().is_success() {
        let status = resp.status();
        tracing::warn!(
            repository_url = %repo_url,
            api_url = %api_url,
            status = %status,
            "GitHub Skill repository returned non-success status"
        );
        return Err(ApiError::InvalidInput(
            SKILL_REPOSITORY_PUBLIC_ERROR.to_string(),
        ));
    }
    let items: Vec<serde_json::Value> = resp
        .json()
        .await
        .map_err(|e| skill_repository_error(repo_url, "解析 GitHub Skill 列表失败", e))?;
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

    // 从仓库下载技能内容到本地缓存，运行时只读取本地目录。
    download_github_skill(&skill_id, &target_dir).await?;

    // directoryPath 仅保存在后端配置中供本地加载器读取，不作为安装响应外发。
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
    strip_scope_binding_fields_from_map(&mut entry);
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
    let name = request
        .get("skillName")
        .or_else(|| request.get("toolName"))
        .or_else(|| request.get("name"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("skillName 不能为空".to_string()))?;
    match source {
        "custom" => {
            let mut config = load_skills_config_object(&state);
            let mut custom_tools = config
                .remove("customTools")
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default();
            custom_tools.retain(|item| {
                !["toolName", "name"].iter().any(|field| {
                    item.get(*field)
                        .and_then(|value| value.as_str())
                        .is_some_and(|value| value == name)
                })
            });
            config.insert(
                "customTools".to_string(),
                serde_json::Value::Array(custom_tools),
            );
            persist_skills_config_object(&state, config);
            Ok(Json(serde_json::json!({
                "removed": true,
                "source": "custom",
                "skillName": name,
            })))
        }
        "instruction" => {
            let mut instruction_skills = load_instruction_skills(&state);
            let removed_skill_name = instruction_skills
                .iter()
                .find(|item| instruction_skill_matches(item, name))
                .and_then(instruction_skill_name)
                .unwrap_or_else(|| name.to_string());
            let removed = remove_instruction_skill_from_list(&mut instruction_skills, name);
            if !removed {
                return Err(ApiError::not_found("技能未安装", name));
            }
            persist_instruction_skills(&state, instruction_skills);
            Ok(Json(serde_json::json!({
                "removed": true,
                "source": "instruction",
                "skillName": removed_skill_name,
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

async fn download_github_skill(
    skill_id: &str,
    target_dir: &std::path::Path,
) -> Result<(), ApiError> {
    let client = reqwest::Client::builder()
        .user_agent("magi-agent")
        .build()
        .map_err(|e| skill_download_error(skill_id, "构建 Skill 下载客户端失败", e))?;

    let branches = vec!["main", "master"];

    // Create target directory
    if !target_dir.exists() {
        std::fs::create_dir_all(target_dir)
            .map_err(|e| skill_cache_error("创建 Skill 缓存目录失败", target_dir, e))?;
    }

    let mut prompt_downloaded = false;
    let mut download_failed = false;

    // Try fetching prompt.md or README.md
    for branch in &branches {
        let prompt_url = format!(
            "https://raw.githubusercontent.com/{}/{}/prompt.md",
            skill_id, branch
        );
        match client.get(&prompt_url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.text().await {
                Ok(text) => {
                    let prompt_path = target_dir.join("prompt.md");
                    std::fs::write(&prompt_path, text).map_err(|e| {
                        skill_cache_error("写入 Skill prompt 失败", &prompt_path, e)
                    })?;
                    prompt_downloaded = true;
                    download_optional_skill_config(&client, skill_id, branch, target_dir).await;
                    break;
                }
                Err(error) => {
                    download_failed = true;
                    tracing::warn!(
                        skill_id,
                        url = %prompt_url,
                        error = %error,
                        "Skill prompt body read failed"
                    );
                }
            },
            Ok(_) => {}
            Err(error) => {
                download_failed = true;
                tracing::warn!(
                    skill_id,
                    url = %prompt_url,
                    error = %error,
                    "Skill prompt download failed"
                );
            }
        }

        let readme_url = format!(
            "https://raw.githubusercontent.com/{}/{}/README.md",
            skill_id, branch
        );
        match client.get(&readme_url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.text().await {
                Ok(text) => {
                    let readme_path = target_dir.join("README.md");
                    std::fs::write(&readme_path, text).map_err(|e| {
                        skill_cache_error("写入 Skill README 失败", &readme_path, e)
                    })?;
                    prompt_downloaded = true;
                    download_optional_skill_config(&client, skill_id, branch, target_dir).await;
                    break;
                }
                Err(error) => {
                    download_failed = true;
                    tracing::warn!(
                        skill_id,
                        url = %readme_url,
                        error = %error,
                        "Skill README body read failed"
                    );
                }
            },
            Ok(_) => {}
            Err(error) => {
                download_failed = true;
                tracing::warn!(
                    skill_id,
                    url = %readme_url,
                    error = %error,
                    "Skill README download failed"
                );
            }
        }
    }

    if !prompt_downloaded {
        if download_failed {
            return Err(ApiError::InvalidInput(
                SKILL_DOWNLOAD_PUBLIC_ERROR.to_string(),
            ));
        }
        return Err(ApiError::InvalidInput(
            "该技能仓库缺少可导入的说明文件".to_string(),
        ));
    }

    Ok(())
}

async fn download_optional_skill_config(
    client: &reqwest::Client,
    skill_id: &str,
    branch: &str,
    target_dir: &Path,
) {
    let config_url = format!(
        "https://raw.githubusercontent.com/{}/{}/config.json",
        skill_id, branch
    );
    if let Ok(response) = client.get(&config_url).send().await
        && response.status().is_success()
        && let Ok(text) = response.text().await
    {
        let _ = std::fs::write(target_dir.join("config.json"), text);
    }
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

fn skill_download_error(skill_id: &str, action: &'static str, error: impl Display) -> ApiError {
    tracing::warn!(
        action,
        skill_id,
        error = %error,
        "Skill download failed"
    );
    ApiError::InvalidInput(SKILL_DOWNLOAD_PUBLIC_ERROR.to_string())
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
        .or_else(|| params.get("skillName"))
        .or_else(|| params.get("name"))
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
                        "workspaceId": "workspace-old",
                        "session_id": "session-old"
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
        let download_error = skill_download_error("private/repo", "下载失败", "connection reset");
        let cache_error = skill_cache_error(
            "缓存失败",
            std::path::Path::new("/private/cache/path"),
            "permission denied",
        );

        assert_eq!(repository_error.message(), SKILL_REPOSITORY_PUBLIC_ERROR);
        assert_eq!(download_error.message(), SKILL_DOWNLOAD_PUBLIC_ERROR);
        assert_eq!(cache_error.message(), SKILL_CACHE_PUBLIC_ERROR);
        assert!(!repository_error.message().contains("github.com"));
        assert!(!download_error.message().contains("connection reset"));
        assert!(!cache_error.message().contains("/private/cache/path"));
    }

    #[tokio::test]
    async fn download_github_skill_cache_error_uses_public_message() {
        let temp = tempfile::tempdir().expect("temp dir should create");
        let file_path = temp.path().join("not-a-directory");
        std::fs::write(&file_path, "occupied").expect("test file should write");
        let blocked_target_dir = file_path.join("skill-cache");

        let error = download_github_skill("owner/repo", &blocked_target_dir)
            .await
            .expect_err("cache path should fail before network request");

        assert_eq!(error.message(), SKILL_CACHE_PUBLIC_ERROR);
    }
}
