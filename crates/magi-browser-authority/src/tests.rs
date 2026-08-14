use std::path::PathBuf;

use magi_core::{
    BrowserLeaseId, BrowserProfileId, BrowserSessionId, BrowserTabId, ExecutionOwnership,
    SessionId, UtcMillis, WorkspaceId,
};

use crate::{
    AcquireBrowserLease, BrowserAnnotation, BrowserAnnotationAnchor, BrowserAnnotationAuthor,
    BrowserAnnotationKind, BrowserAuthority, BrowserDeviceType, BrowserDurableState,
    BrowserLeaseEndReason, BrowserLeaseLifecycle, BrowserProfile, BrowserProfileKind,
    BrowserSessionLifecycle, BrowserTabLifecycle, BrowserViewport, CreateBrowserSession,
    CreateBrowserTab, GoalControlBinding, ValidateBrowserWrite,
};

fn at(value: u64) -> UtcMillis {
    UtcMillis(value)
}

fn profile_id() -> BrowserProfileId {
    BrowserProfileId::new("browser-profile-default")
}

fn register_profile(authority: &mut BrowserAuthority) {
    authority
        .register_profile(BrowserProfile {
            profile_id: profile_id(),
            kind: BrowserProfileKind::ManagedDefault,
            data_path: PathBuf::from("/tmp/magi-browser-profile"),
            created_at: at(1),
            updated_at: at(1),
        })
        .expect("profile should register");
}

fn ready_session(authority: &mut BrowserAuthority) -> BrowserSessionId {
    let browser_session_id = BrowserSessionId::new("browser-session-1");
    authority
        .create_session(CreateBrowserSession {
            browser_session_id: browser_session_id.clone(),
            workspace_id: Some(WorkspaceId::new("workspace-1")),
            session_id: SessionId::new("session-1"),
            profile_id: profile_id(),
            now: at(2),
        })
        .expect("session should create");
    authority
        .transition_session(&browser_session_id, BrowserSessionLifecycle::Ready, at(3))
        .expect("session should become ready");
    browser_session_id
}

fn ready_tab(
    authority: &mut BrowserAuthority,
    browser_session_id: &BrowserSessionId,
) -> BrowserTabId {
    let tab_id = BrowserTabId::new("browser-tab-1");
    authority
        .create_tab(CreateBrowserTab {
            tab_id: tab_id.clone(),
            browser_session_id: browser_session_id.clone(),
            url: "about:blank".to_string(),
            now: at(4),
        })
        .expect("tab should create");
    authority
        .transition_tab(&tab_id, BrowserTabLifecycle::Ready, at(5))
        .expect("tab should become ready");
    tab_id
}

fn surface_id() -> String {
    "surface-1".to_string()
}

fn owner() -> ExecutionOwnership {
    ExecutionOwnership {
        session_id: Some(SessionId::new("session-1")),
        workspace_id: Some(WorkspaceId::new("workspace-1")),
        ..ExecutionOwnership::default()
    }
}

#[test]
fn durable_tab_contains_only_user_visible_page_state() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    authority
        .set_primary_surface(&tab_id, surface_id(), 1, at(6))
        .expect("surface should bind");

    let durable = authority.durable_state();
    assert_eq!(durable.tabs.len(), 1);
    assert_eq!(durable.tabs[0].canonical_url, "about:blank");
    assert!(
        !serde_json::to_value(&durable.tabs[0])
            .expect("durable tab should serialize")
            .as_object()
            .expect("durable tab should be an object")
            .contains_key("viewport")
    );
    assert!(authority.primary_surface(&tab_id).is_some());

    let restored = BrowserAuthority::restore_durable(durable, at(7)).expect("state should restore");
    assert!(restored.primary_surface(&tab_id).is_none());
    assert!(
        restored
            .active_lease_for_surface(&tab_id, &surface_id())
            .is_none()
    );
}

#[test]
fn previous_durable_schema_without_tab_order_is_migrated() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);

    let mut value =
        serde_json::to_value(authority.durable_state()).expect("state should serialize");
    value["schema_version"] = serde_json::json!(4);
    value["tabs"][0]
        .as_object_mut()
        .expect("tab should be an object")
        .remove("order");
    let legacy: BrowserDurableState =
        serde_json::from_value(value).expect("legacy state should decode");

    let restored =
        BrowserAuthority::restore_durable(legacy, at(7)).expect("legacy state should restore");
    let tab = restored
        .snapshot()
        .tabs
        .into_iter()
        .find(|tab| tab.tab_id == tab_id)
        .expect("tab should survive migration");
    assert_eq!(tab.order, 0);
}

#[test]
fn lease_is_scoped_to_one_tab_and_surface() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    authority
        .set_primary_surface(&tab_id, surface_id(), 1, at(6))
        .expect("surface should bind");
    let goal_binding = GoalControlBinding {
        goal_id: magi_core::GoalId::new("goal-1"),
        control_revision: 1,
    };
    let lease = authority
        .acquire_lease(AcquireBrowserLease {
            lease_id: BrowserLeaseId::new("lease-1"),
            tab_id: tab_id.clone(),
            surface_id: surface_id(),
            owner: owner(),
            turn_id: "turn-1".to_string(),
            goal_binding: Some(goal_binding.clone()),
            acquired_at: at(8),
            expires_at: at(100),
        })
        .expect("lease should acquire");
    let surface = surface_id();
    let lease_owner = owner();
    let validated = authority
        .validate_write(ValidateBrowserWrite {
            lease_id: &lease.lease_id,
            fence: lease.fence,
            tab_id: &tab_id,
            surface_id: &surface,
            owner: &lease_owner,
            turn_id: "turn-1",
            goal_binding: Some(&goal_binding),
            now: at(9),
        })
        .expect("lease should validate");
    assert_eq!(validated.fence, lease.fence);
    assert_eq!(
        authority
            .active_lease_for_surface(&tab_id, &surface_id())
            .map(|value| value.lease_id.clone()),
        Some(lease.lease_id.clone())
    );

    let (control, revoked) = authority
        .take_user_control(&tab_id, &surface_id(), at(10))
        .expect("user takeover should succeed");
    assert!(control.lease_id.is_none());
    assert_eq!(revoked.len(), 1);
    assert_eq!(
        revoked[0].end_reason,
        Some(BrowserLeaseEndReason::UserTakeover)
    );
    assert_eq!(revoked[0].lifecycle, BrowserLeaseLifecycle::Revoked);
}

#[test]
fn surface_replacement_revokes_only_that_surface() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    authority
        .set_primary_surface(&tab_id, surface_id(), 1, at(6))
        .expect("surface should bind");
    authority
        .acquire_lease(AcquireBrowserLease {
            lease_id: BrowserLeaseId::new("lease-1"),
            tab_id: tab_id.clone(),
            surface_id: surface_id(),
            owner: owner(),
            turn_id: "turn-1".to_string(),
            goal_binding: None,
            acquired_at: at(8),
            expires_at: at(100),
        })
        .expect("lease should acquire");
    let revoked = authority
        .set_primary_surface(&tab_id, "surface-2".to_string(), 2, at(9))
        .expect("surface replacement should succeed");
    assert_eq!(revoked.len(), 1);
    assert!(
        authority
            .active_lease_for_surface(&tab_id, &surface_id())
            .is_none()
    );
    assert_eq!(
        authority
            .primary_surface(&tab_id)
            .map(|surface| surface.surface_id.as_str()),
        Some("surface-2")
    );
}

#[test]
fn crashed_tab_releases_surface_control_without_deleting_the_logical_tab() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    authority
        .set_primary_surface(&tab_id, surface_id(), 1, at(6))
        .expect("surface should bind");
    let lease_id = BrowserLeaseId::new("lease-crash");
    authority
        .acquire_lease(AcquireBrowserLease {
            lease_id: lease_id.clone(),
            tab_id: tab_id.clone(),
            surface_id: surface_id(),
            owner: owner(),
            turn_id: "turn-crash".to_string(),
            goal_binding: None,
            acquired_at: at(8),
            expires_at: at(100),
        })
        .expect("lease should acquire");

    let crashed = authority
        .transition_tab(&tab_id, BrowserTabLifecycle::Crashed, at(9))
        .expect("tab should enter crashed state");

    assert_eq!(crashed.lifecycle, BrowserTabLifecycle::Crashed);
    assert!(authority.primary_surface(&tab_id).is_none());
    assert!(
        authority
            .active_lease_for_surface(&tab_id, &surface_id())
            .is_none()
    );
    assert_eq!(
        authority.lease(&lease_id).map(|lease| lease.end_reason),
        Some(Some(BrowserLeaseEndReason::RuntimeUnavailable))
    );
    assert!(
        authority
            .session(&browser_session_id)
            .expect("session should remain")
            .tab_ids
            .contains(&tab_id)
    );
}

#[test]
fn browser_tab_document_updates_are_revisioned_without_viewport_state() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    let tab = authority
        .apply_host_page_state(
            &tab_id,
            1,
            "https://example.com/".to_string(),
            Some("https://example.com".to_string()),
            "Example".to_string(),
            at(6),
        )
        .expect("page state should apply");
    assert_eq!(tab.navigation_revision, 1);
    assert_eq!(tab.snapshot_revision, 1);
    assert_eq!(tab.url, "https://example.com/");
    assert_eq!(tab.title, "Example");
    assert_eq!(
        BrowserViewport::default().device_type,
        BrowserDeviceType::Desktop
    );
}

#[test]
fn closing_tab_revokes_surface_lease_and_removes_membership() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    authority
        .set_primary_surface(&tab_id, surface_id(), 1, at(6))
        .expect("surface should bind");
    let lease_id = BrowserLeaseId::new("lease-close");
    authority
        .acquire_lease(AcquireBrowserLease {
            lease_id: lease_id.clone(),
            tab_id: tab_id.clone(),
            surface_id: surface_id(),
            owner: owner(),
            turn_id: "turn-close".to_string(),
            goal_binding: None,
            acquired_at: at(8),
            expires_at: at(100),
        })
        .expect("lease should acquire");
    authority
        .transition_tab(&tab_id, BrowserTabLifecycle::Closed, at(9))
        .expect("tab should close");
    assert!(
        authority
            .active_lease_for_surface(&tab_id, &surface_id())
            .is_none()
    );
    assert_eq!(
        authority.lease(&lease_id).map(|lease| lease.end_reason),
        Some(Some(BrowserLeaseEndReason::SessionClosed))
    );
    assert!(
        !authority
            .session(&browser_session_id)
            .expect("session should remain")
            .tab_ids
            .contains(&tab_id)
    );
}

#[test]
fn annotation_sequence_is_persisted_with_the_tab() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    let annotation = BrowserAnnotation {
        annotation_id: magi_core::BrowserAnnotationId::new("annotation-1"),
        browser_session_id: browser_session_id.clone(),
        tab_id: tab_id.clone(),
        sequence: 1,
        author: BrowserAnnotationAuthor::User,
        kind: BrowserAnnotationKind::Region,
        anchor: BrowserAnnotationAnchor::Region(crate::BrowserRegionAnnotationAnchor {
            url: "about:blank".to_string(),
            origin: None,
            viewport: BrowserViewport::default(),
            scroll_x: 0.0,
            scroll_y: 0.0,
            rect: crate::BrowserNormalizedRect {
                x: 0.1,
                y: 0.1,
                width: 0.2,
                height: 0.2,
            },
            snapshot_revision: 0,
        }),
        comment: "检查区域".to_string(),
        status: crate::BrowserAnnotationStatus::Active,
        screenshot_artifact_id: None,
        created_at: at(6),
        updated_at: at(6),
    };
    authority
        .create_annotation(annotation)
        .expect("annotation should create");
    let durable = authority.durable_state();
    assert_eq!(durable.tabs[0].annotation_sequence, 1);
    assert_eq!(durable.annotations.len(), 1);
}
