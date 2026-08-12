use std::{
    collections::HashSet,
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
    BrowserHostCommandResult, BrowserHostControl, BrowserHostControlMode, BrowserHostSnapshot,
    BrowserLeaseEndReason, BrowserNavigation, BrowserSnapshotNode, BrowserSnapshotTarget,
    BrowserToolAccess, BrowserToolKind, BrowserViewport, BrowserViewportMode, CreateBrowserSession,
    CreateBrowserTab, GoalControlBinding, ValidateBrowserWrite, validate_browser_navigation_url,
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
    pub(crate) browser_views: Arc<crate::state::BrowserViewRegistry>,
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
            context,
            session_id,
            workspace_id,
            call_id,
        } = scope;
        let execution_id = self.browser_execution_id(context, session_id);
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
        let (tab, view_use) = self
            .ensure_tab(&browser_session, arguments, &execution_id, &client)
            .await?;
        let host_tab_id = view_use
            .as_ref()
            .map(|view| view.host_tab_id().clone())
            .unwrap_or_else(|| tab.tab_id.clone());
        if let Some(operation) = browser_devtools_operation(tool_name) {
            return self
                .execute_devtools_operation(
                    tool_name,
                    operation,
                    arguments,
                    &browser_session,
                    &tab,
                    &host_tab_id,
                    scope,
                    &client,
                )
                .await;
        }
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
                            tab_id: host_tab_id.clone(),
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
                let action = optional_string(arguments, "action")
                    .or_else(|| optional_string(arguments, "url").map(|_| "url".to_string()))
                    .ok_or_else(|| {
                        BrowserToolError::new(
                            "invalid_navigation",
                            "必须提供 url，或显式指定 back、forward、reload action",
                        )
                    })?;
                let timeout_ms = arguments
                    .get("timeout_ms")
                    .and_then(Value::as_u64)
                    .map(|value| {
                        u32::try_from(value).map_err(|_| {
                            BrowserToolError::new("invalid_navigation", "timeout_ms 超出支持范围")
                        })
                    })
                    .transpose()?
                    .map(|value| value.clamp(1, 60_000));
                let handle_before_unload = optional_string(arguments, "handle_before_unload");
                if handle_before_unload
                    .as_deref()
                    .is_some_and(|value| value != "accept" && value != "dismiss")
                {
                    return Err(BrowserToolError::new(
                        "invalid_navigation",
                        "handle_before_unload 必须是 accept 或 dismiss",
                    ));
                }
                let init_script = optional_string(arguments, "init_script");
                let navigation = match action.as_str() {
                    "url" => {
                        if bool_arg(arguments, "ignore_cache", false) {
                            return Err(BrowserToolError::new(
                                "invalid_navigation",
                                "ignore_cache 只支持 reload action",
                            ));
                        }
                        let url = string_arg(arguments, "url")?;
                        validate_browser_navigation_url(&url).map_err(|error| {
                            BrowserToolError::new(
                                "browser_navigation_url_rejected",
                                format!("浏览器导航 URL 不合法: {error}"),
                            )
                        })?;
                        BrowserNavigation::Url {
                            url,
                            handle_before_unload,
                            init_script,
                            timeout_ms,
                        }
                    }
                    "back" => BrowserNavigation::Back { timeout_ms },
                    "forward" => BrowserNavigation::Forward { timeout_ms },
                    "reload" => BrowserNavigation::Reload {
                        ignore_cache: bool_arg(arguments, "ignore_cache", false),
                        handle_before_unload,
                        timeout_ms,
                    },
                    _ => return Err(BrowserToolError::new("invalid_navigation", "action 不合法")),
                };
                let control = self
                    .prepare_agent_write(&client, &browser_session, &tab, scope)
                    .await?;
                let reply = client
                    .request(BrowserHostCommand::Navigate {
                        tab_id: host_tab_id.clone(),
                        control,
                        navigation,
                    })
                    .await
                    .map_err(|error| {
                        BrowserToolError::new("browser_host_disconnected", error.to_string())
                    })?;
                let page = page_state(reply.response.outcome, "浏览器导航失败")?;
                let updated = self.apply_page_state(&tab.tab_id, page)?;
                let snapshot = if bool_arg(arguments, "include_snapshot", false) {
                    let snapshot = self
                        .capture_snapshot(&client, &tab.tab_id, &host_tab_id, None)
                        .await?;
                    Some(browser_tool_snapshot_value(&snapshot, &tab.tab_id))
                } else {
                    None
                };
                Ok(json!({
                    "tool": tool_name,
                    "status": "succeeded",
                    "tab": updated,
                    "snapshot": snapshot,
                })
                .to_string())
            }
            "browser_snapshot" => {
                let snapshot = self
                    .capture_snapshot(
                        &client,
                        &tab.tab_id,
                        &host_tab_id,
                        optional_string(arguments, "subtree_ref"),
                    )
                    .await?;
                let snapshot = browser_tool_snapshot_value(&snapshot, &tab.tab_id);
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
                        tab_id: host_tab_id.clone(),
                        control,
                        target: snapshot_target(arguments)?,
                    },
                    "browser_type" => BrowserHostCommand::Type {
                        tab_id: host_tab_id.clone(),
                        control,
                        target: snapshot_target(arguments)?,
                        text: string_arg(arguments, "text")?,
                        replace: arguments
                            .get("replace")
                            .and_then(Value::as_bool)
                            .unwrap_or(true),
                        submit_key: optional_string(arguments, "submit_key"),
                    },
                    "browser_press" => BrowserHostCommand::Press {
                        tab_id: host_tab_id.clone(),
                        control,
                        key: string_arg(arguments, "key")?,
                    },
                    _ => BrowserHostCommand::Scroll {
                        tab_id: host_tab_id.clone(),
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
                let snapshot = if bool_arg(arguments, "include_snapshot", false) {
                    let snapshot = self
                        .capture_snapshot(&client, &tab.tab_id, &host_tab_id, None)
                        .await?;
                    Some(browser_tool_snapshot_value(&snapshot, &tab.tab_id))
                } else {
                    None
                };
                Ok(json!({
                    "tool": tool_name,
                    "status": "succeeded",
                    "tab": updated,
                    "snapshot": snapshot,
                })
                .to_string())
            }
            "browser_screenshot" => {
                let target = optional_snapshot_target(arguments)?;
                let clip = arguments
                    .get("clip")
                    .map(parse_normalized_rect)
                    .transpose()?;
                let format = match optional_string(arguments, "format").as_deref() {
                    None | Some("png") => magi_browser_runtime::BrowserScreenshotFormat::Png,
                    Some("jpeg") => magi_browser_runtime::BrowserScreenshotFormat::Jpeg,
                    Some("webp") => magi_browser_runtime::BrowserScreenshotFormat::Webp,
                    Some(_) => {
                        return Err(BrowserToolError::new(
                            "invalid_screenshot_format",
                            "format 必须是 png、jpeg 或 webp",
                        ));
                    }
                };
                let quality = arguments
                    .get("quality")
                    .and_then(Value::as_u64)
                    .map(|value| {
                        u8::try_from(value).map_err(|_| {
                            BrowserToolError::new(
                                "invalid_screenshot_quality",
                                "quality 必须在 0-100 之间",
                            )
                        })
                    })
                    .transpose()?
                    .map(|value| value.min(100));
                if format == magi_browser_runtime::BrowserScreenshotFormat::Png && quality.is_some()
                {
                    return Err(BrowserToolError::new(
                        "invalid_screenshot_quality",
                        "PNG 不支持 quality，只有 jpeg 或 webp 支持质量参数",
                    ));
                }
                let reply = client
                    .request(BrowserHostCommand::Screenshot {
                        tab_id: host_tab_id.clone(),
                        target,
                        clip,
                        full_page: arguments
                            .get("full_page")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        format,
                        quality,
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
                let extension = match format {
                    magi_browser_runtime::BrowserScreenshotFormat::Png => "png",
                    magi_browser_runtime::BrowserScreenshotFormat::Jpeg => "jpg",
                    magi_browser_runtime::BrowserScreenshotFormat::Webp => "webp",
                };
                let path = self.persist_artifact(session_id, call_id, &bytes, extension)?;
                Ok(json!({ "tool": tool_name, "status": "succeeded", "path": path, "mime": metadata.mime_type, "bytes": bytes.len(), "sha256": metadata.sha256 }).to_string())
            }
            _ => Err(BrowserToolError::new(
                "unknown_browser_tool",
                "未知的浏览器工具",
            )),
        }
    }

    async fn execute_devtools_operation(
        &self,
        tool_name: &str,
        operation: &str,
        arguments: &Map<String, Value>,
        browser_session: &magi_browser_runtime::BrowserSession,
        tab: &magi_browser_runtime::BrowserTab,
        host_tab_id: &BrowserTabId,
        scope: BrowserToolCallScope<'_>,
        client: &BrowserHostClient,
    ) -> Result<String, BrowserToolError> {
        let action = optional_string(arguments, "action");
        let lighthouse_mode = optional_string(arguments, "mode");
        let requires_write = match operation {
            "hover" | "drag" | "fill_form" | "upload_file" | "click_at" | "evaluate"
            | "emulate" => true,
            "dialog" => !matches!(action.as_deref(), Some("list")),
            "console" | "network" => matches!(action.as_deref(), Some("clear")),
            "performance" => matches!(action.as_deref(), Some("start" | "stop")),
            "lighthouse" => !matches!(lighthouse_mode.as_deref(), Some("snapshot")),
            "recording" => true,
            "heap" => matches!(action.as_deref(), Some("take_snapshot")),
            "extensions" => matches!(
                action.as_deref(),
                Some("install" | "reload" | "trigger_action" | "uninstall")
            ),
            "third_party" | "webmcp" => matches!(action.as_deref(), Some("execute")),
            "pwa" => matches!(action.as_deref(), Some("install" | "launch" | "uninstall")),
            _ => false,
        };
        let control = if requires_write {
            Some(
                self.prepare_agent_write(client, browser_session, tab, scope)
                    .await?,
            )
        } else {
            None
        };
        let reply = client
            .request(BrowserHostCommand::Devtools {
                tab_id: host_tab_id.clone(),
                control,
                operation: operation.to_string(),
                arguments: Value::Object(arguments.clone()),
            })
            .await
            .map_err(|error| {
                BrowserToolError::new("browser_host_disconnected", error.to_string())
            })?;
        let BrowserHostCommandResult::Json(value) =
            succeeded_result(reply.response.outcome, "浏览器 DevTools 工具执行失败")?
        else {
            return Err(BrowserToolError::new(
                "browser_result_invalid",
                "浏览器 DevTools 工具返回结果无效",
            ));
        };
        let snapshot = if bool_arg(arguments, "include_snapshot", false) {
            Some(browser_tool_snapshot_value(
                &self
                    .capture_snapshot(client, &tab.tab_id, host_tab_id, None)
                    .await?,
                &tab.tab_id,
            ))
        } else {
            None
        };
        Ok(json!({
            "tool": tool_name,
            "status": "succeeded",
            "result": value,
            "snapshot": snapshot,
        })
        .to_string())
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
        execution_id: &str,
        client: &BrowserHostClient,
    ) -> Result<
        (
            magi_browser_runtime::BrowserTab,
            Option<crate::state::BrowserViewUse>,
        ),
        BrowserToolError,
    > {
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
            match self
                .browser_views
                .acquire_worker_target(&tab.tab_id, execution_id)
            {
                Ok(Some(view_use)) => return Ok((tab, Some(view_use))),
                Ok(None) => {}
                Err(_) => {
                    return Err(BrowserToolError::recoverable(
                        "browser_page_reconnected",
                        "浏览器页面已重新连接，请重新获取快照后继续",
                    ));
                }
            }
            return Ok((self.materialize_tab(tab, client).await?, None));
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
        self.browser_views
            .pin_worker_to_logical(&tab.tab_id, execution_id);
        self.publish_tab_event("browser.tab.created", &tab);
        Ok((tab, None))
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
        let (lease, identity, lease_acquired) = self.acquire_or_reuse_lease(
            session,
            scope.context,
            scope.session_id,
            scope.workspace_id,
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
        if lease_acquired {
            self.publish_browser_event(
                "browser.lease.acquired",
                session,
                json!({
                    "lease_id": lease.lease_id,
                    "browser_session_id": session.browser_session_id,
                    "profile_id": session.profile_id,
                    "turn_id": identity.turn_id,
                    "expires_at": lease.expires_at,
                }),
            );
        }
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
    ) -> Result<
        (
            magi_browser_runtime::BrowserControlLease,
            BrowserExecutionIdentity,
            bool,
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
        let turn_id = self.browser_execution_id(context, session_id);
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
                return Ok((lease, identity, false));
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
        Ok((lease, identity, true))
    }

    fn browser_execution_id(
        &self,
        context: &magi_tool_runtime::ToolExecutionContext,
        session_id: &SessionId,
    ) -> String {
        context
            .browser_execution_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                context
                    .task_id
                    .as_ref()
                    .map(|task_id| format!("task:{task_id}"))
            })
            .or_else(|| {
                self.session_store
                    .runtime_sidecar(session_id)
                    .and_then(|sidecar| sidecar.current_turn.map(|turn| turn.turn_id))
            })
            .unwrap_or_else(|| format!("session:{session_id}"))
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
        let current = self
            .authority
            .lock()
            .expect("browser authority lock poisoned")
            .tab(tab_id)
            .cloned();
        if let Some(current) = current
            && current.lifecycle == magi_browser_runtime::BrowserTabLifecycle::Ready
            && current.navigation_revision == page.navigation_revision
            && current.url == page.url
            && current.origin == page.origin
            && current.title == page.title
        {
            return Ok(current);
        }
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

    async fn capture_snapshot(
        &self,
        client: &BrowserHostClient,
        logical_tab_id: &BrowserTabId,
        host_tab_id: &BrowserTabId,
        subtree_ref: Option<String>,
    ) -> Result<magi_browser_runtime::BrowserHostSnapshot, BrowserToolError> {
        let reply = client
            .request(BrowserHostCommand::Snapshot {
                tab_id: host_tab_id.clone(),
                limits: Default::default(),
                subtree_ref,
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
        self.apply_snapshot_revision(logical_tab_id, snapshot.snapshot_revision)?;
        Ok(snapshot)
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

    fn persist_artifact(
        &self,
        session_id: &SessionId,
        _call_id: &str,
        bytes: &[u8],
        extension: &str,
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
            "browser-shot-{}-{}.{}",
            UtcMillis::now().0,
            BROWSER_ARTIFACT_SEQUENCE.fetch_add(1, Ordering::Relaxed),
            extension,
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
                let initial_url =
                    optional_string(arguments, "url").unwrap_or_else(|| "about:blank".to_string());
                validate_browser_navigation_url(&initial_url).map_err(|error| {
                    BrowserToolError::new(
                        "browser_navigation_url_rejected",
                        format!("浏览器导航 URL 不合法: {error}"),
                    )
                })?;
                let tab_id = BrowserTabId::new(format!(
                    "browser-tool-tab-{}-{}",
                    UtcMillis::now().0,
                    BROWSER_TAB_SEQUENCE.fetch_add(1, Ordering::Relaxed)
                ));
                let created = self.mutate(|authority| {
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
                let view_bindings = self.browser_views.remove_for_tab(&tab_id);
                let mut host_tab_ids = Vec::with_capacity(view_bindings.len() + 1);
                for binding in view_bindings {
                    binding.wait_until_idle().await;
                    host_tab_ids.push(binding.host_tab_id);
                }
                host_tab_ids.push(tab_id.clone());
                let mut close_error = None;
                for host_tab_id in host_tab_ids {
                    if let Err(error) = client
                        .request(BrowserHostCommand::ClosePage {
                            tab_id: host_tab_id,
                        })
                        .await
                    {
                        close_error.get_or_insert_with(|| {
                            BrowserToolError::new("browser_host_disconnected", error.to_string())
                        });
                    }
                }
                let closed = self.mutate(|authority| {
                    authority.transition_tab(
                        &tab_id,
                        magi_browser_runtime::BrowserTabLifecycle::Closed,
                        UtcMillis::now(),
                    )
                })?;
                self.publish_tab_event("browser.tab.closed", &closed);
                if let Some(error) = close_error {
                    return Err(error);
                }
                Ok(json!({ "tool": "browser_tabs", "status": "succeeded", "closed_tab_id": tab_id }).to_string())
            }
            _ => Err(BrowserToolError::new(
                "invalid_tabs_action",
                "browser_tabs action 不合法",
            )),
        }
    }
}

fn browser_devtools_operation(tool_name: &str) -> Option<&'static str> {
    match tool_name {
        "browser_wait_for" => Some("wait_for"),
        "browser_hover" => Some("hover"),
        "browser_drag" => Some("drag"),
        "browser_fill_form" => Some("fill_form"),
        "browser_dialog" => Some("dialog"),
        "browser_upload_file" => Some("upload_file"),
        "browser_click_at" => Some("click_at"),
        "browser_evaluate" => Some("evaluate"),
        "browser_console" => Some("console"),
        "browser_network" => Some("network"),
        "browser_emulate" => Some("emulate"),
        "browser_performance" => Some("performance"),
        "browser_lighthouse" => Some("lighthouse"),
        "browser_screencast" => Some("recording"),
        "browser_heap" => Some("heap"),
        "browser_extensions" => Some("extensions"),
        "browser_third_party" => Some("third_party"),
        "browser_webmcp" => Some("webmcp"),
        "browser_pwa" => Some("pwa"),
        _ => None,
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
    let lighthouse_mode = arguments.get("mode").and_then(Value::as_str);
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
    if matches!(
        kind,
        BrowserToolKind::Dialog
            | BrowserToolKind::Console
            | BrowserToolKind::Network
            | BrowserToolKind::Performance
            | BrowserToolKind::Lighthouse
            | BrowserToolKind::Heap
            | BrowserToolKind::Extensions
            | BrowserToolKind::ThirdParty
            | BrowserToolKind::WebMcp
            | BrowserToolKind::Pwa
    ) {
        return match (kind, action) {
            (BrowserToolKind::Dialog, Some("list"))
            | (BrowserToolKind::Console, Some("list" | "get"))
            | (BrowserToolKind::Network, Some("list" | "get"))
            | (BrowserToolKind::Performance, Some("metrics" | "analyze"))
            | (
                BrowserToolKind::Heap,
                Some(
                    "usage" | "close_snapshot" | "compare_snapshots" | "summary" | "details"
                    | "class_nodes" | "dominators" | "duplicate_strings" | "edges"
                    | "object_details" | "retainers" | "retaining_paths",
                ),
            )
            | (BrowserToolKind::Extensions, Some("list"))
            | (BrowserToolKind::ThirdParty, Some("list"))
            | (BrowserToolKind::WebMcp, Some("list"))
            | (BrowserToolKind::Pwa, Some("state")) => BrowserToolAccess::Read,
            (BrowserToolKind::Lighthouse, _) if lighthouse_mode == Some("snapshot") => {
                BrowserToolAccess::Read
            }
            _ => BrowserToolAccess::Write,
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

    fn recoverable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            recoverable: true,
            ..Self::new(code, message)
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

fn bool_arg(arguments: &Map<String, Value>, name: &str, default: bool) -> bool {
    arguments
        .get(name)
        .and_then(Value::as_bool)
        .unwrap_or(default)
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

fn parse_normalized_rect(
    value: &Value,
) -> Result<magi_browser_runtime::BrowserNormalizedRect, BrowserToolError> {
    let object = value.as_object().ok_or_else(|| {
        BrowserToolError::new(
            "invalid_screenshot_clip",
            "clip 必须是包含 x、y、width、height 的对象",
        )
    })?;
    let number = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite())
            .ok_or_else(|| {
                BrowserToolError::new(
                    "invalid_screenshot_clip",
                    format!("clip.{name} 必须是有限数字"),
                )
            })
    };
    let rect = magi_browser_runtime::BrowserNormalizedRect {
        x: number("x")?,
        y: number("y")?,
        width: number("width")?,
        height: number("height")?,
    };
    if rect.x < 0.0
        || rect.y < 0.0
        || rect.width <= 0.0
        || rect.height <= 0.0
        || rect.x + rect.width > 1.0
        || rect.y + rect.height > 1.0
    {
        return Err(BrowserToolError::new(
            "invalid_screenshot_clip",
            "clip 必须位于 0-1 的视口范围内，且 width、height 大于 0",
        ));
    }
    Ok(rect)
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

fn browser_tool_snapshot_value(
    snapshot: &BrowserHostSnapshot,
    logical_tab_id: &BrowserTabId,
) -> Value {
    const MODEL_SNAPSHOT_ELEMENT_LIMIT: usize = 96;
    const MODEL_SNAPSHOT_TEXT_LIMIT_BYTES: usize = 10 * 1024;
    let mut nodes = Vec::new();
    collect_snapshot_nodes(&snapshot.root, &mut nodes);

    let mut value = Map::new();
    // Host 快照属于某个物理 Page，右侧面板存在时该标识为 browser-view-*。
    // 模型只能持有 Authority 中的逻辑 Tab ID；物理 View ID 不得越过工具边界，
    // 否则下一次点击会把内部 ID 当成 tab_id 并稳定触发 browser_tab_not_found。
    value.insert("tab_id".to_string(), json!(logical_tab_id));
    value.insert(
        "snapshot_revision".to_string(),
        json!(snapshot.snapshot_revision),
    );
    let mut remaining_text_bytes = MODEL_SNAPSHOT_TEXT_LIMIT_BYTES;
    insert_model_snapshot_string(
        &mut value,
        "title",
        snapshot.root.name.as_deref(),
        &mut remaining_text_bytes,
    );

    // 保持页面语义的分层预算：焦点控件、输入控件、操作控件、链接、
    // 标题和正文都能进入模型上下文，避免 GitHub 这类链接密集页面把
    // 页面主信息挤出快照。选中的节点按优先级输出，引用仍来自同一快照。
    const PRIORITY_BUDGETS: [usize; 7] = [4, 16, 20, 28, 12, 12, 4];
    let mut selected = Vec::with_capacity(MODEL_SNAPSHOT_ELEMENT_LIMIT);
    let mut selected_refs = HashSet::new();
    for priority in 0..PRIORITY_BUDGETS.len() {
        for node in nodes
            .iter()
            .filter(|node| snapshot_node_priority(node) == priority as u8)
            .take(PRIORITY_BUDGETS[priority])
        {
            if selected.len() >= MODEL_SNAPSHOT_ELEMENT_LIMIT {
                break;
            }
            selected_refs.insert(node.element_ref.as_str());
            selected.push(*node);
        }
    }
    for node in &nodes {
        if selected.len() >= MODEL_SNAPSHOT_ELEMENT_LIMIT {
            break;
        }
        if selected_refs.insert(node.element_ref.as_str()) {
            selected.push(*node);
        }
    }

    let elements = selected
        .into_iter()
        .map(|node| {
            let mut element = Map::new();
            element.insert(
                "element_ref".to_string(),
                Value::String(node.element_ref.clone()),
            );
            insert_model_snapshot_string(
                &mut element,
                "role",
                node.role.as_deref(),
                &mut remaining_text_bytes,
            );
            insert_model_snapshot_string(
                &mut element,
                "name",
                node.name.as_deref(),
                &mut remaining_text_bytes,
            );
            insert_model_snapshot_string(
                &mut element,
                "value",
                node.value.as_deref(),
                &mut remaining_text_bytes,
            );
            insert_model_snapshot_string(
                &mut element,
                "description",
                node.description.as_deref(),
                &mut remaining_text_bytes,
            );
            if node.disabled {
                element.insert("disabled".to_string(), Value::Bool(true));
            }
            if node.focused {
                element.insert("focused".to_string(), Value::Bool(true));
            }
            if node.editable {
                element.insert("editable".to_string(), Value::Bool(true));
            }
            if let Some(kind) = node.sensitive_input_kind {
                element.insert("sensitive_input_kind".to_string(), json!(kind));
            }
            Value::Object(element)
        })
        .collect::<Vec<_>>();

    value.insert("elements".to_string(), Value::Array(elements));
    Value::Object(value)
}

fn collect_snapshot_nodes<'a>(
    node: &'a BrowserSnapshotNode,
    output: &mut Vec<&'a BrowserSnapshotNode>,
) {
    for child in &node.children {
        output.push(child);
        collect_snapshot_nodes(child, output);
    }
}

fn snapshot_node_priority(node: &BrowserSnapshotNode) -> u8 {
    if node.focused {
        return 0;
    }
    if node.editable {
        return 1;
    }
    match node.role.as_deref() {
        Some("searchbox" | "textbox" | "combobox") => 2,
        Some("button" | "checkbox" | "radio" | "switch") => 3,
        Some("link") => 4,
        Some("heading") => 5,
        Some("paragraph") => 6,
        Some(_) => 6,
        None => 6,
    }
}

fn insert_model_snapshot_string(
    map: &mut Map<String, Value>,
    key: &str,
    value: Option<&str>,
    remaining_bytes: &mut usize,
) {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return;
    };
    const FIELD_LIMIT_BYTES: usize = 160;
    let limit = FIELD_LIMIT_BYTES.min(*remaining_bytes);
    if limit == 0 {
        return;
    }
    let clipped = truncate_utf8(value, limit);
    if clipped.is_empty() {
        return;
    }
    *remaining_bytes = remaining_bytes.saturating_sub(clipped.len());
    map.insert(key.to_string(), Value::String(clipped));
}

fn truncate_utf8(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let suffix = "…";
    if limit <= suffix.len() {
        return value.get(..limit).unwrap_or_default().to_string();
    }
    let mut end = limit - suffix.len();
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &value[..end], suffix)
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

    use magi_browser_runtime::{
        BrowserAuthority, BrowserHostPageState, BrowserHostRect, BrowserHostSnapshot,
        BrowserProfile, BrowserProfileKind, BrowserSessionLifecycle, BrowserSnapshotNode,
        BrowserViewport, CreateBrowserSession, CreateBrowserTab,
    };
    use magi_core::{
        BrowserProfileId, BrowserSessionId, BrowserTabId, SessionId, UtcMillis, WorkspaceId,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_session_store::SessionStore;

    use super::{
        BrowserToolRuntimeDependencies, DEFAULT_BROWSER_PROFILE_ID, browser_tool_snapshot_value,
    };
    use crate::state::{BrowserRuntimeStatusSnapshot, BrowserViewRegistry};

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
            browser_views: Arc::new(BrowserViewRegistry::default()),
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

    #[test]
    fn unchanged_host_page_state_does_not_rewrite_authority_or_publish_duplicate_event() {
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
        let session_id = SessionId::new("session-browser-page-state");
        let workspace_id = WorkspaceId::new("workspace-browser-page-state");
        let browser_session_id = BrowserSessionId::new("browser-session-page-state");
        let tab_id = BrowserTabId::new("browser-tab-page-state");
        authority
            .create_session(CreateBrowserSession {
                browser_session_id: browser_session_id.clone(),
                workspace_id,
                session_id,
                profile_id: BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID),
                now: UtcMillis(2),
            })
            .expect("browser session should create");
        authority
            .transition_session(
                &browser_session_id,
                BrowserSessionLifecycle::Ready,
                UtcMillis(3),
            )
            .expect("browser session should become ready");
        authority
            .create_tab(CreateBrowserTab {
                tab_id: tab_id.clone(),
                browser_session_id,
                url: "about:blank".to_string(),
                viewport: BrowserViewport::default(),
                now: UtcMillis(4),
            })
            .expect("browser tab should create");
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
            browser_views: Arc::new(BrowserViewRegistry::default()),
        };
        let page = BrowserHostPageState {
            tab_id: tab_id.clone(),
            url: "https://example.test/".to_string(),
            origin: Some("https://example.test".to_string()),
            title: "Example".to_string(),
            navigation_revision: 1,
        };

        runtime
            .apply_page_state(&tab_id, page.clone())
            .expect("first page state should apply");
        let authority_revision = runtime
            .authority
            .lock()
            .expect("browser authority lock poisoned")
            .snapshot()
            .revision;
        runtime
            .apply_page_state(&tab_id, page)
            .expect("identical page state should be idempotent");

        assert_eq!(
            runtime
                .authority
                .lock()
                .expect("browser authority lock poisoned")
                .snapshot()
                .revision,
            authority_revision
        );
        assert_eq!(
            event_bus
                .snapshot()
                .recent_events
                .iter()
                .filter(|event| event.event_type == "browser.tab.updated")
                .count(),
            1
        );
    }

    #[test]
    fn model_snapshot_is_compact_and_prioritizes_interactive_elements() {
        let mut nodes = (1..=200)
            .map(|index| {
                snapshot_node(
                    &format!("e-2-{index}"),
                    None,
                    Some("静态内容"),
                    false,
                    false,
                )
            })
            .collect::<Vec<_>>();
        let searchbox = snapshot_node("e-2-2", Some("searchbox"), Some("Search"), true, true);
        nodes.push(searchbox);
        let snapshot = BrowserHostSnapshot {
            tab_id: BrowserTabId::new("browser-tab-compact-snapshot"),
            snapshot_revision: 2,
            root: BrowserSnapshotNode {
                element_ref: "root".to_string(),
                role: Some("document".to_string()),
                name: Some("示例页面".to_string()),
                value: None,
                description: None,
                disabled: false,
                focused: false,
                editable: false,
                sensitive_input_kind: None,
                visible: true,
                bounds: None,
                children: nodes,
            },
            returned_nodes: 201,
            total_nodes: 201,
            text_bytes: 20,
            truncated: true,
            continuation_refs: vec!["e-2-1".to_string()],
        };

        let logical_tab_id = BrowserTabId::new("browser-logical-tab");
        let value = browser_tool_snapshot_value(&snapshot, &logical_tab_id);
        let elements = value["elements"]
            .as_array()
            .expect("compact snapshot elements should be an array");

        assert_eq!(elements.len(), 96);
        assert_eq!(value["tab_id"], "browser-logical-tab");
        assert_eq!(elements[0]["role"], "searchbox");
        assert_eq!(elements[0]["editable"], true);
        assert!(elements[0].get("bounds").is_none());
        assert!(elements[0].get("disabled").is_none());
        assert!(value.get("continuation_refs").is_none());
        assert!(value.get("truncated").is_none());
        assert!(value.get("returned_nodes").is_none());
        assert!(value.get("total_nodes").is_none());
    }

    #[test]
    fn browser_execution_id_is_stable_and_does_not_depend_on_tool_call_id() {
        let event_bus = Arc::new(InMemoryEventBus::new(8));
        let runtime = BrowserToolRuntimeDependencies {
            authority: Arc::new(Mutex::new(BrowserAuthority::new())),
            write_lock: Arc::new(Mutex::new(())),
            control_lock: Arc::new(tokio::sync::Mutex::new(())),
            state_writable: Arc::new(AtomicBool::new(true)),
            runtime_status: Arc::new(RwLock::new(BrowserRuntimeStatusSnapshot::default())),
            host_client: Arc::new(RwLock::new(None)),
            event_bus,
            session_store: Arc::new(SessionStore::new()),
            persistence: None,
            browser_views: Arc::new(BrowserViewRegistry::default()),
        };
        let session_id = SessionId::new("session-browser-execution-id");

        let explicit = magi_tool_runtime::ToolExecutionContext {
            browser_execution_id: Some("turn-stable".to_string()),
            ..Default::default()
        };
        assert_eq!(
            runtime.browser_execution_id(&explicit, &session_id),
            "turn-stable"
        );

        let task_scoped = magi_tool_runtime::ToolExecutionContext {
            task_id: Some(magi_core::TaskId::new("task-stable")),
            ..Default::default()
        };
        assert_eq!(
            runtime.browser_execution_id(&task_scoped, &session_id),
            "task:task-stable"
        );

        let session_scoped = magi_tool_runtime::ToolExecutionContext::default();
        assert_eq!(
            runtime.browser_execution_id(&session_scoped, &session_id),
            "session:session-browser-execution-id"
        );
    }

    fn snapshot_node(
        element_ref: &str,
        role: Option<&str>,
        name: Option<&str>,
        editable: bool,
        focused: bool,
    ) -> BrowserSnapshotNode {
        BrowserSnapshotNode {
            element_ref: element_ref.to_string(),
            role: role.map(str::to_string),
            name: name.map(str::to_string),
            value: None,
            description: None,
            disabled: false,
            focused,
            editable,
            sensitive_input_kind: None,
            visible: true,
            bounds: Some(BrowserHostRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 20.0,
            }),
            children: Vec::new(),
        }
    }
}
