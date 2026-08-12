mod authority;
mod capability;
mod component;
mod control;
mod domain;
mod error;
mod host_client;
mod host_protocol;
mod navigation;

pub use authority::{
    AcquireBrowserLease, BROWSER_DURABLE_STATE_SCHEMA_VERSION, BrowserAuthority,
    BrowserAuthoritySnapshot, BrowserDurableState, BrowserDurableTab,
    BrowserProfileControlSnapshot, CreateBrowserSession, CreateBrowserTab, ValidateBrowserWrite,
    ValidatedBrowserWrite,
};
pub use capability::{
    BrowserCapabilityRejection, BrowserCapabilitySnapshot, BrowserCapabilityUnavailableReason,
    BrowserRuntimeComponentStatus, BrowserToolAccess, BrowserToolKind,
};
pub use component::{
    ActiveBrowserRuntime, BROWSER_RUNTIME_ACTIVE_FILE, BROWSER_RUNTIME_MANIFEST_FILE,
    BROWSER_RUNTIME_MANIFEST_FORMAT_VERSION, BROWSER_RUNTIME_RELEASE_FILE,
    BROWSER_RUNTIME_TRUST_FILE, BrowserRuntimeComponentError, BrowserRuntimeEntrypoints,
    BrowserRuntimeFile, BrowserRuntimeInstallOutcome, BrowserRuntimeManager,
    BrowserRuntimeManagerConfig, BrowserRuntimeManifest, BrowserRuntimeReleaseAssessment,
    BrowserRuntimeReleaseChannel, BrowserRuntimeSelfTest, BrowserRuntimeTarget,
    BrowserRuntimeTrustState, BrowserRuntimeUpdateLevel, SignedBrowserRuntimeRelease,
};
pub use control::{
    BrowserRuntimeComponentAction, BrowserRuntimeComponentOperation, BrowserRuntimeControlClient,
    BrowserRuntimeControlReceiver, BrowserRuntimeControlRequest, browser_runtime_control_channel,
};
pub use domain::{
    BrowserAnnotation, BrowserAnnotationAnchor, BrowserAnnotationAuthor, BrowserAnnotationKind,
    BrowserAnnotationStatus, BrowserControlLease, BrowserDeviceType,
    BrowserElementAnnotationAnchor, BrowserLeaseEndReason, BrowserLeaseLifecycle,
    BrowserLeaseSelector, BrowserNormalizedRect, BrowserProfile, BrowserProfileControlMode,
    BrowserProfileKind, BrowserRegionAnnotationAnchor, BrowserSession, BrowserSessionLifecycle,
    BrowserTab, BrowserTabLifecycle, BrowserViewport, BrowserViewportMode, GoalControlBinding,
};
pub use error::BrowserAuthorityError;
pub use host_client::{
    BrowserHostClient, BrowserHostClientError, BrowserHostCommandReply, BrowserHostIncomingEvent,
};
pub use host_protocol::*;
pub use navigation::{
    BrowserNavigationUrlError, normalize_browser_page_state, validate_browser_navigation_url,
};

#[cfg(test)]
mod tests;
