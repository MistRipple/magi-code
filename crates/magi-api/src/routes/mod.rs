mod changes_files_tunnel;
mod conversation_bridge;
mod dispatch_flow;
mod knowledge;
mod mcp_skills_repos;
mod messages;
mod session_scope;
mod sessions;
pub(crate) mod settings;
mod tasks_graph;
mod tasks_interaction;
mod workspaces;

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use magi_core::{SessionId, UtcMillis, WorkspaceId};
use serde::Deserialize;
use std::sync::atomic::{AtomicU64, Ordering};

static ACCEPTED_AT_COUNTER: AtomicU64 = AtomicU64::new(0);
static SESSION_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn monotonic_accepted_at() -> UtcMillis {
    let now = UtcMillis::now().0;
    let prev = ACCEPTED_AT_COUNTER.fetch_max(now, Ordering::SeqCst);
    let ts = if now <= prev { prev + 1 } else { now };
    ACCEPTED_AT_COUNTER.store(ts, Ordering::SeqCst);
    UtcMillis(ts)
}

pub(super) fn new_session_id() -> SessionId {
    let nonce = SESSION_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
    SessionId::new(format!("session-{}-{}", UtcMillis::now().0, nonce))
}

use crate::{
    dto::{
        AuditUsageLedgerDto, BootstrapDto, BridgeCutoverSmokeSnapshotDto,
        BridgePreflightSnapshotDto, BridgeServicesSnapshotDto, HealthDto, RuntimeReadModelDto,
        VersionHandshakeDto,
    },
    errors::ApiError,
    sse,
    state::ApiState,
};

use conversation_bridge::{
    begin_session_turn, finalize_session_turn, ingest_user_input_to_conversation,
};
use dispatch_flow::{
    accept_session_task_submission, append_dispatch_assistant_message,
    finalize_session_task_dispatch, resolve_dispatch_session,
};

pub fn build_router(state: ApiState) -> Router {
    let api_routes = Router::new()
        .merge(workspaces::routes())
        .merge(sessions::routes())
        .merge(knowledge::routes())
        .merge(settings::routes())
        .merge(mcp_skills_repos::routes())
        .merge(changes_files_tunnel::routes())
        .merge(tasks_interaction::routes())
        .merge(tasks_graph::routes())
        .merge(messages::routes());

    Router::new()
        .route("/health", get(health))
        .route("/bootstrap", get(bootstrap))
        .route("/runtime/read-model", get(runtime_read_model))
        .route("/ledger", get(ledger))
        .route("/bridges/services", get(bridge_services))
        .route("/bridges/preflight", get(bridge_preflight))
        .route("/bridges/cutover-smoke", get(bridge_cutover_smoke))
        .route("/events", get(stream_events))
        .route("/version", get(version))
        .nest("/api", api_routes)
        .with_state(state)
}

async fn health(State(state): State<ApiState>) -> Json<HealthDto> {
    Json(state.health_dto())
}

#[derive(Debug, Deserialize)]
struct BootstrapQuery {
    #[serde(rename = "sessionId", alias = "session_id")]
    session_id: Option<String>,
    #[serde(rename = "workspaceId", alias = "workspace_id")]
    workspace_id: Option<String>,
    #[serde(rename = "workspacePath", alias = "workspace_path")]
    workspace_path: Option<String>,
}

impl BootstrapQuery {
    fn requested_session_id(&self) -> Option<SessionId> {
        self.session_id
            .as_deref()
            .map(str::trim)
            .filter(|session_id| !session_id.is_empty())
            .map(SessionId::new)
    }

    fn requested_workspace_id(&self) -> Option<String> {
        self.workspace_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(String::from)
    }
}

async fn bootstrap(
    State(state): State<ApiState>,
    Query(query): Query<BootstrapQuery>,
) -> Result<Json<BootstrapDto>, ApiError> {
    let requested_session_id = query.requested_session_id();
    let workspace_id = resolve_bootstrap_workspace_id(
        &state,
        query.requested_workspace_id(),
        query.workspace_path.clone(),
        requested_session_id.as_ref(),
    );
    Ok(Json(state.bootstrap_dto_for_workspace_session(
        workspace_id.as_deref(),
        requested_session_id.as_ref(),
    )?))
}

fn resolve_bootstrap_workspace_id(
    state: &ApiState,
    requested_workspace_id: Option<String>,
    requested_workspace_path: Option<String>,
    requested_session_id: Option<&SessionId>,
) -> Option<String> {
    if let Some(workspace_id) = state.resolve_workspace_id_from_request(
        requested_workspace_id.map(WorkspaceId::new),
        requested_workspace_path.as_deref(),
    ) {
        return Some(workspace_id.to_string());
    }
    if let Some(session_id) = requested_session_id
        && let Some(session) = state.session_store.session(session_id)
        && let Some(workspace_id) = session_scope::session_workspace_id(state, &session)
    {
        return Some(workspace_id.to_string());
    }
    state
        .workspace_registry
        .active_workspace_id()
        .map(|workspace_id| workspace_id.to_string())
}

async fn runtime_read_model(State(state): State<ApiState>) -> Json<RuntimeReadModelDto> {
    Json(state.runtime_read_model_dto())
}

async fn ledger(State(state): State<ApiState>) -> Json<AuditUsageLedgerDto> {
    Json(state.audit_usage_ledger_dto())
}

async fn bridge_services(State(state): State<ApiState>) -> Json<BridgeServicesSnapshotDto> {
    Json(state.bridge_services_dto())
}

async fn bridge_preflight(State(state): State<ApiState>) -> Json<BridgePreflightSnapshotDto> {
    Json(state.bridge_preflight_dto())
}

async fn bridge_cutover_smoke(
    State(state): State<ApiState>,
) -> Json<BridgeCutoverSmokeSnapshotDto> {
    Json(state.bridge_cutover_smoke_dto())
}

async fn version(State(state): State<ApiState>) -> Json<VersionHandshakeDto> {
    Json(state.version_handshake_dto())
}

#[derive(Debug, Deserialize)]
struct EventStreamQuery {
    #[serde(rename = "workspaceId", alias = "workspace_id")]
    workspace_id: Option<String>,
}

async fn stream_events(
    State(state): State<ApiState>,
    Query(query): Query<EventStreamQuery>,
) -> impl axum::response::IntoResponse {
    sse::events(state, query.workspace_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{
        BridgeProbeErrorDto, BridgeServiceSnapshotDto, BridgeServicesSnapshotDto,
        BridgeSnapshotProvider,
    };
    use magi_conversation_runtime::session_writeback::session_turn_item;
    use crate::state::{RunnerManager, RunnerStartError};
    use crate::task_execution::{
        DispatchSubmissionAccepted, LlmTaskDispatcher, drive_dispatch_submission,
        submit_dispatch_submission,
    };
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use magi_bridge_client::{
        BridgeErrorLayer, BridgeResponse, BridgeServerHandshake, BridgeServerHealth,
        BridgeServerKind, BridgeServerServiceCatalog, BridgeTransport, BridgeTransportError,
        BridgeTransportRequest, BridgeTransportResponse, LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD,
        LOCAL_BRIDGE_HANDSHAKE_METHOD, LOCAL_BRIDGE_HEALTH_METHOD, McpManagerListServersResponse,
        ModelBridgeClient, ModelInvocationRequest, ModelStreamingDelta, LOOPBACK_MCP_SERVER_NAME,
        LOOPBACK_MCP_TOOL_NAME, LOOPBACK_MODEL_PROVIDER,
    };
    use magi_context_runtime::{ContextBudget, ContextRuntime};
    use magi_core::{
        AbsolutePath, DecisionOption, DecisionTaskPayload, EventId, ExecutionOwnership,
        ExecutorBinding, LeaseId, MissionId, SessionId, Task, TaskId, TaskKind, TaskStatus,
        ThreadId, UtcMillis, WorkerId, WorkspaceId,
    };
    use magi_event_bus::{EventEnvelope, InMemoryEventBus};
    use magi_governance::GovernanceService;
    use magi_knowledge_store::KnowledgeStore;
    use magi_memory_store::MemoryStore;
    use magi_orchestrator::task_runner::{EventBasedResultReceiver, TaskOutcome, TaskResult};
    use magi_orchestrator::{ExecutionContextConfig, OrchestratorService, task_store::TaskStore};
    use magi_session_store::{
        ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
        ActiveExecutionTurn, ActiveExecutionTurnItem, CanonicalTurnItemKind,
        CanonicalTurnItemStatus, ExecutionThread, ExecutionThreadStatus, SessionStore,
        ThreadVisibility, TimelineEntryKind,
    };
    use magi_skill_runtime::SkillDispatchRuntime;
    use magi_tool_runtime::ToolRegistry;
    use magi_worker_runtime::WorkerRuntime;
    use magi_workspace::WorkspaceStore;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use std::{
        collections::{HashMap, HashSet},
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };
    use tower::util::ServiceExt;

    fn test_state() -> ApiState {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let _ = event_bus.publish(EventEnvelope::usage(
            EventId::new("event-usage-1"),
            "usage.token.recorded",
            json!({ "tokens": 11 }),
        ));

        ApiState::new(
            "magi",
            event_bus,
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
    }

    fn build_execution_state(
        worker_runtime_factory: impl FnOnce(Arc<InMemoryEventBus>) -> WorkerRuntime,
        model_bridge_client: Arc<dyn ModelBridgeClient>,
    ) -> ApiState {
        build_execution_state_with_factory(worker_runtime_factory, |_| model_bridge_client)
    }

    fn build_execution_state_with_factory(
        worker_runtime_factory: impl FnOnce(Arc<InMemoryEventBus>) -> WorkerRuntime,
        model_bridge_client_factory: impl FnOnce(Arc<SessionStore>) -> Arc<dyn ModelBridgeClient>,
    ) -> ApiState {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let governance = Arc::new(GovernanceService::default());
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let model_bridge_client = model_bridge_client_factory(Arc::clone(&session_store));

        let session_id = SessionId::new("session-route-loopback");
        session_store
            .create_session(session_id.clone(), "Route Loopback Session")
            .expect("loopback route session should be creatable");

        let workspace_id = WorkspaceId::new("workspace-route-loopback");
        workspace_store
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/Users/xie/code/magi-rust-rewrite"),
            )
            .expect("loopback route workspace should register");
        workspace_store
            .activate(&workspace_id)
            .expect("loopback route workspace should activate");
        session_store.bind_execution_ownership(
            session_id.clone(),
            ExecutionOwnership {
                session_id: Some(session_id.clone()),
                workspace_id: Some(workspace_id.clone()),
                execution_chain_ref: Some("route-chain".to_string()),
                ..ExecutionOwnership::default()
            },
        );

        let knowledge_store = {
            let store = KnowledgeStore::new();
            store.ingest_code_index_in_workspace(
                workspace_id,
                magi_knowledge_store::CodeIndexIngestion {
                    knowledge_id: "kb-route-context-1".to_string(),
                    title: "Route parser refresh".to_string(),
                    content: "Refresh parser through session action route.".to_string(),
                    tags: vec!["parser".to_string(), "route".to_string()],
                    source_ref: Some("knowledge://route-context".to_string()),
                    updated_at: magi_core::UtcMillis::now(),
                    source: magi_knowledge_store::CodeIndexSource {
                        path: "src/routes.rs".to_string(),
                        language: Some("rust".to_string()),
                        repo_ref: Some("repo".to_string()),
                        commit_ref: Some("commit".to_string()),
                        start_line: Some(20),
                        end_line: Some(120),
                        symbol: None,
                    },
                    audit: Some(magi_knowledge_store::KnowledgeAuditLink {
                        audit_event_id: "audit-route-context-1".to_string(),
                        trail_ref: Some("audit/trails/route-context.json".to_string()),
                        sequence: Some(5),
                    }),
                    governance: Some(magi_knowledge_store::KnowledgeGovernanceLink {
                        outcome: magi_knowledge_store::KnowledgeGovernanceOutcome::Allowed,
                        policy_refs: vec!["policy.knowledge.read".to_string()],
                        rationale: Some("allowed for session action dispatch".to_string()),
                        audit_event_id: Some("audit-route-context-1".to_string()),
                    }),
                },
            );
            store
        };

        let memory_store = MemoryStore::new();

        let orchestrator = OrchestratorService::new(Arc::clone(&event_bus));
        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        let tool_registry_for_dispatcher = tool_registry.clone();
        let skill_runtime = SkillDispatchRuntime::new(
            tool_registry.clone(),
            magi_bridge_client::BridgeDispatchRuntime::new(),
        );
        let worker_runtime = worker_runtime_factory(Arc::clone(&event_bus));
        let context_runtime = ContextRuntime::new(knowledge_store, memory_store.clone());
        let context_runtime_for_dispatcher = Arc::new(context_runtime.clone());
        let runner_result_receiver = Arc::new(EventBasedResultReceiver::new());
        let receiver_for_status = runner_result_receiver.clone();
        let event_bus_for_status = event_bus.clone();
        let session_store_for_status = session_store.clone();
        let task_store = Arc::new(TaskStore::with_status_change_callback(Box::new(
            move |task_id, _old_status, new_status, task| {
                crate::task_execution::publish_task_status_turn_item_for_active_sessions(
                    event_bus_for_status.as_ref(),
                    session_store_for_status.as_ref(),
                    None,
                    &task,
                    new_status,
                );
                match new_status {
                    TaskStatus::Completed => {
                        receiver_for_status.push_result(TaskResult {
                            task_id: task_id.clone(),
                            lease_id: LeaseId::new(format!("lease-result-{}", task_id)),
                            outcome: TaskOutcome::Completed {
                                output_refs: Vec::new(),
                            },
                        });
                    }
                    TaskStatus::Failed => {
                        receiver_for_status.push_result(TaskResult {
                            task_id: task_id.clone(),
                            lease_id: LeaseId::new(format!("lease-result-{}", task_id)),
                            outcome: TaskOutcome::Failed {
                                error: "task store reported terminal failure".to_string(),
                            },
                        });
                    }
                    _ => {
                        receiver_for_status.clear_seen(task_id);
                    }
                }
            },
        )));
        let execution_runtime = orchestrator
            .execution_runtime(worker_runtime, tool_registry, skill_runtime)
            .with_task_store(Arc::clone(&task_store))
            .with_context_runtime(
                context_runtime,
                ExecutionContextConfig {
                    budget: ContextBudget {
                        max_turns: 4,
                        max_knowledge: 3,
                        max_memory: 3,
                        max_shared_items: 2,
                        max_file_summaries: 2,
                    },
                    project_key: Some("project-route-loopback".to_string()),
                },
            );

        let mut state = ApiState::new(
            "magi",
            event_bus.clone(),
            session_store,
            workspace_store,
            governance,
        )
        .with_execution_pipeline(orchestrator, execution_runtime, memory_store)
        .with_model_bridge_client(model_bridge_client.clone())
        .with_model_bridge_client(model_bridge_client.clone())
        .with_tool_registry(tool_registry_for_dispatcher.clone())
        .with_task_store(Arc::clone(&task_store));

        let state_for_task_workers = state.clone();
        let state_for_runner_terminal = state.clone();
        let dispatcher = Arc::new(
            LlmTaskDispatcher::new(
                event_bus,
                state
                    .execution_pipeline()
                    .expect("execution pipeline should exist")
                    .clone(),
                state.session_store.clone(),
                state.task_execution_registry().clone(),
                runner_result_receiver.clone(),
                state.spawn_graph.clone(),
            )
            .with_model_bridge_client(model_bridge_client)
            .with_context_runtime(context_runtime_for_dispatcher)
            .with_workspace_registry(state.workspace_registry.clone())
            .with_tool_registry(tool_registry_for_dispatcher)
            .with_conversation_registry(state.conversation_registry.clone())
            .with_stream_fanout(state.stream_fanout.clone())
            .with_agent_role_registry(state.agent_role_registry.clone()),
        );
        let runner_manager = RunnerManager::with_dispatcher_and_worker_catalog(
            task_store,
            Arc::new(move || state_for_task_workers.task_worker_catalog()),
            dispatcher.clone(),
            runner_result_receiver,
        )
        .with_terminal_observer(move |root_task_id, session_id, status| {
            let Some(session_id) = session_id else {
                return;
            };
            crate::task_execution::finalize_background_session_task_turn_if_root_terminal(
                &state_for_runner_terminal,
                &session_id,
                &root_task_id,
                &status,
            );
        });
        state = state
            .with_session_turn_dispatcher(dispatcher)
            .with_runner_manager(runner_manager);
        state
    }

    fn test_state_with_execution_pipeline() -> ApiState {
        build_execution_state(
            WorkerRuntime::new_compare,
            Arc::new(StaticModelBridgeClient),
        )
    }

    fn test_state_with_unhealthy_execution_pipeline() -> ApiState {
        build_execution_state(
            WorkerRuntime::new_compare,
            Arc::new(FailingModelBridgeClient),
        )
        .with_model_bridge_client(Arc::new(StaticModelBridgeClient))
    }

    fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            UtcMillis::now().0
        ));
        std::fs::create_dir_all(&path).expect("temp dir should create");
        path
    }

    async fn start_snapshot_for_session(state: &ApiState, session_id: &str, root: &std::path::Path) {
        state
            .snapshot_manager
            .start_session(session_id.to_string(), root.to_path_buf())
            .await
            .expect("snapshot session should start");
    }

    async fn get_json(app: Router, path: &str) -> serde_json::Value {
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

    async fn post_json(app: Router, path: &str, body: serde_json::Value) -> (StatusCode, Value) {
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

    async fn wait_for_condition(
        timeout: Duration,
        interval: Duration,
        mut condition: impl FnMut() -> bool,
        description: &str,
    ) {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if condition() {
                return;
            }
            if std::time::Instant::now() >= deadline {
                assert!(condition(), "{description}");
                return;
            }
            tokio::time::sleep(interval).await;
        }
    }

    fn bind_test_session_mission(
        state: &ApiState,
        session_id: &str,
        mission_id: &str,
        root_task_id: &str,
    ) {
        let session_id = SessionId::new(session_id);
        if state.session_store.session(&session_id).is_none() {
            state
                .session_store
                .create_session(session_id.clone(), format!("Test Session {session_id}"))
                .expect("test session should be creatable");
        }
        state.session_store.bind_execution_ownership(
            session_id.clone(),
            ExecutionOwnership {
                session_id: Some(session_id),
                mission_id: Some(MissionId::new(mission_id)),
                task_id: Some(TaskId::new(root_task_id)),
                ..ExecutionOwnership::default()
            },
        );
    }

    #[tokio::test]
    async fn runtime_read_model_ledger_and_bridge_routes_match_bootstrap_exports() {
        let app = build_router(test_state());

        let bootstrap = get_json(app.clone(), "/bootstrap").await;
        let runtime_read_model = get_json(app.clone(), "/runtime/read-model").await;
        let ledger = get_json(app.clone(), "/ledger").await;
        let bridge_services = get_json(app.clone(), "/bridges/services").await;
        let bridge_preflight = get_json(app, "/bridges/preflight").await;

        assert_eq!(bootstrap["runtimeReadModel"], runtime_read_model);
        assert_eq!(bootstrap["auditUsageLedger"], ledger);
        assert_eq!(bootstrap["bridgeServices"], bridge_services);
        assert_eq!(bootstrap["bridgePreflight"], bridge_preflight);
    }

    #[tokio::test]
    async fn bootstrap_handles_session_bound_to_unknown_workspace() {
        let state = test_state();
        let session_id = SessionId::new("session-bootstrap-unknown-workspace");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "未知工作区会话",
                Some("workspace-missing".to_string()),
            )
            .expect("session should create");
        state.session_store.append_notification(
            session_id.clone(),
            "notification-bootstrap-unknown-workspace",
            "incident",
            "未知工作区通知",
        );

        let app = build_router(state);
        let bootstrap = get_json(app, "/bootstrap").await;

        assert_eq!(
            bootstrap["currentSession"]["sessionId"],
            serde_json::json!(session_id.as_str())
        );
        assert_eq!(
            bootstrap["notifications"]
                .as_array()
                .expect("notifications should be an array")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn bootstrap_without_query_scopes_to_active_workspace_instead_of_global_current_session()
    {
        let state = test_state();
        let active_root = unique_temp_dir("workspace-bootstrap-active");
        let other_root = unique_temp_dir("workspace-bootstrap-other");
        state
            .workspace_registry
            .register(
                WorkspaceId::new("workspace-bootstrap-active"),
                AbsolutePath::new(active_root.display().to_string()),
            )
            .expect("active workspace should register first");
        state
            .workspace_registry
            .register(
                WorkspaceId::new("workspace-bootstrap-other"),
                AbsolutePath::new(other_root.display().to_string()),
            )
            .expect("other workspace should register");
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("session-bootstrap-active"),
                "Active Workspace Session",
                Some("workspace-bootstrap-active".to_string()),
            )
            .expect("active workspace session should create");
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("session-bootstrap-other"),
                "Other Workspace Current Session",
                Some("workspace-bootstrap-other".to_string()),
            )
            .expect("other workspace session should create and become global current");

        let app = build_router(state);
        let bootstrap = get_json(app, "/bootstrap").await;

        assert_eq!(bootstrap["currentSession"], Value::Null);
        let sessions = bootstrap["sessions"]
            .as_array()
            .expect("sessions should serialize as array");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["sessionId"], "session-bootstrap-active");
    }

    #[tokio::test]
    async fn bootstrap_session_query_infers_session_workspace_when_workspace_missing() {
        let state = test_state();
        let active_root = unique_temp_dir("workspace-bootstrap-active");
        let other_root = unique_temp_dir("workspace-bootstrap-other");
        state
            .workspace_registry
            .register(
                WorkspaceId::new("workspace-bootstrap-active"),
                AbsolutePath::new(active_root.display().to_string()),
            )
            .expect("active workspace should register first");
        state
            .workspace_registry
            .register(
                WorkspaceId::new("workspace-bootstrap-other"),
                AbsolutePath::new(other_root.display().to_string()),
            )
            .expect("other workspace should register");
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("session-bootstrap-active"),
                "Active Workspace Session",
                Some("workspace-bootstrap-active".to_string()),
            )
            .expect("active workspace session should create");
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("session-bootstrap-other"),
                "Other Workspace Session",
                Some("workspace-bootstrap-other".to_string()),
            )
            .expect("other workspace session should create");
        start_snapshot_for_session(&state, "session-bootstrap-other", &other_root).await;

        let app = build_router(state);
        let bootstrap = get_json(app, "/bootstrap?sessionId=session-bootstrap-other").await;

        assert_eq!(
            bootstrap["currentSession"]["sessionId"],
            "session-bootstrap-other"
        );
        let sessions = bootstrap["sessions"]
            .as_array()
            .expect("sessions should serialize as array");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["sessionId"], "session-bootstrap-other");
    }

    #[tokio::test]
    async fn bootstrap_accepts_camel_case_workspace_query() {
        let state = test_state();
        let workspace_id = WorkspaceId::new("workspace-bootstrap-query");
        let workspace_root = unique_temp_dir("magi-workspace-bootstrap-query");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_root.display().to_string()),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-bootstrap-query");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "Bootstrap Query Session",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        state.session_store.bind_execution_ownership(
            session_id.clone(),
            ExecutionOwnership {
                session_id: Some(session_id.clone()),
                workspace_id: Some(workspace_id),
                ..ExecutionOwnership::default()
            },
        );
        start_snapshot_for_session(&state, session_id.as_str(), &workspace_root).await;
        let app = build_router(state);

        let bootstrap = get_json(
            app,
            "/bootstrap?workspaceId=workspace-bootstrap-query&sessionId=session-bootstrap-query",
        )
        .await;

        assert_eq!(
            bootstrap["currentSession"]["sessionId"],
            "session-bootstrap-query"
        );
        let sessions = bootstrap["sessions"]
            .as_array()
            .expect("sessions should be an array");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["sessionId"], "session-bootstrap-query");
    }

    #[tokio::test]
    async fn bootstrap_workspace_session_filters_canonical_turns_to_selected_session() {
        let state = test_state();
        let workspace_id = WorkspaceId::new("workspace-bootstrap-canonical");
        let workspace_root = unique_temp_dir("magi-workspace-bootstrap-canonical");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_root.display().to_string()),
            )
            .expect("workspace should register");

        for session_id in [
            "session-bootstrap-canonical-a",
            "session-bootstrap-canonical-b",
        ] {
            let session_id = SessionId::new(session_id);
            state
                .session_store
                .create_session_for_workspace(
                    session_id.clone(),
                    format!("Bootstrap canonical {session_id}"),
                    Some(workspace_id.to_string()),
                )
                .expect("session should create");
            let accepted_at = UtcMillis(if session_id.as_str().ends_with("-a") {
                10
            } else {
                20
            });
            state
                .session_store
                .upsert_current_turn(
                    session_id.clone(),
                    ActiveExecutionTurn {
                        turn_id: format!("turn-{session_id}"),
                        turn_seq: accepted_at.0,
                        accepted_at,
                        completed_at: Some(UtcMillis(accepted_at.0 + 1)),
                        status: "completed".to_string(),
                        user_message: Some(format!("message {session_id}")),
                        items: Vec::new(),
                        worker_lanes: Vec::new(),
                    },
                )
                .expect("canonical turn should upsert");
        }

        start_snapshot_for_session(&state, "session-bootstrap-canonical-b", &workspace_root).await;
        let app = build_router(state);
        let selected = get_json(
            app.clone(),
            "/bootstrap?workspaceId=workspace-bootstrap-canonical&sessionId=session-bootstrap-canonical-b",
        )
        .await;
        let selected_turns = selected["canonicalTurns"]
            .as_array()
            .expect("canonical turns should serialize as array");
        assert_eq!(selected_turns.len(), 1);
        assert_eq!(
            selected_turns[0]["sessionId"],
            "session-bootstrap-canonical-b"
        );

        let workspace_only =
            get_json(app, "/bootstrap?workspaceId=workspace-bootstrap-canonical").await;
        assert!(
            workspace_only["canonicalTurns"]
                .as_array()
                .is_none_or(Vec::is_empty),
            "未选择 session 时不能把 workspace 内其他会话 canonical turns 混入当前主线"
        );
    }

    #[tokio::test]
    async fn bootstrap_resolves_workspace_path_when_id_missing() {
        let state = test_state();
        state
            .workspace_registry
            .register(
                WorkspaceId::new("workspace-bootstrap-active"),
                AbsolutePath::new("/tmp/magi-workspace-bootstrap-active"),
            )
            .expect("active workspace should register");
        let workspace_id = WorkspaceId::new("workspace-bootstrap-path");
        let workspace_path = "/tmp/magi-workspace-bootstrap-path";
        state
            .workspace_registry
            .register(workspace_id.clone(), AbsolutePath::new(workspace_path))
            .expect("path workspace should register");
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("session-bootstrap-path"),
                "Bootstrap Path Session",
                Some(workspace_id.to_string()),
            )
            .expect("path session should create");
        let app = build_router(state);

        let bootstrap = get_json(
            app,
            "/bootstrap?workspacePath=/tmp/magi-workspace-bootstrap-path",
        )
        .await;

        assert_eq!(bootstrap["currentSession"], Value::Null);
        let sessions = bootstrap["sessions"]
            .as_array()
            .expect("sessions should be an array");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["sessionId"], "session-bootstrap-path");
    }

    #[tokio::test]
    async fn session_history_routes_scope_sessions_and_reject_cross_workspace_binding() {
        let state = test_state();
        for workspace_id in ["workspace-history-a", "workspace-history-b"] {
            state
                .workspace_registry
                .register(
                    WorkspaceId::new(workspace_id),
                    AbsolutePath::new(format!("/tmp/{workspace_id}")),
                )
                .expect("workspace should register");
        }
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("session-history-a"),
                "History A",
                Some("workspace-history-a".to_string()),
            )
            .expect("session a should create");
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("session-history-b"),
                "History B",
                Some("workspace-history-b".to_string()),
            )
            .expect("session b should create");
        let app = build_router(state);

        let messages = get_json(
            app.clone(),
            "/api/messages?workspaceId=workspace-history-a&sessionId=session-history-a",
        )
        .await;
        assert_eq!(messages["currentSession"]["sessionId"], "session-history-a");
        let sessions = messages["sessions"]
            .as_array()
            .expect("messages sessions should be an array");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["sessionId"], "session-history-a");

        let current_session_scope_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/messages?workspaceId=workspace-history-a")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(
            current_session_scope_response.status(),
            StatusCode::BAD_REQUEST
        );

        let bootstrap = get_json(
            app.clone(),
            "/bootstrap?workspaceId=workspace-history-a&sessionId=session-history-b",
        )
        .await;
        assert!(bootstrap["currentSession"].is_null());

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(
                        "/api/messages?workspaceId=workspace-history-a&sessionId=session-history-b",
                    )
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn session_switch_rejects_cross_workspace_session_binding() {
        let state = test_state();
        for workspace_id in ["workspace-switch-a", "workspace-switch-b"] {
            state
                .workspace_registry
                .register(
                    WorkspaceId::new(workspace_id),
                    AbsolutePath::new(format!("/tmp/{workspace_id}")),
                )
                .expect("workspace should register");
        }
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("session-switch-a"),
                "Switch A",
                Some("workspace-switch-a".to_string()),
            )
            .expect("session a should create");
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("session-switch-b"),
                "Switch B",
                Some("workspace-switch-b".to_string()),
            )
            .expect("session b should create");
        let app = build_router(state);

        let (status, _) = post_json(
            app,
            "/api/session/switch",
            json!({
                "workspaceId": "workspace-switch-a",
                "sessionId": "session-switch-b",
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn session_chat_does_not_create_task_graph() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());

        let (status, body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "session-route-loopback",
                "text": "你好，这只是普通对话",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "普通对话应提交成功: {body:?}");
        assert_eq!(body["sessionId"], "session-route-loopback");
        assert!(
            body.get("rootTaskId").is_none(),
            "普通对话响应不应暴露任务根 ID: {body:?}"
        );

        wait_for_condition(
            Duration::from_secs(5),
            Duration::from_millis(20),
            || {
                state
                    .runtime_read_model_dto()
                    .details
                    .sessions
                    .iter()
                    .find(|session| session.session_id == "session-route-loopback")
                    .and_then(|session| session.current_turn.as_ref())
                    .is_some_and(|turn| turn.status == "completed")
            },
            "普通对话应在后台流式执行完成后标记 turn completed",
        )
        .await;
        let runtime_read_model = get_json(app.clone(), "/runtime/read-model").await;
        assert!(
            runtime_read_model["details"]["tasks"]
                .as_array()
                .expect("tasks should serialize as array")
                .is_empty(),
            "普通对话不能创建 TaskStore 任务"
        );
        let session_summary = runtime_read_model["details"]["sessions"]
            .as_array()
            .expect("sessions should serialize as array")
            .iter()
            .find(|session| session["session_id"] == "session-route-loopback")
            .expect("chat session should exist in read model");
        assert_eq!(session_summary["current_turn"]["status"], "completed");
        assert_eq!(session_summary["current_turn"]["mission_id"], Value::Null);
        assert_eq!(session_summary["current_turn"]["root_task_id"], Value::Null);

        let messages_page = get_json(app, "/api/messages?sessionId=session-route-loopback").await;
        let timeline_items = messages_page["timeline"]
            .as_array()
            .expect("timeline should serialize as array");
        assert_eq!(
            timeline_items
                .iter()
                .filter(|entry| entry["kind"] == "UserMessage")
                .count(),
            1
        );
        assert_eq!(
            timeline_items
                .iter()
                .filter(|entry| entry["kind"] == "AssistantMessage")
                .count(),
            0,
            "新会话普通回复不能再写 completed snapshot 到 legacy timeline"
        );
        let canonical_turns = messages_page["canonicalTurns"]
            .as_array()
            .expect("messages response should include canonical turns");
        assert_eq!(canonical_turns.len(), 1);
        assert!(
            canonical_turns[0]["items"]
                .as_array()
                .expect("canonical turn items should serialize as array")
                .iter()
                .any(|item| { item["kind"] == "assistant_text" && item["status"] == "completed" }),
            "普通回复正文必须从 canonical assistant_text 恢复"
        );
    }

    #[tokio::test]
    async fn session_turn_downgrades_low_evidence_task_route_to_chat() {
        let state = build_execution_state(
            WorkerRuntime::new_compare,
            Arc::new(TaskRouteClassifierModelBridgeClient),
        );
        let app = build_router(state.clone());

        let (status, body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "session-route-loopback",
                "text": "你好，这只是普通对话",
                "skillName": null,
                "images": [],
            }),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::OK,
            "低证据 task 误判应降级为普通对话: {body:?}"
        );
        assert_eq!(body["route"], "chat");
        assert!(body["rootTaskId"].is_null());
        assert!(body["actionTaskId"].is_null());

        wait_for_condition(
            Duration::from_secs(5),
            Duration::from_millis(20),
            || {
                state
                    .runtime_read_model_dto()
                    .details
                    .sessions
                    .iter()
                    .find(|session| session.session_id == "session-route-loopback")
                    .and_then(|session| session.current_turn.as_ref())
                    .is_some_and(|turn| turn.status == "completed")
            },
            "误判任务降级后仍应按普通对话完成",
        )
        .await;

        let runtime_read_model = get_json(app, "/runtime/read-model").await;
        assert!(
            runtime_read_model["details"]["tasks"]
                .as_array()
                .expect("tasks should serialize as array")
                .is_empty(),
            "低证据 task 误判不能写入 TaskStore"
        );
        let session_summary = runtime_read_model["details"]["sessions"]
            .as_array()
            .expect("sessions should serialize as array")
            .iter()
            .find(|session| session["session_id"] == "session-route-loopback")
            .expect("chat session should exist in read model");
        assert_eq!(session_summary["current_turn"]["root_task_id"], Value::Null);
    }

    #[tokio::test]
    async fn session_turn_uses_high_evidence_model_task_route_without_frontend_task_signal() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());

        let (status, body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "session-route-loopback",
                "text": "请分析并拆分这个复杂任务",
                "skillName": null,
                "images": [],
            }),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::OK,
            "高证据 task route 应创建任务图: {body:?}"
        );
        assert_eq!(body["route"], "task");
        let root_task_id = body["rootTaskId"]
            .as_str()
            .expect("模型判定 task 应返回 root task id");
        let projection = get_json(
            app,
            &format!("/api/tasks/graph/{root_task_id}?sessionId=session-route-loopback"),
        )
        .await;
        assert_eq!(projection["root_task"]["task_id"], root_task_id);
    }

    #[tokio::test]
    async fn session_turn_classifier_requires_tool_call_payload() {
        let state = build_execution_state(
            WorkerRuntime::new_compare,
            Arc::new(PlainJsonClassifierModelBridgeClient),
        );
        let app = build_router(state);

        let (status, body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": "session-route-loopback",
                "text": "你好，这只是普通对话",
                "skillName": null,
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body["message"],
            "Session Turn 分类器未调用 classify_session_turn 工具"
        );
    }

    #[tokio::test]
    async fn session_turn_classifier_uses_planning_client_when_business_model_unhealthy() {
        let app = build_router(test_state_with_unhealthy_execution_pipeline());

        let (status, body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": "session-route-loopback",
                "text": "你好，这只是普通对话",
                "skillName": null,
                "images": [],
            }),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::OK,
            "分类器不应被业务模型失败阻断: {body:?}"
        );
        assert_eq!(body["route"], "chat");
    }

    #[tokio::test]
    async fn session_execute_route_runs_tool_without_task_graph() {
        let state = build_execution_state(
            WorkerRuntime::new_compare,
            Arc::new(ExecuteToolModelBridgeClient {
                invoke_count: AtomicUsize::new(0),
            }),
        );
        let app = build_router(state.clone());

        let (status, body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "session-route-loopback",
                "text": "请搜索 Route Loopback Session 并说明结果",
                "skillName": null,
                "images": [],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "execute turn 应提交成功: {body:?}");
        assert_eq!(body["route"], "execute");
        assert!(
            body.get("rootTaskId").is_none(),
            "execute 只执行工具，不应创建任务图: {body:?}"
        );

        wait_for_condition(
            Duration::from_secs(5),
            Duration::from_millis(20),
            || {
                state
                    .runtime_read_model_dto()
                    .details
                    .sessions
                    .iter()
                    .find(|session| session.session_id == "session-route-loopback")
                    .is_some_and(|session| {
                        session
                            .current_turn
                            .as_ref()
                            .is_some_and(|turn| turn.status == "completed")
                            && session
                                .turn_items
                                .iter()
                                .filter(|item| {
                                    item.tool_call_id.as_deref() == Some("call-search-text")
                                })
                                .count()
                                == 1
                            && session.turn_items.iter().any(|item| {
                                item.kind == "tool_call_result"
                                    && item.tool_call_id.as_deref() == Some("call-search-text")
                                    && item.tool_result.is_some()
                            })
                            && session
                                .turn_items
                                .iter()
                                .any(|item| item.kind == "assistant_final")
                    })
            },
            "execute turn 应执行工具、写入工具项并生成最终回复",
        )
        .await;

        let runtime_read_model = get_json(app, "/runtime/read-model").await;
        assert!(
            runtime_read_model["details"]["tasks"]
                .as_array()
                .expect("tasks should serialize as array")
                .is_empty(),
            "execute route 不能创建 TaskStore 任务"
        );
    }

    #[tokio::test]
    async fn session_turn_sanitizes_assignment_dispatch_from_stream_and_final() {
        let state = build_execution_state(
            WorkerRuntime::new_compare,
            Arc::new(AssignmentDispatchStreamingModelBridgeClient),
        );
        let app = build_router(state.clone());
        let session_id = "session-route-loopback";

        let (status, body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": session_id,
                "text": "请先分析，再给我一个普通回复",
                "skillName": null,
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "session turn 应提交成功: {body:?}");
        assert_eq!(body["route"], "chat");

        wait_for_condition(
            Duration::from_secs(5),
            Duration::from_millis(20),
            || {
                state
                    .runtime_read_model_dto()
                    .details
                    .sessions
                    .iter()
                    .find(|session| session.session_id == session_id)
                    .and_then(|session| session.current_turn.as_ref())
                    .is_some_and(|turn| turn.status == "completed")
            },
            "含 assignment dispatch 的普通回复应完成并写入净化后的 turn item",
        )
        .await;

        let runtime_read_model = get_json(app.clone(), "/runtime/read-model").await;
        let session_summary = runtime_read_model["details"]["sessions"]
            .as_array()
            .expect("sessions should serialize as array")
            .iter()
            .find(|session| session["session_id"] == session_id)
            .expect("session summary should exist");
        let turn_items = session_summary["turn_items"]
            .as_array()
            .expect("turn items should serialize as array");
        let assistant_final = turn_items
            .iter()
            .find(|item| item["kind"] == "assistant_final")
            .expect("assistant final should exist");
        assert_eq!(assistant_final["content"], "分析完成。");
        for item in turn_items {
            if let Some(content) = item["content"].as_str() {
                assert_no_assignment_dispatch_leak(content);
            }
        }

        let messages_page = get_json(app, "/api/messages?sessionId=session-route-loopback").await;
        let assistant_entries = messages_page["timeline"]
            .as_array()
            .expect("timeline should serialize as array")
            .iter()
            .filter(|entry| entry["kind"] == "AssistantMessage")
            .collect::<Vec<_>>();
        assert_eq!(
            assistant_entries.len(),
            0,
            "assistant 正文应只写 canonical assistant_text，不再写 legacy timeline"
        );
        let canonical_turns = messages_page["canonicalTurns"]
            .as_array()
            .expect("messages response should include canonical turns");
        let canonical_assistant = canonical_turns
            .iter()
            .flat_map(|turn| turn["items"].as_array().into_iter().flatten())
            .find(|item| item["kind"] == "assistant_text" && item["status"] == "completed")
            .expect("canonical assistant_text should exist");
        assert_eq!(canonical_assistant["content"], "分析完成。");
        assert_no_assignment_dispatch_leak(
            canonical_assistant["content"]
                .as_str()
                .expect("canonical assistant content should be string"),
        );
    }

    #[tokio::test]
    async fn session_task_after_chat_replaces_current_turn_owner() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());
        let session_id = "session-chat-then-task";
        let task_text = "请分析并拆分这个复杂任务";
        state
            .session_store
            .create_session(SessionId::new(session_id), "chat then task session")
            .expect("session should create");

        let (chat_status, chat_body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": session_id,
                "text": "你好，这只是普通对话",
                "skillName": null,
                "images": [],
            }),
        )
        .await;
        assert_eq!(
            chat_status,
            StatusCode::OK,
            "普通对话应提交成功: {chat_body:?}"
        );
        assert_eq!(chat_body["route"], "chat");
        wait_for_condition(
            Duration::from_secs(5),
            Duration::from_millis(20),
            || {
                state
                    .runtime_read_model_dto()
                    .details
                    .sessions
                    .iter()
                    .find(|session| session.session_id == session_id)
                    .and_then(|session| session.current_turn.as_ref())
                    .is_some_and(|turn| turn.status == "completed")
            },
            "普通聊天 turn 应先稳定完成，再提交后续 task turn",
        )
        .await;

        let (task_status, task_body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": session_id,
                "text": task_text,
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;
        assert_eq!(
            task_status,
            StatusCode::OK,
            "任务 turn 应提交成功: {task_body:?}"
        );
        assert_eq!(task_body["route"], "task");
        let root_task_id = task_body["rootTaskId"]
            .as_str()
            .expect("task route should expose root task id");

        let read_model = state.runtime_read_model_dto();
        let session_entry = read_model
            .details
            .sessions
            .iter()
            .find(|entry| entry.session_id == session_id)
            .expect("session runtime summary should exist");
        let current_turn = session_entry
            .current_turn
            .as_ref()
            .expect("current turn should exist after task route");

        assert_eq!(current_turn.user_message.as_deref(), Some(task_text));
        assert_eq!(current_turn.root_task_id.as_deref(), Some(root_task_id));
        assert!(
            session_entry
                .turn_items
                .first()
                .is_some_and(|item| item.kind == "user_message"
                    && item.content.as_deref() == Some(task_text)
                    && item.source == "user"),
            "任务 turn 的用户消息只作为请求锚点（kind=user_message + source=user），由前端按 kind 决定不入响应区"
        );
    }

    #[tokio::test]
    async fn session_turn_route_accepts_model_classified_plain_chat() {
        let app = build_router(test_state_with_execution_pipeline());

        let (status, body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": "session-route-loopback",
                "text": "你好，这只是普通对话",
                "skillName": null,
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["route"], "chat");
    }

    #[tokio::test]
    async fn session_turn_rejects_active_current_turn_before_writing_user_message() {
        let state = test_state_with_execution_pipeline();
        let session_id = SessionId::new("session-active-turn-conflict");
        state
            .session_store
            .create_session(session_id.clone(), "Active turn conflict")
            .expect("session should be creatable");
        state
            .session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-still-running".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis(1),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("上一轮仍在执行".to_string()),
                    items: Vec::new(),
                    worker_lanes: Vec::new(),
                },
            )
            .expect("current turn should be stored");
        let app = build_router(state.clone());

        let (status, body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": session_id.to_string(),
                "text": "这条消息不应覆盖正在运行的 turn",
                "skillName": null,
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("turn-still-running")
        );
        assert!(
            !state
                .session_store
                .timeline_for_session(&session_id)
                .iter()
                .any(|entry| matches!(entry.kind, TimelineEntryKind::UserMessage)
                    && entry.message == "这条消息不应覆盖正在运行的 turn"),
            "冲突请求不能先写入用户消息"
        );
        let current_turn = state
            .session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("current turn should remain");
        assert_eq!(current_turn.turn_id, "turn-still-running");
        assert_eq!(current_turn.status, "running");
    }

    #[tokio::test]
    async fn session_task_turn_rejects_active_current_turn_before_writing_user_message() {
        let state = test_state_with_execution_pipeline();
        let session_id = SessionId::new("session-active-task-conflict");
        state
            .session_store
            .create_session(session_id.clone(), "Active task conflict")
            .expect("session should be creatable");
        state
            .session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-task-still-running".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis(1),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("上一轮任务仍在执行".to_string()),
                    items: Vec::new(),
                    worker_lanes: Vec::new(),
                },
            )
            .expect("current turn should be stored");
        let app = build_router(state.clone());

        let (status, body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": session_id.to_string(),
                "text": "这条深度任务不应覆盖正在运行的 turn",
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("turn-task-still-running")
        );
        assert!(
            !state
                .session_store
                .timeline_for_session(&session_id)
                .iter()
                .any(|entry| matches!(entry.kind, TimelineEntryKind::UserMessage)
                    && entry.message == "这条深度任务不应覆盖正在运行的 turn"),
            "任务冲突请求不能先写入用户消息"
        );
        let current_turn = state
            .session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("current turn should remain");
        assert_eq!(current_turn.turn_id, "turn-task-still-running");
        assert_eq!(current_turn.status, "running");
    }

    #[tokio::test]
    async fn session_turn_continue_excludes_finished_branch_from_recoverable_prompt() {
        let state = build_execution_state(
            WorkerRuntime::new_compare,
            Arc::new(ContinueClassifierExpectingNoRecoverableChain),
        );
        let app = build_router(state.clone());
        let session_id = SessionId::new("session-route-loopback");
        let mission_id = MissionId::new("mission-finished-branch-prompt");
        let root_task_id = TaskId::new("task-root-finished-branch-prompt");
        let branch_task_id = TaskId::new("task-branch-finished-branch-prompt");
        let worker_id = WorkerId::new("worker-finished-branch-prompt");
        let accepted_at = UtcMillis::now();
        let task_store = state.task_store().expect("task store should exist");
        for (task_id, parent_task_id, kind) in [
            (root_task_id.clone(), None, TaskKind::Objective),
            (
                branch_task_id.clone(),
                Some(root_task_id.clone()),
                TaskKind::Action,
            ),
        ] {
            task_store.insert_task(Task {
                task_id,
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                parent_task_id,
                kind,
                title: "finish 阶段分支".to_string(),
                goal: "验证 finish 阶段不进入自然语言继续集合".to_string(),
                status: TaskStatus::Blocked,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: None,
                executor_binding: None,
                context_refs: Vec::new(),
                knowledge_refs: Vec::new(),
                workspace_scope: None,
                write_scope: None,
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                repair_count: 0,
                decision_payload: None,
                variant: magi_core::TaskVariant::default(),
                created_at: accepted_at,
                updated_at: accepted_at,
            });
        }
        state
            .session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-finished-branch-prompt".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![branch_task_id.clone()],
                    active_worker_bindings: vec![worker_id.clone()],
                    branches: vec![ActiveExecutionBranch {
                        task_id: branch_task_id.clone(),
                        worker_id,
                        stage: "finish".to_string(),
                        lease_id: None,
                        execution_intent_ref: None,
                        binding_lifecycle: Some("completed".to_string()),
                        checkpoint_stage: None,
                        next_step_index: None,
                        checkpoint_at: None,
                        resume_mode: None,
                        resume_token: None,
                        use_tools: false,
                        skill_name: None,
                        is_primary: true,
                    }],
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at,
                        entry_id: "timeline-finished-branch-prompt".to_string(),
                        trimmed_text: Some("继续".to_string()),
                        skill_name: None,
                    },
                    current_turn: None,
                },
            )
            .expect("active chain should upsert");

        let (status, body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": session_id.to_string(),
                "text": "继续刚刚的任务",
                "skillName": null,
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body:?}");
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("没有可继续的执行链")),
            "错误必须明确说明没有可继续链: {body:?}"
        );
        assert_eq!(
            task_store
                .get_task(&branch_task_id)
                .expect("branch task should still exist")
                .status,
            TaskStatus::Blocked,
            "分类阶段不能把 finish 分支当成 continue 调度来改写"
        );
    }

    #[tokio::test]
    async fn session_turn_natural_language_continue_resumes_recoverable_chain() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());
        let session_id = SessionId::new("session-route-loopback");
        let mission_id = MissionId::new("mission-natural-continue");
        let root_task_id = TaskId::new("task-root-natural-continue");
        let branch_task_id = TaskId::new("task-branch-natural-continue");
        let worker_id = WorkerId::new("worker-natural-continue");
        let accepted_at = UtcMillis::now();
        let task_store = state.task_store().expect("task store should exist");
        for (task_id, parent_task_id, kind) in [
            (root_task_id.clone(), None, TaskKind::Objective),
            (
                branch_task_id.clone(),
                Some(root_task_id.clone()),
                TaskKind::Action,
            ),
        ] {
            task_store.insert_task(Task {
                task_id,
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                parent_task_id,
                kind,
                title: "自然语言继续".to_string(),
                goal: "验证用户说继续时恢复可继续链".to_string(),
                status: TaskStatus::Blocked,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: None,
                executor_binding: None,
                context_refs: Vec::new(),
                knowledge_refs: Vec::new(),
                workspace_scope: None,
                write_scope: None,
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                repair_count: 0,
                decision_payload: None,
                variant: magi_core::TaskVariant::default(),
                created_at: accepted_at,
                updated_at: accepted_at,
            });
        }
        state
            .session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-natural-continue".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![branch_task_id.clone()],
                    active_worker_bindings: vec![worker_id.clone()],
                    branches: vec![ActiveExecutionBranch {
                        task_id: branch_task_id.clone(),
                        worker_id,
                        stage: "execute".to_string(),
                        lease_id: None,
                        execution_intent_ref: None,
                        binding_lifecycle: Some("requested".to_string()),
                        checkpoint_stage: Some("execute".to_string()),
                        next_step_index: Some(0),
                        checkpoint_at: Some(accepted_at),
                        resume_mode: Some("stage-restart".to_string()),
                        resume_token: None,
                        use_tools: false,
                        skill_name: None,
                        is_primary: true,
                    }],
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at,
                        entry_id: "timeline-natural-continue".to_string(),
                        trimmed_text: Some("原任务".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-natural-continue".to_string(),
                        turn_seq: accepted_at.0,
                        accepted_at,
                        completed_at: None,
                        status: "running".to_string(),
                        user_message: Some("原任务".to_string()),
                        items: Vec::new(),
                        worker_lanes: Vec::new(),
                    }),
                },
            )
            .expect("active chain should upsert");

        let task_count_before = task_store.get_tasks_by_mission(&mission_id).len();
        let (status, body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": session_id.to_string(),
                "text": "继续刚刚的任务",
                "skillName": null,
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected body: {body:?}");
        assert_eq!(body["route"], "continue");
        assert_eq!(body["rootTaskId"], root_task_id.to_string());
        assert_eq!(body["actionTaskId"], branch_task_id.to_string());
        assert_eq!(body["executionChainRef"], "chain-natural-continue");
        assert_eq!(
            task_store.get_tasks_by_mission(&mission_id).len(),
            task_count_before,
            "自然语言继续不能创建新的 task graph"
        );
    }

    #[tokio::test]
    async fn session_action_route_drives_dispatch_and_updates_runtime_read_model() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());

        let (status, first_body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "session-route-loopback",
                "text": "Route parser refresh",
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {first_body:?}"
        );
        assert_eq!(first_body["sessionId"], "session-route-loopback");
        assert!(first_body["actionTaskId"].is_string());
        let first_accepted_at = first_body["acceptedAt"]
            .as_u64()
            .expect("accepted_at should serialize as integer");
        let first_extraction_id = format!("extract-session-action-{first_accepted_at}");
        // session 一生一 mission：mission_id 由首次入口的 accepted_at 决定，
        // 该 session 之后所有 dispatch 复用同一 mission_id。
        let session_mission_id = format!("mission-session-action-{first_accepted_at}");
        let first_root_task_id = TaskId::new(
            first_body["rootTaskId"]
                .as_str()
                .expect("root_task_id should serialize as string"),
        );
        let first_action_task_id = TaskId::new(
            first_body["actionTaskId"]
                .as_str()
                .expect("action_task_id should serialize as string"),
        );
        let task_store = state.task_store().expect("task store should be configured");
        wait_for_condition(
            Duration::from_secs(5),
            Duration::from_millis(20),
            || {
                let root_completed = task_store
                    .get_task(&first_root_task_id)
                    .map(|task| task.status == TaskStatus::Completed)
                    .unwrap_or(false);
                let action_completed = task_store
                    .get_task(&first_action_task_id)
                    .map(|task| task.status == TaskStatus::Completed)
                    .unwrap_or(false);
                let read_model = state.runtime_read_model_dto();
                let first_mission_entry = read_model
                    .details
                    .execution_groups
                    .iter()
                    .find(|entry| entry.mission_id == session_mission_id);
                root_completed && action_completed && first_mission_entry.is_some()
            },
            "first session action background dispatch should complete and publish context usage",
        )
        .await;
        let first_read_model = state.runtime_read_model_dto();
        let _first_mission_entry = first_read_model
            .details
            .execution_groups
            .iter()
            .find(|entry| entry.mission_id == session_mission_id)
            .expect("first execution group entry should exist");
        let first_root_task = task_store
            .get_task(&first_root_task_id)
            .expect("root task should exist");
        assert_eq!(first_root_task.status, TaskStatus::Completed);
        let first_action_task = task_store
            .get_task(&first_action_task_id)
            .expect("action task should exist");
        assert_eq!(first_action_task.status, TaskStatus::Completed);
        let first_children = task_store.get_children(&first_root_task_id);
        assert!(
            !first_children.is_empty(),
            "Objective should have at least one Phase child"
        );
        assert!(first_children.iter().all(|child| matches!(child.status, TaskStatus::Completed)));

        let first_verification = state
            .execution_pipeline()
            .expect("execution pipeline should exist")
            .memory_store
            .verify_extraction_linkage(&first_extraction_id)
            .expect("first route extraction should be persisted");
        assert!(first_verification.is_consistent);

        let extraction_history = state
            .execution_pipeline()
            .expect("execution pipeline should exist")
            .memory_store
            .extraction_results_for_session(&SessionId::new("session-route-loopback"));
        assert_eq!(extraction_history.len(), 1);
        assert_eq!(extraction_history[0].extraction_id, first_extraction_id);

        let (status, second_body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": "session-route-loopback",
                "text": "Route parser refresh followup",
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {second_body:?}"
        );
        assert!(second_body["actionTaskId"].is_string());
        let second_accepted_at = second_body["acceptedAt"]
            .as_u64()
            .expect("accepted_at should serialize as integer");
        let second_root_task_id = TaskId::new(
            second_body["rootTaskId"]
                .as_str()
                .expect("root_task_id should serialize as string"),
        );
        let second_action_task_id = TaskId::new(
            second_body["actionTaskId"]
                .as_str()
                .expect("action_task_id should serialize as string"),
        );
        let second_extraction_id = format!("extract-session-action-{second_accepted_at}");
        wait_for_condition(
            Duration::from_secs(5),
            Duration::from_millis(20),
            || {
                let root_completed = task_store
                    .get_task(&second_root_task_id)
                    .map(|task| task.status == TaskStatus::Completed)
                    .unwrap_or(false);
                let action_completed = task_store
                    .get_task(&second_action_task_id)
                    .map(|task| task.status == TaskStatus::Completed)
                    .unwrap_or(false);
                let read_model = state.runtime_read_model_dto();
                let session_mission_entry = read_model
                    .details
                    .execution_groups
                    .iter()
                    .find(|entry| entry.mission_id == session_mission_id);
                let extraction_ready = state
                    .execution_pipeline()
                    .expect("execution pipeline should exist")
                    .memory_store
                    .verify_extraction_linkage(&second_extraction_id)
                    .map(|verification| verification.is_consistent)
                    .unwrap_or(false);
                let _ = first_accepted_at;
                root_completed
                    && action_completed
                    && extraction_ready
                    && session_mission_entry.is_some()
            },
            "second session action background dispatch should complete and reuse prior extraction context",
        )
        .await;
        let read_model = state.runtime_read_model_dto();
        // session 一生一 mission：两次派发聚合到同一 execution_group entry。
        assert_eq!(read_model.details.execution_groups.len(), 1);
        let _mission_entry = read_model
            .details
            .execution_groups
            .iter()
            .find(|entry| entry.mission_id == session_mission_id)
            .expect("session execution group entry should exist");
        let second_root_task = task_store
            .get_task(&second_root_task_id)
            .expect("second root task should exist");
        assert_eq!(second_root_task.status, TaskStatus::Completed);
        let second_children = task_store.get_children(&second_root_task_id);
        assert!(
            !second_children.is_empty(),
            "Objective should have at least one Phase child"
        );
        assert!(second_children.iter().all(|child| matches!(child.status, TaskStatus::Completed)));
        let verification = state
            .execution_pipeline()
            .expect("execution pipeline should exist")
            .memory_store
            .verify_extraction_linkage(&second_extraction_id)
            .expect("second route extraction should be persisted");
        assert!(verification.is_consistent);

        let ownership = state
            .session_store
            .execution_ownership(&SessionId::new("session-route-loopback"))
            .expect("session ownership should be bound");
        assert!(ownership.mission_id.is_some());
        assert!(ownership.task_id.is_some());
        assert!(ownership.worker_id.is_some());
    }

    #[tokio::test]
    async fn session_action_route_uses_requested_workspace_for_explicit_session() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());

        let workspace_id = WorkspaceId::new("workspace-route-alt");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/tmp/magi-route-alt"),
            )
            .expect("workspace should register");

        let session_id = SessionId::new("session-route-alt");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "alt route session",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");

        let (status, body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": session_id.to_string(),
                "workspaceId": workspace_id.to_string(),
                "text": "Use alternate workspace",
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected response body: {body:?}");
        let ownership = state
            .session_store
            .execution_ownership(&session_id)
            .expect("session ownership should exist");
        assert_eq!(ownership.workspace_id, Some(workspace_id.clone()));
        let session = state
            .session_store
            .session(&session_id)
            .expect("session should still exist");
        assert_eq!(session.workspace_id.as_deref(), Some(workspace_id.as_str()));
    }

    #[tokio::test]
    async fn session_action_route_exposes_current_turn_items_and_worker_lanes() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());
        let session_id = SessionId::new("session-turn-view");
        state
            .session_store
            .create_session(session_id.clone(), "turn view session")
            .expect("session should create");

        let (status, body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": session_id.to_string(),
                "text": "请分析并拆分这个复杂任务",
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected response body: {body:?}");

        let read_model = state.runtime_read_model_dto();
        let session_entry = read_model
            .details
            .sessions
            .iter()
            .find(|entry| entry.session_id == session_id.to_string())
            .expect("session runtime summary should exist");
        let current_turn = session_entry
            .current_turn
            .as_ref()
            .expect("current turn should exist after session action");
        assert!(current_turn.turn_id.starts_with("turn-session-action-"));
        assert_eq!(
            current_turn.execution_chain_ref.as_deref(),
            session_entry.execution_chain_ref.as_deref(),
        );
        assert!(
            !session_entry
                .turn_items
                .iter()
                .any(|item| item.kind == "assistant_phase"),
            "P7：后端不再生成 phase 文本，turn items 不得包含 assistant_phase"
        );
        let expected_lane_count = session_entry
            .active_branches
            .iter()
            .filter(|branch| !branch.is_primary)
            .count();
        assert_eq!(session_entry.worker_lanes.len(), expected_lane_count);
        if expected_lane_count > 0 {
            assert!(
                session_entry
                    .worker_lanes
                    .iter()
                    .all(|lane| !lane.status.is_empty()),
                "worker lane status should be populated from task store"
            );
            assert!(
                session_entry
                    .worker_lanes
                    .iter()
                    .all(|lane| {
                        let role_id = lane.role_id.as_str();
                        !role_id.is_empty()
                            && !role_id.starts_with("task-")
                            && !role_id.starts_with("worker-")
                            && !role_id.starts_with("lane-")
                    }),
                "worker lane tabs must expose product role ids, not internal lane/task ids"
            );
            assert!(
                session_entry
                    .turn_items
                    .iter()
                    .filter(|item| matches!(
                        state.session_store.resolve_thread_visibility(
                            &session_id,
                            &ThreadId::new(item.source_thread_id.clone()),
                        ),
                        Some(ThreadVisibility::WorkerDrawer { .. })
                    ))
                    .all(|item| item.role_id.as_deref().is_some_and(|role_id| {
                        !role_id.is_empty()
                            && !role_id.starts_with("task-")
                            && !role_id.starts_with("worker-")
                            && !role_id.starts_with("lane-")
                    })),
                "worker-visible turn items must be grouped by product role id"
            );
        }
    }

    #[tokio::test]
    async fn deep_session_action_finalizes_turn_when_background_root_completes() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());
        let session_id = SessionId::new("session-deep-root-finalizes");
        state
            .session_store
            .create_session(session_id.clone(), "deep root finalizes session")
            .expect("session should create");

        let (status, body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": session_id.to_string(),
                "text": "请完整执行一个深度任务并交付",
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected response body: {body:?}");
        let root_task_id = body["rootTaskId"]
            .as_str()
            .expect("task route should expose root task id")
            .to_string();

        wait_for_condition(
            Duration::from_secs(5),
            Duration::from_millis(50),
            || {
                let root_completed = state
                    .runner_manager()
                    .and_then(|manager| manager.status(&root_task_id))
                    .is_some_and(|status| status.status == "completed");
                let turn_completed = state
                    .session_store
                    .runtime_sidecar(&session_id)
                    .and_then(|sidecar| sidecar.current_turn)
                    .is_some_and(|turn| turn.status == "completed" && turn.completed_at.is_some());
                root_completed && turn_completed
            },
            "background deep runner should finalize the session turn after root completion",
        )
        .await;

        let canonical_turn = state
            .session_store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| turn.status == magi_session_store::CanonicalTurnStatus::Completed)
            .expect("completed deep turn should be stored as canonical turn");
        assert!(
            canonical_turn
                .items
                .iter()
                .any(|item| matches!(
                    state
                        .session_store
                        .resolve_thread_visibility(&session_id, &item.source_thread_id),
                    Some(ThreadVisibility::WorkerDrawer { .. })
                )),
            "deep task canonical turn must preserve worker-drawer items for bootstrap"
        );
        let terminal_turn_event = state
            .event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .rev()
            .find(|event| {
                event.event_type == "session.turn.item"
                    && event.payload["session_id"].as_str() == Some(session_id.as_str())
                    && event.payload["current_turn"]["status"].as_str() == Some("completed")
            })
            .expect("root completion should republish canonical completed turn item");
        assert!(
            terminal_turn_event.payload["current_turn"]["response_duration_ms"].is_number(),
            "live terminal turn event must carry backend duration"
        );
        assert!(
            terminal_turn_event.payload["worker_lanes"]
                .as_array()
                .expect("terminal turn event should include worker lanes")
                .iter()
                .all(|lane| lane["status"].as_str() == Some("completed")),
            "live terminal turn event must refresh worker lane status"
        );
    }

    #[tokio::test]
    async fn session_turn_tool_round_missing_final_reply_fails_with_visible_turn_item() {
        let state = build_execution_state(
            WorkerRuntime::new_compare,
            Arc::new(ExecuteToolMissingFinalModelBridgeClient {
                invoke_count: AtomicUsize::new(0),
            }),
        );
        let app = build_router(state.clone());
        let session_id = SessionId::new("session-turn-tool-missing-final");
        state
            .session_store
            .create_session(session_id.clone(), "tool missing final session")
            .expect("session should create");

        let (status, body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": session_id.to_string(),
                "text": "请搜索当前仓库里的路由实现",
                "skillName": "",
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected response body: {body:?}");

        wait_for_condition(
            Duration::from_secs(5),
            Duration::from_millis(20),
            || {
                let Some(turn) = state
                    .session_store
                    .runtime_sidecar(&session_id)
                    .and_then(|sidecar| sidecar.current_turn)
                else {
                    return false;
                };
                turn.status == "failed"
                    && turn
                        .items
                        .iter()
                        .filter(|item| item.tool_call_id.as_deref() == Some("call-search-text"))
                        .count()
                        == 1
                    && turn.items.iter().any(|item| {
                        item.kind == "tool_call_result"
                            && item.tool_call_id.as_deref() == Some("call-search-text")
                            && item.tool_result.is_some()
                    })
                    && turn.items.iter().any(|item| item.kind == "assistant_error")
                    && !turn.items.iter().any(|item| item.kind == "assistant_final")
            },
            "tool round without final reply should fail through canonical turn items",
        )
        .await;

        let turn = state
            .session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("failed current turn should remain inspectable");
        let error_item = turn
            .items
            .iter()
            .find(|item| item.kind == "assistant_error")
            .expect("assistant_error turn item should be appended");
        assert!(
            error_item
                .content
                .as_deref()
                .is_some_and(|text| text.contains("模型在工具调用后未返回最终回复")),
            "assistant_error must expose the real missing-final-reply reason: {error_item:?}"
        );

        let canonical_turn = state
            .session_store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| turn.status == magi_session_store::CanonicalTurnStatus::Failed)
            .expect("failed canonical turn should be stored");
        assert!(
            canonical_turn.items.iter().any(|item| {
                item.kind == CanonicalTurnItemKind::ToolCall
                    && item.tool.as_ref().is_some_and(|tool| {
                        tool.call_id == "call-search-text" && tool.result.is_some()
                    })
            }),
            "工具结果必须复用同一张 canonical tool item，而不是 started/result 双事实"
        );
        assert!(
            canonical_turn.items.iter().any(|item| {
                item.kind == CanonicalTurnItemKind::AssistantText
                    && item.status == CanonicalTurnItemStatus::Failed
                    && item
                        .content
                        .as_deref()
                        .is_some_and(|text| text.contains("模型在工具调用后未返回最终回复"))
            }),
            "assistant_error 必须作为 canonical assistant_text 失败项恢复为可见失败文本"
        );

        let events = state.event_bus.snapshot().recent_events;
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "session.turn.failed"),
            "terminal route event should be a failed turn, not an internal assembly completion"
        );
    }

    #[tokio::test]
    async fn append_dispatch_assistant_message_uses_current_turn_assistant_final_as_authoritative_source()
     {
        let state = test_state_with_execution_pipeline();
        let session_id = SessionId::new("session-turn-output-refs");
        state
            .session_store
            .create_session(session_id.clone(), "turn output refs session")
            .expect("session should create");

        let mission_id = MissionId::new("mission-turn-output-refs");
        let action_task_id = TaskId::new("task-turn-output-refs");
        let accepted_at = UtcMillis::now();

        let (_mission_id, orchestrator_thread_id) = state
            .session_store
            .ensure_session_mission(&session_id, accepted_at, || mission_id.clone());
        let worker_thread_id = ThreadId::new("thread-worker-turn-output-refs-reviewer");
        state
            .session_store
            .register_thread(ExecutionThread {
                thread_id: worker_thread_id.clone(),
                session_id: session_id.clone(),
                mission_id: mission_id.clone(),
                role_id: "reviewer".to_string(),
                worker_instance_id: WorkerId::new("task-worker-reviewer"),
                status: ExecutionThreadStatus::Active,
                created_at: accepted_at,
                last_used_at: accepted_at,
                handled_task_ids: vec![action_task_id.clone()],
                message_history: Vec::new(),
            });

        state
            .session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: action_task_id.clone(),
                    execution_chain_ref: "chain-turn-output-refs".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![action_task_id.clone()],
                    active_worker_bindings: vec![],
                    branches: vec![],
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at,
                        entry_id: format!("timeline-{session_id}-{}", accepted_at.0),
                        trimmed_text: Some("请输出完整总结".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-output-refs".to_string(),
                        turn_seq: 1,
                        accepted_at,
                        completed_at: None,
                        status: "completed".to_string(),
                        user_message: Some("请输出完整总结".to_string()),
                        items: vec![
                            ActiveExecutionTurnItem {
                                item_id: "turn-item-assistant-stream".to_string(),
                                item_seq: 1,
                                lane_id: None,
                                lane_seq: None,
                                kind: "assistant_stream".to_string(),
                                status: "completed".to_string(),
                                source: "orchestrator".to_string(),
                                title: Some("生成回复".to_string()),
                                content: Some("这是一段流式中的中间内容".to_string()),
                                task_id: Some(action_task_id.clone()),
                                worker_id: None,
                                role_id: None,
                                tool_call_id: None,
                                tool_name: None,
                                tool_status: None,
                                tool_arguments: None,
                                tool_result: None,
                                tool_error: None,
                                request_id: None,
                                user_message_id: None,
                                placeholder_message_id: None,
                                timeline_entry_id: None,
                                source_thread_id: worker_thread_id.clone(),
                            },
                            ActiveExecutionTurnItem {
                                item_id: "turn-item-assistant-final".to_string(),
                                item_seq: 2,
                                lane_id: None,
                                lane_seq: None,
                                kind: "assistant_final".to_string(),
                                status: "completed".to_string(),
                                source: "orchestrator".to_string(),
                                title: Some("最终总结".to_string()),
                                content: Some("这是来自 assistant_final 的最终总结".to_string()),
                                task_id: Some(action_task_id.clone()),
                                worker_id: None,
                                role_id: None,
                                tool_call_id: None,
                                tool_name: None,
                                tool_status: None,
                                tool_arguments: None,
                                tool_result: None,
                                tool_error: None,
                                request_id: None,
                                user_message_id: None,
                                placeholder_message_id: None,
                                timeline_entry_id: None,
                                source_thread_id: orchestrator_thread_id.clone(),
                            },
                        ],
                        worker_lanes: vec![magi_session_store::ActiveExecutionTurnLane {
                            lane_id: "lane-turn-output-refs".to_string(),
                            lane_seq: 1,
                            task_id: action_task_id.clone(),
                            worker_id: WorkerId::new("task-worker-reviewer"),
                            role_id: "reviewer".to_string(),
                            thread_id: worker_thread_id.clone(),
                            title: "评审最终总结".to_string(),
                            is_primary: false,
                        }],
                    }),
                },
            )
            .expect("active execution chain should upsert");

        state
            .task_store()
            .expect("task store should exist")
            .insert_task(Task {
                task_id: action_task_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: action_task_id.clone(),
                parent_task_id: None,
                kind: TaskKind::Action,
                title: "执行: turn output refs".to_string(),
                goal: "验证 assistant_final 作为唯一结果来源".to_string(),
                status: TaskStatus::Completed,
                dependency_ids: vec![],
                required_children: vec![],
                policy_snapshot: None,
                executor_binding: Some(ExecutorBinding {
                    target_role: "reviewer".to_string(),
                    capability_requirements: vec![],
                    parallelism_group: None,
                    exclusive_scope: None,
                    worker_selector: None,
                }),
                context_refs: vec![],
                knowledge_refs: vec![],
                workspace_scope: None,
                write_scope: None,
                input_refs: vec![],
                output_refs: vec!["这是来自 output_refs 的旧结果".to_string()],
                evidence_refs: vec![],
                retry_count: 0,
                repair_count: 0,
                decision_payload: None,
                variant: magi_core::TaskVariant::default(),
                created_at: accepted_at,
                updated_at: accepted_at,
            });

        append_dispatch_assistant_message(
            &state,
            &DispatchSubmissionAccepted {
                session_id: session_id.clone(),
                entry_id: format!("timeline-{session_id}-{}", accepted_at.0),
                accepted_at,
                created_session: false,
                root_task_id: action_task_id.clone(),
                action_task_id: action_task_id.clone(),
                runner_started: false,
            },
        );

        let assistant_messages = state
            .session_store
            .timeline()
            .into_iter()
            .filter(|entry| {
                entry.session_id == session_id
                    && matches!(
                        entry.kind,
                        magi_session_store::TimelineEntryKind::AssistantMessage
                    )
            })
            .collect::<Vec<_>>();
        assert!(
            assistant_messages.is_empty(),
            "完成态不应再写 completed snapshot assistant timeline facts"
        );

        let canonical_turn = state
            .session_store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| turn.turn_id == "turn-output-refs")
            .expect("dispatch completion should persist canonical turn");
        let final_item = canonical_turn
            .items
            .iter()
            .find(|item| item.item_id == "turn-item-assistant-final")
            .expect("canonical turn should keep assistant_final item");
        assert_eq!(
            final_item.content.as_deref(),
            Some("这是来自 assistant_final 的最终总结")
        );
        assert_eq!(
            final_item
                .worker
                .as_ref()
                .and_then(|worker| worker.task_id.as_ref()),
            Some(&action_task_id),
            "canonical worker ref 应保留 task 归属"
        );

        let terminal_turn_event = state
            .event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .rev()
            .find(|event| {
                event.event_type == "session.turn.item"
                    && event.payload["session_id"].as_str() == Some(session_id.as_str())
                    && event.payload["current_turn"]["status"].as_str() == Some("completed")
            })
            .expect("dispatch completion should publish terminal turn event");
        assert_eq!(
            terminal_turn_event.payload["turn_items"][0]["role_id"], "reviewer",
            "terminal event turn_items 应回填 task executor role"
        );
        assert_eq!(
            terminal_turn_event.payload["worker_lanes"][0]["role_id"], "reviewer",
            "terminal event worker_lanes 应回填 task executor role"
        );
    }

    #[tokio::test]
    async fn append_dispatch_assistant_message_waits_for_deep_root_completion() {
        let state = test_state_with_execution_pipeline();
        let session_id = SessionId::new("session-deep-root-still-running");
        state
            .session_store
            .create_session(session_id.clone(), "deep root still running session")
            .expect("session should create");

        let mission_id = MissionId::new("mission-deep-root-still-running");
        let root_task_id = TaskId::new("task-root-deep-still-running");
        let action_task_id = TaskId::new("task-action-deep-primary-done");
        let accepted_at = UtcMillis::now();

        let (_mission_id, orchestrator_thread_id) = state
            .session_store
            .ensure_session_mission(&session_id, accepted_at, || mission_id.clone());

        state
            .session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-deep-root-still-running".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![action_task_id.clone()],
                    active_worker_bindings: vec![],
                    branches: vec![],
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at,
                        entry_id: format!("timeline-{session_id}-{}", accepted_at.0),
                        trimmed_text: Some("执行深度任务".to_string()),
                        skill_name: Some("refactor".to_string()),
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-deep-root-still-running".to_string(),
                        turn_seq: accepted_at.0,
                        accepted_at,
                        completed_at: None,
                        status: "running".to_string(),
                        user_message: Some("执行深度任务".to_string()),
                        items: vec![ActiveExecutionTurnItem {
                            item_id: format!("timeline-streaming-{action_task_id}"),
                            item_seq: 1,
                            lane_id: None,
                            lane_seq: None,
                            kind: "assistant_final".to_string(),
                            status: "completed".to_string(),
                            source: "orchestrator".to_string(),
                            title: Some("阶段性回复".to_string()),
                            content: Some("primary action 已完成，但深度任务还没完成".to_string()),
                            task_id: Some(action_task_id.clone()),
                            worker_id: None,
                            role_id: None,
                            tool_call_id: None,
                            tool_name: None,
                            tool_status: None,
                            tool_arguments: None,
                            tool_result: None,
                            tool_error: None,
                            request_id: None,
                            user_message_id: None,
                            placeholder_message_id: None,
                            timeline_entry_id: None,
                            source_thread_id: orchestrator_thread_id.clone(),
                        }],
                        worker_lanes: vec![],
                    }),
                },
            )
            .expect("active execution chain should upsert");

        let task_store = state.task_store().expect("task store should exist");
        task_store.insert_task(Task {
            task_id: root_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: TaskKind::Objective,
            title: "深度任务根节点".to_string(),
            goal: "验证 root 未完成时不能封口 turn".to_string(),
            status: TaskStatus::Running,
            dependency_ids: vec![],
            required_children: vec![action_task_id.clone()],
            policy_snapshot: None,
            executor_binding: None,
            context_refs: vec![],
            knowledge_refs: vec![],
            workspace_scope: None,
            write_scope: None,
            input_refs: vec![],
            output_refs: vec![],
            evidence_refs: vec![],
            retry_count: 0,
            repair_count: 0,
            decision_payload: None,
            variant: magi_core::TaskVariant::default(),
            created_at: accepted_at,
            updated_at: accepted_at,
        });
        task_store.insert_task(Task {
            task_id: action_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: Some(root_task_id.clone()),
            kind: TaskKind::Action,
            title: "primary action".to_string(),
            goal: "阶段性执行".to_string(),
            status: TaskStatus::Completed,
            dependency_ids: vec![],
            required_children: vec![],
            policy_snapshot: None,
            executor_binding: None,
            context_refs: vec![],
            knowledge_refs: vec![],
            workspace_scope: None,
            write_scope: None,
            input_refs: vec![],
            output_refs: vec![],
            evidence_refs: vec![],
            retry_count: 0,
            repair_count: 0,
            decision_payload: None,
            variant: magi_core::TaskVariant::default(),
            created_at: accepted_at,
            updated_at: accepted_at,
        });

        append_dispatch_assistant_message(
            &state,
            &DispatchSubmissionAccepted {
                session_id: session_id.clone(),
                entry_id: format!("timeline-{session_id}-{}", accepted_at.0),
                accepted_at,
                created_session: false,
                root_task_id: root_task_id.clone(),
                action_task_id,
                runner_started: true,
            },
        );

        let current_turn = state
            .session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("current turn should remain");
        assert_eq!(
            current_turn.status, "running",
            "deep root 仍在运行时，primary action 的 assistant_final 不能封口整轮"
        );
        assert!(
            state
                .session_store
                .timeline()
                .into_iter()
                .filter(|entry| entry.session_id == session_id)
                .all(|entry| !matches!(entry.kind, TimelineEntryKind::AssistantMessage)),
            "deep root 未完成时不能写 completed turn snapshot"
        );
    }

    #[tokio::test]
    async fn append_dispatch_assistant_message_skips_output_refs_when_current_turn_has_no_assistant_final()
     {
        let state = test_state_with_execution_pipeline();
        let session_id = SessionId::new("session-turn-no-assistant-final");
        state
            .session_store
            .create_session(session_id.clone(), "turn no assistant final session")
            .expect("session should create");

        let mission_id = MissionId::new("mission-turn-no-assistant-final");
        let action_task_id = TaskId::new("task-turn-no-assistant-final");
        let accepted_at = UtcMillis::now();

        let (_mission_id, orchestrator_thread_id) = state
            .session_store
            .ensure_session_mission(&session_id, accepted_at, || mission_id.clone());

        state
            .session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: action_task_id.clone(),
                    execution_chain_ref: "chain-turn-no-assistant-final".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![action_task_id.clone()],
                    active_worker_bindings: vec![],
                    branches: vec![],
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at,
                        entry_id: format!("timeline-{session_id}-{}", accepted_at.0),
                        trimmed_text: Some("请不要回退到 output refs".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-no-assistant-final".to_string(),
                        turn_seq: 1,
                        accepted_at,
                        completed_at: None,
                        status: "completed".to_string(),
                        user_message: Some("请不要回退到 output refs".to_string()),
                        items: vec![ActiveExecutionTurnItem {
                            item_id: "turn-item-assistant-phase".to_string(),
                            item_seq: 1,
                            lane_id: None,
                            lane_seq: None,
                            kind: "assistant_phase".to_string(),
                            status: "completed".to_string(),
                            source: "orchestrator".to_string(),
                            title: Some("生成回复".to_string()),
                            content: Some("只存在阶段项".to_string()),
                            task_id: Some(action_task_id.clone()),
                            worker_id: None,
                            role_id: None,
                            tool_call_id: None,
                            tool_name: None,
                            tool_status: None,
                            tool_arguments: None,
                            tool_result: None,
                            tool_error: None,
                            request_id: None,
                            user_message_id: None,
                            placeholder_message_id: None,
                            timeline_entry_id: None,
                            source_thread_id: orchestrator_thread_id.clone(),
                        }],
                        worker_lanes: vec![],
                    }),
                },
            )
            .expect("active execution chain should upsert");

        state
            .task_store()
            .expect("task store should exist")
            .insert_task(Task {
                task_id: action_task_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: action_task_id.clone(),
                parent_task_id: None,
                kind: TaskKind::Action,
                title: "执行: turn no assistant final".to_string(),
                goal: "验证没有 assistant_final 时不回退".to_string(),
                status: TaskStatus::Completed,
                dependency_ids: vec![],
                required_children: vec![],
                policy_snapshot: None,
                executor_binding: None,
                context_refs: vec![],
                knowledge_refs: vec![],
                workspace_scope: None,
                write_scope: None,
                input_refs: vec![],
                output_refs: vec!["这是不应再被采用的 output refs".to_string()],
                evidence_refs: vec![],
                retry_count: 0,
                repair_count: 0,
                decision_payload: None,
                variant: magi_core::TaskVariant::default(),
                created_at: accepted_at,
                updated_at: accepted_at,
            });

        append_dispatch_assistant_message(
            &state,
            &DispatchSubmissionAccepted {
                session_id: session_id.clone(),
                entry_id: format!("timeline-{session_id}-{}", accepted_at.0),
                accepted_at,
                created_session: false,
                root_task_id: action_task_id.clone(),
                action_task_id,
                runner_started: false,
            },
        );

        let assistant_messages = state
            .session_store
            .timeline()
            .into_iter()
            .filter(|entry| {
                entry.session_id == session_id
                    && matches!(
                        entry.kind,
                        magi_session_store::TimelineEntryKind::AssistantMessage
                    )
            })
            .collect::<Vec<_>>();
        assert!(
            assistant_messages.is_empty(),
            "当前 turn 缺少 assistant_final 时，不应再回退到 output refs 追加主线 assistant"
        );
    }

    #[tokio::test]
    async fn append_dispatch_assistant_message_skips_stale_turn_when_current_turn_was_replaced() {
        let state = test_state_with_execution_pipeline();
        let session_id = SessionId::new("session-turn-replaced");
        state
            .session_store
            .create_session(session_id.clone(), "turn replaced session")
            .expect("session should create");

        let mission_id = MissionId::new("mission-turn-replaced");
        let first_action_task_id = TaskId::new("task-turn-replaced-first");
        let second_action_task_id = TaskId::new("task-turn-replaced-second");
        let first_accepted_at = UtcMillis::now();
        let second_accepted_at = UtcMillis(first_accepted_at.0 + 1);

        let (_mission_id, orchestrator_thread_id) = state
            .session_store
            .ensure_session_mission(&session_id, first_accepted_at, || mission_id.clone());

        state
            .session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: second_action_task_id.clone(),
                    execution_chain_ref: "chain-turn-replaced-second".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![second_action_task_id.clone()],
                    active_worker_bindings: vec![],
                    branches: vec![],
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: second_accepted_at,
                        entry_id: format!("timeline-{session_id}-{}", second_accepted_at.0),
                        trimmed_text: Some("第二轮已经替换 current_turn".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-replaced-second".to_string(),
                        turn_seq: 2,
                        accepted_at: second_accepted_at,
                        completed_at: None,
                        status: "running".to_string(),
                        user_message: Some("第二轮已经替换 current_turn".to_string()),
                        items: vec![ActiveExecutionTurnItem {
                            item_id: "turn-item-second-stream".to_string(),
                            item_seq: 1,
                            lane_id: None,
                            lane_seq: None,
                            kind: "assistant_stream".to_string(),
                            status: "running".to_string(),
                            source: "orchestrator".to_string(),
                            title: Some("第二轮".to_string()),
                            content: Some("第二轮内容".to_string()),
                            task_id: Some(second_action_task_id.clone()),
                            worker_id: None,
                            role_id: None,
                            tool_call_id: None,
                            tool_name: None,
                            tool_status: None,
                            tool_arguments: None,
                            tool_result: None,
                            tool_error: None,
                            request_id: None,
                            user_message_id: None,
                            placeholder_message_id: None,
                            timeline_entry_id: None,
                            source_thread_id: orchestrator_thread_id.clone(),
                        }],
                        worker_lanes: vec![],
                    }),
                },
            )
            .expect("active execution chain should upsert");

        state
            .task_store()
            .expect("task store should exist")
            .insert_task(Task {
                task_id: first_action_task_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: first_action_task_id.clone(),
                parent_task_id: None,
                kind: TaskKind::Action,
                title: "执行: first".to_string(),
                goal: "验证 current_turn 被替换后不写回旧轮结果".to_string(),
                status: TaskStatus::Completed,
                dependency_ids: vec![],
                required_children: vec![],
                policy_snapshot: None,
                executor_binding: None,
                context_refs: vec![],
                knowledge_refs: vec![],
                workspace_scope: None,
                write_scope: None,
                input_refs: vec![],
                output_refs: vec![
                    json!({
                        "blocks": [
                            { "type": "tool_call", "content": "工具块不应作为主线文本" },
                            { "type": "text", "content": "第一轮 action 自己的最终结果" }
                        ]
                    })
                    .to_string(),
                ],
                evidence_refs: vec![],
                retry_count: 0,
                repair_count: 0,
                decision_payload: None,
                variant: magi_core::TaskVariant::default(),
                created_at: first_accepted_at,
                updated_at: first_accepted_at,
            });

        append_dispatch_assistant_message(
            &state,
            &DispatchSubmissionAccepted {
                session_id: session_id.clone(),
                entry_id: format!("timeline-{session_id}-{}", first_accepted_at.0),
                accepted_at: first_accepted_at,
                created_session: false,
                root_task_id: first_action_task_id.clone(),
                action_task_id: first_action_task_id,
                runner_started: false,
            },
        );

        let assistant_messages = state
            .session_store
            .timeline()
            .into_iter()
            .filter(|entry| {
                entry.session_id == session_id
                    && matches!(
                        entry.kind,
                        magi_session_store::TimelineEntryKind::AssistantMessage
                    )
            })
            .collect::<Vec<_>>();
        assert!(assistant_messages.is_empty());
    }

    #[tokio::test]
    async fn session_new_route_handles_concurrent_creates_without_duplicate_ids() {
        let persistence_root = std::env::temp_dir().join(format!(
            "magi-route-session-new-concurrency-{}",
            UtcMillis::now().0
        ));
        std::fs::create_dir_all(&persistence_root).expect("persistence root should be creatable");
        let workspace_root = persistence_root.join("workspace-root");
        std::fs::create_dir_all(&workspace_root).expect("workspace root should be creatable");

        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let workspace_id = WorkspaceId::new("workspace-route-concurrent-session-new");
        workspace_store
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_root.to_string_lossy().as_ref()),
            )
            .expect("workspace should register");
        workspace_store
            .activate(&workspace_id)
            .expect("workspace should activate");

        let state = ApiState::new(
            "magi",
            event_bus,
            session_store,
            workspace_store,
            Arc::new(GovernanceService::default()),
        )
        .with_runtime_persistence(Arc::new(crate::state::RuntimeStatePersistence::new(
            persistence_root.join("sessions.json"),
            persistence_root.join("workspaces.json"),
            persistence_root.join("knowledge.json"),
        )));
        let app = build_router(state);

        let tasks = (0..8)
            .map(|_| {
                tokio::spawn(post_json(
                    app.clone(),
                    "/api/session/new",
                    json!({ "workspaceId": workspace_id.to_string() }),
                ))
            })
            .collect::<Vec<_>>();

        let mut seen = std::collections::HashSet::new();
        for task in tasks {
            let (status, body) = task.await.expect("request task should join");
            assert_eq!(status, StatusCode::OK, "unexpected response body: {body:?}");
            let session_id = body["sessionId"]
                .as_str()
                .expect("sessionId should be string")
                .to_string();
            let current_session_id = body["currentSession"]["sessionId"]
                .as_str()
                .expect("currentSession.sessionId should be string")
                .to_string();
            assert_eq!(session_id, current_session_id);
            assert!(seen.insert(session_id), "sessionId should be unique");
        }
    }

    #[tokio::test]
    async fn session_action_route_rejects_cross_workspace_session_submission() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());

        let alt_workspace_id = WorkspaceId::new("workspace-route-other");
        state
            .workspace_registry
            .register(
                alt_workspace_id.clone(),
                AbsolutePath::new("/tmp/magi-route-other"),
            )
            .expect("workspace should register");

        let (status, body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": "session-route-loopback",
                "workspaceId": alt_workspace_id.to_string(),
                "text": "cross workspace should fail",
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error_code"], "INPUT_INVALID");
        assert_eq!(
            body["message"],
            "会话 session-route-loopback 不属于 workspace workspace-route-other"
        );
    }

    #[tokio::test]
    async fn session_action_route_rejects_missing_requested_session() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state);

        let (status, body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": "session-route-missing",
                "workspaceId": "workspace-route-loopback",
                "text": "missing session should fail",
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error_code"], "SESSION_NOT_FOUND");
        assert_eq!(body["message"], "会话不存在: session-route-missing");
    }

    #[tokio::test]
    async fn session_action_tool_and_llm_events_remain_bound_to_owning_session_after_switch() {
        let switched_session_id = SessionId::new("session-route-loopback-other");
        let state = build_execution_state_with_factory(
            WorkerRuntime::new_compare,
            |session_store| {
                Arc::new(SessionSwitchingToolModelBridgeClient {
                    session_store,
                    switch_to: switched_session_id.clone(),
                    invoke_count: AtomicUsize::new(0),
                })
            },
        );
        state
            .session_store
            .create_session(switched_session_id.clone(), "Other Session")
            .expect("other session should be creatable");
        let app = build_router(state.clone());

        let (status, body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": "session-route-loopback",
                "text": "分析并拆分：读取一个配置文件并总结",
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected response body: {body:?}");

        wait_for_condition(
            Duration::from_secs(5),
            Duration::from_millis(20),
            || {
                let events = state.event_bus.snapshot().recent_events;
                events
                    .iter()
                    .any(|event| event.event_type == "task.dispatched")
                    && events
                        .iter()
                        .any(|event| event.event_type == "task.llm.started")
                    && events
                        .iter()
                        .any(|event| event.event_type == "task.tool.invoked")
                    && events
                        .iter()
                        .any(|event| event.event_type == "tool.invoked")
            },
            "session action dispatch/tool events should be emitted",
        )
        .await;

        let events = state.event_bus.snapshot().recent_events;
        let owning_session = Some(SessionId::new("session-route-loopback"));
        let switched_session = Some(switched_session_id);

        let dispatched_event = events
            .iter()
            .find(|event| event.event_type == "task.dispatched")
            .expect("task.dispatched event should exist");
        assert_eq!(dispatched_event.session_id, owning_session);

        let llm_started_event = events
            .iter()
            .find(|event| event.event_type == "task.llm.started")
            .expect("task.llm.started event should exist");
        assert_eq!(llm_started_event.session_id, owning_session);

        let tool_invoked_event = events
            .iter()
            .find(|event| event.event_type == "task.tool.invoked")
            .expect("task.tool.invoked event should exist");
        assert_eq!(tool_invoked_event.session_id, owning_session);

        let tool_audit_event = events
            .iter()
            .find(|event| event.event_type == "tool.invoked")
            .expect("tool.invoked event should exist");
        assert_eq!(tool_audit_event.session_id, owning_session);
        assert_ne!(tool_audit_event.session_id, switched_session);
    }

    #[tokio::test]
    async fn session_turn_task_builds_graph_when_business_model_is_unhealthy() {
        let state = test_state_with_unhealthy_execution_pipeline();
        let app = build_router(state.clone());

        let (status, body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": "session-route-loopback",
                "text": "Route parser refresh failure",
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::OK,
            "深度任务建图不应被业务模型失败阻断: {body:?}"
        );
        assert_eq!(body["route"], "task");
        let root_task_id = body["rootTaskId"]
            .as_str()
            .expect("深度任务应返回 rootTaskId");
        let action_task_id = body["actionTaskId"]
            .as_str()
            .expect("深度任务应返回 actionTaskId");
        let projection = state
            .task_store()
            .expect("task store should exist")
            .build_projection(&TaskId::new(root_task_id))
            .expect("深度任务应生成可投影任务图");
        assert_eq!(projection.execution_mode, "deep");
        assert!(
            projection.progress_summary.total_tasks > 1,
            "深度任务应生成多节点任务图: {projection:?}"
        );

        let ownership = state
            .session_store
            .execution_ownership(&SessionId::new("session-route-loopback"))
            .expect("深度任务应写入 session 执行 ownership");
        assert!(ownership.mission_id.is_some());
        assert_eq!(
            ownership
                .task_id
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some(action_task_id)
        );
        assert!(ownership.worker_id.is_some());
    }

    #[tokio::test]
    async fn task_graph_manual_replan_replaces_active_execution_branches() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());

        let (status, body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "session-route-loopback",
                "text": "Manual task graph replan coverage",
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "深度任务提交应成功: {body:?}");
        let root_task_id = body["rootTaskId"]
            .as_str()
            .expect("深度任务应返回 rootTaskId")
            .to_string();
        let old_action_task_id = body["actionTaskId"]
            .as_str()
            .expect("深度任务应返回 actionTaskId")
            .to_string();

        let (replan_status, replan_body) = post_json(
            app,
            &format!("/api/tasks/{root_task_id}/replan?sessionId=session-route-loopback"),
            json!({}),
        )
        .await;
        assert_eq!(
            replan_status,
            StatusCode::OK,
            "手动重规划应成功并同步 active chain: {replan_body:?}"
        );
        let primary_action_task_id = replan_body["primaryActionTaskId"]
            .as_str()
            .expect("手动重规划应返回主 action")
            .to_string();
        let leaf_action_task_ids = replan_body["leafActionTaskIds"]
            .as_array()
            .expect("手动重规划应返回 leaf action 列表")
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .expect("leaf action id must be string")
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert!(
            !leaf_action_task_ids.is_empty(),
            "重规划必须生成 worker 叶子任务"
        );

        let sidecar = state
            .session_store
            .runtime_sidecar(&SessionId::new("session-route-loopback"))
            .expect("重规划后应保留 session sidecar");
        let chain = sidecar
            .active_execution_chain
            .expect("重规划后应保留 active execution chain");
        let active_branch_ids = chain
            .active_branch_task_ids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert!(
            !active_branch_ids.contains(&old_action_task_id),
            "手动重规划不能让旧 action 分支继续留在 active chain: {active_branch_ids:?}"
        );
        assert!(
            active_branch_ids.contains(&primary_action_task_id),
            "手动重规划主 action 必须进入 active chain: {active_branch_ids:?}"
        );
        for leaf_action_task_id in &leaf_action_task_ids {
            assert!(
                active_branch_ids.contains(leaf_action_task_id),
                "手动重规划 leaf action 必须进入 active chain: {active_branch_ids:?}"
            );
        }
    }

    #[tokio::test]
    async fn session_action_route_skips_extraction_for_blank_text_inputs() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());
        let initial_execution_group_count = state
            .runtime_read_model_dto()
            .details
            .execution_groups
            .len();
        state
            .session_store
            .create_session(SessionId::new("session-route-blank"), "blank route session")
            .expect("blank route session should be creatable");

        let (status, body) = post_json(
            app,
            "/api/session/turn",
            json!({
                "sessionId": "session-route-blank",
                "text": "   ",
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "unexpected response body: {body:?}"
        );
        assert_eq!(body["error_code"], "INPUT_INVALID");
        assert_eq!(body["message"], "任务派发必须提供非空 execution_goal");

        let extraction_history = state
            .execution_pipeline()
            .expect("execution pipeline should exist")
            .memory_store
            .extraction_results_for_session(&SessionId::new("session-route-blank"));
        assert!(extraction_history.is_empty());

        let runtime_read_model = state.runtime_read_model_dto();
        assert_eq!(
            runtime_read_model.details.execution_groups.len(),
            initial_execution_group_count,
            "拒绝的深度任务不应生成新的执行图"
        );
    }

    #[tokio::test]
    async fn session_continue_route_executes_recovery_writeback_and_keeps_same_chain() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());

        let (status, seed_body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "session-route-loopback",
                "text": "seed recovery route state",
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {seed_body:?}"
        );

        let session_id = SessionId::new("session-route-loopback");
        let task_store = state.task_store().expect("task store should be configured");
        let seed_action_task_id = TaskId::new(
            seed_body["actionTaskId"]
                .as_str()
                .expect("seed action task id should be returned"),
        );
        wait_for_condition(
            Duration::from_secs(5),
            Duration::from_millis(20),
            || {
                task_store
                    .get_task(&seed_action_task_id)
                    .is_some_and(|task| task.status == TaskStatus::Completed)
            },
            "异步 seed dispatch 应先完成，测试再构造 recovery 状态",
        )
        .await;
        let chain = state
            .session_store
            .active_execution_chain(&session_id)
            .expect("active execution chain should exist after seed dispatch");
        let ownership = state
            .session_store
            .execution_ownership(&session_id)
            .expect("session ownership should exist after seed dispatch");
        let workspace_id = ownership
            .workspace_id
            .clone()
            .expect("seed dispatch should bind workspace");
        let recovery_task_id = ownership
            .task_id
            .clone()
            .expect("seed dispatch should bind task");
        task_store
            .update_status(&recovery_task_id, TaskStatus::Blocked)
            .expect("seed task should become recoverable");
        let snapshot = state.workspace_registry.append_execution_snapshot(
            workspace_id.clone(),
            ownership.clone(),
            "snapshot-route-recovery",
            "Route recovery snapshot",
        );
        let recovery = state.workspace_registry.prepare_recovery_entry(
            workspace_id,
            ownership,
            snapshot.snapshot_id,
            "recovery-route-1",
            Some("resume route followup".to_string()),
        );
        state
            .workspace_registry
            .mark_recovery_ready(&recovery.recovery_id)
            .expect("recovery should become ready");
        state
            .session_store
            .attach_recovery_ref(&session_id, Some(recovery.recovery_id.clone()))
            .expect("recovery ref should attach to session");

        let (status, body) = post_json(
            app,
            "/api/session/continue",
            json!({
                "sessionId": session_id.to_string(),
                "promptText": "继续刚才的恢复链",
                "requestId": "request-continue-route",
                "userMessageId": "local-user-continue-route",
                "placeholderMessageId": "local-assistant-continue-route",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected response body: {body:?}");
        assert_eq!(body["sessionId"], session_id.to_string());
        assert_eq!(body["missionId"], chain.mission_id.to_string());
        assert_eq!(body["rootTaskId"], chain.root_task_id.to_string());
        assert_eq!(body["executionChainRef"], chain.execution_chain_ref);
        assert!(
            body["resumedBranchCount"]
                .as_u64()
                .expect("resumed_branch_count should be integer")
                >= 1
        );

        let verification = state
            .execution_pipeline()
            .expect("execution pipeline should exist")
            .memory_store
            .verify_extraction_linkage("extract-session-continue-recovery-route-1")
            .expect("session continue extraction should persist");
        assert!(verification.is_consistent);
        let linkage = state
            .execution_pipeline()
            .expect("execution pipeline should exist")
            .memory_store
            .extraction_linkage("extract-session-continue-recovery-route-1")
            .expect("session continue extraction linkage should exist");
        assert_eq!(
            linkage.extraction.source_ref.as_deref(),
            Some("session-continue://recovery-route-1/snapshot/snapshot-route-recovery")
        );
        assert_eq!(linkage.produced_records[0].content, "resume route followup");

        let updated_chain = state
            .session_store
            .active_execution_chain(&session_id)
            .expect("active chain should still exist after continue");
        assert_eq!(updated_chain.mission_id, chain.mission_id);
        assert_eq!(updated_chain.root_task_id, chain.root_task_id);
        assert_eq!(updated_chain.execution_chain_ref, chain.execution_chain_ref);
        assert!(updated_chain.recovery_ref.is_none());
        let timeline = state.session_store.timeline_for_session(&session_id);
        assert!(
            timeline.iter().any(|entry| {
                matches!(entry.kind, TimelineEntryKind::UserMessage)
                    && entry.message == "继续刚才的恢复链"
            }),
            "自然语言 continue 应写入当前 session timeline"
        );
        let continue_user_entry = timeline
            .iter()
            .find(|entry| {
                matches!(entry.kind, TimelineEntryKind::UserMessage)
                    && entry.message == "继续刚才的恢复链"
            })
            .expect("continue 用户消息应存在");
        let current_turn = state
            .session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("continue 后 current turn 应保留");
        let continue_user_item = current_turn
            .items
            .iter()
            .find(|item| item.item_id == "local-user-continue-route")
            .expect("continue 用户消息应写入 current turn item");
        assert_eq!(continue_user_item.kind, "user_message");
        assert_eq!(
            continue_user_item.request_id.as_deref(),
            Some("request-continue-route")
        );
        assert_eq!(
            continue_user_item.user_message_id.as_deref(),
            Some("local-user-continue-route")
        );
        assert_eq!(
            continue_user_item.placeholder_message_id.as_deref(),
            Some("local-assistant-continue-route")
        );
        assert_eq!(
            continue_user_item.timeline_entry_id.as_deref(),
            Some(continue_user_entry.entry_id.as_str())
        );
        wait_for_condition(
            Duration::from_secs(5),
            Duration::from_millis(20),
            || {
                state
                    .runtime_read_model_dto()
                    .details
                    .sessions
                    .iter()
                    .find(|entry| entry.session_id == session_id.to_string())
                    .is_some_and(|entry| {
                        entry
                            .current_turn
                            .as_ref()
                            .is_some_and(|turn| turn.status == "completed")
                            && entry.turn_items.iter().any(|item| {
                                item.kind == "assistant_final"
                                    && item.content.as_deref().is_some_and(|content| {
                                        !content.trim().is_empty()
                                            && !content.contains("loopback-model::")
                                    })
                            })
                    })
            },
            "continue 路由执行完成后应把归一化 assistant 结果写入 canonical current turn",
        )
        .await;
        let runtime_session = state
            .runtime_read_model_dto()
            .details
            .sessions
            .into_iter()
            .find(|entry| entry.session_id == session_id.to_string())
            .expect("runtime read model should still contain current session");
        assert!(!runtime_session.has_recoverable_chain);
        assert_eq!(runtime_session.recoverable_branch_count, 0);
        assert!(
            task_store
                .get_tasks_by_mission(&MissionId::new("mission-recovery-recovery-route-1"))
                .is_empty(),
            "continue 不应再生成 recovery mission"
        );
    }

    #[tokio::test]
    async fn session_continue_route_resumes_only_requested_worker() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("session-route-loopback-override"),
                "loopback override session",
                Some("workspace-route-loopback".to_string()),
            )
            .expect("loopback override session should be creatable");

        let (status, seed_body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "session-route-loopback-override",
                "text": "seed recovery route state",
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {seed_body:?}"
        );

        let session_id = SessionId::new("session-route-loopback-override");
        let mut chain = state
            .session_store
            .active_execution_chain(&session_id)
            .expect("active execution chain should exist after seed dispatch");
        assert!(
            chain.branches.len() >= 2,
            "seed dispatch should create multiple worker branches for scoped continue"
        );
        chain.branches[0].checkpoint_stage = Some("execute".to_string());
        chain.branches[0].next_step_index = Some(2);
        chain.branches[0].resume_mode = Some("step-checkpoint".to_string());
        chain.branches[0].checkpoint_at = Some(UtcMillis::now());
        chain.branches[0].execution_intent_ref =
            Some("worker-intent-scoped-continue-preferred".to_string());
        chain.branches[0].binding_lifecycle = Some("requested".to_string());
        chain.branches[1].worker_id = WorkerId::new("worker-route-held");
        chain.active_worker_bindings = chain
            .branches
            .iter()
            .map(|branch| branch.worker_id.clone())
            .collect();
        state
            .session_store
            .upsert_active_execution_chain(session_id.clone(), chain)
            .expect("active execution chain should accept test worker bindings");
        let chain = state
            .session_store
            .active_execution_chain(&session_id)
            .expect("active execution chain should remain available after rebinding");
        let original_workers = chain
            .branches
            .iter()
            .map(|branch| branch.worker_id.to_string())
            .collect::<Vec<_>>();
        let preferred_branch = chain
            .branches
            .first()
            .expect("seed dispatch should create at least one branch")
            .clone();
        let held_branch = chain
            .branches
            .iter()
            .find(|branch| branch.worker_id != preferred_branch.worker_id)
            .expect("seed dispatch should create an unrequested branch")
            .clone();
        let preferred_worker = preferred_branch.worker_id.to_string();
        let ownership = state
            .session_store
            .execution_ownership(&session_id)
            .expect("session ownership should exist after seed dispatch");
        let task_store = state.task_store().expect("task store should be configured");
        let recovery_task_id = ownership
            .task_id
            .clone()
            .expect("seed dispatch should bind task");
        let mut preferred_ancestor_ids = Vec::new();
        let mut current_parent = task_store
            .get_task(&preferred_branch.task_id)
            .expect("preferred branch task should exist")
            .parent_task_id;
        while let Some(parent_id) = current_parent {
            if parent_id == chain.root_task_id {
                break;
            }
            let parent_task = task_store
                .get_task(&parent_id)
                .expect("preferred branch ancestor should exist");
            preferred_ancestor_ids.push(parent_id);
            current_parent = parent_task.parent_task_id;
        }
        assert!(
            !preferred_ancestor_ids.is_empty(),
            "scoped continue regression must cover a deep task path"
        );
        task_store
            .update_status(&chain.root_task_id, TaskStatus::Blocked)
            .expect("root task should become recoverable");
        task_store
            .update_status(&recovery_task_id, TaskStatus::Blocked)
            .expect("seed task should become recoverable");
        for ancestor_id in &preferred_ancestor_ids {
            task_store
                .update_status(ancestor_id, TaskStatus::Blocked)
                .expect("preferred branch ancestor should become recoverable");
        }
        task_store
            .update_status(&preferred_branch.task_id, TaskStatus::Blocked)
            .expect("preferred branch should become recoverable");
        task_store
            .update_status(&held_branch.task_id, TaskStatus::Blocked)
            .expect("held branch should remain blocked until explicitly requested");
        let worker_runtime = state
            .execution_pipeline()
            .expect("execution pipeline should exist")
            .execution_runtime
            .worker_runtime()
            .clone();
        worker_runtime.restore_durable_snapshot(
            magi_worker_runtime::WorkerRuntimeDurableSnapshot {
                branches: Vec::new(),
            },
        );
        assert!(
            worker_runtime
                .branch_snapshot_for_task(&preferred_branch.task_id)
                .is_none(),
            "测试前先模拟 worker runtime snapshot 缺失"
        );

        let (status, body) = post_json(
            app,
            "/api/session/continue",
            json!({
                "sessionId": session_id.to_string(),
                "requestedWorkerIds": [preferred_worker],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected response body: {body:?}");
        assert_eq!(body["executionChainRef"], chain.execution_chain_ref);
        assert_eq!(body["resumedBranchCount"], 1);

        let resumed_task = task_store
            .get_task(&preferred_branch.task_id)
            .expect("preferred branch should remain queryable");
        assert_ne!(resumed_task.status, TaskStatus::Blocked);
        for ancestor_id in &preferred_ancestor_ids {
            let ancestor = task_store
                .get_task(ancestor_id)
                .expect("preferred branch ancestor should remain queryable");
            assert_ne!(
                ancestor.status,
                TaskStatus::Blocked,
                "requested worker 的祖先任务也必须被释放: {}",
                ancestor_id
            );
        }
        let worker_snapshot = worker_runtime
            .branch_snapshot_for_task(&preferred_branch.task_id)
            .expect("continue 应从 session sidecar 回填 worker runtime checkpoint");
        let checkpoint = worker_snapshot
            .checkpoint_cursor
            .expect("scoped continue 应恢复 checkpoint cursor");
        assert_eq!(
            checkpoint.next_step_index, 2,
            "sidecar checkpoint 的 next_step_index 必须回填到 worker runtime"
        );
        assert_eq!(
            checkpoint.resume_mode,
            magi_worker_runtime::WorkerCheckpointResumeMode::StepCheckpoint
        );
        let held_task = task_store
            .get_task(&held_branch.task_id)
            .expect("held branch should remain queryable");
        assert_eq!(held_task.status, TaskStatus::Blocked);

        let updated_chain = state
            .session_store
            .active_execution_chain(&session_id)
            .expect("active execution chain should remain available");
        let updated_workers = updated_chain
            .branches
            .iter()
            .map(|branch| branch.worker_id.to_string())
            .collect::<Vec<_>>();
        assert_eq!(updated_workers, original_workers);
    }

    #[tokio::test]
    async fn session_continue_route_skips_runtime_finished_worker_even_when_sidecar_is_execute() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("session-runtime-finished-worker"),
                "runtime finished worker session",
                Some("workspace-route-loopback".to_string()),
            )
            .expect("runtime finished worker session should be creatable");

        let (status, seed_body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "session-runtime-finished-worker",
                "text": "seed runtime finished worker state",
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {seed_body:?}"
        );

        let session_id = SessionId::new("session-runtime-finished-worker");
        let mut chain = state
            .session_store
            .active_execution_chain(&session_id)
            .expect("active execution chain should exist after seed dispatch");
        assert!(
            chain.branches.len() >= 2,
            "seed dispatch should create multiple worker branches for runtime finish regression"
        );
        chain.branches.truncate(2);
        for branch in &mut chain.branches {
            branch.stage = "execute".to_string();
            branch.checkpoint_stage = Some("execute".to_string());
            branch.resume_mode = Some("stage-restart".to_string());
            branch.checkpoint_at = Some(UtcMillis::now());
        }
        chain.branches[1].worker_id = WorkerId::new("worker-runtime-finished");
        chain.active_worker_bindings = chain
            .branches
            .iter()
            .map(|branch| branch.worker_id.clone())
            .collect();
        state
            .session_store
            .upsert_active_execution_chain(session_id.clone(), chain)
            .expect("active execution chain should accept runtime finished fixture");
        let chain = state
            .session_store
            .active_execution_chain(&session_id)
            .expect("active execution chain should remain available after fixture update");
        let resumable_branch = chain.branches[0].clone();
        let finished_branch = chain.branches[1].clone();
        let finished_worker = finished_branch.worker_id.to_string();

        let task_store = state.task_store().expect("task store should be configured");
        task_store
            .update_status(&chain.root_task_id, TaskStatus::Blocked)
            .expect("root task should become recoverable");
        for branch in &chain.branches {
            task_store
                .update_status(&branch.task_id, TaskStatus::Blocked)
                .expect("branch task should become recoverable");
        }

        let worker_runtime = state
            .execution_pipeline()
            .expect("execution pipeline should exist")
            .execution_runtime
            .worker_runtime()
            .clone();
        worker_runtime.restore_durable_snapshot(
            magi_worker_runtime::WorkerRuntimeDurableSnapshot {
                branches: vec![magi_worker_runtime::WorkerRuntimeBranchSnapshot {
                    task_id: finished_branch.task_id.clone(),
                    worker_id: finished_branch.worker_id.clone(),
                    stage: magi_worker_runtime::WorkerStage::Finish,
                    lease_id: finished_branch.lease_id.as_ref().map(ToString::to_string),
                    execution_intent_ref: finished_branch.execution_intent_ref.clone(),
                    binding_lifecycle: None,
                    checkpoint_cursor: None,
                }],
            },
        );

        let (status, body) = post_json(
            app.clone(),
            "/api/session/continue",
            json!({
                "sessionId": session_id.to_string(),
                "requestedWorkerIds": [finished_worker],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body:?}");
        assert_eq!(body["message"], "请求继续的 worker 当前不可继续");
        let finalized_task = task_store
            .get_task(&finished_branch.task_id)
            .expect("runtime finished branch task should remain queryable");
        assert_eq!(
            finalized_task.status,
            TaskStatus::Completed,
            "runtime snapshot 已 finish 时，continue 前必须先把 branch 收敛为终态"
        );
        assert!(
            finalized_task
                .evidence_refs
                .iter()
                .any(|evidence| evidence.starts_with("evidence://worker-runtime/")),
            "runtime snapshot 作为终态来源时必须写入可追踪 evidence，不能绕过完成约束"
        );

        let (status, body) = post_json(
            app,
            "/api/session/continue",
            json!({
                "sessionId": session_id.to_string(),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected response body: {body:?}");
        assert_eq!(body["resumedBranchCount"], 1);
        let resumed_task = task_store
            .get_task(&resumable_branch.task_id)
            .expect("remaining branch task should remain queryable");
        assert_ne!(
            resumed_task.status,
            TaskStatus::Blocked,
            "未完成 branch 应继续恢复，已完成 branch 不应重新进入恢复集合"
        );
        let finished_task = task_store
            .get_task(&finished_branch.task_id)
            .expect("runtime finished branch task should remain queryable after continue");
        assert_eq!(finished_task.status, TaskStatus::Completed);
    }

    #[tokio::test]
    async fn session_continue_route_returns_not_found_for_unknown_recovery() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("session-route-missing-recovery"),
                "missing recovery session",
                Some("workspace-route-loopback".to_string()),
            )
            .expect("missing recovery session should be creatable");

        let (status, seed_body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "session-route-missing-recovery",
                "text": "seed missing recovery state",
                "skillName": "refactor",
                "images": [],
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {seed_body:?}"
        );

        let session_id = SessionId::new("session-route-missing-recovery");
        let ownership = state
            .session_store
            .execution_ownership(&session_id)
            .expect("session ownership should exist after seed dispatch");
        let recovery_task_id = ownership
            .task_id
            .clone()
            .expect("seed dispatch should bind task");
        state
            .task_store()
            .expect("task store should be configured")
            .update_status(&recovery_task_id, TaskStatus::Blocked)
            .expect("seed task should become recoverable");
        state
            .session_store
            .attach_recovery_ref(&session_id, Some("missing-recovery".to_string()))
            .expect("recovery ref should attach to session");

        let (status, body) = post_json(
            app,
            "/api/session/continue",
            json!({
                "sessionId": session_id.to_string(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error_code"], "RECOVERY_NOT_FOUND");
        assert_eq!(body["message"], "恢复入口不存在: missing-recovery");
    }

    #[tokio::test]
    async fn session_continue_route_rejects_prepared_recovery_with_input_error() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("session-route-prepared"),
                "prepared recovery session",
                Some("workspace-route-loopback".to_string()),
            )
            .expect("prepared recovery session should be creatable");

        let (status, seed_body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "session-route-prepared",
                "text": "seed prepared recovery state",
                "skillName": "resume",
                "images": [],
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {seed_body:?}"
        );

        let session_id = SessionId::new("session-route-prepared");
        let ownership = state
            .session_store
            .execution_ownership(&session_id)
            .expect("session ownership should exist after seed dispatch");
        let workspace_id = ownership
            .workspace_id
            .clone()
            .expect("seed dispatch should bind workspace");
        let recovery_task_id = ownership
            .task_id
            .clone()
            .expect("seed dispatch should bind task");
        state
            .task_store()
            .expect("task store should be configured")
            .update_status(&recovery_task_id, TaskStatus::Blocked)
            .expect("seed task should become recoverable");
        let recovery = state.workspace_registry.prepare_recovery_entry(
            workspace_id.clone(),
            ownership,
            "snapshot-route-prepared",
            "recovery-route-prepared",
            Some("prepared recovery".to_string()),
        );
        state
            .session_store
            .attach_recovery_ref(&session_id, Some(recovery.recovery_id.clone()))
            .expect("recovery ref should attach to session");

        let (status, body) = post_json(
            app,
            "/api/session/continue",
            json!({
                "sessionId": session_id.to_string(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error_code"], "INPUT_INVALID");
        assert_eq!(
            body["message"],
            "继续检查点 recovery-route-prepared 当前状态为 prepared，必须先进入 ready 才能继续会话"
        );
    }

    #[tokio::test]
    async fn session_continue_route_rejects_consumed_recovery_with_input_error() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("session-route-consumed"),
                "consumed recovery session",
                Some("workspace-route-loopback".to_string()),
            )
            .expect("consumed recovery session should be creatable");

        let (status, seed_body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "session-route-consumed",
                "text": "seed consumed recovery state",
                "skillName": "resume",
                "images": [],
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {seed_body:?}"
        );

        let session_id = SessionId::new("session-route-consumed");
        let ownership = state
            .session_store
            .execution_ownership(&session_id)
            .expect("session ownership should exist after seed dispatch");
        let workspace_id = ownership
            .workspace_id
            .clone()
            .expect("seed dispatch should bind workspace");
        let recovery_task_id = ownership
            .task_id
            .clone()
            .expect("seed dispatch should bind task");
        state
            .task_store()
            .expect("task store should be configured")
            .update_status(&recovery_task_id, TaskStatus::Blocked)
            .expect("seed task should become recoverable");
        let recovery = state.workspace_registry.prepare_recovery_entry(
            workspace_id.clone(),
            ownership,
            "snapshot-route-consumed",
            "recovery-route-consumed",
            Some("consumed recovery".to_string()),
        );
        state
            .workspace_registry
            .mark_recovery_ready(&recovery.recovery_id)
            .expect("recovery should become ready");
        state
            .workspace_registry
            .consume_recovery(&recovery.recovery_id)
            .expect("recovery should become consumed");
        state
            .session_store
            .attach_recovery_ref(&session_id, Some(recovery.recovery_id.clone()))
            .expect("recovery ref should attach to session");

        let (status, body) = post_json(
            app,
            "/api/session/continue",
            json!({
                "sessionId": session_id.to_string(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error_code"], "INPUT_INVALID");
        assert_eq!(
            body["message"],
            "继续检查点 recovery-route-consumed 已被消费，不能再次继续会话"
        );
    }

    #[derive(Clone)]
    enum FakeTransportOutcome {
        Payload(Value),
        RemoteBusiness {
            code: i64,
            message: String,
            data: Option<Value>,
        },
        Protocol {
            message: String,
        },
    }

    struct FakeTransport {
        responses: Mutex<HashMap<String, FakeTransportOutcome>>,
    }

    struct StaticModelBridgeClient;

    struct PlainJsonClassifierModelBridgeClient;

    struct TaskRouteClassifierModelBridgeClient;

    struct AssignmentDispatchStreamingModelBridgeClient;

    struct ExecuteToolModelBridgeClient {
        invoke_count: AtomicUsize,
    }

    struct ExecuteToolMissingFinalModelBridgeClient {
        invoke_count: AtomicUsize,
    }

    struct DelayedModelBridgeClient {
        delay: Duration,
        payload: String,
    }

    impl ModelBridgeClient for StaticModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            if let Some(payload) = classifier_payload_for_prompt(&request.prompt) {
                return Ok(BridgeResponse { ok: true, payload });
            }
            if request.prompt.contains("任务图规划器") {
                return Ok(BridgeResponse {
                    ok: true,
                    payload: serde_json::json!({
                        "phases": [
                            {
                                "title": "Phase 1",
                                "workPackages": [
                                    {
                                        "title": "WP 1",
                                        "actions": [
                                            { "title": "Action 1", "goal": "Do first thing", "dependsOn": [], "writeScope": null }
                                        ]
                                    }
                                ]
                            },
                            {
                                "title": "Phase 2",
                                "workPackages": [
                                    {
                                        "title": "WP 2",
                                        "actions": [
                                            { "title": "Action 2", "goal": "Do second thing", "dependsOn": [], "writeScope": null }
                                        ]
                                    }
                                ]
                            }
                        ]
                    })
                    .to_string(),
                });
            }
            Ok(BridgeResponse {
                ok: true,
                payload: format!("loopback-model::{}", request.prompt.trim()),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for PlainJsonClassifierModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            if request.prompt.contains("Session Turn 编排分类器") {
                return Ok(BridgeResponse {
                    ok: true,
                    payload: serde_json::json!({
                        "route": "chat",
                        "taskTitle": null,
                        "executionGoal": null,
                        "requiredWorkers": [],
                        "toolIntent": null,
                    })
                    .to_string(),
                });
            }
            Ok(BridgeResponse {
                ok: true,
                payload: "loopback-model::ok".to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for TaskRouteClassifierModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            if request.prompt.contains("Session Turn 编排分类器") {
                return Ok(BridgeResponse {
                    ok: true,
                    payload: classifier_tool_payload(serde_json::json!({
                        "route": "task",
                        "taskTitle": "误判任务",
                        "executionGoal": "这个普通对话不应该进入任务图",
                        "requiredWorkers": ["integration-dev"],
                        "toolIntent": "不应泄漏到普通对话 prompt",
                        "confidence": 0.41,
                        "reasonCode": "plain_chat",
                        "routeReason": "普通对话缺少任务证据",
                        "taskEvidence": [],
                    })),
                });
            }
            Ok(BridgeResponse {
                ok: true,
                payload: "loopback-model::普通对话回复".to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for AssignmentDispatchStreamingModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            if request.prompt.contains("Session Turn 编排分类器") {
                return Ok(BridgeResponse {
                    ok: true,
                    payload: classifier_tool_payload(serde_json::json!({
                        "route": "chat",
                        "taskTitle": null,
                        "executionGoal": null,
                        "requiredWorkers": [],
                        "toolIntent": null,
                    })),
                });
            }
            Ok(BridgeResponse {
                ok: true,
                payload: serde_json::json!({
                    "content": assignment_dispatch_visible_leak_fixture(),
                    "finish_reason": "stop",
                })
                .to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            let content = assignment_dispatch_visible_leak_fixture();
            on_delta(&ModelStreamingDelta {
                content: "分析完成。".to_string(),
                thinking: String::new(),
            });
            on_delta(&ModelStreamingDelta {
                content: content[..content
                    .find("\"tasks\"")
                    .expect("fixture should contain tasks")]
                    .to_string(),
                thinking: String::new(),
            });
            on_delta(&ModelStreamingDelta {
                content,
                thinking: String::new(),
            });
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for ExecuteToolModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            if request.prompt.contains("Session Turn 编排分类器") {
                return Ok(BridgeResponse {
                    ok: true,
                    payload: classifier_tool_payload(serde_json::json!({
                        "route": "execute",
                        "taskTitle": null,
                        "executionGoal": null,
                        "requiredWorkers": [],
                        "toolIntent": null,
                    })),
                });
            }
            let index = self.invoke_count.fetch_add(1, Ordering::SeqCst);
            let payload = if index == 0 {
                serde_json::json!({
                    "content": null,
                    "finish_reason": "tool_calls",
                    "tool_calls": [{
                        "id": "call-search-text",
                        "type": "function",
                        "function": {
                            "name": "search_text",
                            "arguments": serde_json::json!({
                                "query": "Route Loopback Session",
                                "root": ".",
                                "limit": 1
                            }).to_string(),
                        }
                    }]
                })
            } else {
                serde_json::json!({
                    "content": "工具执行完成，已根据搜索结果给出回复。",
                    "finish_reason": "stop"
                })
            };
            Ok(BridgeResponse {
                ok: true,
                payload: payload.to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for ExecuteToolMissingFinalModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            if request.prompt.contains("Session Turn 编排分类器") {
                return Ok(BridgeResponse {
                    ok: true,
                    payload: classifier_tool_payload(serde_json::json!({
                        "route": "execute",
                        "taskTitle": null,
                        "executionGoal": null,
                        "requiredWorkers": [],
                        "toolIntent": null,
                    })),
                });
            }
            let index = self.invoke_count.fetch_add(1, Ordering::SeqCst);
            let payload = if index == 0 {
                serde_json::json!({
                    "content": null,
                    "finish_reason": "tool_calls",
                    "tool_calls": [{
                        "id": "call-search-text",
                        "type": "function",
                        "function": {
                            "name": "search_text",
                            "arguments": serde_json::json!({
                                "query": "Route Loopback Session",
                                "root": ".",
                                "limit": 1
                            }).to_string(),
                        }
                    }]
                })
            } else {
                serde_json::json!({
                    "content": null,
                    "finish_reason": "stop",
                    "tool_calls": []
                })
            };
            Ok(BridgeResponse {
                ok: true,
                payload: payload.to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for DelayedModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            if let Some(payload) = classifier_payload_for_prompt(&request.prompt) {
                return Ok(BridgeResponse { ok: true, payload });
            }
            if request.prompt.contains("任务图规划器") {
                return Ok(BridgeResponse {
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
            thread::sleep(self.delay);
            Ok(BridgeResponse {
                ok: true,
                payload: self.payload.clone(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            self.invoke(request)
        }
    }

    struct FailingModelBridgeClient;

    struct ContinueClassifierExpectingNoRecoverableChain;

    struct SessionSwitchingToolModelBridgeClient {
        session_store: Arc<SessionStore>,
        switch_to: SessionId,
        invoke_count: AtomicUsize,
    }

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
        } else if !prompt.contains("skillName=\"\"")
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
            "confidence": if route == "task" { 0.93 } else { 0.88 },
            "reasonCode": match route {
                "task" => "explicit_task_request",
                "execute" => "tool_request",
                "continue" => "continue_requested",
                _ => "plain_chat",
            },
            "routeReason": match route {
                "task" => "用户请求需要结构化任务执行",
                "execute" => "用户请求需要工具执行但不需要任务图",
                "continue" => "用户要求继续且存在可恢复链",
                _ => "普通对话",
            },
            "taskEvidence": if route == "task" {
                vec!["需要结构化执行".to_string()]
            } else {
                Vec::<String>::new()
            },
        });
        Some(classifier_tool_payload(arguments))
    }

    fn classifier_tool_payload(arguments: serde_json::Value) -> String {
        let arguments = normalize_classifier_tool_arguments(arguments);
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
        .to_string()
    }

    fn normalize_classifier_tool_arguments(mut arguments: serde_json::Value) -> serde_json::Value {
        let Some(map) = arguments.as_object_mut() else {
            return arguments;
        };
        let route = map
            .get("route")
            .and_then(|value| value.as_str())
            .unwrap_or("chat")
            .to_string();
        map.entry("confidence".to_string())
            .or_insert_with(|| serde_json::json!(if route == "task" { 0.93 } else { 0.88 }));
        map.entry("reasonCode".to_string()).or_insert_with(|| {
            serde_json::json!(match route.as_str() {
                "task" => "explicit_task_request",
                "execute" => "tool_request",
                "continue" => "continue_requested",
                _ => "plain_chat",
            })
        });
        map.entry("routeReason".to_string()).or_insert_with(|| {
            serde_json::json!(match route.as_str() {
                "task" => "用户请求需要结构化任务执行",
                "execute" => "用户请求需要工具执行但不需要任务图",
                "continue" => "用户要求继续且存在可恢复链",
                _ => "普通对话",
            })
        });
        map.entry("taskEvidence".to_string()).or_insert_with(|| {
            if route == "task" {
                serde_json::json!(["需要结构化执行"])
            } else {
                serde_json::json!([])
            }
        });
        arguments
    }

    fn assignment_dispatch_visible_leak_fixture() -> String {
        r#"分析完成。
我将安排以下任务：
```json
{
  "mission_title": "实现用户认证",
  "tasks": [{
    "task_name": "实现 JWT 验证",
    "ownership_hint": "backend",
    "mode_hint": "implement",
    "goal": "实现 JWT token 验证中间件",
    "acceptance": ["通过单元测试"],
    "constraints": ["使用现有模块"],
    "context": ["auth"],
    "requires_modification": true
  }]
}
```"#
            .to_string()
    }

    fn assert_no_assignment_dispatch_leak(content: &str) {
        assert!(
            !content.contains("\"tasks\"")
                && !content.contains("\"mission_title\"")
                && !content.contains("```json"),
            "用户可见内容不能泄漏 assignment dispatch JSON: {content}"
        );
    }

    impl ModelBridgeClient for FailingModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            Err(magi_bridge_client::BridgeClientError::CallFailed {
                layer: magi_bridge_client::BridgeErrorLayer::RemoteBusiness,
                code: Some(-32099),
                message: "model bridge unavailable".to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for ContinueClassifierExpectingNoRecoverableChain {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            if request.prompt.contains("Session Turn 编排分类器") {
                assert!(
                    request
                        .prompt
                        .lines()
                        .any(|line| line.trim() == "hasRecoverableChain=false"),
                    "finish 阶段 branch 不能让分类器看到可继续链"
                );
                return Ok(BridgeResponse {
                    ok: true,
                    payload: classifier_tool_payload(serde_json::json!({
                        "route": "continue",
                        "taskTitle": null,
                        "executionGoal": null,
                        "requiredWorkers": [],
                        "toolIntent": null,
                    })),
                });
            }
            Ok(BridgeResponse {
                ok: true,
                payload: "loopback-model::ok".to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for SessionSwitchingToolModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            if let Some(payload) = classifier_payload_for_prompt(&request.prompt) {
                return Ok(BridgeResponse { ok: true, payload });
            }
            if request.prompt.contains("请将以下任务分解") {
                return Ok(BridgeResponse {
                    ok: true,
                    payload: "读取配置文件\n总结配置内容".to_string(),
                });
            }
            if request.prompt.contains("任务图规划器") {
                return Ok(BridgeResponse {
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

            let round = self.invoke_count.fetch_add(1, Ordering::SeqCst);
            if round == 0 {
                self.session_store
                    .switch_session(&self.switch_to)
                    .expect("test helper should switch current session");
                return Ok(BridgeResponse {
                    ok: true,
                    payload: serde_json::json!({
                        "content": "先读取当前仓库配置。",
                        "finish_reason": "tool_calls",
                        "tool_calls": [{
                            "id": "call_session_bound_read",
                            "type": "function",
                            "function": {
                                "name": "file_read",
                                "arguments": serde_json::json!({
                                    "path": "/Users/xie/code/magi-rust-rewrite/Cargo.toml"
                                })
                                .to_string(),
                            }
                        }]
                    })
                    .to_string(),
                });
            }

            Ok(BridgeResponse {
                ok: true,
                payload: serde_json::json!({
                    "content": "读取完成。",
                    "finish_reason": "stop",
                    "tool_calls": []
                })
                .to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            self.invoke(request)
        }
    }

    impl FakeTransport {
        fn new(responses: HashMap<String, FakeTransportOutcome>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }

    struct ProviderAwareModelTransport;

    struct CutoverAwareModelTransport;

    impl BridgeTransport for ProviderAwareModelTransport {
        fn call(
            &self,
            request: BridgeTransportRequest,
        ) -> Result<BridgeTransportResponse, BridgeTransportError> {
            match request.method.as_str() {
                "model.invoke" => {
                    let provider = request
                        .params
                        .get("provider")
                        .and_then(Value::as_str)
                        .ok_or_else(|| BridgeTransportError::Protocol {
                            message: "model.invoke missing provider".to_string(),
                        })?;
                    let payload = match provider {
                        LOOPBACK_MODEL_PROVIDER => {
                            bridge_response("loopback-model::bridge preflight ping")
                        }
                        "openai-compatible" => {
                            bridge_response("openai-compatible::bridge preflight ping")
                        }
                        other => {
                            return Err(BridgeTransportError::Protocol {
                                message: format!("unexpected provider {other}"),
                            });
                        }
                    };
                    Ok(BridgeTransportResponse { payload })
                }
                LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD => Ok(BridgeTransportResponse {
                    payload: serde_json::to_value(BridgeServerServiceCatalog {
                        protocol_version: "local-bridge-v1".to_string(),
                        server_kind: BridgeServerKind::Model,
                        services: vec![
                            descriptor(LOOPBACK_MODEL_PROVIDER),
                            descriptor_with_health("openai-compatible", "ready"),
                        ],
                    })
                    .expect("model service catalog should serialize"),
                }),
                other => Err(BridgeTransportError::Protocol {
                    message: format!("unexpected method {other}"),
                }),
            }
        }
    }

    impl BridgeTransport for CutoverAwareModelTransport {
        fn call(
            &self,
            request: BridgeTransportRequest,
        ) -> Result<BridgeTransportResponse, BridgeTransportError> {
            match request.method.as_str() {
                "model.invoke" => {
                    let provider = request
                        .params
                        .get("provider")
                        .and_then(Value::as_str)
                        .ok_or_else(|| BridgeTransportError::Protocol {
                            message: "model.invoke missing provider".to_string(),
                        })?;
                    let payload = match provider {
                        LOOPBACK_MODEL_PROVIDER => {
                            bridge_response("loopback-model::bridge cutover smoke")
                        }
                        "openai-compatible" => structured_bridge_response(json!({
                            "content": "hello from cutover smoke",
                            "finish_reason": "tool_calls",
                            "usage": {
                                "total_tokens": 17,
                            },
                            "tool_calls": [{
                                "function": {
                                    "name": "demo.lookup",
                                    "arguments": "{\"city\":\"Paris\"}",
                                }
                            }],
                        })),
                        other => {
                            return Err(BridgeTransportError::Protocol {
                                message: format!("unexpected provider {other}"),
                            });
                        }
                    };
                    Ok(BridgeTransportResponse { payload })
                }
                LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD => Ok(BridgeTransportResponse {
                    payload: serde_json::to_value(BridgeServerServiceCatalog {
                        protocol_version: "local-bridge-v1".to_string(),
                        server_kind: BridgeServerKind::Model,
                        services: vec![
                            descriptor_with_profile(
                                LOOPBACK_MODEL_PROVIDER,
                                "ready",
                                "model-bridge-payload-v1",
                            ),
                            descriptor_with_profile(
                                "openai-compatible",
                                "ready",
                                "openai-compatible-chat-completions-v1",
                            ),
                        ],
                    })
                    .expect("model service catalog should serialize"),
                }),
                other => Err(BridgeTransportError::Protocol {
                    message: format!("unexpected method {other}"),
                }),
            }
        }
    }

    impl BridgeTransport for FakeTransport {
        fn call(
            &self,
            request: BridgeTransportRequest,
        ) -> Result<BridgeTransportResponse, BridgeTransportError> {
            let responses = self.responses.lock().expect("responses lock should hold");
            let outcome = responses.get(&request.method).cloned().unwrap_or_else(|| {
                FakeTransportOutcome::Protocol {
                    message: format!("unexpected method {}", request.method),
                }
            });
            match outcome {
                FakeTransportOutcome::Payload(payload) => Ok(BridgeTransportResponse { payload }),
                FakeTransportOutcome::RemoteBusiness {
                    code,
                    message,
                    data,
                } => Err(BridgeTransportError::RemoteBusiness {
                    code,
                    message,
                    data,
                }),
                FakeTransportOutcome::Protocol { message } => {
                    Err(BridgeTransportError::Protocol { message })
                }
            }
        }
    }

    fn handshake(kind: BridgeServerKind) -> Value {
        serde_json::to_value(BridgeServerHandshake {
            protocol_version: "local-bridge-v1".to_string(),
            server_kind: kind,
            health_method: LOCAL_BRIDGE_HEALTH_METHOD.to_string(),
            supported_methods: vec!["bridge.describe_services".to_string()],
        })
        .expect("handshake should serialize")
    }

    fn health(kind: BridgeServerKind, status: &str, ok: bool) -> Value {
        serde_json::to_value(BridgeServerHealth {
            protocol_version: "local-bridge-v1".to_string(),
            server_kind: kind,
            status: status.to_string(),
            ok,
        })
        .expect("health should serialize")
    }

    fn catalog(kind: BridgeServerKind, service_name: &str) -> Value {
        serde_json::to_value(BridgeServerServiceCatalog {
            protocol_version: "local-bridge-v1".to_string(),
            server_kind: kind,
            services: vec![descriptor(service_name)],
        })
        .expect("catalog should serialize")
    }

    fn descriptor(service_name: &str) -> magi_bridge_client::BridgeServerServiceDescriptor {
        descriptor_with_health(service_name, "healthy")
    }

    fn descriptor_with_health(
        service_name: &str,
        service_health: &str,
    ) -> magi_bridge_client::BridgeServerServiceDescriptor {
        descriptor_with_details(service_name, service_health, None, None, None)
    }

    fn descriptor_with_profile(
        service_name: &str,
        service_health: &str,
        capability_profile: &str,
    ) -> magi_bridge_client::BridgeServerServiceDescriptor {
        descriptor_with_details(
            service_name,
            service_health,
            Some(capability_profile),
            None,
            None,
        )
    }

    fn descriptor_with_route(
        service_name: &str,
        default_route_status: &str,
        default_route_target: &str,
    ) -> magi_bridge_client::BridgeServerServiceDescriptor {
        descriptor_with_details(
            service_name,
            "healthy",
            None,
            Some(default_route_status),
            Some(default_route_target),
        )
    }

    fn descriptor_with_details(
        service_name: &str,
        service_health: &str,
        capability_profile: Option<&str>,
        default_route_status: Option<&str>,
        default_route_target: Option<&str>,
    ) -> magi_bridge_client::BridgeServerServiceDescriptor {
        magi_bridge_client::BridgeServerServiceDescriptor {
            service_name: service_name.to_string(),
            shim_kind: "loopback".to_string(),
            supported_operations: vec!["inspect".to_string()],
            capabilities: vec!["service_catalog".to_string()],
            service_health: Some(service_health.to_string()),
            service_health_reason: None,
            implementation_source: None,
            capability_profile: capability_profile.map(str::to_string),
            workspace_roots_source: None,
            manager_version: None,
            registry_profile: None,
            registry_manifest: None,
            selection_strategy: None,
            default_server: None,
            default_server_health: None,
            default_server_selection_key: None,
            default_route_status: default_route_status.map(str::to_string),
            default_route_target: default_route_target.map(str::to_string),
            selection_targets: None,
            selection_key: None,
            server_manifest: None,
            shell_manifest: None,
            shell_profile: None,
            command_capability_profiles: None,
            session_descriptor: None,
            workspace_context: None,
            context_resolution_boundary: None,
        }
    }

    fn bridge_response(payload: &str) -> Value {
        serde_json::to_value(BridgeResponse {
            ok: true,
            payload: payload.to_string(),
        })
        .expect("bridge response should serialize")
    }

    fn bridge_response_with_status(ok: bool, payload: &str) -> Value {
        serde_json::to_value(BridgeResponse {
            ok,
            payload: payload.to_string(),
        })
        .expect("bridge response should serialize")
    }

    fn structured_bridge_response(payload: Value) -> Value {
        serde_json::to_value(BridgeResponse {
            ok: true,
            payload: payload.to_string(),
        })
        .expect("structured bridge response should serialize")
    }

    #[tokio::test]
    async fn bridge_services_route_exports_probe_snapshot_from_fake_transport() {
        let app = build_router(test_state().with_bridge_probe_transport(
            BridgeServerKind::Model,
            Arc::new(FakeTransport::new(HashMap::from([
                (
                    LOCAL_BRIDGE_HANDSHAKE_METHOD.to_string(),
                    FakeTransportOutcome::Payload(handshake(BridgeServerKind::Model)),
                ),
                (
                    LOCAL_BRIDGE_HEALTH_METHOD.to_string(),
                    FakeTransportOutcome::Payload(health(BridgeServerKind::Model, "healthy", true)),
                ),
                (
                    LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
                    FakeTransportOutcome::Payload(catalog(BridgeServerKind::Model, "loopback-model")),
                ),
            ]))),
        ));

        let snapshot = get_json(app, "/bridges/services").await;
        assert_eq!(snapshot["services"][0]["server_kind"], "model");
        assert_eq!(snapshot["services"][0]["health"]["status"], "healthy");
        assert_eq!(
            snapshot["services"][0]["service_catalog"]["services"][0]["service_name"],
            "loopback-model"
        );
    }

    #[tokio::test]
    async fn bridge_preflight_route_executes_smoke_results_from_registered_transports() {
        let app = build_router(
            test_state()
                .with_bridge_probe_transport(
                    BridgeServerKind::Model,
                    Arc::new(FakeTransport::new(HashMap::from([
                        (
                            "model.invoke".to_string(),
                            FakeTransportOutcome::Payload(bridge_response(
                                "loopback-model::bridge preflight ping",
                            )),
                        ),
                        (
                            LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
                            FakeTransportOutcome::Payload(catalog(
                                BridgeServerKind::Model,
                                LOOPBACK_MODEL_PROVIDER,
                            )),
                        ),
                    ]))),
                )
                .with_bridge_probe_transport(
                    BridgeServerKind::Mcp,
                    Arc::new(FakeTransport::new(HashMap::from([
                        (
                            "mcp.list_servers".to_string(),
                            FakeTransportOutcome::Payload(
                                serde_json::to_value(McpManagerListServersResponse {
                                    manager: descriptor("loopback-mcp-manager"),
                                    servers: vec![descriptor(LOOPBACK_MCP_SERVER_NAME)],
                                    selection_targets: vec![LOOPBACK_MCP_SERVER_NAME.to_string()],
                                    default_route_status: "available".to_string(),
                                    default_route_target: LOOPBACK_MCP_SERVER_NAME.to_string(),
                                })
                                .expect("mcp manager list should serialize"),
                            ),
                        ),
                        (
                            "mcp.call_tool".to_string(),
                            FakeTransportOutcome::Payload(bridge_response("echo.inspect::ok")),
                        ),
                    ]))),
                ),
        );

        let snapshot = get_json(app, "/bridges/preflight").await;
        let services = snapshot["services"]
            .as_array()
            .expect("services should serialize as array");
        assert_eq!(
            services.len(),
            2,
            "unexpected preflight snapshot: {snapshot:?}"
        );

        let model = services
            .iter()
            .find(|entry| entry["server_kind"] == "model")
            .expect("model preflight should exist");
        assert_eq!(model["checks"][0]["check_name"], "invoke");
        assert_eq!(model["checks"][0]["target"], LOOPBACK_MODEL_PROVIDER);
        assert_eq!(model["checks"][0]["ok"], true);

        let mcp = services
            .iter()
            .find(|entry| entry["server_kind"] == "mcp")
            .expect("mcp preflight should exist");
        assert_eq!(mcp["checks"][0]["check_name"], "list_servers");
        assert_eq!(mcp["checks"][0]["target"], "loopback-mcp-manager");
        assert_eq!(mcp["checks"][0]["ok"], true);
        assert_eq!(
            mcp["checks"][1]["target"],
            format!("{LOOPBACK_MCP_SERVER_NAME}.{LOOPBACK_MCP_TOOL_NAME}")
        );
        assert_eq!(mcp["checks"][1]["ok"], true);
    }

    #[tokio::test]
    async fn bridge_preflight_route_executes_ready_openai_compatible_smoke() {
        let app = build_router(test_state().with_bridge_probe_transport(
            BridgeServerKind::Model,
            Arc::new(ProviderAwareModelTransport),
        ));

        let snapshot = get_json(app, "/bridges/preflight").await;
        let model = snapshot["services"]
            .as_array()
            .expect("services should serialize as array")
            .iter()
            .find(|entry| entry["server_kind"] == "model")
            .expect("model preflight should exist");
        let checks = model["checks"]
            .as_array()
            .expect("model checks should serialize as array");
        assert_eq!(checks.len(), 2, "unexpected model preflight: {model:?}");
        assert!(
            checks
                .iter()
                .any(|check| check["target"] == LOOPBACK_MODEL_PROVIDER && check["ok"] == true),
            "model preflight should keep loopback-model smoke: {model:?}"
        );
        assert!(
            checks
                .iter()
                .any(|check| check["target"] == "openai-compatible"
                    && check["ok"] == true
                    && check["response_excerpt"]
                        .as_str()
                        .expect("response excerpt should serialize as string")
                        .contains("openai-compatible::bridge preflight ping")),
            "model preflight should execute openai-compatible ready smoke: {model:?}"
        );
    }

    #[tokio::test]
    async fn bridge_cutover_smoke_route_evaluates_model_and_mcp_contracts() {
        let app = build_router(
            test_state()
                .with_bridge_probe_transport(
                    BridgeServerKind::Model,
                    Arc::new(CutoverAwareModelTransport),
                )
                .with_bridge_probe_transport(
                    BridgeServerKind::Mcp,
                    Arc::new(FakeTransport::new(HashMap::from([
                        (
                            "mcp.list_servers".to_string(),
                            FakeTransportOutcome::Payload(
                                serde_json::to_value(McpManagerListServersResponse {
                                    manager: descriptor_with_route(
                                        "loopback-mcp-manager",
                                        "ready",
                                        "loopback-mcp-observability",
                                    ),
                                    servers: vec![
                                        descriptor_with_profile(
                                            LOOPBACK_MCP_SERVER_NAME,
                                            "ready",
                                            "inspection-core-v1",
                                        ),
                                        descriptor_with_profile(
                                            "loopback-mcp-observability",
                                            "ready",
                                            "observability-v1",
                                        ),
                                    ],
                                    selection_targets: vec![
                                        LOOPBACK_MCP_SERVER_NAME.to_string(),
                                        "loopback-mcp-observability".to_string(),
                                    ],
                                    default_route_status: "ready".to_string(),
                                    default_route_target: "loopback-mcp-observability".to_string(),
                                })
                                .expect("mcp manager list should serialize"),
                            ),
                        ),
                        (
                            "mcp.describe_server".to_string(),
                            FakeTransportOutcome::Payload(json!({
                                "manager": descriptor_with_route(
                                    "loopback-mcp-manager",
                                    "ready",
                                    "loopback-mcp-observability",
                                ),
                                "server": descriptor_with_profile(
                                    "loopback-mcp-observability",
                                    "ready",
                                    "observability-v1",
                                ),
                                "lifecycle_events": [],
                            })),
                        ),
                        (
                            "mcp.call_tool".to_string(),
                            FakeTransportOutcome::Payload(structured_bridge_response(json!({
                                "server_name": "loopback-mcp-observability",
                                "default_route_status": "ready",
                                "default_route_target": "loopback-mcp-observability",
                                "tool_name": "echo.describe",
                            }))),
                        ),
                    ]))),
                ),
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        let services = snapshot["services"]
            .as_array()
            .expect("services should serialize as array");
        let failing_checks = services
            .iter()
            .flat_map(|service| {
                service["checks"]
                    .as_array()
                    .expect("checks should serialize as array")
                    .iter()
            })
            .filter(|check| check["ok"] != true)
            .count();
        assert_eq!(
            services.len(),
            2,
            "unexpected cutover smoke snapshot: {snapshot:?}"
        );
        assert_eq!(snapshot["overall_ok"], true);
        assert_eq!(snapshot["checked_service_count"], 2);
        assert_eq!(snapshot["blocking_check_count"], failing_checks);
        assert!(
            snapshot["blocking_services"]
                .as_array()
                .expect("blocking services should serialize as array")
                .is_empty(),
            "no service should block in fake ready path: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_issues"]
                .as_array()
                .expect("blocking issues should serialize as array")
                .is_empty(),
            "ready snapshot should not export blocking issues: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_issue_counts_by_reason_code"]
                .as_object()
                .expect("reason-code counts should serialize as object")
                .is_empty(),
            "ready snapshot should not export reason-code counts: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_issue_counts_by_server_kind"]
                .as_object()
                .expect("server-kind counts should serialize as object")
                .is_empty(),
            "ready snapshot should not export server-kind counts: {snapshot:?}"
        );
        assert!(services.iter().all(|service| {
            service["service_ok"] == true
                && service["blocking_check_count"] == 0
                && service["blocking_targets"]
                    .as_array()
                    .expect("blocking targets should serialize as array")
                    .is_empty()
        }));

        let model = services
            .iter()
            .find(|entry| entry["server_kind"] == "model")
            .expect("model cutover should exist");
        assert_eq!(model["service_ok"], true);
        assert_eq!(model["blocking_check_count"], 0);
        assert!(
            model["blocking_targets"]
                .as_array()
                .expect("model blocking targets should serialize as array")
                .is_empty()
        );
        assert!(
            model["checks"]
                .as_array()
                .expect("model checks should serialize as array")
                .iter()
                .any(|check| check["target"] == "openai-compatible"
                    && check["ok"] == true
                    && check["model_contract"]["payload_kind"] == "structured_json"
                    && check["model_contract"]["tool_call_count"] == 1),
            "openai-compatible cutover contract should be exported: {model:?}"
        );

        let mcp = services
            .iter()
            .find(|entry| entry["server_kind"] == "mcp")
            .expect("mcp cutover should exist");
        assert_eq!(mcp["service_ok"], true);
        assert_eq!(mcp["blocking_check_count"], 0);
        assert!(
            mcp["blocking_targets"]
                .as_array()
                .expect("mcp blocking targets should serialize as array")
                .is_empty()
        );
        assert_eq!(mcp["checks"][0]["check_name"], "default_route_contract");
        assert_eq!(mcp["checks"][0]["ok"], true);
        assert_eq!(mcp["checks"][0]["mcp_contract"]["route_status"], "ready");
        assert_eq!(
            mcp["checks"][0]["mcp_contract"]["route_target"],
            "loopback-mcp-observability"
        );
        assert_eq!(
            mcp["checks"][0]["mcp_contract"]["resolved_server"],
            "loopback-mcp-observability"
        );
        assert_eq!(mcp["mcp_default_route_gate"]["route_status"], "ready");
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_status"],
            mcp["checks"][0]["mcp_contract"]["route_status"]
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_target"],
            "loopback-mcp-observability"
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_target"],
            mcp["checks"][0]["mcp_contract"]["route_target"]
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["resolved_server"],
            "loopback-mcp-observability"
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["resolved_server"],
            mcp["checks"][0]["mcp_contract"]["resolved_server"]
        );
        assert_eq!(mcp["mcp_default_route_gate"]["contract_ok"], true);
        assert_eq!(
            mcp["mcp_default_route_gate"]["contract_ok"],
            mcp["checks"][0]["mcp_contract"]["contract_ok"]
        );
    }

    #[tokio::test]
    async fn bridge_cutover_smoke_route_surfaces_mcp_describe_errors() {
        let app = build_router(
            test_state().with_bridge_probe_transport(
                BridgeServerKind::Mcp,
                Arc::new(FakeTransport::new(HashMap::from([
                    (
                        "mcp.list_servers".to_string(),
                        FakeTransportOutcome::Payload(
                            serde_json::to_value(McpManagerListServersResponse {
                                manager: descriptor_with_route(
                                    "loopback-mcp-manager",
                                    "ready",
                                    "loopback-mcp-observability",
                                ),
                                servers: vec![descriptor_with_profile(
                                    "loopback-mcp-observability",
                                    "ready",
                                    "observability-v1",
                                )],
                                selection_targets: vec!["loopback-mcp-observability".to_string()],
                                default_route_status: "ready".to_string(),
                                default_route_target: "loopback-mcp-observability".to_string(),
                            })
                            .expect("mcp manager list should serialize"),
                        ),
                    ),
                    (
                        "mcp.describe_server".to_string(),
                        FakeTransportOutcome::Protocol {
                            message: "describe degraded".to_string(),
                        },
                    ),
                    (
                        "mcp.call_tool".to_string(),
                        FakeTransportOutcome::Payload(structured_bridge_response(json!({
                            "server_name": "loopback-mcp-observability",
                            "default_route_status": "ready",
                            "default_route_target": "loopback-mcp-observability",
                            "tool_name": "echo.describe",
                        }))),
                    ),
                ]))),
            ),
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        assert_eq!(snapshot["overall_ok"], false);
        assert_eq!(snapshot["blocking_check_count"], 1);
        assert_eq!(
            snapshot["blocking_issues"][0]["reason_code"],
            "mcp_default_route_target_describe_failed"
        );
        assert_eq!(
            snapshot["blocking_issue_counts_by_reason_code"]["mcp_default_route_target_describe_failed"],
            1
        );
        assert_eq!(snapshot["blocking_issue_counts_by_server_kind"]["mcp"], 1);
        assert_eq!(snapshot["blocking_issues"][0]["facet"], "mcp_default_route");
        assert_eq!(snapshot["blocking_issues"][0]["error"]["layer"], "Protocol");
        assert!(
            snapshot["blocking_issues"][0]["error"]["message"]
                .as_str()
                .expect("describe error should serialize as string")
                .contains("describe degraded"),
            "cutover issue should retain describe error details: {snapshot:?}"
        );
        assert_eq!(snapshot["services"][0]["service_ok"], false);
        assert_eq!(
            snapshot["services"][0]["mcp_default_route_gate"]["contract_ok"],
            false
        );
    }

    #[tokio::test]
    async fn bridge_cutover_smoke_route_surfaces_mcp_blank_selection_reason_codes() {
        let app = build_router(
            test_state().with_bridge_probe_transport(
                BridgeServerKind::Mcp,
                Arc::new(FakeTransport::new(HashMap::from([
                    (
                        "mcp.list_servers".to_string(),
                        FakeTransportOutcome::Payload(
                            serde_json::to_value(McpManagerListServersResponse {
                                manager: descriptor_with_route(
                                    "loopback-mcp-manager",
                                    "ready",
                                    "loopback-mcp-observability",
                                ),
                                servers: vec![descriptor_with_profile(
                                    "loopback-mcp-observability",
                                    "ready",
                                    "observability-v1",
                                )],
                                selection_targets: vec!["loopback-mcp-observability".to_string()],
                                default_route_status: "ready".to_string(),
                                default_route_target: "loopback-mcp-observability".to_string(),
                            })
                            .expect("mcp manager list should serialize"),
                        ),
                    ),
                    (
                        "mcp.describe_server".to_string(),
                        FakeTransportOutcome::Payload(json!({
                            "manager": descriptor_with_route(
                                "loopback-mcp-manager",
                                "ready",
                                "loopback-mcp-observability",
                            ),
                            "server": descriptor_with_profile(
                                "loopback-mcp-observability",
                                "ready",
                                "observability-v1",
                            ),
                            "lifecycle_events": [],
                        })),
                    ),
                    (
                        "mcp.call_tool".to_string(),
                        FakeTransportOutcome::Payload(bridge_response_with_status(false, "denied")),
                    ),
                ]))),
            ),
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        assert_eq!(snapshot["overall_ok"], false);
        assert_eq!(snapshot["blocking_check_count"], 1);
        assert_eq!(
            snapshot["blocking_issues"][0]["reason_code"],
            "mcp_blank_selection_response_not_ok"
        );
        assert_eq!(
            snapshot["blocking_issues"][0]["blocking_reason"],
            "blank selection response was not ok"
        );
        assert_eq!(snapshot["blocking_issues"][0]["error"], Value::Null);
        assert_eq!(snapshot["blocking_issues"][0]["response_excerpt"], "denied");
        assert_eq!(snapshot["services"][0]["service_ok"], false);
        assert_eq!(
            snapshot["services"][0]["mcp_default_route_gate"]["contract_ok"],
            false
        );
    }

    #[tokio::test]
    async fn bridge_cutover_smoke_route_surfaces_mcp_blank_selection_invocation_failures() {
        let app = build_router(
            test_state().with_bridge_probe_transport(
                BridgeServerKind::Mcp,
                Arc::new(FakeTransport::new(HashMap::from([
                    (
                        "mcp.list_servers".to_string(),
                        FakeTransportOutcome::Payload(
                            serde_json::to_value(McpManagerListServersResponse {
                                manager: descriptor_with_route(
                                    "loopback-mcp-manager",
                                    "ready",
                                    "loopback-mcp-observability",
                                ),
                                servers: vec![descriptor_with_profile(
                                    "loopback-mcp-observability",
                                    "ready",
                                    "observability-v1",
                                )],
                                selection_targets: vec!["loopback-mcp-observability".to_string()],
                                default_route_status: "ready".to_string(),
                                default_route_target: "loopback-mcp-observability".to_string(),
                            })
                            .expect("mcp manager list should serialize"),
                        ),
                    ),
                    (
                        "mcp.describe_server".to_string(),
                        FakeTransportOutcome::Payload(json!({
                            "manager": descriptor_with_route(
                                "loopback-mcp-manager",
                                "ready",
                                "loopback-mcp-observability",
                            ),
                            "server": descriptor_with_profile(
                                "loopback-mcp-observability",
                                "ready",
                                "observability-v1",
                            ),
                            "lifecycle_events": [],
                        })),
                    ),
                    (
                        "mcp.call_tool".to_string(),
                        FakeTransportOutcome::RemoteBusiness {
                            code: -32015,
                            message: "default route unavailable".to_string(),
                            data: None,
                        },
                    ),
                ]))),
            ),
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        assert_eq!(snapshot["overall_ok"], false);
        assert_eq!(
            snapshot["blocking_issues"][0]["reason_code"],
            "mcp_blank_selection_invocation_failed"
        );
        assert_eq!(
            snapshot["blocking_issues"][0]["blocking_reason"],
            "blank selection invocation failed"
        );
        assert_eq!(
            snapshot["blocking_issues"][0]["error"]["layer"],
            "RemoteBusiness"
        );
        assert_eq!(
            snapshot["blocking_issues"][0]["response_excerpt"],
            Value::Null
        );
        assert_eq!(snapshot["services"][0]["service_ok"], false);
        assert_eq!(
            snapshot["services"][0]["mcp_default_route_gate"]["contract_ok"],
            false
        );
    }

    #[tokio::test]
    async fn bridge_cutover_smoke_route_surfaces_mcp_metadata_drift_reason_codes() {
        let app = build_router(
            test_state().with_bridge_probe_transport(
                BridgeServerKind::Mcp,
                Arc::new(FakeTransport::new(HashMap::from([
                    (
                        "mcp.list_servers".to_string(),
                        FakeTransportOutcome::Payload(
                            serde_json::to_value(McpManagerListServersResponse {
                                manager: descriptor_with_route(
                                    "loopback-mcp-manager",
                                    "ready",
                                    "loopback-mcp-observability",
                                ),
                                servers: vec![descriptor_with_profile(
                                    "loopback-mcp-observability",
                                    "ready",
                                    "observability-v1",
                                )],
                                selection_targets: vec!["loopback-mcp-observability".to_string()],
                                default_route_status: "ready".to_string(),
                                default_route_target: "loopback-mcp-observability".to_string(),
                            })
                            .expect("mcp manager list should serialize"),
                        ),
                    ),
                    (
                        "mcp.describe_server".to_string(),
                        FakeTransportOutcome::Payload(json!({
                            "manager": descriptor_with_route(
                                "loopback-mcp-manager",
                                "ready",
                                "loopback-mcp-observability",
                            ),
                            "server": descriptor_with_profile(
                                "loopback-mcp-observability",
                                "ready",
                                "observability-v1",
                            ),
                            "lifecycle_events": [],
                        })),
                    ),
                    (
                        "mcp.call_tool".to_string(),
                        FakeTransportOutcome::Payload(structured_bridge_response(json!({
                            "server_name": "loopback-mcp-observability",
                            "default_route_status": "ready",
                            "default_route_target": "loopback-mcp-inspection",
                            "tool_name": "echo.describe",
                        }))),
                    ),
                ]))),
            ),
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        assert_eq!(snapshot["overall_ok"], false);
        assert_eq!(
            snapshot["blocking_issues"][0]["reason_code"],
            "mcp_default_route_metadata_drift"
        );
        assert_eq!(
            snapshot["blocking_issues"][0]["blocking_reason"],
            "blank selection payload drifted from manager metadata"
        );
        assert_eq!(snapshot["blocking_issues"][0]["error"], Value::Null);
        assert_eq!(
            snapshot["services"][0]["mcp_default_route_gate"]["resolved_server"],
            "loopback-mcp-observability"
        );
        assert_eq!(
            snapshot["services"][0]["mcp_default_route_gate"]["contract_ok"],
            false
        );
    }

    #[tokio::test]
    async fn bridge_cutover_smoke_route_surfaces_mcp_resolved_server_mismatch_reason_codes() {
        let app = build_router(
            test_state().with_bridge_probe_transport(
                BridgeServerKind::Mcp,
                Arc::new(FakeTransport::new(HashMap::from([
                    (
                        "mcp.list_servers".to_string(),
                        FakeTransportOutcome::Payload(
                            serde_json::to_value(McpManagerListServersResponse {
                                manager: descriptor_with_route(
                                    "loopback-mcp-manager",
                                    "ready",
                                    "loopback-mcp-observability",
                                ),
                                servers: vec![
                                    descriptor_with_profile(
                                        "loopback-mcp-observability",
                                        "ready",
                                        "observability-v1",
                                    ),
                                    descriptor_with_profile(
                                        "loopback-mcp-inspection",
                                        "ready",
                                        "inspection-v1",
                                    ),
                                ],
                                selection_targets: vec![
                                    "loopback-mcp-observability".to_string(),
                                    "loopback-mcp-inspection".to_string(),
                                ],
                                default_route_status: "ready".to_string(),
                                default_route_target: "loopback-mcp-observability".to_string(),
                            })
                            .expect("mcp manager list should serialize"),
                        ),
                    ),
                    (
                        "mcp.describe_server".to_string(),
                        FakeTransportOutcome::Payload(json!({
                            "manager": descriptor_with_route(
                                "loopback-mcp-manager",
                                "ready",
                                "loopback-mcp-observability",
                            ),
                            "server": descriptor_with_profile(
                                "loopback-mcp-observability",
                                "ready",
                                "observability-v1",
                            ),
                            "lifecycle_events": [],
                        })),
                    ),
                    (
                        "mcp.call_tool".to_string(),
                        FakeTransportOutcome::Payload(structured_bridge_response(json!({
                            "server_name": "loopback-mcp-inspection",
                            "default_route_status": "ready",
                            "default_route_target": "loopback-mcp-observability",
                            "tool_name": "echo.describe",
                        }))),
                    ),
                ]))),
            ),
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        assert_eq!(snapshot["overall_ok"], false);
        assert_eq!(
            snapshot["blocking_issues"][0]["reason_code"],
            "mcp_default_route_resolved_server_mismatch"
        );
        assert_eq!(
            snapshot["blocking_issues"][0]["blocking_reason"],
            "blank selection resolved to the wrong MCP server"
        );
        assert_eq!(snapshot["blocking_issues"][0]["error"], Value::Null);
        assert_eq!(
            snapshot["services"][0]["mcp_default_route_gate"]["route_target"],
            "loopback-mcp-observability"
        );
        assert_eq!(
            snapshot["services"][0]["mcp_default_route_gate"]["resolved_server"],
            "loopback-mcp-inspection"
        );
        assert_eq!(
            snapshot["services"][0]["mcp_default_route_gate"]["contract_ok"],
            false
        );
    }

    #[tokio::test]
    async fn bridge_routes_do_not_touch_execution_state() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(
            state
                .clone()
                .with_bridge_probe_transport(
                    BridgeServerKind::Model,
                    Arc::new(FakeTransport::new(HashMap::from([
                        (
                            "model.invoke".to_string(),
                            FakeTransportOutcome::Payload(bridge_response(
                                "loopback-model::bridge preflight ping",
                            )),
                        ),
                        (
                            LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
                            FakeTransportOutcome::Payload(catalog(
                                BridgeServerKind::Model,
                                LOOPBACK_MODEL_PROVIDER,
                            )),
                        ),
                    ]))),
                )
                .with_bridge_probe_transport(
                    BridgeServerKind::Mcp,
                    Arc::new(FakeTransport::new(HashMap::from([
                        (
                            "mcp.list_servers".to_string(),
                            FakeTransportOutcome::Payload(
                                serde_json::to_value(McpManagerListServersResponse {
                                    manager: descriptor("loopback-mcp-manager"),
                                    servers: vec![descriptor(LOOPBACK_MCP_SERVER_NAME)],
                                    selection_targets: vec![LOOPBACK_MCP_SERVER_NAME.to_string()],
                                    default_route_status: "available".to_string(),
                                    default_route_target: LOOPBACK_MCP_SERVER_NAME.to_string(),
                                })
                                .expect("mcp manager list should serialize"),
                            ),
                        ),
                        (
                            "mcp.call_tool".to_string(),
                            FakeTransportOutcome::Payload(bridge_response("echo.inspect::ok")),
                        ),
                    ]))),
                ),
        );

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
                .execution_pipeline()
                .expect("execution pipeline should exist")
                .memory_store
                .extraction_results_for_session(&SessionId::new("bridge-route-guard"))
                .is_empty()
        );
    }

    #[derive(Clone)]
    struct MockBridgeSnapshotProvider {
        snapshot: BridgeServicesSnapshotDto,
    }

    impl BridgeSnapshotProvider for MockBridgeSnapshotProvider {
        fn services_snapshot(&self) -> BridgeServicesSnapshotDto {
            self.snapshot.clone()
        }
    }

    #[tokio::test]
    async fn bridge_services_route_preserves_partial_failures_from_snapshot_provider() {
        let app = build_router(test_state().with_bridge_snapshot_provider(Arc::new(
            MockBridgeSnapshotProvider {
                snapshot: BridgeServicesSnapshotDto {
                    services: vec![BridgeServiceSnapshotDto {
                        server_kind: BridgeServerKind::Model,
                        handshake: Some(BridgeServerHandshake {
                            protocol_version: "local-bridge-v1".to_string(),
                            server_kind: BridgeServerKind::Model,
                            health_method: LOCAL_BRIDGE_HEALTH_METHOD.to_string(),
                            supported_methods: vec!["host.call".to_string()],
                        }),
                        handshake_error: None,
                        health: None,
                        health_error: Some(BridgeProbeErrorDto {
                            layer: Some(BridgeErrorLayer::RemoteBusiness),
                            code: None,
                            message: "桥接调用失败[RemoteBusiness]: probe degraded".to_string(),
                        }),
                        service_catalog: Some(BridgeServerServiceCatalog {
                            protocol_version: "local-bridge-v1".to_string(),
                            server_kind: BridgeServerKind::Model,
                            services: vec![],
                        }),
                        service_catalog_error: None,
                    }],
                },
            },
        )));

        let snapshot = get_json(app, "/bridges/services").await;
        assert_eq!(snapshot["services"][0]["server_kind"], "model");
        assert!(snapshot["services"][0]["handshake"].is_object());
        assert!(snapshot["services"][0]["health"].is_null());
        assert_eq!(
            snapshot["services"][0]["health_error"]["layer"],
            "RemoteBusiness"
        );
        assert!(snapshot["services"][0]["service_catalog"].is_object());
    }

    #[tokio::test]
    async fn task_interrupt_route_requires_structured_task_id_and_cancels_target_task() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());

        let (submit_status, submit_body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "session-route-loopback",
                "text": "interrupt target task",
                "skillName": "code",
                "images": [],
            }),
        )
        .await;
        assert_eq!(
            submit_status,
            StatusCode::OK,
            "unexpected response body: {submit_body:?}"
        );

        let action_task_id = submit_body["actionTaskId"]
            .as_str()
            .expect("action_task_id should serialize as string");
        let task_store = state.task_store().expect("task store should be configured");
        task_store
            .update_status(&TaskId::new(action_task_id), TaskStatus::Running)
            .expect("action task should become running for interrupt test");
        let action_task = task_store
            .get_task(&TaskId::new(action_task_id))
            .expect("action task should remain queryable");
        task_store
            .update_status(&action_task.root_task_id, TaskStatus::Running)
            .expect("root task should become running for interrupt test");
        let (interrupt_status, interrupt_body) = post_json(
            app,
            "/api/task/interrupt",
            json!({
                "sessionId": "session-route-loopback",
                "taskId": action_task_id,
            }),
        )
        .await;

        assert_eq!(
            interrupt_status,
            StatusCode::OK,
            "unexpected response body: {interrupt_body:?}"
        );
        assert_eq!(interrupt_body["interrupted"], true);

        let interrupted_task = task_store
            .get_task(&TaskId::new(action_task_id))
            .expect("interrupted task should remain queryable");
        assert_eq!(interrupted_task.status, TaskStatus::Blocked);
        let root_task = task_store
            .get_task(&interrupted_task.root_task_id)
            .expect("root task should remain queryable");
        assert_eq!(root_task.status, TaskStatus::Blocked);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_action_interrupt_discards_late_completion_for_blocked_session_task() {
        let state = build_execution_state(
            WorkerRuntime::new_compare,
            Arc::new(DelayedModelBridgeClient {
                delay: Duration::from_millis(200),
                payload: "loopback-model::被中断的晚到结果".to_string(),
            }),
        );
        let app = build_router(state.clone());

        let submit_app = app.clone();
        let submit_handle = tokio::spawn(async move {
            post_json(
                submit_app,
                "/api/session/turn",
                json!({
                    "sessionId": "session-route-loopback",
                    "text": "interrupt delayed completion",
                    "skillName": "code",
                    "images": [],
                }),
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(40)).await;

        let chain = state
            .session_store
            .active_execution_chain(&SessionId::new("session-route-loopback"))
            .expect("active execution chain should exist while session action is running");
        let action_task_id = chain
            .branches
            .iter()
            .find(|branch| branch.is_primary)
            .map(|branch| branch.task_id.clone())
            .or_else(|| chain.active_branch_task_ids.first().cloned())
            .expect("primary action branch should exist");

        let (interrupt_status, interrupt_body) = post_json(
            app,
            "/api/task/interrupt",
            json!({
                "sessionId": "session-route-loopback",
                "taskId": action_task_id.to_string(),
            }),
        )
        .await;
        assert_eq!(
            interrupt_status,
            StatusCode::OK,
            "unexpected response body: {interrupt_body:?}"
        );
        assert_eq!(interrupt_body["interrupted"], true);

        let (submit_status, submit_body) = submit_handle
            .await
            .expect("session action task should join successfully");
        assert_eq!(
            submit_status,
            StatusCode::OK,
            "unexpected response body: {submit_body:?}"
        );

        let task_store = state.task_store().expect("task store should be configured");
        let interrupted_task = task_store
            .get_task(&action_task_id)
            .expect("interrupted task should remain queryable");
        assert_eq!(interrupted_task.status, TaskStatus::Blocked);
        assert!(
            interrupted_task.output_refs.is_empty(),
            "late completion must not write assistant output refs after interrupt"
        );
        let root_task = task_store
            .get_task(&interrupted_task.root_task_id)
            .expect("root task should remain queryable");
        assert_eq!(root_task.status, TaskStatus::Blocked);

        let assistant_messages = state
            .session_store
            .timeline()
            .into_iter()
            .filter(|entry| {
                entry.session_id == SessionId::new("session-route-loopback")
                    && matches!(
                        entry.kind,
                        magi_session_store::TimelineEntryKind::AssistantMessage
                    )
            })
            .collect::<Vec<_>>();
        assert!(
            assistant_messages.is_empty(),
            "interrupted session action must not append assistant message from stale completion"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn regular_session_turn_interrupt_cancels_turn_and_discards_late_completion() {
        let state = build_execution_state(
            WorkerRuntime::new_compare,
            Arc::new(DelayedModelBridgeClient {
                delay: Duration::from_millis(200),
                payload: "loopback-model::普通会话晚到回复".to_string(),
            }),
        );
        let app = build_router(state.clone());
        let session_id = SessionId::new("session-route-loopback");

        let submit_app = app.clone();
        let submit_handle = tokio::spawn(async move {
            post_json(
                submit_app,
                "/api/session/turn",
                json!({
                    "sessionId": "session-route-loopback",
                    "text": "ordinary interrupt delayed completion",
                    "skillName": "",
                    "images": [],
                }),
            )
            .await
        });

        wait_for_condition(
            Duration::from_secs(1),
            Duration::from_millis(20),
            || {
                state
                    .session_store
                    .runtime_sidecar(&session_id)
                    .and_then(|sidecar| sidecar.current_turn)
                    .is_some_and(|turn| turn.status == "running")
            },
            "普通 session turn 应先进入 running 状态，才能验证中断",
        )
        .await;

        let (interrupt_status, interrupt_body) = post_json(
            app,
            "/api/session/interrupt",
            json!({
                "sessionId": "session-route-loopback",
            }),
        )
        .await;
        assert_eq!(
            interrupt_status,
            StatusCode::OK,
            "unexpected response body: {interrupt_body:?}"
        );
        assert_eq!(interrupt_body["interrupted"], true);

        let (submit_status, submit_body) = submit_handle
            .await
            .expect("regular session turn submit should join successfully");
        assert_eq!(
            submit_status,
            StatusCode::OK,
            "unexpected response body: {submit_body:?}"
        );
        tokio::time::sleep(Duration::from_millis(260)).await;

        let turn = state
            .session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("interrupted regular turn should remain inspectable");
        assert_eq!(turn.status, "cancelled");
        assert!(turn.completed_at.is_some());
        assert!(
            !turn.items.iter().any(|item| item.kind == "assistant_final"),
            "late completion must not append assistant_final after session interrupt"
        );

        let assistant_messages = state
            .session_store
            .timeline()
            .into_iter()
            .filter(|entry| {
                entry.session_id == session_id
                    && matches!(
                        entry.kind,
                        magi_session_store::TimelineEntryKind::AssistantMessage
                    )
            })
            .collect::<Vec<_>>();
        assert!(
            assistant_messages.is_empty(),
            "interrupted regular turn must not append assistant message from stale completion"
        );

        let events = state.event_bus.snapshot().recent_events;
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "session.turn.interrupted"),
            "session interrupt should publish a terminal interrupted event"
        );
        assert!(
            !events
                .iter()
                .any(|event| event.event_type == "session.turn.completed"),
            "interrupted regular turn must not later publish completed"
        );
    }

    #[tokio::test]
    async fn session_interrupt_finalizes_completed_root_before_cancelling_turn() {
        let state = test_state_with_execution_pipeline();
        let app = build_router(state.clone());
        let session_id = SessionId::new("session-interrupt-completed-root");
        let mission_id = MissionId::new("mission-interrupt-completed-root");
        let root_task_id = TaskId::new("task-interrupt-completed-root");
        let now = UtcMillis::now();

        state
            .session_store
            .create_session(session_id.clone(), "interrupt completed root")
            .expect("session should be creatable");
        let (_mission_id, orchestrator_thread_id) = state
            .session_store
            .ensure_session_mission(&session_id, now, || mission_id.clone());
        let mut phase_item = session_turn_item(
            "assistant_phase",
            "pending",
            Some("任务状态".to_string()),
            Some("任务运行中".to_string()),
            Some("turn-item-interrupt-completed-root-phase".to_string()),
            orchestrator_thread_id.clone(),
        );
        phase_item.task_id = Some(root_task_id.clone());
        state
            .session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-interrupt-completed-root".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![root_task_id.clone()],
                    active_worker_bindings: Vec::new(),
                    branches: Vec::new(),
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-interrupt-completed-root".to_string(),
                        trimmed_text: Some("执行任务".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-interrupt-completed-root".to_string(),
                        turn_seq: 1,
                        accepted_at: now,
                        completed_at: None,
                        status: "accepted".to_string(),
                        user_message: Some("执行任务".to_string()),
                        items: vec![phase_item],
                        worker_lanes: Vec::new(),
                    }),
                },
            )
            .expect("active chain should be stored");
        state
            .task_store()
            .expect("task store should exist")
            .insert_task(Task {
                task_id: root_task_id.clone(),
                mission_id,
                root_task_id: root_task_id.clone(),
                parent_task_id: None,
                kind: TaskKind::Objective,
                title: "已完成 root".to_string(),
                goal: "验证停止不会覆盖已完成 root".to_string(),
                status: TaskStatus::Completed,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: None,
                executor_binding: None,
                context_refs: Vec::new(),
                knowledge_refs: Vec::new(),
                workspace_scope: None,
                write_scope: None,
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                repair_count: 0,
                decision_payload: None,
                variant: magi_core::TaskVariant::default(),
                created_at: now,
                updated_at: now,
            });

        let (status, body) = post_json(
            app,
            "/api/session/interrupt",
            json!({
                "sessionId": session_id.to_string(),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected response body: {body:?}");
        assert_eq!(
            body["interrupted"], false,
            "root 已完成时停止请求不应再取消 current_turn"
        );

        let turn = state
            .session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("turn should remain inspectable");
        assert_eq!(turn.status, "completed");
        assert!(turn.completed_at.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deep_session_action_interrupt_discards_late_completion_for_blocked_session_task() {
        let state = build_execution_state(
            WorkerRuntime::new_compare,
            Arc::new(DelayedModelBridgeClient {
                delay: Duration::from_millis(200),
                payload: "loopback-model::deep interrupted late result".to_string(),
            }),
        );
        let app = build_router(state.clone());
        let session_id = SessionId::new("session-route-loopback-deep");
        state
            .session_store
            .create_session(session_id.clone(), "deep interrupt session")
            .expect("deep interrupt session should be creatable");
        let accepted_at = UtcMillis::now();
        let mut accepted = submit_dispatch_submission(
            &state,
            crate::task_execution::DispatchSubmissionRequest {
                accepted_at,
                session_id: session_id.clone(),
                workspace_id: None,
                entry_id: format!("timeline-{session_id}-{}", accepted_at.0),
                timeline_message: "deep interrupt delayed completion".to_string(),
                created_session: false,
                mission_title: "deep interrupt delayed completion".to_string(),
                task_title: "执行: deep interrupt delayed completion".to_string(),
                trimmed_text: Some("deep interrupt delayed completion".to_string()),
                execution_goal: Some("deep interrupt delayed completion".to_string()),
                skill_name: None,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
            },
        )
        .expect("deep dispatch submission should succeed");
        let action_task_id = accepted.action_task_id.clone();

        let drive_state = state.clone();
        let drive_handle = tokio::spawn(async move {
            drive_dispatch_submission(&drive_state, &mut accepted)
                .expect("deep dispatch should finish without route-level error");
            append_dispatch_assistant_message(&drive_state, &accepted);
        });

        tokio::time::sleep(Duration::from_millis(40)).await;

        let (interrupt_status, interrupt_body) = post_json(
            app,
            "/api/task/interrupt",
            json!({
                "sessionId": session_id.to_string(),
                "taskId": action_task_id.to_string(),
            }),
        )
        .await;
        assert_eq!(
            interrupt_status,
            StatusCode::OK,
            "unexpected response body: {interrupt_body:?}"
        );
        assert_eq!(interrupt_body["interrupted"], true);

        drive_handle
            .await
            .expect("deep dispatch task should join successfully");

        let task_store = state.task_store().expect("task store should be configured");
        let interrupted_task = task_store
            .get_task(&action_task_id)
            .expect("interrupted task should remain queryable");
        assert_eq!(interrupted_task.status, TaskStatus::Blocked);
        assert!(
            interrupted_task.output_refs.is_empty(),
            "interrupted deep session action must not write assistant output refs after interrupt"
        );
        let root_task = task_store
            .get_task(&interrupted_task.root_task_id)
            .expect("root task should remain queryable");
        assert_eq!(root_task.status, TaskStatus::Blocked);

        let assistant_messages = state
            .session_store
            .timeline()
            .into_iter()
            .filter(|entry| {
                entry.session_id == SessionId::new("session-route-loopback-deep")
                    && matches!(
                        entry.kind,
                        magi_session_store::TimelineEntryKind::AssistantMessage
                    )
            })
            .collect::<Vec<_>>();
        assert!(
            assistant_messages.is_empty(),
            "interrupted deep session action must not append assistant message from stale completion"
        );
    }

    // ─── /api/tasks/* task graph routes integration tests ───

    fn test_state_with_task_store() -> ApiState {
        use magi_orchestrator::task_store::TaskStore;
        let state = test_state();
        let task_store = Arc::new(TaskStore::new());
        state.with_task_store(task_store)
    }

    fn insert_test_task(
        state: &ApiState,
        task_id: &str,
        mission_id: &str,
        root_task_id: &str,
        parent_task_id: Option<&str>,
        kind: TaskKind,
        status: TaskStatus,
        decision_payload: Option<DecisionTaskPayload>,
    ) {
        let now = UtcMillis::now();
        state
            .task_store()
            .expect("task store should be configured")
            .insert_task(Task {
                task_id: TaskId::new(task_id),
                mission_id: MissionId::new(mission_id),
                root_task_id: TaskId::new(root_task_id),
                parent_task_id: parent_task_id.map(TaskId::new),
                kind,
                title: format!("Task {task_id}"),
                goal: format!("Goal for {task_id}"),
                status,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: None,
                executor_binding: None,
                context_refs: Vec::new(),
                knowledge_refs: Vec::new(),
                workspace_scope: None,
                write_scope: None,
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                repair_count: 0,
                decision_payload,
                variant: magi_core::TaskVariant::default(),
                created_at: now,
                updated_at: now,
            });
    }

    #[tokio::test]
    async fn task_graph_get_task_reads_existing_session_task() {
        let state = test_state_with_task_store();
        insert_test_task(
            &state,
            "task-1",
            "mission-1",
            "task-1",
            None,
            TaskKind::Objective,
            TaskStatus::Draft,
            None,
        );
        bind_test_session_mission(&state, "session-task-graph", "mission-1", "task-1");
        let app = build_router(state);

        let retrieved = get_json(app, "/api/tasks/task-1?sessionId=session-task-graph").await;
        assert_eq!(retrieved["task_id"], "task-1");
        assert_eq!(retrieved["title"], "Task task-1");
        assert_eq!(retrieved["mission_id"], "mission-1");
    }

    #[tokio::test]
    async fn task_graph_get_projection_reads_existing_task_tree() {
        let state = test_state_with_task_store();

        for (task_id, parent, kind, status) in [
            ("obj-1", None, TaskKind::Objective, TaskStatus::Running),
            (
                "phase-1",
                Some("obj-1"),
                TaskKind::Phase,
                TaskStatus::Running,
            ),
            (
                "act-1",
                Some("phase-1"),
                TaskKind::Action,
                TaskStatus::Completed,
            ),
            (
                "act-2",
                Some("phase-1"),
                TaskKind::Action,
                TaskStatus::Running,
            ),
        ] {
            insert_test_task(
                &state,
                task_id,
                "mission-proj",
                "obj-1",
                parent,
                kind,
                status,
                None,
            );
        }

        bind_test_session_mission(&state, "session-task-graph", "mission-proj", "obj-1");
        let app = build_router(state);

        let projection = get_json(app, "/api/tasks/graph/obj-1?sessionId=session-task-graph").await;
        assert_eq!(projection["root_task"]["task_id"], "obj-1");
        let task_ids: Vec<&str> = projection["tasks"]
            .as_array()
            .expect("projection tasks must be an array")
            .iter()
            .map(|task| task["task_id"].as_str().expect("task_id must be a string"))
            .collect();
        assert_eq!(task_ids, vec!["obj-1", "phase-1", "act-1", "act-2"]);
        assert_eq!(projection["current_phase"], "Task phase-1");
        assert_eq!(projection["progress_summary"]["total_tasks"], 4);
        assert_eq!(projection["progress_summary"]["completed_tasks"], 1);
        assert_eq!(projection["progress_summary"]["running_tasks"], 3);
        assert_eq!(projection["aggregate_status"], "Running");
    }

    #[tokio::test]
    async fn task_graph_projection_uses_runner_manager_status_as_authority() {
        let task_store = Arc::new(TaskStore::new());
        let runner_manager = RunnerManager::new(Arc::clone(&task_store), Vec::new());
        runner_manager.set_status_for_test("obj-live-runner", "running");
        let state = test_state()
            .with_task_store(task_store)
            .with_runner_manager(runner_manager);
        insert_test_task(
            &state,
            "obj-live-runner",
            "mission-live-runner",
            "obj-live-runner",
            None,
            TaskKind::Objective,
            TaskStatus::Completed,
            None,
        );
        bind_test_session_mission(
            &state,
            "session-live-runner",
            "mission-live-runner",
            "obj-live-runner",
        );
        let app = build_router(state);

        let projection = get_json(
            app,
            "/api/tasks/graph/obj-live-runner?sessionId=session-live-runner",
        )
        .await;
        assert_eq!(projection["aggregate_status"], "Completed");
        assert_eq!(projection["runner_status"], "running");
    }

    #[test]
    fn runner_manager_pause_tree_updates_live_status_without_restarting_runner() {
        let task_store = Arc::new(TaskStore::new());
        let runner_manager = RunnerManager::new(Arc::clone(&task_store), Vec::new());
        runner_manager.set_status_for_test("obj-pause-live", "running");
        let state = test_state().with_task_store(task_store);
        insert_test_task(
            &state,
            "obj-pause-live",
            "mission-pause-live",
            "obj-pause-live",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
            None,
        );

        runner_manager
            .pause_tree("obj-pause-live")
            .expect("running task graph should pause");

        let status = runner_manager
            .status("obj-pause-live")
            .expect("runner status should exist");
        assert_eq!(status.status, "blocked");
        assert!(
            matches!(
                runner_manager.start("obj-pause-live", None),
                Err(RunnerStartError::AlreadyRunning)
            ),
            "active paused runner must not be replaced before its loop exits"
        );
    }

    #[test]
    fn runner_manager_stop_updates_live_status_immediately() {
        let task_store = Arc::new(TaskStore::new());
        let runner_manager = RunnerManager::new(Arc::clone(&task_store), Vec::new());
        runner_manager.set_status_for_test("obj-stop-live", "running");
        let state = test_state().with_task_store(task_store);
        insert_test_task(
            &state,
            "obj-stop-live",
            "mission-stop-live",
            "obj-stop-live",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
            None,
        );

        runner_manager
            .stop("obj-stop-live")
            .expect("running task graph should stop");

        let status = runner_manager
            .status("obj-stop-live")
            .expect("runner status should exist");
        assert_eq!(status.status, "stopped");
        assert!(
            matches!(
                runner_manager.start("obj-stop-live", None),
                Err(RunnerStartError::AlreadyRunning)
            ),
            "stopping runner must not be replaced before its loop exits"
        );
    }

    #[tokio::test]
    async fn task_graph_resolves_decision_through_product_action() {
        let state = test_state_with_task_store();
        insert_test_task(
            &state,
            "task-decision-root",
            "mission-status",
            "task-decision-root",
            None,
            TaskKind::Objective,
            TaskStatus::Blocked,
            None,
        );
        insert_test_task(
            &state,
            "task-decision-1",
            "mission-status",
            "task-decision-root",
            Some("task-decision-root"),
            TaskKind::Decision,
            TaskStatus::AwaitingApproval,
            Some(DecisionTaskPayload {
                decision_context: "任务失败后需要选择处理方式".to_string(),
                blocked_reason: "等待用户选择失败任务处理方式".to_string(),
                target_task_id: Some(TaskId::new("task-decision-root")),
                options: vec![
                    DecisionOption {
                        option_id: "retry".to_string(),
                        label: "重试".to_string(),
                        description: "重新执行失败任务".to_string(),
                    },
                    DecisionOption {
                        option_id: "abort".to_string(),
                        label: "中止".to_string(),
                        description: "中止当前任务链".to_string(),
                    },
                ],
                risk_notes: Vec::new(),
                recommended_option: Some("retry".to_string()),
                required_user_input: true,
                decision_evidence: None,
            }),
        );
        bind_test_session_mission(
            &state,
            "session-task-status",
            "mission-status",
            "task-decision-root",
        );

        let app = build_router(state.clone());
        let (status, body) = post_json(
            app,
            "/api/tasks/task-decision-1/decision?sessionId=session-task-status",
            json!({ "chosenOption": "retry" }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected decision response: {body:?}"
        );
        assert_eq!(body["resolved"], true);
        assert_eq!(body["chosenOption"], "retry");

        let store = state.task_store().expect("task store should be configured");
        let decision = store
            .get_task(&TaskId::new("task-decision-1"))
            .expect("decision task should remain queryable");
        assert_eq!(decision.status, TaskStatus::Completed);
        assert_eq!(decision.output_refs, vec!["decision_chosen:retry"]);
        let root = store
            .get_task(&TaskId::new("task-decision-root"))
            .expect("root task should remain queryable");
        // Decision resolve 后 release_open_branch 把 root 从 Blocked 释放为 Ready
        assert_eq!(root.status, TaskStatus::Ready);
    }

    #[tokio::test]
    async fn task_graph_returns_not_found_for_missing_task() {
        let state = test_state_with_task_store();
        let app = build_router(state.clone());
        bind_test_session_mission(
            &state,
            "session-task-missing",
            "mission-missing",
            "nonexistent-task",
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/tasks/nonexistent-task?sessionId=session-task-missing")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn task_graph_returns_error_when_task_store_not_configured() {
        let app = build_router(test_state());

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/tasks/some-task")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn generated_session_ids_are_unique() {
        let mut seen = HashSet::new();
        for _ in 0..64 {
            let session_id = new_session_id();
            assert!(
                seen.insert(session_id.to_string()),
                "session id should remain unique"
            );
        }
    }
}
