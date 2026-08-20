use std::{
    collections::HashSet,
    future::Future,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use magi_browser_authority::{
    AcquireBrowserLease, BrowserCapabilitySnapshot, BrowserDeviceType, BrowserHostClient,
    BrowserHostClientError, BrowserHostCommand, BrowserHostCommandError, BrowserHostCommandOutcome,
    BrowserHostCommandResult, BrowserHostControl, BrowserHostControlUpdate, BrowserHostSnapshot,
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
use sha2::{Digest, Sha256};

use crate::{RuntimeStatePersistence, state::BrowserHostStatusSnapshot};

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
    workspace_id: Option<&'a WorkspaceId>,
    call_id: &'a str,
}

#[derive(Clone)]
pub struct BrowserToolRuntimeDependencies {
    pub authority: Arc<Mutex<magi_browser_authority::BrowserAuthority>>,
    pub write_lock: Arc<Mutex<()>>,
    pub control_lock: Arc<tokio::sync::Mutex<()>>,
    pub state_writable: Arc<std::sync::atomic::AtomicBool>,
    pub host_status: Arc<RwLock<BrowserHostStatusSnapshot>>,
    pub host_client: Arc<RwLock<Option<BrowserHostClient>>>,
    pub event_bus: Arc<InMemoryEventBus>,
    pub session_store: Arc<SessionStore>,
    pub persistence: Option<Arc<RuntimeStatePersistence>>,
}

impl BrowserToolRuntimeDependencies {
    pub fn capabilities(&self, session_id: Option<&SessionId>) -> BrowserCapabilitySnapshot {
        let host = self
            .host_status
            .read()
            .expect("browser host status lock poisoned")
            .clone();
        BrowserCapabilitySnapshot {
            revision: host.revision,
            in_app_browser_enabled: host.in_app_browser_enabled,
            browser_use_enabled: host.browser_use_enabled,
            host_status: host.status,
            host_protocol_compatible: host.protocol_compatible,
            access_profile: session_id
                .and_then(|id| self.session_store.active_goal(id))
                .map(|goal| goal.access_profile)
                .unwrap_or(magi_core::AccessProfile::Restricted),
        }
    }

    fn publish_browser_event(
        &self,
        event_type: &str,
        session: &magi_browser_authority::BrowserSession,
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
                workspace_id: session.workspace_id.clone(),
                session_id: Some(session.session_id.clone()),
                ..EventContext::default()
            }),
        );
    }

    fn publish_tab_event(&self, event_type: &str, tab: &magi_browser_authority::BrowserTab) {
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
        let workspace_id = context.workspace_id.clone();
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
            return capability_unavailable(tool_name, &error.to_string());
        }
        let client = self
            .host_client
            .read()
            .expect("browser Host client lock poisoned")
            .clone();
        let Some(client) = client else {
            return capability_unavailable(tool_name, "浏览器 Host 尚未就绪");
        };
        let call_id = tool_call_id.to_string();
        let scope = BrowserToolCallScope {
            context,
            session_id: &session_id,
            workspace_id: workspace_id.as_ref(),
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
            context: _,
            session_id,
            workspace_id,
            call_id,
        } = scope;
        let browser_session = self.ensure_session(session_id, workspace_id)?;
        if tool_name == "browser_tabs" {
            return self
                .execute_tabs(arguments, &browser_session, scope, &client)
                .await;
        }
        let tab = self
            .ensure_tab(&browser_session, arguments, &client)
            .await?;
        if browser_devtools_operation(tool_name).is_some() {
            return self
                .execute_devtools_operation(
                    tool_name,
                    arguments,
                    &browser_session,
                    &tab,
                    scope,
                    &client,
                )
                .await;
        }
        match tool_name {
            "browser_viewport" => {
                let action = string_arg(arguments, "action")?;
                if action == "get" {
                    let _control_guard = self.control_lock.lock().await;
                    let reply = client
                        .request(BrowserHostCommand::GetLogicalViewport {
                            tab_id: tab.tab_id.clone(),
                        })
                        .await
                        .map_err(browser_host_client_error)?;
                    let BrowserHostCommandResult::Json { value } =
                        succeeded_result(reply.response.outcome, "读取浏览器页面视口失败")?
                    else {
                        return Err(BrowserToolError::new(
                            "browser_result_invalid",
                            "浏览器页面视口返回结果无效",
                        ));
                    };
                    let logical_viewport = value
                        .get("viewport")
                        .cloned()
                        .ok_or_else(|| {
                            BrowserToolError::new(
                                "browser_result_invalid",
                                "浏览器页面视口缺少运行态视口信息",
                            )
                        })
                        .and_then(|viewport| {
                            serde_json::from_value::<
                                magi_browser_authority::BrowserLogicalViewport,
                            >(viewport)
                            .map_err(|error| {
                                BrowserToolError::new(
                                    "browser_result_invalid",
                                    format!("浏览器页面视口信息无效: {error}"),
                                )
                            })
                        })?;
                    let (mode, viewport) = match logical_viewport {
                        magi_browser_authority::BrowserLogicalViewport::Auto => {
                            (BrowserViewportMode::Auto, Value::Null)
                        }
                        magi_browser_authority::BrowserLogicalViewport::Fixed {
                            width,
                            height,
                            device_scale_factor_millis,
                            device_type,
                        } => (
                            BrowserViewportMode::Fixed,
                            json!(BrowserViewport {
                                width,
                                height,
                                device_scale_factor_millis,
                                device_type,
                            }),
                        ),
                    };
                    return Ok(json!({
                        "tool": tool_name,
                        "status": "succeeded",
                        "tab_id": tab.tab_id,
                        "mode": mode,
                        "viewport": viewport,
                    })
                    .to_string());
                }
                if action != "set" {
                    return Err(BrowserToolError::new(
                        "invalid_viewport_action",
                        "browser_viewport action 不合法",
                    ));
                }
                let mode = optional_string(arguments, "mode")
                    .unwrap_or_else(|| "fixed".to_string());
                if mode == "auto" {
                    let _control_guard = self.control_lock.lock().await;
                    let tab = tab_in_session(self, &browser_session, &tab.tab_id)?;
                    let reply = client
                        .request(BrowserHostCommand::SetLogicalViewport {
                            tab_id: tab.tab_id.clone(),
                            viewport: magi_browser_authority::BrowserLogicalViewport::Auto,
                        })
                        .await
                        .map_err(browser_host_client_error)?;
                    succeeded_result(reply.response.outcome, "恢复浏览器页面自适应视口失败")?;
                    return Ok(json!({
                        "tool": tool_name,
                        "status": "succeeded",
                        "tab_id": tab.tab_id,
                        "mode": BrowserViewportMode::Auto,
                        "viewport": Value::Null,
                    })
                    .to_string());
                }
                if mode != "fixed" {
                    return Err(BrowserToolError::new(
                        "browser_viewport_mode_invalid",
                        "browser_viewport mode 必须是 auto 或 fixed",
                    ));
                }
                let width = u32_arg(arguments, "width")?;
                let height = u32_arg(arguments, "height")?;
                validate_viewport_dimensions(width, height)?;
                let device_scale_factor_millis = arguments
                    .get("device_scale_factor_millis")
                    .map(|_| u32_arg(arguments, "device_scale_factor_millis"))
                    .transpose()?
                    .unwrap_or(1_000);
                validate_device_scale_factor(device_scale_factor_millis)?;
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
                    device_scale_factor_millis,
                    device_type,
                };
                let reply = client
                    .request(BrowserHostCommand::SetLogicalViewport {
                        tab_id: tab.tab_id.clone(),
                        viewport: magi_browser_authority::BrowserLogicalViewport::Fixed {
                            width,
                            height,
                            device_scale_factor_millis: viewport.device_scale_factor_millis,
                            device_type: viewport.device_type,
                        },
                    })
                    .await
                    .map_err(browser_host_client_error)?;
                succeeded_result(reply.response.outcome, "调整浏览器页面视口失败")?;
                Ok(json!({
                    "tool": tool_name,
                    "status": "succeeded",
                    "tab_id": tab.tab_id,
                    "mode": BrowserViewportMode::Fixed,
                    "viewport": viewport,
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
                        tab_id: tab.tab_id.clone(),
                        control,
                        navigation,
                    })
                    .await
                    .map_err(browser_host_client_error)?;
                let page = page_state(reply.response.outcome, "浏览器导航失败")?;
                let updated = self.apply_page_state(&tab.tab_id, page)?;
                let snapshot = if bool_arg(arguments, "include_snapshot", false) {
                    let snapshot = self.capture_snapshot(&client, &tab.tab_id, None).await?;
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
                        submit_key: optional_string(arguments, "submit_key"),
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
                let reply = client
                    .request(command)
                    .await
                    .map_err(browser_host_client_error)?;
                let page = page_state(reply.response.outcome, "浏览器交互失败")?;
                let updated = self.apply_page_state(&tab.tab_id, page)?;
                let snapshot = if bool_arg(arguments, "include_snapshot", false) {
                    let snapshot = self.capture_snapshot(&client, &tab.tab_id, None).await?;
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
                let has_element_scope = optional_string(arguments, "element_ref").is_some();
                let target = optional_snapshot_target(arguments)?;
                let clip = arguments
                    .get("clip")
                    .map(parse_normalized_rect)
                    .transpose()?;
                let full_page = arguments
                    .get("full_page")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                validate_screenshot_scope(has_element_scope, clip.is_some(), full_page)?;
                let format = match optional_string(arguments, "format").as_deref() {
                    None | Some("png") => magi_browser_authority::BrowserScreenshotFormat::Png,
                    Some("jpeg") => magi_browser_authority::BrowserScreenshotFormat::Jpeg,
                    Some("webp") => magi_browser_authority::BrowserScreenshotFormat::Webp,
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
                if format == magi_browser_authority::BrowserScreenshotFormat::Png
                    && quality.is_some()
                {
                    return Err(BrowserToolError::new(
                        "invalid_screenshot_quality",
                        "PNG 不支持 quality，只有 jpeg 或 webp 支持质量参数",
                    ));
                }
                let reply = client
                    .request(BrowserHostCommand::Screenshot {
                        tab_id: tab.tab_id.clone(),
                        target,
                        clip,
                        full_page,
                        format,
                        quality,
                    })
                    .await
                    .map_err(browser_host_client_error)?;
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
                validate_screenshot_binary(&format, &metadata, &bytes)?;
                let extension = match format {
                    magi_browser_authority::BrowserScreenshotFormat::Png => "png",
                    magi_browser_authority::BrowserScreenshotFormat::Jpeg => "jpg",
                    magi_browser_authority::BrowserScreenshotFormat::Webp => "webp",
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
        arguments: &Map<String, Value>,
        browser_session: &magi_browser_authority::BrowserSession,
        tab: &magi_browser_authority::BrowserTab,
        scope: BrowserToolCallScope<'_>,
        client: &BrowserHostClient,
    ) -> Result<String, BrowserToolError> {
        let operation = browser_devtools_operation(tool_name).ok_or_else(|| {
            BrowserToolError::new("unknown_browser_tool", "未知的浏览器 DevTools 工具")
        })?;
        validate_devtools_arguments(operation, arguments)?;
        let action = optional_string(arguments, "action");
        let lighthouse_mode = optional_string(arguments, "mode");
        let requires_write = match operation {
            "hover" | "drag" | "fill_form" | "upload_file" | "click_at" | "evaluate"
            | "emulate" => true,
            "dialog" => !matches!(action.as_deref(), Some("list")),
            "console" | "network" => matches!(action.as_deref(), Some("clear")),
            "performance" => matches!(
                action.as_deref(),
                Some(
                    "start"
                        | "stop"
                        | "profile_start"
                        | "profile_stop"
                        | "coverage_start"
                        | "coverage_stop"
                )
            ),
            "lighthouse" => !matches!(lighthouse_mode.as_deref(), Some("snapshot")),
            "heap" => matches!(action.as_deref(), Some("take_snapshot" | "close_snapshot")),
            "third_party" => matches!(action.as_deref(), Some("clear")),
            "webmcp" => matches!(action.as_deref(), Some("execute")),
            "pwa" => false,
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
                tab_id: tab.tab_id.clone(),
                control,
                operation: operation.to_string(),
                arguments: Value::Object(arguments.clone()),
            })
            .await
            .map_err(browser_host_client_error)?;
        let BrowserHostCommandResult::Json { value } =
            succeeded_result(reply.response.outcome, "浏览器 DevTools 工具执行失败")?
        else {
            return Err(BrowserToolError::new(
                "browser_result_invalid",
                "浏览器 DevTools 工具返回结果无效",
            ));
        };
        let snapshot = if bool_arg(arguments, "include_snapshot", false) {
            Some(browser_tool_snapshot_value(
                &self.capture_snapshot(client, &tab.tab_id, None).await?,
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
        workspace_id: Option<&WorkspaceId>,
    ) -> Result<magi_browser_authority::BrowserSession, BrowserToolError> {
        let current = self
            .authority
            .lock()
            .expect("browser authority lock poisoned")
            .session_for_magi_session(session_id)
            .cloned();
        if let Some(session) = current {
            if session.workspace_id.as_ref() != workspace_id {
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
                workspace_id: workspace_id.cloned(),
                session_id: session_id.clone(),
                profile_id: BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID),
                now: UtcMillis::now(),
            })?;
            authority.transition_session(
                &browser_session_id,
                magi_browser_authority::BrowserSessionLifecycle::Ready,
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
        session: &magi_browser_authority::BrowserSession,
        arguments: &Map<String, Value>,
        client: &BrowserHostClient,
    ) -> Result<magi_browser_authority::BrowserTab, BrowserToolError> {
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
                    magi_browser_authority::BrowserTabLifecycle::Ready
                        | magi_browser_authority::BrowserTabLifecycle::Suspended
                        | magi_browser_authority::BrowserTabLifecycle::Crashed
                ) {
                    return Err(BrowserToolError::new(
                        "browser_tab_not_ready",
                        "指定的浏览器 Tab 当前不可用",
                    ));
                }
                Some(tab.clone())
            } else {
                authority
                    .active_tab(&session.browser_session_id)
                    .into_iter()
                    .chain(session.tab_ids.iter())
                    .find_map(|id| authority.tab(id).cloned())
                    .filter(|tab| {
                        matches!(
                            tab.lifecycle,
                            magi_browser_authority::BrowserTabLifecycle::Ready
                                | magi_browser_authority::BrowserTabLifecycle::Suspended
                                | magi_browser_authority::BrowserTabLifecycle::Crashed
                        )
                    })
            }
        };
        if let Some(tab) = tab {
            self.mutate(|authority| {
                authority.set_active_tab(&session.browser_session_id, &tab.tab_id)
            })?;
            return self.materialize_tab(tab, client).await;
        }
        let tab_id = BrowserTabId::new(format!(
            "browser-tool-tab-{}-{}",
            UtcMillis::now().0,
            BROWSER_TAB_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let created = self.mutate(|authority| {
            let created = authority.create_tab(CreateBrowserTab {
                tab_id: tab_id.clone(),
                browser_session_id: session.browser_session_id.clone(),
                url: initial_url.clone(),
                now: UtcMillis::now(),
            })?;
            authority.set_active_tab(&session.browser_session_id, &tab_id)?;
            Ok(created)
        })?;
        self.publish_tab_event("browser.tab.created", &created);
        let reply = match client
            .request(BrowserHostCommand::CreatePage {
                tab_id: tab_id.clone(),
                browser_session_id: session.browser_session_id.clone(),
                initial_url,
                logical_viewport: magi_browser_authority::BrowserLogicalViewport::Auto,
                navigation_revision: 0,
                snapshot_revision: 0,
                allow_page_eviction: false,
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
        tab: magi_browser_authority::BrowserTab,
        client: &BrowserHostClient,
    ) -> Result<magi_browser_authority::BrowserTab, BrowserToolError> {
        if !matches!(
            tab.lifecycle,
            magi_browser_authority::BrowserTabLifecycle::Ready
                | magi_browser_authority::BrowserTabLifecycle::Suspended
                | magi_browser_authority::BrowserTabLifecycle::Crashed
        ) {
            return Err(BrowserToolError::new(
                "browser_tab_not_ready",
                "指定的浏览器 Tab 当前不可用",
            ));
        }
        let tab = if tab.lifecycle == magi_browser_authority::BrowserTabLifecycle::Crashed {
            self.mutate(|authority| {
                authority.transition_tab(
                    &tab.tab_id,
                    magi_browser_authority::BrowserTabLifecycle::Suspended,
                    UtcMillis::now(),
                )
            })?
        } else {
            tab
        };
        let reply = client
            .request(BrowserHostCommand::RestorePage {
                tab_id: tab.tab_id.clone(),
                browser_session_id: tab.browser_session_id.clone(),
                initial_url: tab.url.clone(),
                logical_viewport: magi_browser_authority::BrowserLogicalViewport::Auto,
                navigation_revision: tab.navigation_revision,
                snapshot_revision: tab.snapshot_revision,
                allow_page_eviction: false,
            })
            .await
            .map_err(browser_host_client_error)?;
        let page = page_state(reply.response.outcome, "恢复浏览器 Tab 失败")?;
        self.apply_page_state(&tab.tab_id, page)
    }

    async fn prepare_agent_write(
        &self,
        client: &BrowserHostClient,
        session: &magi_browser_authority::BrowserSession,
        tab: &magi_browser_authority::BrowserTab,
        scope: BrowserToolCallScope<'_>,
    ) -> Result<BrowserHostControl, BrowserToolError> {
        let _control_guard = self.control_lock.lock().await;
        // Control is scoped to one physical Surface. There is no session-wide
        // control mode to reclaim; acquiring the target Surface lease below is
        // the only authority transition required for an Agent write.
        let (lease, identity, lease_acquired) = self.acquire_or_reuse_lease(
            session,
            tab,
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
                tab_id: &tab.tab_id,
                surface_id: &lease.surface_id,
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
                tab_id: tab.tab_id.clone(),
                surface_id: lease.surface_id.clone(),
                control: BrowserHostControlUpdate::Agent {
                    lease_id: lease.lease_id.clone(),
                    fence: validated.fence,
                },
            })
            .await
            .map_err(browser_host_client_error)?;
        succeeded_result(reply.response.outcome, "同步浏览器控制权失败")?;
        if lease_acquired {
            self.publish_browser_event(
                "browser.lease.acquired",
                session,
                json!({
                    "lease_id": lease.lease_id,
                    "tab_id": tab.tab_id,
                    "surface_id": lease.surface_id,
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

    fn acquire_or_reuse_lease(
        &self,
        _session: &magi_browser_authority::BrowserSession,
        tab: &magi_browser_authority::BrowserTab,
        context: &magi_tool_runtime::ToolExecutionContext,
        session_id: &SessionId,
        workspace_id: Option<&WorkspaceId>,
    ) -> Result<
        (
            magi_browser_authority::BrowserControlLease,
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
                workspace_id: workspace_id.cloned(),
                mission_id: None,
                task_id: context.task_id.clone(),
                worker_id: context.worker_id.clone(),
                execution_chain_ref: None,
            });
        ownership.session_id = Some(session_id.clone());
        ownership.workspace_id = workspace_id.cloned();
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
        let surface_id = authority
            .primary_surface(&tab.tab_id)
            .map(|surface| surface.surface_id.clone())
            .ok_or_else(|| {
                BrowserToolError::new(
                    "browser_surface_unavailable",
                    "浏览器 Tab 尚未绑定可控制的真实 Browser Surface",
                )
            })?;
        if let Some(lease) = authority
            .active_lease_for_surface(&tab.tab_id, &surface_id)
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
                    "当前浏览器 Surface 正由另一个执行者控制",
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
                tab_id: tab.tab_id.clone(),
                surface_id,
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
            &mut magi_browser_authority::BrowserAuthority,
        ) -> Result<T, magi_browser_authority::BrowserAuthorityError>,
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
        if let Some(persistence) = self.persistence.as_ref() {
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
        page: magi_browser_authority::BrowserHostPageState,
    ) -> Result<magi_browser_authority::BrowserTab, BrowserToolError> {
        let current = self
            .authority
            .lock()
            .expect("browser authority lock poisoned")
            .tab(tab_id)
            .cloned();
        if let Some(current) = current
            && current.lifecycle == magi_browser_authority::BrowserTabLifecycle::Ready
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
                magi_browser_authority::BrowserTabLifecycle::Ready,
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
        tab_id: &BrowserTabId,
        subtree_ref: Option<String>,
    ) -> Result<magi_browser_authority::BrowserHostSnapshot, BrowserToolError> {
        let (navigation_revision, snapshot_revision) =
            self.mutate(|authority| authority.record_snapshot(tab_id, UtcMillis::now()))?;
        let reply = client
            .request(BrowserHostCommand::Snapshot {
                tab_id: tab_id.clone(),
                navigation_revision,
                snapshot_revision,
                limits: Default::default(),
                subtree_ref,
            })
            .await
            .map_err(browser_host_client_error)?;
        let BrowserHostCommandResult::Snapshot(snapshot) =
            succeeded_result(reply.response.outcome, "浏览器快照失败")?
        else {
            return Err(BrowserToolError::new(
                "browser_snapshot_failed",
                "浏览器快照结果无效",
            ));
        };
        if snapshot.tab_id != *tab_id {
            return Err(BrowserToolError::new(
                "browser_snapshot_tab_mismatch",
                "浏览器快照返回了错误的 Tab",
            ));
        }
        self.mutate(|authority| {
            authority.validate_snapshot_result(
                tab_id,
                snapshot.navigation_revision,
                snapshot.snapshot_revision,
            )
        })?;
        Ok(snapshot)
    }

    fn mark_tab_crashed(&self, tab_id: &BrowserTabId) {
        let crashed = self.mutate(|authority| {
            authority.transition_tab(
                tab_id,
                magi_browser_authority::BrowserTabLifecycle::Crashed,
                UtcMillis::now(),
            )
        });
        if let Ok(tab) = crashed {
            self.publish_tab_event("browser.tab.crashed", &tab);
        }
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
        session: &magi_browser_authority::BrowserSession,
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
            return Ok(json!({
                "tool": "browser_tabs",
                "status": "succeeded",
                "active_tab_id": authority.active_tab(&session.browser_session_id),
                "tabs": tabs,
            })
            .to_string());
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
                    let created = authority.create_tab(CreateBrowserTab {
                        tab_id: tab_id.clone(),
                        browser_session_id: session.browser_session_id.clone(),
                        url: initial_url.clone(),
                        now: UtcMillis::now(),
                    })?;
                    authority.set_active_tab(&session.browser_session_id, &tab_id)?;
                    Ok(created)
                })?;
                self.publish_tab_event("browser.tab.created", &created);
                let reply = match client
                    .request(BrowserHostCommand::CreatePage {
                        tab_id: tab_id.clone(),
                        browser_session_id: session.browser_session_id.clone(),
                        initial_url,
                        logical_viewport: magi_browser_authority::BrowserLogicalViewport::Auto,
                        navigation_revision: 0,
                        snapshot_revision: 0,
                        allow_page_eviction: false,
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
                self.mutate(|authority| {
                    authority.set_active_tab(&session.browser_session_id, &tab_id)
                })?;
                self.publish_browser_event(
                    "browser.tab.activated",
                    session,
                    json!({
                        "browser_session_id": session.browser_session_id,
                        "tab_id": tab_id,
                    }),
                );
                Ok(
                    json!({ "tool": "browser_tabs", "status": "succeeded", "tab_id": tab_id })
                        .to_string(),
                )
            }
            "close" => {
                let tab_id = BrowserTabId::new(string_arg(arguments, "tab_id")?);
                let _target = tab_in_session(self, session, &tab_id)?;
                let close_result = client
                    .request(BrowserHostCommand::ClosePage {
                        tab_id: tab_id.clone(),
                    })
                    .await;
                let closed = self.mutate(|authority| {
                    authority.transition_tab(
                        &tab_id,
                        magi_browser_authority::BrowserTabLifecycle::Closed,
                        UtcMillis::now(),
                    )
                })?;
                self.publish_tab_event("browser.tab.closed", &closed);
                if let Err(error) = close_result {
                    return Err(BrowserToolError::new(
                        "browser_host_disconnected",
                        error.to_string(),
                    ));
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
        "browser_heap" => Some("heap"),
        "browser_third_party" => Some("third_party"),
        "browser_webmcp" => Some("webmcp"),
        "browser_pwa" => Some("pwa"),
        _ => None,
    }
}

fn validate_devtools_arguments(
    operation: &str,
    arguments: &Map<String, Value>,
) -> Result<(), BrowserToolError> {
    match operation {
        "drag" => {
            validate_nested_snapshot_target(arguments, "source")?;
            validate_nested_snapshot_target(arguments, "target")?;
        }
        "fill_form" => {
            let fields = arguments
                .get("fields")
                .and_then(Value::as_array)
                .filter(|fields| !fields.is_empty())
                .ok_or_else(|| {
                    BrowserToolError::new(
                        "invalid_arguments",
                        "browser_fill_form 必须提供非空 fields 数组",
                    )
                })?;
            for field in fields {
                let field = field.as_object().ok_or_else(|| {
                    BrowserToolError::new(
                        "invalid_arguments",
                        "browser_fill_form.fields 的每一项必须是对象",
                    )
                })?;
                validate_snapshot_target_object(field, "browser_fill_form.fields")?;
                if !field.contains_key("value") {
                    return Err(BrowserToolError::new(
                        "invalid_arguments",
                        "browser_fill_form.fields 的每一项必须包含 value",
                    ));
                }
                let value = field.get("value").expect("value was checked above");
                if !is_supported_fill_value(value) {
                    return Err(BrowserToolError::new(
                        "invalid_arguments",
                        "browser_fill_form.fields.value 必须是字符串、数字、布尔值或字符串/数字数组",
                    ));
                }
            }
        }
        "performance" => {
            let action = arguments
                .get("action")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    BrowserToolError::new(
                        "invalid_arguments",
                        "browser_performance 必须提供 action",
                    )
                })?;
            if !matches!(
                action,
                "start"
                    | "stop"
                    | "metrics"
                    | "analyze"
                    | "profile_start"
                    | "profile_stop"
                    | "coverage_start"
                    | "coverage_take"
                    | "coverage_stop"
            ) {
                return Err(BrowserToolError::new(
                    "invalid_arguments",
                    format!("browser_performance 不支持 action: {action}"),
                ));
            }
        }
        "heap" => {
            let action = arguments
                .get("action")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    BrowserToolError::new("invalid_arguments", "browser_heap 必须提供 action")
                })?;
            if !matches!(
                action,
                "usage"
                    | "take_snapshot"
                    | "close_snapshot"
                    | "compare_snapshots"
                    | "summary"
                    | "details"
                    | "class_nodes"
                    | "dominators"
                    | "duplicate_strings"
                    | "edges"
                    | "object_details"
                    | "retainers"
                    | "retaining_paths"
            ) {
                return Err(BrowserToolError::new(
                    "invalid_arguments",
                    format!("browser_heap 不支持 action: {action}"),
                ));
            }
        }
        "pwa" => {
            if arguments.get("action").and_then(Value::as_str) != Some("state") {
                return Err(BrowserToolError::new(
                    "invalid_arguments",
                    "browser_pwa 只支持 state 状态审计，不会安装、启动或卸载系统 PWA",
                ));
            }
        }
        "third_party" => {
            let action = arguments
                .get("action")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    BrowserToolError::new(
                        "invalid_arguments",
                        "browser_third_party 必须提供 action",
                    )
                })?;
            if !matches!(action, "list" | "clear") {
                return Err(BrowserToolError::new(
                    "invalid_arguments",
                    format!("browser_third_party 不支持 action: {action}"),
                ));
            }
        }
        "evaluate" => {
            let expression = arguments
                .get("expression")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|expression| !expression.is_empty())
                .ok_or_else(|| {
                    BrowserToolError::new(
                        "invalid_arguments",
                        "browser_evaluate 必须提供非空 expression",
                    )
                })?;
            if expression.len() > 100_000 {
                return Err(BrowserToolError::new(
                    "invalid_arguments",
                    "browser_evaluate.expression 不能超过 100000 个字符",
                ));
            }
        }
        "upload_file" => {
            validate_snapshot_target_object(arguments, "browser_upload_file")?;
            let file_path = arguments
                .get("file_path")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let file_paths = arguments
                .get("file_paths")
                .and_then(Value::as_array)
                .filter(|values| !values.is_empty())
                .filter(|values| values.len() <= 20)
                .and_then(|values| values.iter().map(Value::as_str).collect::<Option<Vec<_>>>());
            if file_path.is_none() && file_paths.is_none() {
                return Err(BrowserToolError::new(
                    "invalid_arguments",
                    "browser_upload_file 必须提供 file_path 或非空 file_paths",
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

fn is_supported_fill_value(value: &Value) -> bool {
    match value {
        Value::String(_) | Value::Number(_) | Value::Bool(_) => true,
        Value::Array(values) => values
            .iter()
            .all(|item| item.is_string() || item.is_number()),
        Value::Null | Value::Object(_) => false,
    }
}

fn validate_nested_snapshot_target(
    arguments: &Map<String, Value>,
    name: &str,
) -> Result<(), BrowserToolError> {
    let target = arguments
        .get(name)
        .and_then(Value::as_object)
        .ok_or_else(|| {
            BrowserToolError::new(
                "invalid_arguments",
                format!("browser_drag 必须提供 {name} 快照引用"),
            )
        })?;
    validate_snapshot_target_object(target, &format!("browser_drag.{name}"))
}

fn validate_snapshot_target_object(
    target: &Map<String, Value>,
    context: &str,
) -> Result<(), BrowserToolError> {
    let revision = target
        .get("snapshot_revision")
        .and_then(Value::as_u64)
        .filter(|revision| *revision > 0)
        .ok_or_else(|| {
            BrowserToolError::new(
                "invalid_arguments",
                format!("{context}.snapshot_revision 必须是正整数"),
            )
        })?;
    let _ = revision;
    let element_ref = target
        .get("element_ref")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|element_ref| !element_ref.is_empty() && *element_ref != "root")
        .ok_or_else(|| {
            BrowserToolError::new(
                "invalid_arguments",
                format!("{context}.element_ref 必须是有效的快照元素引用"),
            )
        })?;
    let _ = element_ref;
    Ok(())
}

fn tab_in_session(
    runtime: &BrowserToolRuntimeDependencies,
    session: &magi_browser_authority::BrowserSession,
    tab_id: &BrowserTabId,
) -> Result<magi_browser_authority::BrowserTab, BrowserToolError> {
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
            | BrowserToolKind::ThirdParty
            | BrowserToolKind::WebMcp
            | BrowserToolKind::Pwa
    ) {
        return match (kind, action) {
            (BrowserToolKind::Dialog, Some("list"))
            | (BrowserToolKind::Console, Some("list" | "get"))
            | (BrowserToolKind::Network, Some("list" | "get" | "failed"))
            | (BrowserToolKind::Performance, Some("metrics" | "analyze"))
            | (
                BrowserToolKind::Heap,
                Some(
                    "usage" | "compare_snapshots" | "summary" | "details" | "class_nodes"
                    | "dominators" | "duplicate_strings" | "edges" | "object_details" | "retainers"
                    | "retaining_paths",
                ),
            )
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

fn validate_device_scale_factor(value: u32) -> Result<(), BrowserToolError> {
    if !(500..=4_000).contains(&value) {
        return Err(BrowserToolError::new(
            "browser_viewport_invalid",
            "浏览器页面设备像素比超出支持范围",
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

fn capability_unavailable(tool: &str, message: &str) -> (String, ExecutionResultStatus) {
    let details = json!({
        "capability": "desktop_browser_surface",
        "supported_platform": "desktop",
        "available_fallback": "browser_records",
    });
    failure_payload(
        tool,
        "capability_unavailable",
        message,
        true,
        false,
        Some(&details),
    )
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
) -> Result<magi_browser_authority::BrowserNormalizedRect, BrowserToolError> {
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
    let rect = magi_browser_authority::BrowserNormalizedRect {
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

fn validate_screenshot_scope(
    has_element_ref: bool,
    has_clip: bool,
    full_page: bool,
) -> Result<(), BrowserToolError> {
    let scopes = [has_element_ref, has_clip, full_page]
        .into_iter()
        .filter(|value| *value)
        .count();
    if scopes > 1 {
        return Err(BrowserToolError::new(
            "invalid_screenshot_scope",
            "element_ref、clip、full_page 三者只能选择一个截图范围",
        ));
    }
    Ok(())
}

fn validate_screenshot_binary(
    format: &magi_browser_authority::BrowserScreenshotFormat,
    metadata: &magi_browser_authority::BrowserHostBinaryPayload,
    bytes: &[u8],
) -> Result<(), BrowserToolError> {
    let (mime, valid_header) = match format {
        magi_browser_authority::BrowserScreenshotFormat::Png => (
            "image/png",
            bytes.len() >= 8 && bytes[..8] == [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
        ),
        magi_browser_authority::BrowserScreenshotFormat::Jpeg => (
            "image/jpeg",
            bytes.len() >= 3 && bytes[0..3] == [0xff, 0xd8, 0xff],
        ),
        magi_browser_authority::BrowserScreenshotFormat::Webp => (
            "image/webp",
            bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        ),
    };
    let digest = format!("{:x}", Sha256::digest(bytes));
    if metadata.mime_type != mime
        || metadata.byte_length != bytes.len() as u64
        || metadata.sha256 != digest
        || !valid_header
    {
        return Err(BrowserToolError::new(
            "browser_screenshot_format_mismatch",
            "截图 MIME、文件头、长度或 SHA-256 校验不一致",
        ));
    }
    Ok(())
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

fn browser_host_client_error(error: BrowserHostClientError) -> BrowserToolError {
    let code = match &error {
        BrowserHostClientError::RequestTimeout(_) => "browser_host_request_timeout",
        BrowserHostClientError::UnexpectedResponse(_)
        | BrowserHostClientError::UnexpectedBinaryPayload
        | BrowserHostClientError::BinarySizeMismatch { .. }
        | BrowserHostClientError::BinaryHashMismatch
        | BrowserHostClientError::Json(_)
        | BrowserHostClientError::ProtocolIncompatible { .. } => "browser_host_protocol_error",
        BrowserHostClientError::Disconnected => "browser_host_disconnected",
        BrowserHostClientError::InvalidConfiguration(_)
        | BrowserHostClientError::Connect(_)
        | BrowserHostClientError::Transport(_)
        | BrowserHostClientError::HandshakeTimeout
        | BrowserHostClientError::DesktopEpochMismatch { .. }
        | BrowserHostClientError::DesktopProcessMismatch { .. } => "browser_host_unavailable",
    };
    BrowserToolError::new(code, error.to_string())
}

fn page_state(
    outcome: BrowserHostCommandOutcome,
    context: &str,
) -> Result<magi_browser_authority::BrowserHostPageState, BrowserToolError> {
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
        "navigation_revision".to_string(),
        json!(snapshot.navigation_revision),
    );
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
    for (priority, budget) in PRIORITY_BUDGETS.iter().copied().enumerate() {
        for node in nodes
            .iter()
            .filter(|node| snapshot_node_priority(node) == priority as u8)
            .take(budget)
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
    if !snapshot.accessibility_tree.is_empty() {
        value.insert(
            "accessibility_tree".to_string(),
            Value::Array(snapshot.accessibility_tree.clone()),
        );
    }
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

    use magi_browser_authority::{
        BrowserAuthority, BrowserHostPageState, BrowserHostRect, BrowserHostSnapshot,
        BrowserProfile, BrowserProfileKind, BrowserSessionLifecycle, BrowserSnapshotNode,
        CreateBrowserSession, CreateBrowserTab,
    };
    use magi_core::{
        BrowserProfileId, BrowserSessionId, BrowserTabId, ExecutionResultStatus, SessionId,
        ToolCallId, UtcMillis, WorkspaceId,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_session_store::SessionStore;
    use serde_json::{Map, Value, json};

    use super::{
        BrowserToolRuntimeDependencies, DEFAULT_BROWSER_PROFILE_ID, browser_tool_requested_access,
        browser_tool_snapshot_value, optional_snapshot_target, parse_normalized_rect,
        validate_devtools_arguments, validate_screenshot_binary, validate_screenshot_scope,
    };
    use crate::state::BrowserHostStatusSnapshot;
    use magi_browser_authority::{BrowserToolAccess, BrowserToolKind};

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
            host_status: Arc::new(RwLock::new(BrowserHostStatusSnapshot::default())),
            host_client: Arc::new(RwLock::new(None)),
            event_bus: Arc::clone(&event_bus),
            session_store: Arc::new(SessionStore::new()),
            persistence: None,
        };
        let session_id = SessionId::new("session-browser-tabs-list");
        let workspace_id = WorkspaceId::new("workspace-browser-tabs-list");

        let created = runtime
            .ensure_session(&session_id, Some(&workspace_id))
            .expect("browser session should create");
        assert!(created.tab_ids.is_empty());
        let existing = runtime
            .ensure_session(&session_id, Some(&workspace_id))
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
                workspace_id: Some(workspace_id),
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
                now: UtcMillis(4),
            })
            .expect("browser tab should create");
        let event_bus = Arc::new(InMemoryEventBus::new(16));
        let runtime = BrowserToolRuntimeDependencies {
            authority: Arc::new(Mutex::new(authority)),
            write_lock: Arc::new(Mutex::new(())),
            control_lock: Arc::new(tokio::sync::Mutex::new(())),
            state_writable: Arc::new(AtomicBool::new(true)),
            host_status: Arc::new(RwLock::new(BrowserHostStatusSnapshot::default())),
            host_client: Arc::new(RwLock::new(None)),
            event_bus: Arc::clone(&event_bus),
            session_store: Arc::new(SessionStore::new()),
            persistence: None,
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
    fn browser_tools_return_structured_error_when_host_is_not_ready() {
        let runtime = BrowserToolRuntimeDependencies {
            authority: Arc::new(Mutex::new(BrowserAuthority::new())),
            write_lock: Arc::new(Mutex::new(())),
            control_lock: Arc::new(tokio::sync::Mutex::new(())),
            state_writable: Arc::new(AtomicBool::new(true)),
            host_status: Arc::new(RwLock::new(BrowserHostStatusSnapshot::default())),
            host_client: Arc::new(RwLock::new(None)),
            event_bus: Arc::new(InMemoryEventBus::new(8)),
            session_store: Arc::new(SessionStore::new()),
            persistence: None,
        };
        let context = magi_tool_runtime::ToolExecutionContext {
            session_id: Some(SessionId::new("session-browser-capability")),
            browser_capability_revision: Some(1),
            ..Default::default()
        };
        let (payload, status) = runtime.execute(
            &ToolCallId::new("call-browser-capability"),
            "browser_lighthouse",
            "{}",
            &context,
        );
        let payload: Value = serde_json::from_str(&payload).expect("capability payload JSON");
        assert_eq!(status, ExecutionResultStatus::Failed);
        assert_eq!(payload["error_code"], "capability_unavailable");
        assert_eq!(payload["details"]["capability"], "desktop_browser_surface");
        assert_eq!(payload["details"]["supported_platform"], "desktop");
    }

    #[test]
    fn browser_devtools_contract_uses_source_target_fields_and_expression() {
        let mut arguments = Map::new();
        arguments.insert(
            "source".to_string(),
            json!({"snapshot_revision": 2, "element_ref": "e-1"}),
        );
        arguments.insert(
            "target".to_string(),
            json!({"snapshot_revision": 2, "element_ref": "e-2"}),
        );
        validate_devtools_arguments("drag", &arguments)
            .expect("drag should use source and target snapshot references");
        assert!(validate_devtools_arguments("drag", &Map::new()).is_err());

        let mut fill_arguments = Map::new();
        fill_arguments.insert(
            "fields".to_string(),
            json!([{"snapshot_revision": 2, "element_ref": "e-1", "value": "Magi"}]),
        );
        validate_devtools_arguments("fill_form", &fill_arguments)
            .expect("fill_form should use fields");

        let mut invalid_fill = Map::new();
        invalid_fill.insert(
            "fields".to_string(),
            json!([{"snapshot_revision": 2, "element_ref": "e-1", "value": {"unexpected": true}}]),
        );
        assert!(validate_devtools_arguments("fill_form", &invalid_fill).is_err());

        let mut invalid_pwa = Map::new();
        invalid_pwa.insert("action".to_string(), json!("install"));
        assert!(validate_devtools_arguments("pwa", &invalid_pwa).is_err());

        let mut invalid_heap = Map::new();
        invalid_heap.insert("action".to_string(), json!("query_objects"));
        assert!(validate_devtools_arguments("heap", &invalid_heap).is_err());

        let mut invalid_third_party = Map::new();
        invalid_third_party.insert("action".to_string(), json!("execute"));
        assert!(validate_devtools_arguments("third_party", &invalid_third_party).is_err());

        assert!(validate_devtools_arguments("third_party", &Map::new()).is_err());

        let mut third_party_list = Map::new();
        third_party_list.insert("action".to_string(), json!("list"));
        validate_devtools_arguments("third_party", &third_party_list)
            .expect("third_party list should be supported");
        assert_eq!(
            browser_tool_requested_access(BrowserToolKind::ThirdParty, &third_party_list),
            BrowserToolAccess::Read
        );
        let mut third_party_clear = Map::new();
        third_party_clear.insert("action".to_string(), json!("clear"));
        validate_devtools_arguments("third_party", &third_party_clear)
            .expect("third_party clear should be supported");
        assert_eq!(
            browser_tool_requested_access(BrowserToolKind::ThirdParty, &third_party_clear),
            BrowserToolAccess::Write
        );
        let mut pwa_state = Map::new();
        pwa_state.insert("action".to_string(), json!("state"));
        assert_eq!(
            browser_tool_requested_access(BrowserToolKind::Pwa, &pwa_state),
            BrowserToolAccess::Read
        );

        let mut evaluate_arguments = Map::new();
        evaluate_arguments.insert("expression".to_string(), json!("document.title"));
        validate_devtools_arguments("evaluate", &evaluate_arguments)
            .expect("evaluate should use expression");
        assert!(validate_devtools_arguments("evaluate", &Map::new()).is_err());
    }

    #[test]
    fn browser_screenshot_root_is_page_scope_and_normalized_clip_is_validated() {
        let root = json!({ "element_ref": "root", "snapshot_revision": 4 });
        assert_eq!(
            optional_snapshot_target(root.as_object().expect("root target object"))
                .expect("root target should parse"),
            None
        );

        let element = json!({ "element_ref": "e:4:1", "snapshot_revision": 4 });
        assert_eq!(
            optional_snapshot_target(element.as_object().expect("element target object"))
                .expect("element target should parse")
                .expect("non-root target should remain addressable")
                .element_ref,
            "e:4:1"
        );

        let clip = parse_normalized_rect(&json!({
            "x": 0.25,
            "y": 0.1,
            "width": 0.5,
            "height": 0.25
        }))
        .expect("normalized clip should be accepted");
        assert_eq!(clip.x, 0.25);
        assert_eq!(clip.width, 0.5);
        assert!(
            parse_normalized_rect(&json!({
                "x": 0.8,
                "y": 0.1,
                "width": 0.5,
                "height": 0.25
            }))
            .is_err()
        );
    }

    #[test]
    fn browser_screenshot_scope_is_explicitly_mutually_exclusive() {
        assert!(validate_screenshot_scope(true, false, false).is_ok());
        assert!(validate_screenshot_scope(false, true, false).is_ok());
        assert!(validate_screenshot_scope(false, false, true).is_ok());
        for (element, clip, full_page) in [
            (true, true, false),
            (true, false, true),
            (false, true, true),
        ] {
            let error = validate_screenshot_scope(element, clip, full_page)
                .expect_err("screenshot scopes must not be combined");
            assert_eq!(error.code, "invalid_screenshot_scope");
        }
    }

    #[test]
    fn browser_screenshot_metadata_and_file_header_must_match() {
        use sha2::{Digest, Sha256};

        let bytes = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        let metadata = magi_browser_authority::BrowserHostBinaryPayload {
            payload_id: "payload-1".to_string(),
            mime_type: "image/png".to_string(),
            byte_length: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        validate_screenshot_binary(
            &magi_browser_authority::BrowserScreenshotFormat::Png,
            &metadata,
            &bytes,
        )
        .expect("valid PNG metadata should pass");

        let mut invalid = metadata;
        invalid.mime_type = "image/webp".to_string();
        let error = validate_screenshot_binary(
            &magi_browser_authority::BrowserScreenshotFormat::Png,
            &invalid,
            &bytes,
        )
        .expect_err("MIME mismatch must fail");
        assert_eq!(error.code, "browser_screenshot_format_mismatch");
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
            navigation_revision: 1,
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
            accessibility_tree: Vec::new(),
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
            host_status: Arc::new(RwLock::new(BrowserHostStatusSnapshot::default())),
            host_client: Arc::new(RwLock::new(None)),
            event_bus,
            session_store: Arc::new(SessionStore::new()),
            persistence: None,
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
