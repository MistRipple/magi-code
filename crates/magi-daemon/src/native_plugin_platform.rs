//! Native DSH plugin platform coordinator implementing `magi_api::NativePluginPlatform`.
//! Bridges HTTP/UI projection, static upstream preview, client sandbox assets, and `PluginWebGateway`
//! directly onto `PluginRuntime` + `PluginManager` (single source of truth).

use crate::{
    plugin_management::PluginBundleManagementConfig,
    plugin_runtime::{PluginRuntime, PluginRuntimeConfig, SharedHostSupervisorState},
};
use axum::{
    body::{Body, to_bytes},
    http::{HeaderName, HeaderValue, Method, Request},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use magi_api::native_plugins::{
    InstalledUpstreamPlugins, PluginInstallOutcome, PluginRouteSession,
};
use magi_api::{
    ApiError, NativePlatformFuture, NativePluginPlatform, NpmArchive, PluginClientSlotsReport,
    fetch_npm_packument, fetch_npm_release_archive,
};
use magi_app_server_protocol::generated::{
    PluginCandidateOperation, PluginCandidatePreview, PluginClientContribution,
    PluginContributions, PluginEcosystem, PluginExtension, PluginInstalled, PluginManifest,
    PluginNamedContribution, PluginPermission, PluginPermissionKind, PluginRouteCallRequest,
    PluginRouteCallResult, PluginRouteMethod, PluginRuntimeState, PluginRuntimeStatus,
    PluginSourceKind, PluginSourceProjection, PluginToolContribution, PluginUpstreamPreview,
    PluginUpstreamRequest, PluginViewContribution, PluginViewPlacement,
};
use magi_core::fs_atomic::write_atomic;
use magi_core::{EventId, SessionId};
use magi_event_bus::{EventEnvelope, InMemoryEventBus};
use magi_extension_host::{
    peer::{BoxFuture, Incoming},
    protocol::{ErrorKind, RpcError},
};
use magi_plugin_system::{
    LoadingActivation, LoadingApplication, LoadingInstanceStatus, PluginManager,
    loading_runtime::{LoadingPreparation, LoadingSourceObservation},
    npm::is_valid_package_name,
};
use magi_process::ManagedProcessGroup;
use magi_session_store::{CanonicalTurn, CanonicalTurnEvent, SessionInput, SessionRecord};
use magi_settings_store::SettingsStore;
use magi_tool_runtime::{PluginToolCatalogProvider, PluginToolExecutor, WebProviderExecutor};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, RwLock, Weak},
    time::{Duration, Instant},
};
use tokio::sync::Notify;

pub(crate) mod session_facts;
pub(crate) mod session_observation;

const LOCKED_DSH_VERSION: &str = "0.2.2-rc.3";
const DEFAULT_NPM_REGISTRY: &str = "https://registry.npmjs.org";
const PREVIEW_RETENTION_TTL: Duration = Duration::from_secs(900);
const MAX_PREVIEW_CACHE: usize = 32;

const SANDBOX_INDEX_HTML: &str = r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <link rel="stylesheet" href="./dsh-client-host.css" />
</head>
<body>
  <div id="root"></div>
  <script src="./dsh-client-host.js"></script>
  <script src="./plugin-client.js"></script>
</body>
</html>
"#;

#[derive(Clone, Debug, Default)]
struct ObservedClientSlots {
    instance_id: Option<String>,
    mounted_slots: Vec<String>,
    unmounted_slots: Vec<String>,
    has_settings_view: Option<bool>,
    main_keys: Vec<String>,
    right_pane_tabs: Vec<magi_api::native_plugins::PluginRightPaneTab>,
}

#[derive(Clone, Debug)]
struct RetainedArchivePreview {
    digest: String,
    package_name: String,
    created_at: Instant,
}

#[derive(Clone, Debug)]
struct ResolvedDshToolchain {
    node: PathBuf,
    npm_cli: PathBuf,
    sdk_project: PathBuf,
}

fn page_instance_id(activation: &LoadingActivation) -> String {
    format!(
        "{}:{}:{}",
        activation.host_generation,
        activation.activation_generation,
        activation.executable.instance_id,
    )
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SessionHistoryRequest {
    pub(crate) session_id: SessionId,
    pub(crate) after: u64,
    pub(crate) through: Option<u64>,
    pub(crate) limit: usize,
    #[serde(default)]
    pub(crate) subagent_task_id: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionIdRequest {
    session_id: SessionId,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionRenameRequest {
    session_id: SessionId,
    title: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionQueueRequest {
    session_id: SessionId,
    queue_id: String,
    action: SessionQueueAction,
}

#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum SessionQueueAction {
    Edit { text: String },
    Remove,
    Steer,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionPromptRequest {
    session_id: SessionId,
    request_id: String,
    #[serde(default)]
    steer: bool,
    text: String,
    #[serde(default)]
    images: Vec<SessionPromptImage>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionPromptImage {
    #[serde(default)]
    name: Option<String>,
    media_type: String,
    data: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionCreateRequest {
    #[serde(default)]
    workspace_id: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionCommandEventRequest {
    session_id: SessionId,
    event: magi_session_store::SessionCommandEvent,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionAccessProfileRequest {
    session_id: SessionId,
    access_profile: magi_core::AccessProfile,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GoalIdRequest {
    session_id: SessionId,
    goal_id: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoalCreateRequest {
    session_id: SessionId,
    #[serde(alias = "title", alias = "text")]
    objective: String,
    #[serde(default)]
    access_profile: magi_core::AccessProfile,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoalUpdateRequest {
    session_id: SessionId,
    goal_id: String,
    #[serde(alias = "title", alias = "text")]
    objective: String,
}

type SessionHistoryReader =
    Arc<dyn Fn(SessionHistoryRequest) -> Result<Value, RpcError> + Send + Sync>;

struct RuntimeEventSink {
    events: RwLock<Option<Arc<InMemoryEventBus>>>,
    session_history: OnceLock<SessionHistoryReader>,
    session_facts: OnceLock<session_facts::SessionFacts>,
    session_control: RwLock<Option<Arc<magi_api::plugin_session_control::PluginSessionControl>>>,
}

impl RuntimeEventSink {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            events: RwLock::new(None),
            session_history: OnceLock::new(),
            session_facts: OnceLock::new(),
            session_control: RwLock::new(None),
        })
    }

    fn bind_events(&self, events: Arc<InMemoryEventBus>) {
        *self.events.write().expect("events lock") = Some(events);
    }

    fn publish_changed(&self) {
        if let Some(bus) = self.events.read().expect("events lock").clone() {
            bus.publish(EventEnvelope::domain(
                EventId::unique("plugin-registry-changed"),
                "plugin.registry.changed",
                json!({}),
            ));
        }
    }
}

impl Incoming for RuntimeEventSink {
    fn request(&self, method: String, params: Value) -> BoxFuture<Result<Value, RpcError>> {
        if method == "host.session.history" {
            let reader = self.session_history.get().cloned();
            return Box::pin(async move {
                let reader = reader
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话历史服务尚未装配"))?;
                let request: SessionHistoryRequest = serde_json::from_value(params)
                    .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
                if !(1..=256).contains(&request.limit)
                    || request
                        .through
                        .is_some_and(|through| request.after > through)
                    || request
                        .subagent_task_id
                        .as_deref()
                        .is_some_and(|id| id.trim().is_empty())
                {
                    return Err(RpcError::new(
                        ErrorKind::InvalidParams,
                        "会话历史读取范围无效",
                    ));
                }
                tokio::task::spawn_blocking(move || reader(request))
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Internal, error.to_string()))?
            });
        }
        if matches!(method.as_str(), "host.session.list" | "host.session.get") {
            let facts = self.session_facts.get().cloned();
            return Box::pin(async move {
                let facts = facts
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话事实面尚未装配"))?;
                tokio::task::spawn_blocking(move || match method.as_str() {
                    "host.session.list" => facts.list(params),
                    _ => facts.get(params),
                })
                .await
                .map_err(|error| RpcError::new(ErrorKind::Internal, error.to_string()))?
            });
        }
        if method == "host.session.queue" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                use magi_api::plugin_session_control::PluginQueueAction;
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let request: SessionQueueRequest = serde_json::from_value(params)
                    .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
                let action = match request.action {
                    SessionQueueAction::Edit { text } => PluginQueueAction::Edit { text },
                    SessionQueueAction::Remove => PluginQueueAction::Remove,
                    SessionQueueAction::Steer => PluginQueueAction::Steer,
                };
                control
                    .update_queue(&request.session_id, &request.queue_id, action)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "accepted": true }))
            });
        }
        if method == "host.session.prompt" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let request: SessionPromptRequest = serde_json::from_value(params)
                    .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
                let prompt = magi_api::plugin_session_control::PluginPrompt {
                    request_id: request.request_id,
                    steer: request.steer,
                    text: request.text,
                    images: request
                        .images
                        .into_iter()
                        .map(
                            |image| magi_api::plugin_session_control::PluginPromptImage {
                                name: image.name,
                                media_type: image.media_type,
                                data: image.data,
                            },
                        )
                        .collect(),
                };
                control
                    .prompt(&request.session_id, prompt)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "accepted": true }))
            });
        }
        if method == "host.session.create" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let request: SessionCreateRequest = serde_json::from_value(params)
                    .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
                let session_id = control
                    .create(request.workspace_id.as_deref())
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "sessionId": session_id }))
            });
        }
        if method == "host.session.rename" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let request: SessionRenameRequest = serde_json::from_value(params)
                    .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
                let (title, seq) = control
                    .rename(&request.session_id, &request.title)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "title": title, "seq": seq }))
            });
        }
        if method == "host.session.cancel" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let request: SessionIdRequest = serde_json::from_value(params)
                    .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
                control
                    .cancel(&request.session_id)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "accepted": true }))
            });
        }
        if method == "host.session.appendCommandEvent" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let request: SessionCommandEventRequest = serde_json::from_value(params)
                    .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
                let seq = control
                    .append_command_event(&request.session_id, request.event)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "seq": seq }))
            });
        }
        if method == "host.session.setAccessProfile" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let request: SessionAccessProfileRequest = serde_json::from_value(params)
                    .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
                let profile = control
                    .set_access_profile(&request.session_id, request.access_profile)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "accessProfile": profile }))
            });
        }
        if method == "host.goals.list" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let request: SessionIdRequest = serde_json::from_value(params)
                    .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
                let goals = control
                    .list_goals(&request.session_id)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "goals": goals }))
            });
        }
        if method == "host.goals.get" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let request: GoalIdRequest = serde_json::from_value(params)
                    .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
                let goal = control
                    .get_goal(&request.session_id, &request.goal_id)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "goal": goal }))
            });
        }
        if method == "host.goals.create" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let request: GoalCreateRequest = serde_json::from_value(params)
                    .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
                let goal = control
                    .create_goal(
                        &request.session_id,
                        &request.objective,
                        request.access_profile,
                    )
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "goal": goal }))
            });
        }
        if method == "host.goals.update" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let request: GoalUpdateRequest = serde_json::from_value(params)
                    .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
                let goal = control
                    .update_goal(&request.session_id, &request.goal_id, &request.objective)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "goal": goal }))
            });
        }
        if method == "host.goals.delete" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let request: GoalIdRequest = serde_json::from_value(params)
                    .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
                control
                    .delete_goal(&request.session_id, &request.goal_id)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "deleted": true }))
            });
        }
        if method == "host.worktrees.list" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let session_id = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(SessionId::new);
                let worktrees = control
                    .list_worktrees(session_id.as_ref())
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "worktrees": worktrees }))
            });
        }
        if method == "host.mcp.list" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let servers = control
                    .list_mcp_servers()
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "servers": servers }))
            });
        }
        if method == "host.browser.snapshot" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let snapshot = control
                    .browser_snapshot()
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(snapshot)
            });
        }
        if method == "host.terminals.list" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let session_id = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let terminals = control
                    .terminal_list(session_id)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "terminals": terminals }))
            });
        }
        if method == "host.terminals.create" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let session_id = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(magi_core::SessionId::new)
                    .ok_or_else(|| RpcError::new(ErrorKind::InvalidParams, "缺少 sessionId"))?;
                let tab_id = params
                    .get("id")
                    .or_else(|| params.get("terminalTabId"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| RpcError::new(ErrorKind::InvalidParams, "缺少 terminal id"))?;
                let cwd = params.get("cwd").and_then(Value::as_str);
                let cols = params.get("cols").and_then(Value::as_u64).unwrap_or(80) as u16;
                let rows = params.get("rows").and_then(Value::as_u64).unwrap_or(24) as u16;
                let terminal = control
                    .terminal_create(&session_id, tab_id, cwd, cols, rows)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "terminal": terminal }))
            });
        }
        if method == "host.terminals.write" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let id = params
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError::new(ErrorKind::InvalidParams, "缺少 terminal id"))?;
                let data = params
                    .get("data")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError::new(ErrorKind::InvalidParams, "缺少写入数据"))?;
                control
                    .terminal_write(id, data)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))
            });
        }
        if method == "host.terminals.resize" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let id = params
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError::new(ErrorKind::InvalidParams, "缺少 terminal id"))?;
                let cols = params.get("cols").and_then(Value::as_u64).unwrap_or(80) as u16;
                let rows = params.get("rows").and_then(Value::as_u64).unwrap_or(24) as u16;
                control
                    .terminal_resize(id, cols, rows)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))
            });
        }
        if method == "host.terminals.read" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let id = params
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError::new(ErrorKind::InvalidParams, "缺少 terminal id"))?;
                control
                    .terminal_read(id)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))
            });
        }
        if method == "host.terminals.close" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let id = params
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError::new(ErrorKind::InvalidParams, "缺少 terminal id"))?;
                control
                    .terminal_close(id)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))
            });
        }
        if method == "host.worktrees.create" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let repo_dir = params
                    .get("repoDir")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError::new(ErrorKind::InvalidParams, "缺少 repoDir"))?;
                let path = params
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError::new(ErrorKind::InvalidParams, "缺少 path"))?;
                let branch = params.get("branch").and_then(Value::as_str);
                let base = params.get("base").and_then(Value::as_str);
                let detached = params
                    .get("detach")
                    .or_else(|| params.get("detached"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let entry = control
                    .worktree_create(repo_dir, path, branch, base, detached)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "worktree": entry }))
            });
        }
        if method == "host.worktrees.remove" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let repo_dir = params
                    .get("repoDir")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError::new(ErrorKind::InvalidParams, "缺少 repoDir"))?;
                let path = params
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError::new(ErrorKind::InvalidParams, "缺少 path"))?;
                let force = params
                    .get("force")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                control
                    .worktree_remove(repo_dir, path, force)
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))
            });
        }
        if method == "host.subagentModelSelection.get" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                control
                    .subagent_model_selection()
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))
            });
        }
        if method == "host.mcp.resources" {
            let control = self
                .session_control
                .read()
                .expect("session control lock")
                .clone();
            return Box::pin(async move {
                let control = control
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "会话控制端口尚未装配"))?;
                let resources = control
                    .mcp_resources()
                    .await
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, format!("{error:?}")))?;
                Ok(json!({ "resources": resources }))
            });
        }
        Box::pin(async move {
            Err(RpcError::new(
                ErrorKind::MethodNotFound,
                format!("Unsupported incoming request: {method}"),
            ))
        })
    }

    fn notification(&self, method: String, _params: Value) {
        if method == "plugin.status" {
            self.publish_changed();
        }
    }
}

#[derive(Clone)]
pub struct NativePluginCoordinator {
    inner: Arc<CoordinatorInner>,
}

#[derive(Clone, Debug)]
pub(crate) struct PluginSessionObserver(Weak<CoordinatorInner>);

impl PluginSessionObserver {
    pub(crate) fn publish_created(&self, session: &SessionRecord) {
        if session.kind.is_user()
            && let Some(runtime) = self.runtime()
        {
            runtime.services().observe_session_created(session);
        }
    }

    fn runtime(&self) -> Option<Arc<PluginRuntime>> {
        self.0
            .upgrade()?
            .runtime
            .read()
            .expect("runtime lock")
            .clone()
    }

    pub(crate) fn publish(
        &self,
        events: &[CanonicalTurnEvent],
        before_turns: &[CanonicalTurn],
        before_inputs: &[SessionInput],
        before_latest_request: Option<&session_observation::LatestModelRequestState>,
        after_turns: &[CanonicalTurn],
    ) {
        if events.is_empty() {
            return;
        }
        let Some(runtime) = self.runtime() else {
            return;
        };
        let services = runtime.services();
        // 同一会话的待配对流记录跨事务保留，直到被助手消息领走或回合结束。
        let Some(inner) = self.0.upgrade() else {
            return;
        };
        let mut pending_streams = inner
            .pending_streams
            .lock()
            .expect("pending attempt streams lock");
        let session_id = events[0].session_id.clone();
        {
            let pending = pending_streams
                .entry((session_id.clone(), None))
                .or_default();
            pending.seed_latest_request(before_latest_request);
            for (event, observation) in session_observation::committed_observations(
                events,
                before_turns,
                before_inputs,
                after_turns,
                pending,
            ) {
                if let Some(tracker) = inner
                    .live_attempts
                    .lock()
                    .expect("live assistant attempts lock")
                    .get_mut(&event.session_id)
                {
                    tracker.on_committed_observation(&observation);
                }
                services.observe_committed_session_event(event, observation, None);
            }
        }
        for task_id in session_observation::touched_subagent_task_ids(events, after_turns) {
            let pending = pending_streams
                .entry((session_id.clone(), Some(task_id.clone())))
                .or_default();
            for (event, observation) in session_observation::committed_observations_for_scope(
                events,
                before_turns,
                before_inputs,
                after_turns,
                pending,
                session_observation::ObservationScope::Subagent {
                    task_id: task_id.as_str(),
                },
            ) {
                services.observe_committed_session_event(
                    event,
                    observation,
                    Some(task_id.as_str()),
                );
            }
        }
    }

    pub(crate) fn publish_live_assistant_stream(
        &self,
        session_id: &SessionId,
        attempt_id: &str,
        main_step: Option<(usize, usize, u64)>,
        event: magi_session_store::LiveAssistantStreamEvent,
    ) {
        let Some(runtime) = self.runtime() else {
            return;
        };
        let Some(inner) = self.0.upgrade() else {
            return;
        };
        let services = runtime.services();
        let mut live_attempts = inner
            .live_attempts
            .lock()
            .expect("live assistant attempts lock");
        let frames: Vec<Value> = match event {
            magi_session_store::LiveAssistantStreamEvent::Start => {
                let Some((turn, step, after_event_seq)) = main_step else {
                    return;
                };
                live_attempts
                    .entry(session_id.clone())
                    .or_default()
                    .on_start(attempt_id, turn, step, after_event_seq)
            }
            magi_session_store::LiveAssistantStreamEvent::Chunk { time, chunk } => {
                let Some(tracker) = live_attempts.get_mut(session_id) else {
                    return;
                };
                tracker
                    .on_chunk(attempt_id, time, chunk)
                    .into_iter()
                    .collect()
            }
            magi_session_store::LiveAssistantStreamEvent::Abandon => {
                let Some(tracker) = live_attempts.get_mut(session_id) else {
                    return;
                };
                tracker.on_abandon(attempt_id).into_iter().collect()
            }
        };
        for frame in frames {
            if let Some(Err(error)) = services.notify_assistant_stream(session_id, frame) {
                tracing::warn!(
                    %session_id,
                    attempt_id,
                    %error,
                    "插件实时助手流帧投递失败，本尝试已降级为 overflow"
                );
                if let Some(overflow) = live_attempts
                    .get_mut(session_id)
                    .and_then(|tracker| tracker.mark_delivery_failed(attempt_id))
                {
                    let _ = services.notify_assistant_stream(session_id, overflow);
                }
                break;
            }
        }
    }
}

struct CoordinatorInner {
    pending_streams:
        Mutex<HashMap<(SessionId, Option<String>), session_observation::PendingAttemptStreams>>,
    live_attempts: Mutex<HashMap<SessionId, session_observation::LiveAttemptTracker>>,
    facts_forwarder: Mutex<Option<tokio::task::JoinHandle<()>>>,
    state_root: PathBuf,
    public_origin: String,
    processes: ManagedProcessGroup,
    owner: Arc<Mutex<PluginManager>>,
    toolchain: Option<ResolvedDshToolchain>,
    runtime: RwLock<Option<Arc<PluginRuntime>>>,
    launch_error: RwLock<Option<String>>,
    ready_notify: Notify,
    launching: Mutex<bool>,
    events: RwLock<Option<Arc<InMemoryEventBus>>>,
    settings_store: RwLock<Option<Arc<SettingsStore>>>,
    event_sink: Arc<RuntimeEventSink>,
    slot_observations: RwLock<HashMap<String, ObservedClientSlots>>,
    preview_cache: Mutex<Vec<RetainedArchivePreview>>,
}

impl NativePluginCoordinator {
    pub fn open(
        state_root: &Path,
        processes: ManagedProcessGroup,
        bind_host: &str,
        port: u16,
    ) -> Result<Self, String> {
        let plugins_root = state_root.join("plugins");
        let manager =
            PluginManager::open(&plugins_root).map_err(|err| format!("加载插件状态失败: {err}"))?;
        let toolchain = resolve_dsh_toolchain(&plugins_root);
        let host_for_origin = match bind_host.trim() {
            "" | "0.0.0.0" | "::" | "[::]" => "127.0.0.1",
            other => other,
        };
        let public_origin = format!("http://{host_for_origin}:{port}");

        Ok(Self {
            inner: Arc::new(CoordinatorInner {
                pending_streams: Mutex::new(HashMap::new()),
                live_attempts: Mutex::new(HashMap::new()),
                facts_forwarder: Mutex::new(None),
                state_root: state_root.to_path_buf(),
                public_origin,
                processes,
                owner: Arc::new(Mutex::new(manager)),
                toolchain,
                runtime: RwLock::new(None),
                launch_error: RwLock::new(None),
                ready_notify: Notify::new(),
                launching: Mutex::new(false),
                events: RwLock::new(None),
                settings_store: RwLock::new(None),
                event_sink: RuntimeEventSink::new(),
                slot_observations: RwLock::new(HashMap::new()),
                preview_cache: Mutex::new(Vec::new()),
            }),
        })
    }

    pub fn manager(&self) -> Arc<Mutex<PluginManager>> {
        self.inner.owner.clone()
    }

    pub(crate) fn session_observer(&self) -> PluginSessionObserver {
        PluginSessionObserver(Arc::downgrade(&self.inner))
    }

    pub(crate) fn bind_session_history(&self, reader: SessionHistoryReader) {
        assert!(self.inner.event_sink.session_history.set(reader).is_ok());
    }

    /// 会话控制端口：插件写操作进入 daemon 服务层的唯一入口（见 `magi_api::plugin_session_control`）。
    pub(crate) fn bind_session_control(
        &self,
        control: Arc<magi_api::plugin_session_control::PluginSessionControl>,
    ) {
        // 守护进程重新装配 `ApiState`（例如恢复）时替换端口，旧端口持有的是旧状态。
        *self
            .inner
            .event_sink
            .session_control
            .write()
            .expect("session control lock") = Some(control);
    }

    /// 会话事实面：宿主读取用户会话目录的唯一来源，序号计数与变化通知共用。
    pub(crate) fn bind_session_facts(&self, store: Arc<magi_session_store::SessionStore>) {
        let facts = session_facts::SessionFacts::new(store, Arc::default());
        assert!(self.inner.event_sink.session_facts.set(facts).is_ok());
    }

    pub fn as_api_platform(&self) -> Arc<dyn NativePluginPlatform> {
        Arc::new(self.clone())
    }

    pub fn bind_settings(&self, settings: &Arc<SettingsStore>) {
        *self.inner.settings_store.write().expect("settings lock") = Some(settings.clone());
        if let Some(rt) = self.inner.runtime.read().expect("runtime lock").clone() {
            rt.services().bind_settings_store(settings.clone());
        }
    }

    pub fn start(&self, events: Arc<InMemoryEventBus>) {
        *self.inner.events.write().expect("events lock") = Some(events.clone());
        self.inner.event_sink.bind_events(events);
        let has_existing_native = self
            .inner
            .owner
            .lock()
            .ok()
            .and_then(|g| g.loading_configuration().cloned())
            .is_some_and(|cfg| !cfg.preparation.packages.is_empty() || !cfg.executables.is_empty());
        if has_existing_native {
            self.ensure_launching();
        }
    }

    fn ensure_launching(&self) -> bool {
        if self.inner.toolchain.is_none() {
            return false;
        }
        {
            let mut guard = self.inner.launching.lock().expect("launching lock");
            if *guard || self.inner.runtime.read().expect("runtime lock").is_some() {
                return true;
            }
            *guard = true;
        }
        let events = self.inner.events.read().expect("events lock").clone();
        let coordinator = self.clone();
        tokio::spawn(async move {
            if let Err(err) = coordinator.launch_inner(events).await {
                *coordinator
                    .inner
                    .launch_error
                    .write()
                    .expect("launch_error lock") = Some(err.message().to_string());
                tracing::warn!(error = %err.message(), "DSH 共享插件宿主启动失败");
            }
            *coordinator.inner.launching.lock().expect("launching lock") = false;
            coordinator.inner.ready_notify.notify_waiters();
            coordinator.inner.event_sink.publish_changed();
        });
        true
    }

    async fn launch_inner(&self, events: Option<Arc<InMemoryEventBus>>) -> Result<(), ApiError> {
        let Some(toolchain) = self.inner.toolchain.clone() else {
            return Err(ApiError::Conflict(
                "未找到随包 Node 或 DSH 运行时目录".into(),
            ));
        };
        let plugins_root = self.inner.state_root.join("plugins");
        let projects_dir = plugins_root.join("projects");
        fs::create_dir_all(&projects_dir)
            .map_err(|e| ApiError::internal_assembly("创建插件项目目录失败", e))?;
        let endpoint_dir = std::env::temp_dir().join(format!(
            "magi-dsh-{}",
            Sha256::digest(plugins_root.to_string_lossy().as_bytes())[..6]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ));
        fs::create_dir_all(&endpoint_dir)
            .map_err(|e| ApiError::internal_assembly("创建插件通信目录失败", e))?;

        let initial = ensure_initial_loading_preparation(&self.inner.owner, &projects_dir)?;
        let registry = configured_npm_registry();
        let config = PluginRuntimeConfig {
            node: toolchain.node,
            sdk_project: toolchain.sdk_project,
            endpoint_dir,
            public_origin: self.inner.public_origin.clone(),
            bundle_management: Some(PluginBundleManagementConfig {
                npm_cli: toolchain.npm_cli,
                projects_dir,
                registry,
            }),
        };

        let runtime = PluginRuntime::launch(
            self.inner.owner.clone(),
            &self.inner.processes,
            config,
            initial,
            self.inner.event_sink.clone(),
        )
        .await?;

        if let Some(bus) = events {
            runtime.services().bind_event_bus(bus.clone());
            if let Some(facts) = self.inner.event_sink.session_facts.get().cloned() {
                let services = runtime.services();
                let forwarder = facts.spawn_forwarder(bus, move |params| {
                    services.notify_session_changed(params);
                });
                // 重新启动宿主时旧转发任务随之终止，避免向已失效的宿主继续发送。
                if let Some(previous) = self
                    .inner
                    .facts_forwarder
                    .lock()
                    .expect("facts forwarder lock")
                    .replace(forwarder)
                {
                    previous.abort();
                }
            }
        }
        if let Some(settings) = self
            .inner
            .settings_store
            .read()
            .expect("settings lock")
            .clone()
        {
            runtime.services().bind_settings_store(settings);
        }

        let mut state_rx = runtime.subscribe_state();
        let mut sources = runtime.source_observations();
        *self.inner.runtime.write().expect("runtime lock") = Some(Arc::new(runtime));
        *self.inner.launch_error.write().expect("launch_error lock") = None;

        let sink = self.inner.event_sink.clone();
        tokio::spawn(async move {
            while state_rx.changed().await.is_ok() {
                sink.publish_changed();
            }
        });
        let sink = self.inner.event_sink.clone();
        tokio::spawn(async move {
            let mut previous = None;
            loop {
                let current = configuration_error(&sources.borrow_and_update()).map(str::to_owned);
                if current != previous {
                    sink.publish_changed();
                    previous = current;
                }
                if sources.changed().await.is_err() {
                    break;
                }
            }
        });
        Ok(())
    }

    async fn wait_runtime(&self) -> Result<Arc<PluginRuntime>, ApiError> {
        let mut attempted_on_demand = false;
        loop {
            if let Some(rt) = self.inner.runtime.read().expect("runtime lock").clone() {
                return Ok(rt);
            }
            let notified = self.inner.ready_notify.notified();
            let is_launching = *self.inner.launching.lock().expect("launching lock");
            if !is_launching {
                if !attempted_on_demand && self.inner.toolchain.is_some() {
                    attempted_on_demand = true;
                    if self.ensure_launching() {
                        continue;
                    }
                }
                if let Some(err) = self
                    .inner
                    .launch_error
                    .read()
                    .expect("launch_error lock")
                    .clone()
                {
                    return Err(ApiError::Conflict(format!("DSH 插件宿主启动失败：{err}")));
                }
                return Err(ApiError::Conflict(
                    "DSH 插件宿主未就绪（未检测到随包 Node 或 DSH 运行时）".into(),
                ));
            }
            notified.await;
        }
    }

    pub fn tool_catalog_provider(&self) -> PluginToolCatalogProvider {
        let inner = self.inner.clone();
        Arc::new(move || {
            let Some(rt) = inner.runtime.read().expect("runtime lock").clone() else {
                return Vec::new();
            };
            let provider = rt.services().tool_catalog_provider();
            provider()
        })
    }

    pub fn tool_executor(&self, handle: tokio::runtime::Handle) -> PluginToolExecutor {
        let inner = self.inner.clone();
        Arc::new(move |model_tool_name, arguments, context, cancelled| {
            let Some(rt) = inner.runtime.read().expect("runtime lock").clone() else {
                return (
                    json!({
                        "tool": model_tool_name,
                        "status": "failed",
                        "error_code": "plugin_host_unavailable",
                        "error": "DSH 插件宿主未运行",
                    })
                    .to_string(),
                    magi_core::ExecutionResultStatus::Failed,
                );
            };
            let exec = rt.services().tool_executor(handle.clone());
            exec(model_tool_name, arguments, context, cancelled)
        })
    }

    pub fn web_executor(&self, handle: tokio::runtime::Handle) -> WebProviderExecutor {
        let inner = self.inner.clone();
        Arc::new(move |tool, input, context, cancelled| {
            let rt = inner.runtime.read().expect("runtime lock").clone()?;
            let exec = rt.services().web_executor(handle.clone());
            exec(tool, input, context, cancelled)
        })
    }

    pub fn command_executor(
        &self,
        handle: tokio::runtime::Handle,
    ) -> Arc<dyn magi_conversation_runtime::plugin_command::PluginCommandExecutor> {
        struct DynamicCommandExecutor {
            inner: Arc<CoordinatorInner>,
            handle: tokio::runtime::Handle,
        }
        impl magi_conversation_runtime::plugin_command::PluginCommandExecutor for DynamicCommandExecutor {
            fn invoke(
                &self,
                invocation: magi_conversation_runtime::plugin_command::PluginCommandInvocation,
                cancelled: &std::sync::atomic::AtomicBool,
            ) -> Result<magi_conversation_runtime::plugin_command::PluginCommandResult, String>
            {
                let rt = self
                    .inner
                    .runtime
                    .read()
                    .expect("runtime lock")
                    .clone()
                    .ok_or_else(|| "DSH 插件宿主未运行".to_string())?;
                rt.services()
                    .command_executor(self.handle.clone())
                    .invoke(invocation, cancelled)
            }
        }
        Arc::new(DynamicCommandExecutor {
            inner: self.inner.clone(),
            handle,
        })
    }

    pub fn extension_hooks(
        &self,
        handle: tokio::runtime::Handle,
    ) -> Arc<dyn magi_conversation_runtime::extension_hooks::ExtensionHooks> {
        use magi_bridge_client::{ChatMessage, ChatToolCall};
        use magi_conversation_runtime::extension_hooks::{
            ApprovalHookDecision, ApprovalHookRequest, CompactionDecision, CompactionView,
            ExtensionHooks, LlmRequestExtendView, LlmRequestExtension, SystemPromptOverride,
            ToolCallDecision, ToolHookScope, ToolResultView,
        };
        struct DynamicExtensionHooks {
            inner: Arc<CoordinatorInner>,
            handle: tokio::runtime::Handle,
        }
        impl DynamicExtensionHooks {
            fn delegate(&self) -> Option<Arc<dyn ExtensionHooks>> {
                let rt = self.inner.runtime.read().expect("runtime lock").clone()?;
                Some(rt.services().extension_hooks(self.handle.clone()))
            }
        }
        impl ExtensionHooks for DynamicExtensionHooks {
            fn compaction_before(&self, view: &CompactionView<'_>) -> CompactionDecision {
                self.delegate()
                    .map_or(CompactionDecision::Proceed, |h| h.compaction_before(view))
            }

            fn system_prompt_assemble(
                &self,
                scope: &ToolHookScope<'_>,
                base_prompt: &str,
            ) -> Option<SystemPromptOverride> {
                self.delegate()
                    .and_then(|h| h.system_prompt_assemble(scope, base_prompt))
            }

            fn approval_decide(
                &self,
                scope: &ToolHookScope<'_>,
                request: &ApprovalHookRequest<'_>,
            ) -> ApprovalHookDecision {
                self.delegate().map_or(ApprovalHookDecision::Defer, |h| {
                    h.approval_decide(scope, request)
                })
            }

            fn filter_visible_tools(
                &self,
                scope: &ToolHookScope<'_>,
                names: Vec<String>,
            ) -> Vec<String> {
                self.delegate()
                    .map_or(names.clone(), |h| h.filter_visible_tools(scope, names))
            }

            fn project_request_messages(
                &self,
                scope: &ToolHookScope<'_>,
                messages: &[ChatMessage],
            ) -> Option<Vec<ChatMessage>> {
                self.delegate()
                    .and_then(|h| h.project_request_messages(scope, messages))
            }

            fn extend_llm_request(
                &self,
                scope: &ToolHookScope<'_>,
                view: &LlmRequestExtendView<'_>,
            ) -> Option<LlmRequestExtension> {
                self.delegate()
                    .and_then(|h| h.extend_llm_request(scope, view))
            }

            fn intercept_llm_stream(
                &self,
                scope: &ToolHookScope<'_>,
                view: &magi_conversation_runtime::extension_hooks::LlmStreamInterceptView<'_>,
            ) -> magi_conversation_runtime::extension_hooks::LlmStreamInterceptDecision
            {
                self.delegate().map_or(
                    magi_conversation_runtime::extension_hooks::LlmStreamInterceptDecision::Accept,
                    |h| h.intercept_llm_stream(scope, view),
                )
            }

            fn has_selected_plugin_agent_loop(&self) -> bool {
                self.delegate()
                    .is_some_and(|h| h.has_selected_plugin_agent_loop())
            }

            fn dispatch_agent_loop(
                &self,
                request: &magi_conversation_runtime::extension_hooks::PluginAgentLoopRequest<'_>,
                cancelled: &std::sync::atomic::AtomicBool,
            ) -> magi_conversation_runtime::extension_hooks::PluginAgentLoopOutcome {
                self.delegate().map_or(
                    magi_conversation_runtime::extension_hooks::PluginAgentLoopOutcome::Native,
                    |h| h.dispatch_agent_loop(request, cancelled),
                )
            }

            fn tool_call(
                &self,
                scope: &ToolHookScope<'_>,
                call: &ChatToolCall,
            ) -> ToolCallDecision {
                self.delegate().map_or(
                    ToolCallDecision::Proceed {
                        arguments: None,
                        rewritten_by: Vec::new(),
                    },
                    |h| h.tool_call(scope, call),
                )
            }

            fn tool_result(
                &self,
                scope: &ToolHookScope<'_>,
                call: &ChatToolCall,
                view: ToolResultView<'_>,
            ) -> Option<String> {
                self.delegate()
                    .and_then(|h| h.tool_result(scope, call, view))
            }
        }
        Arc::new(DynamicExtensionHooks {
            inner: self.inner.clone(),
            handle,
        })
    }
}

fn configured_npm_registry() -> String {
    std::env::var("MAGI_NPM_REGISTRY")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_NPM_REGISTRY.to_string())
}

fn ensure_initial_loading_preparation(
    owner: &Arc<Mutex<PluginManager>>,
    projects_dir: &Path,
) -> Result<LoadingPreparation, ApiError> {
    let guard = owner
        .lock()
        .map_err(|_| ApiError::internal_assembly("读取插件加载配置失败", "插件锁已损坏"))?;
    if let Some(existing) = guard.loading_configuration() {
        return Ok(existing.preparation.clone());
    }
    let source_root = guard.loading_source_root();
    drop(guard);

    fs::create_dir_all(&source_root)
        .map_err(|e| ApiError::internal_assembly("初始化插件配置目录失败", e))?;
    for (file, default_content) in [
        ("cordis.yml", "[]\n"),
        ("bundle.patch.yml", "[]\n"),
        ("cordis.patch.yml", "[]\n"),
        (
            "package.json",
            "{\n  \"name\": \"magi-dsh-profile\",\n  \"private\": true,\n  \"dependencies\": {}\n}\n",
        ),
    ] {
        let target = source_root.join(file);
        if !target.exists() {
            write_atomic(&target, default_content.as_bytes())
                .map_err(|e| ApiError::internal_assembly("写入初始插件配置失败", e))?;
        }
    }

    let bootstrap_project = projects_dir.join("bootstrap");
    fs::create_dir_all(bootstrap_project.join("node_modules"))
        .map_err(|e| ApiError::internal_assembly("创建初始插件项目失败", e))?;
    let pkg_json = bootstrap_project.join("package.json");
    if !pkg_json.exists() {
        write_atomic(
            &pkg_json,
            b"{\n  \"name\": \"magi-native-plugin-project\",\n  \"private\": true,\n  \"dependencies\": {}\n}\n",
        )
        .map_err(|e| ApiError::internal_assembly("写入初始项目清单失败", e))?;
    }
    let canonical_project = fs::canonicalize(&bootstrap_project)
        .map_err(|e| ApiError::internal_assembly("规范化初始项目目录失败", e))?;

    Ok(LoadingPreparation {
        project_directory: canonical_project,
        base: "cordis.yml".into(),
        overlays: vec!["bundle.patch.yml".into(), "cordis.patch.yml".into()],
        packages: Vec::new(),
    })
}

fn resolve_dsh_toolchain(plugins_root: &Path) -> Option<ResolvedDshToolchain> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let node_bin_name = if cfg!(windows) { "node.exe" } else { "node" };

    // 1. Packaged Electron sidecar layout: <exe_dir>/{node/node, node/npm/bin/npm-cli.js, dsh}
    if let Some(dir) = &exe_dir {
        let node = dir.join("node").join(node_bin_name);
        let npm_cli = dir.join("node/npm/bin/npm-cli.js");
        let dsh = dir.join("dsh");
        if node.is_file()
            && npm_cli.is_file()
            && dsh.join("runtime/entry.mjs").is_file()
            && let (Ok(node), Ok(npm_cli), Ok(sdk_project)) = (
                fs::canonicalize(node),
                fs::canonicalize(npm_cli),
                fs::canonicalize(dsh),
            )
        {
            return Some(ResolvedDshToolchain {
                node,
                npm_cli,
                sdk_project,
            });
        }
    }

    // 2. Development workspace / environment overrides.
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let candidate_nodes = [
        std::env::var_os("MAGI_EXTENSION_NODE").map(PathBuf::from),
        Some(
            repo_root
                .join("apps/desktop/dist/node-runtime")
                .join(node_bin_name),
        ),
        Some(
            repo_root
                .join("target/electron-dist/mac-arm64/Magi.app/Contents/Resources/daemon/node")
                .join(node_bin_name),
        ),
        magi_process::resolve_executable("node"),
    ];
    let node = candidate_nodes
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
        .and_then(|p| fs::canonicalize(p).ok())?;

    let candidate_npms = [
        std::env::var_os("MAGI_DSH_NPM_CLI").map(PathBuf::from),
        node.parent().map(|p| p.join("npm/bin/npm-cli.js")),
        Some(repo_root.join("apps/desktop/dist/node-runtime/npm/bin/npm-cli.js")),
        Some(
            repo_root.join(
                "target/electron-dist/mac-arm64/Magi.app/Contents/Resources/daemon/node/npm/bin/npm-cli.js",
            ),
        ),
        magi_process::resolve_executable("npm"),
    ];
    let npm_cli = candidate_npms
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
        .and_then(|p| fs::canonicalize(p).ok())?;

    if let Some(env_sdk) = std::env::var_os("MAGI_DSH_SDK_PROJECT").map(PathBuf::from)
        && env_sdk.join("runtime/entry.mjs").is_file()
        && let Ok(canon) = fs::canonicalize(env_sdk)
    {
        return Some(ResolvedDshToolchain {
            node,
            npm_cli,
            sdk_project: canon,
        });
    }

    let source_dsh = repo_root.join("crates/magi-extension-host/runtime/dsh");
    if source_dsh.join("entry.mjs").is_file()
        && source_dsh.join("node_modules/@deepseek-ai/cordis").is_dir()
        && let Ok(canon_src) = fs::canonicalize(&source_dsh)
        && let Some(staged_dev) = stage_dev_sdk_view(plugins_root, &canon_src)
    {
        return Some(ResolvedDshToolchain {
            node,
            npm_cli,
            sdk_project: staged_dev,
        });
    }

    let dist_dsh = repo_root.join("apps/desktop/dist/dsh");
    if dist_dsh.join("runtime/entry.mjs").is_file()
        && let Ok(canon) = fs::canonicalize(dist_dsh)
    {
        return Some(ResolvedDshToolchain {
            node,
            npm_cli,
            sdk_project: canon,
        });
    }

    None
}

fn stage_dev_sdk_view(plugins_root: &Path, source_dsh: &Path) -> Option<PathBuf> {
    let view_dir = plugins_root.join("dsh-sdk-dev");
    let runtime_dir = view_dir.join("runtime");
    fs::create_dir_all(&runtime_dir).ok()?;
    for entry in fs::read_dir(source_dsh).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("mjs") {
            let _ = fs::copy(&path, runtime_dir.join(entry.file_name()));
        }
    }
    if source_dsh.join("dist").is_dir() {
        let target_dist = runtime_dir.join("dist");
        let _ = fs::create_dir_all(&target_dist);
        for file in ["dsh-client-host.js", "dsh-client-host.css"] {
            let src = source_dsh.join("dist").join(file);
            if src.is_file() {
                let _ = fs::copy(&src, target_dist.join(file));
            }
        }
    }
    let _ = fs::copy(
        source_dsh.join("package.json"),
        runtime_dir.join("package.json"),
    );
    let _ = fs::copy(
        source_dsh.join("package.json"),
        view_dir.join("package.json"),
    );
    let _ = fs::copy(
        source_dsh.join("package-lock.json"),
        view_dir.join("package-lock.json"),
    );
    let nm_link = view_dir.join("node_modules");
    if !nm_link.exists() {
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(source_dsh.join("node_modules"), &nm_link);
        #[cfg(windows)]
        let _ = std::os::windows::fs::symlink_dir(source_dsh.join("node_modules"), &nm_link);
    }
    fs::canonicalize(view_dir).ok()
}

fn read_original_manifest(package_dir: &Path) -> Option<Value> {
    let bytes = fs::read(package_dir.join("package.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn resolve_display_title(pkg_name: &str, manifest: &Value) -> String {
    if let Some(title) = manifest
        .pointer("/dsh/title")
        .or_else(|| manifest.get("displayName"))
        .or_else(|| manifest.get("title"))
    {
        if let Some(s) = title.as_str()
            && !s.trim().is_empty()
        {
            return s.trim().to_string();
        }
        if let Some(obj) = title.as_object() {
            for key in ["zh-CN", "zh", "en-US", "en"] {
                if let Some(s) = obj.get(key).and_then(Value::as_str)
                    && !s.trim().is_empty()
                {
                    return s.trim().to_string();
                }
            }
        }
    }
    pkg_name.to_string()
}

fn resolve_description(manifest: &Value) -> String {
    manifest
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn package_is_enabled_in_configuration(
    cfg: &magi_plugin_system::LoadingConfiguration,
    pkg: &magi_extension_host::shared::PackageInspection,
    orig_manifest: &Value,
) -> bool {
    let pkg_dir = &pkg.directory;
    let has_executable = cfg
        .executables
        .iter()
        .any(|exec| exec.plugin_id == pkg.name || exec.entry.starts_with(pkg_dir));
    if has_executable {
        return true;
    }
    let has_server_entry = orig_manifest.get("main").is_some()
        || orig_manifest
            .get("exports")
            .and_then(|e| if e.is_string() { Some(e) } else { e.get(".") })
            .is_some();
    if has_server_entry {
        return false;
    }
    cfg.sources
        .iter()
        .find(|s| s.name() == "bundle.patch.yml")
        .is_some_and(|s| s.content().contains(&pkg.name))
}

fn resolve_client_entry_file(package_dir: &Path, manifest: &Value) -> Option<PathBuf> {
    let exports = manifest.get("exports")?.get("./client")?;
    fn pick_target(val: &Value) -> Option<&str> {
        if let Some(s) = val.as_str() {
            return Some(s);
        }
        let obj = val.as_object()?;
        for key in ["browser", "default", "import", "module"] {
            if let Some(found) = obj.get(key).and_then(pick_target) {
                return Some(found);
            }
        }
        None
    }
    let rel = pick_target(exports)?.trim();
    let stripped = rel.strip_prefix("./").unwrap_or(rel);
    if stripped.is_empty() || Path::new(stripped).is_absolute() {
        return None;
    }
    for comp in Path::new(stripped).components() {
        if !matches!(comp, Component::Normal(_)) {
            return None;
        }
    }
    let canon_root = fs::canonicalize(package_dir).ok()?;
    let canon_file = fs::canonicalize(canon_root.join(stripped)).ok()?;
    (canon_file.starts_with(&canon_root) && canon_file.is_file()).then_some(canon_file)
}

fn resolve_icon_file(package_dir: &Path, manifest: &Value) -> Option<(&'static str, Vec<u8>)> {
    let mut candidates = Vec::new();
    if let Some(icon) = manifest
        .pointer("/dsh/icon")
        .or_else(|| manifest.get("icon"))
        .and_then(Value::as_str)
    {
        candidates.push(icon.trim_start_matches("./").to_string());
    }
    for fallback in [
        "icon.svg",
        "icon.png",
        "assets/icon.svg",
        "assets/icon.png",
        "logo.svg",
        "logo.png",
    ] {
        candidates.push(fallback.to_string());
    }
    let canon_root = fs::canonicalize(package_dir).ok()?;
    for rel in candidates {
        if rel.is_empty() || Path::new(&rel).is_absolute() {
            continue;
        }
        if Path::new(&rel)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            continue;
        }
        let Ok(file) = fs::canonicalize(canon_root.join(&rel)) else {
            continue;
        };
        if !file.starts_with(&canon_root) || !file.is_file() {
            continue;
        }
        let Some(mime) = magi_media::image_content_type(&rel) else {
            continue;
        };
        let Ok(bytes) = fs::read(&file) else {
            continue;
        };
        if bytes.len() <= 2 * 1024 * 1024 {
            return Some((mime, bytes));
        }
    }
    None
}

fn parse_partial_semver(raw: &str) -> Option<(semver::Version, usize)> {
    let trimmed = raw.trim().strip_prefix('v').unwrap_or(raw.trim());
    if let Ok(full) = semver::Version::parse(trimmed) {
        return Some((full, 3));
    }
    let parts: Vec<&str> = trimmed.split('.').collect();
    match parts.as_slice() {
        [maj] if !maj.is_empty() && *maj != "*" && *maj != "x" && *maj != "X" => {
            let m = maj.parse::<u64>().ok()?;
            Some((semver::Version::new(m, 0, 0), 1))
        }
        [maj, min] => {
            let m = maj.parse::<u64>().ok()?;
            if *min == "*" || *min == "x" || *min == "X" {
                Some((semver::Version::new(m, 0, 0), 1))
            } else {
                let n = min.parse::<u64>().ok()?;
                Some((semver::Version::new(m, n, 0), 2))
            }
        }
        [maj, min, pat] if *pat == "*" || *pat == "x" || *pat == "X" => {
            let m = maj.parse::<u64>().ok()?;
            let n = min.parse::<u64>().ok()?;
            Some((semver::Version::new(m, n, 0), 2))
        }
        _ => None,
    }
}

fn zero_prerelease(major: u64, minor: u64, patch: u64) -> semver::Version {
    semver::Version {
        major,
        minor,
        patch,
        pre: semver::Prerelease::new("0").expect("valid zero prerelease"),
        build: semver::BuildMetadata::EMPTY,
    }
}

fn matches_npm_comparator_include_prerelease(comp: &str, v: &semver::Version) -> bool {
    let c = comp.trim();
    if c.is_empty() || c == "*" || c == "x" || c == "X" {
        return true;
    }
    if let Some(rest) = c.strip_prefix(">=") {
        let Some((bound, _)) = parse_partial_semver(rest) else {
            return false;
        };
        return v >= &bound;
    }
    if let Some(rest) = c.strip_prefix("<=") {
        let Some((bound, arity)) = parse_partial_semver(rest) else {
            return false;
        };
        return match arity {
            1 => v < &zero_prerelease(bound.major + 1, 0, 0),
            2 => v < &zero_prerelease(bound.major, bound.minor + 1, 0),
            _ => v <= &bound,
        };
    }
    if let Some(rest) = c.strip_prefix('>') {
        let Some((bound, arity)) = parse_partial_semver(rest) else {
            return false;
        };
        return match arity {
            1 => v >= &semver::Version::new(bound.major + 1, 0, 0),
            2 => v >= &semver::Version::new(bound.major, bound.minor + 1, 0),
            _ => v > &bound,
        };
    }
    if let Some(rest) = c.strip_prefix('<') {
        let Some((mut bound, _)) = parse_partial_semver(rest) else {
            return false;
        };
        if bound.pre.is_empty() {
            bound = zero_prerelease(bound.major, bound.minor, bound.patch);
        }
        return v < &bound;
    }
    if let Some(rest) = c.strip_prefix('^') {
        let Some((lower, arity)) = parse_partial_semver(rest) else {
            return false;
        };
        if v < &lower {
            return false;
        }
        let upper = if lower.major > 0 || arity == 1 {
            zero_prerelease(lower.major + 1, 0, 0)
        } else if lower.minor > 0 || arity == 2 {
            zero_prerelease(0, lower.minor + 1, 0)
        } else {
            zero_prerelease(0, 0, lower.patch + 1)
        };
        return v < &upper;
    }
    if let Some(rest) = c.strip_prefix('~') {
        let Some((lower, arity)) = parse_partial_semver(rest) else {
            return false;
        };
        if v < &lower {
            return false;
        }
        let upper = if arity == 1 {
            zero_prerelease(lower.major + 1, 0, 0)
        } else {
            zero_prerelease(lower.major, lower.minor + 1, 0)
        };
        return v < &upper;
    }
    let exact_str = c.strip_prefix('=').unwrap_or(c);
    let Some((bound, arity)) = parse_partial_semver(exact_str) else {
        return false;
    };
    match arity {
        1 => v >= &bound && v < &zero_prerelease(bound.major + 1, 0, 0),
        2 => v >= &bound && v < &zero_prerelease(bound.major, bound.minor + 1, 0),
        _ => v == &bound,
    }
}

fn check_static_dsh_compatibility(manifest: &Value) -> Result<(), String> {
    let baseline = semver::Version::parse(LOCKED_DSH_VERSION).expect("valid baseline semver");
    let satisfies = |range_str: &str| -> bool {
        let trimmed = range_str.trim();
        if trimmed.is_empty() {
            return false;
        }
        if matches!(trimmed, "workspace:^" | "workspace:~" | "workspace:*" | "*") {
            return true;
        }
        for branch in trimmed.split("||") {
            let part = branch.trim();
            if part.is_empty() {
                continue;
            }
            // Normalize operator whitespace (e.g. `>= 1.0.0` -> `>=1.0.0`) and hyphen ranges (`A - B` -> `>=A <=B`)
            let collapsed = part
                .replace(">= ", ">=")
                .replace("<= ", "<=")
                .replace("> ", ">")
                .replace("< ", "<")
                .replace("^ ", "^")
                .replace("~ ", "~")
                .replace("= ", "=");
            let expanded = if let Some((lhs, rhs)) = collapsed.split_once(" - ") {
                format!(">={} <={}", lhs.trim(), rhs.trim())
            } else {
                collapsed.replace(',', " ")
            };
            let tokens: Vec<&str> = expanded.split_whitespace().collect();
            if !tokens.is_empty()
                && tokens
                    .iter()
                    .all(|tok| matches_npm_comparator_include_prerelease(tok, &baseline))
            {
                return true;
            }
        }
        false
    };

    if let Some(dsh_range) = manifest
        .pointer("/engines/dsh")
        .or_else(|| manifest.pointer("/dsh/engines/dsh"))
        .and_then(Value::as_str)
    {
        if !satisfies(dsh_range) {
            return Err(format!(
                "插件要求 engines.dsh = \"{dsh_range}\"，与当前锁定的 DSH 基线 {LOCKED_DSH_VERSION} 不兼容"
            ));
        }
        return Ok(());
    }

    if let Some(peers) = manifest.get("peerDependencies").and_then(Value::as_object) {
        let mut unmet = BTreeMap::new();
        for (dep, range_val) in peers {
            if dep != "@deepseek-ai/dsh" && !dep.starts_with("@deepseek-ai/dsh-") {
                continue;
            }
            if let Some(range) = range_val.as_str()
                && !satisfies(range)
            {
                unmet.insert(dep.clone(), range.to_string());
            }
        }
        if !unmet.is_empty() {
            return Err(format!(
                "插件声明的 DSH peerDependencies {:?} 与当前锁定的 DSH 基线 {LOCKED_DSH_VERSION} 不兼容",
                unmet
            ));
        }
    }
    Ok(())
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn build_synthetic_manifest(
    pkg_name: &str,
    version: &str,
    orig_manifest: &Value,
    has_icon: bool,
    client_declared: bool,
    observed: Option<&ObservedClientSlots>,
) -> PluginManifest {
    let title = resolve_display_title(pkg_name, orig_manifest);
    let description = resolve_description(orig_manifest);
    let mut views = Vec::new();
    let show_settings =
        client_declared && observed.and_then(|o| o.has_settings_view).unwrap_or(true);
    if show_settings {
        views.push(PluginViewContribution {
            id: "settings".to_string(),
            title: title.clone(),
            entry: "ui/index.html".to_string(),
            placements: vec![PluginViewPlacement::Settings],
            priority: None,
        });
    }
    if client_declared && let Some(obs) = observed {
        for key in &obs.main_keys {
            views.push(PluginViewContribution {
                id: format!("main:{key}"),
                title: title.clone(),
                entry: "ui/index.html".to_string(),
                placements: vec![PluginViewPlacement::Main],
                priority: None,
            });
        }
        for tab in &obs.right_pane_tabs {
            views.push(PluginViewContribution {
                id: tab.key.clone(),
                title: tab.title.clone(),
                entry: "ui/index.html".to_string(),
                placements: vec![PluginViewPlacement::RightPane],
                priority: tab.launcher_order,
            });
        }
    }

    PluginManifest {
        sdk_version: 2,
        id: pkg_name.to_string(),
        name: title,
        version: version.to_string(),
        description,
        icon: has_icon.then(|| "icon".to_string()),
        data_schema_version: 1,
        permissions: vec![PluginPermission {
            kind: PluginPermissionKind::Extension,
            targets: vec!["full-trust".to_string()],
        }],
        settings_schema: json!({ "type": "object" }),
        contributions: PluginContributions {
            views,
            commands: Vec::new(),
            tools: Vec::new(),
            skills: Vec::new(),
            model_sources: Vec::new(),
            resources: Vec::new(),
            browser_surfaces: Vec::new(),
            client: client_declared.then(|| PluginClientContribution {
                entry: "ui/index.html".to_string(),
            }),
        },
        extension: Some(PluginExtension {
            hooks: Vec::new(),
            compat: None,
            runtime_contributions: Vec::new(),
        }),
    }
}

impl NativePluginPlatform for NativePluginCoordinator {
    fn configuration_error(&self) -> Option<String> {
        let runtime = self.inner.runtime.read().expect("runtime lock").clone()?;
        configuration_error(&runtime.source_observations().borrow()).map(str::to_owned)
    }

    fn list_installed(&self, manager: &PluginManager) -> Result<Vec<PluginInstalled>, ApiError> {
        let mut out = Vec::new();
        let mut native_ids = std::collections::HashSet::new();

        let rt_ref = self.inner.runtime.read().expect("runtime lock").clone();
        let sup_state = rt_ref.as_ref().map(|rt| rt.supervisor_state());
        let live_tools = rt_ref
            .as_ref()
            .map(|rt| rt.services().tools_snapshot())
            .unwrap_or_default();
        let live_commands = rt_ref
            .as_ref()
            .map(|rt| rt.services().commands_snapshot())
            .unwrap_or_default();
        let live_llm = rt_ref
            .as_ref()
            .map(|rt| rt.services().llm_providers_snapshot())
            .unwrap_or_default();
        let launch_err = self
            .inner
            .launch_error
            .read()
            .expect("launch_error lock")
            .clone();
        let observations = self
            .inner
            .slot_observations
            .read()
            .expect("slot_observations lock")
            .clone();

        if let Some(cfg) = manager.loading_configuration() {
            let active_host_gen = match &sup_state {
                Some(SharedHostSupervisorState::Running { host_generation }) => {
                    Some(*host_generation)
                }
                _ => None,
            };
            let instances: Vec<_> = active_host_gen
                .map(|gen_id| {
                    cfg.executables
                        .iter()
                        .filter_map(|exec| manager.loading_instance_status(&exec.instance_id))
                        .filter(|(act, _)| act.host_generation == gen_id)
                        .collect()
                })
                .unwrap_or_default();

            for pkg in &cfg.preparation.packages {
                native_ids.insert(pkg.name.clone());
                let orig_manifest =
                    read_original_manifest(&pkg.directory).unwrap_or_else(|| json!({}));
                let client_declared =
                    resolve_client_entry_file(&pkg.directory, &orig_manifest).is_some();
                let has_icon = resolve_icon_file(&pkg.directory, &orig_manifest).is_some();
                let enabled = package_is_enabled_in_configuration(cfg, pkg, &orig_manifest);
                let obs = observations.get(&pkg.name);
                let mut manifest = build_synthetic_manifest(
                    &pkg.name,
                    &pkg.version,
                    &orig_manifest,
                    has_icon,
                    client_declared,
                    obs,
                );

                if enabled {
                    let pkg_prefix = format!("{}/", pkg.directory.display());
                    let owned_ids: std::collections::HashSet<&str> = cfg
                        .executables
                        .iter()
                        .filter(|e| {
                            e.plugin_id == pkg.name
                                || e.entry.to_string_lossy().starts_with(&pkg_prefix)
                        })
                        .flat_map(|e| [e.plugin_id.as_str(), e.instance_id.as_str()])
                        .collect();
                    let matches_owner = |pid: Option<&str>, iid: Option<&str>| -> bool {
                        pid.is_some_and(|p| p == pkg.name || owned_ids.contains(p))
                            || iid.is_some_and(|i| owned_ids.contains(i))
                    };
                    manifest.contributions.tools = live_tools
                        .iter()
                        .filter(|t| matches_owner(t.plugin_id.as_deref(), t.instance_id.as_deref()))
                        .map(|t| PluginToolContribution {
                            id: t.name.clone(),
                            title: t.name.clone(),
                            description: t.description.clone(),
                            input_schema: if t.parameters.is_object() {
                                t.parameters.clone()
                            } else {
                                json!({ "type": "object", "properties": {} })
                            },
                            read_only: false,
                            timeout_ms: t.timeout_ms.map(|v| v as i64),
                        })
                        .collect();
                    manifest.contributions.commands = live_commands
                        .iter()
                        .filter(|c| matches_owner(c.plugin_id.as_deref(), c.instance_id.as_deref()))
                        .map(|c| PluginNamedContribution {
                            id: c.name.clone(),
                            title: c.name.clone(),
                            description: c.description.clone().unwrap_or_default(),
                        })
                        .collect();
                    manifest.contributions.model_sources = live_llm
                        .iter()
                        .filter(|(pid, _, _, _)| {
                            pid == &pkg.name || owned_ids.contains(pid.as_str())
                        })
                        .map(|(_, prov_id, display_name, _)| PluginNamedContribution {
                            id: prov_id.clone(),
                            title: display_name.clone(),
                            description: String::new(),
                        })
                        .collect();
                }

                let instance = instances
                    .iter()
                    .copied()
                    .filter(|(activation, _)| {
                        activation.executable.plugin_id == pkg.name
                            || activation.executable.entry.starts_with(&pkg.directory)
                    })
                    .min_by_key(|(_, status)| match status {
                        LoadingInstanceStatus::Running => 0,
                        LoadingInstanceStatus::Failed(_) => 1,
                        LoadingInstanceStatus::Waiting(_) => 2,
                        LoadingInstanceStatus::Starting => 3,
                        LoadingInstanceStatus::Stopping | LoadingInstanceStatus::Stopped => 4,
                    });

                let has_settings_view = if !client_declared {
                    Some(false)
                } else {
                    obs.and_then(|o| o.has_settings_view)
                };
                let unmounted_slots = obs.map(|o| o.unmounted_slots.clone()).unwrap_or_default();
                let mounted_slots = obs.map(|o| o.mounted_slots.clone()).unwrap_or_default();

                let runtime_status = if !enabled {
                    PluginRuntimeStatus {
                        state: PluginRuntimeState::Stopped,
                        reason: None,
                        generation: active_host_gen.unwrap_or(0),
                        crashes: 0,
                        blocking: Vec::new(),
                        instance_id: None,
                        unmounted_slots,
                        mounted_slots,
                        has_settings_view,
                    }
                } else if let Some(err) = &launch_err {
                    PluginRuntimeStatus {
                        state: PluginRuntimeState::Failed,
                        reason: Some(err.clone()),
                        generation: 0,
                        crashes: 0,
                        blocking: Vec::new(),
                        instance_id: None,
                        unmounted_slots,
                        mounted_slots,
                        has_settings_view,
                    }
                } else {
                    match &sup_state {
                        None | Some(SharedHostSupervisorState::Starting) => PluginRuntimeStatus {
                            state: PluginRuntimeState::Starting,
                            reason: None,
                            generation: 0,
                            crashes: 0,
                            blocking: Vec::new(),
                            instance_id: None,
                            unmounted_slots,
                            mounted_slots,
                            has_settings_view,
                        },
                        Some(SharedHostSupervisorState::Running { host_generation }) => {
                            let application_failure = match manager.loading_application() {
                                Some(LoadingApplication::Failed(token, reason))
                                    if token.revision == cfg.revision
                                        && token.host_generation == *host_generation =>
                                {
                                    Some(reason)
                                }
                                _ => None,
                            };
                            let (state, reason) = match instance.map(|(_, status)| status) {
                                Some(LoadingInstanceStatus::Running) => {
                                    (PluginRuntimeState::Running, None)
                                }
                                Some(LoadingInstanceStatus::Waiting(missing)) => (
                                    PluginRuntimeState::Waiting,
                                    (!missing.is_empty())
                                        .then(|| format!("等待依赖服务：{}", missing.join("、"))),
                                ),
                                Some(LoadingInstanceStatus::Failed(message)) => (
                                    PluginRuntimeState::Failed,
                                    message.clone().or_else(|| Some("插件激活失败".into())),
                                ),
                                Some(
                                    LoadingInstanceStatus::Stopping
                                    | LoadingInstanceStatus::Stopped,
                                ) => (PluginRuntimeState::Stopped, None),
                                Some(LoadingInstanceStatus::Starting) | None
                                    if application_failure.is_some() =>
                                {
                                    (PluginRuntimeState::Failed, application_failure.cloned())
                                }
                                Some(LoadingInstanceStatus::Starting) | None => {
                                    (PluginRuntimeState::Starting, None)
                                }
                            };
                            PluginRuntimeStatus {
                                state,
                                reason,
                                generation: *host_generation,
                                crashes: 0,
                                blocking: Vec::new(),
                                instance_id: instance
                                    .map(|(activation, _)| page_instance_id(activation)),
                                unmounted_slots,
                                mounted_slots,
                                has_settings_view,
                            }
                        }
                        Some(SharedHostSupervisorState::Backoff {
                            crashes_in_window,
                            reason,
                            ..
                        }) => PluginRuntimeStatus {
                            state: PluginRuntimeState::Restarting,
                            reason: Some(reason.clone()),
                            generation: 0,
                            crashes: *crashes_in_window as u64,
                            blocking: Vec::new(),
                            instance_id: None,
                            unmounted_slots,
                            mounted_slots,
                            has_settings_view,
                        },
                        Some(SharedHostSupervisorState::StoppedByCrashLimit {
                            crashes_in_window,
                            reason,
                            ..
                        }) => PluginRuntimeStatus {
                            state: PluginRuntimeState::Stopped,
                            reason: Some(format!("插件宿主已停止：{reason}")),
                            generation: 0,
                            crashes: *crashes_in_window as u64,
                            blocking: Vec::new(),
                            instance_id: None,
                            unmounted_slots,
                            mounted_slots,
                            has_settings_view,
                        },
                        Some(SharedHostSupervisorState::Stopped) => PluginRuntimeStatus {
                            state: PluginRuntimeState::Stopped,
                            reason: Some("插件宿主已停止".into()),
                            generation: 0,
                            crashes: 0,
                            blocking: Vec::new(),
                            instance_id: None,
                            unmounted_slots,
                            mounted_slots,
                            has_settings_view,
                        },
                    }
                };

                if runtime_status.state != PluginRuntimeState::Running
                    || runtime_status.instance_id.as_ref()
                        != obs.and_then(|o| o.instance_id.as_ref())
                {
                    manifest
                        .contributions
                        .views
                        .retain(|view| !view.placements.contains(&PluginViewPlacement::RightPane));
                }
                out.push(PluginInstalled {
                    manifest,
                    digest: pkg.manifest_sha256.clone(),
                    enabled,
                    source: PluginSourceProjection {
                        kind: PluginSourceKind::Upstream,
                        location: format!("dsh:{}@{}", pkg.name, pkg.version),
                        github: None,
                    },
                    upstream_manifest: Some(orig_manifest),
                    upstream_archive_path: None,
                    disabled_by: None,
                    reinstall_required: false,
                    runtime: Some(runtime_status),
                });
            }
        }

        // Hard cutover: surface legacy SDK 2 / translated installations
        // as `reinstallRequired = true` without attempting to load or start them.
        if let Ok(legacy_list) = manager.projection() {
            for mut legacy in legacy_list.plugins {
                if native_ids.contains(&legacy.manifest.id) {
                    continue;
                }
                legacy.enabled = false;
                legacy.reinstall_required = true;
                legacy.runtime = Some(PluginRuntimeStatus {
                    state: PluginRuntimeState::Stopped,
                    reason: Some("插件平台已切换至原生 DSH 宿主，请重新安装原包".into()),
                    generation: 0,
                    crashes: 0,
                    blocking: Vec::new(),
                    instance_id: None,
                    unmounted_slots: Vec::new(),
                    mounted_slots: Vec::new(),
                    has_settings_view: Some(false),
                });
                out.push(legacy);
            }
        }

        Ok(out)
    }

    fn installed_upstream(
        &self,
        manager: &PluginManager,
    ) -> Result<InstalledUpstreamPlugins, ApiError> {
        let mut map = HashMap::new();
        if let Some(cfg) = manager.loading_configuration() {
            for pkg in &cfg.preparation.packages {
                map.insert(
                    ("dsh".to_string(), pkg.name.clone()),
                    (pkg.name.clone(), pkg.version.clone()),
                );
            }
        }
        for plugin in manager.state().plugins.values() {
            if let magi_plugin_system::PluginSource::Upstream {
                ecosystem,
                package,
                version,
                github: None,
            } = &plugin.source
            {
                map.entry((ecosystem.clone(), package.clone()))
                    .or_insert_with(|| (plugin.id.clone(), version.clone()));
            }
        }
        Ok(map)
    }

    fn enabled_manifests(&self, manager: &PluginManager) -> Result<Vec<PluginManifest>, ApiError> {
        Ok(self
            .list_installed(manager)?
            .into_iter()
            .filter(|p| p.enabled && !p.reinstall_required)
            .map(|p| p.manifest)
            .collect())
    }

    fn enabled_commands(&self) -> Vec<Value> {
        let Some(rt) = self.inner.runtime.read().expect("runtime lock").clone() else {
            return Vec::new();
        };
        rt.services()
            .commands_snapshot()
            .into_iter()
            .filter_map(|cmd| {
                let owner = cmd.plugin_id?;
                Some(json!({
                    "id": format!("plugin/{owner}/{}", cmd.name),
                    "pluginId": owner,
                    "contributionId": cmd.name,
                    "version": LOCKED_DSH_VERSION,
                    "title": format!("/{}", cmd.name),
                    "description": cmd.description.unwrap_or_default(),
                }))
            })
            .collect()
    }

    fn preview_upstream(
        &self,
        request: PluginUpstreamRequest,
    ) -> NativePlatformFuture<'_, Result<PluginUpstreamPreview, ApiError>> {
        Box::pin(async move {
            if request.ecosystem != PluginEcosystem::Dsh {
                return Err(ApiError::InvalidInput("当前仅支持 DSH 原包安装".into()));
            }
            if request.github.is_some() {
                return Err(ApiError::InvalidInput(
                    "原生 DSH 平台当前请通过 npm 包名或本地 .tgz 原包归档安装".into(),
                ));
            }
            let package_name = request
                .package
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty() && is_valid_package_name(s))
                .ok_or_else(|| ApiError::InvalidInput("请输入有效的 npm 包名".into()))?
                .to_string();

            let registry = configured_npm_registry();
            let resolved_version = match request
                .version
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Some(v) => v.to_string(),
                None => {
                    let (_, packument) =
                        fetch_npm_packument(Some(&registry), &package_name).await?;
                    packument
                        .pointer("/dist-tags/latest")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            ApiError::InvalidInput("该 npm 包没有可用的 latest 版本".into())
                        })?
                        .to_string()
                }
            };

            let release =
                fetch_npm_release_archive(Some(&registry), &package_name, Some(&resolved_version))
                    .await?;
            let temp = tempfile::tempdir()
                .map_err(|e| ApiError::internal_assembly("创建预览临时目录失败", e))?;
            release.stage(temp.path())?;
            let manifest_bytes = fs::read(temp.path().join("package.json"))
                .map_err(|e| ApiError::InvalidInput(format!("读取 package.json 失败：{e}")))?;
            let orig_manifest: Value = serde_json::from_slice(&manifest_bytes)
                .map_err(|e| ApiError::InvalidInput(format!("package.json 不是有效 JSON：{e}")))?;
            check_static_dsh_compatibility(&orig_manifest).map_err(ApiError::InvalidInput)?;

            let client_declared = resolve_client_entry_file(temp.path(), &orig_manifest).is_some();
            let has_icon = resolve_icon_file(temp.path(), &orig_manifest).is_some();
            let synthetic_manifest = build_synthetic_manifest(
                &package_name,
                &resolved_version,
                &orig_manifest,
                has_icon,
                client_declared,
                None,
            );

            let (previous_version, previous_digest) = {
                let guard =
                    self.inner.owner.lock().map_err(|_| {
                        ApiError::internal_assembly("读取插件状态失败", "插件锁已损坏")
                    })?;
                if let Some(pkg) = guard.loading_configuration().and_then(|c| {
                    c.preparation
                        .packages
                        .iter()
                        .find(|p| p.name == package_name)
                }) {
                    (Some(pkg.version.clone()), Some(pkg.manifest_sha256.clone()))
                } else if let Some(legacy) = guard.state().plugins.get(&package_name) {
                    (Some(legacy.version.clone()), Some(legacy.digest.clone()))
                } else {
                    (None, None)
                }
            };

            let operation = if previous_version.is_some() {
                PluginCandidateOperation::Upgrade
            } else {
                PluginCandidateOperation::Install
            };

            let candidate = PluginCandidatePreview {
                manifest: synthetic_manifest.clone(),
                digest: release.sha256(),
                operation,
                installed_version: previous_version,
                installed_digest: previous_digest,
                added_permissions: synthetic_manifest.permissions.clone(),
                removed_permissions: Vec::new(),
                data_schema_from: None,
                data_reset: false,
            };

            let publisher = orig_manifest
                .pointer("/author/name")
                .or_else(|| orig_manifest.get("author"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let official = package_name.starts_with("@deepseek-ai/")
                || package_name.starts_with("@earendil-works/");

            Ok(PluginUpstreamPreview {
                candidate,
                package: package_name,
                version: resolved_version,
                integrity: release.integrity().to_string(),
                publisher,
                official,
                github: None,
            })
        })
    }

    fn install_upstream(
        &self,
        request: PluginUpstreamRequest,
        request_id: Option<String>,
    ) -> NativePlatformFuture<'_, Result<PluginInstallOutcome, ApiError>> {
        Box::pin(async move {
            if request.ecosystem != PluginEcosystem::Dsh {
                return Err(ApiError::InvalidInput("当前仅支持 DSH 原包安装".into()));
            }
            let package_name = request
                .package
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty() && is_valid_package_name(s))
                .ok_or_else(|| ApiError::InvalidInput("请输入有效的 npm 包名".into()))?;
            let version = request
                .version
                .as_deref()
                .map(str::trim)
                .filter(|s| semver::Version::parse(s).is_ok())
                .ok_or_else(|| ApiError::InvalidInput("安装必须携带已确认的精确版本号".into()))?;
            let digest = request
                .expected_digest
                .filter(|digest| {
                    digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
                })
                .ok_or_else(|| {
                    ApiError::InvalidInput("在线安装前必须先预览并携带原包归档摘要".into())
                })?;
            let spec = format!("{package_name}@{version}");
            let package_name = package_name.to_owned();
            let coordinator = self.clone();
            // Once submitted, the daemon owns settlement even if the initiating tab disconnects.
            tokio::spawn(async move {
                let rt = coordinator.wait_runtime().await?;
                let mgmt = rt
                    .management()
                    .ok_or_else(|| ApiError::Conflict("插件包管理服务未启用".into()))?;
                let outcome = mgmt
                    .install_confirmed_bundle(&spec, digest, request_id)
                    .await?;
                if outcome["application"] == "cancelled" {
                    return Ok(PluginInstallOutcome::Cancelled);
                }
                ensure_management_outcome_ok(&outcome)?;
                coordinator.refresh_after_install(&package_name);
                Ok(PluginInstallOutcome::Applied)
            })
            .await
            .map_err(|err| ApiError::internal_assembly("在线插件操作未正常结算", err))?
        })
    }

    fn cancel_install(
        &self,
        request_id: String,
    ) -> NativePlatformFuture<'_, Result<Value, ApiError>> {
        Box::pin(async move {
            let rt = self.wait_runtime().await?;
            let mgmt = rt
                .management()
                .ok_or_else(|| ApiError::Conflict("插件包管理服务未启用".into()))?;
            Ok(mgmt.cancel_install(&request_id).await)
        })
    }

    fn preview_archive(
        &self,
        _source: String,
        archive_bytes: Vec<u8>,
    ) -> NativePlatformFuture<'_, Result<PluginCandidatePreview, ApiError>> {
        Box::pin(async move {
            let archive = NpmArchive::from_tarball_bytes(archive_bytes.clone())?;
            let temp = tempfile::tempdir()
                .map_err(|e| ApiError::internal_assembly("创建预览临时目录失败", e))?;
            archive.stage(temp.path())?;
            let manifest_bytes = fs::read(temp.path().join("package.json"))
                .map_err(|e| ApiError::InvalidInput(format!("读取 package.json 失败：{e}")))?;
            let orig_manifest: Value = serde_json::from_slice(&manifest_bytes)
                .map_err(|e| ApiError::InvalidInput(format!("package.json 不是有效 JSON：{e}")))?;

            let package_name = orig_manifest
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| is_valid_package_name(s))
                .ok_or_else(|| ApiError::InvalidInput("原包 package.json 缺少有效的 name".into()))?
                .to_string();
            let version = orig_manifest
                .get("version")
                .and_then(Value::as_str)
                .filter(|s| semver::Version::parse(s).is_ok())
                .ok_or_else(|| {
                    ApiError::InvalidInput("原包 package.json 缺少有效的 SemVer version".into())
                })?
                .to_string();

            check_static_dsh_compatibility(&orig_manifest).map_err(ApiError::InvalidInput)?;

            let digest = hex_sha256(&archive_bytes);
            let client_declared = resolve_client_entry_file(temp.path(), &orig_manifest).is_some();
            let has_icon = resolve_icon_file(temp.path(), &orig_manifest).is_some();
            let synthetic_manifest = build_synthetic_manifest(
                &package_name,
                &version,
                &orig_manifest,
                has_icon,
                client_declared,
                None,
            );

            let (previous_version, previous_digest) = {
                let guard =
                    self.inner.owner.lock().map_err(|_| {
                        ApiError::internal_assembly("读取插件状态失败", "插件锁已损坏")
                    })?;
                if let Some(pkg) = guard.loading_configuration().and_then(|c| {
                    c.preparation
                        .packages
                        .iter()
                        .find(|p| p.name == package_name)
                }) {
                    (Some(pkg.version.clone()), Some(pkg.manifest_sha256.clone()))
                } else if let Some(legacy) = guard.state().plugins.get(&package_name) {
                    (Some(legacy.version.clone()), Some(legacy.digest.clone()))
                } else {
                    (None, None)
                }
            };

            {
                let mut cache = self.inner.preview_cache.lock().expect("preview cache lock");
                cache.retain(|item| {
                    item.created_at.elapsed() < PREVIEW_RETENTION_TTL && item.digest != digest
                });
                if cache.len() >= MAX_PREVIEW_CACHE {
                    cache.remove(0);
                }
                cache.push(RetainedArchivePreview {
                    digest: digest.clone(),
                    package_name: package_name.clone(),
                    created_at: Instant::now(),
                });
            }

            Ok(PluginCandidatePreview {
                manifest: synthetic_manifest.clone(),
                digest,
                operation: if previous_version.is_some() {
                    PluginCandidateOperation::Upgrade
                } else {
                    PluginCandidateOperation::Install
                },
                installed_version: previous_version,
                installed_digest: previous_digest,
                added_permissions: synthetic_manifest.permissions,
                removed_permissions: Vec::new(),
                data_schema_from: None,
                data_reset: false,
            })
        })
    }

    fn install_archive(
        &self,
        _source: String,
        archive_bytes: Vec<u8>,
        expected_digest: String,
    ) -> NativePlatformFuture<'_, Result<(), ApiError>> {
        Box::pin(async move {
            let actual_digest = hex_sha256(&archive_bytes);
            if actual_digest != expected_digest {
                return Err(ApiError::Conflict(
                    "候选原包归档与预览确认摘要不一致，请重新预览后再安装".into(),
                ));
            }
            let cached = {
                let mut cache = self.inner.preview_cache.lock().expect("preview cache lock");
                cache.retain(|item| item.created_at.elapsed() < PREVIEW_RETENTION_TTL);
                cache
                    .iter()
                    .find(|item| item.digest == expected_digest)
                    .cloned()
            };
            let pkg_name = if let Some(c) = &cached {
                c.package_name.clone()
            } else {
                let archive = NpmArchive::from_tarball_bytes(archive_bytes.clone())?;
                let temp = tempfile::tempdir()
                    .map_err(|e| ApiError::internal_assembly("创建临时目录失败", e))?;
                archive.stage(temp.path())?;
                let orig: Value = serde_json::from_slice(
                    &fs::read(temp.path().join("package.json"))
                        .map_err(|e| ApiError::InvalidInput(e.to_string()))?,
                )
                .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
                orig.get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ApiError::InvalidInput("缺少包名".into()))?
                    .to_string()
            };

            let temp_dir = tempfile::tempdir()
                .map_err(|e| ApiError::internal_assembly("创建安装暂存目录失败", e))?;
            let tarball_path = temp_dir.path().join("package.tgz");
            fs::write(&tarball_path, &archive_bytes)
                .map_err(|e| ApiError::internal_assembly("写入暂存归档失败", e))?;
            let canon_tarball = fs::canonicalize(&tarball_path)
                .map_err(|e| ApiError::internal_assembly("规范化暂存归档路径失败", e))?;

            let rt = self.wait_runtime().await?;
            let mgmt = rt
                .management()
                .ok_or_else(|| ApiError::Conflict("插件包管理服务未启用".into()))?;

            let spec = format!("file:{}", canon_tarball.display());
            let outcome = mgmt
                .install_confirmed_bundle(&spec, expected_digest, None)
                .await?;
            ensure_management_outcome_ok(&outcome)?;
            self.refresh_after_install(&pkg_name);
            Ok(())
        })
    }

    fn set_enabled(
        &self,
        plugin_id: String,
        enabled: bool,
        _expected_digest: Option<String>,
    ) -> NativePlatformFuture<'_, Result<(), ApiError>> {
        Box::pin(async move {
            {
                let guard =
                    self.inner.owner.lock().map_err(|_| {
                        ApiError::internal_assembly("读取插件状态失败", "插件锁已损坏")
                    })?;
                let is_native = guard
                    .loading_configuration()
                    .is_some_and(|c| c.preparation.packages.iter().any(|p| p.name == plugin_id));
                if !is_native && guard.state().plugins.contains_key(&plugin_id) {
                    return Err(ApiError::Conflict(
                        "旧格式插件不可直接启用，请点击重新安装原包".into(),
                    ));
                }
            }
            let rt = self.wait_runtime().await?;
            let mgmt = rt
                .management()
                .ok_or_else(|| ApiError::Conflict("插件包管理服务未启用".into()))?;
            let outcome = mgmt.set_bundle_enabled(&plugin_id, enabled).await?;
            ensure_management_outcome_ok(&outcome)?;
            self.inner.event_sink.publish_changed();
            Ok(())
        })
    }

    fn uninstall(&self, plugin_id: String) -> NativePlatformFuture<'_, Result<(), ApiError>> {
        Box::pin(async move {
            let is_native = {
                let guard =
                    self.inner.owner.lock().map_err(|_| {
                        ApiError::internal_assembly("读取插件状态失败", "插件锁已损坏")
                    })?;
                guard
                    .loading_configuration()
                    .is_some_and(|c| c.preparation.packages.iter().any(|p| p.name == plugin_id))
            };
            if is_native {
                let rt = self.wait_runtime().await?;
                let mgmt = rt
                    .management()
                    .ok_or_else(|| ApiError::Conflict("插件包管理服务未启用".into()))?;
                let outcome = mgmt.remove_bundle(&plugin_id).await?;
                ensure_management_outcome_ok(&outcome)?;
            } else if !self.cleanup_legacy_record_if_present(&plugin_id) {
                return Err(ApiError::NotFound(format!("插件 {plugin_id} 未安装")));
            }
            self.inner
                .slot_observations
                .write()
                .expect("slot_observations lock")
                .remove(&plugin_id);
            self.inner.event_sink.publish_changed();
            Ok(())
        })
    }

    fn serve_ui_asset(
        &self,
        manager: &PluginManager,
        plugin_id: &str,
        path: &str,
        expected_digest: Option<&str>,
    ) -> Result<(&'static str, Vec<u8>), ApiError> {
        let cfg = manager
            .loading_configuration()
            .ok_or_else(|| ApiError::NotFound("尚未加载原生插件配置".into()))?;
        let pkg = cfg
            .preparation
            .packages
            .iter()
            .find(|p| p.name == plugin_id)
            .ok_or_else(|| ApiError::NotFound("插件未安装".into()))?;
        if let Some(expected) = expected_digest
            && !expected.is_empty()
            && expected != pkg.manifest_sha256
        {
            return Err(ApiError::Conflict("插件页面版本已失效".into()));
        }
        let orig_manifest = read_original_manifest(&pkg.directory).unwrap_or_else(|| json!({}));
        if !package_is_enabled_in_configuration(cfg, pkg, &orig_manifest) {
            return Err(ApiError::Forbidden("插件当前未启用".into()));
        }

        let clean_path = path.trim_start_matches('/').trim_start_matches("ui/");
        match clean_path {
            "" | "index.html" => Ok((
                "text/html; charset=utf-8",
                SANDBOX_INDEX_HTML.as_bytes().to_vec(),
            )),
            "dsh-client-host.js" => {
                let bytes = self.read_client_host_asset("dsh-client-host.js")?;
                Ok(("text/javascript; charset=utf-8", bytes))
            }
            "dsh-client-host.css" => {
                let bytes = self.read_client_host_asset("dsh-client-host.css")?;
                Ok(("text/css; charset=utf-8", bytes))
            }
            "plugin-client.js" => {
                let orig = read_original_manifest(&pkg.directory)
                    .ok_or_else(|| ApiError::NotFound("缺少原包 package.json".into()))?;
                if let Some(entry_file) = resolve_client_entry_file(&pkg.directory, &orig) {
                    let bytes = fs::read(&entry_file)
                        .map_err(|e| ApiError::internal_assembly("读取插件客户端产物失败", e))?;
                    Ok(("text/javascript; charset=utf-8", bytes))
                } else {
                    Ok((
                        "text/javascript; charset=utf-8",
                        b"/* no ./client export */\n".to_vec(),
                    ))
                }
            }
            other => {
                if Path::new(other).is_absolute()
                    || Path::new(other)
                        .components()
                        .any(|c| !matches!(c, Component::Normal(_)))
                {
                    return Err(ApiError::Forbidden("非法的插件静态资源路径".into()));
                }
                let canon_root = fs::canonicalize(&pkg.directory)
                    .map_err(|_| ApiError::NotFound("原包目录不可读".into()))?;
                let canon_file = fs::canonicalize(canon_root.join(other))
                    .map_err(|_| ApiError::NotFound("插件静态资源不存在".into()))?;
                if !canon_file.starts_with(&canon_root) || !canon_file.is_file() {
                    return Err(ApiError::NotFound("插件静态资源不存在".into()));
                }
                let bytes = fs::read(&canon_file)
                    .map_err(|e| ApiError::internal_assembly("读取静态资源失败", e))?;
                let ext = canon_file
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let ct = match ext.as_str() {
                    "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
                    "css" => "text/css; charset=utf-8",
                    "json" => "application/json; charset=utf-8",
                    "woff" => "font/woff",
                    "woff2" => "font/woff2",
                    _ => "application/octet-stream",
                };
                Ok((ct, bytes))
            }
        }
    }

    fn serve_icon(
        &self,
        manager: &PluginManager,
        plugin_id: &str,
    ) -> Result<Option<(&'static str, Vec<u8>)>, ApiError> {
        let Some(cfg) = manager.loading_configuration() else {
            return Ok(None);
        };
        let Some(pkg) = cfg
            .preparation
            .packages
            .iter()
            .find(|p| p.name == plugin_id)
        else {
            return Ok(None);
        };
        let Some(orig) = read_original_manifest(&pkg.directory) else {
            return Ok(None);
        };
        Ok(resolve_icon_file(&pkg.directory, &orig))
    }

    fn call_gateway(
        &self,
        plugin_id: String,
        request: PluginRouteCallRequest,
        session: Option<PluginRouteSession>,
    ) -> NativePlatformFuture<'_, Result<PluginRouteCallResult, ApiError>> {
        Box::pin(async move {
            let (rt, candidate_activations) = self
                .admit_plugin_page_activations(
                    &plugin_id,
                    request.expected_digest.as_deref(),
                    request.expected_instance_id.as_deref(),
                )
                .await?;

            let method = match request.method {
                PluginRouteMethod::GET => Method::GET,
                PluginRouteMethod::HEAD => Method::HEAD,
                PluginRouteMethod::POST => Method::POST,
                PluginRouteMethod::PUT => Method::PUT,
                PluginRouteMethod::PATCH => Method::PATCH,
                PluginRouteMethod::DELETE => Method::DELETE,
                PluginRouteMethod::OPTIONS => Method::OPTIONS,
            };
            let uri = match &request.query {
                Some(q) if !q.is_empty() => format!("{}?{q}", request.path),
                _ => request.path.clone(),
            };
            let body_bytes: Vec<u8> = if let Some(b64) = &request.body_base64 {
                BASE64
                    .decode(b64)
                    .map_err(|_| ApiError::InvalidInput("路由二进制载荷不是合法的 base64".into()))?
            } else if let Some(json_body) = &request.body {
                serde_json::to_vec(json_body)
                    .map_err(|e| ApiError::InvalidInput(format!("编码路由 JSON 正文失败：{e}")))?
            } else {
                Vec::new()
            };

            let gateway = rt.gateway();
            let mut pages = Vec::new();
            for act in &candidate_activations {
                if let Ok(page) = gateway.bind_page(act) {
                    pages.push(page);
                }
            }
            if pages.is_empty() && (request.path == "/api" || request.path.starts_with("/api/")) {
                pages.push(gateway.bind_host_page()?);
            }
            if pages.is_empty() {
                return Err(ApiError::Conflict("插件当前没有活跃的后端实例".into()));
            }

            let mut last_response = None;
            for (idx, page) in pages.iter().enumerate() {
                let mut builder = Request::builder().method(method.clone()).uri(&uri);
                let mut has_content_type = false;
                if let Some(headers_obj) = request.headers.as_ref().and_then(Value::as_object) {
                    for (k, v) in headers_obj {
                        let Some(val_str) = v.as_str() else { continue };
                        let Ok(h_name) = HeaderName::from_bytes(k.as_bytes()) else {
                            continue;
                        };
                        let Ok(h_val) = HeaderValue::from_str(val_str) else {
                            continue;
                        };
                        if h_name == axum::http::header::CONTENT_TYPE {
                            has_content_type = true;
                        }
                        builder = builder.header(h_name, h_val);
                    }
                }
                if !has_content_type && request.body.is_some() {
                    builder = builder.header(axum::http::header::CONTENT_TYPE, "application/json");
                }
                // Same-origin CSRF proof for plugins like `dsh-plugin-ops-bundle` that enforce `Origin` == `Host`.
                builder = builder.header(axum::http::header::ORIGIN, &self.inner.public_origin);

                let mut http_req = builder
                    .body(Body::from(body_bytes.clone()))
                    .map_err(|e| ApiError::InvalidInput(format!("构造网关请求失败：{e}")))?;
                if let Some(session) = &session {
                    http_req.extensions_mut().insert(session.clone());
                }
                let resp = gateway.forward(page, http_req).await?;
                if resp.status() == axum::http::StatusCode::NOT_FOUND && idx + 1 < pages.len() {
                    last_response = Some(resp);
                    continue;
                }
                last_response = Some(resp);
                break;
            }

            let resp = last_response.expect("pages non-empty");
            let status = resp.status().as_u16();
            let mut resp_headers = serde_json::Map::new();
            let mut resp_content_type: Option<String> = None;
            let mut is_json = false;
            for (name, val) in resp.headers() {
                let key = name.as_str();
                if matches!(
                    key,
                    "set-cookie"
                        | "host"
                        | "origin"
                        | "cookie"
                        | "connection"
                        | "content-length"
                        | "content-encoding"
                        | "transfer-encoding"
                        | "upgrade"
                ) {
                    continue;
                }
                if let Ok(s) = val.to_str() {
                    if key == "content-type" {
                        resp_content_type = Some(s.to_string());
                        if s.to_ascii_lowercase().contains("application/json") {
                            is_json = true;
                        }
                    }
                    resp_headers.insert(key.to_string(), Value::String(s.to_string()));
                }
            }

            let raw_bytes = to_bytes(resp.into_body(), 16 * 1024 * 1024)
                .await
                .map_err(|e| ApiError::Conflict(format!("读取网关响应体失败：{e}")))?;

            let (body_val, body_b64) = if raw_bytes.is_empty() {
                (None, None)
            } else if is_json && let Ok(parsed) = serde_json::from_slice::<Value>(&raw_bytes) {
                (Some(parsed), None)
            } else {
                (None, Some(BASE64.encode(&raw_bytes)))
            };

            Ok(PluginRouteCallResult {
                status,
                content_type: resp_content_type,
                headers: (!resp_headers.is_empty()).then_some(Value::Object(resp_headers)),
                body: body_val,
                body_base64: body_b64,
            })
        })
    }

    fn forward_gateway_upgrade(
        &self,
        plugin_id: String,
        expected_digest: Option<String>,
        expected_instance_id: Option<String>,
        mut request: axum::http::Request<Body>,
    ) -> NativePlatformFuture<'_, Result<axum::response::Response, ApiError>> {
        Box::pin(async move {
            let (rt, candidate_activations) = self
                .admit_plugin_page_activations(
                    &plugin_id,
                    expected_digest.as_deref(),
                    expected_instance_id.as_deref(),
                )
                .await?;
            let gateway = rt.gateway();
            let page = candidate_activations
                .iter()
                .find_map(|act| gateway.bind_page(act).ok())
                .map(Ok)
                .unwrap_or_else(|| gateway.bind_host_page())?;
            let origin = HeaderValue::from_str(&self.inner.public_origin)
                .map_err(|e| ApiError::InvalidInput(format!("网关公开来源无效：{e}")))?;
            request
                .headers_mut()
                .insert(axum::http::header::ORIGIN, origin);
            gateway.forward(&page, request).await
        })
    }

    fn report_client_slots(
        &self,
        plugin_id: &str,
        report: PluginClientSlotsReport,
    ) -> Result<(), ApiError> {
        let mut tabs = std::collections::HashSet::new();
        if report
            .right_pane_tabs
            .as_ref()
            .is_some_and(|right_pane_tabs| {
                right_pane_tabs.len() > 64
                    || right_pane_tabs.iter().any(|tab| {
                        tab.key.is_empty()
                            || tab.key.chars().count() > 64
                            || tab.key == "settings"
                            || tab.key.starts_with("main:")
                            || tab.key.chars().any(char::is_control)
                            || tab.title.trim().is_empty()
                            || tab.title.chars().count() > 128
                            || !tabs.insert(&tab.key)
                    })
            })
        {
            return Err(ApiError::InvalidInput("插件右栏声明无效".into()));
        }
        let owner = self
            .inner
            .owner
            .lock()
            .map_err(|_| ApiError::internal_assembly("读取插件运行状态失败", "插件锁已损坏"))?;
        if let Some(expected) = &report.instance_id {
            let active = self
                .inner
                .runtime
                .read()
                .expect("runtime lock")
                .clone()
                .and_then(|runtime| match runtime.supervisor_state() {
                    SharedHostSupervisorState::Running { host_generation } => Some(host_generation),
                    _ => None,
                });
            let current = owner.loading_configuration().is_some_and(|cfg| {
                cfg.preparation
                    .packages
                    .iter()
                    .find(|pkg| pkg.name == plugin_id)
                    .is_some_and(|pkg| {
                        cfg.executables
                            .iter()
                            .filter(|exec| {
                                exec.plugin_id == plugin_id
                                    || exec.entry.starts_with(&pkg.directory)
                            })
                            .filter_map(|exec| owner.loading_instance_status(&exec.instance_id))
                            .any(|(activation, status)| {
                                active == Some(activation.host_generation)
                                    && matches!(status, LoadingInstanceStatus::Running)
                                    && page_instance_id(activation) == *expected
                            })
                    })
            });
            if !current {
                return Err(ApiError::Conflict("插件页面的运行实例已失效".into()));
            }
        } else if report.right_pane_tabs.is_some() {
            return Err(ApiError::InvalidInput(
                "插件右栏声明缺少运行实例身份".into(),
            ));
        }
        let mut guard = self
            .inner
            .slot_observations
            .write()
            .expect("slot_observations lock");
        // Do not let a transient empty report (`hasSettingsView: false`) overwrite an
        // already-confirmed settings view within the same client document instance.
        let preserved_settings_view = if report.has_settings_view {
            Some(true)
        } else if let Some(prev) = guard.get(plugin_id)
            && prev.instance_id == report.instance_id
            && prev.has_settings_view == Some(true)
        {
            Some(true)
        } else {
            Some(false)
        };
        let right_pane_tabs = report.right_pane_tabs.unwrap_or_else(|| {
            guard
                .get(plugin_id)
                .filter(|prev| prev.instance_id == report.instance_id)
                .map(|prev| prev.right_pane_tabs.clone())
                .unwrap_or_default()
        });
        const RIGHT_PANE_BODY: &str = "sidebar.right.pane.tab";
        let mut mounted_slots = report.mounted_slots;
        let mut unmounted_slots = report.unmounted_slots;
        if !right_pane_tabs.is_empty() {
            unmounted_slots.retain(|slot| slot != RIGHT_PANE_BODY);
            if !mounted_slots.iter().any(|slot| slot == RIGHT_PANE_BODY) {
                mounted_slots.push(RIGHT_PANE_BODY.to_string());
                mounted_slots.sort();
            }
        }
        let next = ObservedClientSlots {
            instance_id: report.instance_id,
            mounted_slots,
            unmounted_slots,
            has_settings_view: preserved_settings_view,
            main_keys: report.main_keys,
            right_pane_tabs,
        };
        let changed = guard.get(plugin_id).is_none_or(|prev| {
            prev.instance_id != next.instance_id
                || prev.mounted_slots != next.mounted_slots
                || prev.unmounted_slots != next.unmounted_slots
                || prev.has_settings_view != next.has_settings_view
                || prev.main_keys != next.main_keys
                || prev.right_pane_tabs != next.right_pane_tabs
        });
        guard.insert(plugin_id.to_string(), next);
        drop(guard);
        drop(owner);
        if changed {
            self.inner.event_sink.publish_changed();
        }
        Ok(())
    }

    fn restart_host(&self) -> NativePlatformFuture<'_, Result<(), ApiError>> {
        Box::pin(async move {
            let rt = self.wait_runtime().await?;
            rt.restart().await?;
            self.inner.event_sink.publish_changed();
            Ok(())
        })
    }
}

impl NativePluginCoordinator {
    async fn admit_plugin_page_activations(
        &self,
        plugin_id: &str,
        expected_digest: Option<&str>,
        expected_instance_id: Option<&str>,
    ) -> Result<(Arc<PluginRuntime>, Vec<LoadingActivation>), ApiError> {
        let rt = self.wait_runtime().await?;
        let host_gen = match rt.supervisor_state() {
            SharedHostSupervisorState::Running { host_generation } => host_generation,
            _ => return Err(ApiError::Conflict("DSH 插件宿主当前未处于运行状态".into())),
        };
        let owned = {
            let guard =
                self.inner.owner.lock().map_err(|_| {
                    ApiError::internal_assembly("读取插件运行状态失败", "插件锁已损坏")
                })?;
            let cfg = guard
                .loading_configuration()
                .ok_or_else(|| ApiError::Conflict("尚未加载原生插件配置".into()))?;
            let pkg = cfg
                .preparation
                .packages
                .iter()
                .find(|p| p.name == plugin_id)
                .ok_or_else(|| ApiError::NotFound("插件未安装".into()))?;
            if let Some(expected) = expected_digest
                && !expected.is_empty()
                && expected != pkg.manifest_sha256
            {
                return Err(ApiError::Conflict(
                    "插件页面版本已过期，请刷新设置页".into(),
                ));
            }
            let orig_manifest = read_original_manifest(&pkg.directory).unwrap_or_else(|| json!({}));
            if !package_is_enabled_in_configuration(cfg, pkg, &orig_manifest) {
                return Err(ApiError::Forbidden("插件当前已停用".into()));
            }
            let owned: Vec<LoadingActivation> = cfg
                .executables
                .iter()
                .filter(|exec| {
                    exec.plugin_id == plugin_id || exec.entry.starts_with(&pkg.directory)
                })
                .filter_map(|exec| guard.loading_instance_binding(&exec.instance_id))
                .filter(|act| act.host_generation == host_gen)
                .cloned()
                .collect();
            if expected_instance_id.is_some_and(|expected| {
                !owned
                    .iter()
                    .any(|activation| page_instance_id(activation) == expected)
            }) {
                return Err(ApiError::Conflict(
                    "插件页面的运行实例已失效，请重新打开设置页".into(),
                ));
            }
            owned
        };
        Ok((rt, owned))
    }

    fn read_client_host_asset(&self, filename: &str) -> Result<Vec<u8>, ApiError> {
        if let Some(tc) = &self.inner.toolchain {
            for candidate in [
                tc.sdk_project.join("runtime/dist").join(filename),
                tc.sdk_project.join("dist").join(filename),
                tc.sdk_project.join("runtime").join(filename),
            ] {
                if candidate.is_file() {
                    return fs::read(&candidate)
                        .map_err(|e| ApiError::internal_assembly("读取客户端宿主资产失败", e));
                }
            }
        }
        let repo_fallback = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../magi-extension-host/runtime/dsh/dist")
            .join(filename);
        if repo_fallback.is_file() {
            return fs::read(&repo_fallback)
                .map_err(|e| ApiError::internal_assembly("读取客户端宿主资产失败", e));
        }
        Err(ApiError::NotFound(format!(
            "未找到 DSH 客户端页面宿主构建产物：{filename}"
        )))
    }

    fn refresh_after_install(&self, plugin_id: &str) {
        self.cleanup_legacy_record_if_present(plugin_id);
        self.inner
            .slot_observations
            .write()
            .expect("slot_observations lock")
            .remove(plugin_id);
        self.inner.event_sink.publish_changed();
    }

    fn cleanup_legacy_record_if_present(&self, plugin_id: &str) -> bool {
        let Ok(drain) = PluginManager::begin_drain(self.inner.owner.clone(), plugin_id) else {
            return false;
        };
        let Ok(mut guard) = self.inner.owner.lock() else {
            return false;
        };
        let removed = guard.uninstall(plugin_id, &drain).is_ok();
        drain.commit(&mut guard);
        removed
    }
}

fn configuration_error(observation: &LoadingSourceObservation) -> Option<&str> {
    match observation {
        LoadingSourceObservation::Rejected { message } => Some(message),
        _ => None,
    }
}

fn ensure_management_outcome_ok(outcome: &Value) -> Result<(), ApiError> {
    let target = outcome.get("result").unwrap_or(outcome);
    if target.get("ok").and_then(Value::as_bool) == Some(false) {
        let msg = target
            .pointer("/error/diagnostic")
            .or_else(|| target.pointer("/error/message"))
            .or_else(|| target.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("插件管理操作未成功");
        return Err(ApiError::Conflict(msg.to_string()));
    }
    if let Some(app) = target.get("application").and_then(Value::as_str)
        && matches!(app, "failed" | "refused" | "cancelled")
    {
        let msg = target
            .pointer("/error/diagnostic")
            .or_else(|| target.pointer("/error/message"))
            .or_else(|| target.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("插件安装或变更未应用");
        return Err(ApiError::Conflict(msg.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod session_request_tests {
    use super::*;

    /// 宿主发来的请求形状（与 `runtime/dsh/session-gateway.mjs` 一致）必须能被 daemon 原样解析，
    /// 未知字段整体拒绝。
    #[test]
    fn the_session_requests_the_host_sends_parse_and_unknown_fields_are_refused() {
        let prompt: SessionPromptRequest = serde_json::from_value(json!({
            "sessionId": "s1", "requestId": "r1", "steer": true, "text": "你好",
            "images": [{"mediaType": "image/png", "data": "AAAA", "name": "a.png"}, {"mediaType": "image/png", "data": "BB"}],
        }))
        .unwrap();
        assert!(prompt.steer && prompt.images.len() == 2 && prompt.images[1].name.is_none());
        assert!(
            serde_json::from_value::<SessionPromptRequest>(json!({
                "sessionId": "s1", "requestId": "r1", "text": "x", "mode": "queue"
            }))
            .is_err()
        );

        for (value, expect) in [
            (json!({"kind": "remove"}), "remove"),
            (json!({"kind": "steer"}), "steer"),
            (json!({"kind": "edit", "text": "新"}), "edit"),
        ] {
            let request: SessionQueueRequest = serde_json::from_value(json!({
                "sessionId": "s1", "queueId": "q1", "action": value,
            }))
            .unwrap();
            let name = match request.action {
                SessionQueueAction::Edit { .. } => "edit",
                SessionQueueAction::Remove => "remove",
                SessionQueueAction::Steer => "steer",
            };
            assert_eq!(name, expect);
        }
        assert!(
            serde_json::from_value::<SessionQueueRequest>(json!({
                "sessionId": "s1", "queueId": "q1", "action": {"kind": "reorder"}
            }))
            .is_err()
        );

        assert!(serde_json::from_value::<SessionCreateRequest>(json!({})).is_ok());
        assert!(
            serde_json::from_value::<SessionCreateRequest>(json!({"workspaceId": "w"})).is_ok()
        );
        assert!(serde_json::from_value::<SessionCreateRequest>(json!({"cwd": "/x"})).is_err());
        assert!(serde_json::from_value::<SessionIdRequest>(json!({"sessionId": "s"})).is_ok());
        assert!(serde_json::from_value::<SessionIdRequest>(json!({})).is_err());

        // `rename`：宿主发 `{sessionId, title}`，缺任一项或带多余字段都拒绝。
        let rename: SessionRenameRequest =
            serde_json::from_value(json!({"sessionId": "s1", "title": "新标题"})).unwrap();
        assert_eq!(
            (rename.session_id.as_str(), rename.title.as_str()),
            ("s1", "新标题")
        );
        for bad in [
            json!({"sessionId": "s1"}),
            json!({"title": "x"}),
            json!({"sessionId": "s1", "title": "x", "seq": 1}),
        ] {
            assert!(serde_json::from_value::<SessionRenameRequest>(bad).is_err());
        }
    }
}
