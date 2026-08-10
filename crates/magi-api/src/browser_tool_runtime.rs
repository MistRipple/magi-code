use std::{
    future::Future,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use magi_browser_runtime::{
    AcquireBrowserLease, BrowserCapabilitySnapshot, BrowserDeviceType, BrowserHostClient,
    BrowserHostCommand, BrowserHostCommandError, BrowserHostCommandOutcome,
    BrowserHostCommandResult, BrowserHostControl, BrowserHostControlMode, BrowserLeaseEndReason,
    BrowserNavigation, BrowserSnapshotTarget, BrowserToolAccess, BrowserToolKind, BrowserViewport,
    BrowserViewportMode, CreateBrowserSession, CreateBrowserTab, GoalControlBinding,
    ValidateBrowserWrite, validate_browser_navigation_url,
};
use magi_core::{
    BrowserLeaseId, BrowserProfileId, BrowserSessionId, BrowserTabId, EventId, ExecutionOwnership,
    ExecutionResultStatus, SessionId, ToolCallId, UtcMillis, WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_session_store::SessionStore;
use serde_json::{Map, Value, json};

use crate::{RuntimeStatePersistence, state::BrowserRuntimeStatusSnapshot};

const DEFAULT_BROWSER_PROFILE_ID: &str = "browser-profile-default";
const LEASE_TTL: Duration = Duration::from_secs(5 * 60);
static BROWSER_LEASE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static BROWSER_TAB_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static BROWSER_ARTIFACT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static BROWSER_EVENT_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct BrowserExecutionIdentity {
    owner: ExecutionOwnership,
    turn_id: String,
    goal_binding: Option<GoalControlBinding>,
}

#[derive(Clone, Copy)]
struct BrowserToolCallScope<'a> {
    context: &'a magi_tool_runtime::ToolExecutionContext,
    session_id: &'a SessionId,
    workspace_id: &'a WorkspaceId,
    call_id: &'a str,
}

#[derive(Clone)]
pub struct BrowserToolRuntimeDependencies {
    pub authority: Arc<Mutex<magi_browser_runtime::BrowserAuthority>>,
    pub write_lock: Arc<Mutex<()>>,
    pub control_lock: Arc<tokio::sync::Mutex<()>>,
    pub state_writable: Arc<std::sync::atomic::AtomicBool>,
    pub runtime_status: Arc<RwLock<BrowserRuntimeStatusSnapshot>>,
    pub host_client: Arc<RwLock<Option<BrowserHostClient>>>,
    pub event_bus: Arc<InMemoryEventBus>,
    pub session_store: Arc<SessionStore>,
    pub persistence: Option<Arc<RuntimeStatePersistence>>,
}

impl BrowserToolRuntimeDependencies {
    pub fn capabilities(&self, session_id: Option<&SessionId>) -> BrowserCapabilitySnapshot {
        let runtime = self
            .runtime_status
            .read()
            .expect("browser runtime status lock poisoned")
            .clone();
        BrowserCapabilitySnapshot {
            revision: runtime.revision,
            in_app_browser_enabled: runtime.in_app_browser_enabled,
            browser_use_enabled: runtime.browser_use_enabled,
            runtime_status: runtime.component_status,
            host_protocol_compatible: runtime.host_protocol_compatible,
            access_profile: session_id
                .and_then(|id| self.session_store.active_goal(id))
                .map(|goal| goal.access_profile)
                .unwrap_or(magi_core::AccessProfile::Restricted),
        }
    }

    fn publish_browser_event(
        &self,
        event_type: &str,
        session: &magi_browser_runtime::BrowserSession,
        payload: Value,
    ) {
        let now = UtcMillis::now();
        let sequence = BROWSER_EVENT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        self.event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!(
                    "event-{}-{}-{sequence}",
                    event_type.replace('.', "-"),
                    now.0
                )),
                event_type,
                payload,
            )
            .with_context(EventContext {
                workspace_id: Some(session.workspace_id.clone()),
                session_id: Some(session.session_id.clone()),
                ..EventContext::default()
            }),
        );
    }

    fn publish_tab_event(&self, event_type: &str, tab: &magi_browser_runtime::BrowserTab) {
        let session = self
            .authority
            .lock()
            .expect("browser authority lock poisoned")
            .session(&tab.browser_session_id)
            .cloned();
        let Some(session) = session else {
            return;
        };
        self.publish_browser_event(
            event_type,
            &session,
            json!({
                "browser_session_id": tab.browser_session_id,
                "tab_id": tab.tab_id,
                "lifecycle": tab.lifecycle,
                "url": tab.url,
                "title": tab.title,
                "navigation_revision": tab.navigation_revision,
                "viewport": tab.viewport,
                "viewport_mode": tab.viewport_mode,
            }),
        );
    }

    pub fn execute(
        &self,
        tool_call_id: &ToolCallId,
        tool_name: &str,
        input: &str,
        context: &magi_tool_runtime::ToolExecutionContext,
    ) -> (String, ExecutionResultStatus) {
        let Some(session_id) = context.session_id.clone() else {
            return failure(
                tool_name,
                "browser_session_scope_missing",
                "浏览器工具缺少 session_id",
            );
        };
        let Some(workspace_id) = context.workspace_id.clone() else {
            return failure(
                tool_name,
                "browser_workspace_scope_missing",
                "浏览器工具缺少 workspace_id",
            );
        };
        let Ok(object) = serde_json::from_str::<Value>(input) else {
            return failure(
                tool_name,
                "invalid_arguments",
                "浏览器工具参数必须是 JSON 对象",
            );
        };
        let Some(arguments) = object.as_object() else {
            return failure(
                tool_name,
                "invalid_arguments",
                "浏览器工具参数必须是 JSON 对象",
            );
        };
        let Some(kind) = BrowserToolKind::ALL
            .into_iter()
            .find(|kind| kind.name() == tool_name)
        else {
            return failure(tool_name, "unknown_browser_tool", "未知的浏览器工具");
        };
        let mut capability = self.capabilities(Some(&session_id));
        capability.access_profile = context.access_profile;
        let requested_access = browser_tool_requested_access(kind, arguments);
        let Some(catalog_revision) = context.browser_capability_revision else {
            return failure(
                tool_name,
                "browser_capability_snapshot_missing",
                "浏览器工具调用缺少当前模型轮次的能力快照",
            );
        };
        if let Err(error) = capability.allows_execution(catalog_revision, kind, requested_access) {
            return failure(
                tool_name,
                "browser_capability_unavailable",
                &error.to_string(),
            );
        }
        let client = self
            .host_client
            .read()
            .expect("browser Host client lock poisoned")
            .clone();
        let Some(client) = client else {
            return failure(
                tool_name,
                "browser_runtime_unavailable",
                "浏览器 Host 尚未就绪",
            );
        };
        let call_id = tool_call_id.to_string();
        let scope = BrowserToolCallScope {
            context,
            session_id: &session_id,
            workspace_id: &workspace_id,
            call_id: &call_id,
        };
        let result = block_on(self.execute_async(tool_name, arguments, scope, client));
        match result {
            Ok(payload) => (payload, ExecutionResultStatus::Succeeded),
            Err(error) => failure_with_error(tool_name, &error),
        }
    }

    async fn execute_async(
        &self,
        tool_name: &str,
        arguments: &Map<String, Value>,
        scope: BrowserToolCallScope<'_>,
        client: BrowserHostClient,
    ) -> Result<String, BrowserToolError> {
        let BrowserToolCallScope {
            session_id,
            workspace_id,
            call_id,
            ..
        } = scope;
        let browser_session = self.ensure_session(session_id, workspace_id)?;
        if tool_name == "browser_tabs" {
            if string_arg(arguments, "action")?.as_str() != "list" {
                self.reclaim_agent_control(&client, &browser_session)
                    .await?;
            }
            return self
                .execute_tabs(arguments, &browser_session, scope, &client)
                .await;
        }
        let tab = self
            .ensure_tab(&browser_session, arguments, &client)
            .await?;
        match tool_name {
            "browser_viewport" => {
                let action = string_arg(arguments, "action")?;
                if action == "get" {
                    return Ok(json!({
                        "tool": tool_name,
                        "status": "succeeded",
                        "tab_id": tab.tab_id,
                        "mode": tab.viewport_mode,
                        "viewport": tab.viewport,
                    })
                    .to_string());
                }
                if action != "set" {
                    return Err(BrowserToolError::new(
                        "invalid_viewport_action",
                        "browser_viewport action 不合法",
                    ));
                }
                let width = u32_arg(arguments, "width")?;
                let height = u32_arg(arguments, "height")?;
                validate_viewport_dimensions(width, height)?;
                let requested_device_type =
                    match optional_string(arguments, "device_type").as_deref() {
                        Some("desktop") => Some(BrowserDeviceType::Desktop),
                        Some("mobile") => Some(BrowserDeviceType::Mobile),
                        Some(_) => {
                            return Err(BrowserToolError::new(
                                "browser_device_type_invalid",
                                "device_type 必须是 desktop 或 mobile",
                            ));
                        }
                        None => None,
                    };
                let device_type = BrowserDeviceType::for_dimensions(width);
                if requested_device_type.is_some_and(|requested| requested != device_type) {
                    return Err(BrowserToolError::new(
                        "browser_device_type_mismatch",
                        "device_type 与 width 不一致：320-600 必须为 mobile，601 以上必须为 desktop",
                    ));
                }
                let _control_guard = self.control_lock.lock().await;
                let tab = tab_in_session(self, &browser_session, &tab.tab_id)?;
                let viewport = BrowserViewport {
                    width,
                    height,
                    device_scale_factor_millis: tab.viewport.device_scale_factor_millis,
                    device_type,
                };
                if tab.viewport != viewport {
                    let reply = client
                        .request(BrowserHostCommand::SetLogicalViewport {
                            tab_id: tab.tab_id.clone(),
                            viewport: magi_browser_runtime::BrowserLogicalViewport {
                                width,
                                height,
                                device_scale_factor_millis: viewport.device_scale_factor_millis,
                                device_type: viewport.device_type,
                            },
                        })
                        .await
                        .map_err(|error| {
                            BrowserToolError::new("browser_host_disconnected", error.to_string())
                        })?;
                    succeeded_result(reply.response.outcome, "调整浏览器页面视口失败")?;
                }
                let updated = self.mutate_transient(|authority| {
                    authority.set_tab_viewport(
                        &tab.tab_id,
                        viewport,
                        BrowserViewportMode::Fixed,
                        UtcMillis::now(),
                    )
                })?;
                self.publish_tab_event("browser.tab.viewport_changed", &updated);
                Ok(json!({
                    "tool": tool_name,
                    "status": "succeeded",
                    "tab_id": updated.tab_id,
                    "mode": updated.viewport_mode,
                    "viewport": updated.viewport,
                })
                .to_string())
            }
            "browser_navigate" => {
                let action = string_arg(arguments, "action")?;
                let navigation = match action.as_str() {
                    "url" => {
                        let url = string_arg(arguments, "url")?;
                        validate_browser_navigation_url(&url).map_err(|error| {
                            BrowserToolError::new(
                                "browser_navigation_url_rejected",
                                format!("浏览器导航 URL 不合法: {error}"),
                            )
                        })?;
                        BrowserNavigation::Url { url }
                    }
                    "back" => BrowserNavigation::Back,
                    "forward" => BrowserNavigation::Forward,
                    "reload" => BrowserNavigation::Reload,
                    _ => return Err(BrowserToolError::new("invalid_navigation", "action 不合法")),
                };
                let control = self
                    .prepare_agent_write(&client, &browser_session, &tab, scope)
                    .await?;
                let reply = client
                    .request(BrowserHostCommand::Navigate {
                        tab_id: tab.tab_id.clone(),
                        control,
                        navigation,
                    })
                    .await
                    .map_err(|error| {
                        BrowserToolError::new("browser_host_disconnected", error.to_string())
                    })?;
                let page = page_state(reply.response.outcome, "浏览器导航失败")?;
                let updated = self.apply_page_state(&tab.tab_id, page)?;
                Ok(json!({ "tool": tool_name, "status": "succeeded", "tab": updated }).to_string())
            }
            "browser_snapshot" => {
                let reply = client
                    .request(BrowserHostCommand::Snapshot {
                        tab_id: tab.tab_id.clone(),
                        limits: Default::default(),
                        subtree_ref: optional_string(arguments, "subtree_ref"),
                    })
                    .await
                    .map_err(|error| {
                        BrowserToolError::new("browser_host_disconnected", error.to_string())
                    })?;
                let BrowserHostCommandResult::Snapshot(snapshot) =
                    succeeded_result(reply.response.outcome, "浏览器快照失败")?
                else {
                    return Err(BrowserToolError::new(
                        "browser_snapshot_failed",
                        "浏览器快照结果无效",
                    ));
                };
                self.apply_snapshot_revision(&tab.tab_id, snapshot.snapshot_revision)?;
                Ok(serde_json::to_string(&json!({
                    "tool": tool_name,
                    "status": "succeeded",
                    "snapshot": snapshot,
                }))
                .map_err(|error| BrowserToolError::new("serialize_failed", error.to_string()))?)
            }
            "browser_click" | "browser_type" | "browser_press" | "browser_scroll" => {
                let control = self
                    .prepare_agent_write(&client, &browser_session, &tab, scope)
                    .await?;
                let command = match tool_name {
                    "browser_click" => BrowserHostCommand::Click {
                        tab_id: tab.tab_id.clone(),
                        control,
                        target: snapshot_target(arguments)?,
                    },
                    "browser_type" => BrowserHostCommand::Type {
                        tab_id: tab.tab_id.clone(),
                        control,
                        target: snapshot_target(arguments)?,
                        text: string_arg(arguments, "text")?,
                        replace: arguments
                            .get("replace")
                            .and_then(Value::as_bool)
                            .unwrap_or(true),
                    },
                    "browser_press" => BrowserHostCommand::Press {
                        tab_id: tab.tab_id.clone(),
                        control,
                        key: string_arg(arguments, "key")?,
                    },
                    _ => BrowserHostCommand::Scroll {
                        tab_id: tab.tab_id.clone(),
                        control,
                        target: optional_snapshot_target(arguments)?,
                        delta_x: number_arg(arguments, "delta_x").unwrap_or(0.0),
                        delta_y: number_arg(arguments, "delta_y").unwrap_or(0.0),
                    },
                };
                let reply = client.request(command).await.map_err(|error| {
                    BrowserToolError::new("browser_host_disconnected", error.to_string())
                })?;
                let page = page_state(reply.response.outcome, "浏览器交互失败")?;
                let updated = self.apply_page_state(&tab.tab_id, page)?;
                Ok(json!({ "tool": tool_name, "status": "succeeded", "tab": updated }).to_string())
            }
            "browser_screenshot" => {
                let target = optional_snapshot_target(arguments)?;
                if let Some(target) = target.as_ref() {
                    self.validate_snapshot(
                        &browser_session.browser_session_id,
                        &tab.tab_id,
                        target.snapshot_revision,
                    )?;
                }
                let reply = client
                    .request(BrowserHostCommand::Screenshot {
                        tab_id: tab.tab_id.clone(),
                        target,
                        clip: None,
                        full_page: arguments
                            .get("full_page")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        format: magi_browser_runtime::BrowserScreenshotFormat::Png,
                    })
                    .await
                    .map_err(|error| {
                        BrowserToolError::new("browser_host_disconnected", error.to_string())
                    })?;
                let BrowserHostCommandResult::BinaryPayload(metadata) =
                    succeeded_result(reply.response.outcome, "浏览器截图失败")?
                else {
                    return Err(BrowserToolError::new(
                        "browser_screenshot_failed",
                        "浏览器截图结果无效",
                    ));
                };
                let bytes = reply.binary.ok_or_else(|| {
                    BrowserToolError::new("browser_binary_missing", "浏览器截图缺少二进制内容")
                })?;
                let path = self.persist_artifact(session_id, call_id, &bytes)?;
                Ok(json!({ "tool": tool_name, "status": "succeeded", "path": path, "mime": metadata.mime_type, "bytes": bytes.len(), "sha256": metadata.sha256 }).to_string())
            }
            _ => Err(BrowserToolError::new(
                "unknown_browser_tool",
                "未知的浏览器工具",
            )),
        }
    }

    fn ensure_session(
        &self,
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
    ) -> Result<magi_browser_runtime::BrowserSession, BrowserToolError> {
        let current = self
            .authority
            .lock()
            .expect("browser authority lock poisoned")
            .session_for_magi_session(session_id)
            .cloned();
        if let Some(session) = current {
            if &session.workspace_id != workspace_id {
                return Err(BrowserToolError::new(
                    "browser_workspace_scope_mismatch",
                    "浏览器会话与当前工作区不匹配",
                ));
            }
            return Ok(session);
        }

        let browser_session_id =
            BrowserSessionId::new(format!("browser-tool-session-{session_id}"));
        let session = self.mutate(|authority| {
            authority.create_session(CreateBrowserSession {
                browser_session_id: browser_session_id.clone(),
                workspace_id: workspace_id.clone(),
                session_id: session_id.clone(),
                profile_id: BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID),
                now: UtcMillis::now(),
            })?;
            authority.transition_session(
                &browser_session_id,
                magi_browser_runtime::BrowserSessionLifecycle::Ready,
                UtcMillis::now(),
            )
        })?;
        self.publish_browser_event(
            "browser.session.created",
            &session,
            json!({
                "browser_session_id": session.browser_session_id,
                "lifecycle": session.lifecycle,
                "revision": session.revision,
            }),
        );
        Ok(session)
    }

    async fn ensure_tab(
        &self,
        session: &magi_browser_runtime::BrowserSession,
        arguments: &Map<String, Value>,
        client: &BrowserHostClient,
    ) -> Result<magi_browser_runtime::BrowserTab, BrowserToolError> {
        let requested_tab_id = optional_string(arguments, "tab_id").map(BrowserTabId::new);
        let initial_url = "about:blank".to_string();
        validate_browser_navigation_url(&initial_url).map_err(|error| {
            BrowserToolError::new(
                "browser_navigation_url_rejected",
                format!("浏览器导航 URL 不合法: {error}"),
            )
        })?;
        let tab = {
            let authority = self
                .authority
                .lock()
                .expect("browser authority lock poisoned");
            if let Some(id) = requested_tab_id.as_ref() {
                let tab = authority.tab(id).ok_or_else(|| {
                    BrowserToolError::new("browser_tab_not_found", "指定的浏览器 Tab 不存在")
                })?;
                if tab.browser_session_id != session.browser_session_id {
                    return Err(BrowserToolError::new(
                        "browser_tab_scope_mismatch",
                        "指定的浏览器 Tab 不属于当前浏览器会话",
                    ));
                }
                if !matches!(
                    tab.lifecycle,
                    magi_browser_runtime::BrowserTabLifecycle::Ready
                        | magi_browser_runtime::BrowserTabLifecycle::Suspended
                        | magi_browser_runtime::BrowserTabLifecycle::Crashed
                ) {
                    return Err(BrowserToolError::new(
                        "browser_tab_not_ready",
                        "指定的浏览器 Tab 当前不可用",
                    ));
                }
                Some(tab.clone())
            } else {
                session
                    .active_tab_id
                    .as_ref()
                    .and_then(|id| authority.tab(id).cloned())
                    .filter(|tab| {
                        matches!(
                            tab.lifecycle,
                            magi_browser_runtime::BrowserTabLifecycle::Ready
                                | magi_browser_runtime::BrowserTabLifecycle::Suspended
                                | magi_browser_runtime::BrowserTabLifecycle::Crashed
                        )
                    })
            }
        };
        if let Some(tab) = tab {
            return self.materialize_tab(tab, client).await;
        }
        let tab_id = BrowserTabId::new(format!(
            "browser-tool-tab-{}-{}",
            UtcMillis::now().0,
            BROWSER_TAB_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let tab = self.mutate(|authority| {
            authority.create_tab(CreateBrowserTab {
                tab_id: tab_id.clone(),
                browser_session_id: session.browser_session_id.clone(),
                url: initial_url.clone(),
                viewport: BrowserViewport::default(),
                now: UtcMillis::now(),
            })
        })?;
        let reply = match client
            .request(BrowserHostCommand::CreatePage {
                tab_id: tab_id.clone(),
                initial_url,
                viewport: magi_browser_runtime::HostViewport {
                    width: tab.viewport.width,
                    height: tab.viewport.height,
                    surface_width: tab.viewport.width,
                    surface_height: tab.viewport.height,
                    device_scale_factor_millis: tab.viewport.device_scale_factor_millis,
                    device_type: tab.viewport.device_type,
                },
                navigation_revision: 0,
                snapshot_revision: 0,
                allow_streaming_eviction: false,
            })
            .await
        {
            Ok(reply) => reply,
            Err(error) => {
                self.mark_tab_crashed(&tab_id);
                return Err(BrowserToolError::new(
                    "browser_host_disconnected",
                    error.to_string(),
                ));
            }
        };
        let page = match page_state(reply.response.outcome, "创建浏览器页面失败") {
            Ok(page) => page,
            Err(error) => {
                self.mark_tab_crashed(&tab_id);
                return Err(error);
            }
        };
        let tab = self.apply_page_state(&tab_id, page)?;
        self.publish_tab_event("browser.tab.created", &tab);
        Ok(tab)
    }

    async fn materialize_tab(
        &self,
        tab: magi_browser_runtime::BrowserTab,
        client: &BrowserHostClient,
    ) -> Result<magi_browser_runtime::BrowserTab, BrowserToolError> {
        if !matches!(
            tab.lifecycle,
            magi_browser_runtime::BrowserTabLifecycle::Ready
                | magi_browser_runtime::BrowserTabLifecycle::Suspended
                | magi_browser_runtime::BrowserTabLifecycle::Crashed
        ) {
            return Err(BrowserToolError::new(
                "browser_tab_not_ready",
                "指定的浏览器 Tab 当前不可用",
            ));
        }
        let tab = if tab.lifecycle == magi_browser_runtime::BrowserTabLifecycle::Crashed {
            self.mutate(|authority| {
                authority.transition_tab(
                    &tab.tab_id,
                    magi_browser_runtime::BrowserTabLifecycle::Suspended,
                    UtcMillis::now(),
                )
            })?
        } else {
            tab
        };
        let reply = client
            .request(BrowserHostCommand::RestorePage {
                tab_id: tab.tab_id.clone(),
                initial_url: tab.url.clone(),
                viewport: magi_browser_runtime::HostViewport {
                    width: tab.viewport.width,
                    height: tab.viewport.height,
                    surface_width: tab.viewport.width,
                    surface_height: tab.viewport.height,
                    device_scale_factor_millis: tab.viewport.device_scale_factor_millis,
                    device_type: tab.viewport.device_type,
                },
                navigation_revision: tab.navigation_revision,
                snapshot_revision: tab.snapshot_revision,
                allow_streaming_eviction: false,
            })
            .await
            .map_err(|error| {
                BrowserToolError::new("browser_host_disconnected", error.to_string())
            })?;
        let page = page_state(reply.response.outcome, "恢复浏览器 Tab 失败")?;
        self.apply_page_state(&tab.tab_id, page)
    }

    async fn prepare_agent_write(
        &self,
        client: &BrowserHostClient,
        session: &magi_browser_runtime::BrowserSession,
        tab: &magi_browser_runtime::BrowserTab,
        scope: BrowserToolCallScope<'_>,
    ) -> Result<BrowserHostControl, BrowserToolError> {
        let _control_guard = self.control_lock.lock().await;
        self.reclaim_agent_control_locked(client, session).await?;
        let (lease, identity) = self.acquire_or_reuse_lease(
            session,
            scope.context,
            scope.session_id,
            scope.workspace_id,
            scope.call_id,
        )?;
        let validated = self
            .authority
            .lock()
            .expect("browser authority lock poisoned")
            .validate_write(ValidateBrowserWrite {
                lease_id: &lease.lease_id,
                fence: lease.fence,
                browser_session_id: &session.browser_session_id,
                tab_id: &tab.tab_id,
                owner: &identity.owner,
                turn_id: &identity.turn_id,
                goal_binding: identity.goal_binding.as_ref(),
                now: UtcMillis::now(),
            })
            .map_err(|error| {
                BrowserToolError::new("browser_control_lease_invalid", error.to_string())
            })?;
        let reply = client
            .request(BrowserHostCommand::UpdateControl {
                fence: validated.fence,
                mode: BrowserHostControlMode::Agent,
            })
            .await
            .map_err(|error| {
                BrowserToolError::new("browser_host_disconnected", error.to_string())
            })?;
        succeeded_result(reply.response.outcome, "同步浏览器控制权失败")?;
        Ok(BrowserHostControl::Agent {
            lease_id: lease.lease_id,
            fence: validated.fence,
        })
    }

    async fn reclaim_agent_control(
        &self,
        client: &BrowserHostClient,
        session: &magi_browser_runtime::BrowserSession,
    ) -> Result<(), BrowserToolError> {
        let _control_guard = self.control_lock.lock().await;
        self.reclaim_agent_control_locked(client, session).await
    }

    async fn reclaim_agent_control_locked(
        &self,
        client: &BrowserHostClient,
        session: &magi_browser_runtime::BrowserSession,
    ) -> Result<(), BrowserToolError> {
        let control = self.mutate(|authority| {
            let current = authority.profile_control_snapshot(&session.profile_id)?;
            if current.mode == magi_browser_runtime::BrowserProfileControlMode::Agent {
                return Ok(None);
            }
            authority.release_user_control(&session.profile_id)?;
            Ok(Some(
                authority.profile_control_snapshot(&session.profile_id)?,
            ))
        })?;
        let Some(control) = control else {
            return Ok(());
        };
        let reply = client
            .request(BrowserHostCommand::UpdateControl {
                fence: control.fence,
                mode: BrowserHostControlMode::Agent,
            })
            .await
            .map_err(|error| {
                BrowserToolError::new("browser_host_disconnected", error.to_string())
            })?;
        succeeded_result(reply.response.outcome, "恢复浏览器代理控制权失败")?;
        self.publish_browser_event(
            "browser.control.changed",
            session,
            json!({
                "browser_session_id": session.browser_session_id,
                "profile_id": session.profile_id,
                "mode": control.mode,
                "fence": control.fence,
            }),
        );
        Ok(())
    }

    fn acquire_or_reuse_lease(
        &self,
        session: &magi_browser_runtime::BrowserSession,
        context: &magi_tool_runtime::ToolExecutionContext,
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
        call_id: &str,
    ) -> Result<
        (
            magi_browser_runtime::BrowserControlLease,
            BrowserExecutionIdentity,
        ),
        BrowserToolError,
    > {
        let mut ownership = self
            .session_store
            .execution_ownership(session_id)
            .unwrap_or(ExecutionOwnership {
                session_id: Some(session_id.clone()),
                workspace_id: Some(workspace_id.clone()),
                mission_id: None,
                task_id: context.task_id.clone(),
                worker_id: context.worker_id.clone(),
                execution_chain_ref: None,
            });
        ownership.session_id = Some(session_id.clone());
        ownership.workspace_id = Some(workspace_id.clone());
        if context.task_id.is_some() {
            ownership.task_id = context.task_id.clone();
        }
        if context.worker_id.is_some() {
            ownership.worker_id = context.worker_id.clone();
        }
        let turn_id = self
            .session_store
            .runtime_sidecar(session_id)
            .and_then(|sidecar| sidecar.current_turn.map(|turn| turn.turn_id))
            .unwrap_or_else(|| format!("browser-turn-{call_id}"));
        let goal_binding =
            self.session_store
                .active_goal(session_id)
                .map(|goal| GoalControlBinding {
                    goal_id: goal.goal_id,
                    control_revision: goal.control_revision,
                });
        let identity = BrowserExecutionIdentity {
            owner: ownership.clone(),
            turn_id: turn_id.clone(),
            goal_binding: goal_binding.clone(),
        };
        let now = UtcMillis::now();
        let _write_guard = self.write_lock.lock().expect("browser write lock poisoned");
        let mut authority = self
            .authority
            .lock()
            .expect("browser authority lock poisoned");
        if let Some(lease) = authority
            .active_lease_for_profile(&session.profile_id)
            .cloned()
        {
            if lease.owner.session_id == ownership.session_id
                && lease.owner.task_id == ownership.task_id
                && lease.owner.worker_id == ownership.worker_id
                && lease.turn_id == turn_id
                && lease.goal_binding == goal_binding
                && now < lease.expires_at
            {
                return Ok((lease, identity));
            }
            if lease.owner.session_id == ownership.session_id
                && lease.owner.task_id == ownership.task_id
                && lease.owner.worker_id == ownership.worker_id
            {
                authority
                    .revoke_lease(&lease.lease_id, BrowserLeaseEndReason::TurnStopped, now)
                    .map_err(|error| {
                        BrowserToolError::new("browser_control_lease_failed", error.to_string())
                    })?;
            } else {
                return Err(BrowserToolError::new(
                    "browser_control_lease_conflict",
                    "浏览器 Profile 当前由另一个执行者控制",
                ));
            }
        }
        let lease = authority
            .acquire_lease(AcquireBrowserLease {
                lease_id: BrowserLeaseId::new(format!(
                    "browser-lease-{}-{}",
                    now.0,
                    BROWSER_LEASE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
                )),
                profile_id: session.profile_id.clone(),
                browser_session_id: session.browser_session_id.clone(),
                owner: ownership,
                turn_id,
                goal_binding,
                acquired_at: now,
                expires_at: UtcMillis(now.0.saturating_add(LEASE_TTL.as_millis() as u64)),
            })
            .map_err(|error| {
                BrowserToolError::new("browser_control_lease_failed", error.to_string())
            })?;
        let _ = call_id;
        Ok((lease, identity))
    }

    fn mutate<T>(
        &self,
        mutation: impl FnOnce(
            &mut magi_browser_runtime::BrowserAuthority,
        ) -> Result<T, magi_browser_runtime::BrowserAuthorityError>,
    ) -> Result<T, BrowserToolError> {
        self.mutate_with_persistence(true, mutation)
    }

    /// 浏览器视口属于当前 Browser Tab 的运行态，不得写入会话持久状态。
    fn mutate_transient<T>(
        &self,
        mutation: impl FnOnce(
            &mut magi_browser_runtime::BrowserAuthority,
        ) -> Result<T, magi_browser_runtime::BrowserAuthorityError>,
    ) -> Result<T, BrowserToolError> {
        self.mutate_with_persistence(false, mutation)
    }

    fn mutate_with_persistence<T>(
        &self,
        persist: bool,
        mutation: impl FnOnce(
            &mut magi_browser_runtime::BrowserAuthority,
        ) -> Result<T, magi_browser_runtime::BrowserAuthorityError>,
    ) -> Result<T, BrowserToolError> {
        if !self
            .state_writable
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(BrowserToolError::new(
                "browser_state_not_writable",
                "浏览器状态当前不可持久化",
            ));
        }
        let _write_guard = self.write_lock.lock().expect("browser write lock poisoned");
        let mut candidate = self
            .authority
            .lock()
            .expect("browser authority lock poisoned")
            .clone();
        let value = mutation(&mut candidate).map_err(|error| {
            BrowserToolError::new("browser_authority_rejected", error.to_string())
        })?;
        if persist && let Some(persistence) = self.persistence.as_ref() {
            let state_root = persistence.state_root().ok_or_else(|| {
                BrowserToolError::new(
                    "browser_state_persist_failed",
                    "浏览器状态持久化根目录不可用",
                )
            })?;
            persistence
                .save_json(
                    &state_root.join("browser/state.json"),
                    &candidate.durable_state(),
                )
                .map_err(|error| {
                    BrowserToolError::new(
                        "browser_state_persist_failed",
                        format!("浏览器状态持久化失败: {error:?}"),
                    )
                })?;
        }
        *self
            .authority
            .lock()
            .expect("browser authority lock poisoned") = candidate;
        Ok(value)
    }

    fn apply_page_state(
        &self,
        tab_id: &BrowserTabId,
        page: magi_browser_runtime::BrowserHostPageState,
    ) -> Result<magi_browser_runtime::BrowserTab, BrowserToolError> {
        let tab = self.mutate(|authority| {
            authority.transition_tab(
                tab_id,
                magi_browser_runtime::BrowserTabLifecycle::Ready,
                UtcMillis::now(),
            )?;
            authority.apply_host_page_state(
                tab_id,
                page.navigation_revision,
                page.url,
                page.origin,
                page.title,
                UtcMillis::now(),
            )
        })?;
        self.publish_tab_event("browser.tab.updated", &tab);
        Ok(tab)
    }

    fn mark_tab_crashed(&self, tab_id: &BrowserTabId) {
        let crashed = self.mutate(|authority| {
            authority.transition_tab(
                tab_id,
                magi_browser_runtime::BrowserTabLifecycle::Crashed,
                UtcMillis::now(),
            )
        });
        if let Ok(tab) = crashed {
            self.publish_tab_event("browser.tab.crashed", &tab);
        }
    }

    fn apply_snapshot_revision(
        &self,
        tab_id: &BrowserTabId,
        revision: u64,
    ) -> Result<(), BrowserToolError> {
        self.mutate(|authority| {
            authority
                .apply_host_snapshot_revision(tab_id, revision, UtcMillis::now())
                .map(|_| ())
        })
    }

    fn validate_snapshot(
        &self,
        session_id: &BrowserSessionId,
        tab_id: &BrowserTabId,
        revision: u64,
    ) -> Result<(), BrowserToolError> {
        self.authority
            .lock()
            .expect("browser authority lock poisoned")
            .validate_snapshot_ref(session_id, tab_id, revision)
            .map_err(|error| BrowserToolError::new("browser_snapshot_stale", error.to_string()))
    }

    fn persist_artifact(
        &self,
        session_id: &SessionId,
        _call_id: &str,
        bytes: &[u8],
    ) -> Result<String, BrowserToolError> {
        let Some(persistence) = self.persistence.as_ref() else {
            return Err(BrowserToolError::new(
                "browser_artifact_store_unavailable",
                "浏览器 artifact 存储不可用",
            ));
        };
        let root = persistence
            .state_root()
            .ok_or_else(|| {
                BrowserToolError::new(
                    "browser_artifact_store_unavailable",
                    "浏览器 artifact 根目录不可用",
                )
            })?
            .join("browser/artifacts")
            .join(session_id.as_str());
        std::fs::create_dir_all(&root).map_err(|error| {
            BrowserToolError::new("browser_artifact_write_failed", error.to_string())
        })?;
        let path = root.join(format!(
            "browser-shot-{}-{}.png",
            UtcMillis::now().0,
            BROWSER_ARTIFACT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        magi_core::fs_atomic::write_atomic(&path, bytes).map_err(|error| {
            BrowserToolError::new("browser_artifact_write_failed", error.to_string())
        })?;
        Ok(path.display().to_string())
    }

    async fn execute_tabs(
        &self,
        arguments: &Map<String, Value>,
        session: &magi_browser_runtime::BrowserSession,
        scope: BrowserToolCallScope<'_>,
        client: &BrowserHostClient,
    ) -> Result<String, BrowserToolError> {
        let action = string_arg(arguments, "action")?;
        if action == "list" {
            let authority = self
                .authority
                .lock()
                .expect("browser authority lock poisoned");
            let tabs = session
                .tab_ids
                .iter()
                .filter_map(|id| authority.tab(id))
                .cloned()
                .collect::<Vec<_>>();
            return Ok(json!({ "tool": "browser_tabs", "status": "succeeded", "tabs": tabs, "active_tab_id": session.active_tab_id }).to_string());
        }
        match action.as_str() {
            "new" => {
                let tab_id = BrowserTabId::new(format!(
                    "browser-tool-tab-{}-{}",
                    UtcMillis::now().0,
                    BROWSER_TAB_SEQUENCE.fetch_add(1, Ordering::Relaxed)
                ));
                let created = self.mutate(|authority| {
                    authority.create_tab(CreateBrowserTab {
                        tab_id: tab_id.clone(),
                        browser_session_id: session.browser_session_id.clone(),
                        url: "about:blank".to_string(),
                        viewport: BrowserViewport::default(),
                        now: UtcMillis::now(),
                    })
                })?;
                let reply = match client
                    .request(BrowserHostCommand::CreatePage {
                        tab_id: tab_id.clone(),
                        initial_url: "about:blank".to_string(),
                        viewport: magi_browser_runtime::HostViewport {
                            width: created.viewport.width,
                            height: created.viewport.height,
                            surface_width: created.viewport.width,
                            surface_height: created.viewport.height,
                            device_scale_factor_millis: created.viewport.device_scale_factor_millis,
                            device_type: created.viewport.device_type,
                        },
                        navigation_revision: 0,
                        snapshot_revision: 0,
                        allow_streaming_eviction: false,
                    })
                    .await
                {
                    Ok(reply) => reply,
                    Err(error) => {
                        self.mark_tab_crashed(&tab_id);
                        return Err(BrowserToolError::new(
                            "browser_host_disconnected",
                            error.to_string(),
                        ));
                    }
                };
                let page = match page_state(reply.response.outcome, "创建浏览器 Tab 失败") {
                    Ok(page) => page,
                    Err(error) => {
                        self.mark_tab_crashed(&tab_id);
                        return Err(error);
                    }
                };
                let created = self.apply_page_state(&tab_id, page)?;
                self.publish_tab_event("browser.tab.created", &created);
                Ok(
                    json!({ "tool": "browser_tabs", "status": "succeeded", "tab": created })
                        .to_string(),
                )
            }
            "activate" => {
                let tab_id = BrowserTabId::new(string_arg(arguments, "tab_id")?);
                let target = self
                    .materialize_tab(tab_in_session(self, session, &tab_id)?, client)
                    .await?;
                self.prepare_agent_write(client, session, &target, scope)
                    .await?;
                let updated = self.mutate(|authority| {
                    authority.set_active_tab(&session.browser_session_id, &tab_id, UtcMillis::now())
                })?;
                self.publish_browser_event(
                    "browser.tab.activated",
                    &updated,
                    json!({
                        "browser_session_id": updated.browser_session_id,
                        "tab_id": tab_id,
                        "revision": updated.revision,
                    }),
                );
                Ok(json!({ "tool": "browser_tabs", "status": "succeeded", "active_tab_id": updated.active_tab_id }).to_string())
            }
            "close" => {
                let tab_id = BrowserTabId::new(string_arg(arguments, "tab_id")?);
                let _target = tab_in_session(self, session, &tab_id)?;
                client
                    .request(BrowserHostCommand::ClosePage {
                        tab_id: tab_id.clone(),
                    })
                    .await
                    .map_err(|error| {
                        BrowserToolError::new("browser_host_disconnected", error.to_string())
                    })?;
                let closed = self.mutate(|authority| {
                    authority.transition_tab(
                        &tab_id,
                        magi_browser_runtime::BrowserTabLifecycle::Closed,
                        UtcMillis::now(),
                    )
                })?;
                self.publish_tab_event("browser.tab.closed", &closed);
                Ok(json!({ "tool": "browser_tabs", "status": "succeeded", "closed_tab_id": tab_id }).to_string())
            }
            _ => Err(BrowserToolError::new(
                "invalid_tabs_action",
                "browser_tabs action 不合法",
            )),
        }
    }
}

fn tab_in_session(
    runtime: &BrowserToolRuntimeDependencies,
    session: &magi_browser_runtime::BrowserSession,
    tab_id: &BrowserTabId,
) -> Result<magi_browser_runtime::BrowserTab, BrowserToolError> {
    let authority = runtime
        .authority
        .lock()
        .expect("browser authority lock poisoned");
    let tab = authority
        .tab(tab_id)
        .ok_or_else(|| BrowserToolError::new("browser_tab_not_found", "指定的浏览器 Tab 不存在"))?;
    if tab.browser_session_id != session.browser_session_id {
        return Err(BrowserToolError::new(
            "browser_tab_scope_mismatch",
            "指定的浏览器 Tab 不属于当前浏览器会话",
        ));
    }
    Ok(tab.clone())
}

fn browser_tool_requested_access(
    kind: BrowserToolKind,
    arguments: &Map<String, Value>,
) -> BrowserToolAccess {
    let action = arguments.get("action").and_then(Value::as_str);
    if kind == BrowserToolKind::Tabs {
        return if action == Some("list") {
            BrowserToolAccess::Read
        } else {
            BrowserToolAccess::Write
        };
    }
    if kind == BrowserToolKind::Viewport {
        return if action == Some("get") {
            BrowserToolAccess::Read
        } else {
            BrowserToolAccess::Write
        };
    }
    kind.catalog_access()
}

fn u32_arg(arguments: &Map<String, Value>, name: &str) -> Result<u32, BrowserToolError> {
    let value = arguments
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            BrowserToolError::new("invalid_arguments", format!("参数 {name} 必须是正整数"))
        })?;
    Ok(value)
}

fn validate_viewport_dimensions(width: u32, height: u32) -> Result<(), BrowserToolError> {
    if !(320..=7_680).contains(&width) || !(240..=4_320).contains(&height) {
        return Err(BrowserToolError::new(
            "browser_viewport_invalid",
            "浏览器页面视口尺寸超出支持范围",
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct BrowserToolError {
    code: String,
    message: String,
    recoverable: bool,
    requires_user_action: bool,
    details: Option<Value>,
    status: ExecutionResultStatus,
}

impl BrowserToolError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            recoverable: false,
            requires_user_action: false,
            details: None,
            status: ExecutionResultStatus::Failed,
        }
    }

    fn from_host(context: &str, error: BrowserHostCommandError) -> Self {
        let sensitive_action = error.code == "browser_sensitive_action_requires_user";
        Self {
            code: error.code.clone(),
            message: format!("{context}: {}", error.message),
            recoverable: error.recoverable || sensitive_action,
            requires_user_action: sensitive_action,
            details: Some(json!({
                "host_error": {
                    "code": error.code,
                    "recoverable": error.recoverable,
                    "side_effect_started": error.side_effect_started,
                    "diagnostic": error.diagnostic,
                },
            })),
            status: if sensitive_action {
                ExecutionResultStatus::NeedsApproval
            } else {
                ExecutionResultStatus::Failed
            },
        }
    }
}

fn failure(tool: &str, code: &str, message: &str) -> (String, ExecutionResultStatus) {
    failure_payload(tool, code, message, false, false, None)
}

fn failure_with_error(tool: &str, error: &BrowserToolError) -> (String, ExecutionResultStatus) {
    let (payload, _) = failure_payload_with_status(
        tool,
        &error.code,
        &error.message,
        error.recoverable,
        error.requires_user_action,
        error.details.as_ref(),
        error.status,
    );
    (payload, error.status)
}

fn failure_payload(
    tool: &str,
    code: &str,
    message: &str,
    recoverable: bool,
    requires_user_action: bool,
    details: Option<&Value>,
) -> (String, ExecutionResultStatus) {
    failure_payload_with_status(
        tool,
        code,
        message,
        recoverable,
        requires_user_action,
        details,
        ExecutionResultStatus::Failed,
    )
}

fn failure_payload_with_status(
    tool: &str,
    code: &str,
    message: &str,
    recoverable: bool,
    requires_user_action: bool,
    details: Option<&Value>,
    status: ExecutionResultStatus,
) -> (String, ExecutionResultStatus) {
    let status_label = match status {
        ExecutionResultStatus::NeedsApproval => "needs_approval",
        ExecutionResultStatus::Rejected => "rejected",
        ExecutionResultStatus::Cancelled => "cancelled",
        ExecutionResultStatus::Succeeded => "succeeded",
        ExecutionResultStatus::Failed => "failed",
    };
    let mut payload = json!({
        "tool": tool,
        "status": status_label,
        "error_code": code,
        "recoverable": recoverable,
        "requires_user_action": requires_user_action,
        "error": message,
    });
    if let Some(details) = details {
        payload["details"] = details.clone();
    }
    (payload.to_string(), status)
}

fn string_arg(arguments: &Map<String, Value>, name: &str) -> Result<String, BrowserToolError> {
    optional_string(arguments, name)
        .ok_or_else(|| BrowserToolError::new("invalid_arguments", format!("缺少参数 {name}")))
}

fn optional_string(arguments: &Map<String, Value>, name: &str) -> Option<String> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn number_arg(arguments: &Map<String, Value>, name: &str) -> Option<f64> {
    arguments.get(name).and_then(Value::as_f64)
}

fn snapshot_target(
    arguments: &Map<String, Value>,
) -> Result<BrowserSnapshotTarget, BrowserToolError> {
    Ok(BrowserSnapshotTarget {
        snapshot_revision: arguments
            .get("snapshot_revision")
            .and_then(Value::as_u64)
            .ok_or_else(|| BrowserToolError::new("invalid_arguments", "缺少 snapshot_revision"))?,
        element_ref: string_arg(arguments, "element_ref")?,
    })
}

fn optional_snapshot_target(
    arguments: &Map<String, Value>,
) -> Result<Option<BrowserSnapshotTarget>, BrowserToolError> {
    match optional_string(arguments, "element_ref").as_deref() {
        // browser_snapshot 返回的合成根节点 element_ref="root" 不对应可定位
        // DOM 元素。截图时将其视为整页作用域（None），避免 Host 报
        // "snapshot element ref does not exist: root"。
        None | Some("root") => Ok(None),
        Some(_) => snapshot_target(arguments).map(Some),
    }
}

fn succeeded_result(
    outcome: BrowserHostCommandOutcome,
    context: &str,
) -> Result<BrowserHostCommandResult, BrowserToolError> {
    match outcome {
        BrowserHostCommandOutcome::Succeeded(result) => Ok(*result),
        BrowserHostCommandOutcome::Failed(error)
        | BrowserHostCommandOutcome::Indeterminate(error) => {
            Err(BrowserToolError::from_host(context, error))
        }
        BrowserHostCommandOutcome::Cancelled => {
            Err(BrowserToolError::new("browser_command_cancelled", context))
        }
    }
}

fn page_state(
    outcome: BrowserHostCommandOutcome,
    context: &str,
) -> Result<magi_browser_runtime::BrowserHostPageState, BrowserToolError> {
    match succeeded_result(outcome, context)? {
        BrowserHostCommandResult::PageState(page) => Ok(page),
        _ => Err(BrowserToolError::new("browser_result_invalid", context)),
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Browser tool runtime should create a Tokio runtime")
            .block_on(future)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, RwLock, atomic::AtomicBool};

    use magi_browser_runtime::{BrowserAuthority, BrowserProfile, BrowserProfileKind};
    use magi_core::{BrowserProfileId, SessionId, UtcMillis, WorkspaceId};
    use magi_event_bus::InMemoryEventBus;
    use magi_session_store::SessionStore;

    use super::{BrowserToolRuntimeDependencies, DEFAULT_BROWSER_PROFILE_ID};
    use crate::state::BrowserRuntimeStatusSnapshot;

    #[test]
    fn ensuring_browser_session_does_not_implicitly_create_page() {
        let mut authority = BrowserAuthority::new();
        authority
            .register_profile(BrowserProfile {
                profile_id: BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID),
                kind: BrowserProfileKind::ManagedDefault,
                data_path: tempfile::tempdir()
                    .expect("browser profile fixture should create")
                    .keep(),
                created_at: UtcMillis(1),
                updated_at: UtcMillis(1),
            })
            .expect("browser profile should register");
        let event_bus = Arc::new(InMemoryEventBus::new(16));
        let runtime = BrowserToolRuntimeDependencies {
            authority: Arc::new(Mutex::new(authority)),
            write_lock: Arc::new(Mutex::new(())),
            control_lock: Arc::new(tokio::sync::Mutex::new(())),
            state_writable: Arc::new(AtomicBool::new(true)),
            runtime_status: Arc::new(RwLock::new(BrowserRuntimeStatusSnapshot::default())),
            host_client: Arc::new(RwLock::new(None)),
            event_bus: Arc::clone(&event_bus),
            session_store: Arc::new(SessionStore::new()),
            persistence: None,
        };
        let session_id = SessionId::new("session-browser-tabs-list");
        let workspace_id = WorkspaceId::new("workspace-browser-tabs-list");

        let created = runtime
            .ensure_session(&session_id, &workspace_id)
            .expect("browser session should create");
        assert!(created.tab_ids.is_empty());
        assert!(created.active_tab_id.is_none());
        let existing = runtime
            .ensure_session(&session_id, &workspace_id)
            .expect("browser session should be idempotent");
        assert_eq!(existing.browser_session_id, created.browser_session_id);
        assert!(existing.tab_ids.is_empty());
        assert_eq!(
            event_bus
                .snapshot()
                .recent_events
                .iter()
                .filter(|event| event.event_type == "browser.session.created")
                .count(),
            1
        );
    }
}
