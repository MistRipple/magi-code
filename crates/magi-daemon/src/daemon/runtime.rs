use super::{
    bootstrap::bootstrap_shadow_state,
    config::{DaemonConfig, DaemonError},
    events::publish_ledger_status_event,
    maintenance::{ShadowRuntimeMaintenance, ShadowRuntimeMaintenanceConfig},
    persistence::{ShadowRuntimeSidecarPersistence, ShadowStateRepository},
};
use magi_api::task_execution::ShadowTaskDispatcher;
use magi_api::{
    ApiState, DirectHttpModelProbeConfig, RunnerManager, RuntimeStatePersistence, SettingsStore,
    build_router,
};
use magi_bridge_client::{
    BridgeDispatchRuntime, BridgeServerKind, BridgeTransport, HttpModelBridgeClient,
    HttpModelBridgeProtocol, JsonRpcHostBridgeClient, JsonRpcMcpBridgeClient,
    JsonRpcStdioTransport, StdioMcpBridgeClient,
};
use magi_context_runtime::{ContextBudget, ContextRuntime};
use magi_core::EventId;
use magi_core::{LeaseId, TaskStatus};
use magi_event_bus::{EventEnvelope, InMemoryEventBus};
use magi_governance::GovernanceService;
use magi_knowledge_store::{
    KnowledgeStore,
    code_scanner::{ingest_workspace_code_index, ingest_workspace_code_index_in_workspace},
};
use magi_memory_store::MemoryStore;
use magi_orchestrator::task_runner::{EventBasedResultReceiver, TaskOutcome, TaskResult};
use magi_orchestrator::{ExecutionContextConfig, OrchestratorService, task_store::TaskStore};
use magi_session_store::SessionStore;
use magi_skill_runtime::SkillDispatchRuntime;
use magi_tool_runtime::ToolRegistry;
use magi_worker_runtime::WorkerRuntime;
use magi_workspace::WorkspaceStore;
use std::{collections::HashSet, env, path::PathBuf, sync::Arc};
use tracing::warn;

#[cfg(test)]
struct StaticTestModelBridgeClient;

#[cfg(test)]
impl magi_bridge_client::ModelBridgeClient for StaticTestModelBridgeClient {
    fn invoke(
        &self,
        request: magi_bridge_client::ModelInvocationRequest,
    ) -> Result<magi_bridge_client::BridgeResponse, magi_bridge_client::BridgeClientError> {
        if let Some(payload) = classifier_payload_for_prompt(&request.prompt) {
            return Ok(magi_bridge_client::BridgeResponse { ok: true, payload });
        }
        if request.prompt.contains("深度任务图") {
            return Ok(magi_bridge_client::BridgeResponse {
                ok: true,
                payload: serde_json::json!({
                    "phases": [
                        {
                            "title": "P1",
                            "workPackages": [
                                {
                                    "title": "WP1",
                                    "actions": [
                                        { "title": "A1", "goal": "g1", "dependsOn": [], "writeScope": null }
                                    ]
                                }
                            ]
                        },
                        {
                            "title": "P2",
                            "workPackages": [
                                {
                                    "title": "WP2",
                                    "actions": [
                                        { "title": "A2", "goal": "g2", "dependsOn": [], "writeScope": null }
                                    ]
                                }
                            ]
                        },
                        {
                            "title": "P3",
                            "workPackages": [
                                {
                                    "title": "WP3",
                                    "actions": [
                                        { "title": "A3", "goal": "g3", "dependsOn": [], "writeScope": null }
                                    ]
                                }
                            ]
                        }
                    ]
                })
                .to_string(),
            });
        }
        Ok(magi_bridge_client::BridgeResponse {
            ok: true,
            payload: format!("shadow-model::{}", request.prompt.trim()),
        })
    }

    fn invoke_streaming(
        &self,
        request: magi_bridge_client::ModelInvocationRequest,
        _on_delta: &dyn Fn(&magi_bridge_client::ModelStreamingDelta),
    ) -> Result<magi_bridge_client::BridgeResponse, magi_bridge_client::BridgeClientError> {
        self.invoke(request)
    }
}

#[cfg(test)]
fn classifier_payload_for_prompt(prompt: &str) -> Option<String> {
    if !prompt.contains("Session Turn 编排分类器") {
        return None;
    }
    let has_recoverable_chain = prompt
        .lines()
        .any(|line| line.trim() == "hasRecoverableChain=true");
    let user_text = prompt
        .lines()
        .find_map(|line| line.trim().strip_prefix("userText="))
        .unwrap_or("");
    let route = if has_recoverable_chain && user_text.contains("继续") {
        "continue"
    } else if prompt.contains("deepTask=true")
        || !prompt.contains("skillName=\"\"")
        || !prompt.contains("imageCount=0")
        || user_text.contains("复杂任务")
        || user_text.contains("分析并拆分")
    {
        "task"
    } else {
        "chat"
    };
    let arguments = serde_json::json!({
        "route": route,
        "taskTitle": (route == "task").then_some("模型判定任务"),
        "executionGoal": (route == "task").then_some(user_text.trim_matches('"')),
        "requiredWorkers": [],
        "toolIntent": null,
    });
    Some(
        serde_json::json!({
            "content": null,
            "finish_reason": "tool_calls",
            "tool_calls": [{
                "id": "call-classify-session-turn",
                "type": "function",
                "function": {
                    "name": "classify_session_turn",
                    "arguments": arguments.to_string(),
                }
            }]
        })
        .to_string(),
    )
}

#[derive(Clone)]
pub(crate) struct ShadowDaemonRuntime {
    state_root: PathBuf,
    event_bus: Arc<InMemoryEventBus>,
    session_store: Arc<SessionStore>,
    workspace_store: Arc<WorkspaceStore>,
    knowledge_store: Arc<KnowledgeStore>,
    governance: Arc<GovernanceService>,
    worker_runtime: WorkerRuntime,
    runtime_maintenance: ShadowRuntimeMaintenance,
}

impl ShadowDaemonRuntime {
    pub(crate) fn restore(config: &DaemonConfig) -> Result<Self, DaemonError> {
        let state_repository = ShadowStateRepository::new(config.state_root.clone());

        // 先加载工作区注册表（需要工作区路径来定位会话文件）
        let workspace_store = Arc::new(WorkspaceStore::from_persisted_parts(
            state_repository.load_workspace_durable_state()?,
            state_repository.load_workspace_recovery_sidecars()?,
        ));

        // 收集所有工作区的 (workspace_id, root_path)
        let workspace_roots: Vec<(String, std::path::PathBuf)> = workspace_store
            .workspaces()
            .into_iter()
            .map(|w| {
                (
                    w.workspace_id.to_string(),
                    std::path::PathBuf::from(w.root_path.as_str()),
                )
            })
            .collect();

        // 从各工作区 .magi/sessions.json 加载会话（含迁移旧全局 sessions.json）
        let session_durable = state_repository.load_sessions_from_workspaces(&workspace_roots)?;
        let session_store = Arc::new(SessionStore::from_persisted_parts(
            session_durable,
            state_repository.load_session_sidecars()?,
        ));
        let knowledge_store = Arc::new(KnowledgeStore::from_state(
            state_repository.load_knowledge_state()?,
        ));
        let event_bus = Arc::new(InMemoryEventBus::new(2048));
        let worker_runtime = WorkerRuntime::new(event_bus.clone());
        worker_runtime.restore_durable_snapshot(state_repository.load_worker_runtime_snapshot()?);
        let runtime_persistence = ShadowRuntimeSidecarPersistence::new(
            state_repository.clone(),
            session_store.clone(),
            workspace_store.clone(),
            worker_runtime.clone(),
        );

        Self::restore_ledger(&state_repository, &event_bus)?;
        Self::bootstrap_runtime_state(
            config,
            &state_repository,
            &runtime_persistence,
            &session_store,
            &workspace_store,
        )?;

        // 引导阶段：代码索引与 workspace 绑定，避免多个工作区共用同一份上下文摘要。
        let active_workspace = workspace_store.active_workspace_id().and_then(|ws_id| {
            workspace_store
                .workspaces()
                .into_iter()
                .find(|workspace| workspace.workspace_id == ws_id)
        });
        let ingested_code_index = if let Some(workspace) = active_workspace {
            if knowledge_store
                .code_index_summary_for_workspace(&workspace.workspace_id)
                .is_none()
            {
                let scan_root = PathBuf::from(workspace.root_path.as_str());
                ingest_workspace_code_index_in_workspace(
                    &knowledge_store,
                    &workspace.workspace_id,
                    &scan_root,
                );
                true
            } else {
                false
            }
        } else if knowledge_store.code_index_summary().is_none() {
            ingest_workspace_code_index(&knowledge_store, &config.bootstrap_workspace_root);
            true
        } else {
            false
        };
        if ingested_code_index {
            let _ = state_repository.save_knowledge_state(&knowledge_store.export_state());
        }

        let runtime_maintenance = ShadowRuntimeMaintenance::new(
            ShadowRuntimeMaintenanceConfig::default(),
            event_bus.clone(),
            runtime_persistence,
            session_store.clone(),
            workspace_store.clone(),
        );
        runtime_maintenance.publish_runtime_status_event("shadow-system-runtime-maintenance-ready");

        Ok(Self {
            state_root: config.state_root.clone(),
            event_bus,
            session_store,
            workspace_store,
            knowledge_store,
            governance: Arc::new(GovernanceService::default()),
            worker_runtime,
            runtime_maintenance,
        })
    }

    pub(crate) fn start_background_tasks(&self) {
        let runtime_maintenance = self.runtime_maintenance.clone();
        tokio::spawn(async move {
            runtime_maintenance.run_loop().await;
        });
    }

    pub(crate) fn publish_started_event(&self, service_name: &str) {
        let _ = self.event_bus.publish(EventEnvelope::system(
            EventId::new("shadow-system-started"),
            "system.started",
            serde_json::json!({
                "service": service_name,
                "mode": "local-shadow-rewrite"
            }),
        ));
    }

    fn build_api_state(&self, service_name: String) -> ApiState {
        self.build_api_state_with_options(service_name, &[], None)
    }

    fn build_api_state_with_options(
        &self,
        service_name: String,
        bridge_env: &[(&str, &str)],
        model_bridge_override: Option<Arc<dyn magi_bridge_client::ModelBridgeClient>>,
    ) -> ApiState {
        let orchestrator = OrchestratorService::new(self.event_bus.clone());
        let mut tool_registry = ToolRegistry::new(self.governance.clone(), self.event_bus.clone());
        tool_registry.register_default_builtins();
        let host_transport =
            Self::bridge_loopback_transport_with_env("host_bridge_loopback", bridge_env);
        let model_transport =
            Self::bridge_loopback_transport_with_env("model_bridge_loopback", bridge_env);
        let mcp_transport =
            Self::bridge_loopback_transport_with_env("mcp_bridge_loopback", bridge_env);

        // 创建带持久化路径的设置存储，并从磁盘恢复已有设置
        let settings_store = Arc::new(SettingsStore::with_persistence_path(
            self.state_root.join("settings.json"),
        ));
        if let Err(error) = settings_store.load_from_disk() {
            warn!(error = %error, "设置文件加载失败，使用空默认值");
        }

        // 模型桥唯一实现：HttpModelBridgeClient(直连 OpenAI 兼容 API)
        // 配置来源优先级: bridge_env override → 进程 env → settings.json auxiliary 段
        // 测试场景下允许通过 model_bridge_override 注入 stub
        let direct_http_result = if model_bridge_override.is_some() {
            None
        } else {
            Self::try_build_http_model_client(bridge_env, settings_store.as_ref())
        };
        let direct_http_probe_config = direct_http_result
            .as_ref()
            .map(|(_, config)| config.clone());

        // Use StdioMcpBridgeClient for direct MCP server connections when
        // MAGI_MCP_SERVER_COMMAND is configured, falling back to the
        // JSON-RPC subprocess loopback.
        let direct_mcp_client = StdioMcpBridgeClient::from_env();

        let model_bridge_client: Arc<dyn magi_bridge_client::ModelBridgeClient> =
            match (model_bridge_override.clone(), direct_http_result) {
                (Some(client), _) => client,
                (None, Some((http_client, _))) => Arc::new(http_client),
                (None, None) => {
                    panic!(
                        "未找到模型桥配置:请在环境变量 MAGI_OPENAI_COMPAT_BASE_URL 或 \
                         {}/settings.json 的 auxiliary 段配置 baseUrl",
                        self.state_root.display()
                    );
                }
            };
        let business_model_client: Arc<dyn magi_bridge_client::ModelBridgeClient> =
            model_bridge_client.clone();
        let bridge_runtime = BridgeDispatchRuntime::new()
            .with_host_client(Arc::new(JsonRpcHostBridgeClient::new(
                host_transport.clone(),
            )))
            .with_model_client(business_model_client.clone())
            .with_mcp_client(if let Some(mcp_client) = direct_mcp_client {
                Arc::new(mcp_client)
            } else {
                Arc::new(JsonRpcMcpBridgeClient::new(mcp_transport.clone()))
            });
        let skill_runtime = SkillDispatchRuntime::new(tool_registry.clone(), bridge_runtime);
        let worker_runtime = self.worker_runtime.clone();
        let memory_store = MemoryStore::new();
        let context_runtime =
            ContextRuntime::new((*self.knowledge_store).clone(), memory_store.clone());
        let context_runtime_for_dispatcher = Arc::new(context_runtime.clone());
        let tool_registry_for_dispatcher = tool_registry.clone();
        let task_store_checkpoint_path = self.state_root.join("task-store.json");
        let event_bus_for_task_store = self.event_bus.clone();
        let runner_result_receiver = Arc::new(EventBasedResultReceiver::new());
        let task_store = match TaskStore::restore_from_file(&task_store_checkpoint_path) {
            Ok(Some(restored)) => {
                let eb = event_bus_for_task_store.clone();
                let receiver = runner_result_receiver.clone();
                restored.set_status_change_callback(Box::new(
                    move |task_id, new_status, task: magi_core::Task| {
                        let event = magi_event_bus::task_events::task_status_changed_event(
                            &task_id.to_string(),
                            &task.mission_id.to_string(),
                            "",
                            &format!("{:?}", new_status),
                            &format!("{:?}", task.kind),
                        );
                        let _ = eb.publish(event);
                        push_terminal_task_result(&receiver, task_id, new_status);
                    },
                ));
                let (revoked_leases, blocked_tasks) = restored
                    .reconcile_volatile_runtime_after_restore(&worker_runtime.durable_snapshot());
                if revoked_leases > 0 || blocked_tasks > 0 {
                    warn!(
                        revoked_leases,
                        blocked_tasks,
                        "检测到 checkpoint 中残留的易失执行态，已统一收口为可恢复状态"
                    );
                }
                Arc::new(restored)
            }
            _ => {
                let receiver = runner_result_receiver.clone();
                Arc::new(TaskStore::with_status_change_callback(Box::new(
                    move |task_id, new_status, task: magi_core::Task| {
                        let event = magi_event_bus::task_events::task_status_changed_event(
                            &task_id.to_string(),
                            &task.mission_id.to_string(),
                            "",
                            &format!("{:?}", new_status),
                            &format!("{:?}", task.kind),
                        );
                        let _ = event_bus_for_task_store.publish(event);
                        push_terminal_task_result(&receiver, task_id, new_status);
                    },
                )))
            }
        };
        let execution_runtime = orchestrator
            .execution_runtime(worker_runtime.clone(), tool_registry, skill_runtime)
            .with_task_store(Arc::clone(&task_store))
            .with_context_runtime(
                context_runtime,
                ExecutionContextConfig {
                    budget: ContextBudget {
                        max_turns: 8,
                        max_knowledge: 6,
                        max_memory: 6,
                        max_shared_items: 4,
                        max_file_summaries: 4,
                    },
                    project_key: None,
                },
            );

        let app_skill_runtime = Arc::new(
            magi_api::skill_loader::build_skill_runtime_from_settings(&settings_store),
        );

        let mut state = ApiState::new(
            service_name,
            self.event_bus.clone(),
            self.session_store.clone(),
            self.workspace_store.clone(),
            self.governance.clone(),
        )
        .with_knowledge_store(self.knowledge_store.clone())
        .with_settings_store(settings_store.clone())
        .with_skill_runtime(app_skill_runtime.clone())
        .with_tool_registry(tool_registry_for_dispatcher.clone())
        .with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
            self.state_root.join("sessions.json"),
            self.state_root.join("workspaces.json"),
            self.state_root.join("knowledge.json"),
        )))
        .with_bridge_probe_transport(BridgeServerKind::Host, host_transport)
        .with_bridge_probe_transport(BridgeServerKind::Model, model_transport)
        .with_bridge_probe_transport(BridgeServerKind::Mcp, mcp_transport)
        .with_shadow_execution_pipeline(orchestrator, execution_runtime, memory_store);

        let state_for_task_workers = state.clone();
        let state_for_knowledge_persist = state.clone();
        let knowledge_persist_callback = Arc::new(move || {
            if let Err(error) = state_for_knowledge_persist.persist_knowledge_state() {
                tracing::warn!(?error, "自动知识沉淀持久化失败");
            }
        });
        let shadow_task_dispatcher = ShadowTaskDispatcher::new(
            self.event_bus.clone(),
            state
                .shadow_execution_pipeline()
                .expect("shadow execution pipeline should exist when daemon wires task runner")
                .clone(),
            state.session_store.clone(),
            state.shadow_task_execution_registry().clone(),
            runner_result_receiver.clone(),
        );
        let shadow_task_dispatcher = Arc::new(
            shadow_task_dispatcher
                .with_model_bridge_client(business_model_client.clone())
                .with_knowledge_store(state.knowledge_store.clone())
                .with_knowledge_persist_callback(knowledge_persist_callback)
                .with_settings_store(state.settings_store.clone())
                .with_context_runtime(context_runtime_for_dispatcher)
                .with_tool_registry(tool_registry_for_dispatcher)
                .with_skill_runtime(app_skill_runtime),
        );
        let session_turn_dispatcher = shadow_task_dispatcher.clone();
        let runner_manager = RunnerManager::with_dispatcher_and_worker_catalog(
            Arc::clone(&task_store),
            Arc::new(move || state_for_task_workers.task_worker_catalog()),
            shadow_task_dispatcher,
            runner_result_receiver,
        )
        .with_checkpoint_path(task_store_checkpoint_path);
        state = state
            .with_task_store(task_store)
            .with_runner_manager(runner_manager)
            .with_session_turn_dispatcher(session_turn_dispatcher)
            .with_model_bridge_client(business_model_client);

        if let Some(probe_config) = direct_http_probe_config {
            state = state.with_direct_http_model_probe(probe_config);
        }

        state
    }

    pub(crate) fn router(&self, service_name: String) -> axum::Router {
        build_router(self.build_api_state(service_name))
    }

    #[cfg(test)]
    pub(crate) fn router_with_state_for_tests(
        &self,
        service_name: String,
    ) -> (axum::Router, ApiState) {
        let state = self.build_api_state_with_options(
            service_name,
            &[],
            Some(Arc::new(StaticTestModelBridgeClient)),
        );
        (build_router(state.clone()), state)
    }

    #[cfg(test)]
    pub(crate) fn router_with_bridge_env_for_tests(
        &self,
        service_name: String,
        bridge_env: &[(&str, &str)],
    ) -> (axum::Router, ApiState) {
        let state = self.build_api_state_with_options(service_name, bridge_env, None);
        (build_router(state.clone()), state)
    }

    fn restore_ledger(
        state_repository: &ShadowStateRepository,
        event_bus: &Arc<InMemoryEventBus>,
    ) -> Result<(), DaemonError> {
        let audit_usage_ledger = state_repository.load_audit_usage_ledger()?;
        event_bus.import_audit_usage_ledger_snapshot(audit_usage_ledger);
        event_bus.set_audit_usage_ledger_persistence(state_repository.audit_usage_ledger_path());
        if let Err(error) = event_bus.refresh_audit_usage_ledger_persistence() {
            warn!(error = %error, "审计/用量账本初始刷新失败，后续事件仍会继续运行");
        }
        publish_ledger_status_event(
            event_bus,
            "shadow-system-ledger-ready",
            "system.ledger.ready",
        );
        Ok(())
    }

    fn bootstrap_runtime_state(
        config: &DaemonConfig,
        state_repository: &ShadowStateRepository,
        runtime_persistence: &ShadowRuntimeSidecarPersistence,
        session_store: &Arc<SessionStore>,
        workspace_store: &Arc<WorkspaceStore>,
    ) -> Result<(), DaemonError> {
        bootstrap_shadow_state(
            session_store,
            workspace_store,
            &config.bootstrap_workspace_root,
            &config.bootstrap_worktree_root,
        );
        // 会话保存到各工作区的 .magi/sessions.json，不再写全局路径
        let durable = session_store.durable_state();
        for workspace in workspace_store.workspaces() {
            let root = std::path::PathBuf::from(workspace.root_path.as_str());
            let ws_id = workspace.workspace_id.to_string();
            let ws_sessions: Vec<_> = durable
                .sessions
                .iter()
                .filter(|s| s.workspace_id.as_deref() == Some(&ws_id))
                .cloned()
                .collect();
            let session_ids = ws_sessions
                .iter()
                .map(|session| session.session_id.clone())
                .collect::<HashSet<_>>();
            let ws_state = magi_session_store::SessionDurableState {
                sessions: ws_sessions,
                current_session_id: durable.current_session_id.clone(),
                timeline: durable
                    .timeline
                    .iter()
                    .filter(|entry| session_ids.contains(&entry.session_id))
                    .cloned()
                    .collect(),
                notifications: durable
                    .notifications
                    .iter()
                    .filter(|notification| session_ids.contains(&notification.session_id))
                    .cloned()
                    .collect(),
            };
            state_repository.save_workspace_session_state(&root, &ws_state)?;
        }
        state_repository.save_workspace_durable_state(&workspace_store.durable_state())?;
        runtime_persistence.flush_runtime_sidecars()?;
        Ok(())
    }

    fn bridge_loopback_transport_with_env(
        binary_name: &str,
        bridge_env: &[(&str, &str)],
    ) -> Arc<dyn BridgeTransport> {
        let transport = bridge_env.iter().fold(
            JsonRpcStdioTransport::new(Self::bridge_loopback_executable(binary_name)),
            |transport, (key, value)| transport.with_env(*key, *value),
        );
        Arc::new(transport)
    }

    /// Build an `HttpModelBridgeClient` from configuration.
    ///
    /// 配置来源优先级:
    /// 1. `bridge_env` overrides (测试场景)
    /// 2. 进程级 env (`MAGI_OPENAI_COMPAT_*`)
    /// 3. `settings.json` 的 `auxiliary` 段(生产配置)
    ///
    /// 任一字段(baseUrl/apiKey/model/protocol)按上述顺序解析,允许跨源混合。
    ///
    /// Returns the client together with a [`DirectHttpModelProbeConfig`] that
    /// the cutover-smoke provider can use for its own independent probe.
    fn try_build_http_model_client(
        bridge_env: &[(&str, &str)],
        settings_store: &SettingsStore,
    ) -> Option<(HttpModelBridgeClient, DirectHttpModelProbeConfig)> {
        let auxiliary = settings_store.get_section("auxiliary");
        let aux_string = |field: &str| -> Option<String> {
            auxiliary
                .get(field)
                .and_then(|v| v.as_str())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let find_env = |key: &str| -> Option<String> {
            bridge_env
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
                .filter(|v| !v.trim().is_empty())
                .or_else(|| {
                    env::var(key)
                        .ok()
                        .map(|v| v.trim().to_string())
                        .filter(|v| !v.is_empty())
                })
        };

        let base_url = find_env("MAGI_OPENAI_COMPAT_BASE_URL").or_else(|| aux_string("baseUrl"))?;
        let api_key = find_env("MAGI_OPENAI_COMPAT_API_KEY").or_else(|| aux_string("apiKey"));
        let model = find_env("MAGI_OPENAI_COMPAT_MODEL")
            .or_else(|| aux_string("model"))
            .unwrap_or_else(|| "gpt-4".to_string());
        let protocol = match find_env("MAGI_OPENAI_COMPAT_PROTOCOL")
            .or_else(|| aux_string("openaiProtocol"))
            .as_deref()
        {
            Some("responses") => HttpModelBridgeProtocol::Responses,
            _ => HttpModelBridgeProtocol::ChatCompletions,
        };

        let probe_config = DirectHttpModelProbeConfig {
            base_url: base_url.clone(),
            api_key: api_key.clone(),
            model: model.clone(),
        };
        Some((
            HttpModelBridgeClient::new_with_protocol(base_url, api_key, model, protocol),
            probe_config,
        ))
    }

    fn bridge_loopback_executable(binary_name: &str) -> String {
        let env_key = format!("CARGO_BIN_EXE_{binary_name}");
        if let Some(path) = env::var_os(&env_key) {
            return path.to_string_lossy().to_string();
        }

        let mut path = env::current_exe().expect("current executable should exist");
        path.pop();
        if path.ends_with("deps") {
            path.pop();
        }
        path.push(Self::binary_name(binary_name));
        path.to_string_lossy().to_string()
    }

    fn binary_name(binary_name: &str) -> String {
        format!("{binary_name}{}", env::consts::EXE_SUFFIX)
    }
}

fn push_terminal_task_result(
    receiver: &Arc<EventBasedResultReceiver>,
    task_id: &magi_core::TaskId,
    new_status: TaskStatus,
) {
    match new_status {
        TaskStatus::Completed => {
            receiver.push_result(TaskResult {
                task_id: task_id.clone(),
                lease_id: LeaseId::new(format!("lease-result-{}", task_id)),
                outcome: TaskOutcome::Completed {
                    output_refs: Vec::new(),
                },
            });
        }
        TaskStatus::Failed => {
            receiver.push_result(TaskResult {
                task_id: task_id.clone(),
                lease_id: LeaseId::new(format!("lease-result-{}", task_id)),
                outcome: TaskOutcome::Failed {
                    error: "task store reported terminal failure".to_string(),
                },
            });
        }
        _ => {
            // Non-terminal: clear the dedup entry so the task can produce a
            // new result after re-dispatch (e.g. after lease expiry reset).
            receiver.clear_seen(task_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ShadowDaemonRuntime;
    use crate::daemon::config::DaemonConfig;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use magi_core::{TaskStatus, WorkspaceId};
    use magi_event_bus::AuditUsageLedgerSnapshot;
    use serde_json::{Value, json};
    use std::{
        collections::BTreeMap,
        fs,
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        path::PathBuf,
        sync::mpsc,
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };
    use tower::util::ServiceExt;

    fn temp_state_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "magi-daemon-runtime-test-{name}-{}",
            magi_core::UtcMillis::now().0
        ));
        fs::create_dir_all(&root).expect("temp state root should be creatable");
        root
    }

    #[test]
    fn restore_bootstraps_empty_state_and_persists_runtime_files() {
        let state_root = temp_state_root("bootstrap");
        let config = DaemonConfig::new("127.0.0.1", 0, "shadow-test", state_root.clone());
        let workspace_root = config.bootstrap_workspace_root.clone();

        let runtime = ShadowDaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap empty state");

        assert!(runtime.session_store.current_session().is_some());
        assert_eq!(runtime.workspace_store.snapshots().len(), 1);
        assert!(workspace_root.join(".magi").join("sessions.json").exists());
        assert!(!state_root.join("sessions.json").exists());
        assert!(state_root.join("workspaces.json").exists());
        assert!(state_root.join("session-sidecars.json").exists());
        assert!(state_root.join("workspace-recovery-sidecars.json").exists());
        assert!(state_root.join("audit-usage-ledger.json").exists());

        let ledger = serde_json::from_slice::<AuditUsageLedgerSnapshot>(
            &fs::read(state_root.join("audit-usage-ledger.json"))
                .expect("audit usage ledger should be readable"),
        )
        .expect("audit usage ledger should deserialize");
        assert!(ledger.audit_entries.is_empty());
        assert!(ledger.usage_entries.is_empty());
    }

    #[tokio::test]
    async fn knowledge_endpoint_returns_code_index_after_bootstrap_scan() {
        let state_root = temp_state_root("knowledge-code-index");
        let config = DaemonConfig::new("127.0.0.1", 0, "shadow-test", state_root.clone());
        let workspace_root = config.bootstrap_workspace_root.clone();

        // 在引导工作区中创建模拟源文件，供代码扫描器发现
        fs::create_dir_all(workspace_root.join("src")).unwrap();
        fs::write(
            workspace_root.join("src/main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();
        fs::write(
            workspace_root.join("src/lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
        )
        .unwrap();
        fs::write(
            workspace_root.join("Cargo.toml"),
            "[package]\nname = \"test\"\n",
        )
        .unwrap();

        let runtime = ShadowDaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap and scan code");

        // 验证知识存储中有代码索引摘要
        let summary = runtime
            .knowledge_store
            .code_index_summary_for_workspace(&WorkspaceId::new("shadow-workspace-001"))
            .expect("code index summary should exist after bootstrap scan");
        assert!(
            !summary.files.is_empty(),
            "code index should contain scanned files"
        );
        assert!(
            summary.tech_stack.iter().any(|t| t == "Rust"),
            "tech stack should detect Rust"
        );
        assert!(
            summary.entry_points.iter().any(|e| e.ends_with("main.rs")),
            "entry points should detect main.rs"
        );

        // 通过 API 路由验证返回结构
        let app = runtime.router("shadow-test".to_string());
        let knowledge = get_json(app, "/api/knowledge?workspaceId=shadow-workspace-001").await;
        assert!(
            knowledge.get("codeIndex").is_some_and(|v| !v.is_null()),
            "API should return non-null codeIndex"
        );
        let code_index = knowledge["codeIndex"]
            .as_object()
            .expect("codeIndex should be an object");
        let files = code_index["files"]
            .as_array()
            .expect("files should be an array");
        assert!(!files.is_empty(), "codeIndex.files should not be empty");
        assert!(
            code_index["techStack"]
                .as_array()
                .is_some_and(|t| !t.is_empty()),
            "codeIndex.techStack should not be empty"
        );
    }

    async fn post_json(app: axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        let status = response.status();
        let body = serde_json::from_slice(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body should read"),
        )
        .expect("response should be valid json");
        (status, body)
    }

    async fn get_json(app: axum::Router, path: &str) -> Value {
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(path)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body should read"),
        )
        .expect("response should be valid json")
    }

    async fn get_task_projection(app: axum::Router, root_task_id: &str, session_id: &str) -> Value {
        get_json(
            app,
            &format!("/api/tasks/graph/{root_task_id}?sessionId={session_id}"),
        )
        .await
    }

    async fn wait_for_task_projection_completed(
        app: axum::Router,
        root_task_id: &str,
        session_id: &str,
    ) -> Value {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let projection = get_task_projection(app.clone(), root_task_id, session_id).await;
            let total_tasks = projection["progress_summary"]["total_tasks"]
                .as_u64()
                .unwrap_or(0);
            let completed_tasks = projection["progress_summary"]["completed_tasks"]
                .as_u64()
                .unwrap_or(0);
            if total_tasks >= 2
                && completed_tasks == total_tasks
                && projection["root_task"]["status"] == "Completed"
            {
                return projection;
            }
            if Instant::now() >= deadline {
                return projection;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_for_execution_group(
        app: axum::Router,
        mission_id: &str,
        mut is_ready: impl FnMut(&Value) -> bool,
    ) -> Value {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let read_model = get_json(app.clone(), "/runtime/read-model").await;
            if let Some(group) = read_model["details"]["execution_groups"]
                .as_array()
                .and_then(|groups| {
                    groups
                        .iter()
                        .find(|entry| entry["mission_id"] == mission_id)
                })
            {
                if is_ready(group) || Instant::now() >= deadline {
                    return group.clone();
                }
            }
            if Instant::now() >= deadline {
                panic!("execution group {mission_id} did not appear before timeout");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn assert_completed_two_task_projection(projection: &Value) {
        let total_tasks = projection["progress_summary"]["total_tasks"]
            .as_u64()
            .expect("total_tasks should serialize as integer");
        let completed_tasks = projection["progress_summary"]["completed_tasks"]
            .as_u64()
            .expect("completed_tasks should serialize as integer");
        assert!(
            total_tasks >= 2,
            "task projection should include at least root + action"
        );
        assert_eq!(completed_tasks, total_tasks);
        assert_eq!(projection["progress_summary"]["failed_tasks"], 0);
        assert_eq!(projection["root_task"]["status"], "Completed");
    }

    fn service_entries_by_kind(snapshot: &Value) -> BTreeMap<String, &Value> {
        snapshot["services"]
            .as_array()
            .expect("services should be an array")
            .iter()
            .map(|entry| {
                (
                    entry["server_kind"]
                        .as_str()
                        .expect("server kind should serialize as string")
                        .to_string(),
                    entry,
                )
            })
            .collect()
    }

    fn test_bridge_binary_path(binary_name: &str) -> PathBuf {
        let env_key = format!("CARGO_BIN_EXE_{binary_name}");
        if let Some(path) = std::env::var_os(&env_key) {
            return PathBuf::from(path);
        }

        let mut path = std::env::current_exe().expect("current exe should exist");
        path.pop();
        if path.ends_with("deps") {
            path.pop();
        }
        path.push(format!("{binary_name}{}", std::env::consts::EXE_SUFFIX));
        path
    }

    #[derive(Debug)]
    struct RecordedHttpRequest {
        request_line: String,
        headers: BTreeMap<String, String>,
        body: String,
    }

    fn spawn_http_stub(
        status: u16,
        response_body: Value,
    ) -> (String, mpsc::Receiver<RecordedHttpRequest>, JoinHandle<()>) {
        // Default to 2 connections: one for the JSON-RPC bridge loopback
        // and one for the direct HTTP model provider probe.
        spawn_http_stub_multi(status, response_body, 2)
    }

    fn spawn_http_stub_multi(
        status: u16,
        response_body: Value,
        connection_count: usize,
    ) -> (String, mpsc::Receiver<RecordedHttpRequest>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("http stub should bind");
        let address = listener.local_addr().expect("http stub addr should exist");
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            for _ in 0..connection_count {
                let (mut stream, _) = listener.accept().expect("http stub should accept");
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("read timeout should set");
                let request = read_http_request(&mut stream);
                let body = response_body.to_string();
                let response = format!(
                    "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    status_reason(status),
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("http stub should write");
                stream.flush().expect("http stub should flush");
                sender.send(request).expect("request should send to test");
            }
        });

        (format!("http://{address}/v1"), receiver, handle)
    }

    fn read_http_request(stream: &mut TcpStream) -> RecordedHttpRequest {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        let header_end = loop {
            let read = stream.read(&mut chunk).expect("http request should read");
            assert!(read > 0, "http request should include headers");
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(index) = find_header_end(&buffer) {
                break index + 4;
            }
        };

        let header_text =
            String::from_utf8(buffer[..header_end].to_vec()).expect("headers should be utf-8");
        let mut lines = header_text.split("\r\n");
        let request_line = lines.next().expect("request line should exist").to_string();
        let mut headers = BTreeMap::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let (name, value) = line
                .split_once(':')
                .expect("header should contain separator");
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }

        let content_length = headers
            .get("content-length")
            .expect("content-length should exist")
            .parse::<usize>()
            .expect("content-length should parse");
        while buffer.len() < header_end + content_length {
            let read = stream.read(&mut chunk).expect("http body should read");
            assert!(read > 0, "http request should include body");
            buffer.extend_from_slice(&chunk[..read]);
        }

        let body = String::from_utf8(buffer[header_end..header_end + content_length].to_vec())
            .expect("request body should be utf-8");
        RecordedHttpRequest {
            request_line,
            headers,
            body,
        }
    }

    fn find_header_end(buffer: &[u8]) -> Option<usize> {
        buffer.windows(4).position(|window| window == b"\r\n\r\n")
    }

    fn status_reason(status: u16) -> &'static str {
        match status {
            200 => "OK",
            401 => "Unauthorized",
            429 => "Too Many Requests",
            _ => "Test Response",
        }
    }

    #[tokio::test]
    async fn router_session_action_auto_extraction_is_consumed_on_followup_dispatch() {
        let state_root = temp_state_root("router-session-action");
        let config = DaemonConfig::new("127.0.0.1", 0, "shadow-test", state_root);
        let runtime = ShadowDaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap empty state");
        let (app, state) = runtime.router_with_state_for_tests("shadow-test".to_string());
        let active_workspace_id = state
            .workspace_registry
            .active_workspace_id()
            .expect("bootstrap workspace should exist");
        if state
            .session_store
            .session(&magi_core::SessionId::new("shadow-session-001"))
            .is_none()
        {
            state
                .session_store
                .create_session_for_workspace(
                    magi_core::SessionId::new("shadow-session-001"),
                    "runtime session".to_string(),
                    Some(active_workspace_id.to_string()),
                )
                .expect("runtime session should be creatable");
        }

        let (status, first_body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "shadow-session-001",
                "text": "remember parser constraint",
                "deepTask": false,
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {first_body:?}");

        let first_accepted_at = first_body["acceptedAt"]
            .as_u64()
            .expect("accepted_at should serialize as integer");
        let first_mission_id = format!("mission-session-action-{first_accepted_at}");
        let first_root_task_id = first_body["rootTaskId"]
            .as_str()
            .expect("root_task_id should serialize as string");
        let read_model = get_json(app.clone(), "/runtime/read-model").await;
        let first_execution_group = read_model["details"]["execution_groups"]
            .as_array()
            .expect("execution groups should be an array")
            .iter()
            .find(|entry| entry["mission_id"] == first_mission_id)
            .expect("first execution group should exist");
        assert_eq!(first_execution_group["context_used_memory_count"], 0);
        assert_eq!(
            first_execution_group["context_memory_extraction_refs"]
                .as_array()
                .expect("refs should be array")
                .len(),
            0
        );
        let first_projection = wait_for_task_projection_completed(
            app.clone(),
            first_root_task_id,
            "shadow-session-001",
        )
        .await;
        assert_completed_two_task_projection(&first_projection);

        let (status, second_body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "shadow-session-001",
                "text": "follow up parser work",
                "deepTask": false,
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {second_body:?}");

        let second_accepted_at = second_body["acceptedAt"]
            .as_u64()
            .expect("accepted_at should serialize as integer");
        let second_mission_id = format!("mission-session-action-{second_accepted_at}");
        let second_root_task_id = second_body["rootTaskId"]
            .as_str()
            .expect("root_task_id should serialize as string");
        let expected_extraction_id = format!("extract-session-action-{first_accepted_at}");

        let second_execution_group =
            wait_for_execution_group(app.clone(), &second_mission_id, |entry| {
                entry["context_memory_extraction_refs"] == json!([expected_extraction_id])
            })
            .await;
        assert_eq!(second_execution_group["context_used_memory_count"], 1);
        assert_eq!(second_execution_group["context_extracted_memory_count"], 1);
        assert_eq!(
            second_execution_group["context_memory_extraction_refs"],
            json!([expected_extraction_id])
        );
        let second_projection =
            wait_for_task_projection_completed(app, second_root_task_id, "shadow-session-001")
                .await;
        assert_completed_two_task_projection(&second_projection);
    }

    #[tokio::test]
    async fn router_regular_session_turn_uses_daemon_session_turn_dispatcher() {
        let state_root = temp_state_root("router-regular-session-turn");
        let config = DaemonConfig::new("127.0.0.1", 0, "shadow-test", state_root);
        let runtime = ShadowDaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap empty state");
        let (app, state) = runtime.router_with_state_for_tests("shadow-test".to_string());
        let active_workspace_id = state
            .workspace_registry
            .active_workspace_id()
            .expect("bootstrap workspace should exist");
        let session_id = magi_core::SessionId::new("shadow-session-chat");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "runtime chat session".to_string(),
                Some(active_workspace_id.to_string()),
            )
            .expect("runtime session should be creatable");

        let (status, body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": session_id.to_string(),
                "text": "这是一条普通对话",
                "deepTask": false,
                "skillName": null,
                "images": [],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {body:?}");
        assert_eq!(body["route"], "chat");
        assert!(body.get("rootTaskId").is_none());

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let read_model = get_json(app.clone(), "/runtime/read-model").await;
            let session = read_model["details"]["sessions"]
                .as_array()
                .and_then(|sessions| {
                    sessions
                        .iter()
                        .find(|entry| entry["session_id"] == session_id.to_string())
                })
                .expect("session should appear in read model");
            let status = session["current_turn"]["status"].as_str().unwrap_or("");
            if status == "completed" {
                assert!(
                    session["turn_items"]
                        .as_array()
                        .expect("turn items should be an array")
                        .iter()
                        .any(|item| item["kind"] == "assistant_final"),
                    "regular chat turn should produce assistant_final item: {session:?}"
                );
                assert!(
                    read_model["details"]["tasks"]
                        .as_array()
                        .expect("tasks should be an array")
                        .is_empty(),
                    "regular chat turn must not create task graph"
                );
                break;
            }
            assert_ne!(
                status, "failed",
                "regular chat turn should not fail: {session:?}"
            );
            if Instant::now() >= deadline {
                panic!("regular chat turn did not complete before timeout: {session:?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn router_recovery_resume_writeback_is_consumed_on_followup_dispatch() {
        let state_root = temp_state_root("router-recovery-resume");
        let config = DaemonConfig::new("127.0.0.1", 0, "shadow-test", state_root);
        let runtime = ShadowDaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap empty state");
        let (app, state) = runtime.router_with_state_for_tests("shadow-test".to_string());
        let active_workspace_id = state
            .workspace_registry
            .active_workspace_id()
            .expect("bootstrap workspace should exist");
        if state
            .session_store
            .session(&magi_core::SessionId::new("shadow-session-001"))
            .is_none()
        {
            state
                .session_store
                .create_session_for_workspace(
                    magi_core::SessionId::new("shadow-session-001"),
                    "runtime recovery session".to_string(),
                    Some(active_workspace_id.to_string()),
                )
                .expect("runtime recovery session should be creatable");
        }

        let (status, seed_body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "shadow-session-001",
                "text": "seed recovery route state",
                "deepTask": false,
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected seed body: {seed_body:?}"
        );

        let session_id = magi_core::SessionId::new("shadow-session-001");
        let ownership = runtime
            .session_store
            .execution_ownership(&session_id)
            .expect("seed session action should bind execution ownership");
        let workspace_id = ownership
            .workspace_id
            .clone()
            .expect("seed execution ownership should include workspace");
        let recovery_task_id = ownership
            .task_id
            .clone()
            .expect("seed execution ownership should include task");
        state
            .task_store()
            .expect("task store should be configured")
            .update_status(&recovery_task_id, TaskStatus::Blocked)
            .expect("seed task should become recoverable");
        let snapshot = runtime.workspace_store.append_execution_snapshot(
            workspace_id.clone(),
            ownership.clone(),
            "snapshot-daemon-recovery-route",
            "Daemon recovery snapshot",
        );
        let recovery = runtime.workspace_store.prepare_recovery_entry(
            workspace_id,
            ownership,
            snapshot.snapshot_id,
            "recovery-daemon-route",
            Some("resume daemon route followup".to_string()),
        );
        runtime
            .workspace_store
            .mark_recovery_ready(&recovery.recovery_id)
            .expect("recovery should become ready");
        runtime
            .session_store
            .attach_recovery_ref(&session_id, Some(recovery.recovery_id.clone()))
            .expect("recovery ref should attach to session");

        let (status, recovery_body) = post_json(
            app.clone(),
            "/api/session/continue",
            json!({
                "sessionId": session_id.to_string(),
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected recovery body: {recovery_body:?}"
        );
        assert_eq!(recovery_body["sessionId"], session_id.to_string());
        assert_eq!(recovery_body["status"], "continued");

        let (status, followup_body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "shadow-session-001",
                "text": "consume recovery memory",
                "deepTask": false,
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected followup body: {followup_body:?}"
        );

        let followup_accepted_at = followup_body["acceptedAt"]
            .as_u64()
            .expect("accepted_at should serialize as integer");
        let followup_mission_id = format!("mission-session-action-{followup_accepted_at}");
        let expected_extraction_id = "extract-session-continue-recovery-daemon-route";
        let followup_execution_group =
            wait_for_execution_group(app, &followup_mission_id, |entry| {
                entry["context_memory_extraction_refs"]
                    .as_array()
                    .is_some_and(|refs| refs.iter().any(|value| value == expected_extraction_id))
            })
            .await;
        let extraction_refs = followup_execution_group["context_memory_extraction_refs"]
            .as_array()
            .expect("refs should be array");
        assert!(
            extraction_refs
                .iter()
                .any(|value| value == expected_extraction_id),
            "followup execution group should consume recovery extraction, got {extraction_refs:?}"
        );
    }

    #[tokio::test]
    async fn daemon_router_bridge_services_exports_shadow_model_host_and_mcp_catalogs() {
        for binary_name in [
            "host_bridge_loopback",
            "model_bridge_loopback",
            "mcp_bridge_loopback",
        ] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let state_root = temp_state_root("router-bridge-services");
        let config = DaemonConfig::new("127.0.0.1", 0, "shadow-test", state_root);
        let runtime = ShadowDaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap empty state");
        let app = runtime.router("shadow-test".to_string());

        let snapshot = get_json(app, "/bridges/services").await;
        let services = service_entries_by_kind(&snapshot);
        assert_eq!(
            services.len(),
            3,
            "unexpected bridge snapshot: {snapshot:?}"
        );

        let host = services
            .get("host")
            .expect("host bridge snapshot should exist");
        assert_eq!(host["health"]["status"], "ok");
        assert_eq!(host["health"]["ok"], true);
        assert_eq!(
            host["service_catalog"]["services"][0]["service_name"],
            "shadow-host-vscode"
        );

        let model = services
            .get("model")
            .expect("model bridge snapshot should exist");
        assert_eq!(model["health"]["status"], "ok");
        assert_eq!(model["health"]["ok"], true);
        assert!(
            model["service_catalog"]["services"]
                .as_array()
                .expect("model services should be an array")
                .iter()
                .any(|service| service["service_name"] == "shadow-model"),
            "model catalog should include shadow-model: {model:?}"
        );
        assert!(
            model["service_catalog"]["services"]
                .as_array()
                .expect("model services should be an array")
                .iter()
                .any(|service| service["service_name"] == "openai-compatible"),
            "model catalog should include openai-compatible: {model:?}"
        );

        let mcp = services
            .get("mcp")
            .expect("mcp bridge snapshot should exist");
        assert_eq!(mcp["health"]["status"], "ok");
        assert_eq!(mcp["health"]["ok"], true);
        assert_eq!(
            mcp["service_catalog"]["services"][0]["service_name"],
            "shadow-mcp-manager"
        );
    }

    #[tokio::test]
    async fn daemon_router_bridge_preflight_executes_shadow_host_model_and_mcp_smokes() {
        for binary_name in [
            "host_bridge_loopback",
            "model_bridge_loopback",
            "mcp_bridge_loopback",
        ] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let state_root = temp_state_root("router-bridge-preflight");
        let config = DaemonConfig::new("127.0.0.1", 0, "shadow-test", state_root);
        let runtime = ShadowDaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap empty state");
        let app = runtime.router("shadow-test".to_string());
        let services_snapshot = get_json(app.clone(), "/bridges/services").await;

        let snapshot = get_json(app, "/bridges/preflight").await;
        let services = service_entries_by_kind(&snapshot);
        assert_eq!(
            services.len(),
            3,
            "unexpected bridge preflight: {snapshot:?}"
        );

        let host = services.get("host").expect("host preflight should exist");
        assert_eq!(host["checks"][0]["check_name"], "workspace_roots");
        assert_eq!(host["checks"][0]["ok"], true);

        let model = services.get("model").expect("model preflight should exist");
        assert!(
            model["checks"]
                .as_array()
                .expect("model checks should be an array")
                .iter()
                .any(|check| check["target"] == "shadow-model" && check["ok"] == true),
            "model preflight should include shadow-model invoke: {model:?}"
        );
        let openai_ready = services_snapshot["services"]
            .as_array()
            .expect("bridge services should be an array")
            .iter()
            .find(|entry| entry["server_kind"] == "model")
            .and_then(|entry| entry["service_catalog"]["services"].as_array())
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|service| service["service_name"] == "openai-compatible")
            })
            .and_then(|service| service["service_health"].as_str())
            == Some("ready");
        if openai_ready {
            assert!(
                model["checks"]
                    .as_array()
                    .expect("model checks should be an array")
                    .iter()
                    .any(|check| check["target"] == "openai-compatible" && check["ok"] == true),
                "ready openai-compatible provider should execute preflight smoke: {model:?}"
            );
        } else {
            assert!(
                model["checks"]
                    .as_array()
                    .expect("model checks should be an array")
                    .iter()
                    .all(|check| check["target"] != "openai-compatible"),
                "non-ready openai-compatible provider should not execute preflight smoke: {model:?}"
            );
        }

        let mcp = services.get("mcp").expect("mcp preflight should exist");
        assert!(
            mcp["checks"]
                .as_array()
                .expect("mcp checks should be an array")
                .iter()
                .any(|check| check["check_name"] == "list_servers" && check["ok"] == true),
            "mcp preflight should include manager list_servers: {mcp:?}"
        );
        assert!(
            mcp["checks"]
                .as_array()
                .expect("mcp checks should be an array")
                .iter()
                .any(|check| check["target"] == "shadow-mcp.echo.inspect" && check["ok"] == true),
            "mcp preflight should include shadow-mcp echo.inspect: {mcp:?}"
        );
    }

    #[tokio::test]
    async fn daemon_router_bridge_cutover_smoke_exports_contract_snapshots() {
        for binary_name in [
            "host_bridge_loopback",
            "model_bridge_loopback",
            "mcp_bridge_loopback",
        ] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let state_root = temp_state_root("router-bridge-cutover");
        let config = DaemonConfig::new("127.0.0.1", 0, "shadow-test", state_root);
        let runtime = ShadowDaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap empty state");
        let app = runtime.router("shadow-test".to_string());
        let services_snapshot = get_json(app.clone(), "/bridges/services").await;
        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        let services = service_entries_by_kind(&snapshot);
        assert_eq!(
            services.len(),
            3,
            "unexpected bridge cutover snapshot: {snapshot:?}"
        );
        assert_eq!(snapshot["checked_service_count"], 3);
        let blocking_issues = snapshot["blocking_issues"]
            .as_array()
            .expect("blocking issues should serialize as array");
        let failing_check_count = services
            .values()
            .flat_map(|service| {
                service["checks"]
                    .as_array()
                    .expect("checks should serialize as array")
                    .iter()
            })
            .filter(|check| check["ok"] != true)
            .count();
        assert_eq!(
            snapshot["blocking_check_count"]
                .as_u64()
                .expect("blocking count should serialize as u64") as usize,
            failing_check_count
        );
        assert_eq!(blocking_issues.len(), failing_check_count);
        let reason_code_counts = snapshot["blocking_issue_counts_by_reason_code"]
            .as_object()
            .expect("reason-code counts should serialize as object");
        let server_kind_counts = snapshot["blocking_issue_counts_by_server_kind"]
            .as_object()
            .expect("server-kind counts should serialize as object");

        let host = services.get("host").expect("host cutover should exist");
        assert_eq!(host["service_ok"], true);
        assert_eq!(host["blocking_check_count"], 0);
        assert!(
            host["blocking_targets"]
                .as_array()
                .expect("host blocking targets should be an array")
                .is_empty()
        );
        assert_eq!(host["checks"][0]["check_name"], "workspace_roots_contract");
        assert_eq!(host["checks"][0]["ok"], true);

        let model = services.get("model").expect("model cutover should exist");
        let model_service_ok = model["service_ok"]
            .as_bool()
            .expect("model service_ok should serialize as bool");
        assert!(
            model["checks"]
                .as_array()
                .expect("model checks should be an array")
                .iter()
                .any(|check| check["target"] == "shadow-model" && check["ok"] == true),
            "shadow-model cutover contract should always be present: {model:?}"
        );
        let openai_health = services_snapshot["services"]
            .as_array()
            .expect("bridge services should be an array")
            .iter()
            .find(|entry| entry["server_kind"] == "model")
            .and_then(|entry| entry["service_catalog"]["services"].as_array())
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|service| service["service_name"] == "openai-compatible")
            })
            .and_then(|service| service["service_health"].as_str());
        if let Some(openai_health) = openai_health {
            let openai = model["checks"]
                .as_array()
                .expect("model checks should be an array")
                .iter()
                .find(|check| check["target"] == "openai-compatible")
                .expect("cataloged openai-compatible cutover check should exist");
            if openai_health == "ready" {
                assert!(model_service_ok);
                assert_eq!(openai["ok"], true);
                assert_eq!(openai["model_contract"]["contract_ok"], true);
            } else {
                assert!(!model_service_ok);
                assert_eq!(openai["ok"], false);
                assert_eq!(openai["blocking_reason"], "bridge invocation failed");
                assert_eq!(openai["error"]["layer"], "RemoteBusiness");
            }
        }

        let mcp = services.get("mcp").expect("mcp cutover should exist");
        let mcp_route_gate = &mcp["mcp_default_route_gate"];
        let route_status = mcp_route_gate["route_status"]
            .as_str()
            .expect("route status should serialize as string");
        match route_status {
            "ready" => {
                assert_eq!(mcp["service_ok"], true);
                assert_eq!(mcp["blocking_check_count"], 0);
                assert_eq!(mcp_route_gate["contract_ok"], true);
                assert_eq!(mcp["checks"][0]["ok"], true);
                assert_eq!(
                    mcp_route_gate["route_status"],
                    mcp["checks"][0]["mcp_contract"]["route_status"]
                );
                assert_eq!(
                    mcp_route_gate["route_target"],
                    mcp["checks"][0]["mcp_contract"]["route_target"]
                );
                assert_eq!(
                    mcp_route_gate["resolved_server"],
                    mcp["checks"][0]["mcp_contract"]["resolved_server"]
                );
                assert_eq!(
                    mcp_route_gate["contract_ok"],
                    mcp["checks"][0]["mcp_contract"]["contract_ok"]
                );
                assert_eq!(
                    mcp_route_gate["resolved_server"],
                    mcp_route_gate["route_target"]
                );
                if model_service_ok {
                    assert_eq!(snapshot["overall_ok"], true);
                    assert_eq!(snapshot["blocking_check_count"], 0);
                    assert!(
                        reason_code_counts.is_empty(),
                        "ready snapshot should not export reason-code counts: {snapshot:?}"
                    );
                    assert!(
                        server_kind_counts.is_empty(),
                        "ready snapshot should not export server-kind counts: {snapshot:?}"
                    );
                    assert!(
                        snapshot["blocking_services"]
                            .as_array()
                            .expect("blocking services should serialize as array")
                            .is_empty()
                    );
                    assert!(blocking_issues.is_empty());
                } else {
                    assert_eq!(snapshot["overall_ok"], false);
                    assert!(
                        snapshot["blocking_check_count"]
                            .as_u64()
                            .expect("blocking count should serialize as u64")
                            >= 1
                    );
                    assert_eq!(
                        server_kind_counts
                            .get("model")
                            .and_then(|value| value.as_u64()),
                        Some(1)
                    );
                    assert!(
                        snapshot["blocking_services"]
                            .as_array()
                            .expect("blocking services should serialize as array")
                            .iter()
                            .any(|service| service == "model"),
                        "blocking summary should include model: {snapshot:?}"
                    );
                }
            }
            "fallback-only" | "unavailable" => {
                assert_eq!(snapshot["overall_ok"], false);
                assert!(
                    snapshot["blocking_check_count"]
                        .as_u64()
                        .expect("blocking count should serialize as u64")
                        >= 1
                );
                assert_eq!(mcp["service_ok"], false);
                assert!(
                    mcp["blocking_check_count"]
                        .as_u64()
                        .expect("service blocking count should serialize as u64")
                        >= 1
                );
                assert_eq!(blocking_issues.len(), 1);
                assert_eq!(mcp_route_gate["contract_ok"], false);
                assert_eq!(
                    blocking_issues[0]["server_kind"],
                    serde_json::Value::String("mcp".to_string())
                );
                assert_eq!(
                    blocking_issues[0]["reason_code"],
                    serde_json::Value::String(
                        if route_status == "fallback-only" {
                            "mcp_default_route_status_fallback_only"
                        } else {
                            "mcp_default_route_status_unavailable"
                        }
                        .to_string()
                    )
                );
                assert_eq!(
                    reason_code_counts
                        .get(if route_status == "fallback-only" {
                            "mcp_default_route_status_fallback_only"
                        } else {
                            "mcp_default_route_status_unavailable"
                        })
                        .and_then(|value| value.as_u64()),
                    Some(1)
                );
                assert_eq!(
                    server_kind_counts
                        .get("mcp")
                        .and_then(|value| value.as_u64()),
                    Some(1)
                );
                assert_eq!(mcp["checks"][0]["ok"], false);
                assert!(
                    mcp["checks"][0]["blocking_reason"].is_string(),
                    "blocking route should explain itself: {mcp:?}"
                );
                assert!(
                    blocking_issues.iter().any(|issue| {
                        issue["server_kind"] == serde_json::Value::String("mcp".to_string())
                            && issue["reason_code"]
                                == serde_json::Value::String(
                                    if route_status == "fallback-only" {
                                        "mcp_default_route_status_fallback_only"
                                    } else {
                                        "mcp_default_route_status_unavailable"
                                    }
                                    .to_string(),
                                )
                    }),
                    "blocking issues should retain the mcp route failure: {snapshot:?}"
                );
                assert!(
                    snapshot["blocking_services"]
                        .as_array()
                        .expect("blocking services should serialize as array")
                        .iter()
                        .any(|service| service == "mcp"),
                    "blocking summary should include mcp: {snapshot:?}"
                );
            }
            other => panic!("unexpected mcp default route status {other}: {mcp:?}"),
        }
    }

    #[tokio::test]
    async fn daemon_router_bridge_cutover_smoke_reflects_env_backed_provider_and_mcp_routes() {
        for binary_name in [
            "host_bridge_loopback",
            "model_bridge_loopback",
            "mcp_bridge_loopback",
        ] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let (base_url, receiver, handle) = spawn_http_stub(
            200,
            json!({
                "choices": [{
                    "message": {
                        "content": "hello from env-backed cutover smoke"
                    }
                }]
            }),
        );

        let state_root = temp_state_root("router-bridge-cutover-env");
        let config = DaemonConfig::new("127.0.0.1", 0, "shadow-test", state_root);
        let runtime = ShadowDaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap empty state");
        let (app, _) = runtime.router_with_bridge_env_for_tests(
            "shadow-test".to_string(),
            &[
                ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
                ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
                ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
                ("MAGI_MCP_MANAGER_DEFAULT_SERVER", "observability-default"),
                (
                    "MAGI_MCP_MANAGER_ENABLED_SERVERS",
                    "shadow-mcp-observability",
                ),
                ("MAGI_MCP_MANAGER_DISABLED_SERVERS", ""),
                (
                    "MAGI_MCP_MANAGER_SERVER_HEALTHS",
                    "shadow-mcp-observability=healthy,shadow-mcp=healthy",
                ),
            ],
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        let services = service_entries_by_kind(&snapshot);

        assert_eq!(
            snapshot["overall_ok"], true,
            "unexpected cutover snapshot: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_issue_counts_by_reason_code"]
                .as_object()
                .expect("reason-code counts should serialize as object")
                .is_empty(),
            "ready env-backed snapshot should not export reason-code counts: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_issue_counts_by_server_kind"]
                .as_object()
                .expect("server-kind counts should serialize as object")
                .is_empty(),
            "ready env-backed snapshot should not export server-kind counts: {snapshot:?}"
        );

        let model = services
            .get("model")
            .expect("model cutover snapshot should exist");
        let openai = model["checks"]
            .as_array()
            .expect("model checks should serialize as array")
            .iter()
            .find(|check| check["target"] == "openai-compatible")
            .expect("env-backed openai-compatible cutover check should exist");
        assert_eq!(openai["ok"], true);
        assert_eq!(openai["model_contract"]["contract_ok"], true);

        let request = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("http stub should receive env-backed provider request");
        handle.join().expect("http stub should join");
        assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer test-key")
        );
        let body: Value = serde_json::from_str(&request.body).expect("request body should be json");
        assert_eq!(body["model"], "gpt-test");

        let mcp = services
            .get("mcp")
            .expect("mcp cutover snapshot should exist");
        assert_eq!(mcp["service_ok"], true);
        assert_eq!(mcp["mcp_default_route_gate"]["route_status"], "ready");
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_target"],
            "shadow-mcp-observability"
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["resolved_server"],
            "shadow-mcp-observability"
        );
        assert_eq!(mcp["mcp_default_route_gate"]["contract_ok"], true);
    }

    #[tokio::test]
    async fn daemon_router_bridge_cutover_smoke_surfaces_env_backed_provider_failure_with_ready_mcp_route()
     {
        for binary_name in [
            "host_bridge_loopback",
            "model_bridge_loopback",
            "mcp_bridge_loopback",
        ] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let (base_url, receiver, handle) = spawn_http_stub(
            401,
            json!({
                "error": {
                    "message": "bad api key",
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
            }),
        );

        let state_root = temp_state_root("router-bridge-cutover-env-failure");
        let config = DaemonConfig::new("127.0.0.1", 0, "shadow-test", state_root);
        let runtime = ShadowDaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap empty state");
        let (app, _) = runtime.router_with_bridge_env_for_tests(
            "shadow-test".to_string(),
            &[
                ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
                ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
                ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
                ("MAGI_MCP_MANAGER_DEFAULT_SERVER", "observability-default"),
                (
                    "MAGI_MCP_MANAGER_ENABLED_SERVERS",
                    "shadow-mcp-observability",
                ),
                ("MAGI_MCP_MANAGER_DISABLED_SERVERS", ""),
                (
                    "MAGI_MCP_MANAGER_SERVER_HEALTHS",
                    "shadow-mcp-observability=healthy,shadow-mcp=healthy",
                ),
            ],
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        let services = service_entries_by_kind(&snapshot);

        assert_eq!(
            snapshot["overall_ok"], false,
            "unexpected cutover snapshot: {snapshot:?}"
        );
        assert_eq!(snapshot["blocking_check_count"], 2);
        assert_eq!(
            snapshot["blocking_issue_counts_by_reason_code"]["model_provider_rejected"],
            2
        );
        assert_eq!(snapshot["blocking_issue_counts_by_server_kind"]["model"], 2);
        assert!(
            snapshot["blocking_issue_counts_by_server_kind"]
                .get("mcp")
                .is_none(),
            "ready MCP route should not contribute blocking server-kind counts: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_services"]
                .as_array()
                .expect("blocking services should serialize as array")
                .iter()
                .any(|service| service == "model"),
            "blocking summary should include model: {snapshot:?}"
        );
        assert_eq!(
            snapshot["blocking_issues"].as_array().map(Vec::len),
            Some(2)
        );

        let model = services
            .get("model")
            .expect("model cutover snapshot should exist");
        let openai = model["checks"]
            .as_array()
            .expect("model checks should serialize as array")
            .iter()
            .find(|check| check["target"] == "openai-compatible")
            .expect("env-backed openai-compatible cutover check should exist");
        assert_eq!(model["service_ok"], false);
        assert_eq!(openai["ok"], false);
        assert_eq!(openai["blocking_reason"], "bridge invocation failed");
        assert_eq!(openai["error"]["layer"], "RemoteBusiness");
        assert_eq!(openai["error"]["code"], -32006);
        assert!(
            openai["error"]["message"]
                .as_str()
                .expect("model error should serialize as string")
                .contains("provider rejected request"),
            "provider failure should retain upstream bridge error: {openai:?}"
        );

        let request = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("http stub should receive env-backed provider request");
        let _direct_http_request = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("http stub should receive direct HTTP probe request");
        handle.join().expect("http stub should join");
        assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");

        let mcp = services
            .get("mcp")
            .expect("mcp cutover snapshot should exist");
        assert_eq!(mcp["service_ok"], true);
        assert_eq!(mcp["mcp_default_route_gate"]["route_status"], "ready");
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_target"],
            "shadow-mcp-observability"
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["resolved_server"],
            "shadow-mcp-observability"
        );
        assert_eq!(mcp["mcp_default_route_gate"]["contract_ok"], true);
        assert_eq!(mcp["checks"][0]["ok"], true);
    }

    #[tokio::test]
    async fn daemon_router_bridge_cutover_smoke_surfaces_cataloged_degraded_provider_with_ready_mcp_route()
     {
        for binary_name in [
            "host_bridge_loopback",
            "model_bridge_loopback",
            "mcp_bridge_loopback",
        ] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let state_root = temp_state_root("router-bridge-cutover-env-degraded-provider");
        let config = DaemonConfig::new("127.0.0.1", 0, "shadow-test", state_root);
        let runtime = ShadowDaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap empty state");
        let (app, _) = runtime.router_with_bridge_env_for_tests(
            "shadow-test".to_string(),
            &[
                ("MAGI_MCP_MANAGER_DEFAULT_SERVER", "observability-default"),
                (
                    "MAGI_MCP_MANAGER_ENABLED_SERVERS",
                    "shadow-mcp-observability",
                ),
                ("MAGI_MCP_MANAGER_DISABLED_SERVERS", ""),
                (
                    "MAGI_MCP_MANAGER_SERVER_HEALTHS",
                    "shadow-mcp-observability=healthy,shadow-mcp=healthy",
                ),
            ],
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        let services = service_entries_by_kind(&snapshot);

        assert_eq!(
            snapshot["overall_ok"], false,
            "unexpected cutover snapshot: {snapshot:?}"
        );
        assert_eq!(snapshot["blocking_check_count"], 1);
        assert_eq!(
            snapshot["blocking_issue_counts_by_reason_code"]["model_provider_unavailable"],
            1
        );
        assert_eq!(snapshot["blocking_issue_counts_by_server_kind"]["model"], 1);
        assert!(
            snapshot["blocking_issue_counts_by_server_kind"]
                .get("mcp")
                .is_none(),
            "ready MCP route should not contribute blocking server-kind counts: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_services"]
                .as_array()
                .expect("blocking services should serialize as array")
                .iter()
                .any(|service| service == "model"),
            "blocking summary should include model: {snapshot:?}"
        );

        let model = services
            .get("model")
            .expect("model cutover snapshot should exist");
        let openai = model["checks"]
            .as_array()
            .expect("model checks should serialize as array")
            .iter()
            .find(|check| check["target"] == "openai-compatible")
            .expect("cataloged degraded openai-compatible cutover check should exist");
        assert_eq!(model["service_ok"], false);
        assert_eq!(openai["ok"], false);
        assert_eq!(openai["blocking_reason"], "bridge invocation failed");
        assert_eq!(openai["error"]["layer"], "RemoteBusiness");
        assert_eq!(openai["error"]["code"], -32003);
        assert!(
            openai["error"]["message"]
                .as_str()
                .expect("model error should serialize as string")
                .contains("provider unavailable"),
            "degraded provider should keep unavailable error detail: {openai:?}"
        );

        let mcp = services
            .get("mcp")
            .expect("mcp cutover snapshot should exist");
        assert_eq!(mcp["service_ok"], true);
        assert_eq!(mcp["mcp_default_route_gate"]["route_status"], "ready");
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_target"],
            "shadow-mcp-observability"
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["resolved_server"],
            "shadow-mcp-observability"
        );
        assert_eq!(mcp["mcp_default_route_gate"]["contract_ok"], true);
        assert_eq!(mcp["checks"][0]["ok"], true);
    }

    #[tokio::test]
    async fn daemon_router_bridge_cutover_smoke_surfaces_env_backed_provider_invalid_response_with_ready_mcp_route()
     {
        for binary_name in [
            "host_bridge_loopback",
            "model_bridge_loopback",
            "mcp_bridge_loopback",
        ] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let (base_url, receiver, handle) = spawn_http_stub(
            200,
            json!({
                "choices": [{
                    "message": {}
                }]
            }),
        );

        let state_root = temp_state_root("router-bridge-cutover-env-invalid-response");
        let config = DaemonConfig::new("127.0.0.1", 0, "shadow-test", state_root);
        let runtime = ShadowDaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap empty state");
        let (app, _) = runtime.router_with_bridge_env_for_tests(
            "shadow-test".to_string(),
            &[
                ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
                ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
                ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
                ("MAGI_MCP_MANAGER_DEFAULT_SERVER", "observability-default"),
                (
                    "MAGI_MCP_MANAGER_ENABLED_SERVERS",
                    "shadow-mcp-observability",
                ),
                ("MAGI_MCP_MANAGER_DISABLED_SERVERS", ""),
                (
                    "MAGI_MCP_MANAGER_SERVER_HEALTHS",
                    "shadow-mcp-observability=healthy,shadow-mcp=healthy",
                ),
            ],
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        let services = service_entries_by_kind(&snapshot);

        assert_eq!(
            snapshot["overall_ok"], false,
            "unexpected cutover snapshot: {snapshot:?}"
        );
        assert_eq!(snapshot["blocking_check_count"], 2);
        assert_eq!(
            snapshot["blocking_issue_counts_by_reason_code"]["model_provider_invalid_response"],
            2
        );
        assert_eq!(snapshot["blocking_issue_counts_by_server_kind"]["model"], 2);
        assert!(
            snapshot["blocking_issue_counts_by_server_kind"]
                .get("mcp")
                .is_none(),
            "ready MCP route should not contribute blocking server-kind counts: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_services"]
                .as_array()
                .expect("blocking services should serialize as array")
                .iter()
                .any(|service| service == "model"),
            "blocking summary should include model: {snapshot:?}"
        );
        assert_eq!(
            snapshot["blocking_issues"].as_array().map(Vec::len),
            Some(2)
        );

        let model = services
            .get("model")
            .expect("model cutover snapshot should exist");
        let openai = model["checks"]
            .as_array()
            .expect("model checks should serialize as array")
            .iter()
            .find(|check| check["target"] == "openai-compatible")
            .expect("env-backed openai-compatible cutover check should exist");
        assert_eq!(model["service_ok"], false);
        assert_eq!(openai["ok"], false);
        assert_eq!(openai["blocking_reason"], "bridge invocation failed");
        assert_eq!(openai["error"]["layer"], "RemoteBusiness");
        assert_eq!(openai["error"]["code"], -32007);
        let error_message = openai["error"]["message"]
            .as_str()
            .expect("model error should serialize as string");
        assert!(
            error_message.contains("provider response invalid"),
            "invalid provider response should retain bridge error: {openai:?}"
        );

        let request = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("http stub should receive env-backed provider request");
        let _direct_http_request = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("http stub should receive direct HTTP probe request");
        handle.join().expect("http stub should join");
        assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");

        let mcp = services
            .get("mcp")
            .expect("mcp cutover snapshot should exist");
        assert_eq!(mcp["service_ok"], true);
        assert_eq!(mcp["mcp_default_route_gate"]["route_status"], "ready");
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_target"],
            "shadow-mcp-observability"
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["resolved_server"],
            "shadow-mcp-observability"
        );
        assert_eq!(mcp["mcp_default_route_gate"]["contract_ok"], true);
        assert_eq!(mcp["checks"][0]["ok"], true);
    }

    #[tokio::test]
    async fn daemon_router_bridge_cutover_smoke_surfaces_env_backed_mcp_fallback_only_route() {
        for binary_name in [
            "host_bridge_loopback",
            "model_bridge_loopback",
            "mcp_bridge_loopback",
        ] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let (base_url, receiver, handle) = spawn_http_stub(
            200,
            json!({
                "choices": [{
                    "message": {
                        "content": "hello from env-backed cutover smoke"
                    }
                }]
            }),
        );

        let state_root = temp_state_root("router-bridge-cutover-env-fallback-only");
        let config = DaemonConfig::new("127.0.0.1", 0, "shadow-test", state_root);
        let runtime = ShadowDaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap empty state");
        let (app, _) = runtime.router_with_bridge_env_for_tests(
            "shadow-test".to_string(),
            &[
                ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
                ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
                ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
                ("MAGI_MCP_MANAGER_DEFAULT_SERVER", "observability-default"),
                (
                    "MAGI_MCP_MANAGER_ENABLED_SERVERS",
                    "shadow-mcp-observability,shadow-mcp",
                ),
                ("MAGI_MCP_MANAGER_DISABLED_SERVERS", ""),
                (
                    "MAGI_MCP_MANAGER_SERVER_HEALTHS",
                    "shadow-mcp-observability=degraded,shadow-mcp=healthy",
                ),
            ],
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        let services = service_entries_by_kind(&snapshot);

        assert_eq!(
            snapshot["overall_ok"], false,
            "unexpected cutover snapshot: {snapshot:?}"
        );
        assert_eq!(snapshot["blocking_check_count"], 1);
        assert_eq!(
            snapshot["blocking_issue_counts_by_reason_code"]["mcp_default_route_status_fallback_only"],
            1
        );
        assert_eq!(snapshot["blocking_issue_counts_by_server_kind"]["mcp"], 1);
        assert!(
            snapshot["blocking_issue_counts_by_server_kind"]
                .get("model")
                .is_none(),
            "ready provider route should not contribute blocking server-kind counts: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_services"]
                .as_array()
                .expect("blocking services should serialize as array")
                .iter()
                .any(|service| service == "mcp"),
            "blocking summary should include mcp: {snapshot:?}"
        );
        assert_eq!(
            snapshot["blocking_issues"].as_array().map(Vec::len),
            Some(1)
        );

        let model = services
            .get("model")
            .expect("model cutover snapshot should exist");
        let openai = model["checks"]
            .as_array()
            .expect("model checks should serialize as array")
            .iter()
            .find(|check| check["target"] == "openai-compatible")
            .expect("env-backed openai-compatible cutover check should exist");
        assert_eq!(model["service_ok"], true);
        assert_eq!(openai["ok"], true);
        assert_eq!(openai["model_contract"]["contract_ok"], true);

        let request = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("http stub should receive env-backed provider request");
        handle.join().expect("http stub should join");
        assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");

        let mcp = services
            .get("mcp")
            .expect("mcp cutover snapshot should exist");
        assert_eq!(mcp["service_ok"], false);
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_status"],
            "fallback-only"
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_target"],
            "shadow-mcp-observability"
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["resolved_server"],
            "shadow-mcp-observability"
        );
        assert_eq!(mcp["mcp_default_route_gate"]["contract_ok"], false);
        assert_eq!(mcp["checks"][0]["ok"], false);
        assert_eq!(
            mcp["checks"][0]["blocking_reason"],
            "default route is fallback-only"
        );
    }

    #[tokio::test]
    async fn daemon_router_bridge_cutover_smoke_surfaces_env_backed_mcp_unavailable_route() {
        for binary_name in [
            "host_bridge_loopback",
            "model_bridge_loopback",
            "mcp_bridge_loopback",
        ] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let (base_url, receiver, handle) = spawn_http_stub(
            200,
            json!({
                "choices": [{
                    "message": {
                        "content": "hello from env-backed cutover smoke"
                    }
                }]
            }),
        );

        let state_root = temp_state_root("router-bridge-cutover-env-unavailable");
        let config = DaemonConfig::new("127.0.0.1", 0, "shadow-test", state_root);
        let runtime = ShadowDaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap empty state");
        let (app, _) = runtime.router_with_bridge_env_for_tests(
            "shadow-test".to_string(),
            &[
                ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
                ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
                ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
                ("MAGI_MCP_MANAGER_DEFAULT_SERVER", "observability-default"),
                (
                    "MAGI_MCP_MANAGER_ENABLED_SERVERS",
                    "shadow-mcp-observability",
                ),
                ("MAGI_MCP_MANAGER_DISABLED_SERVERS", "shadow-mcp"),
                (
                    "MAGI_MCP_MANAGER_SERVER_HEALTHS",
                    "shadow-mcp-observability=unavailable",
                ),
            ],
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        let services = service_entries_by_kind(&snapshot);

        assert_eq!(
            snapshot["overall_ok"], false,
            "unexpected cutover snapshot: {snapshot:?}"
        );
        assert_eq!(snapshot["blocking_check_count"], 1);
        assert_eq!(
            snapshot["blocking_issue_counts_by_reason_code"]["mcp_default_route_status_unavailable"],
            1
        );
        assert_eq!(snapshot["blocking_issue_counts_by_server_kind"]["mcp"], 1);
        assert!(
            snapshot["blocking_issue_counts_by_server_kind"]
                .get("model")
                .is_none(),
            "ready provider route should not contribute blocking server-kind counts: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_services"]
                .as_array()
                .expect("blocking services should serialize as array")
                .iter()
                .any(|service| service == "mcp"),
            "blocking summary should include mcp: {snapshot:?}"
        );
        assert_eq!(
            snapshot["blocking_issues"].as_array().map(Vec::len),
            Some(1)
        );

        let model = services
            .get("model")
            .expect("model cutover snapshot should exist");
        let openai = model["checks"]
            .as_array()
            .expect("model checks should serialize as array")
            .iter()
            .find(|check| check["target"] == "openai-compatible")
            .expect("env-backed openai-compatible cutover check should exist");
        assert_eq!(model["service_ok"], true);
        assert_eq!(openai["ok"], true);
        assert_eq!(openai["model_contract"]["contract_ok"], true);

        let request = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("http stub should receive env-backed provider request");
        handle.join().expect("http stub should join");
        assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");

        let mcp = services
            .get("mcp")
            .expect("mcp cutover snapshot should exist");
        assert_eq!(mcp["service_ok"], false);
        assert_eq!(mcp["mcp_default_route_gate"]["route_status"], "unavailable");
        assert_eq!(mcp["mcp_default_route_gate"]["route_target"], "<none>");
        assert!(mcp["mcp_default_route_gate"]["resolved_server"].is_null());
        assert_eq!(mcp["mcp_default_route_gate"]["contract_ok"], false);
        assert_eq!(mcp["checks"][0]["target"], "<none>");
        assert_eq!(mcp["checks"][0]["ok"], false);
        assert_eq!(
            mcp["checks"][0]["blocking_reason"],
            "default route is unavailable"
        );
    }

    #[tokio::test]
    async fn daemon_router_bridge_cutover_smoke_surfaces_env_backed_provider_transport_failure_with_ready_mcp_route()
     {
        for binary_name in [
            "host_bridge_loopback",
            "model_bridge_loopback",
            "mcp_bridge_loopback",
        ] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        // Bind a port, capture the address, then drop the listener so nothing
        // is listening — any connection attempt will be refused.
        let unreachable_address = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral bind should succeed");
            let address = listener.local_addr().expect("bound address should exist");
            drop(listener);
            address
        };
        let unreachable_url = format!("http://{unreachable_address}/v1");

        let state_root = temp_state_root("router-bridge-cutover-env-transport-failure");
        let config = DaemonConfig::new("127.0.0.1", 0, "shadow-test", state_root);
        let runtime = ShadowDaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap empty state");
        let (app, _) = runtime.router_with_bridge_env_for_tests(
            "shadow-test".to_string(),
            &[
                ("MAGI_OPENAI_COMPAT_BASE_URL", unreachable_url.as_str()),
                ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
                ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
                ("MAGI_MCP_MANAGER_DEFAULT_SERVER", "observability-default"),
                (
                    "MAGI_MCP_MANAGER_ENABLED_SERVERS",
                    "shadow-mcp-observability",
                ),
                ("MAGI_MCP_MANAGER_DISABLED_SERVERS", ""),
                (
                    "MAGI_MCP_MANAGER_SERVER_HEALTHS",
                    "shadow-mcp-observability=healthy,shadow-mcp=healthy",
                ),
            ],
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        let services = service_entries_by_kind(&snapshot);

        assert_eq!(
            snapshot["overall_ok"], false,
            "unreachable provider should block cutover: {snapshot:?}"
        );
        assert_eq!(snapshot["blocking_check_count"], 2);
        assert_eq!(
            snapshot["blocking_issue_counts_by_reason_code"]["model_provider_transport_failed"],
            2
        );
        assert_eq!(snapshot["blocking_issue_counts_by_server_kind"]["model"], 2);
        assert!(
            snapshot["blocking_issue_counts_by_server_kind"]
                .get("mcp")
                .is_none(),
            "ready MCP route should not contribute blocking server-kind counts: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_services"]
                .as_array()
                .expect("blocking services should serialize as array")
                .iter()
                .any(|service| service == "model"),
            "blocking summary should include model: {snapshot:?}"
        );
        assert_eq!(
            snapshot["blocking_issues"].as_array().map(Vec::len),
            Some(2)
        );

        let model = services
            .get("model")
            .expect("model cutover snapshot should exist");
        let openai = model["checks"]
            .as_array()
            .expect("model checks should serialize as array")
            .iter()
            .find(|check| check["target"] == "openai-compatible")
            .expect("env-backed openai-compatible cutover check should exist");
        assert_eq!(model["service_ok"], false);
        assert_eq!(openai["ok"], false);
        assert_eq!(openai["blocking_reason"], "bridge invocation failed");
        assert_eq!(openai["error"]["layer"], "RemoteBusiness");
        assert_eq!(openai["error"]["code"], -32005);
        let error_message = openai["error"]["message"]
            .as_str()
            .expect("model error should serialize as string");
        assert!(
            error_message.contains("provider transport failed"),
            "transport failure should retain bridge error: {openai:?}"
        );

        let mcp = services
            .get("mcp")
            .expect("mcp cutover snapshot should exist");
        assert_eq!(mcp["service_ok"], true);
        assert_eq!(mcp["mcp_default_route_gate"]["route_status"], "ready");
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_target"],
            "shadow-mcp-observability"
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["resolved_server"],
            "shadow-mcp-observability"
        );
        assert_eq!(mcp["mcp_default_route_gate"]["contract_ok"], true);
        assert_eq!(mcp["checks"][0]["ok"], true);
    }

    #[tokio::test]
    async fn daemon_router_bridge_routes_do_not_touch_execution_state() {
        let state_root = temp_state_root("router-bridge-guard");
        let config = DaemonConfig::new("127.0.0.1", 0, "shadow-test", state_root);
        let runtime = ShadowDaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap empty state");
        let (app, state) = runtime.router_with_state_for_tests("shadow-test".to_string());

        let before_runtime_read_model = serde_json::to_value(state.runtime_read_model_dto())
            .expect("runtime read model should serialize");
        let before_session_sidecars =
            serde_json::to_value(state.session_store.execution_sidecar_exports())
                .expect("session sidecars should serialize");
        let before_workspace_sidecars =
            serde_json::to_value(state.workspace_registry.recovery_sidecar_exports())
                .expect("workspace sidecars should serialize");

        let _ = get_json(app.clone(), "/bridges/preflight").await;
        let _ = get_json(app, "/bridges/cutover-smoke").await;

        assert_eq!(
            serde_json::to_value(state.runtime_read_model_dto())
                .expect("runtime read model should serialize"),
            before_runtime_read_model
        );
        assert_eq!(
            serde_json::to_value(state.session_store.execution_sidecar_exports())
                .expect("session sidecars should serialize"),
            before_session_sidecars
        );
        assert_eq!(
            serde_json::to_value(state.workspace_registry.recovery_sidecar_exports())
                .expect("workspace sidecars should serialize"),
            before_workspace_sidecars
        );

        assert!(
            state
                .shadow_execution_pipeline()
                .expect("shadow execution pipeline should exist")
                .memory_store
                .extraction_results_for_session(&magi_core::SessionId::new("bridge-route-guard"))
                .is_empty()
        );
    }

    #[tokio::test]
    async fn daemon_bootstrap_exports_bridge_services_and_preflight_snapshots() {
        for binary_name in [
            "host_bridge_loopback",
            "model_bridge_loopback",
            "mcp_bridge_loopback",
        ] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let state_root = temp_state_root("router-bootstrap-bridges");
        let config = DaemonConfig::new("127.0.0.1", 0, "shadow-test", state_root);
        let runtime = ShadowDaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap empty state");
        let app = runtime.router("shadow-test".to_string());

        let bootstrap = get_json(app.clone(), "/bootstrap").await;
        let bridge_services = get_json(app.clone(), "/bridges/services").await;
        let bridge_preflight = get_json(app, "/bridges/preflight").await;

        assert_eq!(bootstrap["bridgeServices"], bridge_services);
        assert_eq!(bootstrap["bridgePreflight"], bridge_preflight);

        let services = service_entries_by_kind(&bootstrap["bridgeServices"]);
        assert_eq!(
            services.len(),
            3,
            "unexpected bootstrap bridge services: {bootstrap:?}"
        );

        let preflight = service_entries_by_kind(&bootstrap["bridgePreflight"]);
        assert_eq!(
            preflight.len(),
            3,
            "unexpected bootstrap bridge preflight: {bootstrap:?}"
        );
    }
}
