use std::{fs, io::Cursor, path::PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use ed25519_dalek::{Signer, SigningKey};

use magi_core::{
    AccessProfile, BrowserAnnotationId, BrowserCommandId, BrowserLeaseId, BrowserProfileId,
    BrowserSessionId, BrowserTabId, ExecutionOwnership, GoalId, SessionId, UtcMillis, WorkspaceId,
};
use semver::Version;
use sha2::{Digest, Sha256};

use crate::{
    AcquireBrowserLease, BrowserAnnotation, BrowserAnnotationAnchor, BrowserAnnotationAuthor,
    BrowserAnnotationKind, BrowserAnnotationStatus, BrowserAuthority, BrowserAuthorityError,
    BrowserCapabilityRejection, BrowserCapabilitySnapshot, BrowserCapabilityUnavailableReason,
    BrowserLeaseEndReason, BrowserLeaseLifecycle, BrowserNormalizedRect, BrowserProfile,
    BrowserProfileControlMode, BrowserProfileKind, BrowserRegionAnnotationAnchor,
    BrowserRuntimeComponentError, BrowserRuntimeComponentStatus, BrowserRuntimeFile,
    BrowserRuntimeManager, BrowserRuntimeManagerConfig, BrowserRuntimeManifest,
    BrowserRuntimeReleaseChannel, BrowserRuntimeTarget, BrowserRuntimeUpdateLevel,
    BrowserSessionLifecycle, BrowserTabLifecycle, BrowserToolAccess, BrowserToolKind,
    BrowserViewport, BrowserViewportMode, CreateBrowserSession, CreateBrowserTab,
    GoalControlBinding, SignedBrowserRuntimeRelease, ValidateBrowserWrite,
};

fn at(value: u64) -> UtcMillis {
    UtcMillis(value)
}

fn profile_id() -> BrowserProfileId {
    BrowserProfileId::new("browser-profile-default")
}

fn workspace_id() -> WorkspaceId {
    WorkspaceId::new("workspace-a")
}

fn register_default_profile(authority: &mut BrowserAuthority) {
    authority
        .register_profile(BrowserProfile {
            profile_id: profile_id(),
            kind: BrowserProfileKind::ManagedDefault,
            data_path: PathBuf::from("/tmp/magi-browser-profile"),
            created_at: at(1),
            updated_at: at(1),
        })
        .unwrap();
}

fn create_ready_session(
    authority: &mut BrowserAuthority,
    browser_session_id: &str,
    session_id: &str,
) -> BrowserSessionId {
    let browser_session_id = BrowserSessionId::new(browser_session_id);
    authority
        .create_session(CreateBrowserSession {
            browser_session_id: browser_session_id.clone(),
            workspace_id: workspace_id(),
            session_id: SessionId::new(session_id),
            profile_id: profile_id(),
            now: at(2),
        })
        .unwrap();
    authority
        .transition_session(&browser_session_id, BrowserSessionLifecycle::Ready, at(3))
        .unwrap();
    browser_session_id
}

fn create_ready_tab(
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
            viewport: BrowserViewport::default(),
            now: at(4),
        })
        .unwrap();
    authority
        .transition_tab(&tab_id, BrowserTabLifecycle::Ready, at(5))
        .unwrap();
    tab_id
}

fn owner(session_id: &str) -> ExecutionOwnership {
    ExecutionOwnership {
        session_id: Some(SessionId::new(session_id)),
        workspace_id: Some(workspace_id()),
        mission_id: None,
        task_id: Some(magi_core::TaskId::new(format!("task-{session_id}"))),
        worker_id: Some(magi_core::WorkerId::new(format!("worker-{session_id}"))),
        execution_chain_ref: Some(format!("chain-{session_id}")),
    }
}

fn acquire(
    authority: &mut BrowserAuthority,
    lease_id: &str,
    browser_session_id: &BrowserSessionId,
    session_id: &str,
    now: u64,
) -> crate::BrowserControlLease {
    authority
        .acquire_lease(AcquireBrowserLease {
            lease_id: BrowserLeaseId::new(lease_id),
            profile_id: profile_id(),
            browser_session_id: browser_session_id.clone(),
            owner: owner(session_id),
            turn_id: format!("turn-{session_id}"),
            goal_binding: Some(GoalControlBinding {
                goal_id: GoalId::new(format!("goal-{session_id}")),
                control_revision: 3,
            }),
            acquired_at: at(now),
            expires_at: at(now + 100),
        })
        .unwrap()
}

#[test]
fn one_open_browser_session_is_allowed_per_magi_session() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    create_ready_session(&mut authority, "browser-session-a", "session-a");

    let error = authority
        .create_session(CreateBrowserSession {
            browser_session_id: BrowserSessionId::new("browser-session-b"),
            workspace_id: workspace_id(),
            session_id: SessionId::new("session-a"),
            profile_id: profile_id(),
            now: at(10),
        })
        .unwrap_err();

    assert!(matches!(
        error,
        BrowserAuthorityError::OpenSessionAlreadyExists { .. }
    ));
}

#[test]
fn profile_level_lease_serializes_writers_across_browser_sessions() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let session_a = create_ready_session(&mut authority, "browser-session-a", "session-a");
    let session_b = create_ready_session(&mut authority, "browser-session-b", "session-b");
    let first = acquire(&mut authority, "lease-a", &session_a, "session-a", 10);

    let error = authority
        .acquire_lease(AcquireBrowserLease {
            lease_id: BrowserLeaseId::new("lease-b"),
            profile_id: profile_id(),
            browser_session_id: session_b.clone(),
            owner: owner("session-b"),
            turn_id: "turn-b".to_string(),
            goal_binding: None,
            acquired_at: at(11),
            expires_at: at(111),
        })
        .unwrap_err();
    assert_eq!(
        error,
        BrowserAuthorityError::LeaseConflict {
            lease_id: first.lease_id.clone()
        }
    );

    authority.release_lease(&first.lease_id, at(12)).unwrap();
    let second = authority
        .acquire_lease(AcquireBrowserLease {
            lease_id: BrowserLeaseId::new("lease-b"),
            profile_id: profile_id(),
            browser_session_id: session_b,
            owner: owner("session-b"),
            turn_id: "turn-b".to_string(),
            goal_binding: None,
            acquired_at: at(13),
            expires_at: at(113),
        })
        .unwrap();
    assert!(second.fence > first.fence);
}

#[test]
fn user_takeover_fences_agent_and_requires_a_new_lease_after_release() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let session = create_ready_session(&mut authority, "browser-session-a", "session-a");
    let tab = create_ready_tab(&mut authority, &session, "tab-a");
    let lease = acquire(&mut authority, "lease-a", &session, "session-a", 10);

    let revoked = authority.take_user_control(&profile_id(), at(20)).unwrap();
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].lifecycle, BrowserLeaseLifecycle::Revoked);
    assert_eq!(
        revoked[0].end_reason,
        Some(BrowserLeaseEndReason::UserTakeover)
    );
    assert_eq!(
        authority.profile_control_mode(&profile_id()).unwrap(),
        BrowserProfileControlMode::User
    );
    let error = authority
        .validate_write(ValidateBrowserWrite {
            lease_id: &lease.lease_id,
            fence: lease.fence,
            browser_session_id: &session,
            tab_id: &tab,
            owner: &lease.owner,
            turn_id: &lease.turn_id,
            goal_binding: lease.goal_binding.as_ref(),
            now: at(21),
        })
        .unwrap_err();
    assert_eq!(error, BrowserAuthorityError::LeaseNotHeld(lease.lease_id));

    let blocked = authority
        .acquire_lease(AcquireBrowserLease {
            lease_id: BrowserLeaseId::new("lease-while-user"),
            profile_id: profile_id(),
            browser_session_id: session.clone(),
            owner: owner("session-a"),
            turn_id: "turn-next".to_string(),
            goal_binding: None,
            acquired_at: at(22),
            expires_at: at(122),
        })
        .unwrap_err();
    assert_eq!(blocked, BrowserAuthorityError::UserHasControl(profile_id()));

    authority.release_user_control(&profile_id()).unwrap();
    let next = authority
        .acquire_lease(AcquireBrowserLease {
            lease_id: BrowserLeaseId::new("lease-next"),
            profile_id: profile_id(),
            browser_session_id: session,
            owner: owner("session-a"),
            turn_id: "turn-next".to_string(),
            goal_binding: None,
            acquired_at: at(23),
            expires_at: at(123),
        })
        .unwrap();
    assert!(next.fence > lease.fence);
}

#[test]
fn write_validation_rejects_stale_goal_and_cross_session_tab() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let session_a = create_ready_session(&mut authority, "browser-session-a", "session-a");
    let tab_a = create_ready_tab(&mut authority, &session_a, "tab-a");
    let session_b = create_ready_session(&mut authority, "browser-session-b", "session-b");
    let tab_b = create_ready_tab(&mut authority, &session_b, "tab-b");
    let lease = acquire(&mut authority, "lease-a", &session_a, "session-a", 10);
    let stale_binding = GoalControlBinding {
        goal_id: GoalId::new("goal-session-a"),
        control_revision: 4,
    };

    let stale = authority
        .validate_write(ValidateBrowserWrite {
            lease_id: &lease.lease_id,
            fence: lease.fence,
            browser_session_id: &session_a,
            tab_id: &tab_a,
            owner: &lease.owner,
            turn_id: &lease.turn_id,
            goal_binding: Some(&stale_binding),
            now: at(11),
        })
        .unwrap_err();
    assert_eq!(stale, BrowserAuthorityError::GoalBindingMismatch);

    let mut wrong_owner = lease.owner.clone();
    wrong_owner.worker_id = Some(magi_core::WorkerId::new("worker-other"));
    let owner_mismatch = authority
        .validate_write(ValidateBrowserWrite {
            lease_id: &lease.lease_id,
            fence: lease.fence,
            browser_session_id: &session_a,
            tab_id: &tab_a,
            owner: &wrong_owner,
            turn_id: &lease.turn_id,
            goal_binding: lease.goal_binding.as_ref(),
            now: at(11),
        })
        .unwrap_err();
    assert_eq!(owner_mismatch, BrowserAuthorityError::LeaseOwnerMismatch);

    let turn_mismatch = authority
        .validate_write(ValidateBrowserWrite {
            lease_id: &lease.lease_id,
            fence: lease.fence,
            browser_session_id: &session_a,
            tab_id: &tab_a,
            owner: &lease.owner,
            turn_id: "turn-other",
            goal_binding: lease.goal_binding.as_ref(),
            now: at(11),
        })
        .unwrap_err();
    assert_eq!(turn_mismatch, BrowserAuthorityError::LeaseTurnMismatch);

    let cross_session = authority
        .validate_write(ValidateBrowserWrite {
            lease_id: &lease.lease_id,
            fence: lease.fence,
            browser_session_id: &session_a,
            tab_id: &tab_b,
            owner: &lease.owner,
            turn_id: &lease.turn_id,
            goal_binding: lease.goal_binding.as_ref(),
            now: at(12),
        })
        .unwrap_err();
    assert!(matches!(
        cross_session,
        BrowserAuthorityError::TabSessionMismatch { .. }
    ));
}

#[test]
fn expired_lease_is_terminal_and_advances_the_profile_fence() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let session = create_ready_session(&mut authority, "browser-session-a", "session-a");
    let tab = create_ready_tab(&mut authority, &session, "tab-a");
    let lease = acquire(&mut authority, "lease-a", &session, "session-a", 10);

    let error = authority
        .validate_write(ValidateBrowserWrite {
            lease_id: &lease.lease_id,
            fence: lease.fence,
            browser_session_id: &session,
            tab_id: &tab,
            owner: &lease.owner,
            turn_id: &lease.turn_id,
            goal_binding: lease.goal_binding.as_ref(),
            now: at(110),
        })
        .unwrap_err();
    assert_eq!(error, BrowserAuthorityError::LeaseExpired(lease.lease_id));
    assert!(authority.active_lease_for_profile(&profile_id()).is_none());
}

#[test]
fn navigation_invalidates_snapshot_references() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let session = create_ready_session(&mut authority, "browser-session-a", "session-a");
    let tab = create_ready_tab(&mut authority, &session, "tab-a");
    let snapshot_revision = authority.record_snapshot(&tab, at(10)).unwrap();
    authority
        .validate_snapshot_ref(&session, &tab, snapshot_revision)
        .unwrap();

    authority
        .update_tab_document(
            &tab,
            "https://example.com".to_string(),
            Some("https://example.com".to_string()),
            "Example".to_string(),
            at(11),
        )
        .unwrap();
    let error = authority
        .validate_snapshot_ref(&session, &tab, snapshot_revision)
        .unwrap_err();
    assert!(matches!(
        error,
        BrowserAuthorityError::SnapshotRevisionMismatch { .. }
    ));
}

#[test]
fn annotation_navigation_reference_survives_frame_progress_but_not_navigation() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let session = create_ready_session(&mut authority, "browser-session-a", "session-a");
    let tab = create_ready_tab(&mut authority, &session, "tab-a");
    let navigation_revision = authority.tab(&tab).unwrap().navigation_revision;

    authority.record_frame(&tab, 1, at(10)).unwrap();
    authority.record_frame(&tab, 2, at(11)).unwrap();
    authority
        .validate_navigation_ref(&session, &tab, navigation_revision)
        .unwrap();

    authority
        .update_tab_document(
            &tab,
            "https://example.com".to_string(),
            Some("https://example.com".to_string()),
            "Example".to_string(),
            at(12),
        )
        .unwrap();
    let error = authority
        .validate_navigation_ref(&session, &tab, navigation_revision)
        .unwrap_err();
    assert!(matches!(
        error,
        BrowserAuthorityError::NavigationRevisionMismatch { .. }
    ));
}

#[test]
fn restore_revokes_leases_and_recovers_only_persistent_session_boundary() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let session = create_ready_session(&mut authority, "browser-session-a", "session-a");
    let tab = create_ready_tab(&mut authority, &session, "tab-a");
    let crashed_tab = create_ready_tab(&mut authority, &session, "tab-crashed");
    authority
        .transition_tab(&crashed_tab, BrowserTabLifecycle::Crashed, at(9))
        .unwrap();
    let lease = acquire(&mut authority, "lease-a", &session, "session-a", 10);
    let snapshot = authority.snapshot();

    let restored = BrowserAuthority::restore(snapshot, at(50)).unwrap();
    assert_eq!(
        restored.session(&session).unwrap().lifecycle,
        BrowserSessionLifecycle::Recovering
    );
    assert_eq!(restored.session(&session).unwrap().runtime_epoch, 1);
    assert_eq!(
        restored.tab(&tab).unwrap().lifecycle,
        BrowserTabLifecycle::Suspended
    );
    assert_eq!(
        restored.tab(&crashed_tab).unwrap().lifecycle,
        BrowserTabLifecycle::Suspended
    );
    let restored_lease = restored.lease(&lease.lease_id).unwrap();
    assert_eq!(restored_lease.lifecycle, BrowserLeaseLifecycle::Revoked);
    assert_eq!(
        restored_lease.end_reason,
        Some(BrowserLeaseEndReason::RuntimeShutdown)
    );
    assert!(restored.active_lease_for_profile(&profile_id()).is_none());
    assert_eq!(
        restored.profile_control_mode(&profile_id()).unwrap(),
        BrowserProfileControlMode::Agent
    );
}

#[test]
fn session_recovery_releases_profile_writer_for_other_sessions() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let session_a = create_ready_session(&mut authority, "browser-session-a", "session-a");
    let session_b = create_ready_session(&mut authority, "browser-session-b", "session-b");
    let lease = acquire(&mut authority, "lease-a", &session_a, "session-a", 10);

    authority
        .transition_session(&session_a, BrowserSessionLifecycle::Recovering, at(20))
        .unwrap();
    assert_eq!(
        authority.lease(&lease.lease_id).unwrap().end_reason,
        Some(BrowserLeaseEndReason::RuntimeUnavailable)
    );
    let next = authority
        .acquire_lease(AcquireBrowserLease {
            lease_id: BrowserLeaseId::new("lease-b"),
            profile_id: profile_id(),
            browser_session_id: session_b,
            owner: owner("session-b"),
            turn_id: "turn-b".to_string(),
            goal_binding: None,
            acquired_at: at(21),
            expires_at: at(121),
        })
        .unwrap();
    assert!(next.fence > lease.fence);
}

#[test]
fn capability_snapshot_drives_catalog_and_execution_with_one_revision() {
    let read_only = BrowserCapabilitySnapshot {
        revision: 7,
        in_app_browser_enabled: true,
        browser_use_enabled: true,
        runtime_status: BrowserRuntimeComponentStatus::Installed,
        host_protocol_compatible: true,
        access_profile: AccessProfile::ReadOnly,
    };
    assert_eq!(read_only.visible_tools(), BrowserToolKind::ALL);
    read_only
        .allows_execution(7, BrowserToolKind::Tabs, BrowserToolAccess::Read)
        .unwrap();
    read_only
        .allows_execution(7, BrowserToolKind::Tabs, BrowserToolAccess::Write)
        .unwrap();
    assert!(matches!(
        read_only
            .allows_execution(6, BrowserToolKind::Snapshot, BrowserToolAccess::Read,)
            .unwrap_err(),
        BrowserCapabilityRejection::SnapshotRevisionMismatch { .. }
    ));
}

#[test]
fn browser_ui_and_model_tool_switches_are_independent() {
    let capability = BrowserCapabilitySnapshot {
        revision: 8,
        in_app_browser_enabled: false,
        browser_use_enabled: true,
        runtime_status: BrowserRuntimeComponentStatus::Installed,
        host_protocol_compatible: true,
        access_profile: AccessProfile::FullAccess,
    };
    assert_eq!(capability.visible_tools(), BrowserToolKind::ALL);
    capability
        .allows_execution(8, BrowserToolKind::Click, BrowserToolAccess::Write)
        .unwrap();
}

#[test]
fn changing_tab_viewport_updates_snapshot_without_navigation() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let browser_session_id = create_ready_session(
        &mut authority,
        "browser-session-viewport",
        "session-viewport",
    );
    let tab_id = create_ready_tab(&mut authority, &browser_session_id, "tab-viewport");
    let updated = authority
        .set_tab_viewport(
            &tab_id,
            BrowserViewport {
                width: 720,
                height: 540,
                device_scale_factor_millis: 1_000,
                device_type: crate::BrowserDeviceType::Desktop,
            },
            BrowserViewportMode::Fixed,
            at(10),
        )
        .unwrap();
    assert_eq!(updated.viewport.width, 720);
    assert_eq!(updated.viewport.height, 540);
    assert_eq!(updated.snapshot_revision, 1);
    assert_eq!(updated.navigation_revision, 0);
    assert_eq!(updated.viewport_mode, BrowserViewportMode::Fixed);

    let automatic = authority
        .set_tab_viewport(&tab_id, updated.viewport, BrowserViewportMode::Auto, at(11))
        .unwrap();
    assert_eq!(automatic.viewport_mode, BrowserViewportMode::Auto);
    assert_eq!(automatic.snapshot_revision, updated.snapshot_revision);
}

#[test]
fn durable_browser_tab_contains_url_state_but_never_parent_viewport_state() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let browser_session_id = create_ready_session(
        &mut authority,
        "browser-session-durable-viewport",
        "session-durable-viewport",
    );
    let tab_id = create_ready_tab(&mut authority, &browser_session_id, "tab-durable-viewport");
    authority
        .set_tab_viewport(
            &tab_id,
            BrowserViewport {
                width: 390,
                height: 844,
                device_scale_factor_millis: 1_000,
                device_type: crate::BrowserDeviceType::Mobile,
            },
            BrowserViewportMode::Fixed,
            at(10),
        )
        .unwrap();

    let durable = authority.durable_state();
    let encoded = serde_json::to_value(&durable).unwrap();
    let tab = encoded
        .get("tabs")
        .and_then(serde_json::Value::as_array)
        .and_then(|tabs| {
            tabs.iter().find(|tab| {
                tab.get("tab_id").and_then(serde_json::Value::as_str) == Some(tab_id.as_str())
            })
        })
        .expect("durable browser tab should be present");
    assert!(tab.get("url").is_some());
    assert!(tab.get("viewport").is_none());
    assert!(tab.get("viewport_mode").is_none());
    assert!(tab.get("frame_sequence").is_none());

    let restored = BrowserAuthority::restore_durable(durable, at(20)).unwrap();
    let restored_tab = restored.tab(&tab_id).expect("browser tab should restore");
    assert_eq!(restored_tab.viewport, BrowserViewport::default());
    assert_eq!(restored_tab.viewport_mode, BrowserViewportMode::Auto);
    assert_eq!(restored_tab.frame_sequence, 0);
}

#[test]
fn chromium_error_pages_are_normalized_before_runtime_and_durable_restore() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let browser_session_id = create_ready_session(
        &mut authority,
        "browser-session-error-page",
        "session-error-page",
    );
    let tab_id = create_ready_tab(&mut authority, &browser_session_id, "tab-error-page");

    let updated = authority
        .apply_host_page_state(
            &tab_id,
            1,
            "chrome-error://chromewebdata/".to_string(),
            None,
            "无法访问此网站".to_string(),
            at(10),
        )
        .unwrap();
    assert_eq!(updated.url, "about:blank");
    assert_eq!(updated.origin, None);
    assert_eq!(updated.title, "");

    let mut durable = authority.durable_state();
    assert_eq!(durable.tabs[0].url, "about:blank");
    assert_eq!(durable.tabs[0].origin, None);
    assert_eq!(durable.tabs[0].title, "");
    durable.tabs[0].url = "chrome-error://chromewebdata/".to_string();
    durable.tabs[0].origin = Some("chrome-error://chromewebdata".to_string());
    durable.tabs[0].title = "无法访问此网站".to_string();
    let restored = BrowserAuthority::restore_durable(durable, at(20)).unwrap();
    let restored_tab = restored.tab(&tab_id).unwrap();
    assert_eq!(restored_tab.url, "about:blank");
    assert_eq!(restored_tab.origin, None);
    assert_eq!(restored_tab.title, "");
}

#[test]
fn viewport_runtime_state_is_scoped_to_each_browser_tab() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let browser_session_id = create_ready_session(
        &mut authority,
        "browser-session-viewport-scope",
        "session-viewport-scope",
    );
    let narrow_tab_id = create_ready_tab(
        &mut authority,
        &browser_session_id,
        "tab-viewport-scope-narrow",
    );
    let wide_tab_id = create_ready_tab(
        &mut authority,
        &browser_session_id,
        "tab-viewport-scope-wide",
    );

    authority
        .set_tab_viewport(
            &narrow_tab_id,
            BrowserViewport {
                width: 390,
                height: 844,
                device_scale_factor_millis: 1_000,
                device_type: crate::BrowserDeviceType::Mobile,
            },
            BrowserViewportMode::Fixed,
            at(10),
        )
        .unwrap();
    authority
        .set_tab_viewport(
            &wide_tab_id,
            BrowserViewport {
                width: 1_280,
                height: 800,
                device_scale_factor_millis: 1_000,
                device_type: crate::BrowserDeviceType::Desktop,
            },
            BrowserViewportMode::Auto,
            at(11),
        )
        .unwrap();

    assert_eq!(authority.tab(&narrow_tab_id).unwrap().viewport.width, 390);
    assert_eq!(authority.tab(&wide_tab_id).unwrap().viewport.width, 1_280);
    assert_eq!(
        authority.tab(&narrow_tab_id).unwrap().viewport_mode,
        BrowserViewportMode::Fixed
    );
    assert_eq!(
        authority.tab(&wide_tab_id).unwrap().viewport_mode,
        BrowserViewportMode::Auto
    );
}

#[test]
fn viewport_device_semantics_are_only_wide_desktop_or_narrow_mobile() {
    assert_eq!(
        crate::BrowserDeviceType::for_dimensions(600),
        crate::BrowserDeviceType::Mobile
    );
    assert_eq!(
        crate::BrowserDeviceType::for_dimensions(601),
        crate::BrowserDeviceType::Desktop
    );

    let restored: BrowserViewport = serde_json::from_value(serde_json::json!({
        "width": 768,
        "height": 1024,
        "device_scale_factor_millis": 1000,
        "device_type": "tablet"
    }))
    .unwrap();
    assert_eq!(
        restored.device_type,
        crate::BrowserDeviceType::Desktop,
        "旧平板状态必须收敛到唯一的宽屏语义"
    );
}

#[test]
fn annotations_are_authoritative_revision_bound_and_durable() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let browser_session_id = create_ready_session(
        &mut authority,
        "browser-session-annotation",
        "session-annotation",
    );
    let tab_id = create_ready_tab(&mut authority, &browser_session_id, "tab-annotation");
    let now = at(10);
    let annotation = BrowserAnnotation {
        annotation_id: BrowserAnnotationId::new("annotation-1"),
        browser_session_id: browser_session_id.clone(),
        tab_id: tab_id.clone(),
        sequence: 0,
        author: BrowserAnnotationAuthor::User,
        kind: BrowserAnnotationKind::Region,
        anchor: BrowserAnnotationAnchor::Region(BrowserRegionAnnotationAnchor {
            url: "https://example.com/".to_string(),
            origin: Some("https://example.com".to_string()),
            viewport: BrowserViewport::default(),
            scroll_x: 0.0,
            scroll_y: 0.0,
            rect: BrowserNormalizedRect {
                x: 0.1,
                y: 0.1,
                width: 0.2,
                height: 0.1,
            },
            snapshot_revision: 0,
        }),
        comment: "检查标题".to_string(),
        status: BrowserAnnotationStatus::Active,
        screenshot_artifact_id: Some("session-annotation/annotation-1.png".to_string()),
        created_at: now,
        updated_at: now,
    };
    let created = authority.create_annotation(annotation).unwrap();
    assert_eq!(created.sequence, 1);
    assert_eq!(authority.annotations_for_tab(&tab_id).len(), 1);
    let updated = authority
        .update_annotation_comment(
            &BrowserAnnotationId::new("annotation-1"),
            "检查标题和间距".to_string(),
            at(11),
        )
        .unwrap();
    assert_eq!(updated.comment, "检查标题和间距");
    authority
        .set_tab_viewport(
            &tab_id,
            BrowserViewport {
                width: 720,
                height: 540,
                device_scale_factor_millis: 1_000,
                device_type: crate::BrowserDeviceType::Desktop,
            },
            BrowserViewportMode::Fixed,
            at(12),
        )
        .unwrap();
    assert_eq!(
        authority.annotations_for_tab(&tab_id)[0].status,
        BrowserAnnotationStatus::Active,
        "视口尺寸变化不能使同一文档上的批注失效"
    );
    authority
        .apply_host_page_state(
            &tab_id,
            1,
            "https://example.com/next".to_string(),
            Some("https://example.com".to_string()),
            "Next".to_string(),
            at(13),
        )
        .unwrap();
    assert_eq!(
        authority.annotations_for_tab(&tab_id)[0].status,
        BrowserAnnotationStatus::Stale
    );
    let durable = authority.durable_state();
    let mut restored = BrowserAuthority::restore_durable(durable, at(20)).unwrap();
    restored
        .transition_session(&browser_session_id, BrowserSessionLifecycle::Ready, at(21))
        .unwrap();
    restored
        .transition_tab(&tab_id, BrowserTabLifecycle::Ready, at(21))
        .unwrap();
    let restored_annotation = restored
        .annotations_for_tab(&tab_id)
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(restored_annotation.comment, "检查标题和间距");
    assert_eq!(restored_annotation.status, BrowserAnnotationStatus::Stale);
    assert_eq!(
        restored_annotation.screenshot_artifact_id.as_deref(),
        Some("session-annotation/annotation-1.png"),
        "服务恢复只能使实时锚点失效，持久化截图引用必须继续可读"
    );

    let stale_anchor = match &restored_annotation.anchor {
        BrowserAnnotationAnchor::Region(anchor) => {
            BrowserAnnotationAnchor::Region(BrowserRegionAnnotationAnchor {
                url: anchor.url.clone(),
                origin: anchor.origin.clone(),
                viewport: anchor.viewport,
                scroll_x: anchor.scroll_x,
                scroll_y: anchor.scroll_y,
                rect: anchor.rect,
                snapshot_revision: 99,
            })
        }
        BrowserAnnotationAnchor::Element(_) => unreachable!(),
    };
    let stale = BrowserAnnotation {
        annotation_id: BrowserAnnotationId::new("annotation-stale"),
        anchor: stale_anchor,
        ..restored_annotation.clone()
    };
    let result = restored.create_annotation(stale);
    assert!(
        matches!(
            &result,
            Err(BrowserAuthorityError::SnapshotRevisionMismatch { .. })
        ),
        "unexpected annotation result: {result:?}"
    );
}

#[test]
fn legacy_durable_state_restores_every_tab_to_parent_owned_auto_viewport() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let browser_session_id = create_ready_session(
        &mut authority,
        "browser-session-device-migration",
        "session-device-migration",
    );
    let fixed_tab_id = create_ready_tab(
        &mut authority,
        &browser_session_id,
        "tab-device-migration-fixed",
    );
    authority
        .set_tab_viewport(
            &fixed_tab_id,
            BrowserViewport {
                width: 390,
                height: 844,
                device_scale_factor_millis: 1_000,
                device_type: crate::BrowserDeviceType::Desktop,
            },
            BrowserViewportMode::Fixed,
            at(10),
        )
        .unwrap();
    let auto_tab_id = create_ready_tab(
        &mut authority,
        &browser_session_id,
        "tab-device-migration-auto",
    );
    authority
        .set_tab_viewport(
            &auto_tab_id,
            BrowserViewport {
                width: 390,
                height: 844,
                device_scale_factor_millis: 1_000,
                device_type: crate::BrowserDeviceType::Desktop,
            },
            BrowserViewportMode::Auto,
            at(11),
        )
        .unwrap();

    let mut legacy = authority.durable_state();
    legacy.schema_version = 1;
    let restored = BrowserAuthority::restore_durable(legacy, at(20)).unwrap();

    assert_eq!(
        restored.tab(&fixed_tab_id).unwrap().viewport.device_type,
        crate::BrowserDeviceType::Desktop
    );
    assert_eq!(
        restored.tab(&auto_tab_id).unwrap().viewport.device_type,
        crate::BrowserDeviceType::Desktop
    );
    assert_eq!(
        restored.tab(&fixed_tab_id).unwrap().viewport_mode,
        BrowserViewportMode::Auto
    );
    assert_eq!(
        restored.tab(&auto_tab_id).unwrap().viewport_mode,
        BrowserViewportMode::Auto
    );
    assert_eq!(
        restored.durable_state().schema_version,
        crate::BROWSER_DURABLE_STATE_SCHEMA_VERSION
    );
}

#[test]
fn failed_browser_sessions_are_closed_during_restore() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let browser_session_id = create_ready_session(
        &mut authority,
        "browser-session-failed-restore",
        "session-failed-restore",
    );
    let tab_id = create_ready_tab(&mut authority, &browser_session_id, "tab-failed-restore");
    authority
        .transition_session(&browser_session_id, BrowserSessionLifecycle::Failed, at(6))
        .unwrap();

    let restored = BrowserAuthority::restore_durable(authority.durable_state(), at(20)).unwrap();
    let session = restored.session(&browser_session_id).unwrap();
    let tab = restored.tab(&tab_id).unwrap();

    assert_eq!(session.lifecycle, BrowserSessionLifecycle::Closed);
    assert_eq!(tab.lifecycle, BrowserTabLifecycle::Closed);
    assert!(session.tab_ids.is_empty());
    assert!(!session.lifecycle.is_recoverable());
}

#[test]
fn schema_two_tabs_restore_as_suspended_without_remaining_open_records() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let browser_session_id = create_ready_session(
        &mut authority,
        "browser-session-schema-two",
        "session-schema-two",
    );
    let tab_id = create_ready_tab(&mut authority, &browser_session_id, "tab-schema-two");
    let mut durable = authority.durable_state();
    durable.schema_version = 2;

    let restored = BrowserAuthority::restore_durable(durable, at(20)).unwrap();

    assert_eq!(
        restored.session(&browser_session_id).unwrap().lifecycle,
        BrowserSessionLifecycle::Recovering
    );
    assert_eq!(
        restored.tab(&tab_id).unwrap().lifecycle,
        BrowserTabLifecycle::Suspended
    );
    assert_eq!(
        restored.durable_state().schema_version,
        crate::BROWSER_DURABLE_STATE_SCHEMA_VERSION
    );
}

#[test]
fn crashed_tabs_do_not_consume_global_tab_quota() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let first_session =
        create_ready_session(&mut authority, "browser-session-quota-a", "session-quota-a");
    let second_session =
        create_ready_session(&mut authority, "browser-session-quota-b", "session-quota-b");

    let mut first_tab = None;
    for index in 0..32 {
        let tab_id = create_ready_tab(
            &mut authority,
            &first_session,
            &format!("tab-quota-a-{index}"),
        );
        first_tab.get_or_insert(tab_id);
    }
    for index in 0..32 {
        create_ready_tab(
            &mut authority,
            &second_session,
            &format!("tab-quota-b-{index}"),
        );
    }

    authority
        .transition_tab(
            &first_tab.expect("the first session must contain a tab"),
            BrowserTabLifecycle::Crashed,
            at(10),
        )
        .unwrap();
    let third_session =
        create_ready_session(&mut authority, "browser-session-quota-c", "session-quota-c");

    let replacement = create_ready_tab(&mut authority, &third_session, "tab-quota-replacement");
    assert_eq!(
        authority.tab(&replacement).unwrap().lifecycle,
        BrowserTabLifecycle::Ready
    );
}

#[test]
fn suspended_tabs_invalidate_page_references_before_materialization() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let browser_session_id = create_ready_session(
        &mut authority,
        "browser-session-suspended-quota",
        "session-suspended-quota",
    );
    let tab_id = create_ready_tab(&mut authority, &browser_session_id, "tab-suspended-quota");
    let before = authority.tab(&tab_id).unwrap().clone();

    let suspended = authority
        .transition_tab(&tab_id, BrowserTabLifecycle::Suspended, at(10))
        .unwrap();

    assert_eq!(suspended.lifecycle, BrowserTabLifecycle::Suspended);
    assert_eq!(
        suspended.navigation_revision,
        before.navigation_revision.saturating_add(1)
    );
    assert_eq!(
        suspended.snapshot_revision,
        before.snapshot_revision.saturating_add(1)
    );
    assert_eq!(suspended.frame_sequence, 0);
    let restored = authority
        .transition_tab(&tab_id, BrowserTabLifecycle::Ready, at(11))
        .unwrap();
    assert_eq!(restored.lifecycle, BrowserTabLifecycle::Ready);
}

#[test]
fn interrupted_sessions_restore_for_the_next_runtime() {
    let mut authority = BrowserAuthority::new();
    register_default_profile(&mut authority);
    let browser_session_id = create_ready_session(
        &mut authority,
        "browser-session-interrupted",
        "session-interrupted",
    );
    let tab_id = create_ready_tab(&mut authority, &browser_session_id, "tab-interrupted");
    authority
        .transition_session(
            &browser_session_id,
            BrowserSessionLifecycle::Interrupted,
            at(6),
        )
        .unwrap();

    let restored = BrowserAuthority::restore_durable(authority.durable_state(), at(20)).unwrap();
    let session = restored.session(&browser_session_id).unwrap();
    let tab = restored.tab(&tab_id).unwrap();

    assert_eq!(session.lifecycle, BrowserSessionLifecycle::Recovering);
    assert_eq!(tab.lifecycle, BrowserTabLifecycle::Suspended);
    assert!(session.lifecycle.is_recoverable());
}

#[test]
fn unusable_runtime_hides_every_browser_tool() {
    let capability = BrowserCapabilitySnapshot {
        revision: 1,
        in_app_browser_enabled: true,
        browser_use_enabled: true,
        runtime_status: BrowserRuntimeComponentStatus::UpdateRequired,
        host_protocol_compatible: true,
        access_profile: AccessProfile::FullAccess,
    };
    assert!(capability.visible_tools().is_empty());
    assert_eq!(
        capability.unavailable_reason(),
        Some(BrowserCapabilityUnavailableReason::RuntimeUpdateRequired)
    );
}

fn version(value: &str) -> Version {
    Version::parse(value).unwrap()
}

fn sha256_bytes(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn runtime_manager(root: PathBuf, signing_key: &SigningKey) -> BrowserRuntimeManager {
    BrowserRuntimeManager::new(BrowserRuntimeManagerConfig {
        root,
        target: BrowserRuntimeTarget {
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
        },
        channel: BrowserRuntimeReleaseChannel::Stable,
        magi_version: version("3.0.37"),
        host_protocol_version: crate::BrowserHostProtocolVersion::CURRENT,
        trusted_release_key: signing_key.verifying_key().to_bytes(),
        max_archive_size_bytes: 2 * 1024 * 1024,
        max_unpacked_size_bytes: 4 * 1024 * 1024,
    })
}

fn create_signed_runtime_archive(
    directory: &std::path::Path,
    signing_key: &SigningKey,
    runtime_version: &str,
    manifest_sequence: u64,
) -> (PathBuf, SignedBrowserRuntimeRelease) {
    let node = b"#!/bin/sh\nexit 0\n";
    let host = b"console.log('host');\n";
    let manifest = BrowserRuntimeManifest {
        format_version: crate::BROWSER_RUNTIME_MANIFEST_FORMAT_VERSION,
        runtime_version: version(runtime_version),
        host_version: version("1.0.0"),
        host_protocol: crate::BrowserHostProtocolRange {
            minimum: crate::BrowserHostProtocolVersion::CURRENT,
            maximum: crate::BrowserHostProtocolVersion::CURRENT,
        },
        node_version: version("22.18.0"),
        playwright_version: version("1.55.0"),
        chromium_version: "140.0.7339.16".to_string(),
        target: BrowserRuntimeTarget {
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
        },
        channel: BrowserRuntimeReleaseChannel::Stable,
        manifest_sequence,
        released_at: at(10),
        expires_at: at(10_000),
        minimum_magi_version: version("3.0.0"),
        minimum_safe_runtime_version: version(runtime_version),
        unpacked_size_bytes: (node.len() + host.len()) as u64,
        node_executable_path: "bin/magi-browser-node".to_string(),
        host_entry_path: "host/index.cjs".to_string(),
        chromium_executable_path: "bin/magi-browser-node".to_string(),
        files: vec![
            BrowserRuntimeFile {
                path: "bin/magi-browser-node".to_string(),
                sha256: sha256_bytes(node),
                size_bytes: node.len() as u64,
                executable: true,
                symlink_target: None,
            },
            BrowserRuntimeFile {
                path: "host/index.cjs".to_string(),
                sha256: sha256_bytes(host),
                size_bytes: host.len() as u64,
                executable: false,
                symlink_target: None,
            },
        ],
    };
    let archive_path = directory.join(format!("runtime-{runtime_version}.tar.zst"));
    let archive_file = fs::File::create(&archive_path).unwrap();
    let encoder = zstd::stream::write::Encoder::new(archive_file, 1).unwrap();
    let mut archive = tar::Builder::new(encoder);
    append_archive_file(
        &mut archive,
        crate::BROWSER_RUNTIME_MANIFEST_FILE,
        &serde_json::to_vec_pretty(&manifest).unwrap(),
        0o644,
    );
    append_archive_file(&mut archive, "bin/magi-browser-node", node, 0o755);
    append_archive_file(&mut archive, "host/index.cjs", host, 0o644);
    let encoder = archive.into_inner().unwrap();
    encoder.finish().unwrap();

    let archive_bytes = fs::read(&archive_path).unwrap();
    let mut release = SignedBrowserRuntimeRelease {
        manifest,
        update_level: BrowserRuntimeUpdateLevel::Optional,
        archive_sha256: sha256_bytes(&archive_bytes),
        archive_size_bytes: archive_bytes.len() as u64,
        signature: String::new(),
    };
    let signature = signing_key.sign(&release.signing_bytes().unwrap());
    release.signature = BASE64_STANDARD.encode(signature.to_bytes());
    (archive_path, release)
}

fn append_archive_file<W: std::io::Write>(
    archive: &mut tar::Builder<W>,
    path: &str,
    bytes: &[u8],
    mode: u32,
) {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(mode);
    header.set_cksum();
    archive
        .append_data(&mut header, path, Cursor::new(bytes))
        .unwrap();
}

#[test]
fn signed_runtime_is_verified_installed_and_atomically_activated() {
    let directory = tempfile::tempdir().unwrap();
    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let manager = runtime_manager(directory.path().join("browser"), &signing_key);
    let (archive_path, release) =
        create_signed_runtime_archive(directory.path(), &signing_key, "1.0.0", 10);

    let outcome = manager
        .install_archive(
            &release,
            &archive_path,
            at(100),
            &|root: &std::path::Path, _: &BrowserRuntimeManifest| {
                root.join("host/index.cjs")
                    .is_file()
                    .then_some(())
                    .ok_or_else(|| "host entrypoint is missing".to_string())
            },
        )
        .unwrap();

    assert_eq!(outcome.active.runtime_version, version("1.0.0"));
    assert!(outcome.install_path.join("bin/magi-browser-node").is_file());
    assert_eq!(manager.active().unwrap(), Some(outcome.active));
    assert_eq!(
        manager.inspect_active_release(at(20_000)).unwrap(),
        Some(release)
    );
}

#[test]
fn same_version_release_replaces_stale_install_and_reactivates() {
    let directory = tempfile::tempdir().unwrap();
    let signing_key = SigningKey::from_bytes(&[31u8; 32]);
    let manager = runtime_manager(directory.path().join("browser"), &signing_key);
    let (current_archive, current_release) =
        create_signed_runtime_archive(directory.path(), &signing_key, "1.0.0", 10);
    manager
        .install_archive(
            &current_release,
            &current_archive,
            at(100),
            &runtime_self_test_ok,
        )
        .unwrap();

    let (next_archive, next_release) =
        create_signed_runtime_archive(directory.path(), &signing_key, "1.0.0", 11);
    let assessment = manager.assess_release(&next_release, at(101)).unwrap();
    assert!(assessment.requires_install);

    let outcome = manager
        .install_archive(&next_release, &next_archive, at(102), &runtime_self_test_ok)
        .unwrap();
    assert_eq!(outcome.active.manifest_sequence, 11);
    assert_eq!(
        manager.inspect_active_release(at(20_000)).unwrap(),
        Some(next_release)
    );
    assert!(!manager.root().read_dir().unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".replacing-")
    }));
}

#[test]
fn runtime_uninstall_removes_payload_but_preserves_rollback_trust() {
    let root = tempfile::tempdir().unwrap();
    let signing_key = SigningKey::from_bytes(&[23; 32]);
    let manager = runtime_manager(root.path().join("runtime"), &signing_key);
    let (archive, release) = create_signed_runtime_archive(root.path(), &signing_key, "4.2.0", 11);
    manager
        .install_archive(&release, &archive, at(100), &runtime_self_test_ok)
        .unwrap();

    assert!(manager.uninstall().unwrap());
    assert!(manager.active().unwrap().is_none());
    assert!(!manager.runtime_path(&version("4.2.0")).exists());
    assert_eq!(manager.trust_state().unwrap().highest_manifest_sequence, 11);
    assert!(!manager.uninstall().unwrap());
}

fn runtime_self_test_ok(_: &std::path::Path, _: &BrowserRuntimeManifest) -> Result<(), String> {
    Ok(())
}

#[test]
fn runtime_signature_failure_never_creates_active_state() {
    let directory = tempfile::tempdir().unwrap();
    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let wrong_key = SigningKey::from_bytes(&[8u8; 32]);
    let manager = runtime_manager(directory.path().join("browser"), &wrong_key);
    let (archive_path, release) =
        create_signed_runtime_archive(directory.path(), &signing_key, "1.0.0", 10);

    let error = manager
        .install_archive(
            &release,
            &archive_path,
            at(100),
            &|_: &std::path::Path, _: &BrowserRuntimeManifest| Ok(()),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        BrowserRuntimeComponentError::InvalidSignature
    ));
    assert!(manager.active().unwrap().is_none());
}

#[test]
fn runtime_update_check_reports_newer_magi_requirement_without_rejecting_feed() {
    let directory = tempfile::tempdir().unwrap();
    let signing_key = SigningKey::from_bytes(&[41u8; 32]);
    let manager = runtime_manager(directory.path().join("browser"), &signing_key);
    let (archive_path, mut release) =
        create_signed_runtime_archive(directory.path(), &signing_key, "4.0.0", 40);
    release.manifest.minimum_magi_version = version("4.0.0");
    release.manifest.host_protocol = crate::BrowserHostProtocolRange {
        minimum: crate::BrowserHostProtocolVersion { major: 2, minor: 0 },
        maximum: crate::BrowserHostProtocolVersion { major: 2, minor: 0 },
    };
    release.signature = BASE64_STANDARD.encode(
        signing_key
            .sign(&release.signing_bytes().unwrap())
            .to_bytes(),
    );

    let assessment = manager.assess_release(&release, at(100)).unwrap();
    assert!(assessment.magi_update_required);
    assert_eq!(assessment.minimum_magi_version, version("4.0.0"));

    let error = manager
        .install_archive(&release, &archive_path, at(100), &runtime_self_test_ok)
        .unwrap_err();
    assert!(matches!(
        error,
        BrowserRuntimeComponentError::MagiVersionTooOld {
            minimum,
            current
        } if minimum == version("4.0.0") && current == version("3.0.37")
    ));
}

#[test]
fn runtime_manifest_sequence_cannot_move_backwards() {
    let directory = tempfile::tempdir().unwrap();
    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let manager = runtime_manager(directory.path().join("browser"), &signing_key);
    let (current_archive, current_release) =
        create_signed_runtime_archive(directory.path(), &signing_key, "2.0.0", 20);
    manager
        .install_archive(
            &current_release,
            &current_archive,
            at(100),
            &|_: &std::path::Path, _: &BrowserRuntimeManifest| Ok(()),
        )
        .unwrap();
    let (_, stale_release) =
        create_signed_runtime_archive(directory.path(), &signing_key, "1.0.0", 19);

    let error = manager.assess_release(&stale_release, at(101)).unwrap_err();
    assert!(matches!(
        error,
        BrowserRuntimeComponentError::ManifestReplay {
            accepted: 20,
            received: 19
        }
    ));
}

#[test]
fn runtime_manifest_sequence_migrates_past_legacy_date_based_trust() {
    let directory = tempfile::tempdir().unwrap();
    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let manager = runtime_manager(directory.path().join("browser"), &signing_key);
    let (legacy_archive, legacy_release) = create_signed_runtime_archive(
        directory.path(),
        &signing_key,
        "3.0.38-local.2",
        2_026_080_802,
    );
    manager
        .install_archive(
            &legacy_release,
            &legacy_archive,
            at(100),
            &runtime_self_test_ok,
        )
        .unwrap();
    manager.uninstall().unwrap();

    let (_, current_release) =
        create_signed_runtime_archive(directory.path(), &signing_key, "3.0.40", 3_000_000_000_040);
    let assessment = manager.assess_release(&current_release, at(101)).unwrap();

    assert_eq!(assessment.runtime_version, version("3.0.40"));
    assert!(assessment.requires_install);
}

#[test]
fn failed_runtime_self_test_leaves_no_install_or_active_pointer() {
    let directory = tempfile::tempdir().unwrap();
    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let manager = runtime_manager(directory.path().join("browser"), &signing_key);
    let (archive_path, release) =
        create_signed_runtime_archive(directory.path(), &signing_key, "1.0.0", 10);

    let error = manager
        .install_archive(
            &release,
            &archive_path,
            at(100),
            &|_: &std::path::Path, _: &BrowserRuntimeManifest| {
                Err("host handshake failed".to_string())
            },
        )
        .unwrap_err();
    assert!(matches!(
        error,
        BrowserRuntimeComponentError::SelfTestFailed(_)
    ));
    assert!(manager.active().unwrap().is_none());
    assert!(!manager.runtime_path(&version("1.0.0")).exists());
    let staging = manager.root().join(".staging");
    assert!(!staging.exists() || fs::read_dir(staging).unwrap().next().is_none());
}

#[test]
fn host_protocol_round_trip_keeps_fencing_credentials() {
    let request = crate::BrowserHostRequestEnvelope {
        request_id: BrowserCommandId::new("browser-command-1"),
        protocol_version: crate::BrowserHostProtocolVersion::CURRENT,
        command: crate::BrowserHostCommand::Click {
            tab_id: BrowserTabId::new("tab-a"),
            control: crate::BrowserHostControl::Agent {
                lease_id: BrowserLeaseId::new("lease-a"),
                fence: 42,
            },
            target: crate::BrowserSnapshotTarget {
                snapshot_revision: 9,
                element_ref: "e-17".to_string(),
            },
        },
    };

    let encoded = serde_json::to_vec(&request).unwrap();
    let decoded: crate::BrowserHostRequestEnvelope = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, request);
}
