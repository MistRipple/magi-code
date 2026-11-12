use super::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CreateBrowserSessionRequest {
    pub(super) scope: crate::dto::SessionScopeKindDto,
    #[serde(default)]
    pub(super) workspace_id: Option<String>,
    #[serde(default)]
    pub(super) workspace_path: Option<String>,
    pub(super) session_id: String,
    #[serde(default)]
    pub(super) client_platform: Option<BrowserClientPlatform>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CurrentBrowserSessionQuery {
    pub(super) scope: crate::dto::SessionScopeKindDto,
    #[serde(default)]
    pub(super) workspace_id: Option<String>,
    #[serde(default)]
    pub(super) workspace_path: Option<String>,
    pub(super) session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct AnnotationArtifactQuery {
    pub(super) session_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CurrentBrowserSessionResponse {
    pub(super) session: Option<BrowserSessionResponse>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BrowserSessionResponse {
    pub(super) browser_session_id: BrowserSessionId,
    pub(super) owner: String,
    pub(super) workspace_id: Option<WorkspaceId>,
    pub(super) session_id: Option<SessionId>,
    pub(super) profile_id: BrowserProfileId,
    pub(super) lifecycle: BrowserSessionLifecycle,
    pub(super) active_tab_id: Option<BrowserTabId>,
    pub(super) tabs: Vec<BrowserTabResponse>,
    pub(super) runtime_epoch: u64,
    pub(super) revision: u64,
    pub(super) control_mode: String,
    pub(super) control_fence: u64,
    pub(super) agent_occupied: bool,
    pub(super) created_at: UtcMillis,
    pub(super) updated_at: UtcMillis,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BrowserResourceTabResponse {
    pub(super) tab_id: BrowserTabId,
    pub(super) browser_session_id: BrowserSessionId,
    /// 应用级 Tab 没有归属会话，前端据此展示常驻说明。
    pub(super) session_id: Option<SessionId>,
    pub(super) workspace_id: Option<WorkspaceId>,
    pub(super) scope: String,
    pub(super) session_title: Option<String>,
    pub(super) lifecycle: BrowserTabLifecycle,
    pub(super) url: String,
    pub(super) title: String,
    pub(super) created_at: UtcMillis,
    pub(super) updated_at: UtcMillis,
    pub(super) is_current_session: bool,
    pub(super) is_current_tab: bool,
    pub(super) session_running: bool,
    pub(super) can_reclaim: bool,
    pub(super) reclaim_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BrowserResourcesResponse {
    pub(super) max_tabs: usize,
    pub(super) live_tabs: usize,
    pub(super) available_tabs: usize,
    pub(super) reclaimable_tabs: usize,
    pub(super) tabs: Vec<BrowserResourceTabResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ReclaimBrowserResourcesRequest {
    pub(super) tab_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BrowserResourceReclaimSkip {
    pub(super) tab_id: String,
    pub(super) reason: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReclaimBrowserResourcesResponse {
    pub(super) reclaimed_count: usize,
    pub(super) reclaimed_tab_ids: Vec<BrowserTabId>,
    pub(super) skipped: Vec<BrowserResourceReclaimSkip>,
    pub(super) resources: BrowserResourcesResponse,
}

#[derive(Clone, Debug)]
pub(super) struct BrowserReclaimTarget {
    pub(super) tab_id: BrowserTabId,
    pub(super) browser_session_id: BrowserSessionId,
    pub(super) session_id: SessionId,
    pub(super) workspace_id: Option<WorkspaceId>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BrowserTabResponse {
    pub(super) tab_id: BrowserTabId,
    pub(super) browser_session_id: BrowserSessionId,
    pub(super) lifecycle: BrowserTabLifecycle,
    pub(super) url: String,
    pub(super) origin: Option<String>,
    pub(super) title: String,
    pub(super) navigation_revision: u64,
    pub(super) snapshot_revision: u64,
    pub(super) created_at: UtcMillis,
    pub(super) updated_at: UtcMillis,
    pub(super) annotations: Vec<BrowserAnnotationResponse>,
    #[serde(default)]
    pub(super) surface_id: Option<String>,
    #[serde(default)]
    pub(super) agent_occupied: bool,
    #[serde(default)]
    pub(super) control_fence: u64,
    /// 用户直接操作页面后保持控制，代理需等用户交还。
    #[serde(default)]
    pub(super) user_control_held: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BrowserViewportResponse {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) device_scale_factor_millis: u32,
    pub(super) device_type: magi_browser_authority::BrowserDeviceType,
}

impl From<BrowserViewport> for BrowserViewportResponse {
    fn from(viewport: BrowserViewport) -> Self {
        Self {
            width: viewport.width,
            height: viewport.height,
            device_scale_factor_millis: viewport.device_scale_factor_millis,
            device_type: viewport.device_type,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BrowserAnnotationResponse {
    pub(super) annotation_id: BrowserAnnotationId,
    pub(super) browser_session_id: BrowserSessionId,
    pub(super) tab_id: BrowserTabId,
    pub(super) sequence: u64,
    pub(super) author: BrowserAnnotationAuthor,
    pub(super) kind: BrowserAnnotationKind,
    pub(super) anchor: BrowserAnnotationAnchorResponse,
    pub(super) comment: String,
    pub(super) status: BrowserAnnotationStatus,
    pub(super) screenshot_artifact_id: Option<String>,
    pub(super) created_at: UtcMillis,
    pub(super) updated_at: UtcMillis,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum BrowserAnnotationAnchorResponse {
    Element(Box<BrowserElementAnnotationAnchorResponse>),
    Region(BrowserRegionAnnotationAnchorResponse),
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BrowserElementAnnotationAnchorResponse {
    pub(super) url: String,
    pub(super) origin: Option<String>,
    pub(super) frame_path: Vec<String>,
    pub(super) viewport: BrowserViewportResponse,
    pub(super) scroll_x: f64,
    pub(super) scroll_y: f64,
    pub(super) test_id: Option<String>,
    pub(super) stable_id: Option<String>,
    pub(super) aria_role: Option<String>,
    pub(super) aria_name: Option<String>,
    pub(super) tag_name: String,
    pub(super) text_excerpt: Option<String>,
    pub(super) css_path: String,
    pub(super) ancestor_fingerprint: String,
    pub(super) dom_fingerprint: String,
    pub(super) bounding_box: BrowserNormalizedRect,
    pub(super) snapshot_revision: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BrowserRegionAnnotationAnchorResponse {
    pub(super) url: String,
    pub(super) origin: Option<String>,
    pub(super) viewport: BrowserViewportResponse,
    pub(super) scroll_x: f64,
    pub(super) scroll_y: f64,
    pub(super) rect: BrowserNormalizedRect,
    pub(super) snapshot_revision: u64,
}

impl From<BrowserAnnotationAnchor> for BrowserAnnotationAnchorResponse {
    fn from(anchor: BrowserAnnotationAnchor) -> Self {
        match anchor {
            BrowserAnnotationAnchor::Element(anchor) => {
                let BrowserElementAnnotationAnchor {
                    url,
                    origin,
                    frame_path,
                    viewport,
                    scroll_x,
                    scroll_y,
                    test_id,
                    stable_id,
                    aria_role,
                    aria_name,
                    tag_name,
                    text_excerpt,
                    css_path,
                    ancestor_fingerprint,
                    dom_fingerprint,
                    bounding_box,
                    snapshot_revision,
                } = *anchor;
                Self::Element(Box::new(BrowserElementAnnotationAnchorResponse {
                    url,
                    origin,
                    frame_path,
                    viewport: viewport.into(),
                    scroll_x,
                    scroll_y,
                    test_id,
                    stable_id,
                    aria_role,
                    aria_name,
                    tag_name,
                    text_excerpt,
                    css_path,
                    ancestor_fingerprint,
                    dom_fingerprint,
                    bounding_box,
                    snapshot_revision,
                }))
            }
            BrowserAnnotationAnchor::Region(anchor) => {
                let BrowserRegionAnnotationAnchor {
                    url,
                    origin,
                    viewport,
                    scroll_x,
                    scroll_y,
                    rect,
                    snapshot_revision,
                } = anchor;
                Self::Region(BrowserRegionAnnotationAnchorResponse {
                    url,
                    origin,
                    viewport: viewport.into(),
                    scroll_x,
                    scroll_y,
                    rect,
                    snapshot_revision,
                })
            }
        }
    }
}

impl From<BrowserAnnotation> for BrowserAnnotationResponse {
    fn from(annotation: BrowserAnnotation) -> Self {
        Self {
            annotation_id: annotation.annotation_id,
            browser_session_id: annotation.browser_session_id,
            tab_id: annotation.tab_id,
            sequence: annotation.sequence,
            author: annotation.author,
            kind: annotation.kind,
            anchor: annotation.anchor.into(),
            comment: annotation.comment,
            status: annotation.status,
            screenshot_artifact_id: annotation.screenshot_artifact_id,
            created_at: annotation.created_at,
            updated_at: annotation.updated_at,
        }
    }
}

impl From<BrowserTab> for BrowserTabResponse {
    fn from(tab: BrowserTab) -> Self {
        Self {
            tab_id: tab.tab_id,
            browser_session_id: tab.browser_session_id,
            lifecycle: tab.lifecycle,
            url: tab.url,
            origin: tab.origin,
            title: tab.title,
            navigation_revision: tab.navigation_revision,
            snapshot_revision: tab.snapshot_revision,
            created_at: tab.created_at,
            updated_at: tab.updated_at,
            annotations: Vec::new(),
            surface_id: None,
            agent_occupied: false,
            control_fence: 0,
            user_control_held: false,
        }
    }
}
