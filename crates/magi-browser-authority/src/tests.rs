use std::path::PathBuf;

use magi_core::{
    BrowserLeaseId, BrowserProfileId, BrowserSessionId, BrowserTabId, ExecutionOwnership,
    SessionId, UtcMillis, WorkspaceId,
};

use crate::{
    AcquireBrowserLease, BrowserAnnotation, BrowserAnnotationAnchor, BrowserAnnotationAuthor,
    BrowserAnnotationKind, BrowserAuthority, BrowserDeviceType, BrowserDurableState,
    BrowserLeaseEndReason, BrowserLeaseLifecycle, BrowserProfile, BrowserProfileKind,
    BrowserSessionLifecycle, BrowserSurfaceBinding, BrowserTabLifecycle, BrowserViewport,
    CreateBrowserSession, CreateBrowserTab, GoalControlBinding, MAX_BROWSER_TABS_TOTAL,
    ValidateBrowserNodeSelection, ValidateBrowserWrite,
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
    ready_session_with_ids(authority, "browser-session-1", "session-1")
}

fn ready_session_with_ids(
    authority: &mut BrowserAuthority,
    browser_session_id: &str,
    session_id: &str,
) -> BrowserSessionId {
    let browser_session_id = BrowserSessionId::new(browser_session_id);
    authority
        .create_session(CreateBrowserSession {
            browser_session_id: browser_session_id.clone(),
            workspace_id: Some(WorkspaceId::new("workspace-1")),
            session_id: SessionId::new(session_id),
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
    ready_tab_with_id(authority, browser_session_id, "browser-tab-1")
}

fn ready_tab_with_id(
    authority: &mut BrowserAuthority,
    browser_session_id: &BrowserSessionId,
    tab_id: &str,
) -> BrowserTabId {
    let tab_id = BrowserTabId::new(tab_id);
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

fn binding(
    tab_id: &BrowserTabId,
    surface_id: &str,
    surface_revision: u64,
) -> BrowserSurfaceBinding {
    BrowserSurfaceBinding {
        desktop_epoch: "desktop-epoch".to_string(),
        window_id: "window-1".to_string(),
        surface_id: surface_id.to_string(),
        surface_revision,
        tab_id: tab_id.clone(),
        web_contents_id: 23,
        target_id: "target-1".to_string(),
        browser_context_id: "context-1".to_string(),
        navigation_revision: 0,
    }
}

fn owner() -> ExecutionOwnership {
    ExecutionOwnership {
        session_id: Some(SessionId::new("session-1")),
        workspace_id: Some(WorkspaceId::new("workspace-1")),
        ..ExecutionOwnership::default()
    }
}

#[test]
fn node_selection_requires_current_magi_session_and_page() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    authority
        .set_primary_surface(binding(&tab_id, &surface_id(), 1), at(6))
        .expect("surface should bind");
    let session_id = SessionId::new("session-1");
    let good = ValidateBrowserNodeSelection {
        session_id: &session_id,
        browser_session_id: &browser_session_id,
        tab_id: &tab_id,
        surface_id: &surface_id(),
        navigation_revision: 0,
        page_url: "about:blank",
    };
    authority
        .validate_browser_node_selection(good)
        .expect("current node selection should be accepted");

    let other_session_id = SessionId::new("session-other");
    let error = authority
        .validate_browser_node_selection(ValidateBrowserNodeSelection {
            session_id: &other_session_id,
            ..good
        })
        .expect_err("node selection from another Magi session must be rejected");
    assert!(matches!(
        error,
        crate::BrowserAuthorityError::SessionMagiSessionMismatch { .. }
    ));

    let error = authority
        .validate_browser_node_selection(ValidateBrowserNodeSelection {
            page_url: "https://stale.example/",
            ..good
        })
        .expect_err("node selection from another page must be rejected");
    assert!(matches!(
        error,
        crate::BrowserAuthorityError::NodeSelectionPageMismatch { .. }
    ));
}

#[test]
fn node_selection_requires_current_primary_surface_and_navigation() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    authority
        .set_primary_surface(binding(&tab_id, &surface_id(), 1), at(6))
        .expect("surface should bind");
    let session_id = SessionId::new("session-1");
    let good = ValidateBrowserNodeSelection {
        session_id: &session_id,
        browser_session_id: &browser_session_id,
        tab_id: &tab_id,
        surface_id: &surface_id(),
        navigation_revision: 0,
        page_url: "about:blank",
    };

    let error = authority
        .validate_browser_node_selection(ValidateBrowserNodeSelection {
            surface_id: "surface-old",
            ..good
        })
        .expect_err("node selection from another surface must be rejected");
    assert!(matches!(
        error,
        crate::BrowserAuthorityError::SurfaceNotPrimary { .. }
    ));

    let error = authority
        .validate_browser_node_selection(ValidateBrowserNodeSelection {
            navigation_revision: 1,
            ..good
        })
        .expect_err("node selection from another navigation revision must be rejected");
    assert!(matches!(
        error,
        crate::BrowserAuthorityError::NavigationRevisionMismatch { .. }
    ));
}

#[test]
fn node_selection_rejects_cross_browser_session_tab() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let first_browser_session_id =
        ready_session_with_ids(&mut authority, "browser-session-first", "session-first");
    let first_tab_id = ready_tab_with_id(
        &mut authority,
        &first_browser_session_id,
        "browser-tab-first",
    );
    authority
        .set_primary_surface(binding(&first_tab_id, &surface_id(), 1), at(6))
        .expect("first surface should bind");
    let second_browser_session_id =
        ready_session_with_ids(&mut authority, "browser-session-second", "session-second");
    let second_tab_id = ready_tab_with_id(
        &mut authority,
        &second_browser_session_id,
        "browser-tab-second",
    );
    let first_magi_session_id = SessionId::new("session-first");
    let error = authority
        .validate_browser_node_selection(ValidateBrowserNodeSelection {
            session_id: &first_magi_session_id,
            browser_session_id: &first_browser_session_id,
            tab_id: &second_tab_id,
            surface_id: &surface_id(),
            navigation_revision: 0,
            page_url: "about:blank",
        })
        .expect_err("a tab from another browser session must be rejected");
    assert!(matches!(
        error,
        crate::BrowserAuthorityError::TabSessionMismatch { .. }
    ));
}

#[test]
fn durable_tab_contains_only_user_visible_page_state() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    authority
        .set_primary_surface(binding(&tab_id, &surface_id(), 1), at(6))
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
fn active_browser_tab_is_runtime_focus_and_does_not_persist() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let first_tab_id = ready_tab(&mut authority, &browser_session_id);
    let second_tab_id = BrowserTabId::new("browser-tab-2");
    authority
        .create_tab(CreateBrowserTab {
            tab_id: second_tab_id.clone(),
            browser_session_id: browser_session_id.clone(),
            url: "https://example.com".to_string(),
            now: at(6),
        })
        .expect("second tab should create");
    authority
        .transition_tab(&second_tab_id, BrowserTabLifecycle::Ready, at(7))
        .expect("second tab should become ready");

    authority
        .set_active_tab(&browser_session_id, &second_tab_id)
        .expect("active tab should be recorded");
    assert_eq!(
        authority.active_tab(&browser_session_id),
        Some(&second_tab_id)
    );

    let durable = authority.durable_state();
    let restored = BrowserAuthority::restore_durable(durable, at(8)).expect("state should restore");
    assert_eq!(restored.active_tab(&browser_session_id), None);

    authority
        .transition_tab(&second_tab_id, BrowserTabLifecycle::Closed, at(9))
        .expect("second tab should close");
    assert_eq!(authority.active_tab(&browser_session_id), None);
    assert!(authority.tab(&first_tab_id).is_some());
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
        .set_primary_surface(binding(&tab_id, &surface_id(), 1), at(6))
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
        .set_primary_surface(binding(&tab_id, &surface_id(), 1), at(6))
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
        .set_primary_surface(binding(&tab_id, "surface-2", 2), at(9))
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
fn closing_current_primary_suspends_tab_and_rejects_stale_close_events() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    let first = binding(&tab_id, &surface_id(), 1);
    authority
        .set_primary_surface(first.clone(), at(6))
        .expect("surface should bind");
    let lease = authority
        .acquire_lease(AcquireBrowserLease {
            lease_id: BrowserLeaseId::new("lease-primary-close"),
            tab_id: tab_id.clone(),
            surface_id: surface_id(),
            owner: owner(),
            turn_id: "turn-primary-close".to_string(),
            goal_binding: None,
            acquired_at: at(7),
            expires_at: at(100),
        })
        .expect("lease should acquire");
    let (accepted, tab, revoked) = authority
        .clear_primary_surface(&first, at(8))
        .expect("current primary close should succeed");
    assert!(accepted);
    assert_eq!(tab.lifecycle, BrowserTabLifecycle::Suspended);
    assert!(authority.primary_surface(&tab_id).is_none());
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].lease_id, lease.lease_id);
    assert_eq!(revoked[0].lifecycle, BrowserLeaseLifecycle::Revoked);

    let second = binding(&tab_id, "surface-2", 2);
    authority
        .set_primary_surface(second.clone(), at(9))
        .expect("replacement should bind");
    let (accepted, _, revoked) = authority
        .clear_primary_surface(&first, at(10))
        .expect("stale close should be ignored");
    assert!(!accepted);
    assert!(revoked.is_empty());
    assert_eq!(authority.primary_surface(&tab_id), Some(&second));
}

#[test]
fn stale_primary_surface_events_cannot_rewind_the_binding() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);

    authority
        .set_primary_surface(binding(&tab_id, "surface-current", 8), at(6))
        .expect("current surface should bind");
    authority
        .set_primary_surface(binding(&tab_id, "surface-old", 7), at(7))
        .expect("stale surface event should be ignored");
    assert_eq!(
        authority
            .primary_surface(&tab_id)
            .map(|surface| (surface.surface_id.as_str(), surface.surface_revision)),
        Some(("surface-current", 8))
    );

    authority
        .set_primary_surface(binding(&tab_id, "surface-other", 8), at(8))
        .expect("same-revision replacement should be ignored");
    assert_eq!(
        authority
            .primary_surface(&tab_id)
            .map(|surface| (surface.surface_id.as_str(), surface.surface_revision)),
        Some(("surface-current", 8))
    );

    authority
        .set_primary_surface(binding(&tab_id, "surface-next", 9), at(9))
        .expect("newer surface event should replace the binding");
    assert_eq!(
        authority
            .primary_surface(&tab_id)
            .map(|surface| (surface.surface_id.as_str(), surface.surface_revision)),
        Some(("surface-next", 9))
    );
}

#[test]
fn primary_surface_acceptance_restores_only_the_current_navigation() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);

    let suspended_tab_id = ready_tab_with_id(
        &mut authority,
        &browser_session_id,
        "browser-tab-surface-suspended",
    );
    let suspended = authority
        .transition_tab(&suspended_tab_id, BrowserTabLifecycle::Suspended, at(6))
        .expect("ready tab should become suspended");
    assert_eq!(
        suspended.navigation_revision, 0,
        "挂起只改变运行时可用性，不得伪造新的页面导航代次"
    );
    let before_restore = authority
        .session(&browser_session_id)
        .expect("session should exist")
        .revision;
    let mut suspended_binding = binding(&suspended_tab_id, "surface-suspended", 1);
    suspended_binding.navigation_revision = suspended.navigation_revision;
    let (accepted, restored, revoked) = authority
        .accept_primary_surface(suspended_binding.clone(), at(7))
        .expect("current suspended surface should be accepted");
    assert!(accepted);
    assert!(revoked.is_empty());
    assert_eq!(restored.lifecycle, BrowserTabLifecycle::Ready);
    assert!(
        authority
            .session(&browser_session_id)
            .expect("session should exist")
            .revision
            > before_restore
    );
    let stable_revision = authority
        .session(&browser_session_id)
        .expect("session should exist")
        .revision;
    let (accepted, replayed, revoked) = authority
        .accept_primary_surface(suspended_binding, at(8))
        .expect("the same surface replay should be accepted idempotently");
    assert!(accepted);
    assert!(revoked.is_empty());
    assert_eq!(replayed.lifecycle, BrowserTabLifecycle::Ready);
    assert_eq!(
        authority
            .session(&browser_session_id)
            .expect("session should exist")
            .revision,
        stable_revision
    );

    let creating_tab_id = BrowserTabId::new("browser-tab-surface-creating");
    authority
        .create_tab(CreateBrowserTab {
            tab_id: creating_tab_id.clone(),
            browser_session_id: browser_session_id.clone(),
            url: "about:blank".to_string(),
            now: at(9),
        })
        .expect("tab should start in creating state");
    let creating_binding = binding(&creating_tab_id, "surface-creating", 2);
    let (accepted, restored, _) = authority
        .accept_primary_surface(creating_binding, at(11))
        .expect("creating surface should be accepted and recovered");
    assert!(accepted);
    assert_eq!(restored.lifecycle, BrowserTabLifecycle::Ready);

    let crashed_tab_id = ready_tab_with_id(
        &mut authority,
        &browser_session_id,
        "browser-tab-surface-crashed",
    );
    authority
        .transition_tab(&crashed_tab_id, BrowserTabLifecycle::Crashed, at(12))
        .expect("tab should be crashed before host recreation");
    let crashed_binding = binding(&crashed_tab_id, "surface-crashed", 3);
    let (accepted, restored, _) = authority
        .accept_primary_surface(crashed_binding, at(13))
        .expect("crashed surface should be accepted and recovered");
    assert!(accepted);
    assert_eq!(restored.lifecycle, BrowserTabLifecycle::Ready);

    let stale_tab_id = ready_tab_with_id(
        &mut authority,
        &browser_session_id,
        "browser-tab-surface-stale-navigation",
    );
    authority
        .set_primary_surface(binding(&stale_tab_id, "surface-stale", 4), at(14))
        .expect("stale-navigation surface should bind");
    authority
        .apply_host_page_state(
            &stale_tab_id,
            1,
            "https://example.com/".to_string(),
            Some("https://example.com".to_string()),
            "Example".to_string(),
            at(15),
        )
        .expect("navigation should advance the tab");
    let (accepted, current, _) = authority
        .accept_primary_surface(binding(&stale_tab_id, "surface-stale", 4), at(16))
        .expect("old navigation surface should be ignored");
    assert!(!accepted);
    assert_eq!(current.navigation_revision, 1);
}

#[test]
fn suspending_tab_preserves_navigation_revision_for_surface_rebinding() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    authority
        .apply_host_page_state(
            &tab_id,
            1,
            "https://example.com/".to_string(),
            Some("https://example.com".to_string()),
            "Example".to_string(),
            at(6),
        )
        .expect("页面状态应推进导航代次");
    let mut surface = binding(&tab_id, "surface-rebind", 1);
    surface.navigation_revision = 1;
    authority
        .set_primary_surface(surface.clone(), at(7))
        .expect("当前 Surface 应绑定");

    let suspended = authority
        .transition_tab(&tab_id, BrowserTabLifecycle::Suspended, at(8))
        .expect("Ready Tab 应进入 Suspended");
    assert_eq!(suspended.navigation_revision, 1);

    let (accepted, restored, revoked) = authority
        .accept_primary_surface(surface.clone(), at(9))
        .expect("重连时应接受原页面的真实 Surface");
    assert!(accepted);
    assert!(revoked.is_empty());
    assert_eq!(restored.lifecycle, BrowserTabLifecycle::Ready);
    assert_eq!(restored.navigation_revision, 1);
    assert_eq!(authority.primary_surface(&tab_id), Some(&surface));
}

#[test]
fn runtime_recovery_preserves_navigation_revision_for_replayed_surface() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    authority
        .apply_host_page_state(
            &tab_id,
            1,
            "https://example.com/".to_string(),
            Some("https://example.com".to_string()),
            "Example".to_string(),
            at(6),
        )
        .expect("页面状态应推进导航代次");
    let mut surface = binding(&tab_id, "surface-recovery", 1);
    surface.navigation_revision = 1;
    authority
        .set_primary_surface(surface.clone(), at(7))
        .expect("当前 Surface 应绑定");

    authority.begin_runtime_recovery(at(8));
    let recovering_tab = authority.tab(&tab_id).expect("Tab 应保留");
    assert_eq!(recovering_tab.lifecycle, BrowserTabLifecycle::Suspended);
    assert_eq!(recovering_tab.navigation_revision, 1);

    let (accepted, restored, revoked) = authority
        .accept_primary_surface(surface.clone(), at(9))
        .expect("运行时恢复应接受仍指向当前页面的 Surface");
    assert!(accepted);
    assert!(revoked.is_empty());
    assert_eq!(restored.lifecycle, BrowserTabLifecycle::Ready);
    assert_eq!(restored.navigation_revision, 1);
}

#[test]
fn durable_restore_suspends_tabs_without_changing_navigation_revision() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    authority
        .apply_host_page_state(
            &tab_id,
            1,
            "https://example.com/".to_string(),
            Some("https://example.com".to_string()),
            "Example".to_string(),
            at(6),
        )
        .expect("页面状态应推进导航代次");

    let restored =
        BrowserAuthority::restore(authority.snapshot(), at(7)).expect("持久化恢复应成功");
    let tab = restored.tab(&tab_id).expect("恢复后 Tab 应存在");
    assert_eq!(tab.lifecycle, BrowserTabLifecycle::Suspended);
    assert_eq!(tab.navigation_revision, 1);
}

#[test]
fn session_revision_tracks_surface_page_snapshot_annotation_and_lease_changes() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);

    let revision_after_tab = authority
        .session(&browser_session_id)
        .expect("session should exist")
        .revision;
    authority
        .set_primary_surface(binding(&tab_id, &surface_id(), 1), at(6))
        .expect("surface should bind");
    let revision_after_surface = authority
        .session(&browser_session_id)
        .expect("session should exist")
        .revision;
    assert!(revision_after_surface > revision_after_tab);

    authority
        .set_active_tab(&browser_session_id, &tab_id)
        .expect("active tab should be recorded");
    let revision_after_active = authority
        .session(&browser_session_id)
        .expect("session should exist")
        .revision;
    assert!(revision_after_active > revision_after_surface);
    authority
        .set_active_tab(&browser_session_id, &tab_id)
        .expect("replaying the same active tab should be idempotent");
    assert_eq!(
        authority
            .session(&browser_session_id)
            .expect("session should exist")
            .revision,
        revision_after_active
    );

    authority
        .apply_host_page_state(
            &tab_id,
            1,
            "https://example.com/".to_string(),
            Some("https://example.com".to_string()),
            "Example".to_string(),
            at(7),
        )
        .expect("page state should apply");
    let revision_after_page = authority
        .session(&browser_session_id)
        .expect("session should exist")
        .revision;
    assert!(revision_after_page > revision_after_active);

    let (_, snapshot_revision) = authority
        .record_snapshot(&tab_id, at(8))
        .expect("snapshot should be recorded");
    let revision_after_snapshot = authority
        .session(&browser_session_id)
        .expect("session should exist")
        .revision;
    assert!(revision_after_snapshot > revision_after_page);

    let annotation = BrowserAnnotation {
        annotation_id: magi_core::BrowserAnnotationId::new("annotation-revision"),
        browser_session_id: browser_session_id.clone(),
        tab_id: tab_id.clone(),
        sequence: 0,
        author: BrowserAnnotationAuthor::User,
        kind: BrowserAnnotationKind::Region,
        anchor: BrowserAnnotationAnchor::Region(crate::BrowserRegionAnnotationAnchor {
            url: "https://example.com/".to_string(),
            origin: Some("https://example.com".to_string()),
            viewport: BrowserViewport::default(),
            scroll_x: 0.0,
            scroll_y: 0.0,
            rect: crate::BrowserNormalizedRect {
                x: 0.1,
                y: 0.1,
                width: 0.2,
                height: 0.2,
            },
            snapshot_revision,
        }),
        comment: "revision".to_string(),
        status: crate::BrowserAnnotationStatus::Active,
        screenshot_artifact_id: None,
        created_at: at(9),
        updated_at: at(9),
    };
    let annotation = authority
        .create_annotation(annotation)
        .expect("annotation should be recorded");
    let revision_after_annotation = authority
        .session(&browser_session_id)
        .expect("session should exist")
        .revision;
    assert!(revision_after_annotation > revision_after_snapshot);
    authority
        .update_annotation_comment(&annotation.annotation_id, "updated".to_string(), at(10))
        .expect("annotation comment should update");
    let revision_after_comment = authority
        .session(&browser_session_id)
        .expect("session should exist")
        .revision;
    assert!(revision_after_comment > revision_after_annotation);

    let lease = authority
        .acquire_lease(AcquireBrowserLease {
            lease_id: BrowserLeaseId::new("lease-revision"),
            tab_id: tab_id.clone(),
            surface_id: surface_id(),
            owner: owner(),
            turn_id: "turn-revision".to_string(),
            goal_binding: None,
            acquired_at: at(11),
            expires_at: at(100),
        })
        .expect("lease should acquire");
    let revision_after_lease = authority
        .session(&browser_session_id)
        .expect("session should exist")
        .revision;
    assert!(revision_after_lease > revision_after_comment);
    authority
        .release_lease(&lease.lease_id, at(12))
        .expect("lease should release");
    assert!(
        authority
            .session(&browser_session_id)
            .expect("session should exist")
            .revision
            > revision_after_lease
    );
}

#[test]
fn stale_epoch_and_window_bindings_cannot_replace_the_current_surface() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);

    authority
        .set_primary_surface(binding(&tab_id, "surface-current", 8), at(6))
        .expect("current surface should bind");

    let mut stale_epoch = binding(&tab_id, "surface-old-epoch", 9);
    stale_epoch.desktop_epoch = "desktop-old-epoch".to_string();
    assert!(
        authority
            .set_primary_surface(stale_epoch, at(7))
            .expect("stale epoch event should be ignored")
            .is_empty()
    );

    let mut stale_window = binding(&tab_id, "surface-old-window", 8);
    stale_window.window_id = "window-old".to_string();
    assert!(
        authority
            .set_primary_surface(stale_window, at(8))
            .expect("same epoch stale window event should be ignored")
            .is_empty()
    );

    assert_eq!(
        authority.primary_surface(&tab_id),
        Some(&binding(&tab_id, "surface-current", 8))
    );

    let revoked = authority.accept_desktop_epoch("desktop-next".to_string(), at(9));
    assert!(revoked.is_empty());
    let mut next = binding(&tab_id, "surface-next", 1);
    next.desktop_epoch = "desktop-next".to_string();
    assert!(
        authority
            .set_primary_surface(next.clone(), at(10))
            .expect("new desktop epoch should bind")
            .is_empty()
    );
    assert_eq!(authority.primary_surface(&tab_id), Some(&next));

    let mut delayed_old = binding(&tab_id, "surface-delayed-old", 100);
    delayed_old.desktop_epoch = "desktop-epoch".to_string();
    assert!(
        authority
            .set_primary_surface(delayed_old, at(11))
            .expect("delayed old epoch event should be ignored")
            .is_empty()
    );
    assert_eq!(authority.primary_surface(&tab_id), Some(&next));
}

#[test]
fn page_binding_updates_navigation_only_for_the_current_window_surface() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    authority
        .set_primary_surface(binding(&tab_id, "surface-page", 4), at(6))
        .expect("surface should bind");

    let mut current = binding(&tab_id, "surface-page", 4);
    current.navigation_revision = 2;
    assert!(
        authority
            .accept_page_binding(&current, at(7))
            .expect("current page binding should be accepted")
            .0
    );
    assert_eq!(
        authority
            .primary_surface(&tab_id)
            .map(|surface| surface.navigation_revision),
        Some(2)
    );

    let mut stale = current.clone();
    stale.navigation_revision = 1;
    assert!(
        !authority
            .accept_page_binding(&stale, at(8))
            .expect("stale page binding should be rejected")
            .0
    );

    let mut other_window = current;
    other_window.window_id = "window-other".to_string();
    other_window.navigation_revision = 3;
    assert!(
        !authority
            .accept_page_binding(&other_window, at(9))
            .expect("other window page binding should be rejected")
            .0
    );
}

#[test]
fn agent_lease_retains_the_complete_surface_identity() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    let mut surface = binding(&tab_id, "surface-lease", 5);
    surface.navigation_revision = 3;
    authority
        .set_primary_surface(surface.clone(), at(6))
        .expect("surface should bind");

    let lease = authority
        .acquire_lease(AcquireBrowserLease {
            lease_id: BrowserLeaseId::new("lease-full-binding"),
            tab_id: tab_id.clone(),
            surface_id: surface.surface_id.clone(),
            owner: owner(),
            turn_id: "turn-full-binding".to_string(),
            goal_binding: None,
            acquired_at: at(8),
            expires_at: at(100),
        })
        .expect("lease should acquire");
    assert_eq!(lease.surface_binding(), surface);
}

#[test]
fn crashed_tab_releases_surface_control_without_deleting_the_logical_tab() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    authority
        .set_primary_surface(binding(&tab_id, &surface_id(), 1), at(6))
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
fn snapshot_revision_is_authority_owned_and_survives_restart() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);

    let first = authority
        .record_snapshot(&tab_id, at(6))
        .expect("authority should allocate the first snapshot revision");
    assert_eq!(first, (0, 1));
    authority
        .apply_host_snapshot_revision(&tab_id, 1, at(7))
        .expect("Host must echo the authority revision exactly");
    assert!(
        authority
            .apply_host_snapshot_revision(&tab_id, 0, at(7))
            .is_err()
    );
    assert!(
        authority
            .apply_host_snapshot_revision(&tab_id, 2, at(7))
            .is_err()
    );

    let durable = authority.durable_state();
    assert_eq!(durable.tabs[0].snapshot_revision, 1);
    let restored = BrowserAuthority::restore_durable(durable, at(8))
        .expect("durable browser state should restore");
    let restored_tab = restored.tab(&tab_id).expect("tab should survive restart");
    assert_eq!(restored_tab.snapshot_revision, 2);
    assert!(restored_tab.snapshot_revision > first.1);
}

#[test]
fn old_snapshot_revision_is_rejected_after_navigation() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);

    let old = authority
        .record_snapshot(&tab_id, at(6))
        .expect("authority should allocate the first snapshot revision");
    authority
        .apply_host_page_state(
            &tab_id,
            1,
            "https://example.com/".to_string(),
            Some("https://example.com".to_string()),
            "Example".to_string(),
            at(7),
        )
        .expect("navigation should advance the document revision");
    assert!(
        authority
            .validate_snapshot_result(&tab_id, old.0, old.1)
            .is_err()
    );

    let current = authority
        .record_snapshot(&tab_id, at(8))
        .expect("authority should allocate the post-navigation revision");
    assert_eq!(current, (1, 3));
    authority
        .validate_snapshot_result(&tab_id, current.0, current.1)
        .expect("current navigation and snapshot revisions should validate");
}

#[test]
fn closing_tab_revokes_surface_lease_and_removes_membership() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);
    authority
        .set_primary_surface(binding(&tab_id, &surface_id(), 1), at(6))
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
fn inactive_tabs_are_reclaimable_and_closed_tabs_release_capacity() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let browser_session_id = ready_session(&mut authority);
    let tab_id = ready_tab(&mut authority, &browser_session_id);

    assert!(!authority.is_reclaimable_tab(&tab_id, at(6)));
    authority
        .transition_tab(&tab_id, BrowserTabLifecycle::Suspended, at(7))
        .expect("ready tab should become suspended");
    assert!(authority.is_reclaimable_tab(&tab_id, at(8)));
    assert_eq!(authority.live_tab_count(), 1);

    authority
        .transition_tab(&tab_id, BrowserTabLifecycle::Closed, at(9))
        .expect("suspended tab should close");
    assert!(!authority.is_reclaimable_tab(&tab_id, at(10)));
    assert_eq!(authority.live_tab_count(), 0);
}

#[test]
fn reclaiming_a_suspended_tab_makes_global_capacity_available() {
    let mut authority = BrowserAuthority::new();
    register_profile(&mut authority);
    let first_session_id = ready_session_with_ids(
        &mut authority,
        "browser-session-capacity-first",
        "session-capacity-first",
    );
    let second_session_id = ready_session_with_ids(
        &mut authority,
        "browser-session-capacity-second",
        "session-capacity-second",
    );

    for index in 0..(MAX_BROWSER_TABS_TOTAL / 2) {
        authority
            .create_tab(CreateBrowserTab {
                tab_id: BrowserTabId::new(format!("browser-tab-capacity-first-{index}")),
                browser_session_id: first_session_id.clone(),
                url: "about:blank".to_string(),
                now: at(10 + index as u64),
            })
            .expect("first session should accept tabs up to its limit");
    }
    let mut suspended_tab_id = None;
    for index in 0..(MAX_BROWSER_TABS_TOTAL / 2) {
        let tab_id = BrowserTabId::new(format!("browser-tab-capacity-second-{index}"));
        authority
            .create_tab(CreateBrowserTab {
                tab_id: tab_id.clone(),
                browser_session_id: second_session_id.clone(),
                url: "about:blank".to_string(),
                now: at(50 + index as u64),
            })
            .expect("second session should fill the remaining global capacity");
        if index == 0 {
            suspended_tab_id = Some(tab_id);
        }
    }
    let suspended_tab_id = suspended_tab_id.expect("one tab should be selected for reclamation");
    authority
        .transition_tab(&suspended_tab_id, BrowserTabLifecycle::Suspended, at(90))
        .expect("a tab should become suspended without leaving global capacity");
    assert_eq!(authority.live_tab_count(), MAX_BROWSER_TABS_TOTAL);

    let third_session_id = ready_session_with_ids(
        &mut authority,
        "browser-session-capacity-third",
        "session-capacity-third",
    );
    let rejected = authority.create_tab(CreateBrowserTab {
        tab_id: BrowserTabId::new("browser-tab-capacity-rejected"),
        browser_session_id: third_session_id.clone(),
        url: "about:blank".to_string(),
        now: at(91),
    });
    assert!(matches!(
        rejected,
        Err(crate::BrowserAuthorityError::GlobalTabLimitReached { .. })
    ));

    authority
        .transition_tab(&suspended_tab_id, BrowserTabLifecycle::Closed, at(92))
        .expect("reclaimed tab should close");
    assert_eq!(authority.live_tab_count(), MAX_BROWSER_TABS_TOTAL - 1);
    authority
        .create_tab(CreateBrowserTab {
            tab_id: BrowserTabId::new("browser-tab-capacity-reclaimed"),
            browser_session_id: third_session_id,
            url: "about:blank".to_string(),
            now: at(93),
        })
        .expect("closing a suspended tab should release global capacity");
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
