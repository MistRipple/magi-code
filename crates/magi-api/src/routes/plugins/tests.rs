use super::*;
use axum::body::Body;
use futures_util::StreamExt;
use magi_governance::GovernanceService;
use magi_plugin_system::{PluginManager, PluginPackage, PluginSource};
use magi_session_store::SessionStore;
use magi_workspace::WorkspaceStore;
use std::{
    io::{Cursor, Write},
    sync::Arc,
    time::Duration,
};
use tokio::time::timeout;
use tower::util::ServiceExt;
use zip::ZipWriter;

fn archive(version: &str) -> Vec<u8> {
    let manifest = serde_json::json!({
        "sdkVersion": 2,
        "id": "acme.resources",
        "version": version,
        "name": "Resources",
        "description": "",
        "permissions": [{
            "kind": "storage",
            "targets": ["dashboard"]
        }],
        "dataSchemaVersion": 1,
        "settingsSchema": {"type":"object","additionalProperties":false},
        "contributions": {"resources":[{"id":"dashboard","title":"Dashboard","description":""}]}
    });
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    {
        let (name, bytes) = ("manifest.json", serde_json::to_vec(&manifest).unwrap());
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn package() -> PluginPackage {
    PluginPackage::from_archive(&archive("1.0.0")).unwrap()
}

#[test]
fn remote_plugin_sources_require_http_url() {
    assert!(parse_source("address:https://example.test/plugin.zip").is_ok());
    assert!(parse_source("center:https://example.test/plugin.zip").is_err());
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

async fn post_json(
    state: &ApiState,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = routes()
        .with_state(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(uri)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn the_official_center_is_the_default_and_clearing_the_address_restores_it() {
    let root = tempfile::tempdir().unwrap();
    let state = bare_state(root.path());
    assert_eq!(read_center_config(&state).url, OFFICIAL_PLUGIN_CENTER_URL);
    assert!(is_official_center(OFFICIAL_PLUGIN_CENTER_URL));
    assert!(!is_official_center("https://example.test/catalog.json"));

    let custom = PluginCenterConfig {
        url: "https://example.test/catalog.json".into(),
        trusted_keys: Vec::new(),
    };
    let _ = write_center_config(State(state.clone()), Json(custom))
        .await
        .unwrap();
    assert_eq!(
        read_center_config(&state).url,
        "https://example.test/catalog.json"
    );

    // 地址与公钥都留空：清除已保存的配置，回到官方插件中心。
    let cleared = PluginCenterConfig {
        url: String::new(),
        trusted_keys: Vec::new(),
    };
    let restored = write_center_config(State(state.clone()), Json(cleared))
        .await
        .unwrap();
    assert_eq!(restored.0.url, OFFICIAL_PLUGIN_CENTER_URL);
    assert_eq!(read_center_config(&state).url, OFFICIAL_PLUGIN_CENTER_URL);
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

#[tokio::test]
async fn preview_reports_the_candidate_without_installing_and_install_binds_the_digest() {
    let root = tempfile::tempdir().unwrap();
    let state = bare_state(root.path());
    let encoded = base64::engine::general_purpose::STANDARD.encode(archive("1.0.0"));
    let request = |digest: Option<&str>| {
        let mut body = serde_json::json!({"source": "local:a.zip", "archiveBase64": encoded});
        if let Some(digest) = digest {
            body["expectedDigest"] = digest.into();
        }
        body
    };

    let (status, preview) = post_json(&state, "/plugins/preview", request(None)).await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["operation"], "install");
    assert_eq!(preview["addedPermissions"][0]["kind"], "storage");
    assert!(
        state
            .plugin_manager
            .lock()
            .unwrap()
            .state()
            .plugins
            .is_empty()
    );
    let digest = preview["digest"].as_str().unwrap().to_owned();

    // 确认的摘要与实际内容不一致时必须拒绝，且不留下任何安装状态。
    let (status, _) = post_json(&state, "/plugins/install", request(Some("0000"))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        state
            .plugin_manager
            .lock()
            .unwrap()
            .state()
            .plugins
            .is_empty()
    );

    let (status, _) = post_json(&state, "/plugins/install", request(Some(&digest))).await;
    assert_eq!(status, StatusCode::OK);
    let (_, again) = post_json(&state, "/plugins/preview", request(None)).await;
    assert_eq!(again["operation"], "unchanged");
}

#[tokio::test]
async fn archive_upload_limits_follow_package_contract_for_preview_install_and_upgrade() {
    let root = tempfile::tempdir().unwrap();
    let state = bare_state(root.path());
    for (version, operation) in [("1.0.0", "install"), ("1.1.0", "upgrade")] {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let mut manifest = serde_json::to_value(package().manifest()).unwrap();
        manifest["version"] = version.into();
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("ui/payload.css", vec![b' '; 2 * 1024 * 1024]),
        ] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        let archive = writer.finish().unwrap().into_inner();
        assert!(archive.len() < MAX_PACKAGE_BYTES);
        let mut request = serde_json::json!({
            "source": "local:public-sdk.zip",
            "archiveBase64": base64::engine::general_purpose::STANDARD.encode(archive),
        });
        let (status, preview) = post_json(&state, "/plugins/preview", request.clone()).await;
        assert_eq!(status, StatusCode::OK, "{preview}");
        assert_eq!(preview["operation"], operation);
        request["expectedDigest"] = preview["digest"].clone();
        let (status, result) = post_json(&state, &format!("/plugins/{operation}"), request).await;
        assert_eq!(status, StatusCode::OK, "{result}");
    }
    for uri in ["/plugins/preview", "/plugins/install", "/plugins/upgrade"] {
        let response = routes()
            .with_state(state.clone())
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(vec![b' '; MAX_INSTALL_REQUEST_BYTES + 1]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE, "{uri}");
    }
}

async fn active_resource_state() -> (ApiState, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    manager
        .install(
            &package(),
            PluginSource::Local {
                name: "test.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.resources").unwrap();
    let manager = Arc::new(std::sync::Mutex::new(manager));
    let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
    let api_state = ApiState::new(
        "magi-test",
        event_bus,
        Arc::new(SessionStore::default()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
    .with_plugin_manager(manager);
    (api_state, root)
}

#[tokio::test]
async fn view_resources_carry_a_response_level_sandbox() {
    for trusted in [false, true] {
        let mut manifest = serde_json::json!({
            "sdkVersion": 2,
            "id": "acme.viewer",
            "version": "1.0.0",
            "name": "Viewer",
            "description": "",
            "permissions": [],
            "dataSchemaVersion": 1,
            "settingsSchema": {"type":"object","additionalProperties":false},
            "contributions": {"views":[{
                "id": "page", "title": "Page", "entry": "ui/index.html",
                "placements": ["main"]
            }]}
        });
        if trusted {
            manifest["extension"] = serde_json::json!({"hooks":[]});
            manifest["permissions"] =
                serde_json::json!([{"kind":"extension","targets":["full-trust"]}]);
        }
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("ui/index.html", b"<!doctype html><title>x</title>".to_vec()),
        ] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        if trusted {
            writer
                .start_file(
                    "extension/index.mjs",
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
            writer.write_all(b"export default () => {};").unwrap();
        }
        let package = PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut manager = PluginManager::open(root.path()).unwrap();
        manager
            .install(
                &package,
                PluginSource::Local {
                    name: "viewer.zip".into(),
                },
            )
            .unwrap();
        manager.enable("acme.viewer").unwrap();
        let state = ApiState::new(
            "magi-test",
            Arc::new(magi_event_bus::InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_plugin_manager(Arc::new(std::sync::Mutex::new(manager)));
        let response = routes()
            .with_state(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/plugins/acme.viewer/ui/index.html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let csp = response
            .headers()
            .get("content-security-policy")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            csp.starts_with("sandbox allow-scripts allow-downloads;"),
            "{csp}"
        );
        assert!(!csp.contains("allow-same-origin"), "{csp}");
        assert!(!csp.contains("allow-popups"), "{csp}");
        assert!(csp.contains("connect-src 'none'"), "{csp}");
        assert!(csp.contains("img-src 'self' data: blob:"), "{csp}");
        assert!(csp.contains("object-src 'none'"), "{csp}");
        assert_eq!(csp.contains("unsafe-eval"), trusted);
        drop(root);
    }
}

#[tokio::test]
async fn conversation_view_admission_binds_enablement_digest_and_declared_placement() {
    let root = tempfile::tempdir().unwrap();
    let state = bare_state(root.path());
    let manifest = serde_json::json!({
        "sdkVersion":2,"id":"acme.conversation","version":"1.0.0","name":"Conversation","description":"",
        "dataSchemaVersion":1,"settingsSchema":{"type":"object"},
        "permissions":[{"kind":"sessionRead","targets":["render"]}],
        "contributions":{"views":[{"id":"reply","title":"Reply","entry":"ui/index.html","placements":["chatAssistant"]}]}
    });
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ("ui/index.html", b"<!doctype html><title>x</title>".to_vec()),
    ] {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    let package = PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap();
    {
        let mut manager = state.plugin_manager.lock().unwrap();
        manager
            .install(
                &package,
                PluginSource::Local {
                    name: "conversation.zip".into(),
                },
            )
            .unwrap();
        manager.disable("acme.conversation").unwrap();
    }
    let endpoint = "/plugins/acme.conversation/views/admit";
    let request = serde_json::json!({"expectedDigest":package.digest(),"entry":"ui/index.html","placement":"chatAssistant"});
    assert_eq!(
        post_json(&state, endpoint, request.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    // 启用之前一律拒绝；启用后才可准入。
    state
        .plugin_manager
        .lock()
        .unwrap()
        .enable("acme.conversation")
        .unwrap();
    assert_eq!(
        post_json(&state, endpoint, request.clone()).await.0,
        StatusCode::NO_CONTENT
    );
    for (field, value, status) in [
        ("expectedDigest", "0".repeat(64), StatusCode::CONFLICT),
        ("entry", "ui/other.html".into(), StatusCode::FORBIDDEN),
        ("placement", "composerLeft".into(), StatusCode::FORBIDDEN),
        ("placement", "rightPane".into(), StatusCode::FORBIDDEN),
    ] {
        let mut changed = request.clone();
        changed[field] = serde_json::json!(value);
        assert_eq!(post_json(&state, endpoint, changed).await.0, status);
    }
    state
        .plugin_manager
        .lock()
        .unwrap()
        .disable("acme.conversation")
        .unwrap();
    assert_eq!(
        post_json(&state, endpoint, request).await.0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn composer_admission_binds_enablement_current_digest_and_view() {
    for target in ["images", "text"] {
        check_composer_admission(target).await;
    }
}

async fn check_composer_admission(target: &str) {
    let root = tempfile::tempdir().unwrap();
    let state = bare_state(root.path());
    let manifest = serde_json::json!({
        "sdkVersion":2,"id":"acme.composer","version":"1.0.0","name":"Composer","description":"",
        "dataSchemaVersion":1,"settingsSchema":{"type":"object"},
        "permissions":[{"kind":"composer","targets":[target]}],
        "contributions":{"views":[{"id":"page","title":"Page","entry":"ui/index.html","placements":["rightPane"]}]}
    });
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ("ui/index.html", b"<!doctype html><title>x</title>".to_vec()),
    ] {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    let package = PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap();
    {
        let mut manager = state.plugin_manager.lock().unwrap();
        manager
            .install(
                &package,
                PluginSource::Local {
                    name: "composer.zip".into(),
                },
            )
            .unwrap();
        manager.disable("acme.composer").unwrap();
    }
    let mut request =
        serde_json::json!({"expectedDigest":package.digest(),"entry":"ui/index.html"});
    if target == "text" {
        request["target"] = serde_json::json!(target);
    }
    let endpoint = "/plugins/acme.composer/composer/admit";
    assert_eq!(
        post_json(&state, endpoint, request.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    // 启用之前一律拒绝；启用后才可准入。
    state
        .plugin_manager
        .lock()
        .unwrap()
        .enable("acme.composer")
        .unwrap();
    assert_eq!(
        post_json(&state, endpoint, request.clone()).await.0,
        StatusCode::NO_CONTENT
    );
    let mut text_request = request.clone();
    text_request["target"] = serde_json::json!(if target == "text" { "images" } else { "text" });
    assert_eq!(
        post_json(&state, endpoint, text_request).await.0,
        StatusCode::FORBIDDEN,
        "图片与文本授权不能互相继承"
    );
    let mut stale = request.clone();
    stale["expectedDigest"] = serde_json::json!("0".repeat(64));
    assert_eq!(
        post_json(&state, endpoint, stale).await.0,
        StatusCode::CONFLICT
    );
    let mut wrong = request.clone();
    wrong["entry"] = serde_json::json!("ui/other.html");
    assert_eq!(
        post_json(&state, endpoint, wrong).await.0,
        StatusCode::FORBIDDEN
    );
    let response = routes()
        .with_state(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri(format!(
                    "/plugins/acme.composer/ui-v/{}/index.html",
                    package.digest()
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = routes()
        .with_state(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri(format!(
                    "/plugins/acme.composer/ui-v/{}/index.html",
                    "0".repeat(64)
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    state
        .plugin_manager
        .lock()
        .unwrap()
        .disable("acme.composer")
        .unwrap();
    assert_eq!(
        post_json(&state, endpoint, request).await.0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn resource_writes_publish_snapshot_and_subscription_updates() {
    let (state, root) = active_resource_state().await;
    let router = routes().with_state(state.clone());
    let stream = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/plugins/acme.resources/resources/dashboard/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body()
        .into_data_stream();

    let response = router
        .oneshot(
            axum::http::Request::builder()
                .method("PUT")
                .uri("/plugins/acme.resources/resources/dashboard")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"expectedVersion":0,"value":{"count":1}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body_bytes)
    );

    let mut stream = stream;
    let initial = timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&initial).contains("\"version\":0"));
    let body = timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&body).to_string();
    assert!(text.contains("\"version\":1"), "{text}");
    assert!(text.contains("\"count\":1"), "{text}");
    drop(root);
}
#[tokio::test]
async fn resource_subscription_closes_when_the_plugin_is_disabled() {
    let (state, root) = active_resource_state().await;
    let router = routes().with_state(state.clone());
    let mut stream = router
        .oneshot(
            axum::http::Request::builder()
                .uri("/plugins/acme.resources/resources/dashboard/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body()
        .into_data_stream();
    let initial = timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&initial).contains("\"version\":0"));

    state
        .plugin_manager
        .lock()
        .unwrap()
        .disable("acme.resources")
        .unwrap();
    let closed = timeout(Duration::from_secs(3), stream.next())
        .await
        .expect("撤销后订阅必须在复核间隔内收到关闭事件")
        .unwrap()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&closed).contains("\"closed\":true"),
        "{}",
        String::from_utf8_lossy(&closed)
    );
    assert!(
        timeout(Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .is_none(),
        "关闭事件之后流必须结束"
    );
    drop(root);
}

#[tokio::test]
async fn upgrade_route_switches_the_package_and_keeps_the_enabled_state() {
    let (state, root) = active_resource_state().await;
    let body = serde_json::json!({
        "source": "local:/tmp/acme-resources-1.1.0.zip",
        "archiveBase64": base64::engine::general_purpose::STANDARD.encode(archive("1.1.0")),
    });
    // 没有预览摘要的升级一律拒绝。
    let (status, _) = post_json(&state, "/plugins/upgrade", body.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let digest = PluginPackage::from_archive(&archive("1.1.0"))
        .unwrap()
        .digest()
        .to_string();
    let mut body = body;
    body["expectedDigest"] = digest.into();
    let response = routes()
        .with_state(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/plugins/upgrade")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let list: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let plugin = &list["plugins"][0];
    assert_eq!(plugin["manifest"]["version"], "1.1.0");
    assert_eq!(plugin["enabled"], true);
    assert_eq!(plugin["source"]["kind"], "local");
    // 旧页不能改变新版启停。
    let before = state.plugin_manager.lock().unwrap().state().clone();
    let old_digest = package().digest().to_string();
    for action in ["enable", "disable"] {
        let input = serde_json::json!({"expectedDigest": old_digest});
        let (status, error) =
            post_json(&state, &format!("/plugins/acme.resources/{action}"), input).await;
        assert_eq!(status, StatusCode::CONFLICT, "{action}: {error}");
        assert_eq!(state.plugin_manager.lock().unwrap().state(), &before);
    }
    let current = serde_json::json!({"expectedDigest": plugin["digest"]});
    assert_eq!(
        post_json(
            &state,
            "/plugins/acme.resources/disable",
            serde_json::json!({"expectedDigest":old_digest})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert!(
        state
            .plugin_manager
            .lock()
            .unwrap()
            .is_enabled("acme.resources")
            .unwrap()
    );
    assert_eq!(
        post_json(&state, "/plugins/acme.resources/disable", current)
            .await
            .0,
        StatusCode::OK
    );
    assert!(
        !state
            .plugin_manager
            .lock()
            .unwrap()
            .is_enabled("acme.resources")
            .unwrap()
    );
    drop(root);
}

#[tokio::test]
async fn exhausted_download_preserves_active_version_and_allows_explicit_upgrade_retry() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let (state, _root) = active_resource_state().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/package.zip", listener.local_addr().unwrap());
    let unavailable = Arc::new(AtomicBool::new(true));
    let requests = Arc::new(AtomicUsize::new(0));
    let (fault, count) = (unavailable.clone(), requests.clone());
    let app = axum::Router::new().route(
        "/package.zip",
        get(move || {
            let (fault, count) = (fault.clone(), count.clone());
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                if fault.load(Ordering::SeqCst) {
                    (StatusCode::SERVICE_UNAVAILABLE, Vec::new())
                } else {
                    (StatusCode::OK, archive("1.1.0"))
                }
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let before =
        serde_json::to_value(state.plugin_manager.lock().unwrap().projection().unwrap()).unwrap();
    let request = serde_json::json!({
        "source": format!("address:{url}"),
        "expectedDigest": PluginPackage::from_archive(&archive("1.1.0")).unwrap().digest(),
    });
    let (status, failure) = post_json(&state, "/plugins/upgrade", request.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(failure["message"].as_str().unwrap().contains("已尝试 3 次"));
    assert_eq!(requests.load(Ordering::SeqCst), 3);
    assert_eq!(
        serde_json::to_value(state.plugin_manager.lock().unwrap().projection().unwrap()).unwrap(),
        before
    );
    assert!(state.plugin_manager.lock().unwrap().state().plugins["acme.resources"].enabled);

    // 网络恢复后由用户显式重试，同一确认摘要进入原有升级提交点。
    unavailable.store(false, Ordering::SeqCst);
    let (status, upgraded) = post_json(&state, "/plugins/upgrade", request).await;
    assert_eq!(status, StatusCode::OK, "{upgraded}");
    assert_eq!(requests.load(Ordering::SeqCst), 4);
    assert_eq!(upgraded["plugins"][0]["manifest"]["version"], "1.1.0");
    assert_eq!(upgraded["plugins"][0]["enabled"], true);
    server.abort();
}

#[tokio::test]
async fn local_package_sources_must_be_absolute_paths() {
    let error = read_local_package("relative/plugin.zip").await.unwrap_err();
    assert!(matches!(error, ApiError::InvalidInput(_)));
}
/// 本地发布服务：同一个地址提供已签名目录和（可被替换内容的）发布包。
struct CenterFixture {
    base: String,
    package: Arc<std::sync::Mutex<Vec<u8>>>,
    _server: tokio::task::JoinHandle<()>,
}

async fn start_center(
    signing: &ring::signature::Ed25519KeyPair,
    key_id: &str,
    package_bytes: Vec<u8>,
    advertised_sha256: String,
) -> CenterFixture {
    start_center_with(
        signing,
        key_id,
        package_bytes,
        advertised_sha256,
        serde_json::json!({}),
    )
    .await
}

async fn start_center_with(
    signing: &ring::signature::Ed25519KeyPair,
    key_id: &str,
    package_bytes: Vec<u8>,
    advertised_sha256: String,
    extra: serde_json::Value,
) -> CenterFixture {
    use base64::Engine as _;
    let package = Arc::new(std::sync::Mutex::new(package_bytes.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut catalog = serde_json::json!({
        "format": 1, "issuedAt": 1,
        "plugins": [{
            "id": "acme.resources", "name": "Resources", "description": "",
            "version": "1.0.0", "url": format!("{base}/package.zip"),
            "sha256": advertised_sha256, "size": package_bytes.len()
        }]
    });
    for (key, value) in extra.as_object().unwrap() {
        catalog[key] = value.clone();
    }
    let catalog = serde_json::to_vec(&catalog).unwrap();
    let b64 = base64::engine::general_purpose::STANDARD;
    let envelope = serde_json::to_vec(&serde_json::json!({
        "payload": b64.encode(&catalog),
        "keyId": key_id,
        "signature": b64.encode(ring::signature::Ed25519KeyPair::sign(signing, &catalog).as_ref()),
    }))
    .unwrap();
    let served = Arc::clone(&package);
    let app = axum::Router::new()
        .route(
            "/catalog.json",
            axum::routing::get(move || {
                let envelope = envelope.clone();
                async move { envelope }
            }),
        )
        .route(
            "/package.zip",
            axum::routing::get(move || {
                let served = Arc::clone(&served);
                async move { served.lock().unwrap().clone() }
            }),
        );
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    CenterFixture {
        base,
        package,
        _server: server,
    }
}

fn center_signing_key() -> (ring::signature::Ed25519KeyPair, String) {
    use base64::Engine as _;
    use ring::signature::KeyPair;
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let public = base64::engine::general_purpose::STANDARD.encode(pair.public_key());
    (pair, public)
}

async fn call(
    state: &ApiState,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = axum::http::Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = routes()
        .with_state(state.clone())
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn signed_center_catalog_installs_only_the_exact_published_package() {
    let (signing, public) = center_signing_key();
    let archive = archive("1.0.0");
    let digest = package().digest().to_string();
    let center = start_center(&signing, "center-1", archive.clone(), digest.clone()).await;
    let root = tempfile::tempdir().unwrap();
    let state = bare_state(root.path());

    // 未配置插件中心：没有条目也没有错误。
    let (status, body) = call(&state, "GET", "/plugins/center", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entries"], serde_json::json!([]));

    let config = serde_json::json!({
        "url": format!("{}/catalog.json", center.base),
        "trustedKeys": [{"keyId": "center-1", "publicKey": public}],
    });
    let (status, _) = call(&state, "PUT", "/plugins/center/config", Some(config)).await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = call(&state, "GET", "/plugins/center", None).await;
    assert_eq!(body["error"], serde_json::Value::Null, "{body}");
    assert_eq!(body["entries"][0]["state"], "available");
    assert_eq!(body["entries"][0]["sha256"], digest);

    // 用户确认的是旧摘要：目录已变化时拒绝，不安装未经确认的候选。
    let (status, _) = call(
        &state,
        "POST",
        "/plugins/center/install",
        Some(serde_json::json!({"id": "acme.resources", "expectedSha256": "0".repeat(64)})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // 发布服务被入侵：包内容被替换，但目录签名仍绑定原摘要，安装必须失败且不落盘。
    *center.package.lock().unwrap() = self::archive("1.0.1");
    let (status, rejection) = call(
        &state,
        "POST",
        "/plugins/center/install",
        Some(serde_json::json!({"id": "acme.resources", "expectedSha256": digest})),
    )
    .await;
    // 替换内容要么超出目录声明的大小（下载阶段 400），要么与签名摘要不符（403）；都不得安装。
    assert!(
        matches!(status, StatusCode::FORBIDDEN | StatusCode::BAD_REQUEST),
        "{status} {rejection}"
    );
    assert!(
        state
            .plugin_manager
            .lock()
            .unwrap()
            .state()
            .plugins
            .is_empty()
    );

    *center.package.lock().unwrap() = archive;
    let (status, body) = call(
        &state,
        "POST",
        "/plugins/center/install",
        Some(serde_json::json!({"id": "acme.resources", "expectedSha256": digest})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugins"][0]["source"]["kind"], "center");
    let (_, body) = call(&state, "GET", "/plugins/center", None).await;
    assert_eq!(body["entries"][0]["state"], "installed");
}

#[tokio::test]
async fn plugin_list_merges_the_extension_runtime_status_without_storing_it() {
    use magi_app_server_protocol::generated::{PluginRuntimeState, PluginRuntimeStatus};
    struct Board;
    impl crate::ExtensionStatusSource for Board {
        fn status(&self, plugin_id: &str) -> Option<PluginRuntimeStatus> {
            (plugin_id == "acme.resources").then(|| PluginRuntimeStatus {
                state: PluginRuntimeState::Restarting,
                reason: Some("宿主进程退出".into()),
                generation: 0,
                instance_id: None,
                crashes: 2,
                blocking: vec!["tool.call".into()],
            })
        }
    }
    let (state, root) = active_resource_state().await;
    // 没有状态来源：不带 runtime 字段。
    let (_, body) = call(&state, "GET", "/plugins", None).await;
    assert!(body["plugins"][0].get("runtime").is_none(), "{body}");
    // 有状态来源：合并进每个插件的投影，来源是宿主监管而不是插件管理器。
    let state = state.with_extension_status(std::sync::Arc::new(Board));
    let (_, body) = call(&state, "GET", "/plugins", None).await;
    let runtime = &body["plugins"][0]["runtime"];
    assert_eq!(runtime["state"], "restarting");
    assert_eq!(runtime["reason"], "宿主进程退出");
    assert_eq!(runtime["crashes"], 2);
    assert_eq!(runtime["blocking"], serde_json::json!(["tool.call"]));
    // 变更类操作返回的列表同样带运行状态。
    let (_, body) = call(
        &state,
        "POST",
        "/plugins/acme.resources/disable",
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(
        body["plugins"][0]["runtime"]["state"], "restarting",
        "{body}"
    );
    drop(root);
}

#[tokio::test]
async fn expired_and_revoked_center_catalogs_offer_nothing_to_install() {
    let (signing, public) = center_signing_key();
    let digest = package().digest().to_string();
    let configure = |base: &str| {
        serde_json::json!({
            "url": format!("{base}/catalog.json"),
            "trustedKeys": [{"keyId": "center-1", "publicKey": public}],
        })
    };
    let body = |digest: &str| serde_json::json!({"id": "acme.resources", "expectedSha256": digest});

    // 已过期的签名目录：不展示条目，也不能安装。
    let expired = start_center_with(
        &signing,
        "center-1",
        archive("1.0.0"),
        digest.clone(),
        serde_json::json!({"sequence": 1, "expiresAt": 1}),
    )
    .await;
    let root = tempfile::tempdir().unwrap();
    let state = bare_state(root.path());
    call(
        &state,
        "PUT",
        "/plugins/center/config",
        Some(configure(&expired.base)),
    )
    .await;
    let (_, view) = call(&state, "GET", "/plugins/center", None).await;
    assert_eq!(view["entries"], serde_json::json!([]));
    assert!(view["error"].as_str().unwrap().contains("过期"), "{view}");
    let (status, _) = call(
        &state,
        "POST",
        "/plugins/center/install",
        Some(body(&digest)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 被撤销的版本：不再出现在目录里，按版本指定也不能安装。
    let revoked = start_center_with(
        &signing,
        "center-1",
        archive("1.0.0"),
        digest.clone(),
        serde_json::json!({"sequence": 2, "revoked": [{"id": "acme.resources", "reason": "漏洞"}]}),
    )
    .await;
    call(
        &state,
        "PUT",
        "/plugins/center/config",
        Some(configure(&revoked.base)),
    )
    .await;
    let (_, view) = call(&state, "GET", "/plugins/center", None).await;
    assert_eq!(view["entries"], serde_json::json!([]), "{view}");
    let mut pinned = body(&digest);
    pinned["version"] = "1.0.0".into();
    let (status, _) = call(&state, "POST", "/plugins/center/install", Some(pinned)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 序号回退：该地址已见过更高序号，序号 1 的目录视为回滚。
    let rolled_back = start_center_with(
        &signing,
        "center-1",
        archive("1.0.0"),
        digest.clone(),
        serde_json::json!({"sequence": 1}),
    )
    .await;
    state
        .settings_store
        .set_section(
            "pluginCenterState",
            serde_json::json!({ format!("{}/catalog.json", rolled_back.base): 2 }),
        )
        .unwrap();
    call(
        &state,
        "PUT",
        "/plugins/center/config",
        Some(configure(&rolled_back.base)),
    )
    .await;
    let (_, view) = call(&state, "GET", "/plugins/center", None).await;
    assert!(
        view["error"].as_str().unwrap_or("<none>").contains("回滚"),
        "{view}"
    );
}

#[tokio::test]
async fn center_catalog_signed_by_an_untrusted_key_exposes_nothing() {
    let (signing, _) = center_signing_key();
    let (_, other_public) = center_signing_key();
    let center = start_center(&signing, "center-1", archive("1.0.0"), "a".repeat(64)).await;
    let root = tempfile::tempdir().unwrap();
    let state = bare_state(root.path());
    let config = serde_json::json!({
        "url": format!("{}/catalog.json", center.base),
        "trustedKeys": [{"keyId": "center-1", "publicKey": other_public}],
    });
    call(&state, "PUT", "/plugins/center/config", Some(config)).await;
    let (_, body) = call(&state, "GET", "/plugins/center", None).await;
    assert_eq!(body["entries"], serde_json::json!([]));
    assert!(body["error"].as_str().unwrap().contains("签名"), "{body}");
    let (status, _) = call(
        &state,
        "POST",
        "/plugins/center/install",
        Some(serde_json::json!({"id": "acme.resources", "expectedSha256": "a".repeat(64)})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn center_config_rejects_malformed_keys_and_generic_center_sources() {
    let root = tempfile::tempdir().unwrap();
    let state = bare_state(root.path());
    let (status, _) = call(
        &state,
        "PUT",
        "/plugins/center/config",
        Some(serde_json::json!({
            "url": "https://plugins.example/catalog.json",
            "trustedKeys": [{"keyId": "k", "publicKey": "AAAA"}],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &state,
        "POST",
        "/plugins/install",
        Some(serde_json::json!({"source": "center:https://plugins.example/p.zip"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "中心来源不能自报地址");
}
