use std::collections::HashMap;

use magi_core::{
    BrowserAnnotationId, BrowserLeaseId, BrowserProfileId, BrowserSessionId, BrowserTabId,
    ExecutionOwnership, SessionId, UtcMillis, WorkspaceId,
};
use serde::{Deserialize, Serialize};

use crate::{
    BrowserAnnotation, BrowserAnnotationAnchor, BrowserAnnotationStatus, BrowserAuthorityError,
    BrowserControlLease, BrowserDeviceType, BrowserLeaseEndReason, BrowserLeaseLifecycle,
    BrowserLeaseSelector, BrowserProfile, BrowserProfileControlMode, BrowserSession,
    BrowserSessionLifecycle, BrowserTab, BrowserTabLifecycle, BrowserViewport, BrowserViewportMode,
    GoalControlBinding,
};

#[derive(Clone, Debug)]
pub struct CreateBrowserSession {
    pub browser_session_id: BrowserSessionId,
    pub workspace_id: WorkspaceId,
    pub session_id: SessionId,
    pub profile_id: BrowserProfileId,
    pub now: UtcMillis,
}

#[derive(Clone, Debug)]
pub struct CreateBrowserTab {
    pub tab_id: BrowserTabId,
    pub browser_session_id: BrowserSessionId,
    pub url: String,
    pub viewport: BrowserViewport,
    pub now: UtcMillis,
}

#[derive(Clone, Debug)]
pub struct AcquireBrowserLease {
    pub lease_id: BrowserLeaseId,
    pub profile_id: BrowserProfileId,
    pub browser_session_id: BrowserSessionId,
    pub owner: ExecutionOwnership,
    pub turn_id: String,
    pub goal_binding: Option<GoalControlBinding>,
    pub acquired_at: UtcMillis,
    pub expires_at: UtcMillis,
}

#[derive(Clone, Debug)]
pub struct ValidateBrowserWrite<'a> {
    pub lease_id: &'a BrowserLeaseId,
    pub fence: u64,
    pub browser_session_id: &'a BrowserSessionId,
    pub tab_id: &'a BrowserTabId,
    pub owner: &'a ExecutionOwnership,
    pub turn_id: &'a str,
    pub goal_binding: Option<&'a GoalControlBinding>,
    pub now: UtcMillis,
}

#[derive(Clone, Debug)]
pub struct ValidatedBrowserWrite {
    pub owner: ExecutionOwnership,
    pub turn_id: String,
    pub goal_binding: Option<GoalControlBinding>,
    pub fence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserProfileControlSnapshot {
    pub profile_id: BrowserProfileId,
    pub mode: BrowserProfileControlMode,
    pub fence: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserAuthoritySnapshot {
    pub revision: u64,
    pub profiles: Vec<BrowserProfile>,
    pub sessions: Vec<BrowserSession>,
    pub tabs: Vec<BrowserTab>,
    pub leases: Vec<BrowserControlLease>,
    pub profile_controls: Vec<BrowserProfileControlSnapshot>,
    #[serde(default)]
    pub annotations: Vec<BrowserAnnotation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserDurableState {
    pub schema_version: u16,
    pub revision: u64,
    pub profiles: Vec<BrowserProfile>,
    pub sessions: Vec<BrowserSession>,
    pub tabs: Vec<BrowserTab>,
    #[serde(default)]
    pub annotations: Vec<BrowserAnnotation>,
}

pub const BROWSER_DURABLE_STATE_SCHEMA_VERSION: u16 = 2;
const LEGACY_BROWSER_DURABLE_STATE_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Default)]
pub struct BrowserAuthority {
    revision: u64,
    profiles: HashMap<BrowserProfileId, BrowserProfile>,
    sessions: HashMap<BrowserSessionId, BrowserSession>,
    tabs: HashMap<BrowserTabId, BrowserTab>,
    annotations: HashMap<BrowserAnnotationId, BrowserAnnotation>,
    leases: HashMap<BrowserLeaseId, BrowserControlLease>,
    active_profile_leases: HashMap<BrowserProfileId, BrowserLeaseId>,
    profile_controls: HashMap<BrowserProfileId, BrowserProfileControlMode>,
    profile_fences: HashMap<BrowserProfileId, u64>,
}

impl BrowserAuthority {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn register_profile(
        &mut self,
        profile: BrowserProfile,
    ) -> Result<(), BrowserAuthorityError> {
        if self.profiles.contains_key(&profile.profile_id) {
            return Err(BrowserAuthorityError::ProfileAlreadyExists(
                profile.profile_id,
            ));
        }
        self.profile_controls
            .insert(profile.profile_id.clone(), BrowserProfileControlMode::Agent);
        self.profile_fences.insert(profile.profile_id.clone(), 0);
        self.profiles.insert(profile.profile_id.clone(), profile);
        self.bump_revision();
        Ok(())
    }

    pub fn profile(&self, profile_id: &BrowserProfileId) -> Option<&BrowserProfile> {
        self.profiles.get(profile_id)
    }

    pub fn session(&self, browser_session_id: &BrowserSessionId) -> Option<&BrowserSession> {
        self.sessions.get(browser_session_id)
    }

    pub fn session_for_magi_session(&self, session_id: &SessionId) -> Option<&BrowserSession> {
        self.sessions
            .values()
            .find(|session| &session.session_id == session_id && session.lifecycle.is_open())
    }

    pub fn tab(&self, tab_id: &BrowserTabId) -> Option<&BrowserTab> {
        self.tabs.get(tab_id)
    }

    pub fn annotation(&self, annotation_id: &BrowserAnnotationId) -> Option<&BrowserAnnotation> {
        self.annotations.get(annotation_id)
    }

    pub fn annotations_for_tab(&self, tab_id: &BrowserTabId) -> Vec<BrowserAnnotation> {
        self.annotations
            .values()
            .filter(|annotation| &annotation.tab_id == tab_id)
            .cloned()
            .collect()
    }

    pub fn create_annotation(
        &mut self,
        mut annotation: BrowserAnnotation,
    ) -> Result<BrowserAnnotation, BrowserAuthorityError> {
        if self.annotations.contains_key(&annotation.annotation_id) {
            return Err(BrowserAuthorityError::AnnotationAlreadyExists(
                annotation.annotation_id,
            ));
        }
        let tab = self.require_ready_tab(&annotation.tab_id)?.clone();
        if tab.browser_session_id != annotation.browser_session_id {
            return Err(BrowserAuthorityError::AnnotationSessionMismatch {
                annotation_id: annotation.annotation_id,
                browser_session_id: annotation.browser_session_id,
            });
        }
        let snapshot_revision = annotation_snapshot_revision(&annotation.anchor);
        if snapshot_revision != tab.snapshot_revision {
            return Err(BrowserAuthorityError::SnapshotRevisionMismatch {
                expected: tab.snapshot_revision,
                provided: snapshot_revision,
            });
        }
        annotation.status = BrowserAnnotationStatus::Active;
        self.annotations
            .insert(annotation.annotation_id.clone(), annotation.clone());
        self.bump_revision();
        Ok(annotation)
    }

    pub fn update_annotation_status(
        &mut self,
        annotation_id: &BrowserAnnotationId,
        status: BrowserAnnotationStatus,
        now: UtcMillis,
    ) -> Result<BrowserAnnotation, BrowserAuthorityError> {
        let updated = {
            let annotation = self
                .annotations
                .get_mut(annotation_id)
                .ok_or_else(|| BrowserAuthorityError::UnknownAnnotation(annotation_id.clone()))?;
            if annotation.status == BrowserAnnotationStatus::Deleted {
                return Ok(annotation.clone());
            }
            annotation.status = status;
            annotation.updated_at = now;
            annotation.clone()
        };
        self.bump_revision();
        Ok(updated)
    }

    pub fn update_annotation_comment(
        &mut self,
        annotation_id: &BrowserAnnotationId,
        comment: String,
        now: UtcMillis,
    ) -> Result<BrowserAnnotation, BrowserAuthorityError> {
        let updated = {
            let annotation = self
                .annotations
                .get_mut(annotation_id)
                .ok_or_else(|| BrowserAuthorityError::UnknownAnnotation(annotation_id.clone()))?;
            annotation.comment = comment;
            annotation.updated_at = now;
            annotation.clone()
        };
        self.bump_revision();
        Ok(updated)
    }

    pub fn lease(&self, lease_id: &BrowserLeaseId) -> Option<&BrowserControlLease> {
        self.leases.get(lease_id)
    }

    pub fn active_lease_for_profile(
        &self,
        profile_id: &BrowserProfileId,
    ) -> Option<&BrowserControlLease> {
        self.active_profile_leases
            .get(profile_id)
            .and_then(|lease_id| self.leases.get(lease_id))
    }

    pub fn profile_control_mode(
        &self,
        profile_id: &BrowserProfileId,
    ) -> Result<BrowserProfileControlMode, BrowserAuthorityError> {
        self.require_profile(profile_id)?;
        Ok(self
            .profile_controls
            .get(profile_id)
            .copied()
            .unwrap_or_default())
    }

    pub fn profile_control_snapshot(
        &self,
        profile_id: &BrowserProfileId,
    ) -> Result<BrowserProfileControlSnapshot, BrowserAuthorityError> {
        self.require_profile(profile_id)?;
        Ok(BrowserProfileControlSnapshot {
            profile_id: profile_id.clone(),
            mode: self
                .profile_controls
                .get(profile_id)
                .copied()
                .unwrap_or_default(),
            fence: self
                .profile_fences
                .get(profile_id)
                .copied()
                .unwrap_or_default(),
        })
    }

    pub fn create_session(
        &mut self,
        input: CreateBrowserSession,
    ) -> Result<BrowserSession, BrowserAuthorityError> {
        self.require_profile(&input.profile_id)?;
        if self.sessions.contains_key(&input.browser_session_id) {
            return Err(BrowserAuthorityError::SessionAlreadyExists(
                input.browser_session_id,
            ));
        }
        if let Some(existing) = self.session_for_magi_session(&input.session_id) {
            return Err(BrowserAuthorityError::OpenSessionAlreadyExists {
                session_id: input.session_id,
                browser_session_id: existing.browser_session_id.clone(),
            });
        }
        let session = BrowserSession {
            browser_session_id: input.browser_session_id,
            workspace_id: input.workspace_id,
            session_id: input.session_id,
            profile_id: input.profile_id,
            lifecycle: BrowserSessionLifecycle::Creating,
            active_tab_id: None,
            tab_ids: Vec::new(),
            runtime_epoch: 0,
            revision: 1,
            created_at: input.now,
            updated_at: input.now,
        };
        self.sessions
            .insert(session.browser_session_id.clone(), session.clone());
        self.bump_revision();
        Ok(session)
    }

    pub fn transition_session(
        &mut self,
        browser_session_id: &BrowserSessionId,
        lifecycle: BrowserSessionLifecycle,
        now: UtcMillis,
    ) -> Result<BrowserSession, BrowserAuthorityError> {
        let current = self.require_session(browser_session_id)?.lifecycle;
        if !current.can_transition_to(lifecycle) {
            return Err(BrowserAuthorityError::InvalidSessionTransition {
                from: current,
                to: lifecycle,
            });
        }
        if current == lifecycle {
            return Ok(self.require_session(browser_session_id)?.clone());
        }
        if lifecycle == BrowserSessionLifecycle::Closed {
            self.close_session_resources(browser_session_id, now)?;
        } else if matches!(
            lifecycle,
            BrowserSessionLifecycle::Recovering | BrowserSessionLifecycle::Failed
        ) {
            self.revoke_leases(
                &BrowserLeaseSelector {
                    browser_session_id: Some(browser_session_id.clone()),
                    ..BrowserLeaseSelector::default()
                },
                BrowserLeaseEndReason::RuntimeUnavailable,
                now,
            );
        }
        let session = self
            .sessions
            .get_mut(browser_session_id)
            .expect("browser session was validated before mutation");
        session.lifecycle = lifecycle;
        if lifecycle == BrowserSessionLifecycle::Recovering {
            session.runtime_epoch = session.runtime_epoch.saturating_add(1);
        }
        session.revision = session.revision.saturating_add(1);
        session.updated_at = now;
        let session = session.clone();
        self.bump_revision();
        Ok(session)
    }

    pub fn create_tab(
        &mut self,
        input: CreateBrowserTab,
    ) -> Result<BrowserTab, BrowserAuthorityError> {
        if self.tabs.contains_key(&input.tab_id) {
            return Err(BrowserAuthorityError::TabAlreadyExists(input.tab_id));
        }
        self.require_ready_session(&input.browser_session_id)?;
        let tab = BrowserTab {
            tab_id: input.tab_id,
            browser_session_id: input.browser_session_id.clone(),
            lifecycle: BrowserTabLifecycle::Creating,
            url: input.url,
            origin: None,
            title: String::new(),
            viewport: input.viewport,
            viewport_mode: BrowserViewportMode::Auto,
            navigation_revision: 0,
            snapshot_revision: 0,
            frame_sequence: 0,
            created_at: input.now,
            updated_at: input.now,
        };
        self.tabs.insert(tab.tab_id.clone(), tab.clone());
        let session = self
            .sessions
            .get_mut(&input.browser_session_id)
            .expect("browser session was validated before mutation");
        session.tab_ids.push(tab.tab_id.clone());
        session.active_tab_id = Some(tab.tab_id.clone());
        session.revision = session.revision.saturating_add(1);
        session.updated_at = input.now;
        self.bump_revision();
        Ok(tab)
    }

    pub fn transition_tab(
        &mut self,
        tab_id: &BrowserTabId,
        lifecycle: BrowserTabLifecycle,
        now: UtcMillis,
    ) -> Result<BrowserTab, BrowserAuthorityError> {
        let current = self.require_tab(tab_id)?.lifecycle;
        if !current.can_transition_to(lifecycle) {
            return Err(BrowserAuthorityError::InvalidTabTransition {
                from: current,
                to: lifecycle,
            });
        }
        if current == lifecycle {
            return Ok(self.require_tab(tab_id)?.clone());
        }
        let browser_session_id = self.require_tab(tab_id)?.browser_session_id.clone();
        let tab = self
            .tabs
            .get_mut(tab_id)
            .expect("browser tab was validated before mutation");
        tab.lifecycle = lifecycle;
        tab.updated_at = now;
        let tab = tab.clone();
        if lifecycle == BrowserTabLifecycle::Closed {
            let session = self
                .sessions
                .get_mut(&browser_session_id)
                .expect("browser tab cannot outlive its owning session");
            session.tab_ids.retain(|candidate| candidate != tab_id);
            if session.active_tab_id.as_ref() == Some(tab_id) {
                session.active_tab_id = session.tab_ids.last().cloned();
            }
            session.revision = session.revision.saturating_add(1);
            session.updated_at = now;
        }
        self.bump_revision();
        Ok(tab)
    }

    pub fn set_active_tab(
        &mut self,
        browser_session_id: &BrowserSessionId,
        tab_id: &BrowserTabId,
        now: UtcMillis,
    ) -> Result<BrowserSession, BrowserAuthorityError> {
        self.require_ready_session(browser_session_id)?;
        let tab = self.require_ready_tab(tab_id)?;
        if &tab.browser_session_id != browser_session_id {
            return Err(BrowserAuthorityError::TabSessionMismatch {
                tab_id: tab_id.clone(),
                browser_session_id: browser_session_id.clone(),
            });
        }
        let session = self
            .sessions
            .get_mut(browser_session_id)
            .expect("browser session was validated before mutation");
        if session.active_tab_id.as_ref() != Some(tab_id) {
            session.active_tab_id = Some(tab_id.clone());
            session.revision = session.revision.saturating_add(1);
            session.updated_at = now;
            self.bump_revision();
        }
        Ok(self
            .sessions
            .get(browser_session_id)
            .expect("browser session remains available")
            .clone())
    }

    pub fn update_tab_document(
        &mut self,
        tab_id: &BrowserTabId,
        url: String,
        origin: Option<String>,
        title: String,
        now: UtcMillis,
    ) -> Result<BrowserTab, BrowserAuthorityError> {
        self.require_ready_tab(tab_id)?;
        let tab = self
            .tabs
            .get_mut(tab_id)
            .expect("browser tab was validated before mutation");
        tab.url = url;
        tab.origin = origin;
        tab.title = title;
        tab.navigation_revision = tab.navigation_revision.saturating_add(1);
        tab.snapshot_revision = tab.snapshot_revision.saturating_add(1);
        tab.updated_at = now;
        let tab = tab.clone();
        self.bump_revision();
        Ok(tab)
    }

    pub fn apply_host_page_state(
        &mut self,
        tab_id: &BrowserTabId,
        host_navigation_revision: u64,
        url: String,
        origin: Option<String>,
        title: String,
        now: UtcMillis,
    ) -> Result<BrowserTab, BrowserAuthorityError> {
        self.require_ready_tab(tab_id)?;
        let (tab, document_changed, changed) = {
            let tab = self
                .tabs
                .get_mut(tab_id)
                .expect("browser tab was validated before mutation");
            if host_navigation_revision < tab.navigation_revision {
                return Err(BrowserAuthorityError::NavigationRevisionRegression {
                    current: tab.navigation_revision,
                    received: host_navigation_revision,
                });
            }
            let document_changed = host_navigation_revision > tab.navigation_revision;
            let metadata_changed = tab.url != url || tab.origin != origin || tab.title != title;
            let changed = document_changed || metadata_changed;
            if changed {
                tab.url = url;
                tab.origin = origin;
                tab.title = title;
                tab.navigation_revision = host_navigation_revision;
                if document_changed {
                    tab.snapshot_revision = tab.snapshot_revision.saturating_add(1);
                }
                tab.updated_at = now;
            }
            (tab.clone(), document_changed, changed)
        };
        if document_changed {
            self.mark_active_annotations_stale(tab_id, now);
        }
        if changed {
            self.bump_revision();
            return Ok(tab);
        }
        Ok(tab)
    }

    pub fn set_tab_viewport(
        &mut self,
        tab_id: &BrowserTabId,
        viewport: BrowserViewport,
        mode: BrowserViewportMode,
        now: UtcMillis,
    ) -> Result<BrowserTab, BrowserAuthorityError> {
        self.require_ready_tab(tab_id)?;
        let (tab, changed) = {
            let tab = self
                .tabs
                .get_mut(tab_id)
                .expect("browser tab was validated before mutation");
            let viewport_changed = tab.viewport != viewport;
            let mode_changed = tab.viewport_mode != mode;
            if viewport_changed || mode_changed {
                tab.viewport = viewport;
                tab.viewport_mode = mode;
                if viewport_changed {
                    tab.snapshot_revision = tab.snapshot_revision.saturating_add(1);
                }
                tab.updated_at = now;
            }
            (tab.clone(), viewport_changed || mode_changed)
        };
        if changed {
            self.bump_revision();
        }
        Ok(tab)
    }

    pub fn record_snapshot(
        &mut self,
        tab_id: &BrowserTabId,
        now: UtcMillis,
    ) -> Result<u64, BrowserAuthorityError> {
        self.require_ready_tab(tab_id)?;
        let tab = self
            .tabs
            .get_mut(tab_id)
            .expect("browser tab was validated before mutation");
        tab.snapshot_revision = tab.snapshot_revision.saturating_add(1);
        tab.updated_at = now;
        let revision = tab.snapshot_revision;
        self.bump_revision();
        Ok(revision)
    }

    pub fn apply_host_snapshot_revision(
        &mut self,
        tab_id: &BrowserTabId,
        host_snapshot_revision: u64,
        now: UtcMillis,
    ) -> Result<u64, BrowserAuthorityError> {
        self.require_ready_tab(tab_id)?;
        let tab = self
            .tabs
            .get_mut(tab_id)
            .expect("browser tab was validated before mutation");
        if host_snapshot_revision < tab.snapshot_revision {
            return Err(BrowserAuthorityError::SnapshotRevisionMismatch {
                expected: tab.snapshot_revision,
                provided: host_snapshot_revision,
            });
        }
        let changed = host_snapshot_revision > tab.snapshot_revision;
        let revision = if changed {
            tab.snapshot_revision = host_snapshot_revision;
            tab.updated_at = now;
            host_snapshot_revision
        } else {
            tab.snapshot_revision
        };
        if changed {
            self.bump_revision();
        }
        Ok(revision)
    }

    pub fn record_frame(
        &mut self,
        tab_id: &BrowserTabId,
        frame_sequence: u64,
        now: UtcMillis,
    ) -> Result<(), BrowserAuthorityError> {
        self.require_ready_tab(tab_id)?;
        let tab = self
            .tabs
            .get_mut(tab_id)
            .expect("browser tab was validated before mutation");
        if frame_sequence > tab.frame_sequence {
            tab.frame_sequence = frame_sequence;
            tab.updated_at = now;
            self.bump_revision();
        }
        Ok(())
    }

    pub fn validate_snapshot_ref(
        &self,
        browser_session_id: &BrowserSessionId,
        tab_id: &BrowserTabId,
        snapshot_revision: u64,
    ) -> Result<(), BrowserAuthorityError> {
        self.require_ready_session(browser_session_id)?;
        let tab = self.require_ready_tab(tab_id)?;
        if &tab.browser_session_id != browser_session_id {
            return Err(BrowserAuthorityError::TabSessionMismatch {
                tab_id: tab_id.clone(),
                browser_session_id: browser_session_id.clone(),
            });
        }
        if tab.snapshot_revision != snapshot_revision {
            return Err(BrowserAuthorityError::SnapshotRevisionMismatch {
                expected: tab.snapshot_revision,
                provided: snapshot_revision,
            });
        }
        Ok(())
    }

    pub fn validate_frame_ref(
        &self,
        browser_session_id: &BrowserSessionId,
        tab_id: &BrowserTabId,
        frame_sequence: u64,
        navigation_revision: u64,
    ) -> Result<(), BrowserAuthorityError> {
        self.require_ready_session(browser_session_id)?;
        let tab = self.require_ready_tab(tab_id)?;
        if &tab.browser_session_id != browser_session_id {
            return Err(BrowserAuthorityError::TabSessionMismatch {
                tab_id: tab_id.clone(),
                browser_session_id: browser_session_id.clone(),
            });
        }
        if tab.frame_sequence != frame_sequence {
            return Err(BrowserAuthorityError::FrameSequenceMismatch {
                expected: tab.frame_sequence,
                provided: frame_sequence,
            });
        }
        if tab.navigation_revision != navigation_revision {
            return Err(BrowserAuthorityError::NavigationRevisionMismatch {
                expected: tab.navigation_revision,
                provided: navigation_revision,
            });
        }
        Ok(())
    }

    pub fn validate_navigation_ref(
        &self,
        browser_session_id: &BrowserSessionId,
        tab_id: &BrowserTabId,
        navigation_revision: u64,
    ) -> Result<(), BrowserAuthorityError> {
        self.require_ready_session(browser_session_id)?;
        let tab = self.require_ready_tab(tab_id)?;
        if &tab.browser_session_id != browser_session_id {
            return Err(BrowserAuthorityError::TabSessionMismatch {
                tab_id: tab_id.clone(),
                browser_session_id: browser_session_id.clone(),
            });
        }
        if tab.navigation_revision != navigation_revision {
            return Err(BrowserAuthorityError::NavigationRevisionMismatch {
                expected: tab.navigation_revision,
                provided: navigation_revision,
            });
        }
        Ok(())
    }

    pub fn acquire_lease(
        &mut self,
        input: AcquireBrowserLease,
    ) -> Result<BrowserControlLease, BrowserAuthorityError> {
        self.expire_leases(input.acquired_at);
        self.require_profile(&input.profile_id)?;
        if self.leases.contains_key(&input.lease_id) {
            return Err(BrowserAuthorityError::LeaseAlreadyExists(input.lease_id));
        }
        let session = self.require_ready_session(&input.browser_session_id)?;
        if session.profile_id != input.profile_id {
            return Err(BrowserAuthorityError::SessionProfileMismatch {
                browser_session_id: input.browser_session_id,
                profile_id: input.profile_id,
            });
        }
        validate_lease_owner(&input.owner, session)?;
        if input.turn_id.trim().is_empty() {
            return Err(BrowserAuthorityError::EmptyTurnId);
        }
        if input.expires_at <= input.acquired_at {
            return Err(BrowserAuthorityError::InvalidLeaseExpiry);
        }
        if self.profile_control_mode(&input.profile_id)? == BrowserProfileControlMode::User {
            return Err(BrowserAuthorityError::UserHasControl(input.profile_id));
        }
        if let Some(lease_id) = self.active_profile_leases.get(&input.profile_id) {
            return Err(BrowserAuthorityError::LeaseConflict {
                lease_id: lease_id.clone(),
            });
        }
        let fence = self.advance_profile_fence(&input.profile_id);
        let lease = BrowserControlLease {
            lease_id: input.lease_id,
            profile_id: input.profile_id.clone(),
            browser_session_id: input.browser_session_id,
            owner: input.owner,
            turn_id: input.turn_id,
            goal_binding: input.goal_binding,
            fence,
            lifecycle: BrowserLeaseLifecycle::Held,
            end_reason: None,
            acquired_at: input.acquired_at,
            expires_at: input.expires_at,
            ended_at: None,
        };
        self.active_profile_leases
            .insert(input.profile_id, lease.lease_id.clone());
        self.leases.insert(lease.lease_id.clone(), lease.clone());
        self.bump_revision();
        Ok(lease)
    }

    pub fn validate_write(
        &mut self,
        request: ValidateBrowserWrite<'_>,
    ) -> Result<ValidatedBrowserWrite, BrowserAuthorityError> {
        let lease = self
            .leases
            .get(request.lease_id)
            .cloned()
            .ok_or_else(|| BrowserAuthorityError::UnknownLease(request.lease_id.clone()))?;
        if lease.lifecycle != BrowserLeaseLifecycle::Held {
            return Err(BrowserAuthorityError::LeaseNotHeld(
                request.lease_id.clone(),
            ));
        }
        if request.now >= lease.expires_at {
            self.finish_lease(
                request.lease_id,
                BrowserLeaseLifecycle::Expired,
                BrowserLeaseEndReason::LeaseExpired,
                request.now,
            )?;
            return Err(BrowserAuthorityError::LeaseExpired(
                request.lease_id.clone(),
            ));
        }
        let current_fence = self
            .profile_fences
            .get(&lease.profile_id)
            .copied()
            .unwrap_or_default();
        if lease.fence != current_fence || request.fence != current_fence {
            return Err(BrowserAuthorityError::LeaseFenceMismatch {
                expected: current_fence,
                provided: request.fence,
            });
        }
        if &lease.browser_session_id != request.browser_session_id {
            return Err(BrowserAuthorityError::LeaseSessionMismatch {
                lease_id: request.lease_id.clone(),
            });
        }
        if lease.goal_binding.as_ref() != request.goal_binding {
            return Err(BrowserAuthorityError::GoalBindingMismatch);
        }
        if &lease.owner != request.owner {
            return Err(BrowserAuthorityError::LeaseOwnerMismatch);
        }
        if lease.turn_id != request.turn_id {
            return Err(BrowserAuthorityError::LeaseTurnMismatch);
        }
        self.require_ready_session(request.browser_session_id)?;
        let tab = self.require_ready_tab(request.tab_id)?;
        if &tab.browser_session_id != request.browser_session_id {
            return Err(BrowserAuthorityError::TabSessionMismatch {
                tab_id: request.tab_id.clone(),
                browser_session_id: request.browser_session_id.clone(),
            });
        }
        Ok(ValidatedBrowserWrite {
            owner: lease.owner,
            turn_id: lease.turn_id,
            goal_binding: lease.goal_binding,
            fence: lease.fence,
        })
    }

    pub fn release_lease(
        &mut self,
        lease_id: &BrowserLeaseId,
        now: UtcMillis,
    ) -> Result<BrowserControlLease, BrowserAuthorityError> {
        self.finish_lease(
            lease_id,
            BrowserLeaseLifecycle::Released,
            BrowserLeaseEndReason::OwnerReleased,
            now,
        )
    }

    pub fn revoke_lease(
        &mut self,
        lease_id: &BrowserLeaseId,
        reason: BrowserLeaseEndReason,
        now: UtcMillis,
    ) -> Result<BrowserControlLease, BrowserAuthorityError> {
        self.finish_lease(lease_id, BrowserLeaseLifecycle::Revoked, reason, now)
    }

    pub fn revoke_leases(
        &mut self,
        selector: &BrowserLeaseSelector,
        reason: BrowserLeaseEndReason,
        now: UtcMillis,
    ) -> Vec<BrowserControlLease> {
        let lease_ids = self
            .leases
            .values()
            .filter(|lease| {
                lease.lifecycle == BrowserLeaseLifecycle::Held && selector.matches(lease)
            })
            .map(|lease| lease.lease_id.clone())
            .collect::<Vec<_>>();
        lease_ids
            .into_iter()
            .filter_map(|lease_id| {
                self.finish_lease(&lease_id, BrowserLeaseLifecycle::Revoked, reason, now)
                    .ok()
            })
            .collect()
    }

    pub fn expire_leases(&mut self, now: UtcMillis) -> Vec<BrowserControlLease> {
        let expired = self
            .leases
            .values()
            .filter(|lease| {
                lease.lifecycle == BrowserLeaseLifecycle::Held && now >= lease.expires_at
            })
            .map(|lease| lease.lease_id.clone())
            .collect::<Vec<_>>();
        expired
            .into_iter()
            .filter_map(|lease_id| {
                self.finish_lease(
                    &lease_id,
                    BrowserLeaseLifecycle::Expired,
                    BrowserLeaseEndReason::LeaseExpired,
                    now,
                )
                .ok()
            })
            .collect()
    }

    pub fn take_user_control(
        &mut self,
        profile_id: &BrowserProfileId,
        now: UtcMillis,
    ) -> Result<Vec<BrowserControlLease>, BrowserAuthorityError> {
        self.require_profile(profile_id)?;
        let revoked = self.revoke_leases(
            &BrowserLeaseSelector {
                profile_id: Some(profile_id.clone()),
                ..BrowserLeaseSelector::default()
            },
            BrowserLeaseEndReason::UserTakeover,
            now,
        );
        if revoked.is_empty() {
            self.advance_profile_fence(profile_id);
        }
        self.profile_controls
            .insert(profile_id.clone(), BrowserProfileControlMode::User);
        self.bump_revision();
        Ok(revoked)
    }

    pub fn release_user_control(
        &mut self,
        profile_id: &BrowserProfileId,
    ) -> Result<(), BrowserAuthorityError> {
        self.require_profile(profile_id)?;
        if self.profile_control_mode(profile_id)? != BrowserProfileControlMode::Agent {
            self.advance_profile_fence(profile_id);
            self.profile_controls
                .insert(profile_id.clone(), BrowserProfileControlMode::Agent);
            self.bump_revision();
        }
        Ok(())
    }

    pub fn snapshot(&self) -> BrowserAuthoritySnapshot {
        BrowserAuthoritySnapshot {
            revision: self.revision,
            profiles: self.profiles.values().cloned().collect(),
            sessions: self.sessions.values().cloned().collect(),
            tabs: self.tabs.values().cloned().collect(),
            leases: self.leases.values().cloned().collect(),
            profile_controls: self
                .profiles
                .keys()
                .map(|profile_id| BrowserProfileControlSnapshot {
                    profile_id: profile_id.clone(),
                    mode: self
                        .profile_controls
                        .get(profile_id)
                        .copied()
                        .unwrap_or_default(),
                    fence: self
                        .profile_fences
                        .get(profile_id)
                        .copied()
                        .unwrap_or_default(),
                })
                .collect(),
            annotations: self.annotations.values().cloned().collect(),
        }
    }

    pub fn durable_state(&self) -> BrowserDurableState {
        let mut tabs = self.tabs.values().cloned().collect::<Vec<_>>();
        for tab in &mut tabs {
            tab.frame_sequence = 0;
        }
        BrowserDurableState {
            schema_version: BROWSER_DURABLE_STATE_SCHEMA_VERSION,
            revision: self.revision,
            profiles: self.profiles.values().cloned().collect(),
            sessions: self.sessions.values().cloned().collect(),
            tabs,
            annotations: self.annotations.values().cloned().collect(),
        }
    }

    pub fn restore_durable(
        mut state: BrowserDurableState,
        now: UtcMillis,
    ) -> Result<Self, BrowserAuthorityError> {
        if state.schema_version == LEGACY_BROWSER_DURABLE_STATE_SCHEMA_VERSION {
            for tab in &mut state.tabs {
                tab.viewport.device_type = if tab.viewport_mode == BrowserViewportMode::Fixed {
                    BrowserDeviceType::for_dimensions(tab.viewport.width)
                } else {
                    BrowserDeviceType::Desktop
                };
            }
            state.schema_version = BROWSER_DURABLE_STATE_SCHEMA_VERSION;
        }
        if state.schema_version != BROWSER_DURABLE_STATE_SCHEMA_VERSION {
            return Err(BrowserAuthorityError::InvalidSnapshot(format!(
                "unsupported browser durable state schema: {}",
                state.schema_version
            )));
        }
        Self::restore(
            BrowserAuthoritySnapshot {
                revision: state.revision,
                profiles: state.profiles,
                sessions: state.sessions,
                tabs: state.tabs,
                leases: Vec::new(),
                profile_controls: Vec::new(),
                annotations: state.annotations,
            },
            now,
        )
    }

    pub fn begin_runtime_recovery(&mut self, now: UtcMillis) -> Vec<BrowserSessionId> {
        self.revoke_leases(
            &BrowserLeaseSelector::default(),
            BrowserLeaseEndReason::RuntimeUnavailable,
            now,
        );
        let profile_ids = self.profiles.keys().cloned().collect::<Vec<_>>();
        for profile_id in profile_ids {
            self.advance_profile_fence(&profile_id);
            self.profile_controls
                .insert(profile_id, BrowserProfileControlMode::Agent);
        }
        let recovering_session_ids = self
            .sessions
            .values()
            .filter(|session| session.lifecycle.is_open())
            .map(|session| session.browser_session_id.clone())
            .collect::<Vec<_>>();
        for browser_session_id in &recovering_session_ids {
            if let Some(session) = self.sessions.get_mut(browser_session_id) {
                if session.lifecycle != BrowserSessionLifecycle::Recovering {
                    session.runtime_epoch = session.runtime_epoch.saturating_add(1);
                }
                session.lifecycle = BrowserSessionLifecycle::Recovering;
                session.revision = session.revision.saturating_add(1);
                session.updated_at = now;
                for tab_id in &session.tab_ids {
                    if let Some(tab) = self.tabs.get_mut(tab_id)
                        && tab.lifecycle != BrowserTabLifecycle::Closed
                    {
                        tab.lifecycle = BrowserTabLifecycle::Creating;
                        tab.navigation_revision = tab.navigation_revision.saturating_add(1);
                        tab.snapshot_revision = tab.snapshot_revision.saturating_add(1);
                        tab.frame_sequence = 0;
                        tab.updated_at = now;
                    }
                }
            }
        }
        for browser_session_id in &recovering_session_ids {
            let tab_ids = self
                .sessions
                .get(browser_session_id)
                .map(|session| session.tab_ids.clone())
                .unwrap_or_default();
            for tab_id in tab_ids {
                self.mark_active_annotations_stale(&tab_id, now);
            }
        }
        if !recovering_session_ids.is_empty() {
            self.bump_revision();
        }
        recovering_session_ids
    }

    pub fn restore(
        snapshot: BrowserAuthoritySnapshot,
        now: UtcMillis,
    ) -> Result<Self, BrowserAuthorityError> {
        let mut authority = Self::new();
        authority.revision = snapshot.revision;
        for profile in snapshot.profiles {
            if authority
                .profiles
                .insert(profile.profile_id.clone(), profile)
                .is_some()
            {
                return Err(BrowserAuthorityError::InvalidSnapshot(
                    "duplicate browser profile".to_string(),
                ));
            }
        }
        for control in snapshot.profile_controls {
            if !authority.profiles.contains_key(&control.profile_id) {
                return Err(BrowserAuthorityError::InvalidSnapshot(format!(
                    "profile control references unknown profile {}",
                    control.profile_id
                )));
            }
            authority
                .profile_controls
                .insert(control.profile_id.clone(), BrowserProfileControlMode::Agent);
            authority
                .profile_fences
                .insert(control.profile_id, control.fence.saturating_add(1));
        }
        for profile_id in authority.profiles.keys() {
            authority
                .profile_controls
                .entry(profile_id.clone())
                .or_insert(BrowserProfileControlMode::Agent);
            authority
                .profile_fences
                .entry(profile_id.clone())
                .or_insert(1);
        }
        for mut session in snapshot.sessions {
            if !authority.profiles.contains_key(&session.profile_id) {
                return Err(BrowserAuthorityError::InvalidSnapshot(format!(
                    "browser session {} references unknown profile {}",
                    session.browser_session_id, session.profile_id
                )));
            }
            if session.lifecycle.is_open() {
                session.lifecycle = BrowserSessionLifecycle::Recovering;
                session.runtime_epoch = session.runtime_epoch.saturating_add(1);
                session.revision = session.revision.saturating_add(1);
                session.updated_at = now;
            }
            if authority
                .sessions
                .insert(session.browser_session_id.clone(), session)
                .is_some()
            {
                return Err(BrowserAuthorityError::InvalidSnapshot(
                    "duplicate browser session".to_string(),
                ));
            }
        }
        for mut tab in snapshot.tabs {
            let Some(session) = authority.sessions.get(&tab.browser_session_id) else {
                return Err(BrowserAuthorityError::InvalidSnapshot(format!(
                    "browser tab {} references unknown session {}",
                    tab.tab_id, tab.browser_session_id
                )));
            };
            if session.tab_ids.contains(&tab.tab_id)
                != (tab.lifecycle != BrowserTabLifecycle::Closed)
            {
                return Err(BrowserAuthorityError::InvalidSnapshot(format!(
                    "browser tab {} membership does not match its lifecycle",
                    tab.tab_id
                )));
            }
            if session.lifecycle == BrowserSessionLifecycle::Recovering
                && tab.lifecycle != BrowserTabLifecycle::Closed
            {
                tab.lifecycle = BrowserTabLifecycle::Creating;
                tab.navigation_revision = tab.navigation_revision.saturating_add(1);
                tab.snapshot_revision = tab.snapshot_revision.saturating_add(1);
                tab.frame_sequence = 0;
                tab.updated_at = now;
            }
            if authority.tabs.insert(tab.tab_id.clone(), tab).is_some() {
                return Err(BrowserAuthorityError::InvalidSnapshot(
                    "duplicate browser tab".to_string(),
                ));
            }
        }
        for mut annotation in snapshot.annotations {
            if !authority
                .sessions
                .contains_key(&annotation.browser_session_id)
                || !authority.tabs.contains_key(&annotation.tab_id)
            {
                return Err(BrowserAuthorityError::InvalidSnapshot(format!(
                    "browser annotation {} references unknown state",
                    annotation.annotation_id
                )));
            }
            if authority
                .sessions
                .get(&annotation.browser_session_id)
                .is_some_and(|session| session.lifecycle == BrowserSessionLifecycle::Recovering)
                && annotation.status == BrowserAnnotationStatus::Active
            {
                annotation.status = BrowserAnnotationStatus::Stale;
                annotation.updated_at = now;
            }
            if authority
                .annotations
                .insert(annotation.annotation_id.clone(), annotation)
                .is_some()
            {
                return Err(BrowserAuthorityError::InvalidSnapshot(
                    "duplicate browser annotation".to_string(),
                ));
            }
        }
        validate_open_session_uniqueness(&authority.sessions)?;
        for mut lease in snapshot.leases {
            if !authority.profiles.contains_key(&lease.profile_id)
                || !authority.sessions.contains_key(&lease.browser_session_id)
            {
                return Err(BrowserAuthorityError::InvalidSnapshot(format!(
                    "browser lease {} references unknown state",
                    lease.lease_id
                )));
            }
            if lease.lifecycle == BrowserLeaseLifecycle::Held {
                lease.lifecycle = BrowserLeaseLifecycle::Revoked;
                lease.end_reason = Some(BrowserLeaseEndReason::RuntimeShutdown);
                lease.ended_at = Some(now);
            }
            if authority
                .leases
                .insert(lease.lease_id.clone(), lease)
                .is_some()
            {
                return Err(BrowserAuthorityError::InvalidSnapshot(
                    "duplicate browser lease".to_string(),
                ));
            }
        }
        authority.bump_revision();
        Ok(authority)
    }

    fn close_session_resources(
        &mut self,
        browser_session_id: &BrowserSessionId,
        now: UtcMillis,
    ) -> Result<(), BrowserAuthorityError> {
        let tab_ids = self.require_session(browser_session_id)?.tab_ids.clone();
        self.revoke_leases(
            &BrowserLeaseSelector {
                browser_session_id: Some(browser_session_id.clone()),
                ..BrowserLeaseSelector::default()
            },
            BrowserLeaseEndReason::SessionClosed,
            now,
        );
        for tab_id in tab_ids {
            if let Some(tab) = self.tabs.get_mut(&tab_id) {
                tab.lifecycle = BrowserTabLifecycle::Closed;
                tab.updated_at = now;
            }
        }
        let session = self
            .sessions
            .get_mut(browser_session_id)
            .expect("browser session was validated before mutation");
        session.tab_ids.clear();
        session.active_tab_id = None;
        Ok(())
    }

    fn finish_lease(
        &mut self,
        lease_id: &BrowserLeaseId,
        lifecycle: BrowserLeaseLifecycle,
        reason: BrowserLeaseEndReason,
        now: UtcMillis,
    ) -> Result<BrowserControlLease, BrowserAuthorityError> {
        let existing = self
            .leases
            .get(lease_id)
            .cloned()
            .ok_or_else(|| BrowserAuthorityError::UnknownLease(lease_id.clone()))?;
        if existing.lifecycle.is_terminal() {
            return Ok(existing);
        }
        let profile_id = existing.profile_id.clone();
        if self.active_profile_leases.get(&profile_id) == Some(lease_id) {
            self.active_profile_leases.remove(&profile_id);
        }
        self.advance_profile_fence(&profile_id);
        let lease = self
            .leases
            .get_mut(lease_id)
            .expect("browser lease was validated before mutation");
        lease.lifecycle = lifecycle;
        lease.end_reason = Some(reason);
        lease.ended_at = Some(now);
        let lease = lease.clone();
        self.bump_revision();
        Ok(lease)
    }

    fn advance_profile_fence(&mut self, profile_id: &BrowserProfileId) -> u64 {
        let fence = self.profile_fences.entry(profile_id.clone()).or_default();
        *fence = fence.saturating_add(1);
        *fence
    }

    fn mark_active_annotations_stale(&mut self, tab_id: &BrowserTabId, now: UtcMillis) {
        for annotation in self.annotations.values_mut().filter(|annotation| {
            &annotation.tab_id == tab_id && annotation.status == BrowserAnnotationStatus::Active
        }) {
            annotation.status = BrowserAnnotationStatus::Stale;
            annotation.updated_at = now;
        }
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    fn require_profile(
        &self,
        profile_id: &BrowserProfileId,
    ) -> Result<&BrowserProfile, BrowserAuthorityError> {
        self.profiles
            .get(profile_id)
            .ok_or_else(|| BrowserAuthorityError::UnknownProfile(profile_id.clone()))
    }

    fn require_session(
        &self,
        browser_session_id: &BrowserSessionId,
    ) -> Result<&BrowserSession, BrowserAuthorityError> {
        self.sessions
            .get(browser_session_id)
            .ok_or_else(|| BrowserAuthorityError::UnknownSession(browser_session_id.clone()))
    }

    fn require_ready_session(
        &self,
        browser_session_id: &BrowserSessionId,
    ) -> Result<&BrowserSession, BrowserAuthorityError> {
        let session = self.require_session(browser_session_id)?;
        if session.lifecycle != BrowserSessionLifecycle::Ready {
            return Err(BrowserAuthorityError::SessionNotReady {
                browser_session_id: browser_session_id.clone(),
                lifecycle: session.lifecycle,
            });
        }
        Ok(session)
    }

    fn require_tab(&self, tab_id: &BrowserTabId) -> Result<&BrowserTab, BrowserAuthorityError> {
        self.tabs
            .get(tab_id)
            .ok_or_else(|| BrowserAuthorityError::UnknownTab(tab_id.clone()))
    }

    fn require_ready_tab(
        &self,
        tab_id: &BrowserTabId,
    ) -> Result<&BrowserTab, BrowserAuthorityError> {
        let tab = self.require_tab(tab_id)?;
        if tab.lifecycle != BrowserTabLifecycle::Ready {
            return Err(BrowserAuthorityError::TabNotReady {
                tab_id: tab_id.clone(),
                lifecycle: tab.lifecycle,
            });
        }
        Ok(tab)
    }
}

fn annotation_snapshot_revision(anchor: &BrowserAnnotationAnchor) -> u64 {
    match anchor {
        BrowserAnnotationAnchor::Element(anchor) => anchor.snapshot_revision,
        BrowserAnnotationAnchor::Region(anchor) => anchor.snapshot_revision,
    }
}

fn validate_lease_owner(
    owner: &ExecutionOwnership,
    session: &BrowserSession,
) -> Result<(), BrowserAuthorityError> {
    let session_id =
        owner
            .session_id
            .as_ref()
            .ok_or(BrowserAuthorityError::MissingOwnershipField {
                field: "session_id",
            })?;
    if session_id != &session.session_id {
        return Err(BrowserAuthorityError::OwnershipMismatch {
            field: "session_id",
        });
    }
    let workspace_id =
        owner
            .workspace_id
            .as_ref()
            .ok_or(BrowserAuthorityError::MissingOwnershipField {
                field: "workspace_id",
            })?;
    if workspace_id != &session.workspace_id {
        return Err(BrowserAuthorityError::OwnershipMismatch {
            field: "workspace_id",
        });
    }
    Ok(())
}

fn validate_open_session_uniqueness(
    sessions: &HashMap<BrowserSessionId, BrowserSession>,
) -> Result<(), BrowserAuthorityError> {
    let mut seen = HashMap::<SessionId, BrowserSessionId>::new();
    for session in sessions
        .values()
        .filter(|session| session.lifecycle.is_open())
    {
        if let Some(existing) = seen.insert(
            session.session_id.clone(),
            session.browser_session_id.clone(),
        ) {
            return Err(BrowserAuthorityError::InvalidSnapshot(format!(
                "Magi session {} has multiple open browser sessions: {} and {}",
                session.session_id, existing, session.browser_session_id
            )));
        }
    }
    Ok(())
}
