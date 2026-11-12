use std::sync::Arc;

use axum::{
    body::to_bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header::USER_AGENT},
};
use magi_browser_authority::{
    BrowserAnnotation, BrowserAnnotationAnchor, BrowserAnnotationAuthor, BrowserAnnotationKind,
    BrowserAnnotationStatus, BrowserHostRect, BrowserProfile, BrowserProfileKind,
    BrowserRegionAnnotationAnchor, BrowserSessionLifecycle, BrowserSessionOwner,
    BrowserTabLifecycle, BrowserViewport, CreateBrowserSession, CreateBrowserTab,
};
use magi_core::{
    BrowserAnnotationId, BrowserProfileId, BrowserSessionId, BrowserTabId, SessionId, UtcMillis,
    WorkspaceId,
};
use magi_event_bus::InMemoryEventBus;
use magi_governance::GovernanceService;
use magi_session_store::SessionStore;
use magi_workspace::WorkspaceStore;

use super::{
    AnnotationArtifactQuery, BrowserAnnotationAnchorResponse, BrowserClientPlatform,
    BrowserElementAnnotationAnchorResponse, BrowserRegionAnnotationAnchorResponse,
    BrowserTabActivation, DesktopConnectionClearRequest, DesktopConnectionRequest,
    ReclaimBrowserResourcesRequest, annotation_artifact, apply_browser_tab_activation,
    browser_platform_capabilities, browser_resources_response, browser_session_response,
    clear_desktop_connection, finish_browser_tab_creation, normalize_hit_bounds,
    prepare_browser_tab_activation, reclaim_browser_resources, register_desktop_connection,
    require_desktop_browser_capability, session_browser_artifact,
};
use crate::{
    browser_tool_runtime::resolve_browser_annotation_context,
    errors::ApiError,
    state::{ApiState, BrowserHostConnectionConfig, RuntimeStatePersistence},
};

fn capability_test_state(label: &str) -> ApiState {
    ApiState::new(
        label,
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::new()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
}

fn desktop_request_headers(state: &ApiState) -> HeaderMap {
    let token = "desktop-renderer-test-token";
    state.set_browser_host_connection_config(Some(BrowserHostConnectionConfig {
        socket_path: "/tmp/magi-browser-test.sock".to_string(),
        auth_token: token.to_string(),
        desktop_epoch: "desktop-renderer-test".to_string(),
        parent_pid: 1,
        generation: 1,
    }));
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        "Mozilla/5.0 @magi/desktop/3.0.51 Electron/43.4.0"
            .parse()
            .unwrap(),
    );
    headers.insert("x-magi-desktop-renderer-token", token.parse().unwrap());
    headers
}

fn annotation_fixture() -> (ApiState, SessionId, SessionId, BrowserAnnotationId) {
    let state = ApiState::new(
        "browser-annotation-context-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::new()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );
    let current_session_id = SessionId::new("session-browser-annotation-current");
    let other_session_id = SessionId::new("session-browser-annotation-other");
    let browser_session_id = BrowserSessionId::new("browser-session-annotation-current");
    let tab_id = BrowserTabId::new("browser-tab-annotation-current");
    let annotation_id = BrowserAnnotationId::new("browser-annotation-current");
    let profile_id = BrowserProfileId::new("browser-profile-annotation-context");
    state
        .mutate_browser_authority(|authority| {
            authority.register_profile(BrowserProfile {
                profile_id: profile_id.clone(),
                kind: BrowserProfileKind::ManagedDefault,
                data_path: tempfile::tempdir()
                    .expect("annotation profile fixture should create")
                    .keep(),
                created_at: UtcMillis(1),
                updated_at: UtcMillis(1),
            })?;
            authority.create_session(CreateBrowserSession {
                browser_session_id: browser_session_id.clone(),
                owner: BrowserSessionOwner::Session {
                    session_id: current_session_id.clone(),
                    workspace_id: Some(WorkspaceId::new("workspace-browser-annotation-current")),
                },
                profile_id,
                now: UtcMillis(1),
            })?;
            authority.transition_session(
                &browser_session_id,
                BrowserSessionLifecycle::Ready,
                UtcMillis(1),
            )?;
            authority.create_tab(CreateBrowserTab {
                tab_id: tab_id.clone(),
                browser_session_id: browser_session_id.clone(),
                url: "https://example.com/settings".to_string(),
                now: UtcMillis(1),
            })?;
            authority.transition_tab(&tab_id, BrowserTabLifecycle::Ready, UtcMillis(1))?;
            authority.apply_host_page_state(
                &tab_id,
                3,
                "https://example.com/settings".to_string(),
                Some("https://example.com".to_string()),
                "Settings".to_string(),
                UtcMillis(2),
            )?;
            authority.create_annotation(BrowserAnnotation {
                annotation_id: annotation_id.clone(),
                browser_session_id,
                tab_id,
                sequence: 0,
                author: BrowserAnnotationAuthor::User,
                kind: BrowserAnnotationKind::Region,
                anchor: BrowserAnnotationAnchor::Region(BrowserRegionAnnotationAnchor {
                    url: "https://example.com/settings".to_string(),
                    origin: Some("https://example.com".to_string()),
                    viewport: BrowserViewport::default(),
                    scroll_x: 0.0,
                    scroll_y: 120.0,
                    rect: magi_browser_authority::BrowserNormalizedRect {
                        x: 0.1,
                        y: 0.2,
                        width: 0.3,
                        height: 0.15,
                    },
                    snapshot_revision: 1,
                }),
                comment: "检查保存按钮".to_string(),
                status: BrowserAnnotationStatus::Active,
                screenshot_artifact_id: Some(
                    "session-browser-annotation-current/annotation.png".to_string(),
                ),
                created_at: UtcMillis(2),
                updated_at: UtcMillis(2),
            })?;
            Ok(())
        })
        .expect("annotation authority fixture should create");
    (state, current_session_id, other_session_id, annotation_id)
}

#[tokio::test]
async fn closing_session_publishes_tab_closure_without_browser_host() {
    let (state, session_id, _, _) = annotation_fixture();
    let tab_id = BrowserTabId::new("browser-tab-annotation-current");
    let closed = state
        .close_browser_session_for_magi_session(&session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(closed.lifecycle, BrowserSessionLifecycle::Closed);
    assert_eq!(
        state
            .browser_authority
            .lock()
            .unwrap()
            .tab(&tab_id)
            .unwrap()
            .lifecycle,
        BrowserTabLifecycle::Closed
    );
    assert!(
        state
            .event_bus
            .snapshot()
            .recent_events
            .iter()
            .any(|event| event.event_type == "browser.tab.closed"
                && event.payload["tab_id"] == tab_id.as_str())
    );
}

#[test]
fn annotation_hit_bounds_are_normalized_against_the_chromium_viewport() {
    let normalized = normalize_hit_bounds(
        1280,
        720,
        BrowserHostRect {
            x: 640.0,
            y: 180.0,
            width: 120.0,
            height: 40.0,
        },
    )
    .expect("valid Chromium element bounds should normalize");

    assert_eq!(normalized.x, 0.5);
    assert_eq!(normalized.y, 0.25);
    assert!((normalized.width - 120.0 / 1280.0).abs() < 1e-12);
    assert!((normalized.height - 40.0 / 720.0).abs() < 1e-12);
}

#[test]
fn annotation_hit_bounds_reject_an_empty_or_unavailable_viewport() {
    assert!(
        normalize_hit_bounds(
            0,
            720,
            BrowserHostRect {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
        )
        .is_err()
    );
    assert!(
        normalize_hit_bounds(
            1280,
            720,
            BrowserHostRect {
                x: 10.0,
                y: 20.0,
                width: 0.0,
                height: 40.0,
            },
        )
        .is_err()
    );
}

#[tokio::test]
async fn stale_desktop_process_cannot_clear_the_current_connection() {
    let state = ApiState::new(
        "browser-desktop-connection-owner-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::new()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );
    state.set_browser_host_connection_config(Some(BrowserHostConnectionConfig {
        socket_path: "/tmp/magi-current.sock".to_string(),
        auth_token: "current-token".to_string(),
        desktop_epoch: "desktop-current".to_string(),
        parent_pid: 200,
        generation: 3,
    }));

    let _ = clear_desktop_connection(
        axum::extract::State(state.clone()),
        axum::Json(DesktopConnectionClearRequest {
            desktop_epoch: "desktop-stale".to_string(),
            parent_pid: 100,
            generation: 3,
        }),
    )
    .await
    .expect("stale unregister should be an idempotent no-op");
    let current = state
        .browser_host_connection_config()
        .expect("current desktop connection should remain registered");
    assert_eq!(current.desktop_epoch, "desktop-current");
    assert_eq!(current.parent_pid, 200);

    let _ = clear_desktop_connection(
        axum::extract::State(state.clone()),
        axum::Json(DesktopConnectionClearRequest {
            desktop_epoch: "desktop-current".to_string(),
            parent_pid: 200,
            generation: 3,
        }),
    )
    .await
    .expect("current desktop should unregister itself");
    assert!(state.browser_host_connection_config().is_none());
}

#[tokio::test]
async fn desktop_connection_registration_releases_owner_after_clear() {
    let state = ApiState::new(
        "browser-desktop-connection-cas-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::new()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );

    let first = register_desktop_connection(
        axum::extract::State(state.clone()),
        axum::Json(DesktopConnectionRequest {
            socket_path: "/tmp/magi-first.sock".to_string(),
            auth_token: "first-token".to_string(),
            desktop_epoch: "desktop-first".to_string(),
            parent_pid: 101,
            expected_generation: 0,
        }),
    )
    .await
    .expect("first desktop registration should succeed");
    assert_eq!(first.0.desktop_connection_generation, 1);

    let stale = register_desktop_connection(
        axum::extract::State(state.clone()),
        axum::Json(DesktopConnectionRequest {
            socket_path: "/tmp/magi-stale.sock".to_string(),
            auth_token: "stale-token".to_string(),
            desktop_epoch: "desktop-stale".to_string(),
            parent_pid: 102,
            expected_generation: 0,
        }),
    )
    .await;
    assert!(matches!(stale, Err(ApiError::Conflict(_))));

    let idempotent = register_desktop_connection(
        axum::extract::State(state.clone()),
        axum::Json(DesktopConnectionRequest {
            socket_path: "/tmp/magi-first.sock".to_string(),
            auth_token: "first-token".to_string(),
            desktop_epoch: "desktop-first".to_string(),
            parent_pid: 101,
            expected_generation: 1,
        }),
    )
    .await
    .expect("same owner registration should be idempotent");
    assert_eq!(idempotent.0.desktop_connection_generation, 1);

    let replacement = register_desktop_connection(
        axum::extract::State(state.clone()),
        axum::Json(DesktopConnectionRequest {
            socket_path: "/tmp/magi-second.sock".to_string(),
            auth_token: "second-token".to_string(),
            desktop_epoch: "desktop-second".to_string(),
            parent_pid: 103,
            expected_generation: 1,
        }),
    )
    .await;
    assert!(matches!(replacement, Err(ApiError::Conflict(_))));

    let _ = clear_desktop_connection(
        axum::extract::State(state.clone()),
        axum::Json(DesktopConnectionClearRequest {
            desktop_epoch: "desktop-second".to_string(),
            parent_pid: 103,
            generation: 1,
        }),
    )
    .await
    .expect("stale clear should be an idempotent no-op");
    let current = state
        .browser_host_connection_config()
        .expect("first owner should remain registered");
    assert_eq!(current.desktop_epoch, "desktop-first");
    assert_eq!(current.generation, 1);

    let _ = clear_desktop_connection(
        axum::extract::State(state.clone()),
        axum::Json(DesktopConnectionClearRequest {
            desktop_epoch: "desktop-first".to_string(),
            parent_pid: 101,
            generation: 1,
        }),
    )
    .await
    .expect("owner should clear its active connection");
    assert!(state.browser_host_connection_config().is_none());

    let takeover_after_clear = register_desktop_connection(
        axum::extract::State(state.clone()),
        axum::Json(DesktopConnectionRequest {
            socket_path: "/tmp/magi-second.sock".to_string(),
            auth_token: "second-token".to_string(),
            desktop_epoch: "desktop-second".to_string(),
            parent_pid: 103,
            expected_generation: 1,
        }),
    )
    .await;
    let takeover_after_clear = takeover_after_clear
        .expect("a new Desktop owner should register after the previous owner clears");
    assert_eq!(takeover_after_clear.0.desktop_connection_generation, 2);

    let stale_clear = clear_desktop_connection(
        axum::extract::State(state.clone()),
        axum::Json(DesktopConnectionClearRequest {
            desktop_epoch: "desktop-first".to_string(),
            parent_pid: 101,
            generation: 1,
        }),
    )
    .await
    .expect("stale clear after takeover should remain idempotent");
    assert_eq!(stale_clear.0.desktop_connection_generation, 2);
    assert_eq!(
        state
            .browser_host_connection_config()
            .expect("replacement Desktop should remain registered")
            .desktop_epoch,
        "desktop-second"
    );
}

#[test]
fn annotation_context_is_session_scoped_and_keeps_stale_artifact_context() {
    let (state, session_id, other_session_id, annotation_id) = annotation_fixture();
    let resolved =
        resolve_browser_annotation_context(&state, &session_id, &[annotation_id.to_string()])
            .expect("active annotation should resolve");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0]["annotationId"], annotation_id.to_string());
    assert_eq!(resolved[0]["sequence"], 1);
    assert_eq!(resolved[0]["comment"], "检查保存按钮");
    assert_eq!(resolved[0]["anchor"]["url"], "https://example.com/settings");
    assert_eq!(
        resolved[0]["screenshotArtifactId"],
        "session-browser-annotation-current/annotation.png"
    );

    assert!(matches!(
        resolve_browser_annotation_context(&state, &other_session_id, &[annotation_id.to_string()]),
        Err(ApiError::Conflict(_))
    ));

    state
        .mutate_browser_authority(|authority| {
            authority.update_annotation_status(
                &annotation_id,
                BrowserAnnotationStatus::Stale,
                UtcMillis(3),
            )
        })
        .expect("annotation should become stale");
    let stale =
        resolve_browser_annotation_context(&state, &session_id, &[annotation_id.to_string()])
            .expect("stale annotation artifact should remain usable as historical context");
    assert_eq!(stale[0]["status"], "stale");

    state
        .mutate_browser_authority(|authority| {
            authority.update_annotation_status(
                &annotation_id,
                BrowserAnnotationStatus::Resolved,
                UtcMillis(4),
            )
        })
        .expect("annotation should resolve");
    assert!(matches!(
        resolve_browser_annotation_context(&state, &session_id, &[annotation_id.to_string()]),
        Err(ApiError::Conflict(_))
    ));
}

#[tokio::test]
async fn annotation_artifact_is_session_scoped_and_survives_persistence_boundary() {
    let (state, session_id, other_session_id, annotation_id) = annotation_fixture();
    let durable = state
        .browser_authority
        .lock()
        .expect("browser authority lock should hold")
        .durable_state();
    let state_root = tempfile::tempdir()
        .expect("artifact state root should create")
        .keep();
    std::fs::create_dir_all(
        state_root.join("browser/artifacts/session-browser-annotation-current"),
    )
    .expect("artifact directory should create");
    std::fs::write(
        state_root.join("browser/state.json"),
        serde_json::to_vec(&durable).expect("browser durable state should serialize"),
    )
    .expect("browser durable state should write");
    let artifact_bytes = vec![137, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3];
    std::fs::write(
        state_root.join("browser/artifacts/session-browser-annotation-current/annotation.png"),
        &artifact_bytes,
    )
    .expect("annotation artifact should write");
    let state = state.with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
        state_root.clone(),
        state_root.join("workspaces.json"),
        state_root.join("knowledge.json"),
    )));
    state
        .mutate_browser_authority(|authority| {
            authority.transition_session(
                &BrowserSessionId::new("browser-session-annotation-current"),
                BrowserSessionLifecycle::Ready,
                UtcMillis(8),
            )?;
            authority.transition_tab(
                &BrowserTabId::new("browser-tab-annotation-current"),
                BrowserTabLifecycle::Ready,
                UtcMillis(8),
            )?;
            Ok(())
        })
        .expect("restored browser annotation state should become ready");

    // 工具截图按「会话 + 单段文件名」读取：只能读已存在会话自己目录下的图片。
    state
        .session_store
        .create_session(session_id.clone(), "截图会话")
        .expect("session should create");
    let screenshot = session_browser_artifact(
        State(state.clone()),
        Path((session_id.to_string(), "annotation.png".to_string())),
    )
    .await
    .expect("session should read its own browser artifact");
    assert_eq!(
        screenshot
            .headers()
            .get("content-type")
            .map(|value| value.as_bytes()),
        Some(b"image/png".as_slice())
    );
    for (artifact_session, file_name) in [
        (session_id.to_string(), "../annotation.png".to_string()),
        (session_id.to_string(), "state.json".to_string()),
        ("..".to_string(), "state.json".to_string()),
        (other_session_id.to_string(), "annotation.png".to_string()),
    ] {
        assert!(
            session_browser_artifact(
                State(state.clone()),
                Path((artifact_session.clone(), file_name.clone())),
            )
            .await
            .is_err(),
            "{artifact_session}/{file_name} must be rejected"
        );
    }

    let response = annotation_artifact(
        State(state.clone()),
        Path(annotation_id.to_string()),
        Query(AnnotationArtifactQuery {
            session_id: session_id.to_string(),
        }),
    )
    .await
    .expect("current session should read its annotation artifact");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("image/png")
    );
    assert_eq!(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("artifact response body should read")
            .as_ref(),
        artifact_bytes.as_slice()
    );

    assert!(matches!(
        annotation_artifact(
            State(state.clone()),
            Path(annotation_id.to_string()),
            Query(AnnotationArtifactQuery {
                session_id: other_session_id.to_string(),
            }),
        )
        .await,
        Err(ApiError::NotFound(_))
    ));
    assert!(matches!(
        crate::browser_tool_runtime::browser_artifact_path(&state, "../outside.png"),
        Err(ApiError::Conflict(_))
    ));
    assert!(matches!(
        crate::browser_tool_runtime::browser_artifact_path(&state, "missing.png"),
        Err(ApiError::NotFound(_))
    ));

    let (annotation_without_artifact, restored_snapshot_revision) = {
        let authority = state
            .browser_authority
            .lock()
            .expect("browser authority lock should hold");
        let annotation = authority
            .annotation(&annotation_id)
            .expect("fixture annotation should exist")
            .clone();
        let snapshot_revision = authority
            .tab(&BrowserTabId::new("browser-tab-annotation-current"))
            .expect("fixture tab should exist")
            .snapshot_revision;
        (annotation, snapshot_revision)
    };
    state
        .mutate_browser_authority(|authority| {
            let mut annotation = annotation_without_artifact.clone();
            annotation.annotation_id =
                BrowserAnnotationId::new("browser-annotation-without-artifact");
            annotation.screenshot_artifact_id = None;
            annotation.sequence = 0;
            match &mut annotation.anchor {
                BrowserAnnotationAnchor::Region(anchor) => {
                    anchor.snapshot_revision = restored_snapshot_revision;
                }
                BrowserAnnotationAnchor::Element(anchor) => {
                    anchor.snapshot_revision = restored_snapshot_revision;
                }
            }
            authority.create_annotation(annotation)
        })
        .expect("annotation without artifact should create");
    assert!(matches!(
        annotation_artifact(
            State(state.clone()),
            Path("browser-annotation-without-artifact".to_string()),
            Query(AnnotationArtifactQuery {
                session_id: session_id.to_string(),
            }),
        )
        .await,
        Err(ApiError::NotFound(_))
    ));

    state
        .mutate_browser_authority(|authority| {
            authority.transition_session(
                &BrowserSessionId::new("browser-session-annotation-current"),
                BrowserSessionLifecycle::Closed,
                UtcMillis(9),
            )?;
            Ok(())
        })
        .expect("closing the browser session should succeed");
    let response_after_browser_close = annotation_artifact(
        State(state),
        Path(annotation_id.to_string()),
        Query(AnnotationArtifactQuery {
            session_id: session_id.to_string(),
        }),
    )
    .await
    .expect("historical annotation artifact should survive browser session close");
    assert_eq!(response_after_browser_close.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response_after_browser_close.into_body(), usize::MAX)
            .await
            .expect("closed-session artifact response body should read")
            .as_ref(),
        artifact_bytes.as_slice()
    );
}

#[test]
fn annotation_anchor_response_serializes_variant_fields_as_camel_case() {
    let value = serde_json::to_value(BrowserAnnotationAnchorResponse::Region(
        BrowserRegionAnnotationAnchorResponse {
            url: "https://example.com/settings".to_string(),
            origin: Some("https://example.com".to_string()),
            viewport: BrowserViewport::default().into(),
            scroll_x: 12.0,
            scroll_y: 24.0,
            rect: magi_browser_authority::BrowserNormalizedRect {
                x: 0.1,
                y: 0.2,
                width: 0.3,
                height: 0.4,
            },
            snapshot_revision: 7,
        },
    ))
    .expect("annotation anchor response should serialize");

    assert_eq!(value["kind"], "region");
    assert_eq!(value["scrollX"], 12.0);
    assert_eq!(value["scrollY"], 24.0);
    assert_eq!(value["snapshotRevision"], 7);
    assert!(value.get("rect").is_some());
    assert!(value.get("scroll_x").is_none());
    assert!(value.get("scroll_y").is_none());
    assert!(value.get("snapshot_revision").is_none());

    let element = serde_json::to_value(BrowserAnnotationAnchorResponse::Element(Box::new(
        BrowserElementAnnotationAnchorResponse {
            url: "https://example.com/settings".to_string(),
            origin: Some("https://example.com".to_string()),
            frame_path: Vec::new(),
            viewport: BrowserViewport::default().into(),
            scroll_x: 0.0,
            scroll_y: 0.0,
            test_id: None,
            stable_id: None,
            aria_role: Some("button".to_string()),
            aria_name: Some("Save".to_string()),
            tag_name: "button".to_string(),
            text_excerpt: Some("Save".to_string()),
            css_path: "button.save".to_string(),
            ancestor_fingerprint: "ancestor".to_string(),
            dom_fingerprint: "dom".to_string(),
            bounding_box: magi_browser_authority::BrowserNormalizedRect {
                x: 0.1,
                y: 0.2,
                width: 0.3,
                height: 0.4,
            },
            snapshot_revision: 8,
        },
    )))
    .expect("element annotation anchor response should serialize");

    assert!(element.get("boundingBox").is_some());
    assert_eq!(element["snapshotRevision"], 8);
    assert!(element.get("bounding_box").is_none());
    assert!(element.get("snapshot_revision").is_none());
}

#[test]
fn browser_tab_response_preserves_primary_surface_after_navigation() {
    let state = ApiState::new(
        "browser-tab-response-surface-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::new()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );
    let profile_id = BrowserProfileId::new("browser-profile-response-surface");
    let browser_session_id = BrowserSessionId::new("browser-session-response-surface");
    let tab_id = BrowserTabId::new("browser-tab-response-surface");
    let session_id = SessionId::new("session-response-surface");

    state
        .mutate_browser_authority(|authority| {
            authority.register_profile(BrowserProfile {
                profile_id: profile_id.clone(),
                kind: BrowserProfileKind::ManagedDefault,
                data_path: tempfile::tempdir()
                    .expect("surface response profile should create")
                    .keep(),
                created_at: UtcMillis(1),
                updated_at: UtcMillis(1),
            })?;
            authority.create_session(CreateBrowserSession {
                browser_session_id: browser_session_id.clone(),
                owner: BrowserSessionOwner::Session {
                    session_id,
                    workspace_id: None,
                },
                profile_id,
                now: UtcMillis(1),
            })?;
            authority.transition_session(
                &browser_session_id,
                BrowserSessionLifecycle::Ready,
                UtcMillis(2),
            )?;
            authority.create_tab(CreateBrowserTab {
                tab_id: tab_id.clone(),
                browser_session_id: browser_session_id.clone(),
                url: "https://example.com/old".to_string(),
                now: UtcMillis(2),
            })?;
            authority.transition_tab(&tab_id, BrowserTabLifecycle::Ready, UtcMillis(2))?;
            authority.set_primary_surface(
                magi_browser_authority::BrowserSurfaceBinding {
                    desktop_epoch: "desktop-response".to_string(),
                    window_id: "window-response".to_string(),
                    surface_id: "surface-response".to_string(),
                    surface_revision: 3,
                    tab_id: tab_id.clone(),
                    web_contents_id: 23,
                    target_id: "target-response".to_string(),
                    browser_context_id: "context-response".to_string(),
                    navigation_revision: 0,
                },
                UtcMillis(3),
            )?;
            authority.apply_host_page_state(
                &tab_id,
                1,
                "https://example.com/new".to_string(),
                Some("https://example.com".to_string()),
                "New page".to_string(),
                UtcMillis(4),
            )?;
            Ok(())
        })
        .expect("surface response fixture should create");

    let session = state
        .browser_authority
        .lock()
        .expect("browser authority lock should hold")
        .session(&browser_session_id)
        .cloned()
        .expect("browser session should exist");
    let response = browser_session_response(&state, session)
        .expect("browser session response should assemble");
    assert_eq!(response.tabs.len(), 1);
    assert_eq!(
        response.tabs[0].surface_id.as_deref(),
        Some("surface-response")
    );
    assert_eq!(response.tabs[0].url, "https://example.com/new");
}

#[test]
fn platform_capabilities_are_explicit_for_each_client() {
    let desktop = serde_json::to_value(browser_platform_capabilities(
        BrowserClientPlatform::Desktop,
    ))
    .expect("desktop capabilities should serialize");
    assert_eq!(desktop["desktopBrowserSurface"], true);
    assert_eq!(desktop["browserRecords"], true);
    assert_eq!(desktop["browserAnnotations"], true);
    assert_eq!(desktop["browserRemoteSurface"], false);

    let mobile = serde_json::to_value(browser_platform_capabilities(
        BrowserClientPlatform::MobileWeb,
    ))
    .expect("mobile web capabilities should serialize");
    assert_eq!(mobile["desktopBrowserSurface"], false);
    assert_eq!(mobile["browserRecords"], true);
    assert_eq!(mobile["browserAnnotations"], true);
    assert_eq!(mobile["browserRemoteSurface"], false);
}

#[test]
fn real_browser_operations_are_unavailable_to_web_clients() {
    let state = capability_test_state("browser-client-platform-test");
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, "Mozilla/5.0".parse().unwrap());
    let error = require_desktop_browser_capability(&state, &headers, None)
        .expect_err("web clients must not operate the Desktop browser host");
    assert!(matches!(error, ApiError::CapabilityUnavailable { .. }));

    headers.insert(
        USER_AGENT,
        "Mozilla/5.0 @magi/desktop/3.0.51 Electron/43.4.0"
            .parse()
            .unwrap(),
    );
    let guest_error = require_desktop_browser_capability(&state, &headers, None)
        .expect_err("Electron guest without the App Renderer token must remain untrusted");
    assert!(matches!(
        guest_error,
        ApiError::CapabilityUnavailable { .. }
    ));

    headers = desktop_request_headers(&state);
    require_desktop_browser_capability(&state, &headers, None)
        .expect("Electron clients should be able to operate the Desktop browser host");
}

#[test]
fn declared_desktop_platform_cannot_elevate_web_or_tunnel_requests() {
    let state = capability_test_state("browser-client-elevation-test");
    let mut web_headers = HeaderMap::new();
    web_headers.insert(USER_AGENT, "Mozilla/5.0".parse().unwrap());
    let web_error = require_desktop_browser_capability(
        &state,
        &web_headers,
        Some(BrowserClientPlatform::Desktop),
    )
    .expect_err("clientPlatform must not elevate a web request");
    assert!(matches!(web_error, ApiError::CapabilityUnavailable { .. }));

    let mut tunnel_headers = desktop_request_headers(&state);
    tunnel_headers.insert("cf-ray", "test".parse().unwrap());
    let tunnel_error = require_desktop_browser_capability(
        &state,
        &tunnel_headers,
        Some(BrowserClientPlatform::Desktop),
    )
    .expect_err("public tunnel requests must remain record-only");
    assert!(matches!(
        tunnel_error,
        ApiError::CapabilityUnavailable { .. }
    ));
}

#[test]
fn explicit_non_desktop_platform_cannot_be_overridden_by_a_desktop_user_agent() {
    let state = capability_test_state("browser-client-downgrade-test");
    let headers = desktop_request_headers(&state);
    let error = require_desktop_browser_capability(
        &state,
        &headers,
        Some(BrowserClientPlatform::MobileWeb),
    )
    .expect_err("explicit mobile platform must remain record-only");
    assert!(matches!(error, ApiError::CapabilityUnavailable { .. }));
}

#[test]
fn failed_host_materialization_keeps_logical_tab_recoverable() {
    let state = ApiState::new(
        "browser-tab-materialization-failure-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::new()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );
    let session_id = SessionId::new("session-browser-tab-materialization-failure");
    let browser_session_id = BrowserSessionId::new("browser-session-materialization-failure");
    let tab_id = BrowserTabId::new("browser-tab-materialization-failure");
    let profile_id = BrowserProfileId::new("browser-profile-materialization-failure");

    state
        .mutate_browser_authority(|authority| {
            authority.register_profile(BrowserProfile {
                profile_id: profile_id.clone(),
                kind: BrowserProfileKind::ManagedDefault,
                data_path: tempfile::tempdir()
                    .expect("materialization failure profile should create")
                    .keep(),
                created_at: UtcMillis(1),
                updated_at: UtcMillis(1),
            })?;
            authority.create_session(CreateBrowserSession {
                browser_session_id: browser_session_id.clone(),
                owner: BrowserSessionOwner::Session {
                    session_id: session_id.clone(),
                    workspace_id: None,
                },
                profile_id,
                now: UtcMillis(1),
            })?;
            authority.transition_session(
                &browser_session_id,
                BrowserSessionLifecycle::Ready,
                UtcMillis(2),
            )?;
            authority.create_tab(CreateBrowserTab {
                tab_id: tab_id.clone(),
                browser_session_id: browser_session_id.clone(),
                url: "https://example.com".to_string(),
                now: UtcMillis(2),
            })?;
            Ok(())
        })
        .expect("creating browser tab fixture should succeed");

    finish_browser_tab_creation(
        &state,
        &None,
        &session_id,
        &browser_session_id,
        &tab_id,
        "surface unavailable",
    );

    let tab = state
        .browser_authority
        .lock()
        .expect("browser authority lock should hold")
        .tab(&tab_id)
        .cloned()
        .expect("failed tab should remain in authority");
    assert_eq!(tab.lifecycle, BrowserTabLifecycle::Crashed);
}

#[test]
fn late_creation_failure_cannot_overwrite_ready_tab() {
    let (state, session_id, _, _) = annotation_fixture();
    let browser_session_id = BrowserSessionId::new("browser-session-annotation-current");
    let tab_id = BrowserTabId::new("browser-tab-annotation-current");

    finish_browser_tab_creation(
        &state,
        &None,
        &session_id,
        &browser_session_id,
        &tab_id,
        "late create failure",
    );

    let tab = state
        .browser_authority
        .lock()
        .expect("browser authority lock should hold")
        .tab(&tab_id)
        .cloned()
        .expect("ready tab should remain in authority");
    assert_eq!(tab.lifecycle, BrowserTabLifecycle::Ready);
}

#[test]
fn immediate_activation_converges_a_creating_tab_to_ready() {
    let (state, _, _, _) = annotation_fixture();
    let browser_session_id = BrowserSessionId::new("browser-session-annotation-current");
    let tab_id = BrowserTabId::new("browser-tab-activation-creating");
    state
        .mutate_browser_authority(|authority| {
            authority.create_tab(CreateBrowserTab {
                tab_id: tab_id.clone(),
                browser_session_id,
                url: "https://example.com/activation".to_string(),
                now: UtcMillis(5),
            })?;
            Ok(())
        })
        .expect("creating tab fixture should succeed");

    let tab = state
        .mutate_browser_authority(|authority| prepare_browser_tab_activation(authority, &tab_id))
        .expect("creating tab should be eligible for activation");
    assert_eq!(tab.lifecycle, BrowserTabLifecycle::Creating);

    let resolution = state
        .mutate_browser_authority(|authority| {
            apply_browser_tab_activation(
                authority,
                &tab_id,
                magi_browser_authority::BrowserHostPageState {
                    tab_id: tab_id.clone(),
                    url: "https://example.com/activation".to_string(),
                    origin: Some("https://example.com".to_string()),
                    title: "Activation".to_string(),
                    navigation_revision: 0,
                },
            )
        })
        .expect("creating tab should converge after host restore");
    assert!(matches!(resolution, BrowserTabActivation::Completed(_)));
    assert_eq!(
        state
            .browser_authority
            .lock()
            .expect("browser authority lock should hold")
            .tab(&tab_id)
            .expect("activated tab should remain in authority")
            .lifecycle,
        BrowserTabLifecycle::Ready
    );
}

#[test]
fn activation_retries_when_creation_failure_wins_the_authority_race() {
    let (state, session_id, _, _) = annotation_fixture();
    let browser_session_id = BrowserSessionId::new("browser-session-annotation-current");
    let tab_id = BrowserTabId::new("browser-tab-activation-race");
    state
        .mutate_browser_authority(|authority| {
            authority.create_tab(CreateBrowserTab {
                tab_id: tab_id.clone(),
                browser_session_id: browser_session_id.clone(),
                url: "https://example.com/race".to_string(),
                now: UtcMillis(5),
            })
        })
        .expect("creating tab fixture should succeed");

    let creating = state
        .mutate_browser_authority(|authority| prepare_browser_tab_activation(authority, &tab_id))
        .expect("creating tab should be eligible for activation");
    assert_eq!(creating.lifecycle, BrowserTabLifecycle::Creating);

    finish_browser_tab_creation(
        &state,
        &None,
        &session_id,
        &browser_session_id,
        &tab_id,
        "creation failed while activation was restoring",
    );

    let retry = state
        .mutate_browser_authority(|authority| {
            apply_browser_tab_activation(
                authority,
                &tab_id,
                magi_browser_authority::BrowserHostPageState {
                    tab_id: tab_id.clone(),
                    url: "https://example.com/race".to_string(),
                    origin: Some("https://example.com".to_string()),
                    title: "Race".to_string(),
                    navigation_revision: creating.navigation_revision,
                },
            )
        })
        .expect("crashed activation should request a retry");
    let suspended = match retry {
        BrowserTabActivation::Retry(tab) => tab,
        BrowserTabActivation::Completed(_) => {
            panic!("a crashed tab must retry with suspended revisions")
        }
    };
    assert_eq!(suspended.lifecycle, BrowserTabLifecycle::Suspended);

    let resolution = state
        .mutate_browser_authority(|authority| {
            apply_browser_tab_activation(
                authority,
                &tab_id,
                magi_browser_authority::BrowserHostPageState {
                    tab_id: tab_id.clone(),
                    url: "https://example.com/race".to_string(),
                    origin: Some("https://example.com".to_string()),
                    title: "Race".to_string(),
                    navigation_revision: suspended.navigation_revision,
                },
            )
        })
        .expect("retried suspended tab should converge after restore");
    assert!(matches!(resolution, BrowserTabActivation::Completed(_)));
    assert_eq!(
        state
            .browser_authority
            .lock()
            .expect("browser authority lock should hold")
            .tab(&tab_id)
            .expect("restored tab should remain in authority")
            .lifecycle,
        BrowserTabLifecycle::Ready
    );
}

#[tokio::test]
async fn reclaim_resources_closes_inactive_tab_even_without_browser_host() {
    let state = ApiState::new(
        "browser-resource-reclaim-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::new()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );
    let session_id = SessionId::new("session-browser-resource-reclaim");
    let browser_session_id = BrowserSessionId::new("browser-session-resource-reclaim");
    let tab_id = BrowserTabId::new("browser-tab-resource-reclaim");
    let profile_id = BrowserProfileId::new("browser-profile-resource-reclaim");

    state
        .mutate_browser_authority(|authority| {
            authority.register_profile(BrowserProfile {
                profile_id: profile_id.clone(),
                kind: BrowserProfileKind::ManagedDefault,
                data_path: tempfile::tempdir()
                    .expect("resource profile should create")
                    .keep(),
                created_at: UtcMillis(1),
                updated_at: UtcMillis(1),
            })?;
            authority.create_session(CreateBrowserSession {
                browser_session_id: browser_session_id.clone(),
                owner: BrowserSessionOwner::Session {
                    session_id: session_id.clone(),
                    workspace_id: None,
                },
                profile_id,
                now: UtcMillis(1),
            })?;
            authority.transition_session(
                &browser_session_id,
                BrowserSessionLifecycle::Ready,
                UtcMillis(2),
            )?;
            authority.create_tab(CreateBrowserTab {
                tab_id: tab_id.clone(),
                browser_session_id: browser_session_id.clone(),
                url: "https://example.com/old".to_string(),
                now: UtcMillis(2),
            })?;
            authority.transition_tab(&tab_id, BrowserTabLifecycle::Suspended, UtcMillis(3))?;
            Ok(())
        })
        .expect("resource fixture should create");

    let before = browser_resources_response(&state);
    assert_eq!(before.live_tabs, 1);
    assert_eq!(before.reclaimable_tabs, 1);
    assert!(before.tabs[0].can_reclaim);

    let response = reclaim_browser_resources(
        axum::extract::State(state.clone()),
        desktop_request_headers(&state),
        axum::Json(ReclaimBrowserResourcesRequest {
            tab_ids: vec![tab_id.to_string()],
        }),
    )
    .await
    .expect("resource reclamation should not require a live Host");
    assert_eq!(response.0.reclaimed_count, 1);
    assert_eq!(response.0.reclaimed_tab_ids, vec![tab_id.clone()]);
    assert_eq!(response.0.resources.live_tabs, 0);
    assert_eq!(response.0.resources.reclaimable_tabs, 0);

    let authority = state
        .browser_authority
        .lock()
        .expect("browser authority lock should hold");
    assert_eq!(
        authority
            .session(&browser_session_id)
            .expect("browser session should be retained")
            .lifecycle,
        BrowserSessionLifecycle::Ready
    );
    assert!(
        authority
            .session(&browser_session_id)
            .expect("browser session should be retained")
            .tab_ids
            .is_empty()
    );
    assert_eq!(
        authority
            .tab(&tab_id)
            .expect("closed tab should remain in audit history")
            .lifecycle,
        BrowserTabLifecycle::Closed
    );
}
