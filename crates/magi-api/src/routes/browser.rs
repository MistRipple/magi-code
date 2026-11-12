use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header::USER_AGENT},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use magi_browser_authority::{
    BrowserAnnotation, BrowserAnnotationAnchor, BrowserAnnotationAuthor, BrowserAnnotationKind,
    BrowserAnnotationStatus, BrowserAuthority, BrowserElementAnnotationAnchor, BrowserHostCommand,
    BrowserHostCommandOutcome, BrowserHostCommandResult, BrowserHostControl,
    BrowserHostControlUpdate, BrowserHostHitTest, BrowserHostRect, BrowserHostStatus,
    BrowserNavigation, BrowserNormalizedRect, BrowserRegionAnnotationAnchor, BrowserSession,
    BrowserSessionLifecycle, BrowserSessionOwner, BrowserSurfaceControlSnapshot, BrowserTab,
    BrowserTabLifecycle, BrowserViewport, CreateBrowserSession, CreateBrowserTab,
    MAX_BROWSER_TABS_TOTAL, validate_browser_navigation_url_with,
};
use magi_core::{
    BrowserAnnotationId, BrowserProfileId, BrowserSessionId, BrowserTabId, EventId, SessionId,
    SessionLifecycleStatus, UtcMillis, WorkspaceId,
};
use magi_event_bus::EventEnvelope;
use serde::{Deserialize, Serialize};

use crate::{
    browser_image::crop_browser_screenshot,
    browser_tool_runtime::{
        browser_artifact_path, browser_tab_scope, close_browser_tab, publish_browser_event,
        require_session_scope,
    },
    errors::ApiError,
    scope_binding::{self as session_scope, SessionScope},
    session_activity::session_running_task_count,
    state::{ApiState, BrowserHostConnectionConfig, BrowserHostStatusSnapshot},
};

mod desktop_connection;
use desktop_connection::*;
mod capabilities;
use capabilities::*;
mod dto;
use dto::*;
mod sessions;
use sessions::*;
mod tabs;
use tabs::*;
mod annotations;
use annotations::*;
mod tab_control;
use tab_control::*;
mod host_sync;
use host_sync::*;

static BROWSER_SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static BROWSER_TAB_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static BROWSER_ANNOTATION_ARTIFACT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const BROWSER_READY_WAIT_TIMEOUT: Duration = Duration::from_secs(65);
const BROWSER_READY_POLL_INTERVAL: Duration = Duration::from_millis(50);
const BROWSER_SURFACE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const BROWSER_SURFACE_POLL_INTERVAL: Duration = Duration::from_millis(10);

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/browser/capabilities", get(capabilities))
        .route("/browser/settings", post(update_browser_settings))
        .route("/browser/resources", get(browser_resources))
        .route(
            "/browser/resources/reclaim",
            post(reclaim_browser_resources),
        )
        .route(
            "/browser/desktop/connection",
            get(get_desktop_connection)
                .post(register_desktop_connection)
                .delete(clear_desktop_connection),
        )
        .route("/browser/sessions", post(create_session))
        .route("/browser/sessions/current", get(get_current_session))
        .route("/browser/sessions/{browser_session_id}", get(get_session))
        .route(
            "/browser/sessions/{browser_session_id}",
            delete(close_session),
        )
        .route(
            "/browser/sessions/{browser_session_id}/tabs",
            post(create_tab),
        )
        .route(
            "/browser/sessions/{browser_session_id}/active-tab",
            post(set_active_tab),
        )
        .route("/browser/tabs/{tab_id}", delete(close_tab))
        .route("/browser/tabs/{tab_id}/activate", post(activate_tab))
        .route(
            "/browser/tabs/{tab_id}/control/release",
            post(release_user_control),
        )
        .route("/browser/tabs/{tab_id}/navigation", post(navigate_tab))
        .route("/browser/tabs/{tab_id}/screenshot", post(screenshot_tab))
        .route(
            "/browser/tabs/{tab_id}/annotations",
            get(list_annotations).post(create_annotation),
        )
        .route(
            "/browser/annotations/{annotation_id}",
            post(update_annotation_comment),
        )
        .route(
            "/browser/annotations/{annotation_id}/artifact",
            get(annotation_artifact),
        )
        .route(
            "/browser/artifacts/{session_id}/{file_name}",
            get(session_browser_artifact),
        )
}

#[cfg(test)]
mod tests;
