use axum::{
    Router,
    body::Body,
    extract::{Path as AxumPath, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use magi_browser_authority::{PREVIEW_OPEN_PATH, PREVIEW_PATH_PREFIX};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

use super::changes_files_tunnel::resolve_file_access_scope;
use crate::{change_projection::safe_workspace_path, errors::ApiError, state::ApiState};

const SITE_CSP: &str = "default-src 'none'; base-uri 'none'; object-src 'none'; frame-ancestors 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval' blob:; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; media-src 'self' blob:; connect-src 'self'; form-action 'self'";

pub fn routes() -> Router<ApiState> {
    // 路径来自 network-policy.json：Electron Main 的网络层与请求守卫按同一份路径放行预览。
    let open_route = PREVIEW_OPEN_PATH
        .strip_prefix("/api")
        .expect("preview open path must live under /api");
    let asset_route = format!(
        "{}{{preview_token}}/{{scope_key}}/{{*file_path}}",
        PREVIEW_PATH_PREFIX
            .strip_prefix("/api")
            .expect("preview path prefix must live under /api"),
    );
    Router::new()
        .route(open_route, get(open_site_preview))
        .route(&asset_route, get(serve_site_asset))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SiteOpenQuery {
    session_id: Option<String>,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
    file_path: Option<String>,
    #[serde(default)]
    source_folder_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SitePreviewScope {
    session_id: Option<String>,
    workspace_id: Option<String>,
    source_folder_id: Option<String>,
    execution_root_ref: String,
}

#[derive(Debug, Deserialize)]
struct SiteAssetPath {
    preview_token: String,
    scope_key: String,
    file_path: String,
}

async fn open_site_preview(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<SiteOpenQuery>,
) -> Result<Response, ApiError> {
    let requested_path = query
        .file_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("文件路径不能为空".to_string()))?;
    let scope = resolve_file_access_scope(
        &state,
        query.session_id.as_deref(),
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
        None,
        query.source_folder_id.as_deref(),
    )?;
    let (absolute_path, relative_path) = safe_workspace_path(&scope.root, requested_path)?;
    if !absolute_path.is_file()
        || site_mime_for_path(&absolute_path) != Some("text/html; charset=utf-8")
    {
        return Err(ApiError::InvalidInput(
            "网页预览入口必须是 HTML 文件".to_string(),
        ));
    }
    ensure_public_site_asset(&relative_path, &absolute_path)?;

    // 凭据签名覆盖项目、源目录和实际副本，资源请求仍重新解析 daemon 成员与映射。
    let scope_key = format!(
        "v2:{}",
        serde_json::to_string(&SitePreviewScope {
            session_id: scope.binding["sessionId"].as_str().map(str::to_string),
            workspace_id: scope.binding["workspaceId"].as_str().map(str::to_string),
            source_folder_id: scope.binding["sourceFolderId"].as_str().map(str::to_string),
            execution_root_ref: magi_core::HostPath::canonicalize(&scope.root)
                .map_err(|error| ApiError::internal_assembly("规范化预览目录失败", error))?
                .to_path_ref()
                .as_str()
                .into(),
        })
        .expect("preview scope serializes")
    );
    let preview_token = state.tunnel_manager.site_preview_token(&scope_key).await;
    let path = format!(
        "/api/files/site/{}/{}/{}",
        encode_path_segment(&preview_token),
        encode_path_segment(&scope_key),
        encode_relative_path(&relative_path),
    );
    // 本机预览放到独立来源上：预览里的脚本不能与 API 同源。
    let location = match crate::request_guard::isolated_preview_origin(&headers) {
        Some(origin) => format!("{origin}{path}"),
        None => path,
    };
    let mut response = StatusCode::TEMPORARY_REDIRECT.into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&location)
            .map_err(|error| ApiError::internal_assembly("组装网页预览地址失败", error))?,
    );
    apply_no_store_headers(response.headers_mut());
    Ok(response)
}

async fn serve_site_asset(
    State(state): State<ApiState>,
    AxumPath(path): AxumPath<SiteAssetPath>,
) -> Response {
    if !state
        .tunnel_manager
        .authorize_site_preview_request(&path.scope_key, &path.preview_token)
        .await
    {
        return (StatusCode::UNAUTHORIZED, "网页预览凭据无效").into_response();
    }

    match load_site_asset(&state, &path.scope_key, &path.file_path) {
        Ok((mime, bytes)) => {
            let mut response = Response::new(Body::from(bytes));
            *response.status_mut() = StatusCode::OK;
            let headers = response.headers_mut();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_static("*"),
            );
            headers.insert(
                header::HeaderName::from_static("cross-origin-resource-policy"),
                HeaderValue::from_static("cross-origin"),
            );
            apply_no_store_headers(headers);
            response
        }
        Err(error) => error.into_response(),
    }
}

fn load_site_asset(
    state: &ApiState,
    scope_key: &str,
    requested_path: &str,
) -> Result<(&'static str, Vec<u8>), ApiError> {
    let scope = if let Some(json) = scope_key.strip_prefix("v2:") {
        let original: SitePreviewScope = serde_json::from_str(json)
            .map_err(|_| ApiError::InvalidInput("网页预览作用域无效".into()))?;
        let current = resolve_file_access_scope(
            state,
            original.session_id.as_deref(),
            original.workspace_id.as_deref(),
            None,
            None,
            original.source_folder_id.as_deref(),
        )?;
        let current_ref = magi_core::HostPath::canonicalize(&current.root)
            .map_err(|_| ApiError::Conflict("原网页预览目录已不可用".into()))?
            .to_path_ref();
        if current_ref.as_str() != original.execution_root_ref {
            return Err(ApiError::Conflict(
                "原网页预览的执行目录已变化，请重新打开".into(),
            ));
        }
        current
    } else {
        return Err(ApiError::InvalidInput("网页预览作用域无效".into()));
    };
    let workspace_root = scope.root;
    let (mut absolute_path, mut relative_path) =
        safe_workspace_path(&workspace_root, requested_path)?;
    if absolute_path.is_dir() {
        relative_path = format!("{}/index.html", relative_path.trim_end_matches(['/', '\\']));
        (absolute_path, _) = safe_workspace_path(&workspace_root, &relative_path)?;
    }
    ensure_public_site_asset(&relative_path, &absolute_path)?;
    let mime = site_mime_for_path(&absolute_path)
        .ok_or_else(|| ApiError::InvalidInput("该文件类型不允许用于网页预览".to_string()))?;
    let bytes = std::fs::read(&absolute_path)
        .map_err(|error| ApiError::internal_assembly("读取网页预览资源失败", error))?;
    Ok((mime, bytes))
}

fn ensure_public_site_asset(relative_path: &str, absolute_path: &Path) -> Result<(), ApiError> {
    let has_hidden_component = Path::new(relative_path).components().any(|component| {
        matches!(component, Component::Normal(value) if value.to_string_lossy().starts_with('.'))
    });
    if has_hidden_component {
        return Err(ApiError::InvalidInput(
            "隐藏文件不允许用于网页预览".to_string(),
        ));
    }
    if !absolute_path.is_file() {
        return Err(ApiError::InvalidInput("网页预览资源不存在".to_string()));
    }
    if site_mime_for_path(absolute_path).is_none() {
        return Err(ApiError::InvalidInput(
            "该文件类型不允许用于网页预览".to_string(),
        ));
    }
    Ok(())
}

fn site_mime_for_path(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "html" | "htm" => Some("text/html; charset=utf-8"),
        "css" => Some("text/css; charset=utf-8"),
        "js" | "mjs" => Some("text/javascript; charset=utf-8"),
        "json" => Some("application/json; charset=utf-8"),
        "webmanifest" => Some("application/manifest+json; charset=utf-8"),
        "txt" => Some("text/plain; charset=utf-8"),
        "svg" => Some("image/svg+xml"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "avif" => Some("image/avif"),
        "bmp" => Some("image/bmp"),
        "ico" => Some("image/x-icon"),
        "woff" => Some("font/woff"),
        "woff2" => Some("font/woff2"),
        "ttf" => Some("font/ttf"),
        "otf" => Some("font/otf"),
        "wasm" => Some("application/wasm"),
        "mp3" => Some("audio/mpeg"),
        "wav" => Some("audio/wav"),
        "ogg" => Some("audio/ogg"),
        "mp4" => Some("video/mp4"),
        "webm" => Some("video/webm"),
        _ => None,
    }
}

fn apply_no_store_headers(headers: &mut HeaderMap) {
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(SITE_CSP),
    );
}

fn encode_relative_path(path: &str) -> String {
    path.replace('\\', "/")
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(encode_path_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn encode_path_segment(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use magi_core::{AbsolutePath, SessionId, WorkspaceId};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use std::{fs, sync::Arc};
    use tempfile::tempdir;
    use tower::ServiceExt;

    fn test_state(root: &Path, workspace_id: &str) -> ApiState {
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        state
            .workspace_registry
            .register(
                WorkspaceId::new(workspace_id),
                AbsolutePath::new(root.to_string_lossy().as_ref()),
            )
            .expect("workspace should register");
        state
    }

    async fn preview_location(state: &ApiState, query: &str) -> String {
        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri(format!("/files/site-open?{query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        response.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .trim_start_matches("/api")
            .into()
    }

    #[tokio::test]
    async fn project_preview_binds_source_and_execution_copy_and_rechecks_revocation() {
        use super::super::test_project_fixtures::{add_source, set_members};
        let root = tempdir().unwrap();
        let a = root.path().join("a");
        let b = root.path().join("b");
        let copy = root.path().join("copy");
        for (path, content) in [(&a, "A"), (&b, "B"), (&copy, "copy")] {
            fs::create_dir_all(path).unwrap();
            fs::write(path.join("index.html"), content).unwrap();
            fs::write(path.join("app.js"), content).unwrap();
        }
        let state = test_state(&a, "site-project");
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("site-session"),
                "site",
                Some("site-project".into()),
            )
            .unwrap();
        let first = add_source(&state, "site-project", "second", &b).primary_source_folder_id;
        let query = format!(
            "sessionId=site-session&workspaceId=site-project&sourceFolderId={first}&filePath=index.html"
        );
        let location = preview_location(&state, &query).await;
        let app = routes().with_state(state.clone());
        let request = |uri: String| Request::builder().uri(uri).body(Body::empty()).unwrap();
        set_members(&state, "site-project", &[&first, "second"], "second");
        let response = app
            .clone()
            .oneshot(request(location.clone()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            "A"
        );
        let response = app
            .clone()
            .oneshot(request(location.replace(&first, "second")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(b.join("app.js"), a.join("escape.js")).unwrap();
            let response = app
                .clone()
                .oneshot(request(location.replace("index.html", "escape.js")))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        state
            .session_isolations
            .insert(magi_session_isolation::SessionIsolation {
                session_id: "site-session".into(),
                workspace_id: "site-project".into(),
                source_root: magi_core::HostPath::canonicalize(&a)
                    .unwrap()
                    .into_path_buf(),
                root: magi_core::HostPath::canonicalize(&copy)
                    .unwrap()
                    .into_path_buf(),
                origin: magi_session_isolation::IsolationOrigin::Manual,
                strategy: magi_session_isolation::CloneStrategy::Copy,
                linked_dirs: Vec::new(),
                git_available: false,
                created_at_ms: 0,
            });
        let response = app
            .clone()
            .oneshot(request(location.clone()))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::CONFLICT,
            "原根凭据不能转入新副本"
        );
        let copy_location = preview_location(&state, &query).await;
        let response = app
            .clone()
            .oneshot(request(copy_location.clone()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            "copy"
        );
        state.session_isolations.remove("site-session");
        let response = app.clone().oneshot(request(copy_location)).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        set_members(&state, "site-project", &["second"], "second");
        let response = app.oneshot(request(location)).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "撤销旧目录后不回落第二目录"
        );
    }

    #[tokio::test]
    async fn site_preview_serves_html_and_relative_assets_with_mime_headers() {
        let root = tempdir().expect("tempdir should create");
        fs::create_dir(root.path().join("site")).expect("site dir should create");
        fs::write(
            root.path().join("site/index.html"),
            "<link rel=\"stylesheet\" href=\"style.css\"><script src=\"app.js\"></script>",
        )
        .expect("html should write");
        fs::write(root.path().join("site/style.css"), "body { color: red; }")
            .expect("css should write");
        fs::write(
            root.path().join("site/app.js"),
            "document.body.dataset.ready='1'",
        )
        .expect("js should write");
        let state = test_state(root.path(), "workspace-site");
        let location = preview_location(
            &state,
            "workspaceId=workspace-site&filePath=site/index.html",
        )
        .await;
        let app = routes().with_state(state);

        for (path, expected_mime) in [
            ("site/index.html", "text/html; charset=utf-8"),
            ("site/style.css", "text/css; charset=utf-8"),
            ("site/app.js", "text/javascript; charset=utf-8"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(location.replace("site/index.html", path))
                        .body(Body::empty())
                        .expect("request should build"),
                )
                .await
                .expect("route should respond");
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok()),
                Some(expected_mime),
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("no-store"),
            );
        }
    }

    #[tokio::test]
    async fn site_preview_rejects_wrong_workspace_token_and_non_web_files() {
        let root = tempdir().expect("tempdir should create");
        fs::write(root.path().join("index.html"), "ok").expect("html should write");
        fs::write(root.path().join("secret.rs"), "secret").expect("source should write");
        let state = test_state(root.path(), "workspace-site");
        let location =
            preview_location(&state, "workspaceId=workspace-site&filePath=index.html").await;
        let mut wrong_parts = location.split('/').map(str::to_string).collect::<Vec<_>>();
        wrong_parts[3].push_str("-invalid");
        let app = routes().with_state(state);

        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(wrong_parts.join("/"))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let rejected = app
            .oneshot(
                Request::builder()
                    .uri(location.replace("index.html", "secret.rs"))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(rejected.into_body(), usize::MAX)
            .await
            .expect("body should read");
        assert!(!String::from_utf8_lossy(&body).contains("secret"));
    }

    #[tokio::test]
    async fn site_open_redirects_to_scoped_preview_without_transport_token() {
        let root = tempdir().expect("tempdir should create");
        fs::write(root.path().join("index.html"), "ok").expect("html should write");
        let state = test_state(root.path(), "workspace-site");
        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/files/site-open?workspaceId=workspace-site&filePath=index.html")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .expect("redirect should include location");
        assert!(location.starts_with("/api/files/site/"));
        assert!(location.contains("/v2%3A"));
        assert!(location.ends_with("/index.html"));
        assert!(!location.contains("transport-secret"));
    }

    #[tokio::test]
    async fn personal_site_preview_keeps_assets_in_the_owning_session_and_expires_on_delete() {
        let root = tempdir().unwrap();
        let state = test_state(root.path(), "workspace-site").with_runtime_persistence(Arc::new(
            crate::state::RuntimeStatePersistence::new(
                root.path().join("state"),
                root.path().join("workspaces.json"),
                root.path().join("knowledge.json"),
            ),
        ));
        for id in ["site-a", "site-b"] {
            state
                .session_store
                .create_session(SessionId::new(id), id)
                .unwrap();
            let directory = state
                .personal_session_execution_root(&SessionId::new(id))
                .unwrap();
            fs::write(
                directory.join("index.html"),
                format!("<script src='app.js'></script>{id}"),
            )
            .unwrap();
            fs::write(directory.join("app.js"), id).unwrap();
        }
        let app = routes().with_state(state.clone());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/files/site-open?sessionId=site-a&filePath=index.html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        let location = response.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .trim_start_matches("/api")
            .to_string();
        assert!(location.contains("site-a"));
        for (uri, expected) in [
            (location.clone(), StatusCode::OK),
            (location.replace("index.html", "app.js"), StatusCode::OK),
            (
                location.replace("site-a", "site-b"),
                StatusCode::UNAUTHORIZED,
            ),
            (
                "/files/site-open?sessionId=site-a&workspaceId=workspace-site&filePath=index.html"
                    .into(),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{uri}");
        }
        #[cfg(unix)]
        {
            let a = state.personal_session_execution_root_path(&SessionId::new("site-a"));
            let b = state.personal_session_execution_root_path(&SessionId::new("site-b"));
            std::os::unix::fs::symlink(b.join("app.js"), a.join("escape.js")).unwrap();
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(location.replace("index.html", "escape.js"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        state
            .delete_session_and_resources(&SessionId::new("site-a"))
            .await
            .unwrap();
        let response = app
            .oneshot(
                Request::builder()
                    .uri(location)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            state
                .personal_session_execution_root_path(&SessionId::new("site-b"))
                .join("app.js")
                .exists()
        );
    }

    #[tokio::test]
    async fn local_site_open_redirects_to_the_isolated_preview_origin() {
        let root = tempdir().expect("tempdir should create");
        fs::write(root.path().join("index.html"), "ok").expect("html should write");
        let state = test_state(root.path(), "workspace-site");
        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/files/site-open?workspaceId=workspace-site&filePath=index.html")
                    .header(header::HOST, "127.0.0.1:38123")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .expect("redirect should include location");
        assert!(location.starts_with("http://site.localhost:38123/api/files/site/"));
        assert!(location.contains("/v2%3A"));
        assert!(location.ends_with("/index.html"));
    }
}
