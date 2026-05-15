#[cfg(test)]
use crate::dto::SessionTurnRequestDto;
use crate::{RunnerStartError, errors::ApiError, state::ApiState};
use magi_conversation_runtime::dispatch_submission::{
    DispatchSubmissionAcceptError, accept_dispatch_submission,
    ensure_dispatch_submission_acceptance_available,
};
pub(crate) use magi_conversation_runtime::dispatch_submission::{
    DispatchSubmissionAccepted, DispatchSubmissionRequest,
};
use magi_core::{SessionId, Task, TaskId, WorkspaceId};
#[cfg(test)]
use magi_core::{TaskKind, TaskStatus, UtcMillis};

// M12/M13/M14：任务图构建器、任务分解管线、任务图重规划入口与派发数据载体已
// 迁移到 magi-conversation-runtime；保留内部 re-export 让 dispatch_execution / 测试
// 代码以原名继续调用。
#[cfg(test)]
pub(crate) use magi_conversation_runtime::mission_decomposition::{
    TASK_PLAN_TOOL_NAME, parse_decomposition_response,
};
use magi_conversation_runtime::task_graph_builder::TaskGraphSubmission;
#[cfg(test)]
use magi_conversation_runtime::task_graph_builder::{build_task_policy, make_dispatch_task};
pub(crate) use magi_conversation_runtime::task_graph_replan::{
    TaskGraphReplanError, TaskGraphReplanResult,
};
pub(crate) fn run_dispatch_submission(
    state: &ApiState,
    request: &DispatchSubmissionRequest,
) -> Result<TaskGraphSubmission, ApiError> {
    let _ = state
        .execution_pipeline()
        .ok_or_else(|| ApiError::internal_assembly("任务派发失败", "execution pipeline 未配置"))?;
    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("任务派发失败", "task_store 未配置"))?;
    let workspace_root_path = state.workspace_root_path(&request.workspace_id);

    magi_conversation_runtime::dispatch_submission::run_dispatch_submission(
        &magi_conversation_runtime::dispatch_submission::DispatchSubmissionRuntime {
            session_store: state.session_store.as_ref(),
            task_store,
            execution_registry: state.task_execution_registry(),
            event_bus: &state.event_bus,
            agent_role_registry: &state.agent_role_registry,
            spawn_graph: state.spawn_graph.as_ref(),
            model_bridge_client: state.model_bridge_client(),
            workspace_root_path: workspace_root_path.as_deref(),
        },
        request,
    )
    .map_err(|err| match err {
        magi_conversation_runtime::dispatch_submission::DispatchSubmissionRunError::InvalidInput(msg) => {
            ApiError::InvalidInput(msg)
        }
        magi_conversation_runtime::dispatch_submission::DispatchSubmissionRunError::Internal(msg) => {
            ApiError::internal_assembly("任务派发失败", msg)
        }
    })
}

fn submit_task_submission(
    state: &ApiState,
    request: DispatchSubmissionRequest,
) -> Result<DispatchSubmissionAccepted, ApiError> {
    ensure_dispatch_submission_acceptance_available(state.session_store.as_ref(), &request)
        .map_err(map_dispatch_submission_accept_error)?;
    let graph = run_dispatch_submission(state, &request)?;
    accept_dispatch_submission(
        state.session_store.as_ref(),
        state.task_store(),
        state.task_execution_registry(),
        request,
        graph,
    )
    .map_err(map_dispatch_submission_accept_error)
}

fn map_dispatch_submission_accept_error(error: DispatchSubmissionAcceptError) -> ApiError {
    match error {
        DispatchSubmissionAcceptError::Conflict { message } => {
            ApiError::conflict("任务派发失败", &message)
        }
        DispatchSubmissionAcceptError::Internal { message } => {
            ApiError::internal_assembly("任务派发失败", message)
        }
    }
}

pub(crate) fn submit_dispatch_submission(
    state: &ApiState,
    request: DispatchSubmissionRequest,
) -> Result<DispatchSubmissionAccepted, ApiError> {
    submit_task_submission(state, request)
}

pub(crate) fn drive_dispatch_submission(
    state: &ApiState,
    accepted: &mut DispatchSubmissionAccepted,
) -> Result<(), ApiError> {
    let manager = state
        .runner_manager()
        .ok_or_else(|| ApiError::internal_assembly("任务派发失败", "runner_manager 未配置"))?;
    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("任务派发失败", "task_store 未配置"))?;

    let root_task = task_store
        .get_task(&accepted.root_task_id)
        .ok_or_else(|| ApiError::internal_assembly("任务派发失败", "root task 不存在"))?;
    let background_allowed = root_task
        .policy_snapshot
        .as_ref()
        .map(|policy| policy.background_allowed)
        .unwrap_or(false);

    if background_allowed {
        match manager.start(
            accepted.root_task_id.as_str(),
            Some(accepted.session_id.clone()),
        ) {
            Ok(_) | Err(RunnerStartError::AlreadyRunning) => {
                accepted.runner_started = true;
                Ok(())
            }
            Err(RunnerStartError::NotFound) => Err(ApiError::internal_assembly(
                "任务派发失败",
                "root task 不存在",
            )),
        }
    } else {
        let execution = crate::a_path::drive_a_path(
            state,
            &accepted.root_task_id,
            &accepted.action_task_id,
            "任务派发失败",
        )?;
        accepted.runner_started = execution.runner_started;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn run_session_action(
    state: &ApiState,
    request: &SessionTurnRequestDto,
    accepted_at: UtcMillis,
    session_id: &SessionId,
    entry_id: &str,
    trimmed_text: Option<&str>,
) -> Result<TaskGraphSubmission, ApiError> {
    let mission_title = request.mission_title(trimmed_text);
    run_dispatch_submission(
        state,
        &DispatchSubmissionRequest {
            accepted_at,
            session_id: session_id.clone(),
            workspace_id: request
                .requested_workspace_id()
                .map(magi_core::WorkspaceId::new),
            entry_id: entry_id.to_string(),
            timeline_message: request.timeline_message(trimmed_text),
            created_session: false,
            mission_title: mission_title.clone(),
            task_title: format!("执行: {mission_title}"),
            trimmed_text: trimmed_text.map(str::to_string),
            execution_goal: trimmed_text.map(str::to_string),
            skill_name: request.skill_name.clone(),
            target_role: None,
            request_id: request.request_id(),
            user_message_id: request.user_message_id(),
            placeholder_message_id: request.placeholder_message_id(),
        },
    )
}

pub(crate) fn replan_task_graph(
    state: &ApiState,
    root_task_id: &TaskId,
    prompt: &str,
    context_task: Option<&Task>,
    workspace_id: &Option<WorkspaceId>,
    reason: &str,
) -> Result<TaskGraphReplanResult, ApiError> {
    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("重规划任务图失败", "task_store 未配置"))?;
    let workspace_root_path = state.workspace_root_path(workspace_id);
    magi_conversation_runtime::task_graph_replan::replan_task_graph(
        task_store,
        &state.agent_role_registry,
        state.spawn_graph.as_ref(),
        &state.event_bus,
        state.model_bridge_client(),
        workspace_root_path.as_deref(),
        root_task_id,
        prompt,
        context_task,
        reason,
    )
    .map_err(|err| match err {
        TaskGraphReplanError::InvalidInput(msg) => ApiError::InvalidInput(msg),
        TaskGraphReplanError::Internal(msg) => ApiError::internal_assembly("重规划任务图失败", msg),
    })
}

pub(crate) fn ensure_session_active_execution_chain(
    state: &ApiState,
    session_id: &SessionId,
) -> Result<(), ApiError> {
    magi_conversation_runtime::task_graph_replan::ensure_session_active_execution_chain(
        &state.session_store,
        session_id,
    )
    .map_err(|err| match err {
        TaskGraphReplanError::InvalidInput(msg) => ApiError::InvalidInput(msg),
        TaskGraphReplanError::Internal(msg) => {
            ApiError::internal_assembly("活跃执行链校验失败", msg)
        }
    })
}

pub(crate) fn replace_replanned_task_execution_branches(
    state: &ApiState,
    session_id: &SessionId,
    primary_action_task_id: &TaskId,
    leaf_action_task_ids: &[TaskId],
) -> Result<(), ApiError> {
    magi_conversation_runtime::dispatch_submission::replace_replanned_task_execution_branches(
        &state.session_store,
        state
            .task_store()
            .ok_or_else(|| ApiError::internal_assembly("注册任务执行分支失败", "task_store 未配置"))?,
        state.task_execution_registry(),
        session_id,
        primary_action_task_id,
        leaf_action_task_ids,
    )
    .map_err(|err| match err {
        magi_conversation_runtime::dispatch_submission::DispatchSubmissionRunError::InvalidInput(msg) => {
            ApiError::InvalidInput(msg)
        }
        magi_conversation_runtime::dispatch_submission::DispatchSubmissionRunError::Internal(msg) => {
            ApiError::internal_assembly("注册任务执行分支失败", msg)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ApiState;
    use magi_bridge_client::{
        BridgeClientError, BridgeErrorLayer, BridgeResponse, ModelBridgeClient,
        ModelInvocationRequest, ModelStreamingDelta,
    };
    use magi_core::{AbsolutePath, SessionId, WorkspaceId};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_memory_store::MemoryStore;
    use magi_orchestrator::{OrchestratorService, task_store::TaskStore};
    use magi_session_store::SessionStore;
    use magi_skill_runtime::SkillDispatchRuntime;
    use magi_tool_runtime::ToolRegistry;
    use magi_worker_runtime::WorkerRuntime;
    use magi_workspace::WorkspaceStore;
    use std::sync::{Arc, Mutex};

    use crate::dto::SessionTurnRequestDto;

    struct StaticDeepPlanModelBridgeClient;
    struct PathSensitiveDeepPlanModelBridgeClient;
    struct SequentialBatchDeepPlanModelBridgeClient;

    fn static_deep_plan_payload() -> String {
        task_plan_response(serde_json::json!({
            "phases": [
                {
                    "title": "规划",
                    "workPackages": [
                        {
                            "title": "规划工作包",
                            "actions": [
                                {
                                    "title": "梳理目标",
                                    "goal": "明确目标、边界和验收标准"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "执行",
                    "workPackages": [
                        {
                            "title": "执行工作包",
                            "actions": [
                                {
                                    "title": "执行任务",
                                    "goal": "完成用户目标"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "交付",
                    "workPackages": [
                        {
                            "title": "交付工作包",
                            "actions": [
                                {
                                    "title": "汇总结果",
                                    "goal": "基于执行结果和验证证据汇总结论"
                                }
                            ]
                        }
                    ]
                }
            ]
        }))
    }

    fn path_sensitive_deep_plan_payload() -> String {
        task_plan_response(serde_json::json!({
            "phases": [
                {
                    "title": "规划",
                    "workPackages": [
                        {
                            "title": "规划工作包",
                            "actions": [
                                {
                                    "title": "梳理目标",
                                    "goal": "梳理 /Users/xie/code/TEST 工作区的目标和验收标准"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "执行",
                    "workPackages": [
                        {
                            "title": "执行工作包",
                            "actions": [
                                {
                                    "title": "执行任务",
                                    "goal": "完成 /Users/xie/code/TEST 的只读检查"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "交付",
                    "workPackages": [
                        {
                            "title": "交付工作包",
                            "actions": [
                                {
                                    "title": "汇总结果",
                                    "goal": "基于 /Users/xie/code/TEST 的只读任务执行结果汇总结论"
                                }
                            ]
                        }
                    ]
                }
            ]
        }))
    }

    fn sequential_batch_deep_plan_payload() -> String {
        task_plan_response(serde_json::json!({
            "phases": [
                {
                    "title": "规划",
                    "workPackages": [
                        {
                            "title": "规划工作包",
                            "actions": [
                                {
                                    "title": "梳理目标",
                                    "goal": "梳理两批纵向任务的目标、边界和验收标准"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "第一批执行",
                    "workPackages": [
                        {
                            "title": "第一批工作包",
                            "actions": [
                                {
                                    "title": "执行第一批",
                                    "goal": "用 shell_exec 执行 printf BATCH_ONE_DONE_NEXT_BATCH_TEST，并根据 NEXT_BATCH 结果确认是否继续第二批"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "第二批执行",
                    "workPackages": [
                        {
                            "title": "第二批工作包",
                            "actions": [
                                {
                                    "title": "执行第二批",
                                    "goal": "在第一批完成并发现 NEXT_BATCH 后，用 shell_exec 执行 printf BATCH_TWO_DONE_FINAL_TEST"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "交付",
                    "workPackages": [
                        {
                            "title": "交付工作包",
                            "actions": [
                                {
                                    "title": "汇总两批结果",
                                    "goal": "只基于前两批执行产出总结，不重复调用工具"
                                }
                            ]
                        }
                    ]
                }
            ]
        }))
    }

    impl ModelBridgeClient for StaticDeepPlanModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            Ok(BridgeResponse {
                ok: true,
                payload: static_deep_plan_payload(),
            })
        }

        fn invoke_streaming(
            &self,
            _request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: None,
                message: "static deep plan client does not stream".to_string(),
            })
        }
    }

    impl ModelBridgeClient for PathSensitiveDeepPlanModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            Ok(BridgeResponse {
                ok: true,
                payload: path_sensitive_deep_plan_payload(),
            })
        }

        fn invoke_streaming(
            &self,
            _request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: None,
                message: "path-sensitive deep plan client does not stream".to_string(),
            })
        }
    }

    impl ModelBridgeClient for SequentialBatchDeepPlanModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            Ok(BridgeResponse {
                ok: true,
                payload: sequential_batch_deep_plan_payload(),
            })
        }

        fn invoke_streaming(
            &self,
            _request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: None,
                message: "sequential batch deep plan client does not stream".to_string(),
            })
        }
    }

    struct RecordingDeepPlanModelBridgeClient {
        prompt: Arc<Mutex<Option<String>>>,
    }

    impl ModelBridgeClient for RecordingDeepPlanModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            *self.prompt.lock().expect("prompt lock poisoned") = Some(request.prompt);
            Ok(BridgeResponse {
                ok: true,
                payload: static_deep_plan_payload(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            self.invoke(request)
        }
    }

    /// Build a minimal ApiState with a execution pipeline and task store.
    fn build_test_state() -> (ApiState, Arc<TaskStore>) {
        let event_bus = Arc::new(InMemoryEventBus::new(64));
        let governance = Arc::new(GovernanceService::default());
        let orchestrator = OrchestratorService::new(Arc::clone(&event_bus));
        let session_store = Arc::new(SessionStore::new());
        let workspace_store = Arc::new(WorkspaceStore::new());
        let memory_store = MemoryStore::new();

        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        let execution_runtime = orchestrator.execution_runtime(
            WorkerRuntime::new_compare(Arc::clone(&event_bus)),
            tool_registry.clone(),
            SkillDispatchRuntime::new(
                tool_registry,
                magi_bridge_client::BridgeDispatchRuntime::new(),
            ),
        );

        let task_store = Arc::new(TaskStore::new());

        let state = ApiState::new(
            "test-task",
            Arc::clone(&event_bus),
            Arc::clone(&session_store),
            Arc::clone(&workspace_store),
            governance,
        )
        .with_execution_pipeline(orchestrator, execution_runtime, memory_store)
        .with_task_store(Arc::clone(&task_store))
        .with_model_bridge_client(Arc::new(StaticDeepPlanModelBridgeClient));

        (state, task_store)
    }

    // -----------------------------------------------------------------------
    // Test 1: Session action creates Task Graph entries when TaskStore is configured
    // -----------------------------------------------------------------------

    #[test]
    fn session_action_creates_task_graph_entries() {
        let (state, task_store) = build_test_state();

        let session_id = SessionId::new("session-task-graph-test");
        state
            .session_store
            .create_session(session_id.clone(), "test session")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let request = SessionTurnRequestDto {
            session_id: None,
            workspace_id: None,
            workspace_path: None,
            text: Some("Hello world".to_string()),
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            images: Vec::new(),
            supplement_context: false,
            context_task_id: None,
        };

        let result = run_session_action(
            &state,
            &request,
            accepted_at,
            &session_id,
            "entry-1",
            Some("Hello world"),
        )
        .expect("loopback session action should create task graph");

        let obj_task_id = result.root_task_id;
        let obj = task_store
            .get_task(&obj_task_id)
            .expect("Objective task should exist in TaskStore");
        assert_eq!(obj.kind, TaskKind::Objective);
        assert_eq!(obj.goal, "Hello world");
        assert_eq!(obj.status, TaskStatus::Running);

        let act_task_id = result.action_task_id;
        let act = task_store
            .get_task(&act_task_id)
            .expect("primary Action task should exist in TaskStore");
        assert_eq!(act.kind, TaskKind::Action);
        assert_eq!(act.root_task_id, obj_task_id.clone());
        assert_eq!(act.status, TaskStatus::Ready);

        let children = task_store.get_children(&obj_task_id);
        assert!(
            !children.is_empty(),
            "Objective should have at least one Phase child"
        );
        assert!(
            children.iter().all(|child| child.kind == TaskKind::Phase),
            "Objective children must all be Phase nodes"
        );
    }

    #[test]
    fn dispatch_execution_goal_does_not_rewrite_turn_user_message() {
        let (state, task_store) = build_test_state();
        let session_id = SessionId::new("session-execution-goal-split");
        state
            .session_store
            .create_session(session_id.clone(), "execution goal split")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let submission = run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at,
                session_id: session_id.clone(),
                workspace_id: None,
                entry_id: "entry-execution-goal-split".to_string(),
                timeline_message: "用户原始任务描述".to_string(),
                created_session: false,
                mission_title: "模型判定任务".to_string(),
                task_title: "执行: 模型判定任务".to_string(),
                trimmed_text: Some("用户原始任务描述".to_string()),
                execution_goal: Some("模型结构化执行目标".to_string()),
                skill_name: None,
                target_role: None,
                request_id: Some("request-loopback-alias".to_string()),
                user_message_id: Some("user-loopback-alias".to_string()),
                placeholder_message_id: Some("assistant-placeholder-loopback-alias".to_string()),
            },
        )
        .expect("dispatch should create task graph");

        let objective = task_store
            .get_task(&submission.root_task_id)
            .expect("objective task should exist");
        assert_eq!(objective.goal, "模型结构化执行目标");

        let turn = submission
            .active_execution_chain
            .as_ref()
            .and_then(|chain| chain.current_turn.as_ref())
            .expect("current turn should exist");
        assert_eq!(turn.user_message.as_deref(), Some("用户原始任务描述"));
        let user_item = turn
            .items
            .first()
            .expect("turn 必须包含 canonical 用户消息");
        assert_eq!(user_item.item_id, "user-loopback-alias");
        assert_eq!(user_item.kind, "user_message");
        assert_eq!(user_item.content.as_deref(), Some("用户原始任务描述"));
        assert_eq!(
            state
                .session_store
                .resolve_thread_visibility(&session_id, &user_item.source_thread_id),
            Some(magi_session_store::ThreadVisibility::Main)
        );
        assert_eq!(
            user_item.request_id.as_deref(),
            Some("request-loopback-alias")
        );
        assert_eq!(
            user_item.user_message_id.as_deref(),
            Some("user-loopback-alias")
        );
        assert_eq!(
            user_item.placeholder_message_id.as_deref(),
            Some("assistant-placeholder-loopback-alias")
        );
    }

    #[test]
    fn task_turn_keeps_user_message_on_mainline_and_routes_workers_to_drawer() {
        let (state, _task_store) = build_test_state();
        let state = state.with_model_bridge_client(Arc::new(StaticDeepPlanModelBridgeClient));
        let session_id = SessionId::new("session-deep-mainline-dialog");
        state
            .session_store
            .create_session(session_id.clone(), "deep mainline dialog")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let submission = run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at,
                session_id: session_id.clone(),
                workspace_id: None,
                entry_id: "entry-deep-mainline-dialog".to_string(),
                timeline_message: "用户原始深度任务".to_string(),
                created_session: false,
                mission_title: "深度任务主线".to_string(),
                task_title: "执行: 深度任务主线".to_string(),
                trimmed_text: Some("用户原始深度任务".to_string()),
                execution_goal: Some("深度任务执行目标".to_string()),
                skill_name: None,
                target_role: None,
                request_id: Some("request-deep-mainline".to_string()),
                user_message_id: Some("user-deep-mainline".to_string()),
                placeholder_message_id: Some("assistant-deep-mainline".to_string()),
            },
        )
        .expect("deep dispatch should create task graph");

        let turn = submission
            .active_execution_chain
            .as_ref()
            .and_then(|chain| chain.current_turn.as_ref())
            .expect("current turn should exist");
        let visible_items = turn
            .items
            .iter()
            .filter(|item| {
                state
                    .session_store
                    .resolve_thread_visibility(&session_id, &item.source_thread_id)
                    == Some(magi_session_store::ThreadVisibility::Main)
            })
            .map(|item| {
                (
                    item.kind.as_str(),
                    item.content.as_deref().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();

        assert!(
            visible_items
                .iter()
                .any(|(kind, content)| *kind == "user_message" && *content == "用户原始深度任务"),
            "深度任务主线必须像正常对话一样保留用户消息"
        );
        assert!(
            !visible_items
                .iter()
                .any(|(kind, _)| *kind == "assistant_phase"),
            "P7：后端不再生成 phase 文本，主线不得出现 assistant_phase item"
        );
        assert!(
            turn.items.iter().any(|item| {
                item.kind == "worker_spawned"
                    && matches!(
                        state
                            .session_store
                            .resolve_thread_visibility(&session_id, &item.source_thread_id),
                        Some(magi_session_store::ThreadVisibility::WorkerDrawer { .. })
                    )
            }),
            "任务分配本身仍应通过 worker_dispatch 卡片展示，不作为普通主线文本重复出现"
        );
    }

    #[test]
    fn task_requires_non_empty_execution_goal() {
        let (state, task_store) = build_test_state();
        let session_id = SessionId::new("session-deep-empty-goal");
        state
            .session_store
            .create_session(session_id.clone(), "deep empty goal")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let result = run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at,
                session_id,
                workspace_id: None,
                entry_id: "entry-deep-empty-goal".to_string(),
                timeline_message: "用户原始深度任务".to_string(),
                created_session: false,
                mission_title: "深度任务".to_string(),
                task_title: "执行: 深度任务".to_string(),
                trimmed_text: Some("用户原始深度任务".to_string()),
                execution_goal: Some("   ".to_string()),
                skill_name: None,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
            },
        );

        assert!(
            matches!(result, Err(ApiError::InvalidInput(message)) if message.contains("execution_goal")),
            "深度任务空 execution_goal 必须直接拒绝"
        );
        assert!(
            task_store
                .get_task(&TaskId::new(format!("task-obj-{}", accepted_at.0)))
                .is_none(),
            "拒绝的深度任务不能留下半截任务图"
        );
    }

    #[test]
    fn task_registers_validation_execution_plans() {
        let (state, task_store) = build_test_state();
        let state = state.with_model_bridge_client(Arc::new(StaticDeepPlanModelBridgeClient));
        let session_id = SessionId::new("session-deep-validation-plans");
        state
            .session_store
            .create_session(session_id.clone(), "deep validation plans")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let submission = run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at,
                session_id,
                workspace_id: None,
                entry_id: "entry-deep-validation-plans".to_string(),
                timeline_message: "执行深度验证计划".to_string(),
                created_session: false,
                mission_title: "深度验证计划".to_string(),
                task_title: "执行: 深度验证计划".to_string(),
                trimmed_text: Some("执行深度验证计划".to_string()),
                execution_goal: Some("执行深度验证计划".to_string()),
                skill_name: None,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
            },
        )
        .expect("deep task should create graph");

        let validation_ids = task_store
            .collect_subtree_ids(&submission.root_task_id)
            .into_iter()
            .filter(|task_id| {
                task_store
                    .get_task(task_id)
                    .is_some_and(|task| task.kind == TaskKind::Validation)
            })
            .collect::<Vec<_>>();
        assert_eq!(validation_ids.len(), 3);
        for validation_id in validation_ids {
            assert!(
                state
                    .task_execution_registry()
                    .remove(&validation_id)
                    .is_some(),
                "validation task {validation_id} should have a registered execution plan"
            );
        }
    }

    #[test]
    fn task_policies_are_unified_across_all_nodes() {
        let (state, task_store) = build_test_state();
        let state = state.with_model_bridge_client(Arc::new(StaticDeepPlanModelBridgeClient));
        let session_id = SessionId::new("session-task-unified-policy");
        state
            .session_store
            .create_session(session_id.clone(), "task unified policy")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let submission = run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at,
                session_id,
                workspace_id: None,
                entry_id: "entry-task-unified-policy".to_string(),
                timeline_message: "执行统一 policy 验收".to_string(),
                created_session: false,
                mission_title: "统一 policy 验收".to_string(),
                task_title: "执行: 统一 policy 验收".to_string(),
                trimmed_text: Some("执行统一 policy 验收".to_string()),
                execution_goal: Some("执行统一 policy 验收".to_string()),
                skill_name: None,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
            },
        )
        .expect("task should create graph");

        let tasks = task_store
            .collect_subtree_ids(&submission.root_task_id)
            .into_iter()
            .filter_map(|task_id| task_store.get_task(&task_id))
            .collect::<Vec<_>>();

        let unified = build_task_policy();
        for task in tasks.iter().filter(|task| task.kind != TaskKind::Objective) {
            let policy = task
                .policy_snapshot
                .as_ref()
                .unwrap_or_else(|| panic!("{:?} task missing policy", task.kind));
            assert_eq!(
                policy.autonomy_level, unified.autonomy_level,
                "{:?} 节点 autonomy_level 必须与统一 policy 一致",
                task.kind
            );
            assert_eq!(
                policy.command_mode, unified.command_mode,
                "{:?} 节点 command_mode 必须与统一 policy 一致",
                task.kind
            );
            assert_eq!(
                policy.retry_limit, unified.retry_limit,
                "{:?} 节点 retry_limit 必须与统一 policy 一致",
                task.kind
            );
            assert_eq!(
                policy.repair_limit, unified.repair_limit,
                "{:?} 节点 repair_limit 必须与统一 policy 一致",
                task.kind
            );
            assert_eq!(
                policy.validation_profile, unified.validation_profile,
                "{:?} 节点 validation_profile 必须与统一 policy 一致",
                task.kind
            );
            assert!(
                policy.allowed_tools.is_empty(),
                "统一 policy 不应在引擎层裁剪工具：{:?} 节点 allowed_tools={:?}",
                task.kind,
                policy.allowed_tools
            );
            assert!(
                policy.denied_tools.is_empty(),
                "统一 policy 不应在引擎层裁剪工具：{:?} 节点 denied_tools={:?}",
                task.kind,
                policy.denied_tools
            );
        }

        let execution_action_id = tasks
            .iter()
            .find(|task| task.kind == TaskKind::Action && task.title == "执行任务")
            .map(|task| task.task_id.clone())
            .expect("execution action should exist");
        let delivery_action = tasks
            .iter()
            .find(|task| task.kind == TaskKind::Action && task.title == "汇总结果")
            .expect("delivery action should exist");
        assert!(
            delivery_action
                .dependency_ids
                .iter()
                .any(|dependency_id| dependency_id == &execution_action_id),
            "交付 action 必须基于执行 action 的产出，而不是重新执行用户目标"
        );

        let planning_validation = tasks
            .iter()
            .find(|task| task.kind == TaskKind::Validation && task.title == "规划 验证")
            .expect("planning validation should exist");
        assert!(
            planning_validation.goal.contains("只验证规划文本完整性"),
            "规划验证 goal 必须保留 LLM 给出的规划阶段验收语义"
        );
    }

    #[test]
    fn task_graph_supports_sequential_execution_batches() {
        let (state, task_store) = build_test_state();
        let state =
            state.with_model_bridge_client(Arc::new(SequentialBatchDeepPlanModelBridgeClient));
        let session_id = SessionId::new("session-deep-sequential-batches");
        state
            .session_store
            .create_session(session_id.clone(), "deep sequential batches")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let submission = run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at,
                session_id,
                workspace_id: None,
                entry_id: "entry-deep-sequential-batches".to_string(),
                timeline_message: "执行多段任务".to_string(),
                created_session: false,
                mission_title: "多段任务".to_string(),
                task_title: "执行: 多段任务".to_string(),
                trimmed_text: Some("执行多段任务".to_string()),
                execution_goal: Some("执行多段任务".to_string()),
                skill_name: None,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
            },
        )
        .expect("sequential deep task should create graph");

        let tasks = task_store
            .collect_subtree_ids(&submission.root_task_id)
            .into_iter()
            .filter_map(|task_id| task_store.get_task(&task_id))
            .collect::<Vec<_>>();
        let phases = tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Phase)
            .collect::<Vec<_>>();
        assert_eq!(phases.len(), 4, "两批执行应成为独立 phase");
        let first_batch_phase = phases
            .iter()
            .find(|task| task.title == "第一批执行")
            .expect("first batch phase should exist");
        let second_batch_phase = phases
            .iter()
            .find(|task| task.title == "第二批执行")
            .expect("second batch phase should exist");
        assert!(
            second_batch_phase
                .dependency_ids
                .iter()
                .any(|dependency_id| dependency_id == &first_batch_phase.task_id),
            "第二批 phase 必须依赖第一批 phase 完成"
        );

        let unified = build_task_policy();
        let policy_for_action = |title: &str| {
            tasks
                .iter()
                .find(|task| task.kind == TaskKind::Action && task.title == title)
                .and_then(|task| task.policy_snapshot.as_ref())
                .expect("action policy should exist")
        };
        for title in ["执行第一批", "执行第二批", "汇总两批结果"] {
            let policy = policy_for_action(title);
            assert_eq!(
                policy.command_mode, unified.command_mode,
                "{title} 必须使用统一 task policy"
            );
        }

        let execution_action_ids = tasks
            .iter()
            .filter(|task| {
                task.kind == TaskKind::Action
                    && matches!(task.title.as_str(), "执行第一批" | "执行第二批")
            })
            .map(|task| task.task_id.clone())
            .collect::<Vec<_>>();
        let delivery_action = tasks
            .iter()
            .find(|task| task.kind == TaskKind::Action && task.title == "汇总两批结果")
            .expect("delivery action should exist");
        for execution_action_id in execution_action_ids {
            assert!(
                delivery_action
                    .dependency_ids
                    .iter()
                    .any(|dependency_id| dependency_id == &execution_action_id),
                "交付 action 必须基于每个执行批次的产出"
            );
        }
    }

    #[test]
    fn task_action_roles_stay_runnable_when_goal_mentions_test_path() {
        let (state, task_store) = build_test_state();
        let state =
            state.with_model_bridge_client(Arc::new(PathSensitiveDeepPlanModelBridgeClient));
        let session_id = SessionId::new("session-deep-action-role-compat");
        state
            .session_store
            .create_session(session_id.clone(), "deep action role compat")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let submission = run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at,
                session_id,
                workspace_id: None,
                entry_id: "entry-deep-action-role-compat".to_string(),
                timeline_message: "检查 /Users/xie/code/TEST".to_string(),
                created_session: false,
                mission_title: "检查 TEST 工作区".to_string(),
                task_title: "执行: 检查 TEST 工作区".to_string(),
                trimmed_text: Some("检查 /Users/xie/code/TEST".to_string()),
                execution_goal: Some("检查 /Users/xie/code/TEST".to_string()),
                skill_name: None,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
            },
        )
        .expect("deep task should create graph");

        let action_roles = task_store
            .collect_subtree_ids(&submission.root_task_id)
            .into_iter()
            .filter_map(|task_id| task_store.get_task(&task_id))
            .filter(|task| task.kind == TaskKind::Action)
            .map(|task| {
                task.executor_binding_target_role()
                    .expect("action task should have executor binding")
                    .to_string()
            })
            .collect::<Vec<_>>();

        assert_eq!(action_roles.len(), 3);
        let registry = magi_agent_role::AgentRoleRegistry::load_default();
        for role in action_roles {
            assert!(
                magi_orchestrator::task_worker_catalog::supported_kinds_for_role(&registry, &role)
                    .contains(&TaskKind::Action),
                "deep action role must be runnable by an Action worker, got {role}"
            );
        }
    }

    #[test]
    fn task_planner_receives_workspace_root_context() {
        let (state, _task_store) = build_test_state();
        let captured_prompt = Arc::new(Mutex::new(None));
        let state = state.with_model_bridge_client(Arc::new(RecordingDeepPlanModelBridgeClient {
            prompt: Arc::clone(&captured_prompt),
        }));
        let workspace_id = WorkspaceId::new("workspace-deep-current-project");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/tmp/magi-deep-current-project"),
            )
            .expect("workspace should be registered");
        let session_id = SessionId::new("session-deep-current-project");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "deep current project",
                Some(workspace_id.to_string()),
            )
            .expect("session should be creatable");

        run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at: UtcMillis::now(),
                session_id,
                workspace_id: Some(workspace_id),
                entry_id: "entry-deep-current-project".to_string(),
                timeline_message: "深度分析当前项目".to_string(),
                created_session: false,
                mission_title: "深度分析当前项目".to_string(),
                task_title: "执行: 深度分析当前项目".to_string(),
                trimmed_text: Some("深度分析当前项目".to_string()),
                execution_goal: Some("深度分析当前项目".to_string()),
                skill_name: None,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
            },
        )
        .expect("deep task should create graph");

        let prompt = captured_prompt
            .lock()
            .expect("prompt lock poisoned")
            .clone()
            .expect("planner prompt should be captured");
        assert!(prompt.contains("/tmp/magi-deep-current-project"));
        assert!(prompt.contains("读取这个工作区的真实目录"));
    }

    // -----------------------------------------------------------------------
    // Test 2: Task status reflects dispatch outcome (failure path)
    // -----------------------------------------------------------------------

    #[test]
    fn session_action_registers_execution_plan() {
        let (state, task_store) = build_test_state();

        let session_id = SessionId::new("session-task-status-test");
        state
            .session_store
            .create_session(session_id.clone(), "test session")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let request = SessionTurnRequestDto {
            session_id: None,
            workspace_id: None,
            workspace_path: None,
            text: Some("Run a failing action".to_string()),
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            images: Vec::new(),
            supplement_context: false,
            context_task_id: None,
        };

        let submission = run_session_action(
            &state,
            &request,
            accepted_at,
            &session_id,
            "entry-2",
            Some("Run a failing action"),
        )
        .expect("loopback session action should register a task execution plan");

        let obj = task_store
            .get_task(&submission.root_task_id)
            .expect("objective task should exist");
        let act = task_store
            .get_task(&submission.action_task_id)
            .expect("action task should exist");

        assert_eq!(obj.status, TaskStatus::Running);
        assert_eq!(act.status, TaskStatus::Ready);
        assert!(
            state
                .task_execution_registry()
                .remove(&submission.action_task_id)
                .is_some(),
            "action task should have a registered execution plan",
        );
    }

    #[test]
    fn intake_replan_registers_new_branches_and_replaces_active_chain() {
        let (state, task_store) = build_test_state();

        let session_id = SessionId::new("session-intake-replan-registers");
        state
            .session_store
            .create_session(session_id.clone(), "intake replan")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let submission = run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at,
                session_id: session_id.clone(),
                workspace_id: None,
                entry_id: "entry-intake-replan".to_string(),
                timeline_message: "旧任务".to_string(),
                created_session: false,
                mission_title: "旧任务".to_string(),
                task_title: "执行: 旧任务".to_string(),
                trimmed_text: Some("旧任务".to_string()),
                execution_goal: Some("旧任务".to_string()),
                skill_name: None,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
            },
        )
        .expect("dispatch should create active chain");
        state
            .session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                submission
                    .active_execution_chain
                    .expect("active chain should exist"),
            )
            .expect("active chain should be stored");

        let root_task = task_store
            .get_task(&submission.root_task_id)
            .expect("root task should exist");
        let primary_task_id = TaskId::new("task-replan-primary");
        let leaf_task_id = TaskId::new("task-replan-leaf");
        for (task_id, title, is_primary) in [
            (&primary_task_id, "重规划主任务", true),
            (&leaf_task_id, "重规划 worker 任务", false),
        ] {
            task_store.insert_task(make_dispatch_task(
                task_id.clone(),
                root_task.mission_id.clone(),
                submission.root_task_id.clone(),
                Some(submission.root_task_id.clone()),
                TaskKind::Action,
                title.to_string(),
                title.to_string(),
                TaskStatus::Ready,
                UtcMillis::now(),
                Some(if is_primary {
                    "integration-dev"
                } else {
                    "reviewer"
                }),
                None,
                root_task.policy_snapshot.clone(),
            ));
        }

        replace_replanned_task_execution_branches(
            &state,
            &session_id,
            &primary_task_id,
            &[leaf_task_id.clone()],
        )
        .expect("replanned branches should replace active chain");

        assert!(
            state
                .task_execution_registry()
                .remove(&submission.action_task_id)
                .is_none(),
            "重规划后旧 action plan 不能继续留在 registry"
        );
        assert!(
            state
                .task_execution_registry()
                .remove(&primary_task_id)
                .is_some(),
            "重规划主任务必须注册 execution plan"
        );
        assert!(
            state
                .task_execution_registry()
                .remove(&leaf_task_id)
                .is_some(),
            "重规划 worker 任务必须注册 execution plan"
        );
        let sidecar = state
            .session_store
            .runtime_sidecar(&session_id)
            .expect("sidecar should exist");
        let chain = sidecar
            .active_execution_chain
            .expect("active chain should exist");
        assert!(chain.active_branch_task_ids.contains(&primary_task_id));
        assert!(chain.active_branch_task_ids.contains(&leaf_task_id));
        assert!(
            !chain
                .active_branch_task_ids
                .contains(&submission.action_task_id)
        );
        let turn = chain.current_turn.expect("current turn should exist");
        assert_eq!(turn.worker_lanes.len(), 1);
        assert_eq!(turn.worker_lanes[0].task_id, leaf_task_id);
        assert_eq!(turn.worker_lanes[0].role_id, "reviewer");
    }

    // -----------------------------------------------------------------------
    // Test 3: No TaskStore configured
    // -----------------------------------------------------------------------

    #[test]
    fn no_task_store_is_rejected() {
        let event_bus = Arc::new(InMemoryEventBus::new(64));
        let governance = Arc::new(GovernanceService::default());
        let orchestrator = OrchestratorService::new(Arc::clone(&event_bus));
        let session_store = Arc::new(SessionStore::new());
        let workspace_store = Arc::new(WorkspaceStore::new());
        let memory_store = MemoryStore::new();

        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        let execution_runtime = orchestrator.execution_runtime(
            WorkerRuntime::new_compare(Arc::clone(&event_bus)),
            tool_registry.clone(),
            SkillDispatchRuntime::new(
                tool_registry,
                magi_bridge_client::BridgeDispatchRuntime::new(),
            ),
        );

        // No with_task_store — TaskStore is None
        let state = ApiState::new(
            "test-no-task-store",
            Arc::clone(&event_bus),
            Arc::clone(&session_store),
            Arc::clone(&workspace_store),
            governance,
        )
        .with_execution_pipeline(orchestrator, execution_runtime, memory_store);

        assert!(state.task_store().is_none());

        let session_id = SessionId::new("session-no-store");
        state
            .session_store
            .create_session(session_id.clone(), "test session")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let request = SessionTurnRequestDto {
            session_id: None,
            workspace_id: None,
            workspace_path: None,
            text: Some("test".to_string()),
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            images: Vec::new(),
            supplement_context: false,
            context_task_id: None,
        };

        let result = run_session_action(
            &state,
            &request,
            accepted_at,
            &session_id,
            "entry-3",
            Some("test"),
        );
        assert!(result.is_err());
    }

    fn task_plan_response(arguments: serde_json::Value) -> String {
        serde_json::json!({
            "content": null,
            "finish_reason": "tool_calls",
            "tool_calls": [{
                "id": "call-1",
                "type": "function",
                "function": {
                    "name": TASK_PLAN_TOOL_NAME,
                    "arguments": arguments.to_string(),
                }
            }]
        })
        .to_string()
    }

    #[test]
    fn decomposition_parser_rejects_prompt_echo_lines() {
        let response = task_plan_response(serde_json::json!({
            "phases": [
                {
                    "title": "规划",
                    "workPackages": [
                        {
                            "title": "规划工作包",
                            "actions": [
                                {
                                    "title": "请分析并拆分这个复杂任务",
                                    "goal": "梳理约束"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "执行",
                    "workPackages": [
                        {
                            "title": "执行工作包",
                            "actions": [
                                {
                                    "title": "实现方案",
                                    "goal": "落地实现",
                                    "dependsOn": ["请分析并拆分这个复杂任务"]
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "交付",
                    "workPackages": [
                        {
                            "title": "交付工作包",
                            "actions": [
                                {
                                    "title": "验证结果",
                                    "goal": "确认交付",
                                    "dependsOn": ["实现方案"],
                                    "writeScope": "src/"
                                }
                            ]
                        }
                    ]
                }
            ]
        }));

        let plan = parse_decomposition_response(response.as_str(), "请分析并拆分这个复杂任务");

        assert!(plan.is_none(), "提示词回显不能被当成结构化计划");
    }

    #[test]
    fn decomposition_parser_accepts_structured_tool_plan() {
        let plan_value = serde_json::json!({
            "phases": [
                {
                    "title": "规划",
                    "workPackages": [
                        {
                            "title": "规划工作包",
                            "actions": [
                                {
                                    "title": "分析目标",
                                    "goal": "梳理约束"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "执行",
                    "workPackages": [
                        {
                            "title": "执行工作包",
                            "actions": [
                                {
                                    "title": "实现方案",
                                    "goal": "落地实现",
                                    "dependsOn": ["分析目标"]
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "交付",
                    "workPackages": [
                        {
                            "title": "交付工作包",
                            "actions": [
                                {
                                    "title": "验证结果",
                                    "goal": "确认交付",
                                    "dependsOn": ["实现方案"],
                                    "writeScope": "src/"
                                }
                            ]
                        }
                    ]
                }
            ]
        });
        let response = task_plan_response(plan_value.clone());

        let plan = parse_decomposition_response(response.as_str(), "请分析并拆分这个复杂任务");

        let plan = plan.expect("structured plan should parse");
        assert_eq!(plan.phases.len(), 3);
        assert_eq!(plan.phases[0].work_packages[0].actions[0].title, "分析目标");
        assert_eq!(
            plan.phases[2].work_packages[0].actions[0]
                .write_scope
                .as_deref(),
            Some("src/")
        );

        let prefixed_response = format!("loopback-model::{}", plan_value);
        let prefixed_plan =
            parse_decomposition_response(prefixed_response.as_str(), "请分析并拆分这个复杂任务")
                .expect("prefixed structured plan should parse");
        assert_eq!(
            prefixed_plan.phases[1].work_packages[0].actions[0].title,
            "实现方案"
        );
    }
}
