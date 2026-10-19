use crate::{
    errors::ApiError,
    goal_service::{GoalActionRequest, GoalQuery, GoalUpdateRequest},
    state::ApiState,
};
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/goals/current", get(get_current_goal))
        .route("/goals/current/update", post(update_current_goal))
        .route("/goals/current/pause", post(pause_current_goal))
        .route("/goals/current/resume", post(resume_current_goal))
        .route("/goals/current/clear", post(clear_current_goal))
        .route("/goals/current/plan/clear", post(clear_current_plan))
}

async fn get_current_goal(
    State(state): State<ApiState>,
    Query(query): Query<GoalQuery>,
) -> Result<Json<magi_app_server_protocol::GoalSnapshot>, ApiError> {
    crate::goal_service::get_current_goal(&state, query)
        .await
        .and_then(crate::goal_service::snapshot_to_wire)
        .map(Json)
}

async fn update_current_goal(
    State(state): State<ApiState>,
    Json(request): Json<GoalUpdateRequest>,
) -> Result<Json<magi_app_server_protocol::GoalSnapshot>, ApiError> {
    crate::goal_service::update_current_goal(&state, request)
        .await
        .and_then(crate::goal_service::snapshot_to_wire)
        .map(Json)
}

async fn pause_current_goal(
    State(state): State<ApiState>,
    Json(request): Json<GoalActionRequest>,
) -> Result<Json<magi_app_server_protocol::GoalSnapshot>, ApiError> {
    crate::goal_service::pause_current_goal(&state, request)
        .await
        .and_then(crate::goal_service::snapshot_to_wire)
        .map(Json)
}

async fn resume_current_goal(
    State(state): State<ApiState>,
    Json(request): Json<GoalActionRequest>,
) -> Result<Json<magi_app_server_protocol::GoalSnapshot>, ApiError> {
    crate::goal_service::resume_current_goal(&state, request)
        .await
        .and_then(crate::goal_service::snapshot_to_wire)
        .map(Json)
}

async fn clear_current_goal(
    State(state): State<ApiState>,
    Json(request): Json<GoalActionRequest>,
) -> Result<Json<magi_app_server_protocol::GoalSnapshot>, ApiError> {
    crate::goal_service::clear_current_goal(&state, request)
        .await
        .and_then(crate::goal_service::snapshot_to_wire)
        .map(Json)
}

async fn clear_current_plan(
    State(state): State<ApiState>,
    Json(request): Json<GoalActionRequest>,
) -> Result<Json<magi_app_server_protocol::GoalSnapshot>, ApiError> {
    crate::goal_service::clear_current_plan(&state, request)
        .await
        .and_then(crate::goal_service::snapshot_to_wire)
        .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal_service::allowed_actions;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header::CONTENT_TYPE};
    use magi_bridge_client::ModelResponse;
    use magi_bridge_client::{
        BridgeClientError, BridgeErrorLayer, ModelBridgeClient, ModelInvocationRequest,
        ModelStreamingDelta,
    };
    use magi_conversation_runtime::{
        task_execution_dispatcher::{
            ExecutionPipeline, LlmTaskDispatcher, LlmTaskDispatcherDependencies,
        },
        task_runner_bridge::TaskDispatcher,
    };
    use magi_core::{AbsolutePath, MissionId, SessionId, UtcMillis, WorkspaceId};
    use magi_core::{AccessProfile, PlanItemStatus};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_orchestrator::{OrchestratedExecutionRuntime, task_store::TaskStore};
    use magi_session_store::{GoalRevisionExpectation, GoalStatus, SessionGoal, SessionStore};
    use magi_tool_runtime::ToolRegistry;
    use magi_worker_runtime::WorkerRuntime;
    use magi_workspace::WorkspaceStore;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tower::ServiceExt;

    fn create_test_goal(
        state: &ApiState,
        session_id: &SessionId,
        turn_id: &str,
        objective: &str,
    ) -> SessionGoal {
        let (_, thread_id) =
            state
                .session_store
                .ensure_session_mission(session_id, UtcMillis::now(), || {
                    MissionId::new(format!("mission-{session_id}"))
                });
        state
            .session_store
            .create_goal(
                session_id.clone(),
                thread_id,
                turn_id,
                objective,
                AccessProfile::Restricted,
            )
            .expect("test goal should be creatable")
    }

    struct RecordingFailingModelClient {
        calls: Arc<AtomicUsize>,
    }

    impl ModelBridgeClient for RecordingFailingModelClient {
        fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
            magi_bridge_client::native_executor_capabilities()
        }

        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32007),
                message: "目标续跑执行测试".to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            self.invoke(request)
        }
    }

    fn state_with_recording_goal_dispatcher(calls: Arc<AtomicUsize>) -> ApiState {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let task_store = Arc::new(TaskStore::new());
        let state = ApiState::new(
            "magi-test",
            Arc::clone(&event_bus),
            Arc::clone(&session_store),
            Arc::clone(&workspace_store),
            Arc::clone(&governance),
        )
        .with_task_store(Arc::clone(&task_store));
        let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        let execution_runtime = OrchestratedExecutionRuntime::new(WorkerRuntime::new())
            .with_task_store(Arc::clone(&task_store));
        let dispatcher = Arc::new(
            LlmTaskDispatcher::new(
                event_bus,
                ExecutionPipeline {
                    execution_runtime,
                    memory_store: magi_memory_store::MemoryStore::new(),
                },
                LlmTaskDispatcherDependencies {
                    session_store: Arc::clone(&session_store),
                    execution_registry: state.task_execution_registry().clone(),
                    completion_sink: Arc::new(
                        magi_conversation_runtime::TaskCompletionNotifier::new(task_store.clone()),
                    ),
                    conversation_registry: Arc::clone(&state.conversation_registry),
                    agent_role_registry: Arc::clone(&state.agent_role_registry),
                },
                std::env::temp_dir().join("magi-goal-resume-dispatcher"),
            )
            .with_model_bridge_client(Arc::new(RecordingFailingModelClient { calls }))
            .with_workspace_registry(workspace_store)
            .with_tool_registry(tool_registry.clone()),
        );
        let state_for_workers = state.clone();
        let runner_dispatcher: Arc<dyn TaskDispatcher> = dispatcher.clone();
        let runner_manager = crate::state::RunnerManager::with_dispatcher_and_worker_catalog(
            task_store,
            session_store,
            Arc::new(move || state_for_workers.task_worker_catalog()),
            runner_dispatcher,
        )
        .with_agent_role_registry(Arc::clone(&state.agent_role_registry));
        state
            .with_tool_registry(tool_registry)
            .with_runner_manager(runner_manager)
            .with_session_turn_dispatcher(dispatcher)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pausing_goal_does_not_cancel_unrelated_current_turn_or_shell() {
        use magi_core::{ExecutionResultStatus, ToolCallId};
        use magi_tool_runtime::{
            BuiltinToolName, ToolExecutionContext, ToolExecutionContextQuery, ToolExecutionInput,
            ToolExecutionPolicy,
        };
        let state = state_with_recording_goal_dispatcher(Arc::new(AtomicUsize::new(0)));
        let session = SessionId::new("goal-pause-unrelated-shell");
        state
            .session_store
            .create_session(session.clone(), "unrelated work")
            .unwrap();
        let goal = create_test_goal(
            &state,
            &session,
            "goal-original-owner",
            "preserve unrelated task",
        );
        crate::routes::test_turn_fixtures::seed_conversation_turn(
            &state.session_store,
            state.turn_coordinator(),
            &session,
            "unrelated-current-turn",
            1,
            UtcMillis::now(),
            "running",
            "unrelated task",
        );
        let registry = state.tool_registry().unwrap().clone();
        let runner_registry = registry.clone();
        let context_session = session.clone();
        let workspace = tempfile::tempdir().unwrap();
        let workdir = workspace.path().to_path_buf();
        let runner = std::thread::spawn(move || {
            runner_registry.execute_with_policy(
                ToolExecutionInput::for_builtin_invocation(
                    ToolCallId::new("goal-unrelated-shell"),
                    BuiltinToolName::ShellExec.as_str(),
                    serde_json::json!({"command":"sleep 1; printf survives", "timeout_ms":5000})
                        .to_string(),
                ),
                ToolExecutionContext {
                    session_id: Some(context_session),
                    workspace_id: Some(WorkspaceId::new("unrelated-workspace")),
                    working_directory: Some(workdir),
                    access_profile: AccessProfile::FullAccess,
                    ..Default::default()
                },
                &ToolExecutionPolicy {
                    access_profile: AccessProfile::FullAccess,
                    ..Default::default()
                },
            )
        });
        let query = ToolExecutionContextQuery {
            session_id: Some(session.clone()),
            ..Default::default()
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !registry.has_active_executions(&query) {
            if runner.is_finished() {
                panic!("真实 Shell 提前退出: {:?}", runner.join().unwrap());
            }
            assert!(std::time::Instant::now() < deadline, "真实 Shell 必须在途");
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let request = serde_json::from_value(serde_json::json!({"sessionId":session,"scope":"personal","goalId":goal.goal_id,"expectedRevision":goal.control_revision})).unwrap();
        crate::goal_service::pause_current_goal(&state, request)
            .await
            .unwrap();
        assert_eq!(
            state
                .session_store
                .runtime_sidecar(&session)
                .unwrap()
                .current_turn
                .unwrap()
                .status,
            "running"
        );
        assert!(registry.has_active_executions(&query));
        let result = runner.join().unwrap();
        assert_eq!(
            result.status,
            ExecutionResultStatus::Succeeded,
            "{result:?}"
        );
    }

    #[test]
    fn paused_goal_resume_action_remains_available_while_session_turn_is_running() {
        let state = state_with_recording_goal_dispatcher(Arc::new(AtomicUsize::new(0)));
        let session_id = SessionId::new("session-goal-resume-action-running");
        state
            .session_store
            .create_session(session_id.clone(), "goal resume action running")
            .expect("session should create");
        let goal = create_test_goal(
            &state,
            &session_id,
            "turn-goal-resume-action-owner",
            "验证运行中不展示 Goal 恢复操作",
        );
        let paused_goal = state
            .session_store
            .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, None)
            .expect("goal should pause")
            .0;
        assert!(
            allowed_actions(&state, &session_id, Some(&paused_goal), None).can_resume,
            "空闲会话里的暂停 Goal 应允许恢复"
        );
        let coordinator = state.turn_coordinator().clone();
        crate::routes::test_turn_fixtures::seed_conversation_turn(
            &state.session_store,
            &coordinator,
            &session_id,
            "turn-goal-resume-action-running",
            1,
            UtcMillis::now(),
            "running",
            "会话正在执行",
        );

        assert!(
            allowed_actions(&state, &session_id, Some(&paused_goal), None).can_resume,
            "已有运行 Turn 时恢复请求应进入 waiting，而不是禁用操作"
        );
    }

    #[tokio::test]
    async fn busy_session_accepts_goal_resume_as_waiting() {
        let state = state_with_recording_goal_dispatcher(Arc::new(AtomicUsize::new(0)));
        let workspace_id = WorkspaceId::new("workspace-goal-resume-waiting");
        let workspace_path = std::env::temp_dir().join("magi-goal-resume-waiting");
        std::fs::create_dir_all(&workspace_path).expect("workspace directory should create");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_path.display().to_string()),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-goal-resume-waiting");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "goal resume waiting",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let goal = create_test_goal(
            &state,
            &session_id,
            "turn-goal-resume-waiting-owner",
            "验证忙碌会话恢复等待",
        );
        let paused = state
            .session_store
            .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, None)
            .expect("goal should pause")
            .0;
        let coordinator = state.turn_coordinator().clone();
        crate::routes::test_turn_fixtures::seed_conversation_turn(
            &state.session_store,
            &coordinator,
            &session_id,
            "turn-unrelated-running",
            1,
            UtcMillis::now(),
            "running",
            "执行其他任务",
        );

        let app = Router::new().merge(routes()).with_state(state.clone());
        let response = app
            .oneshot(json_post(
                "/goals/current/resume",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "workspace",
                    "workspaceId": workspace_id.to_string(),
                    "goalId": goal.goal_id,
                    "expectedRevision": paused.control_revision,
                }),
            ))
            .await
            .expect("resume should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["goal"]["status"].as_str(), Some("active"));
        assert_eq!(
            payload["goal"]["continuation"]["phase"].as_str(),
            Some("waiting")
        );
        assert!(payload["goal"]["timingStartedAt"].is_null());
        assert!(
            state
                .session_store
                .canonical_turns_for_session(&session_id)
                .iter()
                .all(|turn| !turn.turn_id.starts_with("turn-goal-continuation-")),
            "busy resume must not fabricate a continuation turn"
        );
    }

    #[tokio::test]
    async fn current_goal_route_reads_session_goal_without_task_projection() {
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let workspace_id = WorkspaceId::new("workspace-goal-route");
        let workspace_path = std::env::temp_dir().join("magi-goal-route");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_path.display().to_string()),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-goal-route");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "goal route",
                Some(workspace_id.to_string()),
            )
            .expect("session should be creatable");
        let goal = create_test_goal(&state, &session_id, "turn-goal-route", "完成 Goal API");

        let app = Router::new().merge(routes()).with_state(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/goals/current?scope=workspace&sessionId={}&workspaceId={}",
                        session_id, workspace_id
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should complete");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("response should be json");
        assert_eq!(
            payload["goal"]["goalId"].as_str(),
            Some(goal.goal_id.as_str())
        );
        assert_eq!(payload["goal"]["objective"].as_str(), Some("完成 Goal API"));
        assert_eq!(payload["goal"]["status"].as_str(), Some("active"));
        assert!(payload["plan"].is_null());
    }

    #[tokio::test]
    async fn personal_goal_routes_use_personal_scope_without_workspace_binding() {
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let session_id = SessionId::new("session-personal-goal-routes");
        state
            .session_store
            .create_session(session_id.clone(), "个人目标会话")
            .expect("personal session should be creatable");
        let goal = create_test_goal(
            &state,
            &session_id,
            "turn-personal-goal-routes",
            "完成个人会话目标",
        );
        let app = Router::new().merge(routes()).with_state(state.clone());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/goals/current?scope=personal&sessionId={}",
                        session_id
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("personal goal should read");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert!(payload["workspaceId"].is_null());
        assert!(payload["workspacePath"].is_null());
        assert_eq!(
            payload["goal"]["goalId"].as_str(),
            Some(goal.goal_id.as_str())
        );

        let response = app
            .clone()
            .oneshot(json_post(
                "/goals/current/update",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "personal",
                    "goalId": goal.goal_id,
                    "expectedRevision": goal.control_revision,
                    "objective": "更新个人会话目标",
                }),
            ))
            .await
            .expect("personal goal should update");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert!(payload["workspaceId"].is_null());
        assert_eq!(
            payload["goal"]["objective"].as_str(),
            Some("更新个人会话目标")
        );
        let updated_revision = payload["goal"]["controlRevision"]
            .as_u64()
            .expect("updated revision should exist");

        let response = app
            .clone()
            .oneshot(json_post(
                "/goals/current/pause",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "personal",
                    "goalId": goal.goal_id,
                    "expectedRevision": updated_revision,
                }),
            ))
            .await
            .expect("personal goal should pause");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["goal"]["status"].as_str(), Some("paused"));
        let paused_revision = payload["goal"]["controlRevision"]
            .as_u64()
            .expect("paused revision should exist");

        let response = app
            .oneshot(json_post(
                "/goals/current/clear",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "personal",
                    "goalId": goal.goal_id,
                    "expectedRevision": paused_revision,
                }),
            ))
            .await
            .expect("personal goal should clear");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert!(payload["workspaceId"].is_null());
        assert!(payload["goal"].is_null());
    }

    #[tokio::test]
    async fn current_goal_route_returns_stable_revisioned_plan() {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let state = ApiState::new(
            "magi-test",
            event_bus,
            Arc::clone(&session_store),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let workspace_id = WorkspaceId::new("workspace-goal-plan-route");
        let workspace_path = std::env::temp_dir().join("magi-goal-plan-route");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_path.display().to_string()),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-goal-plan-route");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "goal plan route",
                Some(workspace_id.to_string()),
            )
            .expect("session should be creatable");
        let goal = create_test_goal(
            &state,
            &session_id,
            "turn-goal-plan-route",
            "完成稳定计划展示",
        );
        let plan_store = magi_plan::PlanStore::new(Arc::clone(&session_store), session_id.clone());
        let created = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: Some(goal.goal_id.to_string()),
                expected_goal_control_revision: Some(goal.control_revision),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![
                    magi_plan::UpdatePlanItemInput {
                        item_id: Some("inspect".to_string()),
                        step: "检查现状".to_string(),
                        status: PlanItemStatus::InProgress,
                    },
                    magi_plan::UpdatePlanItemInput {
                        item_id: Some("verify".to_string()),
                        step: "验证结果".to_string(),
                        status: PlanItemStatus::Pending,
                    },
                ],
            })
            .expect("plan should create");
        assert_eq!(created.goal_id.as_ref(), Some(&goal.goal_id));
        let updated = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: Some(created.plan_id.to_string()),
                expected_revision: Some(created.revision),
                expected_goal_id: Some(goal.goal_id.to_string()),
                expected_goal_control_revision: Some(goal.control_revision),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![
                    magi_plan::UpdatePlanItemInput {
                        item_id: Some("inspect".to_string()),
                        step: "检查现状".to_string(),
                        status: PlanItemStatus::Completed,
                    },
                    magi_plan::UpdatePlanItemInput {
                        item_id: Some("verify".to_string()),
                        step: "验证结果".to_string(),
                        status: PlanItemStatus::InProgress,
                    },
                ],
            })
            .expect("plan should update");

        let app = Router::new().merge(routes()).with_state(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/goals/current?scope=workspace&sessionId={}&workspaceId={}",
                        session_id, workspace_id
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(
            payload["plan"]["planId"].as_str(),
            Some(updated.plan_id.as_str())
        );
        assert_eq!(payload["plan"]["revision"].as_u64(), Some(updated.revision));
        let items = payload["plan"]["items"].as_array().expect("plan items");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["itemId"].as_str(), Some("inspect"));
        assert_eq!(items[0]["status"].as_str(), Some("completed"));
        assert_eq!(items[1]["itemId"].as_str(), Some("verify"));
        assert_eq!(items[1]["status"].as_str(), Some("in_progress"));
    }

    #[tokio::test]
    async fn current_goal_actions_edit_pause_and_reject_resume_without_runtime() {
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let workspace_id = WorkspaceId::new("workspace-goal-actions");
        let workspace_path = std::env::temp_dir().join("magi-goal-actions");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_path.display().to_string()),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-goal-actions");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "goal actions",
                Some(workspace_id.to_string()),
            )
            .expect("session should be creatable");
        let goal = create_test_goal(&state, &session_id, "turn-goal-actions", "原目标");
        let goal_id = goal.goal_id.to_string();
        let plan_store = magi_plan::PlanStore::new(state.session_store.clone(), session_id.clone());
        plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: Some(goal.goal_id.to_string()),
                expected_goal_control_revision: Some(goal.control_revision),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("execute".to_string()),
                    step: "执行当前目标".to_string(),
                    status: PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should be creatable");

        let app = Router::new().merge(routes()).with_state(state.clone());
        let update_body = serde_json::json!({
            "sessionId": session_id.to_string(),
            "scope": "workspace",
            "workspaceId": workspace_id.to_string(),
            "goalId": goal_id,
            "expectedRevision": goal.control_revision,
            "objective": "更新后的目标"
        });
        let response = app
            .clone()
            .oneshot(json_post("/goals/current/update", update_body))
            .await
            .expect("update should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["goal"]["objective"].as_str(), Some("更新后的目标"));
        let updated_revision = payload["goal"]["controlRevision"]
            .as_u64()
            .expect("updated goal revision");
        let pause_body = serde_json::json!({
            "sessionId": session_id.to_string(),
            "scope": "workspace",
            "workspaceId": workspace_id.to_string(),
            "goalId": goal_id,
            "expectedRevision": updated_revision,
            "expectedPlanRevision": 1,
        });
        let coordinator = state.turn_coordinator().clone();
        crate::routes::test_turn_fixtures::seed_conversation_turn(
            &state.session_store,
            &coordinator,
            &session_id,
            "turn-goal-actions",
            1,
            UtcMillis::now(),
            "running",
            "执行当前目标",
        );

        let response = app
            .clone()
            .oneshot(json_post("/goals/current/pause", pause_body))
            .await
            .expect("pause should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["goal"]["status"].as_str(), Some("paused"));
        assert_eq!(
            state
                .session_store
                .runtime_sidecar(&session_id)
                .and_then(|sidecar| sidecar.current_turn)
                .map(|turn| turn.status),
            Some("cancelled".to_string()),
            "手动暂停 Goal 必须同时终止所属运行 Turn"
        );
        let paused_revision = payload["goal"]["controlRevision"]
            .as_u64()
            .expect("paused goal revision");
        assert_eq!(
            plan_store.snapshot().expect("plan should exist").state,
            magi_core::PlanState::Paused
        );
        assert!(
            state
                .event_bus
                .snapshot()
                .recent_events
                .iter()
                .any(|event| {
                    event.event_type == "session.plan.paused"
                        && event.payload["session_id"].as_str() == Some(session_id.as_str())
                })
        );

        let response = app
            .clone()
            .oneshot(json_post(
                "/goals/current/resume",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "workspace",
                    "workspaceId": workspace_id.to_string(),
                    "goalId": goal_id,
                    "expectedRevision": paused_revision,
                    "expectedPlanRevision": 2,
                }),
            ))
            .await
            .expect("resume should complete");
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let payload = response_json(response).await;
        assert_eq!(payload["error_code"].as_str(), Some("CONFLICT"));
        assert_eq!(
            state
                .session_store
                .current_visible_goal(&session_id)
                .expect("goal should remain visible")
                .status,
            GoalStatus::Paused
        );
        assert_eq!(
            plan_store.snapshot().expect("plan should exist").state,
            magi_core::PlanState::Paused
        );
        assert!(
            state
                .session_store
                .canonical_turns_for_session(&session_id)
                .iter()
                .all(|turn| !turn.turn_id.starts_with("turn-goal-continuation-")),
            "执行器不可用时不能留下伪造的目标续跑 Turn"
        );

        let paused_plan_revision = plan_store
            .snapshot()
            .expect("paused plan should remain")
            .revision;
        let response = app
            .clone()
            .oneshot(json_post(
                "/goals/current/plan/clear",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "workspace",
                    "workspaceId": workspace_id.to_string(),
                    "goalId": goal_id,
                    "expectedRevision": paused_revision,
                    "expectedPlanRevision": paused_plan_revision,
                }),
            ))
            .await
            .expect("unfinished goal plan clear should complete");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(plan_store.snapshot().is_some());

        let paused_goal = state
            .session_store
            .current_visible_goal(&session_id)
            .expect("goal should remain available after rejected resume");
        let paused_goal_id = paused_goal.goal_id.to_string();
        let paused_plan = plan_store.snapshot().expect("paused plan should exist");
        let (active_goal, resumed_plan, _) = state
            .session_store
            .resume_goal_with_plan(
                &session_id,
                &paused_goal.goal_id,
                paused_goal.control_revision,
                Some(paused_plan.revision),
                None,
            )
            .expect("matching Goal and Plan revisions should permit activation");
        let resumed_plan = resumed_plan.expect("bound plan should resume");
        let completed_plan = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: Some(resumed_plan.plan_id.to_string()),
                expected_revision: Some(resumed_plan.revision),
                expected_goal_id: Some(active_goal.goal_id.to_string()),
                expected_goal_control_revision: Some(active_goal.control_revision),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("execute".to_string()),
                    step: "执行当前目标".to_string(),
                    status: PlanItemStatus::Completed,
                }],
            })
            .expect("plan should complete");
        let completed_goal = state
            .session_store
            .complete_goal(
                &session_id,
                &active_goal.goal_id,
                GoalRevisionExpectation::new(
                    active_goal.control_revision,
                    Some(completed_plan.revision),
                ),
                "turn-goal-actions",
                "目标已完成",
                vec!["test:goal-actions".to_string()],
            )
            .expect("goal should be markable complete");
        let response = app
            .clone()
            .oneshot(json_post(
                "/goals/current/resume",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "workspace",
                    "workspaceId": workspace_id.to_string(),
                    "goalId": paused_goal_id,
                    "expectedRevision": completed_goal.control_revision,
                    "expectedPlanRevision": completed_plan.revision,
                }),
            ))
            .await
            .expect("completed goal resume should complete");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = response_json(response).await;
        assert_eq!(payload["error_code"].as_str(), Some("INPUT_INVALID"));
        assert_eq!(
            payload["message"].as_str(),
            Some("只有暂停或阻塞目标可以继续执行")
        );
        assert!(
            state
                .session_store
                .canonical_turns_for_session(&session_id)
                .iter()
                .all(|turn| !turn.turn_id.starts_with("turn-goal-continuation-")),
            "已完成目标不能创建续跑 Turn"
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/goals/current?scope=workspace&sessionId={}&workspaceId={}",
                        session_id, workspace_id
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["goal"]["status"].as_str(), Some("complete"));
        let response = app
            .clone()
            .oneshot(json_post(
                "/goals/current/clear",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "workspace",
                    "workspaceId": workspace_id.to_string(),
                    "goalId": paused_goal_id,
                    "expectedRevision": completed_goal.control_revision,
                    "expectedPlanRevision": completed_plan.revision,
                }),
            ))
            .await
            .expect("clear should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert!(payload["goal"].is_null());
        assert!(state.session_store.plan(&session_id).is_none());
        assert!(
            state
                .event_bus
                .snapshot()
                .recent_events
                .iter()
                .any(|event| {
                    event.event_type == "session.plan.cleared"
                        && event.payload["session_id"].as_str() == Some(session_id.as_str())
                        && event.payload["plan"].is_null()
                })
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/goals/current?scope=workspace&sessionId={}&workspaceId={}",
                        session_id, workspace_id
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert!(payload["goal"].is_null());
        assert!(payload["plan"].is_null());
    }

    #[tokio::test]
    async fn resuming_paused_goal_starts_real_continuation_turn() {
        let model_calls = Arc::new(AtomicUsize::new(0));
        let state = state_with_recording_goal_dispatcher(Arc::clone(&model_calls));
        let workspace_id = WorkspaceId::new("workspace-goal-real-resume");
        let workspace_path = std::env::temp_dir().join("magi-goal-real-resume");
        std::fs::create_dir_all(&workspace_path).expect("workspace directory should create");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_path.display().to_string()),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-goal-real-resume");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "goal real resume",
                Some(workspace_id.to_string()),
            )
            .expect("session should be creatable");
        let goal = create_test_goal(
            &state,
            &session_id,
            "turn-goal-real-resume",
            "验证目标恢复会启动续跑任务",
        );
        let plan_store = magi_plan::PlanStore::new(state.session_store.clone(), session_id.clone());
        plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: Some(goal.goal_id.to_string()),
                expected_goal_control_revision: Some(goal.control_revision),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("resume".to_string()),
                    step: "恢复目标续跑".to_string(),
                    status: PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should be creatable");
        let goal = state
            .session_store
            .active_goal(&session_id)
            .expect("goal should be active initially");
        state
            .session_store
            .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, Some(1))
            .expect("goal and plan should pause");

        let app = Router::new().merge(routes()).with_state(state.clone());
        let response = app
            .oneshot(json_post(
                "/goals/current/resume",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "workspace",
                    "workspaceId": workspace_id.to_string(),
                    "goalId": goal.goal_id,
                    "expectedRevision": goal.control_revision + 1,
                    "expectedPlanRevision": 2,
                    "accessProfile": "full_access",
                }),
            ))
            .await
            .expect("resume should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["goal"]["status"].as_str(), Some("active"));
        assert_eq!(
            payload["goal"]["accessProfile"].as_str(),
            Some("full_access")
        );
        assert_eq!(
            plan_store.snapshot().expect("plan should exist").state,
            magi_core::PlanState::Active
        );
        assert!(
            state
                .session_store
                .canonical_turns_for_session(&session_id)
                .iter()
                .any(|turn| turn.turn_id.starts_with("turn-goal-continuation-")),
            "恢复后必须接受一轮目标续跑 Turn"
        );
        assert!(
            state
                .event_bus
                .snapshot()
                .recent_events
                .iter()
                .any(|event| {
                    event.event_type == "session.turn.task.accepted"
                        && event.payload["session_id"].as_str() == Some(session_id.as_str())
                        && event.payload["goal_continuation"] == true
                }),
            "恢复后必须发布 Turn 已接受事件"
        );
        for _ in 0..20 {
            if model_calls.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            model_calls.load(Ordering::SeqCst) > 0,
            "恢复目标后执行器必须实际发起模型调用"
        );
    }

    #[tokio::test]
    async fn personal_goal_resume_starts_continuation_without_workspace_binding() {
        let model_calls = Arc::new(AtomicUsize::new(0));
        let state = state_with_recording_goal_dispatcher(Arc::clone(&model_calls));
        let session_id = SessionId::new("session-personal-goal-resume");
        state
            .session_store
            .create_session(session_id.clone(), "个人目标恢复")
            .expect("personal session should be creatable");
        let goal = create_test_goal(
            &state,
            &session_id,
            "turn-personal-goal-resume",
            "验证个人目标恢复",
        );
        let paused = state
            .session_store
            .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, None)
            .expect("personal goal should pause")
            .0;

        let app = Router::new().merge(routes()).with_state(state.clone());
        let response = app
            .oneshot(json_post(
                "/goals/current/resume",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "personal",
                    "goalId": goal.goal_id,
                    "expectedRevision": paused.control_revision,
                    "accessProfile": "full_access",
                }),
            ))
            .await
            .expect("personal goal resume should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert!(payload["workspaceId"].is_null());
        assert!(payload["workspacePath"].is_null());
        assert_eq!(payload["goal"]["status"].as_str(), Some("active"));
        for _ in 0..20 {
            if model_calls.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(model_calls.load(Ordering::SeqCst) > 0);
        assert!(
            state
                .event_bus
                .snapshot()
                .recent_events
                .iter()
                .any(|event| {
                    event.event_type == "session.turn.task.accepted"
                        && event.session_id.as_ref() == Some(&session_id)
                        && event.workspace_id.is_none()
                })
        );
    }

    #[tokio::test]
    async fn completed_plan_can_be_cleared_without_removing_goal() {
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let workspace_id = WorkspaceId::new("workspace-goal-plan-clear");
        let workspace_path = std::env::temp_dir().join("magi-goal-plan-clear");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_path.display().to_string()),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-goal-plan-clear");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "goal plan clear",
                Some(workspace_id.to_string()),
            )
            .expect("session should be creatable");
        let goal = create_test_goal(
            &state,
            &session_id,
            "turn-goal-plan-clear",
            "保留目标，仅清除计划",
        );
        let plan_store = magi_plan::PlanStore::new(state.session_store.clone(), session_id.clone());
        let created = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: Some(goal.goal_id.to_string()),
                expected_goal_control_revision: Some(goal.control_revision),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("done".to_string()),
                    step: "已完成任务".to_string(),
                    status: PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should create");
        let completed_plan = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: Some(created.plan_id.to_string()),
                expected_revision: Some(created.revision),
                expected_goal_id: Some(goal.goal_id.to_string()),
                expected_goal_control_revision: Some(goal.control_revision),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("done".to_string()),
                    step: "已完成任务".to_string(),
                    status: PlanItemStatus::Completed,
                }],
            })
            .expect("plan should complete");
        let completed_goal = state
            .session_store
            .complete_goal(
                &session_id,
                &goal.goal_id,
                GoalRevisionExpectation::new(goal.control_revision, Some(completed_plan.revision)),
                "turn-goal-plan-clear",
                "计划已完成",
                vec!["test:completed-plan-clear".to_string()],
            )
            .expect("goal should complete");

        let app = Router::new().merge(routes()).with_state(state.clone());
        let response = app
            .oneshot(json_post(
                "/goals/current/plan/clear",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "workspace",
                    "workspaceId": workspace_id.to_string(),
                    "goalId": goal.goal_id,
                    "expectedRevision": completed_goal.control_revision,
                    "expectedPlanRevision": completed_plan.revision,
                }),
            ))
            .await
            .expect("plan clear should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(
            payload["goal"]["goalId"].as_str(),
            Some(completed_goal.goal_id.as_str())
        );
        assert_eq!(payload["goal"]["status"].as_str(), Some("complete"));
        assert!(payload["plan"].is_null());
        assert!(state.session_store.plan(&session_id).is_none());
    }

    fn json_post(path: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(path)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("request should build")
    }

    async fn response_json(response: axum::response::Response) -> serde_json::Value {
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        serde_json::from_slice(&body).expect("response should be json")
    }
}
