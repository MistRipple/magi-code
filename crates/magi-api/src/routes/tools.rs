use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use magi_core::AccessProfile;
use serde::Deserialize;
use std::str::FromStr;

use super::session_scope;
use crate::{errors::ApiError, state::ApiState};

pub fn routes() -> Router<ApiState> {
    Router::new().route("/tools/catalog", get(get_tool_catalog))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ToolCatalogQuery {
    #[serde(default)]
    include_internal: Option<bool>,
    #[serde(default)]
    include_schema: Option<bool>,
    #[serde(default)]
    include_external: Option<bool>,
    #[serde(default)]
    include_mcp_servers: Option<bool>,
    #[serde(default)]
    include_agent_roles: Option<bool>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    #[serde(default)]
    access_profile: Option<String>,
    #[serde(default)]
    refresh_environment: bool,
}

async fn get_tool_catalog(
    State(state): State<ApiState>,
    Query(query): Query<ToolCatalogQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let environment = if query.refresh_environment {
        magi_process::refresh_user_process_environment()
    } else {
        magi_process::initialize_user_process_environment()
    };
    let context = query.tool_context(&state)?;
    let input = query.catalog_input();
    let mut catalog = state.public_tool_catalog_json(&input.to_string(), &context)?;
    catalog["commandEnvironment"] = command_environment_json(environment);
    Ok(Json(catalog))
}

fn command_environment_json(
    environment: magi_process::ProcessEnvironmentSummary,
) -> serde_json::Value {
    let commands = magi_process::common_command_names()
        .iter()
        .map(|name| {
            let path = magi_process::resolve_executable(name);
            serde_json::json!({
                "name": name,
                "available": path.is_some(),
                "path": path.map(|value| value.display().to_string()),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "source": environment.source,
        "pathAvailable": environment.path.is_some(),
        "commands": commands,
    })
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

    fn tool_context(
        &self,
        state: &ApiState,
    ) -> Result<magi_tool_runtime::ToolExecutionContext, ApiError> {
        let mut context = session_scope::resolve_optional_session_workspace_scope(
            state,
            self.session_id.as_deref(),
            self.workspace_id.as_deref(),
            self.workspace_path.as_deref(),
        )?
        .tool_context();
        context.access_profile = self
            .access_profile
            .as_deref()
            .and_then(|value| AccessProfile::from_str(value).ok())
            .unwrap_or_default();
        Ok(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use magi_core::{AbsolutePath, SessionId, WorkspaceId};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_tool_runtime::{BuiltinToolName, ToolRegistry};
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_state_with_tool_registry() -> ApiState {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let governance = Arc::new(GovernanceService::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        workspace_store
            .register(
                WorkspaceId::new("workspace-tools"),
                AbsolutePath::new("/tmp/magi-tools-test-workspace"),
            )
            .expect("workspace should register");
        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        ApiState::new(
            "magi-tools-test",
            event_bus,
            Arc::new(SessionStore::default()),
            workspace_store,
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

    #[test]
    fn tool_catalog_query_rejects_legacy_snake_case_fields() {
        serde_json::from_value::<ToolCatalogQuery>(serde_json::json!({
            "include_internal": true,
            "workspace_id": "workspace-tools"
        }))
        .expect_err("tools catalog query 不得继续接受 snake_case 请求字段");

        let query = serde_json::from_value::<ToolCatalogQuery>(serde_json::json!({
            "includeInternal": true,
            "workspaceId": "workspace-tools"
        }))
        .expect("canonical camelCase tools query");
        assert_eq!(query.include_internal, Some(true));
        assert_eq!(query.workspace_id.as_deref(), Some("workspace-tools"));
    }

    #[tokio::test]
    async fn tools_catalog_route_reuses_tool_registry_health_source() {
        let state = test_state_with_tool_registry();
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("session-tools"),
                "工具诊断会话",
                Some("workspace-tools".to_string()),
            )
            .expect("session should create");
        let app = Router::new().merge(routes()).with_state(state);

        let (status, body) = get_json(
            app,
            "/tools/catalog?workspaceId=workspace-tools&sessionId=session-tools",
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["tool"], "tool_catalog");
        assert_eq!(body["status"], "succeeded");
        assert_eq!(body["builtinTotal"], BuiltinToolName::ALL.len());
        assert_eq!(body["externalCatalogStatus"], "unavailable");
        assert_eq!(body["agentRoleCatalogStatus"], "unavailable");
        assert!(body["runtimeDependencies"][1].get("workspaceId").is_none());
        assert!(
            body["tools"]
                .as_array()
                .expect("tools should be an array")
                .iter()
                .any(|tool| tool["name"] == "tool_catalog")
        );
        assert!(body["commandEnvironment"]["commands"].is_array());
    }

    #[tokio::test]
    async fn tools_catalog_route_can_refresh_command_environment() {
        let app = Router::new()
            .merge(routes())
            .with_state(test_state_with_tool_registry());

        let (status, body) = get_json(app, "/tools/catalog?refreshEnvironment=true").await;

        assert_eq!(status, StatusCode::OK);
        assert!(
            body["commandEnvironment"]["pathAvailable"]
                .as_bool()
                .is_some()
        );
        assert!(matches!(
            body["commandEnvironment"]["source"].as_str(),
            Some("login_shell" | "inherited")
        ));
    }

    #[tokio::test]
    async fn tools_catalog_route_applies_requested_access_profile() {
        let app = Router::new()
            .merge(routes())
            .with_state(test_state_with_tool_registry());

        let (status, body) = get_json(app, "/tools/catalog?accessProfile=full_access").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["currentAccessProfile"], "full_access");
        let shell_exec = body["tools"]
            .as_array()
            .expect("tools should be an array")
            .iter()
            .find(|tool| tool["name"] == "shell_exec")
            .expect("shell_exec should be listed");
        assert_eq!(
            shell_exec["effectiveApprovalPolicy"],
            "regular_risk_block_skipped"
        );
        assert_eq!(
            shell_exec["accessProfileBehavior"],
            "full_access_skips_regular_risk_blocks"
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
        assert_eq!(process_launch["parametersSchema"]["type"], "object");
    }

    #[tokio::test]
    async fn tools_catalog_route_rejects_workspace_mismatched_session_scope() {
        let state = test_state_with_tool_registry();
        let workspace_a = WorkspaceId::new("workspace-tools");
        let workspace_b = WorkspaceId::new("workspace-tools-b");
        state
            .workspace_registry
            .register(
                workspace_b.clone(),
                AbsolutePath::new("/tmp/magi-tools-test-workspace-b"),
            )
            .expect("workspace B should register");
        let session_b = SessionId::new("session-tools-b");
        state
            .session_store
            .create_session_for_workspace(
                session_b.clone(),
                "B 会话",
                Some(workspace_b.to_string()),
            )
            .expect("session should create");
        let app = Router::new().merge(routes()).with_state(state);

        let (status, body) = get_json(
            app,
            &format!(
                "/tools/catalog?workspaceId={}&sessionId={}",
                workspace_a, session_b
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body.to_string().contains("不属于 workspace"),
            "mismatched scope should be rejected by backend authority: {body}"
        );
    }

    #[tokio::test]
    async fn tools_catalog_route_resolves_workspace_from_registered_path_when_query_id_is_stale() {
        let state = test_state_with_tool_registry();
        let workspace_path = "/tmp/magi-tools-test-workspace";
        let session_id = SessionId::new("session-tools-path-authority");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "路径权威工具诊断",
                Some("workspace-tools".to_string()),
            )
            .expect("session should create");
        let app = Router::new().merge(routes()).with_state(state);

        let (status, body) = get_json(
            app,
            &format!(
                "/tools/catalog?workspaceId=workspace-stale-query&workspacePath={}&sessionId={}",
                workspace_path,
                session_id.as_str()
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "succeeded");
        assert!(body["runtimeDependencies"][1].get("workspaceId").is_none());
    }

    #[tokio::test]
    async fn tools_catalog_route_rejects_session_when_path_resolves_other_workspace() {
        let state = test_state_with_tool_registry();
        let workspace_b = WorkspaceId::new("workspace-tools-path-b");
        state
            .workspace_registry
            .register(
                workspace_b.clone(),
                AbsolutePath::new("/tmp/magi-tools-test-workspace-path-b"),
            )
            .expect("workspace B should register");
        let session_b = SessionId::new("session-tools-path-b");
        state
            .session_store
            .create_session_for_workspace(
                session_b.clone(),
                "B 会话",
                Some(workspace_b.to_string()),
            )
            .expect("session should create");
        let app = Router::new().merge(routes()).with_state(state);

        let (status, body) = get_json(
            app,
            &format!(
                "/tools/catalog?workspaceId={}&workspacePath=/tmp/magi-tools-test-workspace&sessionId={}",
                workspace_b,
                session_b.as_str()
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body.to_string().contains("不属于 workspace"),
            "path authority should still reject session/workspace mismatch: {body}"
        );
    }
}
