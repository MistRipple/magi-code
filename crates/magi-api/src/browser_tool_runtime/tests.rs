#[test]
fn only_a_navigating_action_gets_the_follow_up_snapshot_by_default() {
    use magi_browser_authority::BrowserActionVerdict;
    let target = |verdict| magi_browser_authority::BrowserActionTarget {
        role: None,
        name: None,
        effect: Some(magi_browser_authority::BrowserActionEffect {
            verdict,
            delivered: magi_browser_authority::BrowserActionDelivery::Confirmed,
            navigated: false,
            dom_mutations: None,
            target_connected: None,
            target_state: None,
        }),
    };
    assert!(snapshot_by_default(&action_outcome(Some(&target(
        BrowserActionVerdict::Navigated
    )))));
    assert!(!snapshot_by_default(&action_outcome(Some(&target(
        BrowserActionVerdict::Changed
    )))));
    assert!(!snapshot_by_default(&action_outcome(Some(&target(
        BrowserActionVerdict::NoVisibleChange
    )))));
    assert!(!snapshot_by_default(&action_outcome(None)));
}

#[test]
fn action_outcome_tells_the_agent_whether_the_action_took_effect() {
    use magi_browser_authority::{
        BrowserActionDelivery, BrowserActionEffect, BrowserActionTarget, BrowserActionVerdict,
    };
    let target = |verdict| BrowserActionTarget {
        role: Some("button".into()),
        name: Some("展开".into()),
        effect: Some(BrowserActionEffect {
            verdict,
            delivered: BrowserActionDelivery::Confirmed,
            navigated: matches!(verdict, BrowserActionVerdict::Navigated),
            dom_mutations: Some(0),
            target_connected: Some(true),
            target_state: None,
        }),
    };
    for (verdict, expected) in [
        (BrowserActionVerdict::Navigated, "page_navigated"),
        (BrowserActionVerdict::Changed, "page_changed"),
        (BrowserActionVerdict::NoVisibleChange, "no_visible_change"),
    ] {
        assert_eq!(action_outcome(Some(&target(verdict)))["result"], expected);
    }
    let none = action_outcome(None);
    assert_eq!(none["result"], "unobserved");
    assert!(
        action_outcome(Some(&target(BrowserActionVerdict::NoVisibleChange)))["instruction"]
            .as_str()
            .unwrap()
            .contains("不要重复")
    );
}

use super::*;
#[test]
fn browser_viewport_auto_returns_actual_metrics_without_fabricating_fixed_config() {
    let result = viewport_tool_result("browser_viewport", &BrowserTabId::new("tab"),
            BrowserHostCommandOutcome::Succeeded(Box::new(BrowserHostCommandResult::Json { value: json!({
                "viewport": {"mode": "auto"},
                "actual_viewport": {"width": 500, "height": 700, "device_scale_factor_millis": 2000, "device_type": "desktop"},
                "display_scale": 1.0,
            }) }))).unwrap();
    let result: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["mode"], "auto");
    assert_eq!(result["viewport"], Value::Null);
    assert_eq!(result["actual_viewport"]["width"], 500);
    assert_eq!(result["actual_viewport"]["device_type"], "desktop");
}

#[test]
fn browser_viewport_rejects_missing_actual_metrics() {
    let result = viewport_tool_result(
        "browser_viewport",
        &BrowserTabId::new("tab"),
        BrowserHostCommandOutcome::Succeeded(Box::new(BrowserHostCommandResult::Json {
            value: json!({
                "viewport": {"mode": "auto"}, "display_scale": 1.0,
            }),
        })),
    );
    assert_eq!(result.unwrap_err().code, "browser_result_invalid");
}

#[tokio::test]
async fn download_is_copied_without_overwriting_unless_asked() {
    let root = tempfile::tempdir().expect("tempdir");
    let source = root.path().join("private-download.bin");
    std::fs::write(&source, b"payload").expect("source");
    let target = root.path().join("workspace/out/report.bin");

    let copied = copy_download_into_place(&source, &target, false)
        .await
        .expect("first save");
    assert_eq!(copied, 7);
    assert_eq!(std::fs::read(&target).expect("saved file"), b"payload");

    let exists = copy_download_into_place(&source, &target, false)
        .await
        .expect_err("default must not overwrite");
    assert_eq!(exists.code, "browser_download_destination_exists");

    std::fs::write(&source, b"new").expect("update source");
    copy_download_into_place(&source, &target, true)
        .await
        .expect("overwrite=true replaces");
    assert_eq!(std::fs::read(&target).expect("saved file"), b"new");
    assert!(
        !target
            .with_file_name(".report.bin.magi-download.tmp")
            .exists(),
        "临时文件不能残留"
    );

    std::fs::remove_file(&source).expect("remove source");
    let missing = copy_download_into_place(&source, &root.path().join("x.bin"), false)
        .await
        .expect_err("missing source");
    assert_eq!(missing.code, "browser_download_file_missing");
}

#[test]
fn download_requested_access_is_read_for_list_and_write_for_save() {
    let list = serde_json::from_str::<Value>(r#"{"action":"list"}"#).expect("json");
    let save = serde_json::from_str::<Value>(r#"{"action":"save"}"#).expect("json");
    assert_eq!(
        browser_tool_requested_access(BrowserToolKind::Download, list.as_object().expect("obj")),
        BrowserToolAccess::Read
    );
    assert_eq!(
        browser_tool_requested_access(BrowserToolKind::Download, save.as_object().expect("obj")),
        BrowserToolAccess::Write
    );
}

#[test]
fn page_content_results_are_marked_untrusted() {
    for tool in [
        "browser_read",
        "browser_snapshot",
        "browser_console",
        "browser_network",
        "browser_evaluate",
        "browser_storage",
        "browser_tabs",
        "browser_webmcp",
    ] {
        let marked: Value = serde_json::from_str(&mark_untrusted_web_content(
            tool,
            json!({ "tool": tool, "status": "succeeded" }).to_string(),
        ))
        .expect("marked payload should stay JSON");
        assert_eq!(marked["content_trust"], "untrusted_web_content", "{tool}");
        assert_eq!(marked["content_notice"], UNTRUSTED_CONTENT_NOTICE, "{tool}");
        assert_eq!(marked["status"], "succeeded", "{tool}");
    }
}

#[test]
fn action_results_are_marked_only_when_they_carry_a_snapshot() {
    let plain =
        json!({ "tool": "browser_click", "status": "succeeded", "snapshot": null }).to_string();
    assert_eq!(
        mark_untrusted_web_content("browser_click", plain.clone()),
        plain
    );
    let with_snapshot =
        json!({ "tool": "browser_click", "status": "succeeded", "snapshot": { "nodes": [] } })
            .to_string();
    let marked: Value =
        serde_json::from_str(&mark_untrusted_web_content("browser_click", with_snapshot))
            .expect("marked payload should stay JSON");
    assert_eq!(marked["content_trust"], "untrusted_web_content");
    let screenshot =
        json!({ "tool": "browser_screenshot", "status": "succeeded", "path": "a.png" }).to_string();
    assert_eq!(
        mark_untrusted_web_content("browser_screenshot", screenshot.clone()),
        screenshot
    );
    assert_eq!(
        mark_untrusted_web_content("browser_read", "not json".to_string()),
        "not json"
    );
}

#[test]
fn surface_resync_only_when_the_host_is_ahead_and_attempts_remain() {
    use super::{SURFACE_SYNC_ATTEMPTS, should_resync_surface};
    // 新建 Tab 的初始加载：Host 已到 1，Authority 还是 0 -> 值得重新确认。
    assert!(should_resync_surface(Some(0), 1, 1));
    // Host 落后或相等（真正的 Surface 变更）、Tab 不存在：立即失败，不重试。
    assert!(!should_resync_surface(Some(3), 2, 1));
    assert!(!should_resync_surface(Some(2), 2, 1));
    assert!(!should_resync_surface(None, 5, 1));
    // 次数用尽后不再重试，避免无限等待。
    assert!(should_resync_surface(Some(0), 1, SURFACE_SYNC_ATTEMPTS - 1));
    assert!(!should_resync_surface(Some(0), 1, SURFACE_SYNC_ATTEMPTS));
}

use std::sync::{Arc, Mutex, RwLock, atomic::AtomicBool};

use magi_browser_authority::{
    BrowserAuthority, BrowserCapabilitySnapshot, BrowserHostPageState, BrowserHostRect,
    BrowserHostSnapshot, BrowserHostStatus, BrowserProfile, BrowserProfileKind,
    BrowserSessionLifecycle, BrowserSnapshotNode, CreateBrowserSession, CreateBrowserTab,
};
use magi_core::{
    BrowserProfileId, BrowserSessionId, BrowserTabId, ExecutionResultStatus, SessionId, ToolCallId,
    UtcMillis, WorkspaceId,
};
use magi_event_bus::InMemoryEventBus;
use magi_session_store::SessionStore;
use serde_json::{Map, Value, json};

use super::{
    BrowserToolRuntimeDependencies, DEFAULT_BROWSER_PROFILE_ID, browser_devtools_operation,
    browser_tool_requested_access, browser_tool_snapshot_value, normalize_screenshot_clip,
    optional_snapshot_target, parse_browser_navigation, parse_normalized_rect,
    screenshot_has_element_scope, tab_in_session, validate_browser_tabs_arguments,
    validate_devtools_arguments, validate_screenshot_binary, validate_screenshot_scope,
};
use crate::state::BrowserHostStatusSnapshot;
use magi_browser_authority::{BrowserToolAccess, BrowserToolKind};

#[test]
fn browser_tabs_rejects_nested_tab_fields_at_runtime_boundary() {
    let mut arguments = Map::new();
    arguments.insert("action".to_string(), Value::String("new".to_string()));
    arguments.insert(
        "parent_tab_id".to_string(),
        Value::String("tab-parent".to_string()),
    );

    let error = validate_browser_tabs_arguments(&arguments)
        .expect_err("browser_tabs must reject a nested tab identity");
    assert_eq!(error.code, "browser_nested_tab_unsupported");
}

#[test]
fn browser_navigation_is_rejected_before_tab_materialization() {
    let mut arguments = Map::new();
    arguments.insert("action".to_string(), Value::String("url".to_string()));
    arguments.insert(
        "url".to_string(),
        Value::String("file:///Users/xie/.magi/personal-sessions/session/".to_string()),
    );

    let error = parse_browser_navigation(&arguments, true)
        .expect_err("invalid navigation must fail before ensure_tab can create a tab");
    assert_eq!(error.code, "browser_navigation_url_rejected");
}

#[test]
fn browser_navigation_stop_has_no_url_or_timeout_side_effects() {
    let arguments = Map::from_iter([("action".to_string(), Value::String("stop".to_string()))]);
    assert!(matches!(
        parse_browser_navigation(&arguments, true),
        Ok(magi_browser_authority::BrowserNavigation::Stop)
    ));

    let invalid = Map::from_iter([
        ("action".to_string(), Value::String("stop".to_string())),
        ("timeout_ms".to_string(), Value::from(1000)),
    ]);
    let error = parse_browser_navigation(&invalid, true)
        .expect_err("stop must reject timeout because it is an immediate interrupt");
    assert_eq!(error.code, "invalid_navigation");
}

#[test]
fn ensuring_browser_session_does_not_implicitly_create_page() {
    let mut authority = BrowserAuthority::new();
    authority
        .register_profile(BrowserProfile {
            profile_id: BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID),
            kind: BrowserProfileKind::ManagedDefault,
            data_path: tempfile::tempdir()
                .expect("browser profile fixture should create")
                .keep(),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        })
        .expect("browser profile should register");
    let event_bus = Arc::new(InMemoryEventBus::new(16));
    let runtime = BrowserToolRuntimeDependencies {
        authority: Arc::new(Mutex::new(authority)),
        write_lock: Arc::new(Mutex::new(())),
        control_locks: Default::default(),
        downloads: Default::default(),
        state_writable: Arc::new(AtomicBool::new(true)),
        host_status: Arc::new(RwLock::new(BrowserHostStatusSnapshot::default())),
        host_client: Arc::new(RwLock::new(None)),
        event_bus: Arc::clone(&event_bus),
        session_store: Arc::new(SessionStore::new()),
        persistence: None,
    };
    let session_id = SessionId::new("session-browser-tabs-list");
    let workspace_id = WorkspaceId::new("workspace-browser-tabs-list");

    let created = runtime
        .ensure_session(&session_id, Some(&workspace_id))
        .expect("browser session should create");
    assert!(created.tab_ids.is_empty());
    let existing = runtime
        .ensure_session(&session_id, Some(&workspace_id))
        .expect("browser session should be idempotent");
    assert_eq!(existing.browser_session_id, created.browser_session_id);
    assert!(existing.tab_ids.is_empty());
    assert_eq!(
        event_bus
            .snapshot()
            .recent_events
            .iter()
            .filter(|event| event.event_type == "browser.session.created")
            .count(),
        1
    );

    let mismatched_workspace_id = WorkspaceId::new("workspace-browser-tabs-other");
    let error = runtime
        .ensure_session(&session_id, Some(&mismatched_workspace_id))
        .expect_err("an existing Browser Session must not be rebound to another workspace");
    assert_eq!(error.code, "browser_workspace_scope_mismatch");
    assert_eq!(
        event_bus
            .snapshot()
            .recent_events
            .iter()
            .filter(|event| event.event_type == "browser.session.created")
            .count(),
        1,
        "scope mismatch must not create a second Browser Session"
    );
}

#[test]
fn browser_tab_from_another_session_is_rejected_by_scope_guard() {
    let mut authority = BrowserAuthority::new();
    authority
        .register_profile(BrowserProfile {
            profile_id: BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID),
            kind: BrowserProfileKind::ManagedDefault,
            data_path: tempfile::tempdir()
                .expect("browser profile fixture should create")
                .keep(),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        })
        .expect("browser profile should register");
    let runtime = BrowserToolRuntimeDependencies {
        authority: Arc::new(Mutex::new(authority)),
        write_lock: Arc::new(Mutex::new(())),
        control_locks: Default::default(),
        downloads: Default::default(),
        state_writable: Arc::new(AtomicBool::new(true)),
        host_status: Arc::new(RwLock::new(BrowserHostStatusSnapshot::default())),
        host_client: Arc::new(RwLock::new(None)),
        event_bus: Arc::new(InMemoryEventBus::new(8)),
        session_store: Arc::new(SessionStore::new()),
        persistence: None,
    };
    let current_session_id = SessionId::new("session-browser-current-scope");
    let current_session = runtime
        .ensure_session(&current_session_id, None)
        .expect("current Browser Session should create");

    let foreign_session_id = SessionId::new("session-browser-foreign-scope");
    let foreign_browser_session_id = BrowserSessionId::new("browser-session-foreign-scope");
    let foreign_tab_id = BrowserTabId::new("browser-tab-foreign-scope");
    {
        let mut authority = runtime
            .authority
            .lock()
            .expect("browser authority lock should hold");
        authority
            .create_session(CreateBrowserSession {
                browser_session_id: foreign_browser_session_id.clone(),
                owner: magi_browser_authority::BrowserSessionOwner::Session {
                    session_id: foreign_session_id,
                    workspace_id: None,
                },
                profile_id: BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID),
                now: UtcMillis(2),
            })
            .expect("foreign Browser Session should create");
        authority
            .transition_session(
                &foreign_browser_session_id,
                BrowserSessionLifecycle::Ready,
                UtcMillis(3),
            )
            .expect("foreign Browser Session should become ready");
        authority
            .create_tab(CreateBrowserTab {
                tab_id: foreign_tab_id.clone(),
                browser_session_id: foreign_browser_session_id.clone(),
                url: "about:blank".to_string(),
                now: UtcMillis(4),
            })
            .expect("foreign Tab should create");
    }

    let error = tab_in_session(&runtime, &current_session, &foreign_tab_id)
        .expect_err("a Tab from another Magi Session must be rejected");
    assert_eq!(error.code, "browser_tab_scope_mismatch");
    assert!(
        runtime
            .authority
            .lock()
            .expect("browser authority lock should hold")
            .primary_surface(&foreign_tab_id)
            .is_none(),
        "scope rejection must happen before requesting a Desktop Surface"
    );
}

#[test]
fn unchanged_host_page_state_does_not_rewrite_authority_or_publish_duplicate_event() {
    let mut authority = BrowserAuthority::new();
    authority
        .register_profile(BrowserProfile {
            profile_id: BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID),
            kind: BrowserProfileKind::ManagedDefault,
            data_path: tempfile::tempdir()
                .expect("browser profile fixture should create")
                .keep(),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        })
        .expect("browser profile should register");
    let session_id = SessionId::new("session-browser-page-state");
    let workspace_id = WorkspaceId::new("workspace-browser-page-state");
    let browser_session_id = BrowserSessionId::new("browser-session-page-state");
    let tab_id = BrowserTabId::new("browser-tab-page-state");
    authority
        .create_session(CreateBrowserSession {
            browser_session_id: browser_session_id.clone(),
            owner: magi_browser_authority::BrowserSessionOwner::Session {
                session_id,
                workspace_id: Some(workspace_id),
            },
            profile_id: BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID),
            now: UtcMillis(2),
        })
        .expect("browser session should create");
    authority
        .transition_session(
            &browser_session_id,
            BrowserSessionLifecycle::Ready,
            UtcMillis(3),
        )
        .expect("browser session should become ready");
    authority
        .create_tab(CreateBrowserTab {
            tab_id: tab_id.clone(),
            browser_session_id,
            url: "about:blank".to_string(),
            now: UtcMillis(4),
        })
        .expect("browser tab should create");
    let event_bus = Arc::new(InMemoryEventBus::new(16));
    let runtime = BrowserToolRuntimeDependencies {
        authority: Arc::new(Mutex::new(authority)),
        write_lock: Arc::new(Mutex::new(())),
        control_locks: Default::default(),
        downloads: Default::default(),
        state_writable: Arc::new(AtomicBool::new(true)),
        host_status: Arc::new(RwLock::new(BrowserHostStatusSnapshot::default())),
        host_client: Arc::new(RwLock::new(None)),
        event_bus: Arc::clone(&event_bus),
        session_store: Arc::new(SessionStore::new()),
        persistence: None,
    };
    let page = BrowserHostPageState {
        tab_id: tab_id.clone(),
        url: "https://example.test/".to_string(),
        origin: Some("https://example.test".to_string()),
        title: "Example".to_string(),
        navigation_revision: 1,
    };

    runtime
        .apply_page_state(&tab_id, page.clone())
        .expect("first page state should apply");
    let authority_revision = runtime
        .authority
        .lock()
        .expect("browser authority lock poisoned")
        .snapshot()
        .revision;
    runtime
        .apply_page_state(&tab_id, page)
        .expect("identical page state should be idempotent");

    assert_eq!(
        runtime
            .authority
            .lock()
            .expect("browser authority lock poisoned")
            .snapshot()
            .revision,
        authority_revision
    );
    assert_eq!(
        event_bus
            .snapshot()
            .recent_events
            .iter()
            .filter(|event| event.event_type == "browser.tab.updated")
            .count(),
        1
    );
}

#[test]
fn browser_tools_return_structured_error_when_host_is_not_ready() {
    let runtime = BrowserToolRuntimeDependencies {
        authority: Arc::new(Mutex::new(BrowserAuthority::new())),
        write_lock: Arc::new(Mutex::new(())),
        control_locks: Default::default(),
        downloads: Default::default(),
        state_writable: Arc::new(AtomicBool::new(true)),
        host_status: Arc::new(RwLock::new(BrowserHostStatusSnapshot::default())),
        host_client: Arc::new(RwLock::new(None)),
        event_bus: Arc::new(InMemoryEventBus::new(8)),
        session_store: Arc::new(SessionStore::new()),
        persistence: None,
    };
    let context = magi_tool_runtime::ToolExecutionContext {
        session_id: Some(SessionId::new("session-browser-capability")),
        browser_capability_snapshot: Some(BrowserCapabilitySnapshot {
            revision: 1,
            in_app_browser_enabled: true,
            browser_use_enabled: true,
            devtools_enabled: true,
            lan_access_enabled: false,
            host_status: BrowserHostStatus::Ready,
            host_protocol_compatible: true,
        }),
        ..Default::default()
    };
    let (payload, status) = runtime.execute(
        &ToolCallId::new("call-browser-capability"),
        "browser_lighthouse",
        "{}",
        &context,
    );
    let payload: Value = serde_json::from_str(&payload).expect("capability payload JSON");
    assert_eq!(status, ExecutionResultStatus::Failed);
    assert_eq!(payload["error_code"], "capability_unavailable");
    assert_eq!(payload["details"]["capability"], "desktop_browser_surface");
    assert_eq!(payload["details"]["supported_platform"], "desktop");
    assert!(
        payload["details"]["suggestion"]
            .as_str()
            .is_some_and(|suggestion| suggestion.contains("桌面端")),
        "不可用时必须给出可执行的建议，而不是指向不存在的回退能力"
    );
    assert!(payload["details"].get("available_fallback").is_none());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_access_profile_matrix_reaches_real_host_protocol_for_read_and_write() {
    use futures_util::{SinkExt, StreamExt};
    use magi_browser_authority::{
        BrowserHostCommand, BrowserHostCommandOutcome, BrowserHostCommandResult, BrowserHostEvent,
        BrowserHostEventEnvelope, BrowserHostHandshake, BrowserHostPageState,
        BrowserHostProtocolVersion, BrowserHostRect, BrowserHostRequestEnvelope,
        BrowserHostResponseEnvelope, BrowserHostSnapshot, BrowserNavigation, BrowserSnapshotNode,
        BrowserSurfaceBinding,
    };
    use std::time::Duration;
    use tokio::net::UnixListener;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    let temp_dir = tempfile::tempdir().expect("browser host matrix temp dir should create");
    let socket_path = temp_dir.path().join("desktop-control.sock");
    let listener = UnixListener::bind(&socket_path).expect("browser host socket should bind");
    let commands = Arc::new(Mutex::new(Vec::<String>::new()));
    let navigation_urls = Arc::new(Mutex::new(Vec::<String>::new()));
    let commands_for_host = Arc::clone(&commands);
    let navigation_urls_for_host = Arc::clone(&navigation_urls);
    let host = tokio::spawn(async move {
        let (stream, _) = listener
            .accept()
            .await
            .expect("browser client should connect");
        let mut websocket = accept_async(stream)
            .await
            .expect("browser host websocket should accept");
        let ready = BrowserHostEventEnvelope {
            protocol_version: BrowserHostProtocolVersion::CURRENT,
            sequence: 1,
            event: BrowserHostEvent::Ready(BrowserHostHandshake {
                protocol_version: BrowserHostProtocolVersion::CURRENT,
                desktop_version: "browser-matrix-test".to_string(),
                electron_version: "electron-test".to_string(),
                chromium_version: "chromium-test".to_string(),
                process_id: 42,
                desktop_epoch: "desktop-epoch".to_string(),
                worker_epoch: "worker-epoch".to_string(),
            }),
        };
        websocket
            .send(Message::Text(
                serde_json::to_string(&ready)
                    .expect("ready event should serialize")
                    .into(),
            ))
            .await
            .expect("ready event should send");

        while let Some(message) = websocket.next().await {
            let message = message.expect("browser host request should read");
            let Message::Text(message) = message else {
                continue;
            };
            let request: BrowserHostRequestEnvelope =
                serde_json::from_str(message.as_str()).expect("browser request should decode");
            let command_name = match &request.command {
                BrowserHostCommand::CreatePage { .. } => "create_page",
                BrowserHostCommand::RestorePage { .. } => "restore_page",
                BrowserHostCommand::EnsureSurface { .. } => "ensure_surface",
                BrowserHostCommand::UpdateControl { .. } => "update_control",
                BrowserHostCommand::Navigate { .. } => "navigate",
                BrowserHostCommand::Snapshot { .. } => "snapshot",
                _ => "other",
            };
            commands_for_host
                .lock()
                .expect("browser command audit lock should hold")
                .push(command_name.to_string());

            let result = match request.command {
                BrowserHostCommand::CreatePage { tab_id, .. }
                | BrowserHostCommand::RestorePage { tab_id, .. } => {
                    BrowserHostCommandResult::PageState(BrowserHostPageState {
                        tab_id,
                        url: "about:blank".to_string(),
                        origin: None,
                        title: "Blank".to_string(),
                        navigation_revision: 0,
                    })
                }
                BrowserHostCommand::EnsureSurface { tab_id } => {
                    BrowserHostCommandResult::SurfaceBinding(BrowserSurfaceBinding {
                        desktop_epoch: "desktop-epoch".to_string(),
                        window_id: "window-browser-matrix".to_string(),
                        surface_id: format!("surface-{tab_id}"),
                        surface_revision: 1,
                        tab_id,
                        web_contents_id: 7,
                        target_id: "target-browser-matrix".to_string(),
                        browser_context_id: "context-browser-matrix".to_string(),
                        navigation_revision: 0,
                    })
                }
                BrowserHostCommand::Navigate {
                    tab_id,
                    navigation: BrowserNavigation::Url { url, .. },
                    ..
                } => {
                    navigation_urls_for_host
                        .lock()
                        .expect("browser navigation audit lock should hold")
                        .push(url.clone());
                    BrowserHostCommandResult::PageState(BrowserHostPageState {
                        tab_id,
                        url,
                        origin: Some("https://example.test".to_string()),
                        title: "Matrix page".to_string(),
                        navigation_revision: 1,
                    })
                }
                BrowserHostCommand::Snapshot {
                    tab_id,
                    navigation_revision,
                    snapshot_revision,
                    ..
                } => BrowserHostCommandResult::Snapshot(BrowserHostSnapshot {
                    tab_id,
                    navigation_revision,
                    snapshot_revision,
                    root: BrowserSnapshotNode {
                        element_ref: "root".to_string(),
                        role: Some("document".to_string()),
                        name: Some("Matrix page".to_string()),
                        value: None,
                        description: None,
                        disabled: false,
                        focused: false,
                        editable: false,
                        sensitive_input_kind: None,
                        states: Vec::new(),
                        visible: true,
                        bounds: Some(BrowserHostRect {
                            x: 0.0,
                            y: 0.0,
                            width: 100.0,
                            height: 100.0,
                        }),
                        children: Vec::new(),
                    },
                    returned_nodes: 1,
                    total_nodes: 1,
                    text_bytes: 11,
                    truncated: false,
                }),
                _ => BrowserHostCommandResult::Empty,
            };
            let response = BrowserHostResponseEnvelope {
                request_id: request.request_id,
                protocol_version: BrowserHostProtocolVersion::CURRENT,
                outcome: BrowserHostCommandOutcome::Succeeded(Box::new(result)),
            };
            websocket
                .send(Message::Text(
                    serde_json::to_string(&response)
                        .expect("browser response should serialize")
                        .into(),
                ))
                .await
                .expect("browser response should send");
        }
    });

    let connection = magi_browser_authority::BrowserHostClient::connect_desktop_socket(
        socket_path.to_str().expect("socket path should be UTF-8"),
        "test-token",
        "desktop-epoch",
        42,
        Duration::from_secs(2),
    )
    .await
    .expect("browser runtime should connect to host protocol");

    let mut authority = BrowserAuthority::new();
    authority
        .register_profile(BrowserProfile {
            profile_id: BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID),
            kind: BrowserProfileKind::ManagedDefault,
            data_path: tempfile::tempdir()
                .expect("browser profile data dir should create")
                .keep(),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        })
        .expect("browser profile should register");
    let host_status = Arc::new(RwLock::new(BrowserHostStatusSnapshot {
        revision: 1,
        settings: crate::state::BrowserCapabilitySettings {
            devtools_enabled: true,
            ..Default::default()
        },
        status: BrowserHostStatus::Ready,
        protocol_compatible: true,
        last_error_code: None,
    }));
    let host_client = Arc::new(RwLock::new(Some(connection.client.clone())));
    let runtime = BrowserToolRuntimeDependencies {
        authority: Arc::new(Mutex::new(authority)),
        write_lock: Arc::new(Mutex::new(())),
        control_locks: Default::default(),
        downloads: Default::default(),
        state_writable: Arc::new(AtomicBool::new(true)),
        host_status,
        host_client,
        event_bus: Arc::new(InMemoryEventBus::new(32)),
        session_store: Arc::new(SessionStore::new()),
        persistence: None,
    };

    for (index, access_profile) in [
        magi_core::AccessProfile::ReadOnly,
        magi_core::AccessProfile::Restricted,
        magi_core::AccessProfile::FullAccess,
    ]
    .into_iter()
    .enumerate()
    {
        let session_id = SessionId::new(format!("session-browser-host-matrix-{index}"));
        let context = magi_tool_runtime::ToolExecutionContext {
            session_id: Some(session_id.clone()),
            workspace_id: Some(WorkspaceId::new(format!("workspace-browser-host-{index}"))),
            access_profile,
            browser_execution_id: Some(format!("turn-browser-host-matrix-{index}")),
            browser_capability_snapshot: Some(BrowserCapabilitySnapshot {
                revision: 1,
                in_app_browser_enabled: true,
                browser_use_enabled: true,
                devtools_enabled: true,
                lan_access_enabled: false,
                host_status: BrowserHostStatus::Ready,
                host_protocol_compatible: true,
            }),
            ..Default::default()
        };
        let (snapshot, snapshot_status) = runtime.execute(
            &ToolCallId::new(format!("browser-host-snapshot-{index}")),
            "browser_snapshot",
            "{}",
            &context,
        );
        assert_eq!(snapshot_status, ExecutionResultStatus::Succeeded);
        assert_eq!(
            serde_json::from_str::<Value>(&snapshot).expect("snapshot result should be JSON")["status"],
            "succeeded"
        );

        let (navigation, navigation_status) = runtime.execute(
            &ToolCallId::new(format!("browser-host-navigate-{index}")),
            "browser_navigate",
            r#"{"url":"https://example.test/matrix"}"#,
            &context,
        );
        assert_eq!(navigation_status, ExecutionResultStatus::Succeeded);
        assert_eq!(
            serde_json::from_str::<Value>(&navigation).expect("navigation result should be JSON")["status"],
            "succeeded"
        );
    }

    let commands = commands
        .lock()
        .expect("browser command audit lock should hold")
        .clone();
    let navigation_urls = navigation_urls
        .lock()
        .expect("browser navigation audit lock should hold")
        .clone();
    assert_eq!(navigation_urls.len(), 3);
    assert_eq!(
        commands
            .iter()
            .filter(|command| command == &"snapshot")
            .count(),
        3
    );
    assert_eq!(
        commands
            .iter()
            .filter(|command| command == &"navigate")
            .count(),
        3
    );
    assert!(
        commands
            .iter()
            .filter(|command| command == &"ensure_surface")
            .count()
            >= 3
    );

    connection.client.close().await;
    host.await.expect("browser host matrix server should stop");
}

#[test]
fn browser_devtools_contract_uses_source_target_fields_and_expression() {
    let mut arguments = Map::new();
    arguments.insert("source".to_string(), json!({"element_ref": "e-1"}));
    arguments.insert("target".to_string(), json!({"element_ref": "e-2"}));
    validate_devtools_arguments("drag", &arguments)
        .expect("drag should use source and target snapshot references");
    assert!(validate_devtools_arguments("drag", &Map::new()).is_err());

    let mut fill_arguments = Map::new();
    fill_arguments.insert(
        "fields".to_string(),
        json!([{"element_ref": "e-1", "value": "Magi"}]),
    );
    validate_devtools_arguments("fill_form", &fill_arguments).expect("fill_form should use fields");

    let mut invalid_fill = Map::new();
    invalid_fill.insert(
        "fields".to_string(),
        json!([{"element_ref": "e-1", "value": {"unexpected": true}}]),
    );
    assert!(validate_devtools_arguments("fill_form", &invalid_fill).is_err());

    let mut invalid_pwa = Map::new();
    invalid_pwa.insert("action".to_string(), json!("install"));
    assert!(validate_devtools_arguments("pwa", &invalid_pwa).is_err());

    let mut invalid_heap = Map::new();
    invalid_heap.insert("action".to_string(), json!("query_objects"));
    assert!(validate_devtools_arguments("heap", &invalid_heap).is_err());

    let mut invalid_third_party = Map::new();
    invalid_third_party.insert("action".to_string(), json!("execute"));
    assert!(validate_devtools_arguments("third_party", &invalid_third_party).is_err());

    assert!(validate_devtools_arguments("third_party", &Map::new()).is_err());

    let mut third_party_list = Map::new();
    third_party_list.insert("action".to_string(), json!("list"));
    validate_devtools_arguments("third_party", &third_party_list)
        .expect("third_party list should be supported");
    assert_eq!(
        browser_tool_requested_access(BrowserToolKind::ThirdParty, &third_party_list),
        BrowserToolAccess::Read
    );
    let mut third_party_clear = Map::new();
    third_party_clear.insert("action".to_string(), json!("clear"));
    validate_devtools_arguments("third_party", &third_party_clear)
        .expect("third_party clear should be supported");
    assert_eq!(
        browser_tool_requested_access(BrowserToolKind::ThirdParty, &third_party_clear),
        BrowserToolAccess::Write
    );
    let mut pwa_state = Map::new();
    pwa_state.insert("action".to_string(), json!("state"));
    assert_eq!(
        browser_tool_requested_access(BrowserToolKind::Pwa, &pwa_state),
        BrowserToolAccess::Read
    );

    // browser_read 是只读工具：整页读取、分页、检索和元素范围读取都不需要写租约。
    assert_eq!(browser_devtools_operation("browser_read"), Some("read"));
    let mut read_page = Map::new();
    read_page.insert("max_chars".to_string(), json!(3000));
    read_page.insert("offset".to_string(), json!(0));
    read_page.insert("query".to_string(), json!("价格"));
    read_page.insert("include_links".to_string(), json!(true));
    validate_devtools_arguments("read", &read_page).expect("read page should be valid");
    validate_devtools_arguments("read", &Map::new()).expect("read defaults to whole page");
    assert_eq!(
        browser_tool_requested_access(BrowserToolKind::Read, &read_page),
        BrowserToolAccess::Read
    );
    let mut read_scoped = Map::new();
    read_scoped.insert("element_ref".to_string(), json!("e:2:5"));
    validate_devtools_arguments("read", &read_scoped).expect("scoped read should be valid");
    for (name, key, value) in [
        ("element_ref 为合成根节点", "element_ref", json!("root")),
        ("max_chars 为 0", "max_chars", json!(0)),
        ("max_chars 超上限", "max_chars", json!(50_001)),
        ("offset 为负数", "offset", json!(-1)),
        ("query 为空白", "query", json!("   ")),
        ("include_links 不是布尔", "include_links", json!("yes")),
    ] {
        let mut invalid = Map::new();
        invalid.insert(key.to_string(), value);
        assert!(
            validate_devtools_arguments("read", &invalid).is_err(),
            "{name} 应被拒绝"
        );
    }

    // browser_storage：存储可读写；cookie 只能列出元数据与清理，读值和写入都被拒绝。
    assert_eq!(
        browser_devtools_operation("browser_storage"),
        Some("storage")
    );
    let storage_args = |pairs: &[(&str, Value)]| {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect::<Map<String, Value>>()
    };
    for valid in [
        storage_args(&[("area", json!("local")), ("action", json!("list"))]),
        storage_args(&[
            ("area", json!("session")),
            ("action", json!("get")),
            ("key", json!("flag")),
        ]),
        storage_args(&[
            ("area", json!("local")),
            ("action", json!("set")),
            ("key", json!("flag")),
            ("value", json!("on")),
        ]),
        storage_args(&[("area", json!("cookies")), ("action", json!("list"))]),
        storage_args(&[("area", json!("cookies")), ("action", json!("clear"))]),
    ] {
        validate_devtools_arguments("storage", &valid).expect("valid storage call");
    }
    for (name, invalid) in [
        (
            "cookie 不能读值",
            storage_args(&[
                ("area", json!("cookies")),
                ("action", json!("get")),
                ("key", json!("sid")),
            ]),
        ),
        (
            "cookie 不能写入",
            storage_args(&[
                ("area", json!("cookies")),
                ("action", json!("set")),
                ("key", json!("sid")),
                ("value", json!("x")),
            ]),
        ),
        (
            "get 缺少 key",
            storage_args(&[("area", json!("local")), ("action", json!("get"))]),
        ),
        (
            "set 缺少 value",
            storage_args(&[
                ("area", json!("local")),
                ("action", json!("set")),
                ("key", json!("k")),
            ]),
        ),
        (
            "未知 area",
            storage_args(&[("area", json!("indexeddb")), ("action", json!("list"))]),
        ),
    ] {
        assert!(
            validate_devtools_arguments("storage", &invalid).is_err(),
            "{name} 应被拒绝"
        );
    }
    assert_eq!(
        browser_tool_requested_access(
            BrowserToolKind::Storage,
            &storage_args(&[("area", json!("cookies")), ("action", json!("list"))])
        ),
        BrowserToolAccess::Read
    );
    assert_eq!(
        browser_tool_requested_access(
            BrowserToolKind::Storage,
            &storage_args(&[("area", json!("cookies")), ("action", json!("clear"))])
        ),
        BrowserToolAccess::Write
    );

    let mut evaluate_arguments = Map::new();
    evaluate_arguments.insert("expression".to_string(), json!("document.title"));
    validate_devtools_arguments("evaluate", &evaluate_arguments)
        .expect("evaluate should use expression");
    assert!(validate_devtools_arguments("evaluate", &Map::new()).is_err());
}

#[test]
fn browser_screenshot_root_is_page_scope_and_normalized_clip_is_validated() {
    let root = json!({ "element_ref": "root" });
    assert_eq!(
        optional_snapshot_target(root.as_object().expect("root target object"))
            .expect("root target should parse"),
        None
    );

    let element = json!({ "element_ref": "e:4:1" });
    assert_eq!(
        optional_snapshot_target(element.as_object().expect("element target object"))
            .expect("element target should parse")
            .expect("non-root target should remain addressable")
            .element_ref,
        "e:4:1"
    );

    let clip = parse_normalized_rect(&json!({
        "x": 0.25,
        "y": 0.1,
        "width": 0.5,
        "height": 0.25
    }))
    .expect("normalized clip should be accepted");
    assert_eq!(clip.x, 0.25);
    assert_eq!(clip.width, 0.5);
    assert!(
        parse_normalized_rect(&json!({
            "x": 0.8,
            "y": 0.1,
            "width": 0.5,
            "height": 0.25
        }))
        .is_err()
    );
}

#[test]
fn browser_screenshot_full_viewport_clip_is_sent_as_page_scope() {
    let clip = parse_normalized_rect(&json!({
        "x": 0.0,
        "y": 0.0,
        "width": 1.0,
        "height": 1.0
    }))
    .expect("full viewport clip should be valid");

    assert_eq!(normalize_screenshot_clip(clip), None);
}

#[test]
fn browser_screenshot_scope_is_explicitly_mutually_exclusive() {
    let mut root_arguments = Map::new();
    root_arguments.insert("element_ref".to_string(), json!("root"));
    assert!(!screenshot_has_element_scope(&root_arguments));

    let mut element_arguments = Map::new();
    element_arguments.insert("element_ref".to_string(), json!("e:4:1"));
    assert!(screenshot_has_element_scope(&element_arguments));

    assert!(validate_screenshot_scope(true, false, false).is_ok());
    assert!(validate_screenshot_scope(false, true, false).is_ok());
    assert!(validate_screenshot_scope(false, false, true).is_ok());
    for (element, clip, full_page) in [(true, true, false), (true, false, true)] {
        let error = validate_screenshot_scope(element, clip, full_page)
            .expect_err("screenshot scopes must not be combined");
        assert_eq!(error.code, "invalid_screenshot_scope");
    }
}

#[test]
fn browser_screenshot_metadata_and_file_header_must_match() {
    use sha2::{Digest, Sha256};

    let bytes = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    let metadata = magi_browser_authority::BrowserHostBinaryPayload {
        payload_id: "payload-1".to_string(),
        mime_type: "image/png".to_string(),
        byte_length: bytes.len() as u64,
        sha256: magi_core::hex_lower(Sha256::digest(&bytes)),
    };
    validate_screenshot_binary(
        &magi_browser_authority::BrowserScreenshotFormat::Png,
        &metadata,
        &bytes,
    )
    .expect("valid PNG metadata should pass");

    let mut invalid = metadata;
    invalid.mime_type = "image/webp".to_string();
    let error = validate_screenshot_binary(
        &magi_browser_authority::BrowserScreenshotFormat::Png,
        &invalid,
        &bytes,
    )
    .expect_err("MIME mismatch must fail");
    assert_eq!(error.code, "browser_screenshot_format_mismatch");
}

#[test]
fn model_snapshot_preserves_collector_budget_and_truncation() {
    let mut nodes = (1..=200)
        .map(|index| {
            snapshot_node(
                &format!("e-2-{index}"),
                None,
                Some("静态内容"),
                false,
                false,
            )
        })
        .collect::<Vec<_>>();
    let searchbox = snapshot_node("e-2-2", Some("searchbox"), Some("Search"), true, true);
    nodes.push(searchbox);
    let snapshot = BrowserHostSnapshot {
        tab_id: BrowserTabId::new("browser-tab-compact-snapshot"),
        navigation_revision: 1,
        snapshot_revision: 2,
        root: BrowserSnapshotNode {
            element_ref: "root".to_string(),
            role: Some("document".to_string()),
            name: Some("示例页面".to_string()),
            value: None,
            description: None,
            disabled: false,
            focused: false,
            editable: false,
            sensitive_input_kind: None,
            states: Vec::new(),
            visible: true,
            bounds: None,
            children: nodes,
        },
        returned_nodes: 201,
        total_nodes: 201,
        text_bytes: 20,
        truncated: true,
    };

    let logical_tab_id = BrowserTabId::new("browser-logical-tab");
    let value = browser_tool_snapshot_value(&snapshot, &logical_tab_id);
    let elements = value["elements"]
        .as_array()
        .expect("compact snapshot elements should be an array");

    assert_eq!(elements.len(), 201);
    assert_eq!(value["tab_id"], "browser-logical-tab");
    assert_eq!(elements[200]["role"], "searchbox");
    assert_eq!(elements[200]["editable"], true);
    assert!(elements[0].get("bounds").is_none());
    assert!(elements[0].get("disabled").is_none());
    assert!(elements[0].get("states").is_none());
    assert!(value.get("accessibility_tree").is_none());
    assert_eq!(value["truncated"], true);
    assert_eq!(value["returned_nodes"], 201);
    assert_eq!(value["total_nodes"], 201);
}

#[test]
fn browser_execution_id_is_stable_and_does_not_depend_on_tool_call_id() {
    let event_bus = Arc::new(InMemoryEventBus::new(8));
    let runtime = BrowserToolRuntimeDependencies {
        authority: Arc::new(Mutex::new(BrowserAuthority::new())),
        write_lock: Arc::new(Mutex::new(())),
        control_locks: Default::default(),
        downloads: Default::default(),
        state_writable: Arc::new(AtomicBool::new(true)),
        host_status: Arc::new(RwLock::new(BrowserHostStatusSnapshot::default())),
        host_client: Arc::new(RwLock::new(None)),
        event_bus,
        session_store: Arc::new(SessionStore::new()),
        persistence: None,
    };
    let session_id = SessionId::new("session-browser-execution-id");

    let explicit = magi_tool_runtime::ToolExecutionContext {
        browser_execution_id: Some("turn-stable".to_string()),
        ..Default::default()
    };
    assert_eq!(
        runtime.browser_execution_id(&explicit, &session_id),
        "turn-stable"
    );

    let task_scoped = magi_tool_runtime::ToolExecutionContext {
        task_id: Some(magi_core::TaskId::new("task-stable")),
        ..Default::default()
    };
    assert_eq!(
        runtime.browser_execution_id(&task_scoped, &session_id),
        "task:task-stable"
    );

    let session_scoped = magi_tool_runtime::ToolExecutionContext::default();
    assert_eq!(
        runtime.browser_execution_id(&session_scoped, &session_id),
        "session:session-browser-execution-id"
    );
}

fn snapshot_node(
    element_ref: &str,
    role: Option<&str>,
    name: Option<&str>,
    editable: bool,
    focused: bool,
) -> BrowserSnapshotNode {
    BrowserSnapshotNode {
        element_ref: element_ref.to_string(),
        role: role.map(str::to_string),
        name: name.map(str::to_string),
        value: None,
        description: None,
        disabled: false,
        focused,
        editable,
        sensitive_input_kind: None,
        states: Vec::new(),
        visible: true,
        bounds: Some(BrowserHostRect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 20.0,
        }),
        children: Vec::new(),
    }
}

#[test]
fn snapshot_elements_expose_node_states_for_the_model() {
    use magi_browser_authority::BrowserNodeState;
    let mut checkbox = snapshot_node("e-1-1", Some("checkbox"), Some("同意条款"), false, false);
    checkbox.states = vec![BrowserNodeState::Unchecked, BrowserNodeState::Required];
    let plain = snapshot_node("e-1-2", Some("button"), Some("提交"), false, false);
    let snapshot = BrowserHostSnapshot {
        tab_id: BrowserTabId::new("browser-tab-states"),
        navigation_revision: 1,
        snapshot_revision: 1,
        root: BrowserSnapshotNode {
            element_ref: "root".to_string(),
            role: Some("document".to_string()),
            name: Some("表单".to_string()),
            value: None,
            description: None,
            disabled: false,
            focused: false,
            editable: false,
            sensitive_input_kind: None,
            states: Vec::new(),
            visible: true,
            bounds: None,
            children: vec![checkbox, plain],
        },
        returned_nodes: 2,
        total_nodes: 2,
        text_bytes: 10,
        truncated: false,
    };
    let value = browser_tool_snapshot_value(&snapshot, &BrowserTabId::new("browser-logical-tab"));
    let elements = value["elements"]
        .as_array()
        .expect("elements should be an array");
    let by_ref = |element_ref: &str| {
        elements
            .iter()
            .find(|element| element["element_ref"] == element_ref)
            .expect("element should be present")
    };
    // 未勾选和必填是模型判断“现在能不能提交”的依据，必须出现在元素上。
    assert_eq!(by_ref("e-1-1")["states"], json!(["unchecked", "required"]));
    assert!(by_ref("e-1-2").get("states").is_none());
}
