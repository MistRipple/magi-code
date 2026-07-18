use axum::{
    Router,
    body::Body,
    extract::{Path as AxumPath, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use std::path::{Component, Path, PathBuf};

use super::session_scope::require_registered_workspace_binding;
use crate::{change_projection::safe_workspace_path, errors::ApiError, state::ApiState};

const SITE_CSP: &str = "default-src 'none'; base-uri 'none'; object-src 'none'; frame-ancestors 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval' blob:; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; media-src 'self' blob:; connect-src 'self'; form-action 'self'";

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/files/site-open", get(open_site_preview))
        .route(
            "/files/site/{preview_token}/{workspace_id}/{*file_path}",
            get(serve_site_asset),
        )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SiteOpenQuery {
    workspace_id: Option<String>,
    workspace_path: Option<String>,
    file_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SiteAssetPath {
    preview_token: String,
    workspace_id: String,
    file_path: String,
}

async fn open_site_preview(
    State(state): State<ApiState>,
    Query(query): Query<SiteOpenQuery>,
) -> Result<Response, ApiError> {
    let requested_path = query
        .file_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("文件路径不能为空".to_string()))?;
    let binding = require_registered_workspace_binding(
        &state,
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
    )?;
    let workspace_root = PathBuf::from(&binding.workspace_path);
    let (absolute_path, relative_path) = safe_workspace_path(&workspace_root, requested_path)?;
    if !absolute_path.is_file()
        || site_mime_for_path(&absolute_path) != Some("text/html; charset=utf-8")
    {
        return Err(ApiError::InvalidInput(
            "网页预览入口必须是 HTML 文件".to_string(),
        ));
    }
    ensure_public_site_asset(&relative_path, &absolute_path)?;

    let workspace_id = binding.workspace_id.as_str();
    let preview_token = state.tunnel_manager.site_preview_token(workspace_id).await;
    let location = format!(
        "/api/files/site/{}/{}/{}",
        encode_path_segment(&preview_token),
        encode_path_segment(workspace_id),
        encode_relative_path(&relative_path),
    );
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
        .authorize_site_preview_request(&path.workspace_id, &path.preview_token)
        .await
    {
        return (StatusCode::UNAUTHORIZED, "网页预览凭据无效").into_response();
    }

    match load_site_asset(&state, &path.workspace_id, &path.file_path) {
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
    workspace_id: &str,
    requested_path: &str,
) -> Result<(&'static str, Vec<u8>), ApiError> {
    let binding = require_registered_workspace_binding(state, Some(workspace_id), None)?;
    let workspace_root = PathBuf::from(binding.workspace_path);
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
    use magi_core::{AbsolutePath, WorkspaceId};
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
        let token = state
            .tunnel_manager
            .site_preview_token("workspace-site")
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
                        .uri(format!("/files/site/{token}/workspace-site/{path}"))
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
        let wrong_token = state
            .tunnel_manager
            .site_preview_token("workspace-other")
            .await;
        let valid_token = state
            .tunnel_manager
            .site_preview_token("workspace-site")
            .await;
        let app = routes().with_state(state);

        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/files/site/{wrong_token}/workspace-site/index.html"
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let rejected = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/files/site/{valid_token}/workspace-site/secret.rs"
                    ))
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
                    .uri("/files/site-open?workspaceId=workspace-site&filePath=index.html&tunnel_token=transport-secret")
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
        assert!(location.ends_with("/workspace-site/index.html"));
        assert!(!location.contains("transport-secret"));
    }
}
