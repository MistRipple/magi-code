use magi_core::{
    BrowserAnnotationId, BrowserLeaseId, BrowserProfileId, BrowserSessionId, BrowserTabId,
};

use crate::{BrowserSessionLifecycle, BrowserTabLifecycle};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BrowserAuthorityError {
    #[error("browser profile already exists: {0}")]
    ProfileAlreadyExists(BrowserProfileId),
    #[error("browser profile does not exist: {0}")]
    UnknownProfile(BrowserProfileId),
    #[error("browser session already exists: {0}")]
    SessionAlreadyExists(BrowserSessionId),
    #[error(
        "an open browser session already exists for Magi session {session_id}: {browser_session_id}"
    )]
    OpenSessionAlreadyExists {
        session_id: magi_core::SessionId,
        browser_session_id: BrowserSessionId,
    },
    #[error("browser session does not exist: {0}")]
    UnknownSession(BrowserSessionId),
    #[error("browser tab already exists: {0}")]
    TabAlreadyExists(BrowserTabId),
    #[error("browser session {browser_session_id} reached the tab limit: {limit}")]
    SessionTabLimitReached {
        browser_session_id: BrowserSessionId,
        limit: usize,
    },
    #[error("browser authority reached the global tab limit: {limit}")]
    GlobalTabLimitReached { limit: usize },
    #[error("browser tab does not exist: {0}")]
    UnknownTab(BrowserTabId),
    #[error("browser annotation already exists: {0}")]
    AnnotationAlreadyExists(BrowserAnnotationId),
    #[error("browser annotation does not exist: {0}")]
    UnknownAnnotation(BrowserAnnotationId),
    #[error(
        "browser annotation {annotation_id} belongs to another browser session {browser_session_id}"
    )]
    AnnotationSessionMismatch {
        annotation_id: BrowserAnnotationId,
        browser_session_id: BrowserSessionId,
    },
    #[error("browser lease already exists: {0}")]
    LeaseAlreadyExists(BrowserLeaseId),
    #[error("browser lease does not exist: {0}")]
    UnknownLease(BrowserLeaseId),
    #[error("invalid browser session transition: {from:?} -> {to:?}")]
    InvalidSessionTransition {
        from: BrowserSessionLifecycle,
        to: BrowserSessionLifecycle,
    },
    #[error("invalid browser tab transition: {from:?} -> {to:?}")]
    InvalidTabTransition {
        from: BrowserTabLifecycle,
        to: BrowserTabLifecycle,
    },
    #[error("browser session is not ready: {browser_session_id} ({lifecycle:?})")]
    SessionNotReady {
        browser_session_id: BrowserSessionId,
        lifecycle: BrowserSessionLifecycle,
    },
    #[error("browser session {browser_session_id} does not belong to profile {profile_id}")]
    SessionProfileMismatch {
        browser_session_id: BrowserSessionId,
        profile_id: BrowserProfileId,
    },
    #[error("browser tab {tab_id} does not belong to browser session {browser_session_id}")]
    TabSessionMismatch {
        tab_id: BrowserTabId,
        browser_session_id: BrowserSessionId,
    },
    #[error("browser tab is not ready: {tab_id} ({lifecycle:?})")]
    TabNotReady {
        tab_id: BrowserTabId,
        lifecycle: BrowserTabLifecycle,
    },
    #[error("browser lease owner is missing {field}")]
    MissingOwnershipField { field: &'static str },
    #[error("browser lease owner {field} does not match browser session")]
    OwnershipMismatch { field: &'static str },
    #[error("browser turn id cannot be empty")]
    EmptyTurnId,
    #[error("browser lease expiry must be later than acquisition time")]
    InvalidLeaseExpiry,
    #[error("browser surface already has an active lease: {lease_id}")]
    LeaseConflict { lease_id: BrowserLeaseId },
    #[error("browser lease is no longer held: {0}")]
    LeaseNotHeld(BrowserLeaseId),
    #[error("browser lease has expired: {0}")]
    LeaseExpired(BrowserLeaseId),
    #[error("browser lease fence mismatch: expected={expected}, provided={provided}")]
    LeaseFenceMismatch { expected: u64, provided: u64 },
    #[error("browser lease {lease_id} is bound to another browser tab")]
    LeaseTabMismatch { lease_id: BrowserLeaseId },
    #[error("browser lease {lease_id} is bound to another browser surface")]
    LeaseSurfaceMismatch { lease_id: BrowserLeaseId },
    #[error("browser tab has no primary surface: {0}")]
    PrimarySurfaceUnavailable(BrowserTabId),
    #[error("browser surface is not primary for tab {tab_id}: {surface_id}")]
    SurfaceNotPrimary {
        tab_id: BrowserTabId,
        surface_id: String,
    },
    #[error("browser lease goal binding does not match the current execution")]
    GoalBindingMismatch,
    #[error("browser lease owner does not match the current execution")]
    LeaseOwnerMismatch,
    #[error("browser lease turn does not match the current execution")]
    LeaseTurnMismatch,
    #[error("browser snapshot revision changed: expected={expected}, provided={provided}")]
    SnapshotRevisionMismatch { expected: u64, provided: u64 },
    #[error("browser navigation revision changed: expected={expected}, provided={provided}")]
    NavigationRevisionMismatch { expected: u64, provided: u64 },
    #[error("browser navigation revision moved backwards: current={current}, received={received}")]
    NavigationRevisionRegression { current: u64, received: u64 },
    #[error("browser authority snapshot is invalid: {0}")]
    InvalidSnapshot(String),
}
