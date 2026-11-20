//! Daemon bridge for the shared DSH host service closure (`ctx.tools`, `ctx.web`,
//! `ctx.commands`, `ctx.skills`, `ctx.llm`, `ctx.agentDefaultModel`, `ctx.settings`, and
//! `ctx.userQuestions`).

use magi_api::ApiError;
use magi_bridge_client::{
    ChatMessage, ChatToolCall, ModelBridgeClient as _, ModelInvocationOptions,
    ModelInvocationRequest, llm_types::ImageSource,
};
use magi_conversation_runtime::{
    main_model_sources,
    model_config::{NormalizedModelConfig, resolve_orchestrator_model_config},
    plugin_command::{PluginCommandExecutor, PluginCommandInvocation, PluginCommandResult},
};
use magi_core::{EventId, ExecutionResultStatus, ReasoningEffort};
use magi_event_bus::{EventEnvelope, InMemoryEventBus};
use magi_extension_host::{
    peer::{BoxFuture, Incoming},
    protocol::{ErrorKind, RpcError},
    shared::SharedHost,
};
use magi_plugin_system::PluginManager;
use magi_session_store::{CanonicalTurnEvent, SessionRecord};
use magi_settings_store::SettingsStore;
use magi_tool_runtime::{
    BuiltinToolAccessMode, BuiltinToolName, PluginToolCatalogEntry, PluginToolCatalogProvider,
    PluginToolExecutor, ToolExecutionContext, WebProviderExecutor,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(60);
const WEB_SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
const WEB_FETCH_TIMEOUT: Duration = Duration::from_secs(45);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const HOST_LLM_TIMEOUT: Duration = Duration::from_secs(120);

pub type HostLlmInvoker = Arc<
    dyn Fn(HostLlmInvokeRequest) -> BoxFuture<Result<HostLlmInvokeResponse, String>> + Send + Sync,
>;

pub type HostUserQuestionHandler =
    Arc<dyn Fn(Value) -> BoxFuture<Result<Value, String>> + Send + Sync>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostLlmMessage {
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub images: Vec<ImageSource>,
    #[serde(default)]
    pub tool_calls: Vec<ChatToolCall>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostLlmInvokeRequest {
    pub provider: String,
    pub model: String,
    pub messages: Vec<HostLlmMessage>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub stop: Option<Vec<String>>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostLlmUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostLlmToolCallOut {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostLlmInvokeResponse {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<HostLlmToolCallOut>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<HostLlmUsage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshToolRegistration {
    pub plugin_id: Option<String>,
    pub instance_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parameters: Value,
    #[serde(default)]
    pub defer_loading: bool,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshWebProviderEntry {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub plugin_id: Option<String>,
    #[serde(default)]
    pub instance_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshWebSnapshot {
    pub search_provider: Option<String>,
    pub fetch_provider: Option<String>,
    #[serde(default)]
    pub search: Vec<DshWebProviderEntry>,
    #[serde(default)]
    pub fetch: Vec<DshWebProviderEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshCommandRegistration {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub plugin_id: Option<String>,
    #[serde(default)]
    pub instance_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedHostSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    web: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_default_model: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LlmProviderRegistration {
    plugin_id: String,
    provider: String,
    name: String,
    base_url: String,
    api_key: String,
    api_protocol: String,
    #[serde(default)]
    models: Vec<Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookCapabilities {
    #[serde(default)]
    pub tools_pre_execute: bool,
    #[serde(default)]
    pub tools_post_execute: bool,
    #[serde(default)]
    pub tools_restrict: bool,
    #[serde(default)]
    pub system_prompt_assemble: bool,
    #[serde(default)]
    pub approval: bool,
    #[serde(default)]
    pub compaction_before: bool,
    #[serde(default)]
    pub context_project: bool,
    #[serde(default)]
    pub llm_extend: bool,
    #[serde(default)]
    pub llm_stream_intercept: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshAgentLoopProvider {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub owner_plugin_id: Option<String>,
    #[serde(default)]
    pub capabilities: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshAgentLoopSnapshot {
    pub selected: String,
    #[serde(default)]
    pub providers: Vec<DshAgentLoopProvider>,
}

impl Default for DshAgentLoopSnapshot {
    fn default() -> Self {
        Self {
            selected: "magi-native".to_string(),
            providers: Vec::new(),
        }
    }
}

const HOOK_CALL_TIMEOUT: Duration = Duration::from_secs(30);

pub struct PluginServicesBridge {
    _owner: Arc<Mutex<PluginManager>>,
    current_host: Arc<RwLock<Option<Arc<SharedHost>>>>,
    dsh_home: PathBuf,
    host_settings_path: PathBuf,
    persisted_host_settings: Mutex<PersistedHostSettings>,
    settings_store: OnceLock<Arc<SettingsStore>>,
    event_bus: OnceLock<Arc<InMemoryEventBus>>,
    llm_invoker: RwLock<Option<HostLlmInvoker>>,
    question_handler: RwLock<Option<HostUserQuestionHandler>>,
    tools: RwLock<Vec<DshToolRegistration>>,
    web: RwLock<DshWebSnapshot>,
    commands: RwLock<Vec<DshCommandRegistration>>,
    skills: RwLock<Vec<Value>>,
    active_llm_sources: Mutex<BTreeMap<String, BTreeSet<String>>>,
    latest_llm_providers: RwLock<Vec<(String, String, String, Value)>>,
    hook_capabilities: RwLock<HookCapabilities>,
    agent_loop: RwLock<DshAgentLoopSnapshot>,
}

impl PluginServicesBridge {
    pub fn new(
        owner: Arc<Mutex<PluginManager>>,
        current_host: Arc<RwLock<Option<Arc<SharedHost>>>>,
        dsh_home: PathBuf,
    ) -> Result<Arc<Self>, ApiError> {
        std::fs::create_dir_all(&dsh_home)
            .map_err(|e| ApiError::internal_assembly("创建 DSH_HOME 目录失败", e))?;
        let host_settings_path = dsh_home.join("host-settings.json");
        let persisted = load_persisted_host_settings(&host_settings_path);
        Ok(Arc::new(Self {
            _owner: owner,
            current_host,
            dsh_home,
            host_settings_path,
            persisted_host_settings: Mutex::new(persisted),
            settings_store: OnceLock::new(),
            event_bus: OnceLock::new(),
            llm_invoker: RwLock::new(None),
            question_handler: RwLock::new(None),
            tools: RwLock::new(Vec::new()),
            web: RwLock::new(DshWebSnapshot::default()),
            commands: RwLock::new(Vec::new()),
            skills: RwLock::new(Vec::new()),
            active_llm_sources: Mutex::new(BTreeMap::new()),
            latest_llm_providers: RwLock::new(Vec::new()),
            hook_capabilities: RwLock::new(HookCapabilities::default()),
            agent_loop: RwLock::new(DshAgentLoopSnapshot::default()),
        }))
    }

    pub fn dsh_home(&self) -> &Path {
        &self.dsh_home
    }

    pub fn bind_settings_store(&self, store: Arc<SettingsStore>) {
        let _ = self.settings_store.set(store.clone());
        let cached = self
            .latest_llm_providers
            .read()
            .expect("llm providers lock")
            .clone();
        if !cached.is_empty() {
            self.apply_llm_sources_to_store(&store, &cached);
        }
    }

    pub fn bind_event_bus(&self, events: Arc<InMemoryEventBus>) {
        let _ = self.event_bus.set(events);
    }

    pub fn set_llm_invoker(&self, invoker: Option<HostLlmInvoker>) {
        *self.llm_invoker.write().expect("llm invoker lock") = invoker;
    }

    pub fn set_user_question_handler(&self, handler: Option<HostUserQuestionHandler>) {
        *self
            .question_handler
            .write()
            .expect("user question handler lock") = handler;
    }

    pub fn build_initial_host_config(&self) -> Value {
        let persisted = self
            .persisted_host_settings
            .lock()
            .expect("persisted host settings lock")
            .clone();
        let mut config = serde_json::Map::new();
        if let Some(web) = persisted.web {
            config.insert("web".into(), web);
        }
        let mut host_sources = Vec::new();
        let mut store_default_selection = None;
        if let Some(store) = self.settings_store.get() {
            if let Ok(sources) = main_model_sources::sources(store) {
                for src in sources {
                    if src.kind != main_model_sources::SourceKind::Custom {
                        continue;
                    }
                    let models = src
                        .source
                        .models
                        .iter()
                        .map(|m| {
                            let mut item = json!({
                                "id": m.id,
                                "displayName": if m.display_name.is_empty() { &m.id } else { &m.display_name },
                            });
                            if let Some(window) = m.context_window_tokens {
                                item["contextWindowTokens"] = json!(window);
                            }
                            if let Some(modalities) = &m.input_modalities {
                                item["inputModalities"] = json!(modalities);
                            }
                            item
                        })
                        .collect::<Vec<_>>();
                    host_sources.push(json!({
                        "provider": src.identity,
                        "name": src.source.name,
                        "models": models,
                    }));
                }
            }
            let selected = main_model_sources::with_first_model_default(
                store,
                &store.get_section("orchestratorSelection"),
            );
            if let (Some(provider), Some(model)) = (
                selected.get("sourceId").and_then(Value::as_str),
                selected.get("model").and_then(Value::as_str),
            ) && !provider.is_empty()
                && !model.is_empty()
            {
                let mut default_sel = json!({
                    "provider": provider,
                    "model": model,
                });
                if let Some(effort) = selected
                    .get("reasoningEffort")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    default_sel["reasoningEffort"] = json!(effort);
                }
                store_default_selection = Some(default_sel);
            }
        }
        if let Some(agent_default) = persisted.agent_default_model.or(store_default_selection) {
            config.insert("agentDefaultModel".into(), agent_default);
        }
        if !host_sources.is_empty() {
            config.insert("hostModelSources".into(), Value::Array(host_sources));
        }
        Value::Object(config)
    }

    pub fn on_host_stopped(&self) {
        self.tools.write().expect("tools lock").clear();
        *self.web.write().expect("web lock") = DshWebSnapshot::default();
        self.commands.write().expect("commands lock").clear();
        self.skills.write().expect("skills lock").clear();
        self.latest_llm_providers
            .write()
            .expect("llm providers lock")
            .clear();
        *self
            .hook_capabilities
            .write()
            .expect("hook capabilities lock") = HookCapabilities::default();
        *self.agent_loop.write().expect("agent loop lock") = DshAgentLoopSnapshot::default();
        let drained = {
            let mut active = self
                .active_llm_sources
                .lock()
                .expect("active llm sources lock");
            std::mem::take(&mut *active)
        };
        if let Some(store) = self.settings_store.get() {
            for plugin_id in drained.keys() {
                main_model_sources::clear_plugin_sources(store, plugin_id);
            }
        }
        self.publish_registry_changed();
    }

    pub fn tools_snapshot(&self) -> Vec<DshToolRegistration> {
        self.tools.read().expect("tools lock").clone()
    }

    pub fn web_snapshot(&self) -> DshWebSnapshot {
        self.web.read().expect("web lock").clone()
    }

    pub fn commands_snapshot(&self) -> Vec<DshCommandRegistration> {
        self.commands.read().expect("commands lock").clone()
    }

    pub fn skills_snapshot(&self) -> Vec<Value> {
        self.skills.read().expect("skills lock").clone()
    }

    pub fn llm_providers_snapshot(&self) -> Vec<(String, String, String, Value)> {
        self.latest_llm_providers
            .read()
            .expect("llm providers lock")
            .clone()
    }

    pub fn tool_catalog_provider(self: &Arc<Self>) -> PluginToolCatalogProvider {
        let bridge = Arc::clone(self);
        Arc::new(move || {
            let tools = bridge.tools.read().expect("tools lock");
            tools
                .iter()
                .filter(|tool| BuiltinToolName::from_name(&tool.name).is_none())
                .map(|tool| {
                    let plugin_id = tool
                        .plugin_id
                        .clone()
                        .unwrap_or_else(|| "dsh.shared".to_string());
                    let model_tool_name = if is_valid_wire_tool_name(&tool.name) {
                        tool.name.clone()
                    } else {
                        magi_plugin_system::model_tool_name(&plugin_id, &tool.name)
                    };
                    PluginToolCatalogEntry {
                        model_tool_name,
                        plugin_id,
                        contribution_id: tool.name.clone(),
                        title: tool.name.clone(),
                        description: tool.description.clone(),
                        input_schema: if tool.parameters.is_object() {
                            tool.parameters.clone()
                        } else {
                            json!({ "type": "object", "properties": {} })
                        },
                        read_only: false,
                    }
                })
                .collect()
        })
    }

    pub fn tool_executor(self: &Arc<Self>, handle: tokio::runtime::Handle) -> PluginToolExecutor {
        let bridge = Arc::clone(self);
        Arc::new(move |model_tool_name, arguments, context, cancelled| {
            bridge.execute_model_tool_blocking(
                &handle,
                model_tool_name,
                arguments,
                context,
                cancelled,
            )
        })
    }

    pub fn web_executor(self: &Arc<Self>, handle: tokio::runtime::Handle) -> WebProviderExecutor {
        let bridge = Arc::clone(self);
        Arc::new(move |tool, input, context, cancelled| {
            bridge.execute_web_blocking(&handle, tool, input, context, cancelled)
        })
    }

    pub fn command_executor(
        self: &Arc<Self>,
        handle: tokio::runtime::Handle,
    ) -> Arc<dyn PluginCommandExecutor> {
        Arc::new(SharedDshCommandExecutor {
            bridge: Arc::clone(self),
            handle,
        })
    }

    pub fn extension_hooks(
        self: &Arc<Self>,
        handle: tokio::runtime::Handle,
    ) -> Arc<dyn magi_conversation_runtime::extension_hooks::ExtensionHooks> {
        Arc::new(SharedDshExtensionHooks {
            bridge: Arc::clone(self),
            handle,
        })
    }

    pub fn hook_capabilities_snapshot(&self) -> HookCapabilities {
        self.hook_capabilities
            .read()
            .expect("hook capabilities lock")
            .clone()
    }

    pub fn agent_loop_snapshot(&self) -> DshAgentLoopSnapshot {
        self.agent_loop.read().expect("agent loop lock").clone()
    }

    pub async fn run_agent_loop(&self, session_id: &str, input: Value) -> Result<Value, ApiError> {
        let host = self.require_host()?;
        host.call_cancellable_service(
            "host.agentLoop.run",
            json!({ "sessionId": session_id, "input": input }),
            COMMAND_TIMEOUT,
        )
        .await
        .map_err(|e| ApiError::Conflict(format!("插件执行循环运行失败：{e}")))
    }

    pub async fn cancel_agent_loop(&self, session_id: &str) -> Result<Value, ApiError> {
        let host = self.require_host()?;
        host.call_service(
            "host.agentLoop.cancel",
            json!({ "sessionId": session_id }),
            Duration::from_secs(15),
        )
        .await
        .map_err(|e| ApiError::Conflict(format!("插件执行循环取消失败：{e}")))
    }

    pub async fn resume_agent_loop(
        &self,
        session_id: &str,
        options: Value,
    ) -> Result<Value, ApiError> {
        let host = self.require_host()?;
        host.call_cancellable_service(
            "host.agentLoop.resume",
            json!({ "sessionId": session_id, "options": options }),
            COMMAND_TIMEOUT,
        )
        .await
        .map_err(|e| ApiError::Conflict(format!("插件执行循环恢复失败：{e}")))
    }

    pub async fn execute_tool(
        &self,
        name: &str,
        args: Value,
        session_id: Option<&str>,
    ) -> Result<Value, ApiError> {
        let host = self.require_host()?;
        let timeout = self
            .tools
            .read()
            .expect("tools lock")
            .iter()
            .find(|t| t.name == name)
            .and_then(|t| t.timeout_ms)
            .map(Duration::from_millis)
            .unwrap_or(TOOL_CALL_TIMEOUT);
        host.call_cancellable_service(
            "host.tools.execute",
            json!({
                "name": name,
                "args": args,
                "sessionId": session_id,
            }),
            timeout,
        )
        .await
        .map_err(|e| ApiError::Conflict(format!("插件工具 {name} 调用失败：{e}")))
    }

    pub async fn web_search(&self, query: &str) -> Result<Value, ApiError> {
        let host = self.require_host()?;
        host.call_cancellable_service(
            "host.web.search",
            json!({ "query": query }),
            WEB_SEARCH_TIMEOUT,
        )
        .await
        .map_err(|e| ApiError::Conflict(format!("插件联网搜索失败：{e}")))
    }

    pub async fn web_fetch(&self, url: &str) -> Result<Value, ApiError> {
        let host = self.require_host()?;
        host.call_cancellable_service("host.web.fetch", json!({ "url": url }), WEB_FETCH_TIMEOUT)
            .await
            .map_err(|e| ApiError::Conflict(format!("插件网页抓取失败：{e}")))
    }

    pub async fn describe_settings(&self) -> Result<Value, ApiError> {
        let host = self.require_host()?;
        host.call_service("host.settings.describe", json!({}), Duration::from_secs(15))
            .await
            .map_err(|e| ApiError::Conflict(format!("读取插件设置失败：{e}")))
    }

    pub async fn mutate_settings(
        &self,
        ns: &str,
        ops: Value,
        expected_revision: Option<&str>,
    ) -> Result<Value, ApiError> {
        let host = self.require_host()?;
        let mut payload = json!({ "ns": ns, "ops": ops });
        if let Some(rev) = expected_revision {
            payload["expectedRevision"] = json!(rev);
        }
        host.call_service("host.settings.mutate", payload, Duration::from_secs(15))
            .await
            .map_err(|e| ApiError::Conflict(format!("更新插件设置 {ns} 失败：{e}")))
    }

    pub async fn sync_host_models(
        &self,
        default_selection: Option<Value>,
        sources: Vec<Value>,
    ) -> Result<(), ApiError> {
        let host = self.require_host()?;
        host.call_service(
            "host.llm.syncHostModels",
            json!({
                "defaultSelection": default_selection,
                "sources": sources,
            }),
            Duration::from_secs(15),
        )
        .await
        .map_err(|e| ApiError::Conflict(format!("同步宿主模型目录失败：{e}")))?;
        Ok(())
    }

    fn require_host(&self) -> Result<Arc<SharedHost>, ApiError> {
        self.current_host
            .read()
            .expect("host lock")
            .clone()
            .ok_or_else(|| ApiError::Conflict("共享插件宿主当前未运行".into()))
    }

    pub(crate) fn observe_committed_session_event(
        &self,
        event: &CanonicalTurnEvent,
        observation: Value,
        subagent_task_id: Option<&str>,
    ) {
        let mut params = json!({
            "eventSeq": event.event_seq,
            "occurredAt": event.occurred_at.0,
            "observation": observation,
        });
        if let Some(task_id) = subagent_task_id {
            params["subagentTaskId"] = json!(task_id);
        }
        self.observe_session(&event.session_id, params);
    }

    pub(crate) fn observe_session_created(&self, session: &SessionRecord) {
        let cwd = session
            .initial_working_directory_ref
            .as_deref()
            .and_then(|reference| magi_core::HostPath::from_path_ref(reference).ok());
        let mut observation = json!({"kind": "sessionCreated"});
        if let Some(path) = cwd.as_ref().and_then(|path| path.as_path().to_str()) {
            observation["cwd"] = json!(path);
        }
        self.observe_session(
            &session.session_id,
            json!({ "occurredAt": session.created_at.0, "observation": observation }),
        );
    }

    /// 会话目录变化通知。只说明哪些会话变了，宿主据此重新读取权威事实；投递失败只记录，
    /// 宿主下一次读取或收到的 `factsSequence` 缺口会触发重读。
    pub(crate) fn notify_session_changed(&self, mut params: Value) {
        let Ok(host) = self.require_host() else {
            return;
        };
        params["hostGeneration"] = json!(host.generation());
        if let Err(error) = host.notify_service("host.session.changed", params) {
            tracing::warn!(%error, "插件会话目录变化通知投递失败");
        }
    }

    /// 非持久化实时助手流帧。返回 `None` 表示当前没有运行中的共享宿主；
    /// 返回 `Some(Err(_))`（例如队列满或超帧）时，调用方应标记降级并停止续发跳号块。
    pub(crate) fn notify_assistant_stream(
        &self,
        session_id: &magi_core::SessionId,
        frame: Value,
    ) -> Option<Result<(), magi_extension_host::protocol::RpcError>> {
        let Ok(host) = self.require_host() else {
            return None;
        };
        Some(host.notify_service(
            "host.session.assistantStream",
            json!({
                "hostGeneration": host.generation(),
                "sessionId": session_id,
                "frame": frame,
            }),
        ))
    }

    fn observe_session(&self, session_id: &magi_core::SessionId, mut params: Value) {
        let Ok(host) = self.require_host() else {
            return;
        };
        params["hostGeneration"] = json!(host.generation());
        params["sessionId"] = json!(session_id);
        let result = host.notify_service("host.session.committed", params);
        if let Err(error) = result {
            tracing::warn!(%session_id, %error, "插件会话观察事件投递失败");
        }
    }

    fn execute_model_tool_blocking(
        &self,
        handle: &tokio::runtime::Handle,
        model_tool_name: &str,
        arguments: &str,
        context: &ToolExecutionContext,
        cancelled: &AtomicBool,
    ) -> (String, ExecutionResultStatus) {
        use ExecutionResultStatus::{Cancelled, Failed, Succeeded};
        let failure = |status, code: &str, message: &str| {
            (
                json!({
                    "tool": model_tool_name,
                    "status": match status {
                        Cancelled => "cancelled",
                        _ => "failed",
                    },
                    "error_code": code,
                    "error": message,
                })
                .to_string(),
                status,
            )
        };
        if cancelled.load(Ordering::Acquire) {
            return failure(Cancelled, "tool_cancelled", "调用已取消，工具未执行");
        }
        let Some((dsh_name, timeout_ms)) = ({
            let tools = self.tools.read().expect("tools lock");
            tools.iter().find_map(|tool| {
                let plugin_id = tool.plugin_id.as_deref().unwrap_or("dsh.shared");
                let candidate = if is_valid_wire_tool_name(&tool.name) {
                    tool.name.clone()
                } else {
                    magi_plugin_system::model_tool_name(plugin_id, &tool.name)
                };
                (candidate == model_tool_name).then(|| (tool.name.clone(), tool.timeout_ms))
            })
        }) else {
            return failure(Failed, "plugin_tool_unavailable", "插件工具未注册或已卸载");
        };
        let args: Value = match serde_json::from_str::<Value>(arguments) {
            Ok(v) if v.is_object() => v,
            Ok(_) => {
                return failure(
                    ExecutionResultStatus::Rejected,
                    "invalid_tool_arguments",
                    "工具参数必须是 JSON 对象",
                );
            }
            Err(e) => {
                return failure(
                    ExecutionResultStatus::Rejected,
                    "invalid_tool_arguments",
                    &format!("工具参数 JSON 无效：{e}"),
                );
            }
        };
        let Ok(host) = self.require_host() else {
            return failure(Failed, "plugin_host_unavailable", "共享插件宿主未运行");
        };
        let timeout = timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(TOOL_CALL_TIMEOUT);
        let session_id = context.session_id.as_ref().map(ToString::to_string);
        let call_result = handle.block_on(async {
            tokio::select! {
                res = host.call_cancellable_service(
                    "host.tools.execute",
                    json!({
                        "name": dsh_name,
                        "args": args,
                        "sessionId": session_id,
                    }),
                    timeout,
                ) => Some(res),
                _ = wait_until_cancelled(cancelled) => None,
            }
        });
        let Some(call_result) = call_result else {
            return failure(Cancelled, "tool_cancelled", "调用已取消");
        };
        match call_result {
            Ok(outcome) => {
                let is_error = outcome
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if is_error {
                    let message = outcome
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("插件工具返回错误");
                    let code = outcome
                        .pointer("/error/code")
                        .and_then(Value::as_str)
                        .unwrap_or("plugin_tool_error");
                    return failure(Failed, code, message);
                }
                let content_blocks = outcome.get("content").cloned().unwrap_or_else(|| json!([]));
                let text_summary = content_blocks
                    .as_array()
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|b| {
                                (b.get("type").and_then(Value::as_str) == Some("text"))
                                    .then(|| b.get("text").and_then(Value::as_str))
                                    .flatten()
                            })
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    })
                    .unwrap_or_default();
                let mut payload = json!({
                    "tool": model_tool_name,
                    "status": "succeeded",
                    "output": outcome.get("value").cloned().unwrap_or(Value::String(text_summary.clone())),
                    "content": content_blocks,
                });
                if !text_summary.is_empty() {
                    payload["summary"] = json!(text_summary);
                }
                (payload.to_string(), Succeeded)
            }
            Err(error) => failure(Failed, "plugin_tool_rpc_error", &error.to_string()),
        }
    }

    fn execute_web_blocking(
        &self,
        handle: &tokio::runtime::Handle,
        tool: &str,
        input: &str,
        _context: &ToolExecutionContext,
        cancelled: &AtomicBool,
    ) -> Option<String> {
        let snapshot = self.web.read().expect("web lock").clone();
        let is_search = match tool {
            "web_search" => true,
            "web_fetch" => false,
            _ => return None,
        };
        let has_plugin_provider = if is_search {
            match snapshot
                .search_provider
                .as_deref()
                .filter(|s| !s.is_empty())
            {
                Some(selected) => snapshot.search.iter().any(|p| p.id == selected),
                None => snapshot.search.len() == 1,
            }
        } else {
            match snapshot.fetch_provider.as_deref().filter(|s| !s.is_empty()) {
                Some(selected) => snapshot.fetch.iter().any(|p| p.id == selected),
                None => snapshot.fetch.len() == 1,
            }
        };
        if !has_plugin_provider {
            return None;
        }
        let parsed: Value = match serde_json::from_str::<Value>(input) {
            Ok(v) if v.is_object() => v,
            _ => {
                return Some(
                    json!({
                        "tool": tool,
                        "status": "failed",
                        "error_code": "invalid_input",
                        "error": "联网工具参数必须是 JSON 对象",
                    })
                    .to_string(),
                );
            }
        };
        let Ok(host) = self.require_host() else {
            return None;
        };
        if cancelled.load(Ordering::Acquire) {
            return Some(
                json!({
                    "tool": tool,
                    "status": "cancelled",
                    "error_code": "tool_cancelled",
                    "error": "调用已取消",
                })
                .to_string(),
            );
        }
        if is_search {
            let Some(query) = parsed
                .get("query")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|q| !q.is_empty())
            else {
                return Some(
                    json!({
                        "tool": "web_search",
                        "status": "failed",
                        "error_code": "invalid_input",
                        "error": "缺少搜索关键词 query",
                    })
                    .to_string(),
                );
            };
            let selected_id = snapshot
                .search_provider
                .clone()
                .or_else(|| snapshot.search.first().map(|p| p.id.clone()))
                .unwrap_or_else(|| "dsh-web".into());
            let outcome = handle.block_on(async {
                tokio::select! {
                    res = host.call_cancellable_service("host.web.search", json!({ "query": query }), WEB_SEARCH_TIMEOUT) => Some(res),
                    _ = wait_until_cancelled(cancelled) => None,
                }
            });
            let Some(outcome) = outcome else {
                return Some(
                    json!({
                        "tool": "web_search",
                        "status": "cancelled",
                        "error_code": "tool_cancelled",
                        "error": "搜索已取消",
                    })
                    .to_string(),
                );
            };
            match outcome {
                Ok(val) => {
                    let results = val
                        .get("sources")
                        .and_then(Value::as_array)
                        .map(|arr| {
                            arr.iter()
                                .map(|item| {
                                    let mut mapped = json!({
                                        "title": item.get("title").and_then(Value::as_str).unwrap_or(""),
                                        "url": item.get("url").and_then(Value::as_str).unwrap_or(""),
                                        "snippet": item.get("snippet").and_then(Value::as_str).unwrap_or(""),
                                    });
                                    if let Some(pub_at) =
                                        item.get("publishedAt").and_then(Value::as_str)
                                    {
                                        mapped["published"] = json!(pub_at);
                                    }
                                    mapped
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let mut payload = json!({
                        "tool": "web_search",
                        "status": "succeeded",
                        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
                        "query": query,
                        "source": selected_id,
                        "result_count": results.len(),
                        "results": results,
                        "summary": format!("搜索 \"{query}\" 返回 {} 条结果", results.len()),
                    });
                    if let Some(answer) = val
                        .get("answer")
                        .and_then(Value::as_str)
                        .filter(|a| !a.is_empty())
                    {
                        payload["answer"] = json!(answer);
                    }
                    Some(payload.to_string())
                }
                Err(error) => Some(
                    json!({
                        "tool": "web_search",
                        "status": "failed",
                        "error_code": "web_search_unavailable",
                        "error": format!("联网搜索失败：{error}"),
                    })
                    .to_string(),
                ),
            }
        } else {
            let Some(url) = parsed
                .get("url")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|u| !u.is_empty())
            else {
                return Some(
                    json!({
                        "tool": "web_fetch",
                        "status": "failed",
                        "error_code": "invalid_input",
                        "error": "缺少 URL",
                    })
                    .to_string(),
                );
            };
            let outcome = handle.block_on(async {
                tokio::select! {
                    res = host.call_cancellable_service("host.web.fetch", json!({ "url": url }), WEB_FETCH_TIMEOUT) => Some(res),
                    _ = wait_until_cancelled(cancelled) => None,
                }
            });
            let Some(outcome) = outcome else {
                return Some(
                    json!({
                        "tool": "web_fetch",
                        "status": "cancelled",
                        "error_code": "tool_cancelled",
                        "error": "网页抓取已取消",
                    })
                    .to_string(),
                );
            };
            match outcome {
                Ok(val) => {
                    let final_url = val
                        .get("finalUrl")
                        .or_else(|| val.get("url"))
                        .and_then(Value::as_str)
                        .unwrap_or(url);
                    let body_kind = val.pointer("/body/kind").and_then(Value::as_str);
                    let content_type = val
                        .get("contentType")
                        .and_then(Value::as_str)
                        .or(match body_kind {
                            Some("html") => Some("text/html"),
                            Some("text") => Some("text/plain"),
                            _ => None,
                        })
                        .unwrap_or("text/markdown");
                    let content = val
                        .pointer("/body/content")
                        .or_else(|| val.get("content"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let truncated = val
                        .get("truncated")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    Some(
                        json!({
                            "tool": "web_fetch",
                            "status": "succeeded",
                            "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
                            "url": final_url,
                            "content_type": content_type,
                            "content_length": content.len(),
                            "truncated": truncated,
                            "content": content,
                            "summary": format!("已获取 {final_url} ({} 字符)", content.len()),
                        })
                        .to_string(),
                    )
                }
                Err(error) => Some(
                    json!({
                        "tool": "web_fetch",
                        "status": "failed",
                        "error_code": "network_error",
                        "error": format!("网页获取失败：{error}"),
                    })
                    .to_string(),
                ),
            }
        }
    }

    fn handle_notification(&self, method: &str, params: &Value) -> bool {
        match method {
            "host.tools.changed" => {
                if let Some(arr) = params.get("tools")
                    && let Ok(parsed) =
                        serde_json::from_value::<Vec<DshToolRegistration>>(arr.clone())
                {
                    *self.tools.write().expect("tools lock") = parsed;
                    self.publish_registry_changed();
                }
                true
            }
            "host.web.changed" => {
                let snapshot = DshWebSnapshot {
                    search_provider: params
                        .get("searchProvider")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    fetch_provider: params
                        .get("fetchProvider")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    search: params
                        .get("search")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default(),
                    fetch: params
                        .get("fetch")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default(),
                };
                *self.web.write().expect("web lock") = snapshot;
                self.publish_registry_changed();
                true
            }
            "host.commands.changed" => {
                if let Some(arr) = params.get("commands")
                    && let Ok(parsed) =
                        serde_json::from_value::<Vec<DshCommandRegistration>>(arr.clone())
                {
                    *self.commands.write().expect("commands lock") = parsed;
                    self.publish_registry_changed();
                }
                true
            }
            "host.skills.changed" => {
                if let Some(arr) = params.get("skills").and_then(Value::as_array) {
                    *self.skills.write().expect("skills lock") = arr.clone();
                    self.publish_registry_changed();
                }
                true
            }
            "host.llm.providersChanged" => {
                if let Some(arr) = params.get("providers")
                    && let Ok(providers) =
                        serde_json::from_value::<Vec<LlmProviderRegistration>>(arr.clone())
                {
                    let next_entries = providers
                        .into_iter()
                        .map(|prov| {
                            let input = json!({
                                "baseUrl": prov.base_url,
                                "apiKey": prov.api_key,
                                "apiProtocol": prov.api_protocol,
                                "models": prov.models,
                            });
                            let display_name = if prov.name.trim().is_empty() {
                                prov.provider.clone()
                            } else {
                                prov.name
                            };
                            (prov.plugin_id, prov.provider, display_name, input)
                        })
                        .collect::<Vec<_>>();
                    *self
                        .latest_llm_providers
                        .write()
                        .expect("llm providers lock") = next_entries.clone();
                    if let Some(store) = self.settings_store.get() {
                        self.apply_llm_sources_to_store(store, &next_entries);
                    }
                    self.publish_registry_changed();
                }
                true
            }
            "host.settings.hostChanged" => {
                if let Some(ns) = params.get("ns").and_then(Value::as_str)
                    && let Some(cfg) = params.get("config")
                {
                    let mut guard = self
                        .persisted_host_settings
                        .lock()
                        .expect("persisted host settings lock");
                    match ns {
                        "web" => guard.web = Some(cfg.clone()),
                        "agentDefaultModel" => guard.agent_default_model = Some(cfg.clone()),
                        _ => {}
                    }
                    let _ = save_persisted_host_settings(&self.host_settings_path, &guard);
                }
                true
            }
            "host.settings.updated" => {
                self.publish_registry_changed();
                true
            }
            "host.hooks.capabilities" => {
                if let Some(caps) = params.get("capabilities")
                    && let Ok(parsed) = serde_json::from_value::<HookCapabilities>(caps.clone())
                {
                    *self
                        .hook_capabilities
                        .write()
                        .expect("hook capabilities lock") = parsed;
                }
                true
            }
            "host.agentLoop.changed" => {
                if let Ok(parsed) = serde_json::from_value::<DshAgentLoopSnapshot>(params.clone()) {
                    *self.agent_loop.write().expect("agent loop lock") = parsed;
                    self.publish_registry_changed();
                }
                true
            }
            _ => false,
        }
    }

    fn apply_llm_sources_to_store(
        &self,
        store: &SettingsStore,
        entries: &[(String, String, String, Value)],
    ) {
        let mut next_map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (plugin_id, provider_id, name, input) in entries {
            if main_model_sources::set_plugin_source(store, plugin_id, provider_id, name, input)
                .is_ok()
            {
                next_map
                    .entry(plugin_id.clone())
                    .or_default()
                    .insert(provider_id.clone());
            }
        }
        let mut prev_guard = self
            .active_llm_sources
            .lock()
            .expect("active llm sources lock");
        for (old_plugin, old_providers) in prev_guard.iter() {
            match next_map.get(old_plugin) {
                None => main_model_sources::clear_plugin_sources(store, old_plugin),
                Some(current_providers) => {
                    for old_provider in old_providers {
                        if !current_providers.contains(old_provider) {
                            main_model_sources::clear_plugin_source(
                                store,
                                old_plugin,
                                old_provider,
                            );
                        }
                    }
                }
            }
        }
        *prev_guard = next_map;
    }

    fn publish_registry_changed(&self) {
        if let Some(events) = self.event_bus.get() {
            events.publish(EventEnvelope::domain(
                EventId::unique("plugin-runtime-catalog"),
                "plugin.registry.changed",
                json!({}),
            ));
        }
    }

    async fn invoke_host_llm(&self, params: Value) -> Result<Value, RpcError> {
        let request: HostLlmInvokeRequest = serde_json::from_value(params)
            .map_err(|e| RpcError::new(ErrorKind::InvalidParams, e.to_string()))?;
        let custom_invoker = self.llm_invoker.read().expect("llm invoker lock").clone();
        if let Some(invoker) = custom_invoker {
            let response = invoker(request)
                .await
                .map_err(|e| RpcError::new(ErrorKind::Internal, e))?;
            return serde_json::to_value(response)
                .map_err(|e| RpcError::new(ErrorKind::Internal, e.to_string()));
        }
        let store = self
            .settings_store
            .get()
            .cloned()
            .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "主对话模型设置尚未绑定"))?;
        tokio::task::spawn_blocking(move || invoke_via_settings_store(&store, request))
            .await
            .map_err(|e| RpcError::new(ErrorKind::Internal, e.to_string()))?
            .map_err(|e| RpcError::new(ErrorKind::Conflict, e))
    }

    async fn handle_user_question(&self, params: Value) -> Result<Value, RpcError> {
        let handler = self
            .question_handler
            .read()
            .expect("user question handler lock")
            .clone();
        let Some(handler) = handler else {
            return Err(RpcError::new(
                ErrorKind::HostGone,
                "当前没有活跃的用户提问交互提供方",
            ));
        };
        handler(params)
            .await
            .map_err(|e| RpcError::new(ErrorKind::Conflict, e))
    }
}

pub struct PluginServicesIncoming {
    bridge: Arc<PluginServicesBridge>,
    host_generation: u64,
    delegate: Arc<dyn Incoming>,
}

impl PluginServicesIncoming {
    pub fn new(
        bridge: Arc<PluginServicesBridge>,
        host_generation: u64,
        delegate: Arc<dyn Incoming>,
    ) -> Arc<Self> {
        Arc::new(Self {
            bridge,
            host_generation,
            delegate,
        })
    }

    fn generation_matches(&self, params: &Value) -> bool {
        params
            .get("hostGeneration")
            .and_then(Value::as_u64)
            .is_none_or(|g| g == self.host_generation)
    }
}

impl Incoming for PluginServicesIncoming {
    fn request(&self, method: String, params: Value) -> BoxFuture<Result<Value, RpcError>> {
        match method.as_str() {
            "host.llm.invoke" => {
                let bridge = Arc::clone(&self.bridge);
                Box::pin(async move { bridge.invoke_host_llm(params).await })
            }
            "host.userQuestions.ask" => {
                let bridge = Arc::clone(&self.bridge);
                Box::pin(async move { bridge.handle_user_question(params).await })
            }
            "host.settings.openDocument" => Box::pin(async { Ok(json!({ "opened": true })) }),
            _ => self.delegate.request(method, params),
        }
    }

    fn notification(&self, method: String, params: Value) {
        if method.starts_with("host.")
            && method != "host.diagnostic"
            && method != "plugin.status"
            && self.generation_matches(&params)
            && self.bridge.handle_notification(&method, &params)
        {
            return;
        }
        self.delegate.notification(method, params);
    }
}

struct SharedDshExtensionHooks {
    bridge: Arc<PluginServicesBridge>,
    handle: tokio::runtime::Handle,
}

impl magi_conversation_runtime::extension_hooks::ExtensionHooks for SharedDshExtensionHooks {
    fn compaction_before(
        &self,
        view: &magi_conversation_runtime::extension_hooks::CompactionView<'_>,
    ) -> magi_conversation_runtime::extension_hooks::CompactionDecision {
        use magi_conversation_runtime::extension_hooks::CompactionDecision;
        if !self.bridge.hook_capabilities_snapshot().compaction_before {
            return CompactionDecision::Proceed;
        }
        let Ok(host) = self.bridge.require_host() else {
            return CompactionDecision::Proceed;
        };
        let payload = json!({
            "sessionId": view.session_id,
            "reason": view.reason,
            "compactedMessages": view.compacted_messages,
            "originalTokens": view.original_tokens,
            "summaryBudgetTokens": view.summary_budget_tokens,
            "source": view.source,
        });
        let Ok(value) = self.handle.block_on(host.call_cancellable_service(
            "host.compaction.before",
            payload,
            HOOK_CALL_TIMEOUT,
        )) else {
            return CompactionDecision::Proceed;
        };
        match value.get("kind").and_then(Value::as_str) {
            Some("cancel") => CompactionDecision::Cancel {
                plugin: value
                    .get("plugin")
                    .and_then(Value::as_str)
                    .unwrap_or("dsh-plugin")
                    .to_string(),
                reason: value
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("cancelled by plugin")
                    .to_string(),
            },
            Some("summary") => match value.get("summary").and_then(Value::as_str) {
                Some(summary) if !summary.trim().is_empty() => CompactionDecision::Summary {
                    plugin: value
                        .get("plugin")
                        .and_then(Value::as_str)
                        .unwrap_or("dsh-plugin")
                        .to_string(),
                    summary: summary.to_string(),
                },
                _ => CompactionDecision::Proceed,
            },
            _ => CompactionDecision::Proceed,
        }
    }

    fn system_prompt_assemble(
        &self,
        scope: &magi_conversation_runtime::extension_hooks::ToolHookScope<'_>,
        base_prompt: &str,
    ) -> Option<magi_conversation_runtime::extension_hooks::SystemPromptOverride> {
        if !self
            .bridge
            .hook_capabilities_snapshot()
            .system_prompt_assemble
        {
            return None;
        }
        let host = self.bridge.require_host().ok()?;
        let value = self
            .handle
            .block_on(host.call_cancellable_service(
                "host.systemPrompt.assemble",
                json!({
                    "sessionId": scope.session_id,
                    "basePrompt": base_prompt,
                    "round": scope.round,
                }),
                HOOK_CALL_TIMEOUT,
            ))
            .ok()?;
        let text = value
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())?
            .to_string();
        let complete = value
            .get("complete")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Some(magi_conversation_runtime::extension_hooks::SystemPromptOverride { complete, text })
    }

    fn approval_decide(
        &self,
        scope: &magi_conversation_runtime::extension_hooks::ToolHookScope<'_>,
        request: &magi_conversation_runtime::extension_hooks::ApprovalHookRequest<'_>,
    ) -> magi_conversation_runtime::extension_hooks::ApprovalHookDecision {
        use magi_conversation_runtime::extension_hooks::ApprovalHookDecision;
        if !self.bridge.hook_capabilities_snapshot().approval {
            return ApprovalHookDecision::Defer;
        }
        let Ok(host) = self.bridge.require_host() else {
            return ApprovalHookDecision::Defer;
        };
        let Ok(value) = self.handle.block_on(host.call_cancellable_service(
            "host.approval.ask",
            json!({
                "sessionId": scope.session_id,
                "callId": request.call_id,
                "toolName": request.tool_name,
                "reason": request.reason,
            }),
            HOOK_CALL_TIMEOUT,
        )) else {
            return ApprovalHookDecision::Defer;
        };
        match value.get("outcome").and_then(Value::as_str) {
            Some("allowed-once") => ApprovalHookDecision::Allow {
                plugin: "dsh-approval".to_string(),
            },
            Some("rejected") => ApprovalHookDecision::Deny {
                plugin: "dsh-approval".to_string(),
                reason: request.reason.to_string(),
            },
            Some("cancelled") => ApprovalHookDecision::Deny {
                plugin: "dsh-approval".to_string(),
                reason: "审批已取消".to_string(),
            },
            _ => ApprovalHookDecision::Defer,
        }
    }

    fn filter_visible_tools(
        &self,
        scope: &magi_conversation_runtime::extension_hooks::ToolHookScope<'_>,
        names: Vec<String>,
    ) -> Vec<String> {
        if names.is_empty() || !self.bridge.hook_capabilities_snapshot().tools_restrict {
            return names;
        }
        let Ok(host) = self.bridge.require_host() else {
            return names;
        };
        let Ok(value) = self.handle.block_on(host.call_service(
            "host.tools.filterVisible",
            json!({
                "sessionId": scope.session_id,
                "names": &names,
            }),
            Duration::from_secs(10),
        )) else {
            return names;
        };
        let Some(allowed) = value.get("names").and_then(Value::as_array) else {
            return names;
        };
        let allowed_set: BTreeSet<&str> = allowed.iter().filter_map(Value::as_str).collect();
        names
            .into_iter()
            .filter(|name| allowed_set.contains(name.as_str()))
            .collect()
    }

    fn project_request_messages(
        &self,
        scope: &magi_conversation_runtime::extension_hooks::ToolHookScope<'_>,
        messages: &[ChatMessage],
    ) -> Option<Vec<ChatMessage>> {
        if !self.bridge.hook_capabilities_snapshot().context_project || messages.is_empty() {
            return None;
        }
        let host = self.bridge.require_host().ok()?;
        let wire_messages: Vec<Value> = messages
            .iter()
            .enumerate()
            .map(|(index, msg)| {
                json!({
                    "index": index,
                    "role": msg.role,
                    "content": msg.content.as_deref().unwrap_or_default(),
                })
            })
            .collect();
        let value = self
            .handle
            .block_on(host.call_cancellable_service(
                "host.session.projectMessages",
                json!({
                    "sessionId": scope.session_id,
                    "messages": wire_messages,
                }),
                HOOK_CALL_TIMEOUT,
            ))
            .ok()?;
        let returned = value.get("messages").and_then(Value::as_array)?;
        let mut next = Vec::with_capacity(returned.len());
        let mut changed = returned.len() != messages.len();
        for item in returned {
            let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
            let content = item
                .get("content")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            if let Some(orig_idx) = item
                .get("index")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                && let Some(orig) = messages.get(orig_idx)
            {
                let mut cloned = orig.clone();
                if cloned.content != content {
                    changed = true;
                    cloned.content = content;
                }
                next.push(cloned);
            } else {
                changed = true;
                next.push(ChatMessage {
                    input_request_id: None,
                    context_origin: magi_bridge_client::ChatMessageOrigin::ContextReference,
                    role: role.to_string(),
                    content,
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: Vec::new(),
                });
            }
        }
        changed.then_some(next)
    }

    fn extend_llm_request(
        &self,
        scope: &magi_conversation_runtime::extension_hooks::ToolHookScope<'_>,
        view: &magi_conversation_runtime::extension_hooks::LlmRequestExtendView<'_>,
    ) -> Option<magi_conversation_runtime::extension_hooks::LlmRequestExtension> {
        if !self.bridge.hook_capabilities_snapshot().llm_extend {
            return None;
        }
        let host = self.bridge.require_host().ok()?;
        let value = self
            .handle
            .block_on(host.call_cancellable_service(
                "host.llm.extendRequest",
                json!({
                    "sessionId": scope.session_id,
                    "request": {
                        "provider": view.provider,
                        "model": view.model,
                        "maxOutputTokens": view.max_output_tokens,
                        "messageCount": view.message_count,
                        "toolCount": view.tool_count,
                    },
                }),
                HOOK_CALL_TIMEOUT,
            ))
            .ok()?;
        let req = value.get("request")?;
        let max_output_tokens = req
            .get("maxOutputTokens")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| Some(*v) != view.max_output_tokens);
        let append_user_messages: Vec<String> = req
            .get("appendUserMessages")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if max_output_tokens.is_none() && append_user_messages.is_empty() {
            return None;
        }
        Some(
            magi_conversation_runtime::extension_hooks::LlmRequestExtension {
                max_output_tokens,
                append_user_messages,
            },
        )
    }

    fn intercept_llm_stream(
        &self,
        scope: &magi_conversation_runtime::extension_hooks::ToolHookScope<'_>,
        view: &magi_conversation_runtime::extension_hooks::LlmStreamInterceptView<'_>,
    ) -> magi_conversation_runtime::extension_hooks::LlmStreamInterceptDecision {
        use magi_conversation_runtime::extension_hooks::LlmStreamInterceptDecision;
        if !self
            .bridge
            .hook_capabilities_snapshot()
            .llm_stream_intercept
        {
            return LlmStreamInterceptDecision::Accept;
        }
        let Ok(host) = self.bridge.require_host() else {
            return LlmStreamInterceptDecision::Accept;
        };
        let Ok(value) = self.handle.block_on(host.call_cancellable_service(
            "host.llm.interceptStream",
            json!({
                "sessionId": scope.session_id,
                "provider": view.provider,
                "model": view.model,
                "attempt": view.attempt,
                "content": view.content,
                "reasoningContent": view.reasoning_content,
                "toolCallNames": view.tool_call_names,
                "finishReason": view.finish_reason,
            }),
            HOOK_CALL_TIMEOUT,
        )) else {
            return LlmStreamInterceptDecision::Accept;
        };
        match value.get("action").and_then(Value::as_str) {
            Some("replace") => match value.get("content").and_then(Value::as_str) {
                Some(content) => LlmStreamInterceptDecision::Replace {
                    content: content.to_string(),
                },
                None => LlmStreamInterceptDecision::Accept,
            },
            Some("retry") => LlmStreamInterceptDecision::Retry {
                reason: value
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                append_user_message: value
                    .get("appendUserMessage")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            },
            _ => LlmStreamInterceptDecision::Accept,
        }
    }

    fn has_selected_plugin_agent_loop(&self) -> bool {
        let selected = self.bridge.agent_loop_snapshot().selected;
        !selected.is_empty() && selected != "magi-native"
    }

    fn dispatch_agent_loop(
        &self,
        request: &magi_conversation_runtime::extension_hooks::PluginAgentLoopRequest<'_>,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> magi_conversation_runtime::extension_hooks::PluginAgentLoopOutcome {
        use magi_conversation_runtime::extension_hooks::PluginAgentLoopOutcome;
        let selected = self.bridge.agent_loop_snapshot().selected;
        if selected.is_empty() || selected == "magi-native" {
            return PluginAgentLoopOutcome::Native;
        }
        let session_id = request.session_id.to_string();
        let input = json!({
            "turnId": request.turn_id,
            "taskId": request.task_id,
            "text": request.prompt,
        });
        let bridge = Arc::clone(&self.bridge);
        let result = self.handle.block_on(async move {
            let run_fut = bridge.run_agent_loop(&session_id, input);
            tokio::pin!(run_fut);
            let mut ticker = tokio::time::interval(Duration::from_millis(25));
            loop {
                tokio::select! {
                    res = &mut run_fut => break res,
                    _ = ticker.tick() => {
                        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                            let _ = bridge.cancel_agent_loop(&session_id).await;
                            return Err(ApiError::Conflict("插件执行循环已取消".into()));
                        }
                    }
                }
            }
        });
        match result {
            Ok(value) => {
                let output = value
                    .get("output")
                    .or_else(|| value.get("text"))
                    .or_else(|| value.get("content"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| {
                        if let Some(s) = value.as_str() {
                            s.to_string()
                        } else {
                            value.to_string()
                        }
                    });
                PluginAgentLoopOutcome::Completed { output }
            }
            Err(error) => PluginAgentLoopOutcome::Failed {
                error: format!("插件执行循环 ({selected}) 执行失败：{error:?}"),
            },
        }
    }

    fn tool_call(
        &self,
        scope: &magi_conversation_runtime::extension_hooks::ToolHookScope<'_>,
        call: &ChatToolCall,
    ) -> magi_conversation_runtime::extension_hooks::ToolCallDecision {
        use magi_conversation_runtime::extension_hooks::ToolCallDecision;
        if !self.bridge.hook_capabilities_snapshot().tools_pre_execute {
            return ToolCallDecision::Proceed {
                arguments: None,
                rewritten_by: Vec::new(),
            };
        }
        let Ok(host) = self.bridge.require_host() else {
            return ToolCallDecision::Proceed {
                arguments: None,
                rewritten_by: Vec::new(),
            };
        };
        let parsed_args =
            serde_json::from_str::<Value>(&call.function.arguments).unwrap_or_else(|_| json!({}));
        let Ok(value) = self.handle.block_on(host.call_cancellable_service(
            "host.tools.preExecute",
            json!({
                "sessionId": scope.session_id,
                "callId": call.id,
                "name": call.function.name,
                "arguments": parsed_args,
            }),
            HOOK_CALL_TIMEOUT,
        )) else {
            return ToolCallDecision::Proceed {
                arguments: None,
                rewritten_by: Vec::new(),
            };
        };
        let rewritten_args = value
            .get("arguments")
            .filter(|v| v.is_object())
            .and_then(|v| serde_json::to_string(v).ok());
        let rewritten_by = if rewritten_args.is_some() {
            vec!["dsh-tools".to_string()]
        } else {
            Vec::new()
        };
        match value.get("kind").and_then(Value::as_str) {
            Some("deny") => ToolCallDecision::Blocked {
                plugin: "dsh-tools".to_string(),
                reason: value
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("Denied by tools/pre-execute")
                    .to_string(),
            },
            Some("cancel") => ToolCallDecision::Blocked {
                plugin: "dsh-tools".to_string(),
                reason: "调用已被插件取消".to_string(),
            },
            Some("ask") => ToolCallDecision::RequireApproval {
                plugin: "dsh-tools".to_string(),
                reason: value
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("Requires approval")
                    .to_string(),
            },
            Some("allow")
                if value
                    .get("approvedByPlugin")
                    .and_then(Value::as_bool)
                    .unwrap_or(false) =>
            {
                ToolCallDecision::AllowByPlugin {
                    plugin: "dsh-approval".to_string(),
                    arguments: rewritten_args,
                    rewritten_by,
                }
            }
            _ => ToolCallDecision::Proceed {
                arguments: rewritten_args,
                rewritten_by,
            },
        }
    }

    fn tool_result(
        &self,
        scope: &magi_conversation_runtime::extension_hooks::ToolHookScope<'_>,
        call: &ChatToolCall,
        view: magi_conversation_runtime::extension_hooks::ToolResultView<'_>,
    ) -> Option<String> {
        if !self.bridge.hook_capabilities_snapshot().tools_post_execute {
            return None;
        }
        let host = self.bridge.require_host().ok()?;
        let parsed_args =
            serde_json::from_str::<Value>(&call.function.arguments).unwrap_or_else(|_| json!({}));
        let is_error = view.status != "succeeded";
        let value = self
            .handle
            .block_on(host.call_cancellable_service(
                "host.tools.postExecute",
                json!({
                    "sessionId": scope.session_id,
                    "callId": call.id,
                    "name": call.function.name,
                    "arguments": parsed_args,
                    "isError": is_error,
                    "status": view.status,
                    "content": view.projection,
                }),
                HOOK_CALL_TIMEOUT,
            ))
            .ok()?;
        match value.get("kind").and_then(Value::as_str) {
            Some("block") => {
                let feedback = value
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                    .unwrap_or("工具结果已被插件拦截");
                Some(
                    json!({
                        "tool": call.function.name,
                        "status": "rejected",
                        "error_code": "extension_post_execute_blocked",
                        "error": feedback,
                    })
                    .to_string(),
                )
            }
            Some("accept") => value
                .get("text")
                .and_then(Value::as_str)
                .filter(|t| *t != view.projection)
                .map(ToOwned::to_owned),
            _ => None,
        }
    }
}

struct SharedDshCommandExecutor {
    bridge: Arc<PluginServicesBridge>,
    handle: tokio::runtime::Handle,
}

impl PluginCommandExecutor for SharedDshCommandExecutor {
    fn invoke(
        &self,
        invocation: PluginCommandInvocation,
        cancelled: &AtomicBool,
    ) -> Result<PluginCommandResult, String> {
        if cancelled.load(Ordering::Acquire) {
            return Err("调用已取消，命令未执行".into());
        }
        let session_id = invocation
            .context
            .session_id
            .as_ref()
            .ok_or_else(|| "插件命令必须由已有会话接纳".to_string())?;
        let command = magi_plugin_system::ContributionRef::parse("命令", &invocation.command_id)?;
        let command_name = &command.contribution_id;
        let line = if invocation.input.trim().is_empty() {
            format!("/{command_name}")
        } else {
            format!("/{command_name} {}", invocation.input)
        };
        let host = self
            .bridge
            .require_host()
            .map_err(|e| e.message().to_owned())?;
        let timeout = invocation
            .job
            .as_ref()
            .map(|job| Duration::from_millis(job.budget.timeout_ms as u64))
            .unwrap_or(COMMAND_TIMEOUT);
        let outcome = self.handle.block_on(async {
            tokio::select! {
                res = host.call_cancellable_service(
                    "host.commands.execute",
                    json!({
                        "sessionId": session_id,
                        "line": line,
                        "expectedOwner": { "pluginId": command.plugin_id, "name": command_name },
                    }),
                    timeout,
                ) => Some(res),
                _ = wait_until_cancelled(cancelled) => None,
            }
        });
        let Some(outcome) = outcome else {
            return Err("调用已取消".into());
        };
        let value = outcome.map_err(|e| e.to_string())?;
        let result_obj = value.get("result").unwrap_or(&value);
        let kind = result_obj
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("success");
        let text = result_obj
            .get("text")
            .or_else(|| result_obj.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if kind == "error" || kind == "failed" {
            return Err(if text.is_empty() {
                "插件命令执行失败".into()
            } else {
                text
            });
        }
        Ok(PluginCommandResult { content: text })
    }
}

fn invoke_via_settings_store(
    store: &SettingsStore,
    req: HostLlmInvokeRequest,
) -> Result<Value, String> {
    let snapshot = store.execution_snapshot();
    let config = if req.provider.starts_with("custom/") || req.provider.starts_with("plugin/") {
        let effective = main_model_sources::effective_config(
            &snapshot,
            &json!({
                "sourceId": req.provider,
                "model": req.model,
                "reasoningEffort": req.reasoning_effort,
            }),
        )?;
        NormalizedModelConfig::from_settings_with_catalog(&snapshot, &effective)?
    } else {
        resolve_orchestrator_model_config(&snapshot, None)?
    };
    let effort = req
        .reasoning_effort
        .as_deref()
        .and_then(|raw| serde_json::from_value::<ReasoningEffort>(json!(raw)).ok());
    let stop = req.stop.unwrap_or_default();
    let client = config
        .to_http_model_client()
        .ok_or_else(|| "默认模型连接未配置或不完整".to_string())?
        .with_generation_controls(req.temperature, stop, effort)?;
    let messages = req
        .messages
        .into_iter()
        .map(|m| {
            serde_json::from_value::<ChatMessage>(json!({
                "role": m.role,
                "content": m.content,
                "images": m.images,
                "tool_calls": m.tool_calls,
                "tool_call_id": m.tool_call_id,
            }))
            .map_err(|e| format!("消息格式无效：{e}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let invocation_req = ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "plugin-dsh-host".into(),
        prompt: String::new(),
        messages: Some(messages),
        tools: None,
        tool_choice: None,
    };
    let started = std::time::Instant::now();
    let is_cancelled = || started.elapsed() > HOST_LLM_TIMEOUT;
    let response = client
        .invoke_observed(
            invocation_req,
            ModelInvocationOptions {
                on_delta: None,
                on_retry: &|_| {},
                is_cancelled: &is_cancelled,
                max_output_tokens: req.max_output_tokens,
            },
            &|_| Ok(()),
        )
        .map_err(|e| e.to_string())?;
    let serialized = serde_json::to_value(&response).map_err(|e| e.to_string())?;
    let content = serialized
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let reasoning = serialized
        .get("reasoning_content")
        .or_else(|| serialized.get("reasoning"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let tool_calls = serialized
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .filter_map(|c| {
                    let id = c.get("id")?.as_str()?;
                    let name = c.pointer("/function/name")?.as_str()?;
                    let arguments = c
                        .pointer("/function/arguments")
                        .cloned()
                        .unwrap_or_else(|| json!("{}"));
                    Some(json!({
                        "id": id,
                        "name": name,
                        "arguments": arguments,
                    }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut out = json!({
        "content": content,
        "toolCalls": tool_calls,
    });
    if let Some(r) = reasoning {
        out["reasoning"] = json!(r);
    }
    if let Some(usage) = serialized.get("usage") {
        let input_tokens = usage
            .get("prompt_tokens")
            .or_else(|| usage.get("input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output_tokens = usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        out["usage"] = json!({
            "inputTokens": input_tokens,
            "outputTokens": output_tokens,
        });
    }
    Ok(out)
}

async fn wait_until_cancelled(cancelled: &AtomicBool) {
    while !cancelled.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

fn is_valid_wire_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn load_persisted_host_settings(path: &Path) -> PersistedHostSettings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_persisted_host_settings(
    path: &Path,
    settings: &PersistedHostSettings,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
    magi_core::fs_atomic::write_atomic(path, bytes)
}
