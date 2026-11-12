use super::*;

pub(super) fn canonical_mcp_servers(state: &ApiState) -> Vec<serde_json::Value> {
    stored_mcp_servers(state)
        .into_iter()
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

pub(super) fn stored_mcp_servers(state: &ApiState) -> Vec<serde_json::Value> {
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

pub(super) fn stored_mcp_server_entry(
    state: &ApiState,
    server_id: &str,
) -> Option<serde_json::Value> {
    stored_mcp_servers(state)
        .into_iter()
        .find(|entry| mcp_server_entry_id(entry).is_some_and(|id| id == server_id))
}

pub(super) async fn list_mcp_servers(State(state): State<ApiState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "servers": canonical_mcp_servers(&state) }))
}

pub(super) async fn add_mcp_server(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let normalized = normalize_mcp_server_entry(&request)?;
    let server_id = mcp_server_entry_id(&normalized)
        .map(str::to_string)
        .ok_or_else(|| ApiError::InvalidInput("MCP server id 不能为空".to_string()))?;
    state
        .settings_store
        .upsert_array_entry("mcpServers", "id", &normalized)
        .map_err(settings_persistence_error)?;
    remove_mcp_connection(&state, &server_id);
    Ok(Json(serde_json::json!({ "added": true })))
}

pub(super) async fn update_mcp_server(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let requested_server_id = mcp_server_entry_id(&request).map(str::to_string);
    let normalized = normalize_mcp_server_entry(&request)?;
    let server_id = requested_server_id
        .or_else(|| mcp_server_entry_id(&normalized).map(str::to_string))
        .unwrap_or_default()
        .trim()
        .to_string();
    state
        .settings_store
        .upsert_array_entry("mcpServers", "id", &normalized)
        .map_err(settings_persistence_error)?;
    remove_mcp_connection(&state, &server_id);
    Ok(Json(serde_json::json!({ "updated": true })))
}

pub(super) async fn delete_mcp_server(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let server_id = request
        .get("serverId")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    state
        .settings_store
        .remove_array_entry("mcpServers", "id", server_id)
        .map_err(settings_persistence_error)?;
    {
        let mut pool = state
            .mcp_connections()
            .write()
            .expect("mcp connections write lock poisoned");
        pool.remove(server_id);
    }
    Ok(Json(serde_json::json!({ "deleted": true })))
}

pub(super) fn find_server_entry(state: &ApiState, server_id: &str) -> Option<serde_json::Value> {
    stored_mcp_server_entry(state, server_id)
}

pub(super) fn remove_mcp_connection(state: &ApiState, server_id: &str) {
    let mut pool = state
        .mcp_connections()
        .write()
        .expect("mcp connections write lock poisoned");
    pool.remove(server_id);
}

pub(super) fn public_mcp_error_detail(error: &str) -> String {
    magi_core::public_runtime_excerpt(error, 4_000)
}

pub(super) fn mcp_tools_unavailable_response(
    server_id: &str,
    error_detail: &str,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "tools": [],
        "connected": false,
        "health": MCP_DISCONNECTED_HEALTH,
        "error": MCP_CONNECTION_FAILED_MARKER,
        "errorDetail": public_mcp_error_detail(error_detail),
        "serverId": server_id,
        "toolCount": 0,
        "lastCheckedAt": epoch_ms_now(),
    }))
}

pub(super) fn mcp_tools_disabled_response(server_id: &str) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "tools": [],
        "connected": false,
        "health": MCP_DISABLED_HEALTH,
        "serverId": server_id,
        "toolCount": 0,
        "lastCheckedAt": epoch_ms_now(),
    }))
}

pub(super) async fn connect_configured_mcp_server(
    state: &ApiState,
    server_id: &str,
) -> Result<(Arc<McpServerClient>, Vec<McpToolInfo>), String> {
    let existing = state
        .mcp_connections()
        .read()
        .expect("mcp connections read lock poisoned")
        .get(server_id)
        .cloned();
    if let Some(client) = existing {
        match list_mcp_tools(client.clone()).await {
            Ok(tools) => return Ok((client, tools)),
            Err(error) => {
                tracing::warn!(server_id = %server_id, error = %error, "existing MCP connection is unavailable; reconnecting");
                remove_mcp_connection(state, server_id);
            }
        }
    }

    let entry = find_server_entry(state, server_id)
        .ok_or_else(|| format!("MCP server {server_id} 配置不存在"))?;
    if !mcp_server_entry_enabled(&entry) {
        remove_mcp_connection(state, server_id);
        return Err(format!("MCP server {server_id} 已禁用"));
    }
    let config = build_mcp_config_from_entry(&entry)
        .ok_or_else(|| format!("MCP server {server_id} 配置不完整"))?;
    let client = Arc::new(McpServerClient::new(config));
    let tools = list_mcp_tools(client.clone()).await?;
    state
        .mcp_connections()
        .write()
        .expect("mcp connections write lock poisoned")
        .insert(server_id.to_string(), client.clone());
    Ok((client, tools))
}

pub(super) async fn connect_mcp_server(
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

    let tools = match connect_configured_mcp_server(&state, &server_id).await {
        Ok((_, tools)) => tools,
        Err(error) => {
            tracing::warn!(
                server_id = %server_id,
                error = %error,
                "MCP server connect failed"
            );
            remove_mcp_connection(&state, &server_id);
            return Ok(mcp_tools_unavailable_response(&server_id, &error));
        }
    };

    Ok(Json(serde_json::json!({
        "connected": true,
        "health": "connected",
        "serverId": server_id,
        "toolCount": tools.len(),
        "lastCheckedAt": epoch_ms_now(),
    })))
}

pub(super) async fn disconnect_mcp_server(
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

pub(super) async fn get_mcp_tools(
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

    let tools = match connect_configured_mcp_server(&state, server_id).await {
        Ok((_, tools)) => tools,
        Err(err) => {
            tracing::warn!(
                server_id = %server_id,
                error = ?err,
                "MCP tools fetch failed"
            );
            remove_mcp_connection(&state, server_id);
            return Ok(mcp_tools_unavailable_response(server_id, &err));
        }
    };

    let tool_count = tools.len();
    Ok(Json(serde_json::json!({
        "tools": tools,
        "connected": true,
        "health": "connected",
        "serverId": server_id,
        "toolCount": tool_count,
        "lastCheckedAt": epoch_ms_now(),
    })))
}

pub(super) async fn refresh_mcp_tools(
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

    remove_mcp_connection(&state, server_id);
    let tools = match connect_configured_mcp_server(&state, server_id).await {
        Ok((_, tools)) => tools,
        Err(err) => {
            tracing::warn!(
                server_id = %server_id,
                error = ?err,
                "MCP tools refresh failed"
            );
            remove_mcp_connection(&state, server_id);
            return Ok(mcp_tools_unavailable_response(server_id, &err));
        }
    };

    let tool_count = tools.len();
    Ok(Json(serde_json::json!({
        "tools": tools,
        "connected": true,
        "health": "connected",
        "serverId": server_id,
        "toolCount": tool_count,
        "lastCheckedAt": epoch_ms_now(),
    })))
}
