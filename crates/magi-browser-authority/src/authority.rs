use std::collections::{HashMap, HashSet};

use magi_core::{
    BrowserAnnotationId, BrowserLeaseId, BrowserProfileId, BrowserSessionId, BrowserTabId,
    ExecutionOwnership, SessionId, UtcMillis, WorkspaceId,
};
use serde::{Deserialize, Serialize};

use crate::{
    BrowserAnnotation, BrowserAnnotationAnchor, BrowserAnnotationStatus, BrowserAuthorityError,
    BrowserControlLease, BrowserLeaseEndReason, BrowserLeaseLifecycle, BrowserLeaseSelector,
    BrowserProfile, BrowserSession, BrowserSessionLifecycle, BrowserSurfaceBinding, BrowserTab,
    BrowserTabLifecycle, GoalControlBinding, normalize_browser_page_state,
};

const MAX_BROWSER_TABS_PER_SESSION: usize = 32;
const MAX_BROWSER_TABS_TOTAL: usize = 64;

#[derive(Clone, Debug)]
pub struct CreateBrowserSession {
    pub browser_session_id: BrowserSessionId,
    pub workspace_id: Option<WorkspaceId>,
    pub session_id: SessionId,
    pub profile_id: BrowserProfileId,
    pub now: UtcMillis,
}

#[derive(Clone, Debug)]
pub struct CreateBrowserTab {
    pub tab_id: BrowserTabId,
    pub browser_session_id: BrowserSessionId,
    pub url: String,
    pub now: UtcMillis,
}

#[derive(Clone, Debug)]
pub struct AcquireBrowserLease {
    pub lease_id: BrowserLeaseId,
    pub tab_id: BrowserTabId,
    pub surface_id: String,
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
    pub tab_id: &'a BrowserTabId,
    pub surface_id: &'a str,
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
pub struct BrowserSurfaceControlSnapshot {
    pub desktop_epoch: String,
    pub window_id: String,
    pub tab_id: BrowserTabId,
    pub surface_id: String,
    pub surface_revision: u64,
    pub web_contents_id: u32,
    pub target_id: String,
    pub browser_context_id: String,
    pub navigation_revision: u64,
    pub fence: u64,
    pub lease_id: Option<BrowserLeaseId>,
}

pub type BrowserPrimarySurface = BrowserSurfaceBinding;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserAuthoritySnapshot {
    pub revision: u64,
    pub profiles: Vec<BrowserProfile>,
    pub sessions: Vec<BrowserSession>,
    pub tabs: Vec<BrowserTab>,
    pub annotations: Vec<BrowserAnnotation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserDurableState {
    pub schema_version: u16,
    pub revision: u64,
    pub profiles: Vec<BrowserProfile>,
    pub sessions: Vec<BrowserSession>,
    pub tabs: Vec<BrowserDurableTab>,
    pub annotations: Vec<BrowserAnnotation>,
}

/// 浏览器 Tab 的持久部分。Surface、viewport、焦点和控制 Lease 均由桌面运行态持有。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserDurableTab {
    pub tab_id: BrowserTabId,
    pub browser_session_id: BrowserSessionId,
    #[serde(default)]
    pub order: u32,
    pub lifecycle: BrowserTabLifecycle,
    #[serde(alias = "url")]
    pub canonical_url: String,
    #[serde(alias = "title")]
    pub page_title: String,
    pub display_label: Option<String>,
    pub navigation_revision: u64,
    /// Authority 分配的快照 revision。它属于 element_ref 失效协议，必须跨 daemon 重启持久化。
    #[serde(default)]
    pub snapshot_revision: u64,
    #[serde(default)]
    pub annotation_sequence: u64,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

impl From<&BrowserTab> for BrowserDurableTab {
    fn from(tab: &BrowserTab) -> Self {
        let (url, _, title) =
            normalize_browser_page_state(tab.url.clone(), tab.origin.clone(), tab.title.clone());
        Self {
            tab_id: tab.tab_id.clone(),
            browser_session_id: tab.browser_session_id.clone(),
            order: tab.order,
            lifecycle: tab.lifecycle,
            canonical_url: url,
            page_title: title,
            display_label: tab.display_label.clone(),
            navigation_revision: tab.navigation_revision,
            snapshot_revision: tab.snapshot_revision,
            annotation_sequence: tab.annotation_sequence,
            created_at: tab.created_at,
            updated_at: tab.updated_at,
        }
    }
}

impl BrowserDurableTab {
    fn into_runtime_tab(self) -> BrowserTab {
        let (url, origin, title) =
            normalize_browser_page_state(self.canonical_url, None, self.page_title);
        BrowserTab {
            tab_id: self.tab_id,
            browser_session_id: self.browser_session_id,
            order: self.order,
            lifecycle: self.lifecycle,
            url,
            origin,
            title,
            display_label: self.display_label,
            navigation_revision: self.navigation_revision,
            snapshot_revision: self.snapshot_revision,
            annotation_sequence: self.annotation_sequence,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

pub const BROWSER_DURABLE_STATE_SCHEMA_VERSION: u16 = 6;
const PREVIOUS_BROWSER_DURABLE_STATE_SCHEMA_VERSION: u16 = 5;
const LEGACY_BROWSER_DURABLE_STATE_SCHEMA_VERSION: u16 = 4;

#[derive(Clone, Debug, Default)]
pub struct BrowserAuthority {
    revision: u64,
    profiles: HashMap<BrowserProfileId, BrowserProfile>,
    sessions: HashMap<BrowserSessionId, BrowserSession>,
    tabs: HashMap<BrowserTabId, BrowserTab>,
    annotations: HashMap<BrowserAnnotationId, BrowserAnnotation>,
    leases: HashMap<BrowserLeaseId, BrowserControlLease>,
    active_surface_leases: HashMap<(BrowserTabId, String), BrowserLeaseId>,
    surface_fences: HashMap<(BrowserTabId, String), u64>,
    primary_surfaces: HashMap<BrowserTabId, BrowserPrimarySurface>,
    active_desktop_epoch: Option<String>,
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
        let mut annotations = self
            .annotations
            .values()
            .filter(|annotation| &annotation.tab_id == tab_id)
            .cloned()
            .collect::<Vec<_>>();
        annotations.sort_by(|left, right| {
            left.sequence.cmp(&right.sequence).then_with(|| {
                left.annotation_id
                    .as_str()
                    .cmp(right.annotation_id.as_str())
            })
        });
        annotations
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
        annotation.sequence = tab.annotation_sequence.checked_add(1).ok_or_else(|| {
            BrowserAuthorityError::InvalidSnapshot(
                "browser annotation sequence overflow".to_string(),
            )
        })?;
        self.tabs
            .get_mut(&annotation.tab_id)
            .expect("browser tab was validated before annotation mutation")
            .annotation_sequence = annotation.sequence;
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

    pub fn active_lease_for_surface(
        &self,
        tab_id: &BrowserTabId,
        surface_id: &str,
    ) -> Option<&BrowserControlLease> {
        self.active_surface_leases
            .get(&(tab_id.clone(), surface_id.to_string()))
            .and_then(|lease_id| self.leases.get(lease_id))
    }

    pub fn primary_surface(&self, tab_id: &BrowserTabId) -> Option<&BrowserPrimarySurface> {
        self.primary_surfaces.get(tab_id)
    }

    pub fn surface_control_snapshot(
        &self,
        tab_id: &BrowserTabId,
        surface_id: &str,
    ) -> Result<BrowserSurfaceControlSnapshot, BrowserAuthorityError> {
        self.require_tab(tab_id)?;
        let key = (tab_id.clone(), surface_id.to_string());
        let binding = self
            .primary_surfaces
            .get(tab_id)
            .filter(|binding| binding.surface_id == surface_id)
            .cloned()
            .ok_or_else(|| BrowserAuthorityError::SurfaceNotPrimary {
                tab_id: tab_id.clone(),
                surface_id: surface_id.to_string(),
            })?;
        Ok(BrowserSurfaceControlSnapshot {
            desktop_epoch: binding.desktop_epoch,
            window_id: binding.window_id,
            tab_id: binding.tab_id,
            surface_id: binding.surface_id,
            surface_revision: binding.surface_revision,
            web_contents_id: binding.web_contents_id,
            target_id: binding.target_id,
            browser_context_id: binding.browser_context_id,
            navigation_revision: binding.navigation_revision,
            fence: self.surface_fences.get(&key).copied().unwrap_or_default(),
            lease_id: self.active_surface_leases.get(&key).cloned(),
        })
    }

    /// 切换 Desktop 运行边界时，旧窗口的 Surface 和 Lease 立即失效；逻辑
    /// Browser Tab 仍保留，由新 Desktop 重新物化。相同 epoch 的重连不清空
    /// 现有 Surface，避免 daemon 短暂断线造成页面闪断。
    pub fn accept_desktop_epoch(
        &mut self,
        desktop_epoch: String,
        now: UtcMillis,
    ) -> Vec<BrowserControlLease> {
        if self.active_desktop_epoch.as_deref() == Some(desktop_epoch.as_str()) {
            return Vec::new();
        }
        let revoked = self.revoke_leases(
            &BrowserLeaseSelector::default(),
            BrowserLeaseEndReason::RuntimeUnavailable,
            now,
        );
        self.primary_surfaces.clear();
        self.active_desktop_epoch = Some(desktop_epoch);
        self.bump_revision();
        revoked
    }

    pub fn set_primary_surface(
        &mut self,
        binding: BrowserSurfaceBinding,
        now: UtcMillis,
    ) -> Result<Vec<BrowserControlLease>, BrowserAuthorityError> {
        self.require_tab(&binding.tab_id)?;
        if binding.desktop_epoch.trim().is_empty()
            || binding.window_id.trim().is_empty()
            || binding.surface_id.trim().is_empty()
            || binding.target_id.trim().is_empty()
            || binding.browser_context_id.trim().is_empty()
            || binding.web_contents_id == 0
        {
            return Err(BrowserAuthorityError::InvalidSurfaceBinding);
        }
        if let Some(active_epoch) = &self.active_desktop_epoch {
            if active_epoch != &binding.desktop_epoch {
                return Ok(Vec::new());
            }
        } else {
            self.active_desktop_epoch = Some(binding.desktop_epoch.clone());
        }

        if let Some(current) = self.primary_surfaces.get(&binding.tab_id).cloned() {
            if binding.surface_revision < current.surface_revision {
                return Ok(Vec::new());
            }
            if binding.surface_revision == current.surface_revision {
                if !same_physical_surface(&current, &binding) {
                    return Ok(Vec::new());
                }
                if binding.navigation_revision > current.navigation_revision {
                    let revoked = self.revoke_surface_leases(
                        &binding.tab_id,
                        &binding.surface_id,
                        BrowserLeaseEndReason::RuntimeUnavailable,
                        now,
                    );
                    self.primary_surfaces
                        .insert(binding.tab_id.clone(), binding);
                    self.bump_revision();
                    return Ok(revoked);
                }
                return Ok(Vec::new());
            }
        }

        let revoked = self.revoke_leases(
            &BrowserLeaseSelector {
                tab_id: Some(binding.tab_id.clone()),
                ..BrowserLeaseSelector::default()
            },
            BrowserLeaseEndReason::RuntimeUnavailable,
            now,
        );
        self.primary_surfaces
            .insert(binding.tab_id.clone(), binding);
        self.bump_revision();
        Ok(revoked)
    }

    pub fn accept_page_binding(
        &mut self,
        binding: &BrowserSurfaceBinding,
        now: UtcMillis,
    ) -> Result<(bool, Vec<BrowserControlLease>), BrowserAuthorityError> {
        self.require_tab(&binding.tab_id)?;
        let Some(current) = self.primary_surfaces.get(&binding.tab_id).cloned() else {
            return Ok((false, Vec::new()));
        };
        if !same_physical_surface(&current, binding)
            || binding.navigation_revision < current.navigation_revision
        {
            return Ok((false, Vec::new()));
        }
        let mut revoked = Vec::new();
        if binding.navigation_revision > current.navigation_revision {
            revoked = self.revoke_surface_leases(
                &binding.tab_id,
                &binding.surface_id,
                BrowserLeaseEndReason::RuntimeUnavailable,
                now,
            );
            self.primary_surfaces
                .insert(binding.tab_id.clone(), binding.clone());
            self.bump_revision();
        }
        Ok((true, revoked))
    }

    pub fn is_current_surface_binding(&self, binding: &BrowserSurfaceBinding) -> bool {
        self.primary_surfaces
            .get(&binding.tab_id)
            .is_some_and(|current| {
                same_physical_surface(current, binding)
                    && binding.navigation_revision == current.navigation_revision
            })
    }

    pub fn clear_primary_surfaces(&mut self, now: UtcMillis) -> Vec<BrowserControlLease> {
        if self.primary_surfaces.is_empty() {
            self.active_desktop_epoch = None;
            return Vec::new();
        }
        let revoked = self.revoke_leases(
            &BrowserLeaseSelector::default(),
            BrowserLeaseEndReason::RuntimeUnavailable,
            now,
        );
        self.primary_surfaces.clear();
        self.active_desktop_epoch = None;
        self.bump_revision();
        revoked
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
            BrowserSessionLifecycle::Recovering
                | BrowserSessionLifecycle::Interrupted
                | BrowserSessionLifecycle::Failed
        ) {
            let tab_ids = self.require_session(browser_session_id)?.tab_ids.clone();
            for tab_id in tab_ids {
                self.revoke_leases(
                    &BrowserLeaseSelector {
                        tab_id: Some(tab_id),
                        ..BrowserLeaseSelector::default()
                    },
                    BrowserLeaseEndReason::RuntimeUnavailable,
                    now,
                );
            }
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
        let session = self.require_ready_session(&input.browser_session_id)?;
        if self.live_tab_count_for_session(session) >= MAX_BROWSER_TABS_PER_SESSION {
            return Err(BrowserAuthorityError::SessionTabLimitReached {
                browser_session_id: input.browser_session_id.clone(),
                limit: MAX_BROWSER_TABS_PER_SESSION,
            });
        }
        let open_tab_count = self.live_tab_count();
        if open_tab_count >= MAX_BROWSER_TABS_TOTAL {
            return Err(BrowserAuthorityError::GlobalTabLimitReached {
                limit: MAX_BROWSER_TABS_TOTAL,
            });
        }
        let order = self
            .require_ready_session(&input.browser_session_id)?
            .tab_ids
            .len()
            .try_into()
            .unwrap_or(u32::MAX);
        let tab = BrowserTab {
            tab_id: input.tab_id,
            browser_session_id: input.browser_session_id.clone(),
            order,
            lifecycle: BrowserTabLifecycle::Creating,
            url: input.url,
            origin: None,
            title: String::new(),
            display_label: None,
            navigation_revision: 0,
            snapshot_revision: 0,
            annotation_sequence: 0,
            created_at: input.now,
            updated_at: input.now,
        };
        self.tabs.insert(tab.tab_id.clone(), tab.clone());
        let session = self
            .sessions
            .get_mut(&input.browser_session_id)
            .expect("browser session was validated before mutation");
        session.tab_ids.push(tab.tab_id.clone());
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
        {
            let tab = self
                .tabs
                .get_mut(tab_id)
                .expect("browser tab was validated before mutation");
            tab.lifecycle = lifecycle;
            tab.updated_at = now;
            if lifecycle == BrowserTabLifecycle::Suspended {
                tab.navigation_revision = tab.navigation_revision.saturating_add(1);
                tab.snapshot_revision = tab.snapshot_revision.saturating_add(1);
            }
        }
        if lifecycle == BrowserTabLifecycle::Closed {
            self.revoke_leases(
                &BrowserLeaseSelector {
                    tab_id: Some(tab_id.clone()),
                    ..BrowserLeaseSelector::default()
                },
                BrowserLeaseEndReason::SessionClosed,
                now,
            );
            self.primary_surfaces.remove(tab_id);
            let session = self
                .sessions
                .get_mut(&browser_session_id)
                .expect("browser tab cannot outlive its owning session");
            session.tab_ids.retain(|candidate| candidate != tab_id);
            session.revision = session.revision.saturating_add(1);
            session.updated_at = now;
        } else if lifecycle == BrowserTabLifecycle::Suspended {
            self.revoke_leases(
                &BrowserLeaseSelector {
                    tab_id: Some(tab_id.clone()),
                    ..BrowserLeaseSelector::default()
                },
                BrowserLeaseEndReason::RuntimeUnavailable,
                now,
            );
            self.primary_surfaces.remove(tab_id);
            self.mark_active_annotations_stale(tab_id, now);
        } else if lifecycle == BrowserTabLifecycle::Crashed {
            self.revoke_leases(
                &BrowserLeaseSelector {
                    tab_id: Some(tab_id.clone()),
                    ..BrowserLeaseSelector::default()
                },
                BrowserLeaseEndReason::RuntimeUnavailable,
                now,
            );
            self.primary_surfaces.remove(tab_id);
        }
        let tab = self
            .tabs
            .get(tab_id)
            .expect("browser tab remains available")
            .clone();
        self.bump_revision();
        Ok(tab)
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
        let (url, origin, title) = normalize_browser_page_state(url, origin, title);
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

    pub fn apply_native_page_state(
        &mut self,
        tab_id: &BrowserTabId,
        url: Option<String>,
        title: Option<String>,
        now: UtcMillis,
    ) -> Result<BrowserTab, BrowserAuthorityError> {
        self.require_ready_tab(tab_id)?;
        let (tab, document_changed, changed) = {
            let tab = self
                .tabs
                .get_mut(tab_id)
                .expect("browser tab was validated before mutation");
            let (next_url, next_origin, next_title) = match url {
                Some(url) => normalize_browser_page_state(
                    url.clone(),
                    crate::browser_navigation_origin(&url),
                    title.unwrap_or_else(|| tab.title.clone()),
                ),
                None => (
                    tab.url.clone(),
                    tab.origin.clone(),
                    title.unwrap_or_else(|| tab.title.clone()),
                ),
            };
            let document_changed = tab.url != next_url;
            let metadata_changed = tab.origin != next_origin || tab.title != next_title;
            let changed = document_changed || metadata_changed;
            if changed {
                tab.url = next_url;
                tab.origin = next_origin;
                tab.title = next_title;
                if document_changed {
                    tab.navigation_revision = tab.navigation_revision.saturating_add(1);
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
        }
        Ok(tab)
    }

    pub fn record_snapshot(
        &mut self,
        tab_id: &BrowserTabId,
        now: UtcMillis,
    ) -> Result<(u64, u64), BrowserAuthorityError> {
        self.require_ready_tab(tab_id)?;
        let tab = self
            .tabs
            .get_mut(tab_id)
            .expect("browser tab was validated before mutation");
        tab.snapshot_revision = tab.snapshot_revision.saturating_add(1);
        tab.updated_at = now;
        let revisions = (tab.navigation_revision, tab.snapshot_revision);
        self.bump_revision();
        Ok(revisions)
    }

    pub fn apply_host_snapshot_revision(
        &mut self,
        tab_id: &BrowserTabId,
        host_snapshot_revision: u64,
        _now: UtcMillis,
    ) -> Result<u64, BrowserAuthorityError> {
        self.require_ready_tab(tab_id)?;
        let tab = self
            .tabs
            .get_mut(tab_id)
            .expect("browser tab was validated before mutation");
        if host_snapshot_revision != tab.snapshot_revision {
            return Err(BrowserAuthorityError::SnapshotRevisionMismatch {
                expected: tab.snapshot_revision,
                provided: host_snapshot_revision,
            });
        }
        Ok(tab.snapshot_revision)
    }

    pub fn validate_snapshot_result(
        &self,
        tab_id: &BrowserTabId,
        navigation_revision: u64,
        snapshot_revision: u64,
    ) -> Result<(), BrowserAuthorityError> {
        let tab = self.require_ready_tab(tab_id)?;
        if tab.navigation_revision != navigation_revision {
            return Err(BrowserAuthorityError::NavigationRevisionRegression {
                current: tab.navigation_revision,
                received: navigation_revision,
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
        if self.leases.contains_key(&input.lease_id) {
            return Err(BrowserAuthorityError::LeaseAlreadyExists(input.lease_id));
        }
        let tab = self.require_ready_tab(&input.tab_id)?;
        let session = self.require_ready_session(&tab.browser_session_id)?;
        validate_lease_owner(&input.owner, session)?;
        let primary = self
            .primary_surfaces
            .get(&input.tab_id)
            .cloned()
            .ok_or_else(|| {
                BrowserAuthorityError::PrimarySurfaceUnavailable(input.tab_id.clone())
            })?;
        if primary.surface_id != input.surface_id {
            return Err(BrowserAuthorityError::SurfaceNotPrimary {
                tab_id: input.tab_id,
                surface_id: input.surface_id,
            });
        }
        if input.turn_id.trim().is_empty() {
            return Err(BrowserAuthorityError::EmptyTurnId);
        }
        if input.expires_at <= input.acquired_at {
            return Err(BrowserAuthorityError::InvalidLeaseExpiry);
        }
        let key = (input.tab_id.clone(), input.surface_id.clone());
        if let Some(lease_id) = self.active_surface_leases.get(&key) {
            return Err(BrowserAuthorityError::LeaseConflict {
                lease_id: lease_id.clone(),
            });
        }
        let fence = self.advance_surface_fence(&input.tab_id, &input.surface_id);
        let lease = BrowserControlLease {
            lease_id: input.lease_id,
            desktop_epoch: primary.desktop_epoch,
            window_id: primary.window_id,
            surface_revision: primary.surface_revision,
            tab_id: input.tab_id,
            surface_id: input.surface_id,
            web_contents_id: primary.web_contents_id,
            target_id: primary.target_id,
            browser_context_id: primary.browser_context_id,
            navigation_revision: primary.navigation_revision,
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
        self.active_surface_leases
            .insert(key, lease.lease_id.clone());
        self.leases.insert(lease.lease_id.clone(), lease.clone());
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
        let key = (lease.tab_id.clone(), lease.surface_id.clone());
        let current_fence = self.surface_fences.get(&key).copied().unwrap_or_default();
        if lease.fence != current_fence || request.fence != current_fence {
            return Err(BrowserAuthorityError::LeaseFenceMismatch {
                expected: current_fence,
                provided: request.fence,
            });
        }
        if &lease.tab_id != request.tab_id {
            return Err(BrowserAuthorityError::LeaseTabMismatch {
                lease_id: request.lease_id.clone(),
            });
        }
        if lease.surface_id != request.surface_id {
            return Err(BrowserAuthorityError::LeaseSurfaceMismatch {
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
        let tab = self.require_ready_tab(request.tab_id)?;
        self.require_ready_session(&tab.browser_session_id)?;
        let primary = self.primary_surfaces.get(request.tab_id).ok_or_else(|| {
            BrowserAuthorityError::PrimarySurfaceUnavailable(request.tab_id.clone())
        })?;
        let lease_binding = lease.surface_binding();
        if primary.surface_id != request.surface_id
            || !same_physical_surface(primary, &lease_binding)
        {
            return Err(BrowserAuthorityError::SurfaceNotPrimary {
                tab_id: request.tab_id.clone(),
                surface_id: request.surface_id.to_string(),
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

    fn revoke_surface_leases(
        &mut self,
        tab_id: &BrowserTabId,
        surface_id: &str,
        reason: BrowserLeaseEndReason,
        now: UtcMillis,
    ) -> Vec<BrowserControlLease> {
        let revoked = self.revoke_leases(
            &BrowserLeaseSelector {
                tab_id: Some(tab_id.clone()),
                surface_id: Some(surface_id.to_string()),
                ..BrowserLeaseSelector::default()
            },
            reason,
            now,
        );
        if revoked.is_empty() {
            self.advance_surface_fence(tab_id, surface_id);
        }
        revoked
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
        tab_id: &BrowserTabId,
        surface_id: &str,
        now: UtcMillis,
    ) -> Result<(BrowserSurfaceControlSnapshot, Vec<BrowserControlLease>), BrowserAuthorityError>
    {
        self.require_tab(tab_id)?;
        let revoked = self.revoke_leases(
            &BrowserLeaseSelector {
                tab_id: Some(tab_id.clone()),
                surface_id: Some(surface_id.to_string()),
                ..BrowserLeaseSelector::default()
            },
            BrowserLeaseEndReason::UserTakeover,
            now,
        );
        if revoked.is_empty() {
            self.advance_surface_fence(tab_id, surface_id);
        }
        Ok((self.surface_control_snapshot(tab_id, surface_id)?, revoked))
    }

    pub fn snapshot(&self) -> BrowserAuthoritySnapshot {
        let mut annotations = self.annotations.values().cloned().collect::<Vec<_>>();
        annotations.sort_by(|left, right| {
            left.tab_id
                .as_str()
                .cmp(right.tab_id.as_str())
                .then_with(|| left.sequence.cmp(&right.sequence))
        });
        BrowserAuthoritySnapshot {
            revision: self.revision,
            profiles: self.profiles.values().cloned().collect(),
            sessions: self.sessions.values().cloned().collect(),
            tabs: self.tabs.values().cloned().collect(),
            annotations,
        }
    }

    pub fn durable_state(&self) -> BrowserDurableState {
        let tabs = self.tabs.values().collect::<Vec<_>>();
        let mut annotations = self.annotations.values().cloned().collect::<Vec<_>>();
        annotations.sort_by(|left, right| {
            left.tab_id
                .as_str()
                .cmp(right.tab_id.as_str())
                .then_with(|| left.sequence.cmp(&right.sequence))
        });
        BrowserDurableState {
            schema_version: BROWSER_DURABLE_STATE_SCHEMA_VERSION,
            revision: self.revision,
            profiles: self.profiles.values().cloned().collect(),
            sessions: self.sessions.values().cloned().collect(),
            tabs: tabs.into_iter().map(BrowserDurableTab::from).collect(),
            annotations,
        }
    }

    pub fn restore_durable(
        state: BrowserDurableState,
        now: UtcMillis,
    ) -> Result<Self, BrowserAuthorityError> {
        if state.schema_version != BROWSER_DURABLE_STATE_SCHEMA_VERSION
            && state.schema_version != PREVIOUS_BROWSER_DURABLE_STATE_SCHEMA_VERSION
            && state.schema_version != LEGACY_BROWSER_DURABLE_STATE_SCHEMA_VERSION
        {
            return Err(BrowserAuthorityError::InvalidSnapshot(format!(
                "unsupported browser durable state schema: {}",
                state.schema_version
            )));
        }
        let BrowserDurableState {
            schema_version,
            revision,
            profiles,
            sessions,
            mut tabs,
            annotations,
        } = state;
        if schema_version == PREVIOUS_BROWSER_DURABLE_STATE_SCHEMA_VERSION
            || schema_version == LEGACY_BROWSER_DURABLE_STATE_SCHEMA_VERSION
        {
            migrate_legacy_tab_order(&mut tabs, &sessions);
        }
        Self::restore(
            BrowserAuthoritySnapshot {
                revision,
                profiles,
                sessions,
                tabs: tabs
                    .into_iter()
                    .map(BrowserDurableTab::into_runtime_tab)
                    .collect(),
                annotations,
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
        self.primary_surfaces.clear();
        self.active_desktop_epoch = None;
        let recovering_session_ids = self
            .sessions
            .values()
            .filter(|session| session.lifecycle.is_recoverable())
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
                        && matches!(
                            tab.lifecycle,
                            BrowserTabLifecycle::Creating
                                | BrowserTabLifecycle::Ready
                                | BrowserTabLifecycle::Suspended
                                | BrowserTabLifecycle::Crashed
                        )
                    {
                        tab.lifecycle = BrowserTabLifecycle::Suspended;
                        tab.navigation_revision = tab.navigation_revision.saturating_add(1);
                        tab.snapshot_revision = tab.snapshot_revision.saturating_add(1);
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

    /// 将上一次 Browser Host 耗尽重试后保留的中断会话重新投入恢复。
    /// 该操作只改变运行边界，不创建或恢复 Chromium Page。
    pub fn resume_interrupted_sessions(&mut self, now: UtcMillis) -> Vec<BrowserSessionId> {
        let session_ids = self
            .sessions
            .values()
            .filter(|session| session.lifecycle == BrowserSessionLifecycle::Interrupted)
            .map(|session| session.browser_session_id.clone())
            .collect::<Vec<_>>();
        for browser_session_id in &session_ids {
            let _ = self.transition_session(
                browser_session_id,
                BrowserSessionLifecycle::Recovering,
                now,
            );
            let tab_ids = self
                .sessions
                .get(browser_session_id)
                .map(|session| session.tab_ids.clone())
                .unwrap_or_default();
            for tab_id in tab_ids {
                if self.tabs.get(&tab_id).is_some_and(|tab| {
                    matches!(
                        tab.lifecycle,
                        BrowserTabLifecycle::Creating
                            | BrowserTabLifecycle::Ready
                            | BrowserTabLifecycle::Suspended
                            | BrowserTabLifecycle::Crashed
                    )
                }) {
                    let _ = self.transition_tab(&tab_id, BrowserTabLifecycle::Suspended, now);
                }
            }
        }
        session_ids
    }

    pub fn restore(
        mut snapshot: BrowserAuthoritySnapshot,
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
        for mut session in snapshot.sessions {
            if !authority.profiles.contains_key(&session.profile_id) {
                return Err(BrowserAuthorityError::InvalidSnapshot(format!(
                    "browser session {} references unknown profile {}",
                    session.browser_session_id, session.profile_id
                )));
            }
            if session.lifecycle.is_recoverable() {
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
                && matches!(
                    tab.lifecycle,
                    BrowserTabLifecycle::Creating
                        | BrowserTabLifecycle::Ready
                        | BrowserTabLifecycle::Suspended
                        | BrowserTabLifecycle::Crashed
                )
            {
                tab.lifecycle = BrowserTabLifecycle::Suspended;
                tab.navigation_revision = tab.navigation_revision.saturating_add(1);
                tab.snapshot_revision = tab.snapshot_revision.saturating_add(1);
                tab.updated_at = now;
            }
            if authority.tabs.insert(tab.tab_id.clone(), tab).is_some() {
                return Err(BrowserAuthorityError::InvalidSnapshot(
                    "duplicate browser tab".to_string(),
                ));
            }
        }
        snapshot.annotations.sort_by(|left, right| {
            left.created_at.cmp(&right.created_at).then_with(|| {
                left.annotation_id
                    .as_str()
                    .cmp(right.annotation_id.as_str())
            })
        });
        let mut maximum_annotation_sequences = HashMap::<BrowserTabId, u64>::new();
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
            let maximum_sequence = maximum_annotation_sequences
                .entry(annotation.tab_id.clone())
                .or_default();
            if annotation.sequence == 0 || annotation.sequence <= *maximum_sequence {
                annotation.sequence = maximum_sequence.checked_add(1).ok_or_else(|| {
                    BrowserAuthorityError::InvalidSnapshot(
                        "browser annotation sequence overflow".to_string(),
                    )
                })?;
            }
            *maximum_sequence = annotation.sequence;
            if let Some(tab) = authority.tabs.get_mut(&annotation.tab_id) {
                tab.annotation_sequence = tab.annotation_sequence.max(annotation.sequence);
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
        // Failed 是旧运行边界的终态，不是可恢复工作区。升级或重启后直接
        // 收口为 Closed，避免不可用 Tab 继续占据右侧 Tab 栏；记录本身仍
        // 留在 durable state 中用于历史审计。
        let failed_session_ids = authority
            .sessions
            .values()
            .filter(|session| session.lifecycle == BrowserSessionLifecycle::Failed)
            .map(|session| session.browser_session_id.clone())
            .collect::<Vec<_>>();
        for browser_session_id in failed_session_ids {
            authority.transition_session(
                &browser_session_id,
                BrowserSessionLifecycle::Closed,
                now,
            )?;
        }
        validate_open_session_uniqueness(&authority.sessions)?;
        authority.bump_revision();
        Ok(authority)
    }

    fn close_session_resources(
        &mut self,
        browser_session_id: &BrowserSessionId,
        now: UtcMillis,
    ) -> Result<(), BrowserAuthorityError> {
        let tab_ids = self.require_session(browser_session_id)?.tab_ids.clone();
        for tab_id in tab_ids {
            self.revoke_leases(
                &BrowserLeaseSelector {
                    tab_id: Some(tab_id.clone()),
                    ..BrowserLeaseSelector::default()
                },
                BrowserLeaseEndReason::SessionClosed,
                now,
            );
            self.primary_surfaces.remove(&tab_id);
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
        let key = (existing.tab_id.clone(), existing.surface_id.clone());
        if self.active_surface_leases.get(&key) == Some(lease_id) {
            self.active_surface_leases.remove(&key);
        }
        self.advance_surface_fence(&existing.tab_id, &existing.surface_id);
        let lease = self
            .leases
            .get_mut(lease_id)
            .expect("browser lease was validated before mutation");
        lease.lifecycle = lifecycle;
        lease.end_reason = Some(reason);
        lease.ended_at = Some(now);
        let lease = lease.clone();
        Ok(lease)
    }

    fn advance_surface_fence(&mut self, tab_id: &BrowserTabId, surface_id: &str) -> u64 {
        let fence = self
            .surface_fences
            .entry((tab_id.clone(), surface_id.to_string()))
            .or_default();
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

    fn live_tab_count_for_session(&self, session: &BrowserSession) -> usize {
        session
            .tab_ids
            .iter()
            .filter(|tab_id| {
                self.tabs.get(*tab_id).is_some_and(|tab| {
                    matches!(
                        tab.lifecycle,
                        BrowserTabLifecycle::Creating
                            | BrowserTabLifecycle::Ready
                            | BrowserTabLifecycle::Suspended
                    )
                })
            })
            .count()
    }

    fn live_tab_count(&self) -> usize {
        self.sessions
            .values()
            .map(|session| self.live_tab_count_for_session(session))
            .sum()
    }
}

fn migrate_legacy_tab_order(tabs: &mut [BrowserDurableTab], sessions: &[BrowserSession]) {
    let mut ordered_tab_ids = HashSet::new();
    let mut next_order_by_session = HashMap::<BrowserSessionId, u32>::new();

    for session in sessions {
        let next_order = session.tab_ids.len().try_into().unwrap_or(u32::MAX);
        next_order_by_session.insert(session.browser_session_id.clone(), next_order);
        for (order, tab_id) in session.tab_ids.iter().enumerate() {
            if let Some(tab) = tabs.iter_mut().find(|tab| &tab.tab_id == tab_id) {
                tab.order = order.try_into().unwrap_or(u32::MAX);
                ordered_tab_ids.insert(tab_id.clone());
            }
        }
    }

    let mut fallback_indices = tabs
        .iter()
        .enumerate()
        .filter_map(|(index, tab)| (!ordered_tab_ids.contains(&tab.tab_id)).then_some(index))
        .collect::<Vec<_>>();
    fallback_indices.sort_by(|left, right| {
        let left_tab = &tabs[*left];
        let right_tab = &tabs[*right];
        left_tab
            .browser_session_id
            .as_str()
            .cmp(right_tab.browser_session_id.as_str())
            .then_with(|| left_tab.created_at.cmp(&right_tab.created_at))
            .then_with(|| left_tab.tab_id.as_str().cmp(right_tab.tab_id.as_str()))
    });

    for index in fallback_indices {
        let tab = &mut tabs[index];
        let next_order = next_order_by_session
            .entry(tab.browser_session_id.clone())
            .or_default();
        tab.order = *next_order;
        *next_order = next_order.saturating_add(1);
    }
}

fn annotation_snapshot_revision(anchor: &BrowserAnnotationAnchor) -> u64 {
    match anchor {
        BrowserAnnotationAnchor::Element(anchor) => anchor.snapshot_revision,
        BrowserAnnotationAnchor::Region(anchor) => anchor.snapshot_revision,
    }
}

fn same_physical_surface(left: &BrowserSurfaceBinding, right: &BrowserSurfaceBinding) -> bool {
    left.desktop_epoch == right.desktop_epoch
        && left.window_id == right.window_id
        && left.surface_id == right.surface_id
        && left.surface_revision == right.surface_revision
        && left.tab_id == right.tab_id
        && left.web_contents_id == right.web_contents_id
        && left.target_id == right.target_id
        && left.browser_context_id == right.browser_context_id
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
    if owner.workspace_id != session.workspace_id {
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
