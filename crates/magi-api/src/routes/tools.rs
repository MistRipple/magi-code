use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use magi_core::{SessionId, WorkspaceId};
use magi_tool_runtime::ToolExecutionContext;
use serde::Deserialize;
use std::path::PathBuf;

use crate::{errors::ApiError, state::ApiState};

pub fn routes() -> Router<ApiState> {
    Router::new().route("/tools/catalog", get(get_tool_catalog))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolCatalogQuery {
    #[serde(default, alias = "include_internal")]
    include_internal: Option<bool>,
    #[serde(default, alias = "include_schema")]
    include_schema: Option<bool>,
    #[serde(default, alias = "include_external")]
    include_external: Option<bool>,
    #[serde(default, alias = "include_mcp_servers")]
    include_mcp_servers: Option<bool>,
    #[serde(default, alias = "include_agent_roles")]
    include_agent_roles: Option<bool>,
    #[serde(default, alias = "session_id")]
    session_id: Option<String>,
    #[serde(default, alias = "workspace_id")]
    workspace_id: Option<String>,
    #[serde(default, alias = "workspace_path")]
    workspace_path: Option<String>,
}

async fn get_tool_catalog(
    State(state): State<ApiState>,
    Query(query): Query<ToolCatalogQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let context = query.tool_context();
    let input = query.catalog_input();
    Ok(Json(state.tool_catalog_json(&input.to_string(), &context)?))
}

impl ToolCatalogQuery {
    fn catalog_input(&self) -> serde_json::Value {
        serde_json::json!({
            "includeInternal": self.include_internal.unwrap_or(false),
            "includeSchema": self.include_schema.unwrap_or(false),
            "includeExternal": self.include_external.unwrap_or(true),
            "includeMcpServers": self.include_mcp_servers.unwrap_or(true),
            "includeAgentRoles": self.include_agent_roles.unwrap_or(true),
        })
    }

    fn tool_context(&self) -> ToolExecutionContext {
        ToolExecutionContext {
            session_id: trimmed_non_empty(self.session_id.as_deref()).map(SessionId::new),
            workspace_id: trimmed_non_empty(self.workspace_id.as_deref()).map(WorkspaceId::new),
            working_directory: trimmed_non_empty(self.workspace_path.as_deref()).map(PathBuf::from),
            ..ToolExecutionContext::default()
        }
    }
}

fn trimmed_non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_tool_runtime::ToolRegistry;
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_state_with_tool_registry() -> ApiState {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let governance = Arc::new(GovernanceService::default());
        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        ApiState::new(
            "magi-tools-test",
            event_bus,
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            governance,
        )
        .with_tool_registry(tool_registry)
    }

    async fn get_json(app: Router, path: &str) -> (StatusCode, serde_json::Value) {
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(path)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should read");
        let body = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "response should be json: {error}; body={}",
                String::from_utf8_lossy(&bytes)
            )
        });
        (status, body)
    }

    #[tokio::test]
    async fn tools_catalog_route_reuses_tool_registry_health_source() {
        let app = Router::new()
            .merge(routes())
            .with_state(test_state_with_tool_registry());

        let (status, body) = get_json(
            app,
            "/tools/catalog?workspaceId=workspace-tools&sessionId=session-tools",
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["tool"], "tool_catalog");
        assert_eq!(body["status"], "succeeded");
        assert_eq!(body["builtin_total"], 35);
        assert_eq!(body["external_catalog_status"], "unavailable");
        assert_eq!(body["agent_role_catalog_status"], "unavailable");
        assert_eq!(
            body["runtime_dependencies"][1]["workspace_id"],
            "workspace-tools"
        );
        assert!(
            body["tools"]
                .as_array()
                .expect("tools should be an array")
                .iter()
                .any(|tool| tool["name"] == "tool_catalog")
        );
    }

    #[tokio::test]
    async fn tools_catalog_route_can_include_internal_tools_and_schema() {
        let app = Router::new()
            .merge(routes())
            .with_state(test_state_with_tool_registry());

        let (status, body) = get_json(
            app,
            "/tools/catalog?includeInternal=true&includeSchema=true",
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let process_launch = body["tools"]
            .as_array()
            .expect("tools should be an array")
            .iter()
            .find(|tool| tool["name"] == "process_launch")
            .expect("internal tool should be present when requested");
        assert_eq!(process_launch["public"], false);
        assert_eq!(process_launch["parameters_schema"]["type"], "object");
    }
}
