//! 会话隔离副本的 HTTP 入口：查询状态、启用、丢弃、预览与应用合并。
use super::session_scope::{SessionScope, parse_session_id, resolve_existing_session_scope};
use crate::{errors::ApiError, state::ApiState};
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_core::SessionId;
use magi_session_isolation::{
    CloneStrategy, ConflictResolution, IsolationOrigin, MergeOutcome, MergePlan, MergeSelection,
    SessionIsolation,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/session/isolations", get(list_session_isolations))
        .route("/session/isolation", get(get_session_isolation))
        .route("/session/isolation/enable", post(enable_session_isolation))
        .route(
            "/session/isolation/discard",
            post(discard_session_isolation),
        )
        .route(
            "/session/isolation/merge-plan",
            post(plan_session_isolation_merge),
        )
        .route(
            "/session/isolation/merge",
            post(apply_session_isolation_merge),
        )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IsolationScope {
    session_id: String,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

impl IsolationScope {
    fn resolve(&self, state: &ApiState) -> Result<(SessionId, SessionScope), ApiError> {
        let session_id = parse_session_id(Some(&self.session_id))?;
        let scope = resolve_existing_session_scope(
            state,
            &session_id,
            self.workspace_id.as_deref(),
            self.workspace_path.as_deref(),
        )?;
        Ok((session_id, scope))
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IsolationDto {
    root: String,
    source_root: String,
    origin: IsolationOrigin,
    strategy: CloneStrategy,
    linked_dirs: Vec<String>,
    git_available: bool,
    created_at_ms: u64,
}

impl From<SessionIsolation> for IsolationDto {
    fn from(isolation: SessionIsolation) -> Self {
        Self {
            root: isolation.root.to_string_lossy().into_owned(),
            source_root: isolation.source_root.to_string_lossy().into_owned(),
            origin: isolation.origin,
            strategy: isolation.strategy,
            linked_dirs: isolation.linked_dirs,
            git_available: isolation.git_available,
            created_at_ms: isolation.created_at_ms,
        }
    }
}

/// 侧栏、输入区用来标记哪些会话运行在隔离副本里。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IsolationSummaryDto {
    session_id: String,
    workspace_id: String,
    origin: IsolationOrigin,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionIsolationListResponse {
    isolations: Vec<IsolationSummaryDto>,
}

async fn list_session_isolations(
    State(state): State<ApiState>,
) -> Json<SessionIsolationListResponse> {
    Json(SessionIsolationListResponse {
        isolations: state
            .session_isolations
            .all()
            .into_iter()
            .map(|isolation| IsolationSummaryDto {
                session_id: isolation.session_id,
                workspace_id: isolation.workspace_id,
                origin: isolation.origin,
            })
            .collect(),
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionIsolationResponse {
    session_id: SessionId,
    enabled: bool,
    isolation: Option<IsolationDto>,
}

fn response(state: &ApiState, session_id: SessionId) -> SessionIsolationResponse {
    let isolation = state.session_isolation(&session_id).map(IsolationDto::from);
    SessionIsolationResponse {
        session_id,
        enabled: isolation.is_some(),
        isolation,
    }
}

async fn get_session_isolation(
    State(state): State<ApiState>,
    Query(scope): Query<IsolationScope>,
) -> Result<Json<SessionIsolationResponse>, ApiError> {
    let (session_id, _) = scope.resolve(&state)?;
    Ok(Json(response(&state, session_id)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnableRequest {
    #[serde(flatten)]
    scope: IsolationScope,
}

async fn enable_session_isolation(
    State(state): State<ApiState>,
    Json(request): Json<EnableRequest>,
) -> Result<Json<SessionIsolationResponse>, ApiError> {
    let (session_id, scope) = request.scope.resolve(&state)?;
    let workspace_id = scope
        .workspace_id()
        .ok_or_else(|| ApiError::InvalidInput("个人会话没有可隔离的工作区".to_string()))?;
    state
        .enable_session_isolation(&session_id, &workspace_id, IsolationOrigin::Manual)
        .await?;
    Ok(Json(response(&state, session_id)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiscardRequest {
    #[serde(flatten)]
    scope: IsolationScope,
    /// 副本里还有未合并的改动时，必须显式确认丢弃。
    #[serde(default)]
    discard_changes: bool,
}

async fn discard_session_isolation(
    State(state): State<ApiState>,
    Json(request): Json<DiscardRequest>,
) -> Result<Json<SessionIsolationResponse>, ApiError> {
    let (session_id, _) = request.scope.resolve(&state)?;
    if !request.discard_changes && state.session_isolation(&session_id).is_some() {
        let plan = state.isolation_merge_plan(&session_id).await?;
        if !plan.entries.is_empty() {
            return Err(ApiError::conflict(
                "隔离副本里还有未合并的改动，先合并，或确认丢弃",
                session_id.as_str(),
            ));
        }
    }
    state.discard_session_isolation(&session_id).await?;
    Ok(Json(response(&state, session_id)))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MergePlanResponse {
    session_id: SessionId,
    #[serde(flatten)]
    plan: MergePlan,
}

async fn plan_session_isolation_merge(
    State(state): State<ApiState>,
    Json(request): Json<EnableRequest>,
) -> Result<Json<MergePlanResponse>, ApiError> {
    let (session_id, _) = request.scope.resolve(&state)?;
    let plan = state.isolation_merge_plan(&session_id).await?;
    Ok(Json(MergePlanResponse { session_id, plan }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MergeRequest {
    #[serde(flatten)]
    scope: IsolationScope,
    /// 只合并这些路径；缺省表示全部。
    #[serde(default)]
    paths: Option<Vec<String>>,
    /// 冲突文件的处理方式。
    #[serde(default)]
    resolutions: HashMap<String, ConflictResolution>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MergeResponse {
    session_id: SessionId,
    #[serde(flatten)]
    outcome: MergeOutcome,
}

async fn apply_session_isolation_merge(
    State(state): State<ApiState>,
    Json(request): Json<MergeRequest>,
) -> Result<Json<MergeResponse>, ApiError> {
    let (session_id, _) = request.scope.resolve(&state)?;
    let outcome = state
        .isolation_merge_apply(
            &session_id,
            MergeSelection {
                paths: request.paths,
                resolutions: request.resolutions,
            },
        )
        .await?;
    Ok(Json(MergeResponse {
        session_id,
        outcome,
    }))
}

#[cfg(test)]
mod tests {
    use crate::state::ApiState;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use magi_core::{SessionId, WorkspaceId};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_state() -> ApiState {
        ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
    }

    async fn call(
        state: &ApiState,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut request = Request::builder().method(method).uri(uri);
        let body = match body {
            Some(body) => {
                request = request.header("content-type", "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let response = crate::routes::build_router(state.clone())
            .oneshot(request.body(body).expect("request should build"))
            .await
            .expect("router should respond");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| serde_json::json!({ "raw": String::from_utf8_lossy(&bytes) }));
        (status, value)
    }

    #[tokio::test]
    async fn isolation_http_lifecycle_enable_merge_and_discard() {
        let state = test_state();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("a.txt"), "original").unwrap();
        let workspace_id = WorkspaceId::new("isolation-route-workspace");
        state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace.path().to_path_buf())
            .unwrap();
        let session_id = SessionId::new("isolation-route-session");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "isolation route",
                Some(workspace_id.to_string()),
            )
            .unwrap();
        let scope = serde_json::json!({
            "sessionId": session_id,
            "workspaceId": workspace_id,
        });
        let query =
            format!("/api/session/isolation?sessionId={session_id}&workspaceId={workspace_id}");

        let (status, body) = call(&state, "GET", &query, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["enabled"], false);
        let (_, listed) = call(&state, "GET", "/api/session/isolations", None).await;
        assert_eq!(listed["isolations"].as_array().unwrap().len(), 0);

        let (status, body) = call(
            &state,
            "POST",
            "/api/session/isolation/enable",
            Some(scope.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["enabled"], true);
        assert_eq!(body["isolation"]["origin"]["kind"], "manual");
        let (_, listed) = call(&state, "GET", "/api/session/isolations", None).await;
        assert_eq!(listed["isolations"][0]["sessionId"], session_id.as_str());
        let copy = std::path::PathBuf::from(body["isolation"]["root"].as_str().unwrap());
        assert_eq!(
            std::fs::read_to_string(copy.join("a.txt")).unwrap(),
            "original"
        );

        // 在副本里改文件，再通过 HTTP 预览并合并。
        std::fs::write(copy.join("a.txt"), "changed").unwrap();
        // 文件预览按会话取副本里的版本，而不是主工作区里的。
        let (status, body) = call(
            &state,
            "GET",
            &format!(
                "/api/files/content?sessionId={session_id}&workspaceId={workspace_id}&filePath=a.txt"
            ),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["content"], "changed");
        let (status, body) = call(
            &state,
            "GET",
            &format!("/api/files/content?workspaceId={workspace_id}&filePath=a.txt"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["content"], "original", "工作区级预览仍读主工作区");
        let (status, body) = call(
            &state,
            "POST",
            "/api/session/isolation/merge-plan",
            Some(scope.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["entries"][0]["path"], "a.txt");
        assert_eq!(body["entries"][0]["state"], "clean");
        assert_eq!(body["entries"][0]["action"], "modify");

        // 有未合并改动时，不带确认就丢弃会被拒绝。
        let (status, _) = call(
            &state,
            "POST",
            "/api/session/isolation/discard",
            Some(scope.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);

        let (status, body) = call(
            &state,
            "POST",
            "/api/session/isolation/merge",
            Some(scope.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["applied"][0], "a.txt");
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("a.txt")).unwrap(),
            "changed"
        );

        let (status, body) = call(
            &state,
            "POST",
            "/api/session/isolation/discard",
            Some(scope.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["enabled"], false);
        assert!(!copy.exists());

        // 再次启用后带着未合并的改动，明确确认才能丢弃；请求体里的 paths / resolutions 也能正常解析。
        let (status, body) = call(
            &state,
            "POST",
            "/api/session/isolation/enable",
            Some(scope.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let copy = std::path::PathBuf::from(body["isolation"]["root"].as_str().unwrap());
        std::fs::write(copy.join("a.txt"), "unmerged").unwrap();
        let mut selective = scope.clone();
        selective["paths"] = serde_json::json!(["a.txt"]);
        selective["resolutions"] = serde_json::json!({ "a.txt": "keep_source" });
        let (status, body) = call(
            &state,
            "POST",
            "/api/session/isolation/merge",
            Some(selective),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["applied"][0], "a.txt");
        std::fs::write(copy.join("a.txt"), "unmerged again").unwrap();
        let mut confirmed = scope.clone();
        confirmed["discardChanges"] = serde_json::json!(true);
        let (status, body) = call(
            &state,
            "POST",
            "/api/session/isolation/discard",
            Some(confirmed),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("a.txt")).unwrap(),
            "unmerged"
        );
    }

    #[tokio::test]
    async fn personal_sessions_cannot_be_isolated() {
        let state = test_state();
        let session_id = SessionId::new("isolation-personal-session");
        state
            .session_store
            .create_session(session_id.clone(), "personal")
            .unwrap();
        let (status, body) = call(
            &state,
            "POST",
            "/api/session/isolation/enable",
            Some(serde_json::json!({ "sessionId": session_id })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
}
