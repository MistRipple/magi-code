//! Native DSH plugin platform port consumed by `magi-api` HTTP routes.
//! Concrete implementation lives in `magi-daemon` over `PluginRuntime` + `PluginManager`.

use crate::errors::ApiError;
use magi_app_server_protocol::generated::{
    PluginCandidatePreview, PluginInstalled, PluginManifest, PluginRouteCallRequest,
    PluginRouteCallResult, PluginUpstreamPreview, PluginUpstreamRequest,
};
use magi_plugin_system::PluginManager;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, future::Future, pin::Pin};

pub type NativePlatformFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Maps (ecosystem, package name) to (plugin ID, installed version).
pub type InstalledUpstreamPlugins = HashMap<(String, String), (String, String)>;

pub enum PluginInstallOutcome {
    Applied,
    Cancelled,
}

/// Daemon-owned inputs to the private route transport, never taken from an application header.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRouteSession {
    pub id: magi_core::SessionId,
    pub created_at: magi_core::UtcMillis,
    pub initial_directory: Option<String>,
    pub execution_directory: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginClientSlotsReport {
    #[serde(default)]
    pub instance_id: Option<String>,
    #[serde(default)]
    pub mounted_slots: Vec<String>,
    #[serde(default)]
    pub unmounted_slots: Vec<String>,
    #[serde(default)]
    pub has_settings_view: bool,
    #[serde(default)]
    pub main_keys: Vec<String>,
    #[serde(default)]
    /// Omitted until a document has observed a right-pane registration.
    pub right_pane_tabs: Option<Vec<PluginRightPaneTab>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginRightPaneTab {
    pub key: String,
    pub title: String,
    pub launcher_order: Option<i64>,
}

pub trait NativePluginPlatform: Send + Sync {
    /// Projects all installed native DSH packages plus any hard-cutover legacy entries.
    fn list_installed(&self, manager: &PluginManager) -> Result<Vec<PluginInstalled>, ApiError>;

    fn configuration_error(&self) -> Option<String>;

    /// Returns `(("dsh", package_name), (plugin_id, version))` for upstream search/update badges.
    fn installed_upstream(
        &self,
        manager: &PluginManager,
    ) -> Result<InstalledUpstreamPlugins, ApiError>;

    /// Projects manifests for currently enabled native DSH packages.
    fn enabled_manifests(&self, manager: &PluginManager) -> Result<Vec<PluginManifest>, ApiError>;

    /// Lists active slash commands published by the shared DSH host.
    fn enabled_commands(&self) -> Vec<Value>;

    /// Performs static non-executing preview of an upstream DSH package.
    fn preview_upstream(
        &self,
        request: PluginUpstreamRequest,
    ) -> NativePlatformFuture<'_, Result<PluginUpstreamPreview, ApiError>>;

    /// Installs or upgrades an upstream DSH original package through `PluginManagementService`.
    fn install_upstream(
        &self,
        request: PluginUpstreamRequest,
        request_id: Option<String>,
    ) -> NativePlatformFuture<'_, Result<PluginInstallOutcome, ApiError>>;

    fn cancel_install(
        &self,
        request_id: String,
    ) -> NativePlatformFuture<'_, Result<Value, ApiError>>;

    /// Performs static non-executing preview of a `.tgz` original archive (local/address/inline).
    fn preview_archive(
        &self,
        source: String,
        archive_bytes: Vec<u8>,
    ) -> NativePlatformFuture<'_, Result<PluginCandidatePreview, ApiError>>;

    /// Installs or upgrades a `.tgz` original archive after digest confirmation.
    fn install_archive(
        &self,
        source: String,
        archive_bytes: Vec<u8>,
        expected_digest: String,
    ) -> NativePlatformFuture<'_, Result<(), ApiError>>;

    /// Toggles bundle enabled state in `cordis.patch.yml` via `PluginManagementService`.
    fn set_enabled(
        &self,
        plugin_id: String,
        enabled: bool,
        expected_digest: Option<String>,
    ) -> NativePlatformFuture<'_, Result<(), ApiError>>;

    /// Uninstalls a native DSH package (or removes a hard-cutover legacy record).
    fn uninstall(&self, plugin_id: String) -> NativePlatformFuture<'_, Result<(), ApiError>>;

    /// Serves sandboxed client host HTML/JS/CSS or original `./client` bundle assets.
    fn serve_ui_asset(
        &self,
        manager: &PluginManager,
        plugin_id: &str,
        path: &str,
        expected_digest: Option<&str>,
    ) -> Result<(&'static str, Vec<u8>), ApiError>;

    /// Serves package icon bytes if declared and present in the original package directory.
    fn serve_icon(
        &self,
        manager: &PluginManager,
        plugin_id: &str,
    ) -> Result<Option<(&'static str, Vec<u8>)>, ApiError>;

    /// Forwards sandboxed client HTTP/Typert requests through `PluginWebGateway` to the shared host.
    fn call_gateway(
        &self,
        plugin_id: String,
        request: PluginRouteCallRequest,
        session: Option<PluginRouteSession>,
    ) -> NativePlatformFuture<'_, Result<PluginRouteCallResult, ApiError>>;

    /// Forwards an admitted plugin page WebSocket upgrade (`/api/remote.mux`) through `PluginWebGateway`.
    fn forward_gateway_upgrade(
        &self,
        _plugin_id: String,
        _expected_digest: Option<String>,
        _expected_instance_id: Option<String>,
        _request: axum::http::Request<axum::body::Body>,
    ) -> NativePlatformFuture<'_, Result<axum::response::Response, ApiError>> {
        Box::pin(async {
            Err(ApiError::Conflict("当前插件平台不支持流式网关连接".into()))
        })
    }

    /// Records runtime slot registrations observed by `dsh-client-host`.
    fn report_client_slots(
        &self,
        plugin_id: &str,
        report: PluginClientSlotsReport,
    ) -> Result<(), ApiError>;

    /// Manually restarts the shared DSH host when stopped by the 3-crash circuit breaker.
    fn restart_host(&self) -> NativePlatformFuture<'_, Result<(), ApiError>>;
}
