mod authority;
mod capability;
mod domain;
mod error;
mod host_client;
mod host_protocol;
mod navigation;

pub use authority::{
    AcquireBrowserLease, BROWSER_DURABLE_STATE_SCHEMA_VERSION, BrowserAuthority,
    BrowserAuthoritySnapshot, BrowserDurableState, BrowserDurableTab, BrowserPrimarySurface,
    BrowserSurfaceControlSnapshot, CreateBrowserSession, CreateBrowserTab, ValidateBrowserWrite,
    ValidatedBrowserWrite,
};
pub use capability::{
    BrowserCapabilityRejection, BrowserCapabilitySnapshot, BrowserCapabilityUnavailableReason,
    BrowserHostStatus, BrowserToolAccess, BrowserToolKind,
};
pub use domain::{
    BrowserAnnotation, BrowserAnnotationAnchor, BrowserAnnotationAuthor, BrowserAnnotationKind,
    BrowserAnnotationStatus, BrowserControlLease, BrowserDeviceType,
    BrowserElementAnnotationAnchor, BrowserLeaseEndReason, BrowserLeaseLifecycle,
    BrowserLeaseSelector, BrowserNormalizedRect, BrowserProfile, BrowserProfileKind,
    BrowserRegionAnnotationAnchor, BrowserSession, BrowserSessionLifecycle, BrowserTab,
    BrowserTabLifecycle, BrowserViewport, BrowserViewportMode, GoalControlBinding,
};
pub use error::BrowserAuthorityError;
pub use host_client::{
    BrowserHostClient, BrowserHostClientError, BrowserHostCommandReply, BrowserHostIncomingEvent,
};
pub use host_protocol::*;
pub use navigation::{
    BrowserNavigationUrlError, browser_navigation_origin, normalize_browser_page_state,
    validate_browser_navigation_url,
};

#[cfg(test)]
mod tests;
