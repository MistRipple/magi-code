use super::*;
use axum::body::Body;
use magi_governance::GovernanceService;
use magi_plugin_system::PluginManager;
use magi_session_store::SessionStore;
use magi_workspace::WorkspaceStore;
use std::sync::Arc;
use tower::util::ServiceExt;

#[test]
fn remote_plugin_sources_require_http_url() {
    assert!(parse_source("address:https://example.test/plugin.tgz").is_ok());
    assert!(parse_source("center:https://example.test/plugin.tgz").is_err());
    assert!(parse_source("address:not-a-url").is_err());
}

pub(super) fn bare_state(root: &std::path::Path) -> ApiState {
    let manager = PluginManager::open(root).unwrap();
    ApiState::new(
        "magi-test",
        Arc::new(magi_event_bus::InMemoryEventBus::new(32)),
        Arc::new(SessionStore::default()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
    .with_plugin_manager(Arc::new(std::sync::Mutex::new(manager)))
}

#[tokio::test]
async fn authoring_guide_is_served_from_the_repository_document() {
    let root = tempfile::tempdir().unwrap();
    let response = routes()
        .with_state(bare_state(root.path()))
        .oneshot(
            axum::http::Request::builder()
                .uri("/plugins/guide")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.starts_with("# Magi 插件作者指南"), "{text:.60}");
    assert!(!text.contains("](../"), "界面内的链接不能依赖仓库相对路径");
}
