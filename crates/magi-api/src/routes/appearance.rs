use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use magi_appearance::{
    AppearanceError, AppearanceErrorKind, AppearanceSnapshot, ImportConflictStrategy, ThemePack,
    ThemeSource,
};
use magi_core::{EventId, UtcMillis};
use magi_event_bus::EventEnvelope;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{errors::ApiError, state::ApiState};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RevisionRequest {
    expected_revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActivateThemeRequest {
    expected_revision: u64,
    theme_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveThemeRequest {
    expected_revision: u64,
    pack: ThemePack,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateThemeRequest {
    expected_revision: u64,
    theme_id: String,
    pack: ThemePack,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportThemeRequest {
    expected_revision: u64,
    package_base64: String,
    #[serde(default)]
    conflict_strategy: ImportConflictStrategyDto,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
enum ImportConflictStrategyDto {
    #[default]
    Reject,
    Duplicate,
    Replace,
}

impl From<ImportConflictStrategyDto> for ImportConflictStrategy {
    fn from(value: ImportConflictStrategyDto) -> Self {
        match value {
            ImportConflictStrategyDto::Reject => Self::Reject,
            ImportConflictStrategyDto::Duplicate => Self::Duplicate,
            ImportConflictStrategyDto::Replace => Self::Replace,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UploadAssetRequest {
    data_base64: String,
}

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/appearance/bootstrap", get(bootstrap))
        .route("/appearance/activate", post(activate_theme))
        .route("/appearance/themes", post(create_theme))
        .route("/appearance/themes/update", post(update_theme))
        .route("/appearance/themes/{theme_id}/delete", post(delete_theme))
        .route("/appearance/themes/import", post(import_theme))
        .route("/appearance/themes/{theme_id}/export", get(export_theme))
        .route("/appearance/assets", post(upload_asset))
        .route("/appearance/assets/{asset_id}", get(read_asset))
}

async fn bootstrap(State(state): State<ApiState>) -> Json<AppearanceSnapshot> {
    Json(state.appearance_library.snapshot())
}

async fn activate_theme(
    State(state): State<ApiState>,
    Json(request): Json<ActivateThemeRequest>,
) -> Result<Json<AppearanceSnapshot>, ApiError> {
    let snapshot = state
        .appearance_library
        .activate(request.theme_id.trim(), request.expected_revision)
        .map_err(appearance_error)?;
    publish_appearance_changed(&state, &snapshot, "activated");
    Ok(Json(snapshot))
}

async fn create_theme(
    State(state): State<ApiState>,
    Json(request): Json<SaveThemeRequest>,
) -> Result<Json<AppearanceSnapshot>, ApiError> {
    let snapshot = state
        .appearance_library
        .create_theme(
            request.pack,
            request.expected_revision,
            ThemeSource::Created,
        )
        .map_err(appearance_error)?;
    publish_appearance_changed(&state, &snapshot, "created");
    Ok(Json(snapshot))
}

async fn update_theme(
    State(state): State<ApiState>,
    Json(request): Json<UpdateThemeRequest>,
) -> Result<Json<AppearanceSnapshot>, ApiError> {
    let snapshot = state
        .appearance_library
        .update_theme(
            request.theme_id.trim(),
            request.pack,
            request.expected_revision,
        )
        .map_err(appearance_error)?;
    publish_appearance_changed(&state, &snapshot, "updated");
    Ok(Json(snapshot))
}

async fn delete_theme(
    State(state): State<ApiState>,
    Path(theme_id): Path<String>,
    Json(request): Json<RevisionRequest>,
) -> Result<Json<AppearanceSnapshot>, ApiError> {
    let snapshot = state
        .appearance_library
        .delete_theme(theme_id.trim(), request.expected_revision)
        .map_err(appearance_error)?;
    publish_appearance_changed(&state, &snapshot, "deleted");
    Ok(Json(snapshot))
}

async fn import_theme(
    State(state): State<ApiState>,
    Json(request): Json<ImportThemeRequest>,
) -> Result<Json<AppearanceSnapshot>, ApiError> {
    let bytes = STANDARD
        .decode(request.package_base64.trim())
        .map_err(|_| ApiError::InvalidInput("主题包编码无效".to_string()))?;
    let snapshot = state
        .appearance_library
        .import_theme(
            &bytes,
            request.expected_revision,
            request.conflict_strategy.into(),
        )
        .map_err(appearance_error)?;
    publish_appearance_changed(&state, &snapshot, "imported");
    Ok(Json(snapshot))
}

async fn export_theme(
    State(state): State<ApiState>,
    Path(theme_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let bytes = state
        .appearance_library
        .export_theme(theme_id.trim())
        .map_err(appearance_error)?;
    Ok(Json(json!({
        "fileName": format!("{}.magi-theme.zip", theme_id.trim()),
        "packageBase64": STANDARD.encode(bytes),
    })))
}

async fn upload_asset(
    State(state): State<ApiState>,
    Json(request): Json<UploadAssetRequest>,
) -> Result<Json<Value>, ApiError> {
    let bytes = STANDARD
        .decode(request.data_base64.trim())
        .map_err(|_| ApiError::InvalidInput("背景图编码无效".to_string()))?;
    let asset = state
        .appearance_library
        .put_asset(&bytes)
        .map_err(appearance_error)?;
    Ok(Json(json!({
        "assetId": asset.asset_id,
        "mimeType": asset.mime_type,
        "width": asset.width,
        "height": asset.height,
    })))
}

async fn read_asset(
    State(state): State<ApiState>,
    Path(asset_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (bytes, mime_type) = state
        .appearance_library
        .read_asset(asset_id.trim())
        .map_err(appearance_error)?;
    Ok(Json(json!({
        "assetId": asset_id,
        "mimeType": mime_type,
        "dataBase64": STANDARD.encode(bytes),
    })))
}

fn appearance_error(error: AppearanceError) -> ApiError {
    match error.kind() {
        AppearanceErrorKind::InvalidInput => ApiError::InvalidInput(error.to_string()),
        AppearanceErrorKind::Conflict => ApiError::Conflict(error.to_string()),
    }
}

fn publish_appearance_changed(state: &ApiState, snapshot: &AppearanceSnapshot, operation: &str) {
    state.event_bus.publish(EventEnvelope::domain(
        EventId::new(format!(
            "event-appearance-{operation}-{}",
            UtcMillis::now().0
        )),
        "appearance.changed",
        json!({
            "operation": operation,
            "revision": snapshot.revision,
            "activeThemeId": snapshot.active_theme_id,
        }),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn app() -> Router {
        let state = ApiState::new(
            "appearance-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        routes().with_state(state)
    }

    #[tokio::test]
    async fn bootstrap_exposes_builtin_themes_at_one_level() {
        let response = app()
            .oneshot(
                Request::get("/appearance/bootstrap")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["activeThemeId"], "builtin.system");
        assert!(
            value["themes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|theme| theme["pack"]["id"] == "builtin.light")
        );
        assert!(
            value["themes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|theme| theme["pack"]["id"] == "builtin.forest")
                .and_then(|theme| theme["pack"]["wallpaper"]["assetId"].as_str())
                .is_some()
        );
    }

    #[tokio::test]
    async fn builtin_wallpaper_is_available_through_asset_route() {
        let bootstrap_response = app()
            .oneshot(
                Request::get("/appearance/bootstrap")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap_bytes = axum::body::to_bytes(bootstrap_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let bootstrap: Value = serde_json::from_slice(&bootstrap_bytes).unwrap();
        let asset_id = bootstrap["themes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|theme| theme["pack"]["id"] == "builtin.forest")
            .and_then(|theme| theme["pack"]["wallpaper"]["assetId"].as_str())
            .unwrap();

        let response = app()
            .oneshot(
                Request::get(format!("/appearance/assets/{asset_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["assetId"], asset_id);
        assert_eq!(value["mimeType"], "image/webp");
        assert!(
            value["dataBase64"]
                .as_str()
                .is_some_and(|payload| !payload.is_empty())
        );
    }

    #[tokio::test]
    async fn stale_appearance_revision_returns_conflict() {
        let response = app()
            .oneshot(
                Request::post("/appearance/activate")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"themeId":"builtin.dark","expectedRevision":0}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["error_code"], "CONFLICT");
    }
}
