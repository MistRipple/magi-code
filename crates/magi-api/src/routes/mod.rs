mod changes_files_tunnel;
mod knowledge;
mod mcp_skills_repos;
mod messages;
mod sessions;
pub(crate) mod settings;
mod tasks_graph;
mod tasks_interaction;
mod tasks_runner;
mod workspaces;

use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use magi_core::{EventId, SessionId, UtcMillis};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_session_store::TimelineEntryKind;
use serde::Deserialize;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

static ACCEPTED_AT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn monotonic_accepted_at() -> UtcMillis {
    let now = UtcMillis::now().0;
    let prev = ACCEPTED_AT_COUNTER.fetch_max(now, Ordering::SeqCst);
    let ts = if now <= prev { prev + 1 } else { now };
    ACCEPTED_AT_COUNTER.store(ts, Ordering::SeqCst);
    UtcMillis(ts)
}

use crate::{
    dto::{
        AuditUsageLedgerDto, BootstrapDto, BridgeCutoverSmokeSnapshotDto,
        BridgePreflightSnapshotDto, BridgeServicesSnapshotDto, HealthDto, RuntimeReadModelDto,
        RecoveryResumeRequestDto, RecoveryResumeResponseDto, SessionActionRequestDto,
        SessionActionResponseDto, VersionHandshakeDto,
    },
    errors::ApiError,
    sse,
    state::ApiState,
    task_execution::{
        DispatchSubmissionAccepted, DispatchSubmissionRequest,
        submit_shadow_dispatch_submission, submit_shadow_recovery_resume_submission,
    },
};

pub(super) struct SessionActionAccepted {
    session_id: SessionId,
    entry_id: String,
    accepted_at: UtcMillis,
    created_session: bool,
    root_task_id: magi_core::TaskId,
    runner_started: bool,
}

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
        .merge(tasks_runner::routes())
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
        .route("/recovery/resume", post(resume_recovery))
        .route("/session/action", post(submit_session_action))
        .nest("/api", api_routes)
        .with_state(state)
}

async fn health(State(state): State<ApiState>) -> Json<HealthDto> {
    Json(state.health_dto())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapQuery {
    session_id: Option<String>,
    workspace_id: Option<String>,
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
    let workspace_id = query.requested_workspace_id();
    Ok(Json(
        state.bootstrap_dto_for_workspace_session(
            workspace_id.as_deref(),
            requested_session_id.as_ref(),
        ),
    ))
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

async fn bridge_cutover_smoke(State(state): State<ApiState>) -> Json<BridgeCutoverSmokeSnapshotDto> {
    Json(state.bridge_cutover_smoke_dto())
}

async fn version(State(state): State<ApiState>) -> Json<VersionHandshakeDto> {
    Json(state.version_handshake_dto())
}

async fn submit_session_action(
    State(state): State<ApiState>,
    Json(request): Json<SessionActionRequestDto>,
) -> Result<Json<SessionActionResponseDto>, ApiError> {
    let accepted = execute_session_action_submission(&state, &request)?;

    let event_id = EventId::new(format!("event-session-action-{}", accepted.accepted_at.0));
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.action.accepted",
        json!({
            "session_id": accepted.session_id.to_string(),
            "entry_id": accepted.entry_id,
            "text": request.trimmed_text(),
            "skill_name": request.skill_name.clone(),
            "image_count": request.images.len(),
            "deep_task": request.deep_task,
            "created_session": accepted.created_session,
            "root_task_id": accepted.root_task_id.to_string(),
            "runner_started": accepted.runner_started,
        }),
    )
    .with_context(EventContext {
        session_id: Some(accepted.session_id.clone()),
        ..EventContext::default()
    });
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("事件发布失败", err))?;

    Ok(Json(SessionActionResponseDto::new(
        accepted.session_id,
        accepted.entry_id,
        event_id,
        accepted.accepted_at,
        accepted.created_session,
        Some(accepted.root_task_id),
    )))
}

pub(super) fn execute_session_action_submission(
    state: &ApiState,
    request: &SessionActionRequestDto,
) -> Result<SessionActionAccepted, ApiError> {
    let trimmed_text = request.trimmed_text();
    let message = request.timeline_message(trimmed_text.as_deref());
    let mission_title = trimmed_text
        .clone()
        .unwrap_or_else(|| request.fallback_session_title(trimmed_text.as_deref()));

    execute_dispatch_submission(
        state,
        request.requested_session_id(),
        mission_title,
        message,
        trimmed_text,
        request.deep_task,
        request.skill_name.clone(),
        None,
    )
}

async fn resume_recovery(
    State(state): State<ApiState>,
    Json(request): Json<RecoveryResumeRequestDto>,
) -> Result<Json<RecoveryResumeResponseDto>, ApiError> {
    Ok(Json(execute_recovery_resume(&state, &request)?))
}

pub(super) fn execute_recovery_resume(
    state: &ApiState,
    request: &RecoveryResumeRequestDto,
) -> Result<RecoveryResumeResponseDto, ApiError> {
    let resumed_at = UtcMillis::now();
    let accepted = submit_shadow_recovery_resume_submission(state, request, resumed_at)?;
    let result = accepted.result;
    let memory_writeback_applied = accepted.memory_writeback_applied;
    let event_id = EventId::new(format!(
        "event-recovery-resume-{}-{}",
        result.recovery_input.recovery_id, resumed_at.0
    ));
    let event = EventEnvelope::domain(
        event_id.clone(),
        "recovery.resume.executed",
        json!({
            "recovery_id": result.recovery_input.recovery_id.clone(),
            "snapshot_id": result.recovery_input.snapshot_id.clone(),
            "session_id": result.recovery_input.ownership.session_id.clone(),
            "workspace_id": result.recovery_input.ownership.workspace_id.clone(),
            "mission_id": result.target.mission_id.clone(),
            "assignment_id": result.assignment_id.clone(),
            "task_id": result.target.task_id.clone(),
            "worker_id": result.target.requested_worker_id.clone(),
            "memory_writeback_applied": memory_writeback_applied,
        }),
    )
    .with_context(EventContext {
        session_id: result.recovery_input.ownership.session_id.clone(),
        workspace_id: result.recovery_input.ownership.workspace_id.clone(),
        mission_id: Some(result.target.mission_id.clone()),
        assignment_id: result.assignment_id.clone(),
        task_id: Some(result.target.task_id.clone()),
    });
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("事件发布失败", err))?;

    Ok(RecoveryResumeResponseDto::new(
        &result,
        event_id,
        resumed_at,
        memory_writeback_applied,
    ))
}

fn execute_dispatch_submission(
    state: &ApiState,
    requested_session_id: Option<SessionId>,
    mission_title: String,
    message: String,
    trimmed_text: Option<String>,
    deep_task: bool,
    skill_name: Option<String>,
    target_role: Option<String>,
) -> Result<SessionActionAccepted, ApiError> {
    let accepted_at = monotonic_accepted_at();
    let (session_id, created_session) =
        resolve_dispatch_session(state, requested_session_id, &mission_title, accepted_at)?;
    append_dispatch_user_message(state, &session_id, accepted_at, &message);

    let dispatch = DispatchSubmissionRequest {
        accepted_at,
        session_id: session_id.clone(),
        entry_id: format!("timeline-{}-{}", session_id, accepted_at.0),
        created_session,
        mission_title,
        task_title: message,
        trimmed_text,
        deep_task,
        skill_name,
        target_role,
    };
    let accepted = submit_shadow_dispatch_submission(state, dispatch)?;
    append_dispatch_assistant_message(state, &accepted);

    Ok(SessionActionAccepted {
        session_id: accepted.session_id,
        entry_id: accepted.entry_id,
        accepted_at: accepted.accepted_at,
        created_session: accepted.created_session,
        root_task_id: accepted.root_task_id,
        runner_started: accepted.runner_started,
    })
}

fn resolve_dispatch_session(
    state: &ApiState,
    requested_session_id: Option<SessionId>,
    mission_title: &str,
    accepted_at: UtcMillis,
) -> Result<(SessionId, bool), ApiError> {
    if let Some(session_id) = requested_session_id {
        return Ok((session_id, false));
    }
    if let Some(current_session) = state.session_store.current_session() {
        return Ok((current_session.session_id, false));
    }

    let session_id = SessionId::new(format!("session-{}", accepted_at.0));
    state
        .session_store
        .create_session(session_id.clone(), mission_title.to_string())
        .map_err(|err| ApiError::internal_assembly("创建会话失败", err))?;
    Ok((session_id, true))
}

fn append_dispatch_user_message(
    state: &ApiState,
    session_id: &SessionId,
    accepted_at: UtcMillis,
    message: &str,
) {
    state.session_store.append_timeline_entry(
        session_id.clone(),
        TimelineEntryKind::UserMessage,
        message.to_string(),
    );

    let _ = state.event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-message-user-{}", accepted_at.0)),
            "message.created",
            json!({
                "session_id": session_id.to_string(),
                "role": "user",
                "content": message,
            }),
        )
        .with_context(EventContext {
            session_id: Some(session_id.clone()),
            ..EventContext::default()
        }),
    );
}

fn append_dispatch_assistant_message(state: &ApiState, accepted: &DispatchSubmissionAccepted) {
    let Some(task_store) = state.task_store() else {
        return;
    };
    let Some(task) = task_store.get_task(&accepted.action_task_id) else {
        return;
    };
    let response_text = task.output_refs.join("\n");
    if response_text.is_empty() {
        return;
    }

    state.session_store.append_timeline_entry(
        accepted.session_id.clone(),
        TimelineEntryKind::AssistantMessage,
        response_text.clone(),
    );
    let _ = state.event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-message-assistant-{}", UtcMillis::now().0)),
            "message.created",
            json!({
                "session_id": accepted.session_id.to_string(),
                "role": "assistant",
                "content": response_text,
            }),
        )
        .with_context(EventContext {
            session_id: Some(accepted.session_id.clone()),
            ..EventContext::default()
        }),
    );
}

async fn stream_events(State(state): State<ApiState>) -> impl axum::response::IntoResponse {
    sse::events(state).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use crate::dto::{
        BridgeProbeErrorDto, BridgeServiceSnapshotDto, BridgeSnapshotProvider,
        BridgeServicesSnapshotDto,
    };
    use crate::task_execution::ShadowTaskDispatcher;
    use magi_bridge_client::{
        BridgeErrorLayer, BridgeResponse, BridgeServerHandshake, BridgeServerHealth,
        BridgeServerKind, BridgeServerServiceCatalog, BridgeTransport, BridgeTransportError,
        BridgeTransportRequest, BridgeTransportResponse, LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD,
        LOCAL_BRIDGE_HANDSHAKE_METHOD, LOCAL_BRIDGE_HEALTH_METHOD, ModelBridgeClient,
        ModelInvocationRequest,
        McpManagerListServersResponse, SHADOW_MCP_SERVER_NAME, SHADOW_MCP_TOOL_NAME,
        SHADOW_MODEL_PROVIDER,
    };
    use magi_core::{AbsolutePath, EventId, ExecutionOwnership, SessionId, TaskId, TaskStatus, WorkspaceId};
    use magi_context_runtime::{ContextBudget, ContextRuntime};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_knowledge_store::KnowledgeStore;
    use magi_memory_store::MemoryStore;
    use magi_orchestrator::{ExecutionContextConfig, OrchestratorService, task_store::TaskStore};
    use magi_session_store::SessionStore;
    use serde_json::Value;
    use std::{collections::HashMap, sync::Mutex};
    use magi_skill_runtime::SkillDispatchRuntime;
    use magi_tool_runtime::ToolRegistry;
    use magi_worker_runtime::WorkerRuntime;
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;
    use tower::util::ServiceExt;
    use crate::state::RunnerManager;
    use magi_orchestrator::task_runner::EventBasedResultReceiver;

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

    fn build_shadow_execution_state(
        worker_runtime_factory: impl FnOnce(Arc<InMemoryEventBus>) -> WorkerRuntime,
        model_bridge_client: Arc<dyn ModelBridgeClient>,
    ) -> ApiState {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let governance = Arc::new(GovernanceService::default());
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());

        let session_id = SessionId::new("session-route-shadow");
        session_store
            .create_session(session_id.clone(), "Route Shadow Session")
            .expect("shadow route session should be creatable");

        let workspace_id = WorkspaceId::new("workspace-route-shadow");
        workspace_store
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/Users/xie/code/magi-rust-rewrite"),
            )
            .expect("shadow route workspace should register");
        workspace_store
            .activate(&workspace_id)
            .expect("shadow route workspace should activate");
        session_store.bind_execution_ownership(
            session_id.clone(),
            ExecutionOwnership {
                session_id: Some(session_id.clone()),
                workspace_id: Some(workspace_id),
                execution_chain_ref: Some("shadow-route-chain".to_string()),
                ..ExecutionOwnership::default()
            },
        );

        let knowledge_store = KnowledgeStore::new();
        knowledge_store.ingest_code_index(magi_knowledge_store::CodeIndexIngestion {
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
                rationale: Some("allowed for session action shadow dispatch".to_string()),
                audit_event_id: Some("audit-route-context-1".to_string()),
            }),
        });

        let memory_store = MemoryStore::new();

        let orchestrator = OrchestratorService::new(Arc::clone(&event_bus));
        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        let skill_runtime = SkillDispatchRuntime::new(
            tool_registry.clone(),
            magi_bridge_client::BridgeDispatchRuntime::new(),
        );
        let worker_runtime = worker_runtime_factory(Arc::clone(&event_bus));
        let context_runtime = ContextRuntime::new(knowledge_store, memory_store.clone());
        let context_runtime_for_dispatcher = Arc::new(context_runtime.clone());
        let task_store = Arc::new(TaskStore::new());
        let execution_runtime = orchestrator
            .execution_runtime_with_recovery_support(
                worker_runtime,
                tool_registry,
                skill_runtime,
                Arc::clone(&session_store),
                Arc::clone(&workspace_store),
            )
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
                    project_key: Some("project-route-shadow".to_string()),
                },
            );

        let mut state = ApiState::new(
            "magi",
            event_bus.clone(),
            session_store,
            workspace_store,
            governance,
        )
        .with_shadow_execution_pipeline(orchestrator, execution_runtime, memory_store)
        .with_task_store(Arc::clone(&task_store));

        let state_for_task_workers = state.clone();
        let runner_result_receiver = Arc::new(EventBasedResultReceiver::new());
        let dispatcher = Arc::new(
            ShadowTaskDispatcher::new(
                event_bus,
                state
                    .shadow_execution_pipeline()
                    .expect("shadow execution pipeline should exist")
                    .clone(),
                state.session_store.clone(),
                state.shadow_task_execution_registry().clone(),
                runner_result_receiver.clone(),
            )
            .with_model_bridge_client(model_bridge_client)
            .with_context_runtime(context_runtime_for_dispatcher),
        );
        let runner_manager = RunnerManager::with_dispatcher_and_worker_catalog(
            task_store,
            Arc::new(move || state_for_task_workers.task_worker_catalog()),
            dispatcher,
            runner_result_receiver,
        );
        state = state.with_runner_manager(runner_manager);
        state
    }

    fn test_state_with_shadow_execution_pipeline() -> ApiState {
        build_shadow_execution_state(
            WorkerRuntime::new_compare,
            Arc::new(StaticModelBridgeClient),
        )
    }

    fn test_state_with_unhealthy_shadow_execution_pipeline() -> ApiState {
        build_shadow_execution_state(
            WorkerRuntime::new_compare,
            Arc::new(FailingModelBridgeClient),
        )
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
    async fn session_action_route_drives_shadow_dispatch_and_updates_runtime_read_model() {
        let state = test_state_with_shadow_execution_pipeline();
        let app = build_router(state.clone());

        let (status, first_body) = post_json(
            app.clone(),
            "/session/action",
            json!({
                "session_id": "session-route-shadow",
                "text": "Route parser refresh",
                "deep_task": true,
                "skill_name": "refactor",
                "images": [],
            }),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {first_body:?}"
        );
        assert_eq!(first_body["session_id"], "session-route-shadow");
        let first_accepted_at = first_body["accepted_at"]
            .as_u64()
            .expect("accepted_at should serialize as integer");
        let first_extraction_id = format!("extract-session-action-{first_accepted_at}");
        let first_read_model = state.runtime_read_model_dto();
        assert_eq!(first_read_model.details.execution_groups.len(), 1);
        let first_mission_id = format!("mission-session-action-{first_accepted_at}");
        let first_root_task_id = TaskId::new(
            first_body["root_task_id"]
                .as_str()
                .expect("root_task_id should serialize as string"),
        );
        let first_mission_entry = first_read_model
            .details
            .execution_groups
            .iter()
            .find(|entry| entry.mission_id == first_mission_id)
            .expect("first execution group entry should exist");
        assert_eq!(first_mission_entry.context_used_knowledge_count, 1);
        assert_eq!(first_mission_entry.context_used_memory_count, 0);
        assert!(first_mission_entry.context_memory_extraction_refs.is_empty());
        let task_store = state.task_store().expect("task store should be configured");
        let first_root_task = task_store
            .get_task(&first_root_task_id)
            .expect("root task should exist");
        assert_eq!(first_root_task.status, TaskStatus::Completed);
        let first_children = task_store.get_children(&first_root_task_id);
        assert_eq!(first_children.len(), 1);
        assert_eq!(first_children[0].status, TaskStatus::Completed);

        let first_verification = state
            .shadow_execution_pipeline()
            .expect("shadow execution pipeline should exist")
            .memory_store
            .verify_extraction_linkage(&first_extraction_id)
            .expect("first route extraction should be persisted");
        assert!(first_verification.is_consistent);

        let extraction_history = state
            .shadow_execution_pipeline()
            .expect("shadow execution pipeline should exist")
            .memory_store
            .extraction_results_for_session(&SessionId::new("session-route-shadow"));
        assert_eq!(extraction_history.len(), 1);
        assert_eq!(extraction_history[0].extraction_id, first_extraction_id);

        let (status, second_body) = post_json(
            app,
            "/session/action",
            json!({
                "session_id": "session-route-shadow",
                "text": "Route parser refresh followup",
                "deep_task": true,
                "skill_name": "refactor",
                "images": [],
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected response body: {second_body:?}"
        );
        let second_accepted_at = second_body["accepted_at"]
            .as_u64()
            .expect("accepted_at should serialize as integer");
        let second_mission_id = format!("mission-session-action-{second_accepted_at}");
        let second_root_task_id = TaskId::new(
            second_body["root_task_id"]
                .as_str()
                .expect("root_task_id should serialize as string"),
        );
        let read_model = state.runtime_read_model_dto();
        assert_eq!(read_model.details.execution_groups.len(), 2);
        let mission_entry = read_model
            .details
            .execution_groups
            .iter()
            .find(|entry| entry.mission_id == second_mission_id)
            .expect("second execution group entry should exist");
        assert_eq!(mission_entry.context_used_knowledge_count, 1);
        assert_eq!(mission_entry.context_used_memory_count, 1);
        assert_eq!(mission_entry.context_code_index_knowledge_count, 1);
        assert_eq!(mission_entry.context_extracted_memory_count, 1);
        assert_eq!(
            mission_entry.context_knowledge_source_paths,
            vec!["src/routes.rs".to_string()]
        );
        assert_eq!(
            mission_entry.context_memory_extraction_refs,
            vec![format!("extract-session-action-{first_accepted_at}")]
        );
        let second_root_task = task_store
            .get_task(&second_root_task_id)
            .expect("second root task should exist");
        assert_eq!(second_root_task.status, TaskStatus::Completed);
        let second_children = task_store.get_children(&second_root_task_id);
        assert_eq!(second_children.len(), 1);
        assert_eq!(second_children[0].status, TaskStatus::Completed);
        let second_extraction_id = format!("extract-session-action-{second_accepted_at}");
        let verification = state
            .shadow_execution_pipeline()
            .expect("shadow execution pipeline should exist")
            .memory_store
            .verify_extraction_linkage(&second_extraction_id)
            .expect("second route extraction should be persisted");
        assert!(verification.is_consistent);

        let ownership = state
            .session_store
            .execution_ownership(&SessionId::new("session-route-shadow"))
            .expect("session ownership should be bound");
        assert!(ownership.mission_id.is_some());
        assert!(ownership.task_id.is_some());
        assert!(ownership.worker_id.is_some());
    }

    #[tokio::test]
    async fn session_action_route_does_not_write_extraction_or_bind_mission_when_dispatch_fails() {
        let state = test_state_with_unhealthy_shadow_execution_pipeline();
        let app = build_router(state.clone());

        let (status, body) = post_json(
            app,
            "/session/action",
            json!({
                "session_id": "session-route-shadow",
                "text": "Route parser refresh failure",
                "deep_task": true,
                "skill_name": "refactor",
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "dispatch failure should still accept the task: {body:?}");
        assert!(body["session_id"].is_string());
        assert!(body["root_task_id"].is_string());

        let extraction_history = state
            .shadow_execution_pipeline()
            .expect("shadow execution pipeline should exist")
            .memory_store
            .extraction_results_for_session(&SessionId::new("session-route-shadow"));
        assert!(extraction_history.is_empty());

        let ownership = state
            .session_store
            .execution_ownership(&SessionId::new("session-route-shadow"))
            .expect("base session ownership should remain present");
        assert!(ownership.mission_id.is_none());
        assert!(ownership.task_id.is_none());
        assert!(ownership.worker_id.is_none());
    }

    #[tokio::test]
    async fn session_action_route_skips_extraction_for_blank_text_inputs() {
        let state = test_state_with_shadow_execution_pipeline();
        let app = build_router(state.clone());

        let (status, body) = post_json(
            app,
            "/session/action",
            json!({
                "session_id": "session-route-blank",
                "text": "   ",
                "deep_task": true,
                "skill_name": "refactor",
                "images": [],
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "unexpected response body: {body:?}");

        let extraction_history = state
            .shadow_execution_pipeline()
            .expect("shadow execution pipeline should exist")
            .memory_store
            .extraction_results_for_session(&SessionId::new("session-route-blank"));
        assert!(extraction_history.is_empty());

        let accepted_at = body["accepted_at"]
            .as_u64()
            .expect("accepted_at should serialize as integer");
        let mission_id = format!("mission-session-action-{accepted_at}");
        let runtime_read_model = state.runtime_read_model_dto();
        let mission_entry = runtime_read_model
            .details
            .execution_groups
            .iter()
            .find(|entry| entry.mission_id == mission_id)
            .expect("execution group entry should exist");
        assert_eq!(mission_entry.context_used_memory_count, 0);
        assert!(mission_entry.context_memory_extraction_refs.is_empty());
    }

    #[tokio::test]
    async fn recovery_resume_route_executes_writeback_and_marks_session_resumed() {
        let state = test_state_with_shadow_execution_pipeline();
        let app = build_router(state.clone());

        let (status, seed_body) = post_json(
            app.clone(),
            "/session/action",
            json!({
                "session_id": "session-route-shadow",
                "text": "seed recovery route state",
                "deep_task": true,
                "skill_name": "refactor",
                "images": [],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected response body: {seed_body:?}");

        let session_id = SessionId::new("session-route-shadow");
        let ownership = state
            .session_store
            .execution_ownership(&session_id)
            .expect("session ownership should exist after seed dispatch");
        let workspace_id = ownership
            .workspace_id
            .clone()
            .expect("seed dispatch should bind workspace");
        let expected_worker_id = ownership
            .worker_id
            .as_ref()
            .map(ToString::to_string)
            .expect("seed dispatch should bind worker");
        let task_store = state.task_store().expect("task store should be configured");
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
            "/recovery/resume",
            json!({
                "recovery_id": recovery.recovery_id,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected response body: {body:?}");
        assert_eq!(body["recovery_id"], "recovery-route-1");
        assert_eq!(body["snapshot_id"], "snapshot-route-recovery");
        assert_eq!(body["session_id"], "session-route-shadow");
        assert_eq!(body["workspace_id"], "workspace-route-shadow");
        assert_eq!(body["memory_writeback_applied"], true);
        assert_eq!(body["worker_id"], expected_worker_id);
        assert!(
            body["event_id"]
                .as_str()
                .expect("event_id should serialize as string")
                .contains("event-recovery-resume-recovery-route-1-")
        );

        let verification = state
            .shadow_execution_pipeline()
            .expect("shadow execution pipeline should exist")
            .memory_store
            .verify_extraction_linkage("extract-recovery-recovery-route-1")
            .expect("recovery route extraction should persist");
        assert!(verification.is_consistent);
        let linkage = state
            .shadow_execution_pipeline()
            .expect("shadow execution pipeline should exist")
            .memory_store
            .extraction_linkage("extract-recovery-recovery-route-1")
            .expect("recovery route extraction linkage should exist");
        assert_eq!(
            linkage.extraction.source_ref.as_deref(),
            Some("recovery://recovery-route-1/snapshot/snapshot-route-recovery")
        );
        assert_eq!(linkage.produced_records[0].content, "resume route followup");

        let sidecar = state
            .session_store
            .execution_sidecar_export(&session_id)
            .expect("session sidecar should exist");
        assert_eq!(
            sidecar.current_status,
            magi_session_store::SessionExecutionSidecarStatus::Resumed
        );
        assert_eq!(sidecar.recovery_ref.as_deref(), Some("recovery-route-1"));
    }

    #[tokio::test]
    async fn recovery_resume_route_uses_requested_worker_id_consistently() {
        let state = test_state_with_shadow_execution_pipeline();
        let app = build_router(state.clone());

        let (status, seed_body) = post_json(
            app.clone(),
            "/session/action",
            json!({
                "session_id": "session-route-shadow-override",
                "text": "seed recovery route state",
                "deep_task": true,
                "skill_name": "refactor",
                "images": [],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected response body: {seed_body:?}");

        let session_id = SessionId::new("session-route-shadow-override");
        let ownership = state
            .session_store
            .execution_ownership(&session_id)
            .expect("session ownership should exist after seed dispatch");
        let workspace_id = ownership
            .workspace_id
            .clone()
            .expect("seed dispatch should bind workspace");
        let task_store = state.task_store().expect("task store should be configured");
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
            "snapshot-route-recovery-override",
            "Route recovery snapshot with worker override",
        );
        let recovery = state.workspace_registry.prepare_recovery_entry(
            workspace_id,
            ownership,
            snapshot.snapshot_id,
            "recovery-route-override",
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
            "/recovery/resume",
            json!({
                "recovery_id": recovery.recovery_id,
                "worker_id": "worker-route-override",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected response body: {body:?}");
        assert_eq!(body["worker_id"], "worker-route-override");

        let sidecar = state
            .session_store
            .execution_sidecar_export(&session_id)
            .expect("session sidecar should exist");
        assert_eq!(
            sidecar
                .ownership
                .worker_id
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some("worker-route-override")
        );
    }

    #[tokio::test]
    async fn recovery_resume_route_returns_not_found_for_unknown_recovery() {
        let app = build_router(test_state_with_shadow_execution_pipeline());

        let (status, body) = post_json(
            app,
            "/recovery/resume",
            json!({
                "recovery_id": "missing-recovery",
            }),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error_code"], "RECOVERY_NOT_FOUND");
        assert_eq!(body["message"], "恢复入口不存在: missing-recovery");
    }

    #[tokio::test]
    async fn recovery_resume_route_rejects_prepared_recovery_with_input_error() {
        let state = test_state_with_shadow_execution_pipeline();
        let app = build_router(state.clone());

        let session_id = SessionId::new("session-route-prepared");
        let workspace_id = WorkspaceId::new("workspace-route-prepared");
        state
            .session_store
            .create_session(session_id.clone(), "prepared recovery session")
            .expect("session should be creatable");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/Users/xie/code/magi-rust-rewrite"),
            )
            .expect("workspace should register");
        let recovery = state.workspace_registry.prepare_recovery_entry(
            workspace_id.clone(),
            ExecutionOwnership {
                session_id: Some(session_id),
                workspace_id: Some(workspace_id),
                worker_id: Some(magi_core::WorkerId::new("worker-route-prepared")),
                ..ExecutionOwnership::default()
            },
            "snapshot-route-prepared",
            "recovery-route-prepared",
            Some("prepared recovery".to_string()),
        );

        let (status, body) = post_json(
            app,
            "/recovery/resume",
            json!({
                "recovery_id": recovery.recovery_id,
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error_code"], "INPUT_INVALID");
        assert_eq!(
            body["message"],
            "恢复入口 recovery-route-prepared 当前状态为 prepared，必须先进入 ready 才能恢复"
        );
    }

    #[tokio::test]
    async fn recovery_resume_route_rejects_consumed_recovery_with_input_error() {
        let state = test_state_with_shadow_execution_pipeline();
        let app = build_router(state.clone());

        let session_id = SessionId::new("session-route-consumed");
        let workspace_id = WorkspaceId::new("workspace-route-consumed");
        state
            .session_store
            .create_session(session_id.clone(), "consumed recovery session")
            .expect("session should be creatable");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/Users/xie/code/magi-rust-rewrite"),
            )
            .expect("workspace should register");
        let recovery = state.workspace_registry.prepare_recovery_entry(
            workspace_id.clone(),
            ExecutionOwnership {
                session_id: Some(session_id),
                workspace_id: Some(workspace_id),
                worker_id: Some(magi_core::WorkerId::new("worker-route-consumed")),
                ..ExecutionOwnership::default()
            },
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

        let (status, body) = post_json(
            app,
            "/recovery/resume",
            json!({
                "recovery_id": recovery.recovery_id,
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error_code"], "INPUT_INVALID");
        assert_eq!(
            body["message"],
            "恢复入口 recovery-route-consumed 已被消费，不能重复恢复"
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

    impl ModelBridgeClient for StaticModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            Ok(BridgeResponse {
                ok: true,
                payload: format!("shadow-model::{}", request.prompt.trim()),
            })
        }
    }

    struct FailingModelBridgeClient;

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
                        SHADOW_MODEL_PROVIDER => {
                            bridge_response("shadow-model::bridge preflight ping")
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
                        protocol_version: "shadow-local-bridge-v1".to_string(),
                        server_kind: BridgeServerKind::Model,
                        services: vec![
                            descriptor(SHADOW_MODEL_PROVIDER),
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
                        SHADOW_MODEL_PROVIDER => bridge_response("shadow-model::bridge cutover smoke"),
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
                        protocol_version: "shadow-local-bridge-v1".to_string(),
                        server_kind: BridgeServerKind::Model,
                        services: vec![
                            descriptor_with_profile(
                                SHADOW_MODEL_PROVIDER,
                                "ready",
                                "shadow-model-bridge-payload-v1",
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
            let outcome = responses
                .get(&request.method)
                .cloned()
                .unwrap_or_else(|| FakeTransportOutcome::Protocol {
                    message: format!("unexpected method {}", request.method),
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
            protocol_version: "shadow-local-bridge-v1".to_string(),
            server_kind: kind,
            health_method: LOCAL_BRIDGE_HEALTH_METHOD.to_string(),
            supported_methods: vec!["bridge.describe_services".to_string()],
        })
        .expect("handshake should serialize")
    }

    fn health(kind: BridgeServerKind, status: &str, ok: bool) -> Value {
        serde_json::to_value(BridgeServerHealth {
            protocol_version: "shadow-local-bridge-v1".to_string(),
            server_kind: kind,
            status: status.to_string(),
            ok,
        })
        .expect("health should serialize")
    }

    fn catalog(kind: BridgeServerKind, service_name: &str) -> Value {
        serde_json::to_value(BridgeServerServiceCatalog {
            protocol_version: "shadow-local-bridge-v1".to_string(),
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
            shim_kind: "shadow".to_string(),
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
                    FakeTransportOutcome::Payload(catalog(BridgeServerKind::Model, "shadow-model")),
                ),
            ]))),
        ));

        let snapshot = get_json(app, "/bridges/services").await;
        assert_eq!(snapshot["services"][0]["server_kind"], "model");
        assert_eq!(snapshot["services"][0]["health"]["status"], "healthy");
        assert_eq!(
            snapshot["services"][0]["service_catalog"]["services"][0]["service_name"],
            "shadow-model"
        );
    }

    #[tokio::test]
    async fn bridge_preflight_route_executes_smoke_results_from_registered_transports() {
        let app = build_router(
            test_state()
                .with_bridge_probe_transport(
                    BridgeServerKind::Host,
                    Arc::new(FakeTransport::new(HashMap::from([(
                        "host.call".to_string(),
                        FakeTransportOutcome::Payload(bridge_response("workspace:///repo")),
                    )]))),
                )
                .with_bridge_probe_transport(
                    BridgeServerKind::Model,
                    Arc::new(FakeTransport::new(HashMap::from([
                        (
                            "model.invoke".to_string(),
                            FakeTransportOutcome::Payload(bridge_response(
                                "shadow-model::bridge preflight ping",
                            )),
                        ),
                        (
                            LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
                            FakeTransportOutcome::Payload(catalog(
                                BridgeServerKind::Model,
                                SHADOW_MODEL_PROVIDER,
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
                                    manager: descriptor("shadow-mcp-manager"),
                                    servers: vec![descriptor(SHADOW_MCP_SERVER_NAME)],
                                    selection_targets: vec![SHADOW_MCP_SERVER_NAME.to_string()],
                                    default_route_status: "available".to_string(),
                                    default_route_target: SHADOW_MCP_SERVER_NAME.to_string(),
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
        assert_eq!(services.len(), 3, "unexpected preflight snapshot: {snapshot:?}");

        let host = services
            .iter()
            .find(|entry| entry["server_kind"] == "host")
            .expect("host preflight should exist");
        assert_eq!(host["checks"][0]["check_name"], "workspace_roots");
        assert_eq!(host["checks"][0]["target"], "vscode.workspace_roots");
        assert_eq!(host["checks"][0]["ok"], true);

        let model = services
            .iter()
            .find(|entry| entry["server_kind"] == "model")
            .expect("model preflight should exist");
        assert_eq!(model["checks"][0]["check_name"], "invoke");
        assert_eq!(model["checks"][0]["target"], SHADOW_MODEL_PROVIDER);
        assert_eq!(model["checks"][0]["ok"], true);

        let mcp = services
            .iter()
            .find(|entry| entry["server_kind"] == "mcp")
            .expect("mcp preflight should exist");
        assert_eq!(mcp["checks"][0]["check_name"], "list_servers");
        assert_eq!(mcp["checks"][0]["target"], "shadow-mcp-manager");
        assert_eq!(mcp["checks"][0]["ok"], true);
        assert_eq!(
            mcp["checks"][1]["target"],
            format!("{SHADOW_MCP_SERVER_NAME}.{SHADOW_MCP_TOOL_NAME}")
        );
        assert_eq!(mcp["checks"][1]["ok"], true);
    }

    #[tokio::test]
    async fn bridge_preflight_route_executes_ready_openai_compatible_smoke() {
        let app = build_router(
            test_state().with_bridge_probe_transport(
                BridgeServerKind::Model,
                Arc::new(ProviderAwareModelTransport),
            ),
        );

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
                .any(|check| check["target"] == SHADOW_MODEL_PROVIDER && check["ok"] == true),
            "model preflight should keep shadow-model smoke: {model:?}"
        );
        assert!(
            checks.iter().any(|check| check["target"] == "openai-compatible"
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
                    BridgeServerKind::Host,
                    Arc::new(FakeTransport::new(HashMap::from([(
                        "host.call".to_string(),
                        FakeTransportOutcome::Payload(bridge_response("workspace:///repo")),
                    )]))),
                )
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
                                        "shadow-mcp-manager",
                                        "ready",
                                        "shadow-mcp-observability",
                                    ),
                                    servers: vec![
                                        descriptor_with_profile(
                                            SHADOW_MCP_SERVER_NAME,
                                            "ready",
                                            "inspection-core-v1",
                                        ),
                                        descriptor_with_profile(
                                            "shadow-mcp-observability",
                                            "ready",
                                            "observability-v1",
                                        ),
                                    ],
                                    selection_targets: vec![
                                        SHADOW_MCP_SERVER_NAME.to_string(),
                                        "shadow-mcp-observability".to_string(),
                                    ],
                                    default_route_status: "ready".to_string(),
                                    default_route_target: "shadow-mcp-observability".to_string(),
                                })
                                .expect("mcp manager list should serialize"),
                            ),
                        ),
                        (
                            "mcp.describe_server".to_string(),
                            FakeTransportOutcome::Payload(json!({
                                "manager": descriptor_with_route(
                                    "shadow-mcp-manager",
                                    "ready",
                                    "shadow-mcp-observability",
                                ),
                                "server": descriptor_with_profile(
                                    "shadow-mcp-observability",
                                    "ready",
                                    "observability-v1",
                                ),
                                "lifecycle_events": [],
                            })),
                        ),
                        (
                            "mcp.call_tool".to_string(),
                            FakeTransportOutcome::Payload(structured_bridge_response(json!({
                                "server_name": "shadow-mcp-observability",
                                "default_route_status": "ready",
                                "default_route_target": "shadow-mcp-observability",
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
        assert_eq!(services.len(), 3, "unexpected cutover smoke snapshot: {snapshot:?}");
        assert_eq!(snapshot["overall_ok"], true);
        assert_eq!(snapshot["checked_service_count"], 3);
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

        let host = services
            .iter()
            .find(|entry| entry["server_kind"] == "host")
            .expect("host cutover should exist");
        assert_eq!(host["service_ok"], true);
        assert_eq!(host["blocking_check_count"], 0);
        assert!(
            host["blocking_targets"]
                .as_array()
                .expect("host blocking targets should serialize as array")
                .is_empty()
        );
        assert_eq!(host["checks"][0]["check_name"], "workspace_roots_contract");
        assert_eq!(host["checks"][0]["ok"], true);

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
        assert_eq!(
            mcp["checks"][0]["mcp_contract"]["route_status"],
            "ready"
        );
        assert_eq!(
            mcp["checks"][0]["mcp_contract"]["route_target"],
            "shadow-mcp-observability"
        );
        assert_eq!(
            mcp["checks"][0]["mcp_contract"]["resolved_server"],
            "shadow-mcp-observability"
        );
        assert_eq!(mcp["mcp_default_route_gate"]["route_status"], "ready");
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_status"],
            mcp["checks"][0]["mcp_contract"]["route_status"]
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_target"],
            "shadow-mcp-observability"
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_target"],
            mcp["checks"][0]["mcp_contract"]["route_target"]
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["resolved_server"],
            "shadow-mcp-observability"
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
        let app = build_router(test_state().with_bridge_probe_transport(
            BridgeServerKind::Mcp,
            Arc::new(FakeTransport::new(HashMap::from([
                (
                    "mcp.list_servers".to_string(),
                    FakeTransportOutcome::Payload(
                        serde_json::to_value(McpManagerListServersResponse {
                            manager: descriptor_with_route(
                                "shadow-mcp-manager",
                                "ready",
                                "shadow-mcp-observability",
                            ),
                            servers: vec![descriptor_with_profile(
                                "shadow-mcp-observability",
                                "ready",
                                "observability-v1",
                            )],
                            selection_targets: vec!["shadow-mcp-observability".to_string()],
                            default_route_status: "ready".to_string(),
                            default_route_target: "shadow-mcp-observability".to_string(),
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
                        "server_name": "shadow-mcp-observability",
                        "default_route_status": "ready",
                        "default_route_target": "shadow-mcp-observability",
                        "tool_name": "echo.describe",
                    }))),
                ),
            ]))),
        ));

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        assert_eq!(snapshot["overall_ok"], false);
        assert_eq!(snapshot["blocking_check_count"], 1);
        assert_eq!(
            snapshot["blocking_issues"][0]["reason_code"],
            "mcp_default_route_target_describe_failed"
        );
        assert_eq!(
            snapshot["blocking_issue_counts_by_reason_code"][
                "mcp_default_route_target_describe_failed"
            ],
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
        let app = build_router(test_state().with_bridge_probe_transport(
            BridgeServerKind::Mcp,
            Arc::new(FakeTransport::new(HashMap::from([
                (
                    "mcp.list_servers".to_string(),
                    FakeTransportOutcome::Payload(
                        serde_json::to_value(McpManagerListServersResponse {
                            manager: descriptor_with_route(
                                "shadow-mcp-manager",
                                "ready",
                                "shadow-mcp-observability",
                            ),
                            servers: vec![descriptor_with_profile(
                                "shadow-mcp-observability",
                                "ready",
                                "observability-v1",
                            )],
                            selection_targets: vec!["shadow-mcp-observability".to_string()],
                            default_route_status: "ready".to_string(),
                            default_route_target: "shadow-mcp-observability".to_string(),
                        })
                        .expect("mcp manager list should serialize"),
                    ),
                ),
                (
                    "mcp.describe_server".to_string(),
                    FakeTransportOutcome::Payload(json!({
                        "manager": descriptor_with_route(
                            "shadow-mcp-manager",
                            "ready",
                            "shadow-mcp-observability",
                        ),
                        "server": descriptor_with_profile(
                            "shadow-mcp-observability",
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
        ));

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
        let app = build_router(test_state().with_bridge_probe_transport(
            BridgeServerKind::Mcp,
            Arc::new(FakeTransport::new(HashMap::from([
                (
                    "mcp.list_servers".to_string(),
                    FakeTransportOutcome::Payload(
                        serde_json::to_value(McpManagerListServersResponse {
                            manager: descriptor_with_route(
                                "shadow-mcp-manager",
                                "ready",
                                "shadow-mcp-observability",
                            ),
                            servers: vec![descriptor_with_profile(
                                "shadow-mcp-observability",
                                "ready",
                                "observability-v1",
                            )],
                            selection_targets: vec!["shadow-mcp-observability".to_string()],
                            default_route_status: "ready".to_string(),
                            default_route_target: "shadow-mcp-observability".to_string(),
                        })
                        .expect("mcp manager list should serialize"),
                    ),
                ),
                (
                    "mcp.describe_server".to_string(),
                    FakeTransportOutcome::Payload(json!({
                        "manager": descriptor_with_route(
                            "shadow-mcp-manager",
                            "ready",
                            "shadow-mcp-observability",
                        ),
                        "server": descriptor_with_profile(
                            "shadow-mcp-observability",
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
        ));

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
        assert_eq!(snapshot["blocking_issues"][0]["response_excerpt"], Value::Null);
        assert_eq!(snapshot["services"][0]["service_ok"], false);
        assert_eq!(
            snapshot["services"][0]["mcp_default_route_gate"]["contract_ok"],
            false
        );
    }

    #[tokio::test]
    async fn bridge_cutover_smoke_route_surfaces_mcp_metadata_drift_reason_codes() {
        let app = build_router(test_state().with_bridge_probe_transport(
            BridgeServerKind::Mcp,
            Arc::new(FakeTransport::new(HashMap::from([
                (
                    "mcp.list_servers".to_string(),
                    FakeTransportOutcome::Payload(
                        serde_json::to_value(McpManagerListServersResponse {
                            manager: descriptor_with_route(
                                "shadow-mcp-manager",
                                "ready",
                                "shadow-mcp-observability",
                            ),
                            servers: vec![descriptor_with_profile(
                                "shadow-mcp-observability",
                                "ready",
                                "observability-v1",
                            )],
                            selection_targets: vec!["shadow-mcp-observability".to_string()],
                            default_route_status: "ready".to_string(),
                            default_route_target: "shadow-mcp-observability".to_string(),
                        })
                        .expect("mcp manager list should serialize"),
                    ),
                ),
                (
                    "mcp.describe_server".to_string(),
                    FakeTransportOutcome::Payload(json!({
                        "manager": descriptor_with_route(
                            "shadow-mcp-manager",
                            "ready",
                            "shadow-mcp-observability",
                        ),
                        "server": descriptor_with_profile(
                            "shadow-mcp-observability",
                            "ready",
                            "observability-v1",
                        ),
                        "lifecycle_events": [],
                    })),
                ),
                (
                    "mcp.call_tool".to_string(),
                    FakeTransportOutcome::Payload(structured_bridge_response(json!({
                        "server_name": "shadow-mcp-observability",
                        "default_route_status": "ready",
                        "default_route_target": "shadow-mcp-inspection",
                        "tool_name": "echo.describe",
                    }))),
                ),
            ]))),
        ));

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
            "shadow-mcp-observability"
        );
        assert_eq!(
            snapshot["services"][0]["mcp_default_route_gate"]["contract_ok"],
            false
        );
    }

    #[tokio::test]
    async fn bridge_cutover_smoke_route_surfaces_mcp_resolved_server_mismatch_reason_codes() {
        let app = build_router(test_state().with_bridge_probe_transport(
            BridgeServerKind::Mcp,
            Arc::new(FakeTransport::new(HashMap::from([
                (
                    "mcp.list_servers".to_string(),
                    FakeTransportOutcome::Payload(
                        serde_json::to_value(McpManagerListServersResponse {
                            manager: descriptor_with_route(
                                "shadow-mcp-manager",
                                "ready",
                                "shadow-mcp-observability",
                            ),
                            servers: vec![
                                descriptor_with_profile(
                                    "shadow-mcp-observability",
                                    "ready",
                                    "observability-v1",
                                ),
                                descriptor_with_profile(
                                    "shadow-mcp-inspection",
                                    "ready",
                                    "inspection-v1",
                                ),
                            ],
                            selection_targets: vec![
                                "shadow-mcp-observability".to_string(),
                                "shadow-mcp-inspection".to_string(),
                            ],
                            default_route_status: "ready".to_string(),
                            default_route_target: "shadow-mcp-observability".to_string(),
                        })
                        .expect("mcp manager list should serialize"),
                    ),
                ),
                (
                    "mcp.describe_server".to_string(),
                    FakeTransportOutcome::Payload(json!({
                        "manager": descriptor_with_route(
                            "shadow-mcp-manager",
                            "ready",
                            "shadow-mcp-observability",
                        ),
                        "server": descriptor_with_profile(
                            "shadow-mcp-observability",
                            "ready",
                            "observability-v1",
                        ),
                        "lifecycle_events": [],
                    })),
                ),
                (
                    "mcp.call_tool".to_string(),
                    FakeTransportOutcome::Payload(structured_bridge_response(json!({
                        "server_name": "shadow-mcp-inspection",
                        "default_route_status": "ready",
                        "default_route_target": "shadow-mcp-observability",
                        "tool_name": "echo.describe",
                    }))),
                ),
            ]))),
        ));

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
            "shadow-mcp-observability"
        );
        assert_eq!(
            snapshot["services"][0]["mcp_default_route_gate"]["resolved_server"],
            "shadow-mcp-inspection"
        );
        assert_eq!(
            snapshot["services"][0]["mcp_default_route_gate"]["contract_ok"],
            false
        );
    }

    #[tokio::test]
    async fn bridge_routes_do_not_touch_shadow_execution_state() {
        let state = test_state_with_shadow_execution_pipeline();
        let app = build_router(
            state
                .clone()
                .with_bridge_probe_transport(
                    BridgeServerKind::Host,
                    Arc::new(FakeTransport::new(HashMap::from([(
                        "host.call".to_string(),
                        FakeTransportOutcome::Payload(bridge_response("workspace:///repo")),
                    )]))),
                )
                .with_bridge_probe_transport(
                    BridgeServerKind::Model,
                    Arc::new(FakeTransport::new(HashMap::from([
                        (
                            "model.invoke".to_string(),
                            FakeTransportOutcome::Payload(bridge_response(
                                "shadow-model::bridge preflight ping",
                            )),
                        ),
                        (
                            LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
                            FakeTransportOutcome::Payload(catalog(
                                BridgeServerKind::Model,
                                SHADOW_MODEL_PROVIDER,
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
                                    manager: descriptor("shadow-mcp-manager"),
                                    servers: vec![descriptor(SHADOW_MCP_SERVER_NAME)],
                                    selection_targets: vec![SHADOW_MCP_SERVER_NAME.to_string()],
                                    default_route_status: "available".to_string(),
                                    default_route_target: SHADOW_MCP_SERVER_NAME.to_string(),
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
        let before_session_sidecars = serde_json::to_value(state.session_store.execution_sidecar_exports())
            .expect("session sidecars should serialize");
        let before_workspace_sidecars = serde_json::to_value(state.workspace_registry.recovery_sidecar_exports())
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
                        server_kind: BridgeServerKind::Host,
                        handshake: Some(BridgeServerHandshake {
                            protocol_version: "shadow-local-bridge-v1".to_string(),
                            server_kind: BridgeServerKind::Host,
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
                            protocol_version: "shadow-local-bridge-v1".to_string(),
                            server_kind: BridgeServerKind::Host,
                            services: vec![],
                        }),
                        service_catalog_error: None,
                    }],
                },
            },
        )));

        let snapshot = get_json(app, "/bridges/services").await;
        assert_eq!(snapshot["services"][0]["server_kind"], "host");
        assert!(snapshot["services"][0]["handshake"].is_object());
        assert!(snapshot["services"][0]["health"].is_null());
        assert_eq!(
            snapshot["services"][0]["health_error"]["layer"],
            "RemoteBusiness"
        );
        assert!(snapshot["services"][0]["service_catalog"].is_object());
    }

    // ─── /api/tasks/* task graph routes integration tests ───

    fn test_state_with_task_store() -> ApiState {
        use magi_orchestrator::task_store::TaskStore;
        let state = test_state();
        let task_store = Arc::new(TaskStore::new());
        state.with_task_store(task_store)
    }

    #[tokio::test]
    async fn task_graph_create_and_get_task() {
        let app = build_router(test_state_with_task_store());

        let (status, body) = post_json(
            app.clone(),
            "/api/tasks/create",
            json!({
                "task_id": "task-1",
                "mission_id": "mission-1",
                "root_task_id": "task-1",
                "kind": "Objective",
                "title": "Root objective",
                "goal": "Complete the objective",
                "status": "Draft",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected create response: {body:?}");
        assert_eq!(body["task_id"], "task-1");
        assert_eq!(body["title"], "Root objective");
        assert_eq!(body["kind"], "Objective");
        assert_eq!(body["status"], "Draft");

        let retrieved = get_json(app, "/api/tasks/task-1").await;
        assert_eq!(retrieved["task_id"], "task-1");
        assert_eq!(retrieved["title"], "Root objective");
        assert_eq!(retrieved["mission_id"], "mission-1");
    }

    #[tokio::test]
    async fn task_graph_create_graph_and_get_projection() {
        let app = build_router(test_state_with_task_store());

        // Create a small task tree: Objective -> Phase -> 2 Actions
        for (task_id, parent, kind, status) in [
            ("obj-1", None, "Objective", "Running"),
            ("phase-1", Some("obj-1"), "Phase", "Running"),
            ("act-1", Some("phase-1"), "Action", "Completed"),
            ("act-2", Some("phase-1"), "Action", "Running"),
        ] {
            let mut payload = json!({
                "task_id": task_id,
                "mission_id": "mission-proj",
                "root_task_id": "obj-1",
                "kind": kind,
                "title": format!("Task {}", task_id),
                "goal": format!("Goal for {}", task_id),
                "status": status,
            });
            if let Some(p) = parent {
                payload["parent_task_id"] = json!(p);
            }
            let (status_code, _body) = post_json(app.clone(), "/api/tasks/create", payload).await;
            assert_eq!(status_code, StatusCode::OK);
        }

        let projection = get_json(app, "/api/tasks/graph/obj-1").await;
        assert_eq!(projection["root_task"]["task_id"], "obj-1");
        assert_eq!(projection["current_phase"], "Task phase-1");
        assert_eq!(projection["progress_summary"]["total_tasks"], 4);
        assert_eq!(projection["progress_summary"]["completed_tasks"], 1);
        assert_eq!(projection["progress_summary"]["running_tasks"], 3);
        assert_eq!(projection["aggregate_status"], "Running");
    }

    #[tokio::test]
    async fn task_graph_update_status() {
        let app = build_router(test_state_with_task_store());

        let (status, _body) = post_json(
            app.clone(),
            "/api/tasks/create",
            json!({
                "task_id": "task-status-1",
                "mission_id": "mission-status",
                "root_task_id": "task-status-1",
                "kind": "Action",
                "title": "Status test task",
                "goal": "Test status transitions",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Update Draft -> Ready
        let (status, body) = post_json(
            app.clone(),
            "/api/tasks/task-status-1/status",
            json!({ "status": "Ready" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected status update response: {body:?}");
        assert_eq!(body["status"], "Ready");

        // Update Ready -> Running
        let (status, body) = post_json(
            app.clone(),
            "/api/tasks/task-status-1/status",
            json!({ "status": "Running" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "Running");

        // Update Running -> Completed
        let (status, body) = post_json(
            app.clone(),
            "/api/tasks/task-status-1/status",
            json!({ "status": "Completed" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "Completed");

        // Verify via GET
        let task = get_json(app, "/api/tasks/task-status-1").await;
        assert_eq!(task["status"], "Completed");
    }

    #[tokio::test]
    async fn task_graph_returns_not_found_for_missing_task() {
        let app = build_router(test_state_with_task_store());

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/tasks/nonexistent-task")
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
}
