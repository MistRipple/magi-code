use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_bridge_client::{McpServerClient, McpToolInfo};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Display;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::{
    errors::{ApiError, settings_persistence_error},
    mcp_config::{
        build_mcp_config_from_entry, mcp_server_entry_enabled, mcp_server_entry_id,
        normalize_mcp_server_request_entry as normalize_mcp_server_entry,
        normalize_mcp_server_snapshot_entry,
    },
    scope_binding::strip_scope_binding_fields_from_map,
    skill_loader,
    state::ApiState,
};

mod skills_config;
use skills_config::*;
mod repo_skills;
use repo_skills::*;
mod mcp_servers;
use mcp_servers::*;
mod repository_cache;
use repository_cache::*;
mod skill_library;
use skill_library::*;

const MCP_CONNECTION_FAILED_MARKER: &str = "mcp_connection_failed";
const MCP_DISABLED_HEALTH: &str = "disabled";
const MCP_DISCONNECTED_HEALTH: &str = "disconnected";
const SKILL_REPOSITORY_PUBLIC_ERROR: &str = "Skill 仓库暂不可读取";
const SKILL_CACHE_PUBLIC_ERROR: &str = "Skill 缓存不可保存，请检查本地权限";
const SKILL_REPOSITORY_URL_PUBLIC_ERROR: &str = "仅支持标准 GitHub HTTPS 仓库地址";
const SKILL_UPDATE_CHECK_TTL_MS: u64 = 6 * 60 * 60 * 1000;
static SKILL_REPOSITORY_SYNC_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

async fn list_mcp_tools(client: Arc<McpServerClient>) -> Result<Vec<McpToolInfo>, String> {
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
        .route("/settings/skills/detail", get(get_instruction_skill_detail))
        .route(
            "/settings/skills/scan-local",
            post(scan_local_skill_directory),
        )
        .route("/settings/skills/config/save", post(save_skills_config))
        .route("/settings/skills/toggle", post(toggle_skill))
        .route("/settings/skills/custom-tool/add", post(add_custom_tool))
        .route("/settings/skills/remove", post(remove_installed_skill))
        .route("/settings/skills/check-updates", post(check_skill_updates))
        .route("/settings/skills/update", post(update_skill))
        .route("/settings/skills/update-all", post(update_all_skills))
        .route("/settings/skills/rollback", post(rollback_skill))
}

// ─── MCP Servers ────────────────────────────────────────────────────────────

// ─── Repositories ───────────────────────────────────────────────────────────

// ─── Skills ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
