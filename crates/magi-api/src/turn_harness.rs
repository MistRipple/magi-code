//! 基于真实 TurnService 的消息响应链路测试 harness。
//!
//! 该模块只在测试构建中编译。它不绕过 API 或 canonical sink，而是装配与 daemon
//! 相同的 Conversation dispatcher，使用可观测的流式 Provider 代替网络 Provider。

use crate::{
    dto::{SessionScopeKindDto, SessionTurnRequestDto, SessionTurnResponseDto},
    state::ApiState,
    turn_service::TurnService,
};
use axum::{body::Body, http::Request};
use futures_util::{SinkExt, StreamExt};
use magi_bridge_client::{
    BridgeClientError, BridgeErrorLayer, ChatToolCall, ModelBridgeClient, ModelInvocationRequest,
    ModelResponse, ModelResponseStatus, ModelStreamingDelta, model_invocation_cancelled_error,
};
use magi_conversation_runtime::{
    task_completion_notifier::TaskCompletionNotifier,
    task_execution_dispatcher::{
        ExecutionPipeline, LlmTaskDispatcher, LlmTaskDispatcherDependencies,
    },
    task_execution_registry::AgentSpawnPreflightRuntime,
    task_runner_bridge::EventBasedResultReceiver,
};
use magi_core::{SessionId, UtcMillis};
use magi_event_bus::{EventEnvelope, InMemoryEventBus};
use magi_governance::GovernanceService;
use magi_memory_store::MemoryStore;
use magi_orchestrator::{OrchestratorService, task_store::TaskStore};
use magi_session_store::{ActiveExecutionTurn, CanonicalTurn, CanonicalTurnItemKind, SessionStore};
use magi_skill_runtime::SkillDispatchRuntime;
use magi_tool_runtime::ToolRegistry;
use magi_worker_runtime::WorkerRuntime;
use magi_workspace::WorkspaceStore;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tower::ServiceExt;

#[derive(Clone, Debug)]
enum ProviderBehavior {
    Completed(String),
    Empty,
    Failed(String),
    ToolThenCompleted {
        tool_name: String,
        arguments: String,
        response: String,
    },
    AgentSpawnThenWait {
        response: String,
        child_count: usize,
        role: String,
        display_name: String,
    },
    TransientThenCompleted {
        response: String,
        error: String,
    },
    HoldForCancellation,
}

#[derive(Default)]
struct ProviderState {
    behavior: Option<ProviderBehavior>,
    requests: Vec<ModelInvocationRequest>,
    deltas: Vec<ModelStreamingDelta>,
    tool_round_emitted: bool,
    agent_spawn_emitted: usize,
    transient_failures_remaining: usize,
}

/// 可观测的真流式 Provider 替身。
///
/// 它遵循 ModelBridgeClient 的累计快照合同：每次 delta 都携带当前完整内容，
/// 因此可以直接验证 StreamBuffer、事件序号和最终 canonical projection。
#[derive(Clone)]
pub struct HarnessModelClient {
    state: Arc<Mutex<ProviderState>>,
}

impl HarnessModelClient {
    pub fn new(response: impl Into<String>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ProviderState {
                behavior: Some(ProviderBehavior::Completed(response.into())),
                ..ProviderState::default()
            })),
        }
    }

    pub fn set_completed_response(&self, response: impl Into<String>) {
        self.state
            .lock()
            .expect("harness provider state should hold")
            .behavior = Some(ProviderBehavior::Completed(response.into()));
    }

    pub fn set_empty_response(&self) {
        self.state
            .lock()
            .expect("harness provider state should hold")
            .behavior = Some(ProviderBehavior::Empty);
    }

    pub fn set_failure(&self, message: impl Into<String>) {
        self.state
            .lock()
            .expect("harness provider state should hold")
            .behavior = Some(ProviderBehavior::Failed(message.into()));
    }

    pub fn set_agent_spawn_then_wait(&self, response: impl Into<String>) {
        self.set_agent_spawn_with_role_then_wait(response, "executor", "验收子代理");
    }

    pub fn set_multiple_agent_spawn_then_wait(&self, response: impl Into<String>) {
        let mut state = self
            .state
            .lock()
            .expect("harness provider state should hold");
        state.behavior = Some(ProviderBehavior::AgentSpawnThenWait {
            response: response.into(),
            child_count: 2,
            role: "executor".to_string(),
            display_name: "验收子代理".to_string(),
        });
        state.agent_spawn_emitted = 0;
    }

    pub fn set_agent_spawn_with_role_then_wait(
        &self,
        response: impl Into<String>,
        role: impl Into<String>,
        display_name: impl Into<String>,
    ) {
        let mut state = self
            .state
            .lock()
            .expect("harness provider state should hold");
        state.behavior = Some(ProviderBehavior::AgentSpawnThenWait {
            response: response.into(),
            child_count: 1,
            role: role.into(),
            display_name: display_name.into(),
        });
        state.agent_spawn_emitted = 0;
    }

    pub fn set_retry_then_completed(&self, response: impl Into<String>) {
        let mut state = self
            .state
            .lock()
            .expect("harness provider state should hold");
        state.behavior = Some(ProviderBehavior::TransientThenCompleted {
            response: response.into(),
            error: "provider transport failed: connection reset by peer".to_string(),
        });
        state.transient_failures_remaining = 1;
    }

    pub fn set_timeout_then_completed(&self, response: impl Into<String>) {
        let mut state = self
            .state
            .lock()
            .expect("harness provider state should hold");
        state.behavior = Some(ProviderBehavior::TransientThenCompleted {
            response: response.into(),
            error: "provider request timed out".to_string(),
        });
        state.transient_failures_remaining = 1;
    }

    pub fn set_hold_for_cancellation(&self) {
        self.state
            .lock()
            .expect("harness provider state should hold")
            .behavior = Some(ProviderBehavior::HoldForCancellation);
    }

    pub fn set_tool_then_completed(
        &self,
        tool_name: impl Into<String>,
        arguments: impl Into<String>,
        response: impl Into<String>,
    ) {
        let mut state = self
            .state
            .lock()
            .expect("harness provider state should hold");
        state.behavior = Some(ProviderBehavior::ToolThenCompleted {
            tool_name: tool_name.into(),
            arguments: arguments.into(),
            response: response.into(),
        });
        state.tool_round_emitted = false;
    }

    pub fn requests(&self) -> Vec<ModelInvocationRequest> {
        self.state
            .lock()
            .expect("harness provider state should hold")
            .requests
            .clone()
    }

    pub fn deltas(&self) -> Vec<ModelStreamingDelta> {
        self.state
            .lock()
            .expect("harness provider state should hold")
            .deltas
            .clone()
    }

    fn invoke_streaming_inner(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        let behavior = {
            let mut state = self
                .state
                .lock()
                .expect("harness provider state should hold");
            state.requests.push(request.clone());
            state
                .behavior
                .clone()
                .unwrap_or_else(|| ProviderBehavior::Completed(String::new()))
        };
        match behavior {
            ProviderBehavior::Completed(content) => {
                let mut cumulative = String::new();
                for chunk in content
                    .chars()
                    .collect::<Vec<_>>()
                    .chunks(2)
                    .map(|chunk| chunk.iter().collect::<String>())
                {
                    cumulative.push_str(&chunk);
                    let delta = ModelStreamingDelta {
                        content: cumulative.clone(),
                        thinking: String::new(),
                    };
                    self.state
                        .lock()
                        .expect("harness provider state should hold")
                        .deltas
                        .push(delta.clone());
                    on_delta(&delta);
                }
                Ok(ModelResponse {
                    status: ModelResponseStatus::Completed,
                    content: Some(content),
                    thinking: None,
                    tool_calls: Vec::<ChatToolCall>::new(),
                    usage: None,
                    finish_reason: Some("stop".to_string()),
                    provider_context: Vec::new(),
                })
            }
            ProviderBehavior::TransientThenCompleted { response, error } => {
                let should_fail = {
                    let mut state = self
                        .state
                        .lock()
                        .expect("harness provider state should hold");
                    if state.transient_failures_remaining > 0 {
                        state.transient_failures_remaining -= 1;
                        true
                    } else {
                        false
                    }
                };
                if should_fail {
                    return Err(BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::Transport,
                        code: Some(-32_000),
                        message: error,
                    });
                }
                let mut cumulative = String::new();
                for chunk in response
                    .chars()
                    .collect::<Vec<_>>()
                    .chunks(2)
                    .map(|chunk| chunk.iter().collect::<String>())
                {
                    cumulative.push_str(&chunk);
                    let delta = ModelStreamingDelta {
                        content: cumulative.clone(),
                        thinking: String::new(),
                    };
                    self.state
                        .lock()
                        .expect("harness provider state should hold")
                        .deltas
                        .push(delta.clone());
                    on_delta(&delta);
                }
                Ok(ModelResponse::completed(response))
            }
            ProviderBehavior::Empty => Ok(ModelResponse {
                status: ModelResponseStatus::Completed,
                content: None,
                thinking: None,
                tool_calls: Vec::new(),
                usage: None,
                finish_reason: Some("stop".to_string()),
                provider_context: Vec::new(),
            }),
            ProviderBehavior::Failed(message) => Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32_001),
                message,
            }),
            ProviderBehavior::HoldForCancellation => Ok(ModelResponse::completed("")),
            ProviderBehavior::AgentSpawnThenWait {
                response,
                child_count,
                role,
                display_name,
            } => {
                let classifier_request = request.prompt.contains("Session Turn 编排分类器");
                let messages = request.messages.as_deref().unwrap_or_default();
                let spawn_count = messages
                    .iter()
                    .flat_map(|message| message.tool_calls.iter())
                    .filter(|call| call.function.name == "agent_spawn")
                    .count();
                let wait_count = messages
                    .iter()
                    .flat_map(|message| message.tool_calls.iter())
                    .filter(|call| call.function.name == "agent_wait")
                    .count();
                let coordinator_surface = request.tools.as_ref().is_some_and(|tools| {
                    tools.iter().any(|tool| tool.function.name == "agent_spawn")
                });
                let can_emit_agent_spawn = {
                    let mut state = self
                        .state
                        .lock()
                        .expect("harness provider state should hold");
                    if !classifier_request
                        && coordinator_surface
                        && spawn_count < child_count
                        && state.agent_spawn_emitted < child_count
                    {
                        state.agent_spawn_emitted += 1;
                        true
                    } else {
                        false
                    }
                };
                if can_emit_agent_spawn {
                    let child_index = spawn_count + 1;
                    return Ok(ModelResponse {
                        status: ModelResponseStatus::RequiresToolExecution,
                        content: None,
                        thinking: None,
                        tool_calls: vec![ChatToolCall {
                            id: format!("harness-agent-spawn-call-{child_index}"),
                            kind: "function".to_string(),
                            function: magi_bridge_client::ChatToolFunction {
                                name: "agent_spawn".to_string(),
                                arguments: serde_json::json!({
                                    "task_name": format!("harness_child_{child_index}"),
                                    "display_name": format!("{display_name}{child_index}"),
                                    "role": role,
                                    "goal": "完成 harness 子任务并返回明确结果",
                                    "context_package": {
                                        "summary": "harness 子代理上下文",
                                        "constraints": ["只验证子代理链路"],
                                        "expected_output": "子代理完成",
                                        "references": []
                                    }
                                })
                                .to_string(),
                            },
                        }],
                        usage: None,
                        finish_reason: Some("tool_calls".to_string()),
                        provider_context: Vec::new(),
                    });
                }
                if !classifier_request && coordinator_surface && wait_count == 0 {
                    let child_task_ids = messages
                        .iter()
                        .filter(|message| message.role == "tool")
                        .filter_map(|message| message.content.as_deref())
                        .filter_map(|content| {
                            serde_json::from_str::<serde_json::Value>(content).ok()
                        })
                        .filter_map(|payload| {
                            payload
                                .get("child_task_id")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string)
                        })
                        .collect::<Vec<_>>();
                    if child_task_ids.len() >= child_count {
                        return Ok(ModelResponse {
                            status: ModelResponseStatus::RequiresToolExecution,
                            content: None,
                            thinking: None,
                            tool_calls: vec![ChatToolCall {
                                id: "harness-agent-wait-call".to_string(),
                                kind: "function".to_string(),
                                function: magi_bridge_client::ChatToolFunction {
                                    name: "agent_wait".to_string(),
                                    arguments: serde_json::json!({
                                        "task_ids": child_task_ids,
                                        "timeout_ms": 60_000
                                    })
                                    .to_string(),
                                },
                            }],
                            usage: None,
                            finish_reason: Some("tool_calls".to_string()),
                            provider_context: Vec::new(),
                        });
                    }
                }
                let content = if coordinator_surface && wait_count > 0 {
                    response
                } else {
                    "子代理完成".to_string()
                };
                let mut cumulative = String::new();
                for chunk in content
                    .chars()
                    .collect::<Vec<_>>()
                    .chunks(2)
                    .map(|chunk| chunk.iter().collect::<String>())
                {
                    cumulative.push_str(&chunk);
                    let delta = ModelStreamingDelta {
                        content: cumulative.clone(),
                        thinking: String::new(),
                    };
                    self.state
                        .lock()
                        .expect("harness provider state should hold")
                        .deltas
                        .push(delta.clone());
                    on_delta(&delta);
                }
                Ok(ModelResponse::completed(content))
            }
            ProviderBehavior::ToolThenCompleted {
                tool_name,
                arguments,
                response,
            } => {
                let emit_tool_round = {
                    let mut state = self
                        .state
                        .lock()
                        .expect("harness provider state should hold");
                    let classifier_request = request.prompt.contains("Session Turn 编排分类器");
                    let tool_surface_available = request
                        .tools
                        .as_ref()
                        .is_some_and(|tools| !tools.is_empty());
                    if !classifier_request && tool_surface_available && !state.tool_round_emitted {
                        state.tool_round_emitted = true;
                        true
                    } else {
                        false
                    }
                };
                if emit_tool_round {
                    return Ok(ModelResponse {
                        status: ModelResponseStatus::RequiresToolExecution,
                        content: None,
                        thinking: None,
                        tool_calls: vec![ChatToolCall {
                            id: "harness-tool-call-1".to_string(),
                            kind: "function".to_string(),
                            function: magi_bridge_client::ChatToolFunction {
                                name: tool_name,
                                arguments,
                            },
                        }],
                        usage: None,
                        finish_reason: Some("tool_calls".to_string()),
                        provider_context: Vec::new(),
                    });
                }
                let mut cumulative = String::new();
                for chunk in response
                    .chars()
                    .collect::<Vec<_>>()
                    .chunks(2)
                    .map(|chunk| chunk.iter().collect::<String>())
                {
                    cumulative.push_str(&chunk);
                    let delta = ModelStreamingDelta {
                        content: cumulative.clone(),
                        thinking: String::new(),
                    };
                    self.state
                        .lock()
                        .expect("harness provider state should hold")
                        .deltas
                        .push(delta.clone());
                    on_delta(&delta);
                }
                Ok(ModelResponse::completed(response))
            }
        }
    }
}

impl ModelBridgeClient for HarnessModelClient {
    fn invoke(&self, request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_streaming_inner(request, &|_| {})
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_streaming_inner(request, on_delta)
    }

    fn invoke_streaming_with_cancellation(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
        _on_retry: &dyn Fn(&magi_bridge_client::ModelRetryRuntimeEvent),
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        let hold = self
            .state
            .lock()
            .expect("harness provider state should hold")
            .behavior
            .as_ref()
            .is_some_and(|behavior| matches!(behavior, ProviderBehavior::HoldForCancellation));
        let classifier_request = request.prompt.contains("Session Turn 编排分类器");
        if hold && !classifier_request {
            self.state
                .lock()
                .expect("harness provider state should hold")
                .requests
                .push(request);
            loop {
                if is_cancelled() {
                    return Err(model_invocation_cancelled_error());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        self.invoke_streaming_inner(request, on_delta)
    }
}

/// 真实 TurnService 链路的测试 harness。
///
/// `state` 内的 SessionStore、EventBus、Coordinator 和 dispatcher 全部是真实实现；
/// 只有 Provider 被替换为 `HarnessModelClient`，以便稳定复现正常流、空流和失败。
#[derive(Clone)]
pub struct MagiTurnHarness {
    pub state: ApiState,
    pub provider: HarnessModelClient,
}

impl MagiTurnHarness {
    pub fn new(response: impl Into<String>) -> Self {
        Self::from_parts(
            Arc::new(SessionStore::default()),
            HarnessModelClient::new(response),
            false,
        )
    }

    /// 装配真实 TaskStore/Runner/主动完成通知，用于验证 Task profile 的接纳和终态链路。
    pub fn new_task(response: impl Into<String>) -> Self {
        Self::from_parts(
            Arc::new(SessionStore::default()),
            HarnessModelClient::new(response),
            true,
        )
    }

    fn from_parts(
        session_store: Arc<SessionStore>,
        provider: HarnessModelClient,
        with_task_runtime: bool,
    ) -> Self {
        let event_bus = Arc::new(InMemoryEventBus::new(512));
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let task_store = Arc::new(TaskStore::new());
        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        let skill_dispatch_runtime = SkillDispatchRuntime::new(
            tool_registry.clone(),
            magi_bridge_client::BridgeDispatchRuntime::new(),
        );
        let orchestrator = OrchestratorService::new(Arc::clone(&event_bus));
        let mut execution_runtime = orchestrator.execution_runtime(
            WorkerRuntime::new(Arc::clone(&event_bus)),
            tool_registry.clone(),
            skill_dispatch_runtime,
        );
        if with_task_runtime {
            execution_runtime = execution_runtime.with_task_store(Arc::clone(&task_store));
        }
        let result_receiver = Arc::new(EventBasedResultReceiver::new());
        let completion_notifier = with_task_runtime
            .then(|| Arc::new(TaskCompletionNotifier::new(Arc::clone(&task_store))));
        let mut state = ApiState::new(
            "magi-turn-harness",
            Arc::clone(&event_bus),
            Arc::clone(&session_store),
            workspace_store,
            governance,
        )
        .with_tool_registry(tool_registry.clone());
        if with_task_runtime {
            state = state.with_task_store(Arc::clone(&task_store));
            state
                .task_execution_registry()
                .clone()
                .with_agent_spawn_preflight_runtime(AgentSpawnPreflightRuntime {
                    default_model_client: Some(Arc::new(provider.clone())),
                    ..AgentSpawnPreflightRuntime::default()
                });
        }
        let mut dispatcher_builder = LlmTaskDispatcher::new(
            Arc::clone(&event_bus),
            ExecutionPipeline {
                orchestrator,
                execution_runtime,
                memory_store: MemoryStore::new(),
            },
            LlmTaskDispatcherDependencies {
                session_store: Arc::clone(&session_store),
                execution_registry: state.task_execution_registry().clone(),
                result_receiver: Arc::clone(&result_receiver),
                spawn_graph: Arc::clone(&state.spawn_graph),
                conversation_registry: Arc::clone(&state.conversation_registry),
                agent_role_registry: Arc::clone(&state.agent_role_registry),
            },
            std::env::temp_dir().join(format!(
                "magi-turn-harness-{}-{}",
                std::process::id(),
                UtcMillis::now().0
            )),
        )
        .with_model_bridge_client(Arc::new(provider.clone()));
        if let Some(notifier) = completion_notifier.as_ref() {
            dispatcher_builder = dispatcher_builder.with_completion_notifier(notifier.clone());
        }
        let dispatcher = Arc::new(dispatcher_builder.with_tool_registry(tool_registry));
        state = state
            .with_session_turn_dispatcher(Arc::clone(&dispatcher))
            .with_model_bridge_client(Arc::new(provider.clone()));
        if let Some(notifier) = completion_notifier {
            let result_receiver_for_runner = Arc::clone(&result_receiver);
            result_receiver_for_runner.set_completion_sink(notifier.clone());
            let state_for_workers = state.clone();
            let runner_manager = crate::state::RunnerManager::with_dispatcher_and_worker_catalog(
                Arc::clone(&task_store),
                Arc::clone(&session_store),
                Arc::new(move || state_for_workers.task_worker_catalog()),
                dispatcher,
                result_receiver,
            )
            .with_agent_role_registry(Arc::clone(&state.agent_role_registry));
            state = state
                .with_task_completion_notifier(notifier.clone())
                .with_shared_runner_manager(Arc::new(runner_manager));
            let completion_state = state.clone();
            notifier.set_observer(move |notification| {
                let Some(session_id) = notification.session_id.as_ref() else {
                    return;
                };
                let Some(turn_id) = notification.turn_id.as_deref() else {
                    return;
                };
                let runner_status = match notification.status {
                    magi_core::TaskStatus::Completed => "completed",
                    magi_core::TaskStatus::Failed => "failed",
                    magi_core::TaskStatus::Killed => "killed",
                    _ => return,
                };
                if let Err(error) = crate::task_turn_finalize::finalize_background_session_task_turn_if_root_terminal_for_turn(
                    &completion_state,
                    session_id,
                    &notification.root_task_id,
                    runner_status,
                    Some(turn_id),
                ) {
                    panic!("harness Task terminal 收口失败: {error}");
                }
            });
        }
        let harness = Self { state, provider };
        harness.state.restore_turn_coordinator_from_session_store();
        harness
    }

    /// 使用同一份 canonical SessionStore 构造新的进程内状态，验证 daemon 重启后的
    /// request replay 和 Coordinator 身份恢复；EventBus、dispatcher 和 TaskStore 均重建。
    pub fn restart(&self) -> Self {
        Self::from_parts(
            Arc::clone(&self.state.session_store),
            self.provider.clone(),
            self.state.runner_manager().is_some(),
        )
    }

    pub async fn submit(
        &self,
        session_id: Option<&SessionId>,
        text: &str,
        request_id: &str,
        user_message_id: &str,
    ) -> Result<SessionTurnResponseDto, crate::errors::ApiError> {
        self.submit_with_goal_mode(session_id, text, request_id, user_message_id, false)
            .await
    }

    pub async fn submit_goal(
        &self,
        session_id: Option<&SessionId>,
        text: &str,
        request_id: &str,
        user_message_id: &str,
    ) -> Result<SessionTurnResponseDto, crate::errors::ApiError> {
        self.submit_with_goal_mode(session_id, text, request_id, user_message_id, true)
            .await
    }

    async fn submit_with_goal_mode(
        &self,
        session_id: Option<&SessionId>,
        text: &str,
        request_id: &str,
        user_message_id: &str,
        goal_mode: bool,
    ) -> Result<SessionTurnResponseDto, crate::errors::ApiError> {
        TurnService::new(self.state.clone())
            .submit(SessionTurnRequestDto {
                desktop_browser_tools_allowed: false,
                session_id: session_id.map(ToString::to_string),
                scope: SessionScopeKindDto::Personal,
                workspace_id: None,
                workspace_path: None,
                text: Some(text.to_string()),
                skill_name: None,
                locale: Some("zh-CN".to_string()),
                goal_mode,
                images: Vec::new(),
                context_references: Vec::new(),
                browser_annotation_refs: Vec::new(),
                browser_node_selections: Vec::new(),
                access_profile: None,
                orchestrator_session_config: None,
                request_id: Some(request_id.to_string()),
                user_message_id: Some(user_message_id.to_string()),
                placeholder_message_id: None,
                steer_current_turn: false,
                expected_turn_id: None,
                replace_turn_id: None,
            })
            .await
    }

    pub async fn submit_task(
        &self,
        text: &str,
        request_id: &str,
        user_message_id: &str,
    ) -> Result<SessionTurnResponseDto, crate::errors::ApiError> {
        self.submit(None, text, request_id, user_message_id).await
    }

    pub async fn cancel(&self, session_id: &SessionId) -> Result<(), crate::errors::ApiError> {
        crate::routes::sessions::interrupt_session_turn_for_browser_takeover(
            &self.state,
            session_id,
            None,
        )
        .await
    }

    pub async fn wait_for_terminal(&self, session_id: &SessionId, turn_id: &str) -> CanonicalTurn {
        for _ in 0..200 {
            if let Some(turn) = self
                .state
                .session_store
                .canonical_turn_for_session_turn_id(session_id, turn_id)
            {
                if turn.status.is_terminal() {
                    return turn;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("Turn {turn_id} 未在测试窗口内进入终态");
    }

    pub async fn wait_for_task_terminal(&self, task_id: &magi_core::TaskId) -> magi_core::Task {
        for _ in 0..200 {
            if let Some(task) = self
                .state
                .task_store()
                .and_then(|store| store.get_task(task_id))
            {
                if matches!(
                    task.status,
                    magi_core::TaskStatus::Completed
                        | magi_core::TaskStatus::Failed
                        | magi_core::TaskStatus::Killed
                ) {
                    return task;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("Task {task_id} 未在测试窗口内进入终态");
    }

    pub fn events_for(&self, session_id: &SessionId) -> Vec<EventEnvelope> {
        self.state
            .event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .filter(|event| event.session_id.as_ref() == Some(session_id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_session_store::CanonicalTurnStatus;

    #[tokio::test]
    async fn real_turn_service_streams_and_projects_without_task() {
        let harness = MagiTurnHarness::new("harness 流式响应成功");
        harness
            .provider
            .set_completed_response("harness 流式响应成功");
        let response = harness
            .submit(
                None,
                "请返回流式响应",
                "harness-request-1",
                "harness-user-1",
            )
            .await
            .expect("TurnService 应接受普通 Chat");
        assert_eq!(response.route, crate::dto::SessionTurnRouteDto::Chat);
        assert_eq!(response.request_id.as_deref(), Some("harness-request-1"));
        assert_eq!(
            response.execution_profile,
            Some(magi_conversation_runtime::ExecutionProfile::Conversation)
        );
        let session_id = SessionId::new(response.session_id.clone());
        let turn_id = response
            .turn_id
            .clone()
            .expect("accepted response should expose turn id");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert!(
            harness.state.task_store().is_none(),
            "普通 Chat 不得装配 TaskStore 或创建 root task"
        );
        assert!(
            harness.state.runner_manager().is_none(),
            "普通 Chat 不得装配 Runner"
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::AssistantText
                && item.content.as_deref() == Some("harness 流式响应成功")
        }));
        assert!(!harness.provider.requests().is_empty());
        assert!(!harness.provider.deltas().is_empty());
        let stream_events = harness
            .events_for(&session_id)
            .into_iter()
            .filter(|event| event.event_type == "session.turn.item")
            .filter(|event| event.payload.get("delta").is_some())
            .collect::<Vec<_>>();
        assert!(
            !stream_events.is_empty(),
            "应通过真实 EventBus 发布流式事件"
        );
        assert!(
            stream_events
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
    }

    #[tokio::test]
    async fn duplicate_request_replays_and_fingerprint_conflict_is_rejected() {
        let harness = MagiTurnHarness::new("一次执行");
        let first = harness
            .submit(
                None,
                "重复请求",
                "harness-replay-request",
                "harness-replay-user",
            )
            .await
            .expect("首次提交应成功");
        let session_id = SessionId::new(first.session_id.clone());
        let turn_id = first.turn_id.clone().expect("首次提交应有 Turn");
        harness.wait_for_terminal(&session_id, &turn_id).await;
        let request_count = harness.provider.requests().len();
        let restarted = harness.restart();
        let replay = restarted
            .submit(
                None,
                "重复请求",
                "harness-replay-request",
                "harness-replay-user",
            )
            .await
            .expect("重启后的相同请求应返回 replay");
        assert_eq!(replay.turn_id.as_deref(), Some(turn_id.as_str()));
        assert_eq!(restarted.provider.requests().len(), request_count);
        let conflict = restarted
            .submit(
                None,
                "修改后的请求",
                "harness-replay-request",
                "harness-replay-user-2",
            )
            .await
            .expect_err("相同 requestId 的不同 fingerprint 必须冲突");
        assert!(matches!(
            conflict,
            crate::errors::ApiError::Conflict(message) if message.contains("requestId")
        ));
    }

    #[tokio::test]
    async fn task_profile_keeps_task_store_and_notifier_as_task_fact_source() {
        let harness = MagiTurnHarness::new_task("任务执行成功");
        let response = harness
            .submit_task(
                "请执行一个任务：输出当前执行结果并完成任务",
                "harness-task-request",
                "harness-task-user",
            )
            .await
            .expect("Task Turn 应被真实 TurnService 接纳");
        assert!(matches!(
            response.route,
            crate::dto::SessionTurnRouteDto::Task | crate::dto::SessionTurnRouteDto::Execute
        ));
        assert_eq!(
            response.execution_profile,
            Some(magi_conversation_runtime::ExecutionProfile::Task)
        );
        let session_id = SessionId::new(response.session_id.clone());
        let turn_id = response.turn_id.clone().expect("Task accepted 应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("Task accepted 应有 root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        let task_store = harness.state.task_store().expect("TaskStore 应装配");
        let root_task = task_store
            .get_task(&magi_core::TaskId::new(root_task_id))
            .expect("root task 应持久化");
        assert_eq!(root_task.status, magi_core::TaskStatus::Completed);
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::AssistantText
                && item.content.as_deref() == Some("任务执行成功")
        }));
    }

    #[tokio::test]
    async fn task_profile_executes_a_tool_round_before_final_response() {
        let harness = MagiTurnHarness::new_task("工具调用完成");
        harness.provider.set_tool_then_completed(
            "tool_catalog",
            r#"{"include_external":false}"#,
            "工具调用完成",
        );
        let response = harness
            .submit_task(
                "请执行一个任务：先检查可用工具，再给出最终结果",
                "harness-tool-request",
                "harness-tool-user",
            )
            .await
            .expect("带工具的 Task Turn 应被接纳");
        let session_id = SessionId::new(response.session_id.clone());
        let turn_id = response.turn_id.clone().expect("工具 Task 应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("工具 Task 应有 root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let root_task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(root_task.status, magi_core::TaskStatus::Completed);
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item
                    .tool
                    .as_ref()
                    .is_some_and(|tool| tool.name == "tool_catalog")
        }));
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::AssistantText
                && item.content.as_deref() == Some("工具调用完成")
        }));
        let requests = harness.provider.requests();
        assert!(
            requests.len() >= 2,
            "工具调用后必须再次请求模型完成最终答复"
        );
        assert!(requests.iter().any(|request| {
            request.messages.as_ref().is_some_and(|messages| {
                messages.iter().any(|message| {
                    message.role == "tool"
                        && message.tool_call_id.as_deref() == Some("harness-tool-call-1")
                })
            })
        }));
    }

    #[tokio::test]
    async fn task_profile_spawns_child_and_waits_for_child_result() {
        let harness = MagiTurnHarness::new_task("验收子代理1：子代理完成");
        harness
            .provider
            .set_agent_spawn_then_wait("验收子代理1：子代理完成");
        let response = harness
            .submit_task(
                "请派发一个子代理完成独立验收，再汇总它的结果",
                "harness-agent-spawn-request",
                "harness-agent-spawn-user",
            )
            .await
            .expect("子代理 Task Turn 应被接纳");
        let session_id = SessionId::new(response.session_id.clone());
        let turn_id = response.turn_id.clone().expect("子代理 Turn 应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("子代理 Task 应有 root task");
        let root_task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id.clone()))
            .await;
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(root_task.status, magi_core::TaskStatus::Completed);
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        let children = harness
            .state
            .task_store()
            .expect("子代理场景应装配 TaskStore")
            .get_children(&magi_core::TaskId::new(root_task_id));
        assert_eq!(children.len(), 1, "应真实创建一个子代理任务");
        assert_eq!(children[0].status, magi_core::TaskStatus::Completed);
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item
                    .tool
                    .as_ref()
                    .is_some_and(|tool| tool.name == "agent_spawn")
        }));
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::AssistantText
                && item
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("验收子代理"))
        }));
        let requests = harness.provider.requests();
        assert!(
            requests.iter().any(|request| {
                request.messages.as_ref().is_some_and(|messages| {
                    messages.iter().any(|message| {
                        message
                            .tool_calls
                            .iter()
                            .any(|call| call.function.name == "agent_spawn")
                    })
                })
            }),
            "Provider 请求必须包含 agent_spawn 历史"
        );
        assert!(
            requests.iter().any(|request| {
                request.messages.as_ref().is_some_and(|messages| {
                    messages.iter().any(|message| {
                        message
                            .tool_calls
                            .iter()
                            .any(|call| call.function.name == "agent_wait")
                    })
                })
            }),
            "Provider 请求必须包含 agent_wait 历史"
        );
    }

    #[tokio::test]
    async fn task_profile_preserves_custom_agent_role_snapshot() {
        let harness = MagiTurnHarness::new_task("审查角色1：子代理完成");
        harness.provider.set_agent_spawn_with_role_then_wait(
            "自定义审查代理1：子代理完成",
            "reviewer",
            "自定义审查代理",
        );
        let response = harness
            .submit_task(
                "请执行一个任务：派发审查角色代理完成独立验收并汇总结果",
                "harness-agent-role-request",
                "harness-agent-role-user",
            )
            .await
            .expect("自定义角色 Task Turn 应被接纳");
        let session_id = SessionId::new(response.session_id.clone());
        let turn_id = response.turn_id.clone().expect("自定义角色 Turn 应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("自定义角色 Task 应有 root task");
        let root_task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id.clone()))
            .await;
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(root_task.status, magi_core::TaskStatus::Completed);
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        let children = harness
            .state
            .task_store()
            .expect("自定义角色场景应装配 TaskStore")
            .get_children(&magi_core::TaskId::new(root_task_id));
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].executor_binding_target_role(), Some("reviewer"));
        assert_eq!(children[0].title, "自定义审查代理1");
        assert_eq!(children[0].status, magi_core::TaskStatus::Completed);
    }

    #[tokio::test]
    async fn task_profile_spawns_multiple_children_and_waits_for_all_results() {
        let harness = MagiTurnHarness::new_task("验收子代理1；验收子代理2：子代理完成");
        harness
            .provider
            .set_multiple_agent_spawn_then_wait("验收子代理1；验收子代理2：子代理完成");
        let response = harness
            .submit_task(
                "请并行派发两个子代理完成独立验收，再汇总它们的结果",
                "harness-agent-spawn-many-request",
                "harness-agent-spawn-many-user",
            )
            .await
            .expect("多子代理 Task Turn 应被接纳");
        let session_id = SessionId::new(response.session_id.clone());
        let turn_id = response.turn_id.clone().expect("多子代理 Turn 应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("多子代理 Task 应有 root task");
        let root_task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id.clone()))
            .await;
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(root_task.status, magi_core::TaskStatus::Completed);
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        let children = harness
            .state
            .task_store()
            .expect("多子代理场景应装配 TaskStore")
            .get_children(&magi_core::TaskId::new(root_task_id));
        assert_eq!(children.len(), 2, "应真实创建两个子代理任务");
        assert!(
            children
                .iter()
                .all(|child| child.status == magi_core::TaskStatus::Completed)
        );
        let requests = harness.provider.requests();
        let spawn_calls = requests
            .iter()
            .flat_map(|request| request.messages.as_deref().unwrap_or_default())
            .flat_map(|message| message.tool_calls.iter())
            .filter(|call| call.function.name == "agent_spawn")
            .count();
        let wait_calls = requests
            .iter()
            .flat_map(|request| request.messages.as_deref().unwrap_or_default())
            .flat_map(|message| message.tool_calls.iter())
            .filter(|call| call.function.name == "agent_wait")
            .count();
        assert!(
            spawn_calls >= 2,
            "Provider 请求必须保留两个 agent_spawn 调用"
        );
        assert!(
            wait_calls >= 1,
            "Provider 请求必须保留汇总两个子代理的 agent_wait 调用"
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::AssistantText
                && item
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("验收子代理2"))
        }));
    }

    #[tokio::test]
    async fn goal_profile_stays_on_task_chain_when_provider_cannot_complete_required_tools() {
        let harness = MagiTurnHarness::new_task("目标模式不会跳过工具");
        harness.provider.set_failure("目标 Provider 故障");
        let response = harness
            .submit_goal(
                None,
                "建立一个目标并按步骤推进当前验收",
                "harness-goal-request",
                "harness-goal-user",
            )
            .await
            .expect("Goal Turn 应先被真实 TurnService 接纳");
        assert_eq!(
            response.execution_profile,
            Some(magi_conversation_runtime::ExecutionProfile::Task)
        );
        let session_id = SessionId::new(response.session_id.clone());
        let turn_id = response.turn_id.clone().expect("Goal accepted 应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("Goal Turn 应建立 root task");
        let root_task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id.clone()))
            .await;
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Failed);
        assert_eq!(root_task.status, magi_core::TaskStatus::Failed);
    }

    #[tokio::test]
    async fn reconnect_snapshot_replays_persisted_turn_events_after_receiver_drop() {
        let harness = MagiTurnHarness::new("可恢复流");
        let response = harness
            .submit(
                None,
                "验证断线恢复",
                "harness-reconnect-request",
                "harness-reconnect-user",
            )
            .await
            .expect("断线恢复场景应先接纳");
        let session_id = SessionId::new(response.session_id.clone());
        let turn_id = response.turn_id.clone().expect("断线恢复场景应有 Turn");
        harness.wait_for_terminal(&session_id, &turn_id).await;

        let (_initial_snapshot, receiver) = harness.state.event_bus.snapshot_and_subscribe();
        drop(receiver);
        let (snapshot, _reconnected_receiver) = harness.state.event_bus.snapshot_and_subscribe();
        let session_events = snapshot
            .recent_events
            .iter()
            .filter(|event| event.session_id.as_ref() == Some(&session_id))
            .collect::<Vec<_>>();
        assert!(
            !session_events.is_empty(),
            "重连快照必须包含该 session 的事件"
        );
        assert!(
            session_events
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        assert!(session_events.iter().any(|event| {
            event.event_type == "session.turn.item" && event.payload.get("delta").is_some()
        }));
        assert!(session_events.iter().any(|event| {
            event.event_type == "session.turn.item"
                && event
                    .payload
                    .get("canonical_turn")
                    .and_then(|turn| turn.get("status"))
                    .and_then(serde_json::Value::as_str)
                    == Some("completed")
        }));
    }

    #[tokio::test]
    async fn sse_reconnect_replays_canonical_turn_snapshot_over_real_router_body() {
        let harness = MagiTurnHarness::new("SSE 断线恢复成功");
        let response = harness
            .submit(
                None,
                "通过 SSE 验证断线恢复",
                "harness-sse-request",
                "harness-sse-user",
            )
            .await
            .expect("SSE 场景应先接纳 Turn");
        let session_id = SessionId::new(response.session_id.clone());
        let turn_id = response.turn_id.clone().expect("SSE 场景应有 Turn");
        harness.wait_for_terminal(&session_id, &turn_id).await;

        let app = crate::routes::build_router(harness.state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/events?scope=personal&sessionId={}&afterSequence=0",
                        session_id
                    ))
                    .body(Body::empty())
                    .expect("SSE 请求应构造成功"),
            )
            .await
            .expect("SSE 路由应返回响应");
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        let mut stream = response.into_body().into_data_stream();
        let mut first_connection = String::new();
        for _ in 0..32 {
            let chunk = tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .expect("SSE 首次连接不能超时")
                .expect("SSE 首次连接应保持打开")
                .expect("SSE 首次连接数据应有效");
            first_connection.push_str(&String::from_utf8_lossy(&chunk));
            if first_connection.contains("SSE 断线恢复成功") {
                break;
            }
        }
        assert!(
            first_connection.contains("SSE 断线恢复成功"),
            "首次 SSE 连接必须收到 canonical 回复"
        );
        drop(stream);

        let app = crate::routes::build_router(harness.state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/events?scope=personal&sessionId={}&afterSequence=0",
                        session_id
                    ))
                    .body(Body::empty())
                    .expect("SSE 重连请求应构造成功"),
            )
            .await
            .expect("SSE 重连路由应返回响应");
        let mut reconnected = response.into_body().into_data_stream();
        let mut replay = String::new();
        for _ in 0..32 {
            let chunk = tokio::time::timeout(Duration::from_secs(1), reconnected.next())
                .await
                .expect("SSE 重连不能超时")
                .expect("SSE 重连应保持打开")
                .expect("SSE 重连数据应有效");
            replay.push_str(&String::from_utf8_lossy(&chunk));
            if replay.contains("canonical_turn") && replay.contains("completed") {
                break;
            }
        }
        assert!(
            replay.contains("canonical_turn") && replay.contains("completed"),
            "SSE 重连必须重放 canonical completed 快照"
        );
    }

    #[tokio::test]
    async fn websocket_reconnect_replays_canonical_turn_over_real_app_server() {
        let harness = MagiTurnHarness::new("WebSocket 断线恢复成功");
        let response = harness
            .submit(
                None,
                "通过 WebSocket 验证断线恢复",
                "harness-websocket-request",
                "harness-websocket-user",
            )
            .await
            .expect("WebSocket 场景应先接纳 Turn");
        let session_id = SessionId::new(response.session_id.clone());
        let turn_id = response.turn_id.clone().expect("WebSocket 场景应有 Turn");
        harness.wait_for_terminal(&session_id, &turn_id).await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("WebSocket 测试监听器应绑定");
        let address = listener.local_addr().expect("WebSocket 测试监听器应有地址");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server_state = harness.state.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, crate::routes::build_router(server_state))
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("WebSocket 测试服务应正常退出");
        });

        let (mut first_socket, _) =
            tokio_tungstenite::connect_async(format!("ws://{address}/api/app-server"))
                .await
                .expect("首次 WebSocket 连接应成功");
        first_socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "harness-ws-first-initialize",
                    "method": "initialize",
                    "params": {
                        "clientInfo": {"name": "magi-turn-harness"},
                        "protocol": {"major": 1, "minor": 0},
                        "capabilities": {"streaming": true}
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("首次 initialize 请求应发送");
        loop {
            let message = tokio::time::timeout(Duration::from_secs(2), first_socket.next())
                .await
                .expect("首次 initialize 响应不能超时")
                .expect("首次 WebSocket 应保持连接")
                .expect("首次 WebSocket 帧应有效");
            let tokio_tungstenite::tungstenite::Message::Text(text) = message else {
                continue;
            };
            let value: serde_json::Value =
                serde_json::from_str(&text).expect("首次 WebSocket 文本帧应是 JSON");
            if value.get("id").and_then(serde_json::Value::as_str)
                == Some("harness-ws-first-initialize")
            {
                break;
            }
        }
        first_socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "initialized",
                    "params": {}
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("首次 initialized 通知应发送");
        first_socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "harness-ws-first-subscribe",
                    "method": "events/subscribe",
                    "params": {"sessionId": session_id, "afterSequence": 0}
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("首次 events/subscribe 请求应发送");
        let mut first_snapshot = None;
        let mut first_subscribed = false;
        for _ in 0..16 {
            let message = tokio::time::timeout(Duration::from_secs(2), first_socket.next())
                .await
                .expect("首次快照不能超时")
                .expect("首次 WebSocket 应保持连接")
                .expect("首次 WebSocket 帧应有效");
            let tokio_tungstenite::tungstenite::Message::Text(text) = message else {
                continue;
            };
            let value: serde_json::Value =
                serde_json::from_str(&text).expect("首次快照文本帧应是 JSON");
            if value.get("method").and_then(serde_json::Value::as_str) == Some("events/snapshot") {
                first_snapshot = Some(value.clone());
            }
            if value.get("id").and_then(serde_json::Value::as_str)
                == Some("harness-ws-first-subscribe")
            {
                first_subscribed = value["result"]["subscribed"] == true;
            }
            if first_snapshot.is_some() && first_subscribed {
                break;
            }
        }
        assert!(first_subscribed, "首次 WebSocket 订阅应成功");
        assert!(first_snapshot.is_some(), "首次连接应收到初始快照");
        drop(first_socket);

        let (mut socket, _) =
            tokio_tungstenite::connect_async(format!("ws://{address}/api/app-server"))
                .await
                .expect("重连 WebSocket 应成功");
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "harness-ws-reconnect-initialize",
                    "method": "initialize",
                    "params": {
                        "clientInfo": {"name": "magi-turn-harness"},
                        "protocol": {"major": 1, "minor": 0},
                        "capabilities": {"streaming": true}
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("重连 initialize 请求应发送");
        loop {
            let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
                .await
                .expect("重连 initialize 响应不能超时")
                .expect("重连 WebSocket 应保持连接")
                .expect("重连 WebSocket 帧应有效");
            let tokio_tungstenite::tungstenite::Message::Text(text) = message else {
                continue;
            };
            let value: serde_json::Value =
                serde_json::from_str(&text).expect("重连 WebSocket 文本帧应是 JSON");
            if value.get("id").and_then(serde_json::Value::as_str)
                == Some("harness-ws-reconnect-initialize")
            {
                break;
            }
        }
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "initialized",
                    "params": {}
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("重连 initialized 通知应发送");
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "harness-ws-subscribe",
                    "method": "events/subscribe",
                    "params": {"sessionId": session_id, "afterSequence": 0}
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("重连 events/subscribe 请求应发送");
        let mut subscribed = false;
        let mut reconnect_snapshot = None;
        for _ in 0..16 {
            let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
                .await
                .expect("重连订阅响应不能超时")
                .expect("重连 WebSocket 应保持连接")
                .expect("重连 WebSocket 帧应有效");
            let tokio_tungstenite::tungstenite::Message::Text(text) = message else {
                continue;
            };
            let value: serde_json::Value =
                serde_json::from_str(&text).expect("重连订阅文本帧应是 JSON");
            if value.get("method").and_then(serde_json::Value::as_str) == Some("events/snapshot") {
                reconnect_snapshot = Some(value.clone());
            }
            if value.get("id").and_then(serde_json::Value::as_str) == Some("harness-ws-subscribe") {
                subscribed = value["result"]["subscribed"] == true;
            }
            if subscribed && reconnect_snapshot.is_some() {
                break;
            }
        }
        assert!(subscribed, "重连 WebSocket 订阅应成功");
        let snapshot = reconnect_snapshot.expect("重连应收到初始事件快照");
        assert!(
            snapshot["params"]["recent_events"]
                .as_array()
                .is_some_and(|events| {
                    events.iter().any(|event| {
                        event["event_type"] == "session.turn.item"
                            && event["payload"]["canonical_turn"]["status"] == "completed"
                    })
                })
        );
        drop(socket);
        shutdown_tx.send(()).expect("测试服务应收到停止信号");
        server.await.expect("测试服务任务应结束");
    }

    #[tokio::test]
    async fn cancel_stops_a_running_task_turn_and_preserves_single_terminal_fact() {
        let harness = MagiTurnHarness::new_task("不会返回");
        harness.provider.set_hold_for_cancellation();
        let response = harness
            .submit(
                None,
                "请执行一个任务：取消一个正在运行的任务",
                "harness-cancel-request",
                "harness-cancel-user",
            )
            .await
            .expect("可取消的 Turn 应先被接纳");
        let session_id = SessionId::new(response.session_id.clone());
        let turn_id = response.turn_id.clone().expect("可取消 Turn 应有 Turn");
        for _ in 0..100 {
            if !harness.provider.requests().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            !harness.provider.requests().is_empty(),
            "取消前 Provider 必须已经进入执行"
        );
        harness.cancel(&session_id).await.expect("取消应成功");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Cancelled);
        let terminal_events = harness
            .events_for(&session_id)
            .into_iter()
            .filter(|event| event.event_type == "session.turn.item")
            .filter(|event| {
                event
                    .payload
                    .get("canonical_turn")
                    .and_then(|turn| turn.get("status"))
                    .and_then(serde_json::Value::as_str)
                    == Some("cancelled")
            })
            .count();
        assert_eq!(terminal_events, 1, "取消终态只能发布一次");
    }

    #[tokio::test]
    async fn provider_failure_becomes_a_single_failed_terminal_turn() {
        let harness = MagiTurnHarness::new("不会使用");
        harness.provider.set_failure("harness provider failure");
        let response = harness
            .submit(
                None,
                "失败响应",
                "harness-failure-request",
                "harness-failure-user",
            )
            .await
            .expect("Provider 失败仍应先被接纳");
        let session_id = SessionId::new(response.session_id.clone());
        let turn_id = response.turn_id.clone().expect("失败响应应有 Turn");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Failed);
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::AssistantText
                && item.status == magi_session_store::CanonicalTurnItemStatus::Failed
        }));
    }

    #[tokio::test]
    async fn transient_provider_failure_retries_before_first_delta_and_completes() {
        let harness = MagiTurnHarness::new("重试后完成");
        harness.provider.set_retry_then_completed("重试后完成");
        let response = harness
            .submit(
                None,
                "暂态 Provider 故障后重试",
                "harness-retry-request",
                "harness-retry-user",
            )
            .await
            .expect("暂态故障仍应先接纳 Turn");
        let session_id = SessionId::new(response.session_id.clone());
        let turn_id = response.turn_id.clone().expect("重试 Turn 应有 Turn");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::AssistantText
                && item.content.as_deref() == Some("重试后完成")
        }));
        assert!(
            harness.provider.requests().len() >= 2,
            "首 delta 前暂态 Provider 故障必须重新请求"
        );
    }

    #[tokio::test]
    async fn timeout_provider_failure_retries_before_first_delta_and_completes() {
        let harness = MagiTurnHarness::new("超时后完成");
        harness.provider.set_timeout_then_completed("超时后完成");
        let response = harness
            .submit(
                None,
                "超时后重试",
                "harness-timeout-request",
                "harness-timeout-user",
            )
            .await
            .expect("超时故障仍应先接纳 Turn");
        let session_id = SessionId::new(response.session_id.clone());
        let turn_id = response.turn_id.clone().expect("超时 Turn 应有 Turn");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::AssistantText
                && item.content.as_deref() == Some("超时后完成")
        }));
        assert!(
            harness.provider.requests().len() >= 2,
            "首 delta 前超时必须重新请求"
        );
    }

    #[tokio::test]
    async fn busy_task_session_is_queued_with_stable_request_identity() {
        let harness = MagiTurnHarness::new_task("排队后完成");
        let session_id = SessionId::new("harness-queued-session");
        harness
            .state
            .session_store
            .create_session(session_id.clone(), "排队验收")
            .expect("排队场景应创建 session");
        harness
            .state
            .session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "harness-active-turn".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    status: "running".to_string(),
                    completed_at: None,
                    user_message: Some("占用执行资源".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("排队场景应持久化活跃 Turn");

        let response = harness
            .submit(
                Some(&session_id),
                "请排队执行当前任务",
                "harness-queued-request",
                "harness-queued-user",
            )
            .await
            .expect("忙碌 session 的请求应进入队列");
        assert!(response.queued, "活跃 Turn 存在时请求必须排队");
        assert_eq!(response.queue_position, Some(1));
        let queue_id = response.queue_id.clone().expect("排队响应应返回 queueId");
        let (queued, position) = harness
            .state
            .queued_regular_session_turn_for_request_id("harness-queued-request")
            .expect("请求身份应能从持久化队列恢复");
        assert_eq!(queued.queue_id, queue_id);
        assert_eq!(position, 1);
        assert_eq!(
            queued.request.request_id.as_deref(),
            Some("harness-queued-request")
        );

        let replay = harness
            .submit(
                Some(&session_id),
                "请排队执行当前任务",
                "harness-queued-request",
                "harness-queued-user",
            )
            .await
            .expect("相同队列请求应返回 replay");
        assert!(replay.queued);
        assert_eq!(replay.queue_id.as_deref(), Some(queue_id.as_str()));
        let conflict = harness
            .submit(
                Some(&session_id),
                "排队请求的另一份内容",
                "harness-queued-request",
                "harness-queued-user-2",
            )
            .await
            .expect_err("相同 requestId 的排队 fingerprint 必须冲突");
        assert!(matches!(
            conflict,
            crate::errors::ApiError::Conflict(message)
                if message.contains("排队消息")
        ));
        assert_eq!(
            harness.state.queued_regular_session_turn_count(&session_id),
            1
        );
    }

    #[tokio::test]
    async fn empty_provider_response_becomes_a_single_failed_terminal_turn() {
        let harness = MagiTurnHarness::new("不会使用");
        harness.provider.set_empty_response();
        let response = harness
            .submit(
                None,
                "空响应",
                "harness-empty-request",
                "harness-empty-user",
            )
            .await
            .expect("空响应仍应先被接纳");
        let session_id = SessionId::new(response.session_id.clone());
        let turn_id = response.turn_id.clone().expect("空响应应有 Turn");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Failed);
        let terminal_events = harness
            .events_for(&session_id)
            .into_iter()
            .filter(|event| event.event_type == "session.turn.item")
            .filter(|event| {
                event
                    .payload
                    .get("canonical_event_kind")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|kind| kind.contains("completed"))
            })
            .count();
        assert!(terminal_events <= 1, "终态事件不得重复发布");
    }
}
